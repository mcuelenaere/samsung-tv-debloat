use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tv_authd::{
    identity, ApprovalConfig, ApprovalManager, Broker, GrantStore, Logger, ServerConfig,
};

struct Args {
    socket: PathBuf,
    grants: PathBuf,
    log: PathBuf,
    socket_gid: u32,
    server: ServerConfig,
    approval: ApprovalConfig,
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
    let grants = Arc::new(Mutex::new(GrantStore::load(&args.grants)?));
    let logger = Arc::new(Logger::new(&args.log)?);
    logger.log(&format!(
        "starting socket={} grants={} socket_gid={} ui_label={} ui_app_id={}",
        args.socket.display(),
        args.grants.display(),
        args.socket_gid,
        args.server.ui_label,
        args.approval.ui_app_id
    ))?;

    let manager = ApprovalManager::start(
        args.approval.clone(),
        Arc::clone(&grants),
        Some(Arc::clone(&logger)),
    )?;
    let broker = Arc::new(Broker::new(
        grants,
        manager.handle(),
        Some(Arc::clone(&logger)),
    ));

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

    // Apps and the approval UI have separate slots so apps cannot lock the UI out.
    let current_apps = Arc::new(AtomicUsize::new(0));
    let current_ui = Arc::new(AtomicUsize::new(0));
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                let _ = logger.log(&format!("accept_error={error}"));
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        // Peer identity is a cheap getsockopt, so read it before taking a slot.
        let identity = match identity::from_fd(&stream) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = logger.log(&format!("peer_identity_error={error}"));
                continue;
            }
        };
        if !identity::is_valid_package_label(&identity.label) {
            let _ = logger.log(&format!(
                "connection_rejected label={:?} uid={} pid={}",
                identity.label, identity.uid, identity.pid
            ));
            continue;
        }

        let (counter, limit) = if args.server.is_ui(&identity) {
            (&current_ui, args.server.max_ui_connections)
        } else {
            (&current_apps, args.server.max_connections)
        };
        if counter.fetch_add(1, Ordering::AcqRel) >= limit {
            counter.fetch_sub(1, Ordering::AcqRel);
            let _ = logger.log(&format!(
                "connection_rejected=busy label={}",
                identity.label
            ));
            continue;
        }

        let client_broker = Arc::clone(&broker);
        let client_logger = Arc::clone(&logger);
        let config = args.server.clone();
        let active = ActiveConnection(Arc::clone(counter));
        thread::spawn(move || {
            let _active = active;
            if let Err(error) =
                tv_authd::handle_connection(stream, identity, client_broker, &config)
            {
                let _ = client_logger.log(&format!("connection_error={error}"));
            }
        });
    }
    drop(manager);
    Ok(())
}

fn parse_args() -> Result<Option<Args>, String> {
    use lexopt::prelude::*;

    fn path(parser: &mut lexopt::Parser) -> Result<PathBuf, String> {
        parser
            .value()
            .map(PathBuf::from)
            .map_err(|error| error.to_string())
    }
    fn value<T: std::str::FromStr>(parser: &mut lexopt::Parser, name: &str) -> Result<T, String> {
        let raw = parser.value().map_err(|error| error.to_string())?;
        let text = raw
            .into_string()
            .map_err(|_| format!("--{name} must be valid UTF-8"))?;
        text.parse()
            .map_err(|_| format!("--{name}: invalid value {text:?}"))
    }

    let mut socket = PathBuf::from("/run/tv-auth/broker.sock");
    let mut grants = PathBuf::from("/opt/tv-auth/grants.json");
    let mut log = PathBuf::from("/run/tv-auth/tv-authd.log");
    let mut socket_gid = 100_u32;
    let mut ui_package = "org.tvauth.ui".to_owned();
    let mut ui_uid = 5001_u32;
    let mut ui_app_id: Option<String> = None;
    let mut app_launcher = PathBuf::from("/usr/bin/app_launcher");
    let mut prompt_timeout_ms = 60_000_u64;
    let mut ui_launch_timeout_ms = 10_000_u64;
    let mut launch_cooldown_ms = 1_000_u64;
    let mut prompt_queue_max = 8_usize;
    let mut max_launch_attempts = 3_usize;
    let mut max_connections = 16_usize;

    let mut parser = lexopt::Parser::from_env();
    while let Some(argument) = parser.next().map_err(|error| error.to_string())? {
        match argument {
            Long("socket") => socket = path(&mut parser)?,
            Long("grants") => grants = path(&mut parser)?,
            Long("log") => log = path(&mut parser)?,
            Long("socket-gid") => socket_gid = value(&mut parser, "socket-gid")?,
            Long("max-connections") => max_connections = value(&mut parser, "max-connections")?,
            Long("ui-package") => ui_package = value(&mut parser, "ui-package")?,
            Long("ui-uid") => ui_uid = value(&mut parser, "ui-uid")?,
            Long("ui-app-id") => ui_app_id = Some(value(&mut parser, "ui-app-id")?),
            Long("app-launcher") => app_launcher = path(&mut parser)?,
            Long("prompt-timeout-ms") => {
                prompt_timeout_ms = value(&mut parser, "prompt-timeout-ms")?
            }
            Long("ui-launch-timeout-ms") => {
                ui_launch_timeout_ms = value(&mut parser, "ui-launch-timeout-ms")?
            }
            Long("launch-cooldown-ms") => {
                launch_cooldown_ms = value(&mut parser, "launch-cooldown-ms")?
            }
            Long("prompt-queue-max") => prompt_queue_max = value(&mut parser, "prompt-queue-max")?,
            Long("max-launch-attempts") => {
                max_launch_attempts = value(&mut parser, "max-launch-attempts")?
            }
            Long("help") | Short('h') => {
                println!(
                    "tv-authd {}\n\
                     \n\
                     usage: tv-authd [options]\n\
                     \n\
                     --socket PATH                Unix socket (default /run/tv-auth/broker.sock)\n\
                     --grants PATH                grant store (default /opt/tv-auth/grants.json)\n\
                     --log PATH                   log file (default /run/tv-auth/tv-authd.log)\n\
                     --socket-gid GID             socket group (default 100)\n\
                     --max-connections COUNT      concurrent clients (default 16)\n\
                     --ui-package PKGID           approval UI package (default org.tvauth.ui)\n\
                     --ui-uid UID                 approval UI uid (default 5001)\n\
                     --ui-app-id APPID            app_launcher id (default: --ui-package)\n\
                     --app-launcher PATH          app launcher (default /usr/bin/app_launcher)\n\
                     --prompt-timeout-ms MS       decision timeout (default 60000)\n\
                     --ui-launch-timeout-ms MS    UI connect timeout (default 10000)\n\
                     --launch-cooldown-ms MS      relaunch cooldown (default 1000)\n\
                     --prompt-queue-max COUNT     waiting requests (default 8)\n\
                     --max-launch-attempts COUNT  UI launch attempts (default 3)",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(None);
            }
            other => return Err(other.unexpected().to_string()),
        }
    }

    if !identity::is_valid_package_id(&ui_package) {
        return Err("--ui-package must be a valid package id".into());
    }
    if max_connections == 0
        || prompt_timeout_ms == 0
        || ui_launch_timeout_ms == 0
        || launch_cooldown_ms == 0
        || prompt_queue_max == 0
        || max_launch_attempts == 0
    {
        return Err(
            "connections, timeouts, queue size, and launch attempts must be greater than zero"
                .into(),
        );
    }
    if prompt_timeout_ms > 600_000
        || ui_launch_timeout_ms > 120_000
        || launch_cooldown_ms > 60_000
        || prompt_queue_max > 64
        || max_launch_attempts > 10
    {
        return Err("timeout, queue, or launch-attempt value is too large".into());
    }

    let ui_app_id = ui_app_id.unwrap_or_else(|| ui_package.clone());
    let approval = ApprovalConfig {
        ui_app_id: ui_app_id.clone(),
        app_launcher,
        prompt_timeout: Duration::from_millis(prompt_timeout_ms),
        ui_launch_timeout: Duration::from_millis(ui_launch_timeout_ms),
        launch_cooldown: Duration::from_millis(launch_cooldown_ms),
        prompt_queue_max,
        max_launch_attempts,
        notice_interval: Duration::from_millis(250),
    };
    let server = ServerConfig {
        max_connections,
        max_ui_connections: 2,
        max_requests_per_connection: 1024,
        ui_label: format!("User::Pkg::{ui_package}"),
        ui_uid,
        approval_wait_timeout: approval.approval_wait_timeout(),
    };

    Ok(Some(Args {
        socket,
        grants,
        log,
        socket_gid,
        server,
        approval,
    }))
}

fn prepare_socket_directory(socket: &Path) -> io::Result<()> {
    let parent = socket
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    fs::create_dir_all(parent)?;
    // Traverse-only for apps: enough to connect to the known socket path,
    // but not to list the daemon log or other runtime entries.
    fs::set_permissions(parent, fs::Permissions::from_mode(0o711))
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
    std::os::unix::fs::chown(path, Some(0), Some(gid))
}
