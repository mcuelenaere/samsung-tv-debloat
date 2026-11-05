use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use tv_authd::{handle_connection, identity, GrantStore, Logger, ServerConfig};

struct Args {
    socket: PathBuf,
    grants: PathBuf,
    log: PathBuf,
    socket_gid: u32,
    server: ServerConfig,
}

struct ActiveConnection(Arc<AtomicUsize>);

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn main() {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return,
        Err(error) => {
            eprintln!("tv-authd: {error}");
            std::process::exit(2);
        }
    };

    if let Err(error) = run(args) {
        eprintln!("tv-authd: {error}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> io::Result<()> {
    let grants = Arc::new(GrantStore::load(&args.grants)?);
    let logger = Arc::new(Logger::new(&args.log)?);
    logger.log(&format!(
        "starting socket={} grants={} socket_gid={}",
        args.socket.display(),
        grants.path().display(),
        args.socket_gid
    ))?;

    prepare_socket_directory(&args.socket)?;
    remove_stale_socket(&args.socket)?;
    let listener = UnixListener::bind(&args.socket)?;
    fs::set_permissions(&args.socket, fs::Permissions::from_mode(0o660))?;
    chown_root_group(&args.socket, args.socket_gid)?;
    logger.log(&format!(
        "listening socket={} max_connections={}",
        args.socket.display(),
        args.server.max_connections
    ))?;

    let current = Arc::new(AtomicUsize::new(0));
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                logger.log(&format!("accept_error={error}"))?;
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        if current.fetch_add(1, Ordering::AcqRel) >= args.server.max_connections {
            current.fetch_sub(1, Ordering::AcqRel);
            logger.log("connection_rejected=busy")?;
            drop(stream);
            continue;
        }

        let client_grants = Arc::clone(&grants);
        let client_logger = Arc::clone(&logger);
        let config = args.server.clone();
        let active = ActiveConnection(Arc::clone(&current));
        thread::spawn(move || {
            let _active = active;
            let identity = match identity::from_fd(stream.as_raw_fd()) {
                Ok(identity) => identity,
                Err(error) => {
                    let _ = client_logger.log(&format!("peer_identity_error={error}"));
                    return;
                }
            };
            if !identity::is_valid_package_label(&identity.label) {
                let _ = client_logger.log(&format!(
                    "connection_rejected label={:?} uid={} pid={}",
                    identity.label, identity.uid, identity.pid
                ));
                return;
            }

            if let Err(error) = handle_connection(
                stream,
                identity,
                client_grants,
                Some(Arc::clone(&client_logger)),
                &config,
            ) {
                let _ = client_logger.log(&format!("connection_error={error}"));
            }
        });
    }
    Ok(())
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = std::env::args().skip(1);
    let mut socket = PathBuf::from("/run/tv-auth/broker.sock");
    let mut grants = PathBuf::from("/opt/tv-auth/grants.json");
    let mut log = PathBuf::from("/run/tv-auth/tv-authd.log");
    let mut socket_gid = 100_u32;
    let mut server = ServerConfig::default();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--socket" => {
                socket = PathBuf::from(args.next().ok_or("--socket requires a path")?);
            }
            "--grants" => {
                grants = PathBuf::from(args.next().ok_or("--grants requires a path")?);
            }
            "--log" => {
                log = PathBuf::from(args.next().ok_or("--log requires a path")?);
            }
            "--socket-gid" => {
                socket_gid = args
                    .next()
                    .ok_or("--socket-gid requires a numeric gid")?
                    .parse()
                    .map_err(|_| "--socket-gid must be numeric")?;
            }
            "--max-connections" => {
                server.max_connections = args
                    .next()
                    .ok_or("--max-connections requires a number")?
                    .parse()
                    .map_err(|_| "--max-connections must be numeric")?;
                if server.max_connections == 0 {
                    return Err("--max-connections must be greater than zero".into());
                }
            }
            "--help" | "-h" => {
                println!(
                    "tv-authd {}\n\
                     \n\
                     usage: tv-authd [options]\n\
                     \n\
                     --socket PATH             Unix socket (default /run/tv-auth/broker.sock)\n\
                     --grants PATH             grant store (default /opt/tv-auth/grants.json)\n\
                     --log PATH                log file (default /run/tv-auth/tv-authd.log)\n\
                     --socket-gid GID          socket group (default 100)\n\
                     --max-connections COUNT   concurrent clients (default 16)",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(None);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Some(Args {
        socket,
        grants,
        log,
        socket_gid,
        server,
    }))
}

fn prepare_socket_directory(socket: &Path) -> io::Result<()> {
    let parent = socket
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755))
}

fn remove_stale_socket(socket: &Path) -> io::Result<()> {
    match fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(socket),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "refusing to replace a non-socket path",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn chown_root_group(path: &Path, gid: u32) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let result = unsafe { libc::chown(path.as_ptr(), 0, gid) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
