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
    identity, pkglist, pkgsign, pkgstage, trust, ui_alias, wasinstall, Access, AccessPaths,
    ApprovalConfig, ApprovalManager, Broker, GrantStore, Logger, PackageConfig, ServerConfig,
    UiAliasConfig,
};

struct Args {
    socket: PathBuf,
    grants: PathBuf,
    log: PathBuf,
    socket_gid: u32,
    server: ServerConfig,
    approval: ApprovalConfig,
    packages: Option<PackageConfig>,
    enroll_trust: bool,
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
    if args.enroll_trust {
        let Some(packages) = &args.packages else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--enroll-trust needs package support (remove --no-package-support)",
            ));
        };
        let Some(paths) = &packages.trust else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--enroll-trust needs a trust root (remove --no-trust)",
            ));
        };
        return match trust::ensure_enrolled(paths) {
            Ok(enrollment) => {
                println!("trust enrollment: {enrollment}");
                Ok(())
            }
            Err(error) => Err(io::Error::other(error.to_string())),
        };
    }

    let grants = Arc::new(Mutex::new(GrantStore::load(&args.grants)?));
    let logger = Arc::new(Logger::new(&args.log)?);
    logger.log(&format!(
        "starting socket={} grants={} socket_gid={} ui_label={}",
        args.socket.display(),
        args.grants.display(),
        args.socket_gid,
        args.server.ui_label,
    ))?;

    if let Some(packages) = &args.packages {
        pkgstage::prepare_staging_root(&packages.staging.root)?;
        logger.log(&format!(
            "staging_ready root={} pkgcmd={} pkgdb={}",
            packages.staging.root.display(),
            packages.pkgcmd.display(),
            packages.db.database.display()
        ))?;
        if let Some(was) = &packages.was {
            // The overlay is a top-level symlink farm (one directory read),
            // so this is effectively free even on a cold boot.
            match wasinstall::prepare_etc(was) {
                Ok(()) => logger.log(&format!(
                    "was_ready wascmd={} etc_links={} etc_real={}",
                    was.wascmd.display(),
                    was.etc_copy.display(),
                    was.etc_real.display()
                ))?,
                Err(error) => {
                    // Installs will fail with a clear error; keep exec working.
                    logger.log(&format!("was_etc_failed={error}"))?;
                }
            }
        }
        if let Some(paths) = &packages.trust {
            match trust::ensure_enrolled(paths) {
                Ok(enrollment) => {
                    logger.log(&format!("trust_enrollment={enrollment}"))?;
                }
                Err(error) => {
                    // Exec keeps working; pkg.install will fail with enroll_failed.
                    logger.log(&format!("trust_enrollment_failed={error}"))?;
                }
            }
        }
    }

    let manager = ApprovalManager::start(
        args.approval.clone(),
        Arc::clone(&grants),
        Some(Arc::clone(&logger)),
    )?;
    let access = Arc::new(Access::new(
        AccessPaths::default(),
        Some(Arc::clone(&logger)),
    ));
    access.start(manager.handle(), args.approval.approval_wait_timeout());
    let broker = Arc::new(
        Broker::new(grants, manager.handle(), Some(Arc::clone(&logger))).with_access(access),
    );

    prepare_socket_directory(&args.socket)?;
    if let Some(alias) = &args.server.ui_alias {
        // The native UI is a normal package under `/opt`, which UEP refuses
        // to map. Stage it through `/run` before any prompt can launch it.
        if let Some(pkgid) = args.server.ui_package() {
            match ui_alias::ensure(alias, pkgid) {
                Ok(outcome) => logger.log(&format!("ui_alias={outcome}"))?,
                // A missing .NET UI or a not-yet-migrated package is normal.
                Err(error) => logger.log(&format!("ui_alias_failed={error}"))?,
            }
        }
    }
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
        // Installed packages connect with their SMACK package label; the local
        // administrator (root shell, `System::Privileged`) gets the package
        // management escape hatch. Everything else fails closed.
        if !identity::is_valid_package_label(&identity.label)
            && !identity::is_root_identity(&identity)
        {
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
    let mut ui_exec = ui_alias::DEFAULT_EXEC_NAME.to_owned();
    let mut ui_alias_dir = PathBuf::from(ui_alias::DEFAULT_ALIAS_DIR);
    let mut prompt_timeout_ms = 60_000_u64;
    let mut ui_attach_timeout_ms = 10_000_u64;
    let mut prompt_queue_max = 8_usize;
    let mut max_connections = 16_usize;
    let mut staging_dir = PathBuf::from("/opt/tv-auth/staging");
    let mut max_package_bytes = pkgstage::DEFAULT_MAX_PACKAGE_BYTES;
    let mut pkgcmd = PathBuf::from("/usr/bin/pkgcmd");
    let mut sqlite3 = PathBuf::from("/usr/bin/sqlite3");
    let mut xmlsec1 = PathBuf::from("/usr/bin/xmlsec1");
    let mut pkgdb = PathBuf::from("/opt/dbspace/.pkgmgr_parser.db");
    let mut trust_root = Some(PathBuf::from(trust::DEFAULT_TRUST_ROOT));
    let mut package_support = true;
    let mut was_enabled = true;
    let mut enroll_trust = false;

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
            Long("ui-exec") => ui_exec = value(&mut parser, "ui-exec")?,
            Long("ui-alias-dir") => ui_alias_dir = path(&mut parser)?,
            Long("prompt-timeout-ms") => {
                prompt_timeout_ms = value(&mut parser, "prompt-timeout-ms")?
            }
            Long("ui-attach-timeout-ms") => {
                ui_attach_timeout_ms = value(&mut parser, "ui-attach-timeout-ms")?
            }
            Long("prompt-queue-max") => prompt_queue_max = value(&mut parser, "prompt-queue-max")?,
            Long("staging-dir") => staging_dir = path(&mut parser)?,
            Long("max-package-bytes") => {
                max_package_bytes = value(&mut parser, "max-package-bytes")?
            }
            Long("pkgcmd") => pkgcmd = path(&mut parser)?,
            Long("sqlite3") => sqlite3 = path(&mut parser)?,
            Long("xmlsec1") => xmlsec1 = path(&mut parser)?,
            Long("pkgdb") => pkgdb = path(&mut parser)?,
            Long("trust-root") => trust_root = Some(path(&mut parser)?),
            Long("no-trust") => trust_root = None,
            Long("no-package-support") => package_support = false,
            Long("no-was") => was_enabled = false,
            Long("enroll-trust") => enroll_trust = true,
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
                     --ui-exec NAME               native UI exec (default tv-auth-ui)\n\
                     --ui-alias-dir PATH          UEP-exempt UI mount (default /run/tv-auth/ui-bin)\n\
                     --prompt-timeout-ms MS       decision timeout (default 60000)\n\
                     --ui-attach-timeout-ms MS    wait for tv-auth-popupd (default 10000)\n\
                     --prompt-queue-max COUNT     waiting requests (default 8)\n\
                     --staging-dir PATH           package staging root (default /opt/tv-auth/staging)\n\
                     --max-package-bytes BYTES    staged package cap (default 134217728)\n\
                     --pkgcmd PATH                stock installer (default /usr/bin/pkgcmd)\n\
                     --sqlite3 PATH               sqlite3 CLI (default /usr/bin/sqlite3)\n\
                     --xmlsec1 PATH               XML-DSig signer (default /usr/bin/xmlsec1)\n\
                     --pkgdb PATH                 pkgmgr database (default /opt/dbspace/.pkgmgr_parser.db)\n\
                     --trust-root PATH            trust material (default /opt/tv-auth/trust)\n\
                     --no-trust                   skip trust enrollment (development only)\n\
                     --no-package-support         serve exec only\n\
                     --no-was                     use pkgcmd only (no Home registration)\n\
                     --enroll-trust               verify/repair trust bind mounts and exit",
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
    if ui_exec.is_empty()
        || ui_exec.contains('/')
        || ui_exec.contains('\0')
        || ui_exec == "."
        || ui_exec == ".."
    {
        return Err("--ui-exec must be a plain file name".into());
    }
    if max_connections == 0
        || prompt_timeout_ms == 0
        || ui_attach_timeout_ms == 0
        || prompt_queue_max == 0
    {
        return Err("connections, timeouts, and queue size must be greater than zero".into());
    }
    if prompt_timeout_ms > 600_000 || ui_attach_timeout_ms > 120_000 || prompt_queue_max > 64 {
        return Err("timeout or queue value is too large".into());
    }

    let approval = ApprovalConfig {
        prompt_timeout: Duration::from_millis(prompt_timeout_ms),
        ui_attach_timeout: Duration::from_millis(ui_attach_timeout_ms),
        prompt_queue_max,
        notice_interval: Duration::from_millis(250),
    };
    if !(1024 * 1024..=pkgstage::MAX_PACKAGE_BYTES_LIMIT).contains(&max_package_bytes) {
        return Err("--max-package-bytes must be between 1 MiB and 256 MiB".into());
    }

    let trust_paths = trust_root.map(trust::TrustPaths::new);
    let signing = trust_paths
        .as_ref()
        .map(|paths| pkgsign::SigningKeys::from_trust(paths, xmlsec1.clone()));
    let packages = package_support.then(|| PackageConfig {
        staging: pkgstage::StageConfig {
            root: staging_dir,
            max_bytes: max_package_bytes,
            ..pkgstage::StageConfig::default()
        },
        pkgcmd,
        db: pkglist::PackageDb {
            sqlite3,
            database: pkgdb,
        },
        xmlsec1,
        trust: trust_paths,
        signing,
        was: was_enabled.then(wasinstall::WasConfig::new),
        home_db: PathBuf::from("/opt/dbspace/.webappservice.db"),
        icon_root: PathBuf::from("/opt/share/webappservice/apps_icon/all"),
        vconf: PathBuf::from("/usr/bin/vconf"),
        installed_roots: tv_authd::DEFAULT_INSTALLED_ROOTS
            .iter()
            .map(PathBuf::from)
            .collect(),
    });
    let server = ServerConfig {
        max_connections,
        // tv-auth-popupd, the manager's grant client, and the manager's
        // one-shot store calls (pkg.list, an install, grants.audit).
        max_ui_connections: 6,
        max_requests_per_connection: 1024,
        ui_label: format!("User::Pkg::{ui_package}"),
        ui_uid,
        approval_wait_timeout: approval.approval_wait_timeout(),
        packages: packages.clone(),
        ui_alias: Some(UiAliasConfig::new(ui_exec, ui_alias_dir)),
    };

    Ok(Some(Args {
        socket,
        grants,
        log,
        socket_gid,
        server,
        approval,
        packages,
        enroll_trust,
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
