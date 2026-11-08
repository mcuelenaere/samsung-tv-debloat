//! Drive the stock WAS installer (`wascmd`) from tv-authd.
//!
//! `pkgcmd` installs packages but never contacts `webappservice`, so apps do
//! not appear in Samsung Home ("My apps"), uninstalls leave the Home tile
//! behind, and WGT packages fail outright (`-4 Load archive info fail`).
//! The WAS path (`wascmd` -> `libwas_manager_api` -> `webappservice`) does all
//! of it, but `/usr/bin/wascmd` exits immediately on retail firmware because
//! `/etc/release` exists.
//!
//! This module runs the stock `wascmd` inside a private mount namespace where a
//! copy of `/etc` **without** `release` is bound over `/etc`. Nothing in the
//! host namespace changes, and the binary is the same Samsung-signed one
//! `sdb install` drives in-process through `sdbd`.
//!
//! Install:
//!   unshare -m -- /bin/sh -c 'mount --bind <etc-copy> /etc; exec wascmd -i <pkgid> -p <path>'
//! Uninstall:
//!   ... wascmd -u <appid>
//!
//! `wascmd -u` refuses packages that were not installed through the SDK path
//! (`is_sdk_app`), so callers fall back to `pkgcmd -u` plus an explicit Home
//! cleanup for those.

use crate::executor;
use crate::protocol::MAX_INSTALL_LOG_TAIL_BYTES;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const DEFAULT_WASCMD: &str = "/usr/bin/wascmd";
pub const DEFAULT_UNSHARE: &str = "/usr/bin/unshare";
pub const DEFAULT_MOUNT: &str = "/bin/mount";
pub const DEFAULT_SHELL: &str = "/bin/sh";
pub const DEFAULT_ETC_SOURCE: &str = "/etc";
pub const DEFAULT_ETC_COPY: &str = "/run/tv-auth/etc-norelease";

#[derive(Debug, Clone)]
pub struct WasConfig {
    pub wascmd: PathBuf,
    /// `unshare`, normally `/usr/bin/unshare`.
    pub unshare: PathBuf,
    /// `mount`, normally `/bin/mount`.
    pub mount: PathBuf,
    /// Shell used for the bind-mount + exec one-liner.
    pub shell: PathBuf,
    /// Real `/etc` to copy (tests use a fixture directory).
    pub etc_source: PathBuf,
    /// Where the copy without `release` lives.
    pub etc_copy: PathBuf,
}

impl WasConfig {
    pub fn new() -> Self {
        Self {
            wascmd: PathBuf::from(DEFAULT_WASCMD),
            unshare: PathBuf::from(DEFAULT_UNSHARE),
            mount: PathBuf::from(DEFAULT_MOUNT),
            shell: PathBuf::from(DEFAULT_SHELL),
            etc_source: PathBuf::from(DEFAULT_ETC_SOURCE),
            etc_copy: PathBuf::from(DEFAULT_ETC_COPY),
        }
    }
}

impl Default for WasConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum WasError {
    /// Could not spawn `unshare`.
    Spawn(io::Error),
    /// Could not build or copy the `/etc` overlay.
    Etc(io::Error),
    /// `wascmd` ran but did not report success.
    Failed { detail: String, log_tail: Vec<u8> },
}

impl std::fmt::Display for WasError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "cannot run unshare: {error}"),
            Self::Etc(error) => write!(formatter, "cannot prepare the /etc overlay: {error}"),
            Self::Failed { detail, .. } => write!(formatter, "wascmd failed: {detail}"),
        }
    }
}

#[derive(Debug)]
pub struct WasOutcome {
    pub log_tail: Vec<u8>,
}

/// Install `package` through `webappservice`. `pkgid` is the manifest id
/// (wascmd takes it on the command line).
pub fn install(
    config: &WasConfig,
    pkgid: &str,
    package: &Path,
    timeout_ms: u64,
) -> Result<WasOutcome, WasError> {
    let args: [&OsStr; 5] = [
        config.wascmd.as_os_str(),
        OsStr::new("-i"),
        OsStr::new(pkgid),
        OsStr::new("-p"),
        package.as_os_str(),
    ];
    run(config, &args, "install completed", timeout_ms)
}

/// Remove `appid` (the manifest `mainappid`, e.g. `vetrotvapp.app`) through
/// `webappservice`.
pub fn uninstall(config: &WasConfig, appid: &str, timeout_ms: u64) -> Result<WasOutcome, WasError> {
    let args: [&OsStr; 3] = [
        config.wascmd.as_os_str(),
        OsStr::new("-u"),
        OsStr::new(appid),
    ];
    run(config, &args, "uninstall completed", timeout_ms)
}

fn run(
    config: &WasConfig,
    args: &[&OsStr],
    success_marker: &str,
    timeout_ms: u64,
) -> Result<WasOutcome, WasError> {
    prepare_etc(config).map_err(WasError::Etc)?;
    // `$0` is "sh"; positional parameters start at $1. `$1` is the mount
    // binary and `$2` the overlay, so the rest is the wascmd command line.
    let script =
        "cmd=\"$1\"; src=\"$2\"; shift 2; \"$cmd\" --bind \"$src\" /etc || exit 97; exec \"$@\"";
    let mut argv: Vec<&OsStr> = vec![
        OsStr::new("-m"),
        OsStr::new("--"),
        config.shell.as_os_str(),
        OsStr::new("-c"),
        OsStr::new(script),
        OsStr::new("sh"),
        config.mount.as_os_str(),
        config.etc_copy.as_os_str(),
    ];
    argv.extend_from_slice(args);

    let outcome =
        executor::execute_argv(&config.unshare, &argv, timeout_ms).map_err(WasError::Spawn)?;
    let mut log = outcome.stdout;
    log.extend_from_slice(&outcome.stderr);
    let log_tail = tail(&log, MAX_INSTALL_LOG_TAIL_BYTES);

    if outcome.timed_out {
        return Err(WasError::Failed {
            detail: "wascmd timed out".into(),
            log_tail,
        });
    }
    let text = String::from_utf8_lossy(&log);
    if outcome.exit_code == 0 && text.contains(success_marker) {
        return Ok(WasOutcome { log_tail });
    }
    let detail = if text.contains("mount") && outcome.exit_code == 97 {
        "could not bind the /etc overlay".to_owned()
    } else {
        format!(
            "wascmd exited {} without '{success_marker}'",
            outcome.exit_code
        )
    };
    Err(WasError::Failed { detail, log_tail })
}

/// Build `<etc_copy>` as a copy of `<etc_source>` without `release`.
///
/// The copy is rebuilt on every operation: `/etc` is only 12 MiB and this
/// keeps dynamic-linker/NSS files fresh. It lives on the `/run` tmpfs, next to
/// the daemon log, and is removed when the daemon exits with the tmpfs.
pub fn prepare_etc(config: &WasConfig) -> io::Result<()> {
    if config.etc_copy.exists() {
        fs::remove_dir_all(&config.etc_copy)?;
    }
    if let Some(parent) = config.etc_copy.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(&config.etc_copy)?;
    fs::set_permissions(&config.etc_copy, fs::Permissions::from_mode(0o700))?;
    copy_tree(&config.etc_source, &config.etc_copy, true)
}

fn copy_tree(source: &Path, destination: &Path, root: bool) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if root && name == "release" {
            continue;
        }
        let target = destination.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, target)?;
        } else if file_type.is_dir() {
            fs::create_dir(&target)?;
            copy_permissions(&entry.path(), &target)?;
            copy_tree(&entry.path(), &target, false)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
            copy_permissions(&entry.path(), &target)?;
        }
        // Sockets, FIFOs, and device nodes are not needed by wascmd.
    }
    Ok(())
}

fn copy_permissions(source: &Path, target: &Path) -> io::Result<()> {
    let mode = fs::metadata(source)?.permissions().mode();
    fs::set_permissions(target, fs::Permissions::from_mode(mode))
}

fn tail(log: &[u8], maximum: usize) -> Vec<u8> {
    if log.len() <= maximum {
        return log.to_vec();
    }
    log[log.len() - maximum..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, WasConfig) {
        let directory = tempfile::tempdir().unwrap();
        let etc = directory.path().join("etc");
        fs::create_dir(&etc).unwrap();
        fs::write(etc.join("passwd"), b"root:x:0:0").unwrap();
        fs::write(etc.join("release"), b"").unwrap();
        fs::create_dir(etc.join("ssl")).unwrap();
        fs::write(etc.join("ssl/cert.pem"), b"cert").unwrap();
        symlink("/proc/mounts", etc.join("mtab")).unwrap();
        let config = WasConfig {
            etc_source: etc,
            etc_copy: directory.path().join("copy"),
            ..WasConfig::new()
        };
        (directory, config)
    }

    #[test]
    fn copies_etc_without_release_and_keeps_symlinks() {
        let (_directory, config) = fixture();
        prepare_etc(&config).unwrap();
        let copy = &config.etc_copy;
        assert!(copy.join("passwd").exists());
        assert!(copy.join("ssl/cert.pem").exists());
        assert!(copy.join("release").symlink_metadata().is_err());
        assert_eq!(
            fs::read_link(copy.join("mtab")).unwrap(),
            Path::new("/proc/mounts")
        );
        // Rebuilding replaces the previous copy.
        prepare_etc(&config).unwrap();
        assert!(copy.join("passwd").exists());
    }

    #[test]
    fn tail_is_bounded() {
        assert_eq!(tail(b"1234567890", 4), b"7890");
        assert_eq!(tail(b"123", 4), b"123");
    }
}
