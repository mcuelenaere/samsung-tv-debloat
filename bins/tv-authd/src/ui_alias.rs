//! Stage the native approval UI at a UEP-exempt path.
//!
//! Samsung's kernel UEP module refuses to `exec` or `dlopen` an ELF whose
//! `d_path()` starts with `/opt`, `/tmp`, `/var`, or `/home`.  A Tizen
//! `type="capp"` package always installs its executable under
//! `/opt/usr/home/owner/apps_rw/<pkgid>/bin/`, so launchpad cannot map a native
//! UI from there even though the TPK itself is validly signed.
//!
//! The UEP verifier takes the "disabled" branch for files that resolve through
//! a mount whose root is outside that set.  This module therefore:
//!
//! 1. bind-mounts the installed UI ELF read-only at
//!    `/run/tv-auth/ui-bin/<exec>` in PID 1's mount namespace;
//! 2. replaces the packaged `bin/<exec>` with a symlink to that alias.
//!
//! Launchpad opens `/opt/usr/apps/<pkgid>/bin/<exec>`, the VFS follows the
//! symlink to the `/run` mount, and `d_path()` reports the `/run` path, so UEP
//! skips verification.  The package, its SMACK domain
//! (`User::Pkg::<pkgid>`), AUL registration and Samsung Home entry stay
//! untouched.
//!
//! The operation is idempotent and safe to run on every daemon start and after
//! every install of the UI package.  If the native UI is not installed (for
//! example while the managed `.NET` UI is still live) it is a no-op.

use crate::executor;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Runtime location of the UEP-exempt copy.
pub const DEFAULT_ALIAS_DIR: &str = "/run/tv-auth/ui-bin";
/// Native executable name inside the package's `bin/` directory.
pub const DEFAULT_EXEC_NAME: &str = "tv-auth-ui";
/// Where the package manager installs user packages.
pub const DEFAULT_PACKAGE_ROOTS: &[&str] =
    &["/opt/usr/home/owner/apps_rw", "/opt/usr/apps", "/usr/apps"];
const COMMAND_TIMEOUT_MS: u64 = 10_000;

/// Test seam: runs one `mount`/`umount` invocation in PID 1's namespace.
type CommandRunner<'a> = dyn Fn(&UiAliasConfig, &Path, &[&OsStr]) -> Result<(), UiAliasError> + 'a;

#[derive(Debug, Clone)]
pub struct UiAliasConfig {
    /// Directory containing the UEP-exempt bind-mounted binary.
    pub alias_dir: PathBuf,
    /// Manifest `exec=` value (a plain filename, not a path).
    pub exec_name: String,
    /// Candidate package roots, tried in order.
    pub package_roots: Vec<PathBuf>,
    /// `nsenter`, used to enter PID 1's mount namespace.
    pub nsenter: PathBuf,
    /// `mount`, normally `/bin/mount`.
    pub mount: PathBuf,
    /// `umount`, normally `/bin/umount`.
    pub umount: PathBuf,
}

impl UiAliasConfig {
    pub fn new(exec_name: impl Into<String>, alias_dir: impl Into<PathBuf>) -> Self {
        let nsenter = if Path::new("/usr/bin/nsenter").exists() {
            PathBuf::from("/usr/bin/nsenter")
        } else {
            PathBuf::from("/bin/nsenter")
        };
        Self {
            alias_dir: alias_dir.into(),
            exec_name: exec_name.into(),
            package_roots: DEFAULT_PACKAGE_ROOTS.iter().map(PathBuf::from).collect(),
            nsenter,
            mount: PathBuf::from("/bin/mount"),
            umount: PathBuf::from("/bin/umount"),
        }
    }
}

impl Default for UiAliasConfig {
    fn default() -> Self {
        Self::new(DEFAULT_EXEC_NAME, DEFAULT_ALIAS_DIR)
    }
}

#[derive(Debug)]
pub enum UiAliasError {
    Io(io::Error),
    /// The package directory exists but the executable is missing.
    MissingBinary(PathBuf),
    /// A previous run left a symlink without its `.real` source.
    UnsafeState(String),
    /// A `mount`/`umount`/`nsenter` invocation failed.
    Command {
        program: PathBuf,
        detail: String,
    },
}

impl std::fmt::Display for UiAliasError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "ui alias io error: {error}"),
            Self::MissingBinary(path) => {
                write!(formatter, "ui alias: {} does not exist", path.display())
            }
            Self::UnsafeState(detail) => write!(formatter, "ui alias: {detail}"),
            Self::Command { program, detail } => {
                write!(
                    formatter,
                    "ui alias: {} failed: {detail}",
                    program.display()
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAliasOutcome {
    /// The UI package is not installed; nothing was changed.
    NotInstalled,
    /// The alias mount and symlink are in place.
    Mounted,
}

impl std::fmt::Display for UiAliasOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => formatter.write_str("not_installed"),
            Self::Mounted => formatter.write_str("mounted"),
        }
    }
}

/// Ensure the installed UI package's executable is reachable through a
/// UEP-exempt `/run` bind mount.  Never fatal for the caller: a missing UI or
/// an unsupported layout is reported as `NotInstalled`/an error.
pub fn ensure(config: &UiAliasConfig, pkgid: &str) -> Result<UiAliasOutcome, UiAliasError> {
    ensure_with(config, pkgid, &|config, program, arguments| {
        run_in_root_namespace(config, program, arguments)
    })
}

fn ensure_with(
    config: &UiAliasConfig,
    pkgid: &str,
    run: &CommandRunner<'_>,
) -> Result<UiAliasOutcome, UiAliasError> {
    let Some(bin_path) = locate_binary(config, pkgid) else {
        return Ok(UiAliasOutcome::NotInstalled);
    };
    let bin_dir = bin_path.parent().ok_or_else(|| {
        UiAliasError::UnsafeState(format!("{} has no parent directory", bin_path.display()))
    })?;
    let real_path = bin_dir.join(format!("{}.real", config.exec_name));
    let alias_path = config.alias_dir.join(&config.exec_name);

    // Decide where the actual bytes live.  On first run (or after an upgrade
    // replaced the packaged file) `bin/<exec>` is a regular file.  After a
    // previous run it is a symlink to the alias and the bytes live in
    // `bin/<exec>.real`.
    let metadata = fs::symlink_metadata(&bin_path).map_err(UiAliasError::Io)?;
    let source_is_packaged = metadata.file_type().is_file();
    if metadata.file_type().is_symlink() && !real_path.is_file() {
        return Err(UiAliasError::UnsafeState(format!(
            "{} is a symlink but {} is missing; reinstall the UI package",
            bin_path.display(),
            real_path.display()
        )));
    }
    if !source_is_packaged && !metadata.file_type().is_symlink() {
        return Err(UiAliasError::MissingBinary(bin_path));
    }
    let source = if source_is_packaged {
        bin_path.clone()
    } else {
        real_path.clone()
    };

    fs::create_dir_all(&config.alias_dir).map_err(UiAliasError::Io)?;
    fs::set_permissions(&config.alias_dir, fs::Permissions::from_mode(0o755))
        .map_err(UiAliasError::Io)?;
    // `mount --bind <file> <file>` requires the target to exist. Only create
    // it when absent: opening an already-mounted target for write could touch
    // the source binary.
    if fs::symlink_metadata(&alias_path).is_err() {
        fs::write(&alias_path, b"").map_err(UiAliasError::Io)?;
        fs::set_permissions(&alias_path, fs::Permissions::from_mode(0o644))
            .map_err(UiAliasError::Io)?;
    }

    // Re-mount unconditionally: `/run` is wiped on every boot and an upgrade
    // changes the inode.  A failure here means the old mount (if any) stays;
    // remove it before binding the new source.
    let _ = run(config, &config.umount, &[alias_path.as_os_str()]);
    run(
        config,
        &config.mount,
        &[
            OsStr::new("--bind"),
            source.as_os_str(),
            alias_path.as_os_str(),
        ],
    )?;
    // Best effort: a writable alias would only let the UI rewrite its own
    // binary, but read-only is the intended contract.  Older busybox mounts
    // may not accept the remount syntax, so do not fail the setup.
    let _ = run(
        config,
        &config.mount,
        &[
            OsStr::new("-o"),
            OsStr::new("remount,bind,ro"),
            alias_path.as_os_str(),
        ],
    );

    // Publish the symlink only after the mount is live.  Replacing a regular
    // file with a symlink is not atomic, so keep the original until the link
    // exists.
    if source_is_packaged {
        if real_path.exists() {
            fs::remove_file(&real_path).map_err(UiAliasError::Io)?;
        }
        fs::rename(&bin_path, &real_path).map_err(UiAliasError::Io)?;
    }

    let temporary = bin_dir.join(format!(".{}.tmp", config.exec_name));
    match fs::symlink_metadata(&temporary) {
        Ok(_) => {
            fs::remove_file(&temporary).map_err(UiAliasError::Io)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(UiAliasError::Io(error)),
    }
    std::os::unix::fs::symlink(&alias_path, &temporary).map_err(UiAliasError::Io)?;
    if let Err(error) = fs::rename(&temporary, &bin_path) {
        let _ = fs::remove_file(&temporary);
        // Roll back so the package is still launchable (through the old path)
        // if the symlink could not be published.
        if source_is_packaged {
            let _ = fs::rename(&real_path, &bin_path);
        }
        return Err(UiAliasError::Io(error));
    }

    Ok(UiAliasOutcome::Mounted)
}

/// Put the packaged executable back before an install/upgrade touches the
/// package directory. Some installers overwrite `bin/<exec>` in place; writing
/// through the alias symlink would target the read-only `/run` mount.
pub fn restore(config: &UiAliasConfig, pkgid: &str) -> Result<(), UiAliasError> {
    restore_with(config, pkgid, &|config, program, arguments| {
        run_in_root_namespace(config, program, arguments)
    })
}

fn restore_with(
    config: &UiAliasConfig,
    pkgid: &str,
    run: &CommandRunner<'_>,
) -> Result<(), UiAliasError> {
    let Some(bin_path) = locate_binary(config, pkgid) else {
        return Ok(());
    };
    let Ok(metadata) = fs::symlink_metadata(&bin_path) else {
        return Ok(());
    };
    if !metadata.file_type().is_symlink() {
        return Ok(());
    }
    let Some(bin_dir) = bin_path.parent() else {
        return Ok(());
    };
    let real_path = bin_dir.join(format!("{}.real", config.exec_name));
    if !real_path.is_file() {
        return Err(UiAliasError::UnsafeState(format!(
            "{} is a symlink but {} is missing",
            bin_path.display(),
            real_path.display()
        )));
    }
    // `rename` replaces the symlink atomically with the packaged file.
    fs::rename(&real_path, &bin_path).map_err(UiAliasError::Io)?;
    let alias_path = config.alias_dir.join(&config.exec_name);
    let _ = run(config, &config.umount, &[alias_path.as_os_str()]);
    Ok(())
}

fn locate_binary(config: &UiAliasConfig, pkgid: &str) -> Option<PathBuf> {
    for root in &config.package_roots {
        let candidate = root.join(pkgid).join("bin").join(&config.exec_name);
        if fs::symlink_metadata(&candidate).is_ok() {
            return Some(candidate);
        }
    }
    None
}

fn run_in_root_namespace(
    config: &UiAliasConfig,
    program: &Path,
    arguments: &[&OsStr],
) -> Result<(), UiAliasError> {
    let mut argv: Vec<&OsStr> = vec![
        OsStr::new("-t"),
        OsStr::new("1"),
        OsStr::new("-m"),
        OsStr::new("--"),
        program.as_os_str(),
    ];
    argv.extend_from_slice(arguments);
    let outcome = executor::execute_argv(&config.nsenter, &argv, COMMAND_TIMEOUT_MS)
        .map_err(UiAliasError::Io)?;
    if outcome.timed_out {
        return Err(UiAliasError::Command {
            program: program.to_owned(),
            detail: "timed out".into(),
        });
    }
    if outcome.exit_code != 0 {
        let detail = String::from_utf8_lossy(&outcome.stderr).trim().to_owned();
        return Err(UiAliasError::Command {
            program: program.to_owned(),
            detail: if detail.is_empty() {
                format!("exit {}", outcome.exit_code)
            } else {
                detail
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::fs::symlink;

    struct Fixture {
        _directory: tempfile::TempDir,
        config: UiAliasConfig,
        app_bin: PathBuf,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let apps = directory.path().join("apps");
        let app_bin = apps.join("org.tvauth.ui").join("bin");
        fs::create_dir_all(&app_bin).unwrap();
        fs::write(app_bin.join("tv-auth-ui"), b"first build").unwrap();
        let config = UiAliasConfig {
            alias_dir: directory.path().join("run/ui-bin"),
            exec_name: "tv-auth-ui".into(),
            package_roots: vec![apps],
            nsenter: PathBuf::from("/nonexistent/nsenter"),
            mount: PathBuf::from("/nonexistent/mount"),
            umount: PathBuf::from("/nonexistent/umount"),
        };
        Fixture {
            _directory: directory,
            config,
            app_bin,
        }
    }

    /// Emulate `mount --bind <src> <dst>` and `umount <dst>` with copies.
    fn fake_run(
        config: &UiAliasConfig,
        program: &Path,
        arguments: &[&OsStr],
    ) -> Result<(), UiAliasError> {
        if program == config.mount {
            if arguments.first() == Some(&OsStr::new("--bind")) {
                let source = Path::new(arguments[1]);
                let target = Path::new(arguments[2]);
                fs::copy(source, target).map_err(UiAliasError::Io)?;
            }
            return Ok(());
        }
        if program == config.umount {
            let target = Path::new(arguments[0]);
            match fs::remove_file(target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(UiAliasError::Io(error)),
            }
            return Ok(());
        }
        Ok(())
    }

    #[test]
    fn mounts_and_links_the_installed_binary() {
        let fixture = fixture();
        let calls: RefCell<Vec<(PathBuf, Vec<std::ffi::OsString>)>> = RefCell::new(Vec::new());
        let run = |config: &UiAliasConfig, program: &Path, arguments: &[&OsStr]| {
            calls.borrow_mut().push((
                program.to_owned(),
                arguments
                    .iter()
                    .map(|value| std::ffi::OsString::from(*value))
                    .collect(),
            ));
            fake_run(config, program, arguments)
        };
        assert_eq!(
            ensure_with(&fixture.config, "org.tvauth.ui", &run).unwrap(),
            UiAliasOutcome::Mounted
        );

        let alias = fixture.config.alias_dir.join("tv-auth-ui");
        assert_eq!(fs::read(&alias).unwrap(), b"first build");
        let link = fixture.app_bin.join("tv-auth-ui");
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), alias);
        assert_eq!(
            fs::read(fixture.app_bin.join("tv-auth-ui.real")).unwrap(),
            b"first build"
        );
        assert!(calls
            .borrow()
            .iter()
            .any(|(_, args)| args.first().map(|value| value.as_os_str())
                == Some(OsStr::new("--bind"))));
    }

    #[test]
    fn second_run_is_idempotent_and_mounts_the_real_file() {
        let fixture = fixture();
        let run = |config: &UiAliasConfig, program: &Path, arguments: &[&OsStr]| {
            fake_run(config, program, arguments)
        };
        ensure_with(&fixture.config, "org.tvauth.ui", &run).unwrap();
        assert_eq!(
            ensure_with(&fixture.config, "org.tvauth.ui", &run).unwrap(),
            UiAliasOutcome::Mounted
        );
        assert_eq!(
            fs::read(fixture.config.alias_dir.join("tv-auth-ui")).unwrap(),
            b"first build"
        );
    }

    #[test]
    fn an_upgrade_replaces_the_alias_source() {
        let fixture = fixture();
        let run = |config: &UiAliasConfig, program: &Path, arguments: &[&OsStr]| {
            fake_run(config, program, arguments)
        };
        ensure_with(&fixture.config, "org.tvauth.ui", &run).unwrap();

        // Simulate the package manager replacing the packaged file.
        let link = fixture.app_bin.join("tv-auth-ui");
        fs::remove_file(&link).unwrap();
        fs::write(&link, b"second build").unwrap();
        assert_eq!(
            ensure_with(&fixture.config, "org.tvauth.ui", &run).unwrap(),
            UiAliasOutcome::Mounted
        );
        assert_eq!(
            fs::read(fixture.config.alias_dir.join("tv-auth-ui")).unwrap(),
            b"second build"
        );
        assert_eq!(
            fs::read(fixture.app_bin.join("tv-auth-ui.real")).unwrap(),
            b"second build"
        );
    }

    #[test]
    fn restore_puts_the_packaged_file_back() {
        let fixture = fixture();
        let run = |config: &UiAliasConfig, program: &Path, arguments: &[&OsStr]| {
            fake_run(config, program, arguments)
        };
        ensure_with(&fixture.config, "org.tvauth.ui", &run).unwrap();
        restore_with(&fixture.config, "org.tvauth.ui", &run).unwrap();
        let link = fixture.app_bin.join("tv-auth-ui");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_file());
        assert_eq!(fs::read(&link).unwrap(), b"first build");
        assert!(!fixture.app_bin.join("tv-auth-ui.real").exists());
    }

    #[test]
    fn missing_package_is_not_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let config = UiAliasConfig {
            alias_dir: directory.path().join("alias"),
            package_roots: vec![directory.path().join("apps")],
            ..UiAliasConfig::default()
        };
        let run = |config: &UiAliasConfig, program: &Path, arguments: &[&OsStr]| {
            fake_run(config, program, arguments)
        };
        assert_eq!(
            ensure_with(&config, "org.tvauth.ui", &run).unwrap(),
            UiAliasOutcome::NotInstalled
        );
    }

    #[test]
    fn a_symlink_without_its_source_is_rejected() {
        let fixture = fixture();
        fs::remove_file(fixture.app_bin.join("tv-auth-ui")).unwrap();
        symlink(
            fixture.config.alias_dir.join("tv-auth-ui"),
            fixture.app_bin.join("tv-auth-ui"),
        )
        .unwrap();
        let run = |config: &UiAliasConfig, program: &Path, arguments: &[&OsStr]| {
            fake_run(config, program, arguments)
        };
        assert!(matches!(
            ensure_with(&fixture.config, "org.tvauth.ui", &run),
            Err(UiAliasError::UnsafeState(_))
        ));
    }
}
