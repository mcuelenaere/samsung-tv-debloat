//! Drive the stock WAS installer (`wascmd`) from tv-authd.
//!
//! `pkgcmd` installs packages but never contacts `webappservice`, so apps do
//! not appear in Samsung Home ("My apps"), uninstalls leave the Home tile
//! behind, and WGT packages fail outright (`-4 Load archive info fail`).
//! The WAS path (`wascmd` -> `libwas_manager_api` -> `webappservice`) does all
//! of it, but `/usr/bin/wascmd` exits immediately on retail firmware because
//! `/etc/release` exists.
//!
//! This module runs the stock `wascmd` inside a private mount namespace where
//! `/etc` is replaced by a lightweight top-level symlink farm (`/run/tv-auth/etc-norelease`)
//! pointing into a bind mount of the real `/etc` (`/run/tv-auth/etc-real`),
//! with only `release` omitted. No files are copied and nothing in the host
//! mount namespace changes.
//!
//! Install:
//!   unshare -m -- /bin/sh -c 'mount --bind <etc-src> <etc-real> && mount --bind <etc-links> /etc && exec wascmd -i <pkgid> -p <path>'
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
use std::sync::Mutex;

pub const DEFAULT_WASCMD: &str = "/usr/bin/wascmd";
pub const DEFAULT_UNSHARE: &str = "/usr/bin/unshare";
pub const DEFAULT_MOUNT: &str = "/bin/mount";
pub const DEFAULT_SHELL: &str = "/bin/sh";
pub const DEFAULT_ETC_SOURCE: &str = "/etc";
pub const DEFAULT_ETC_COPY: &str = "/run/tv-auth/etc-norelease";
pub const DEFAULT_ETC_REAL: &str = "/run/tv-auth/etc-real";

/// Serializes symlink-farm rebuilds across package-operation threads.
static PREPARE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct WasConfig {
    pub wascmd: PathBuf,
    /// `unshare`, normally `/usr/bin/unshare`.
    pub unshare: PathBuf,
    /// `mount`, normally `/bin/mount`.
    pub mount: PathBuf,
    /// Shell used for the bind-mount + exec one-liner.
    pub shell: PathBuf,
    /// Real `/etc` to expose (tests use a fixture directory).
    pub etc_source: PathBuf,
    /// Directory of top-level symlinks without `release`, bound over `/etc`.
    pub etc_copy: PathBuf,
    /// Mount point where the real `/etc` is bound inside the child namespace.
    pub etc_real: PathBuf,
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
            etc_real: PathBuf::from(DEFAULT_ETC_REAL),
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
    /// Could not build the `/etc` symlink farm.
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
    // `$0` is "sh"; positional parameters start at $1:
    //   $1 = mount binary
    //   $2 = real /etc source (normally /etc)
    //   $3 = mount point for the real /etc (/run/tv-auth/etc-real)
    //   $4 = symlink farm without `release` (/run/tv-auth/etc-norelease)
    //   $5.. = wascmd command line
    let script = "cmd=\"$1\"; src=\"$2\"; real=\"$3\"; links=\"$4\"; shift 4; \
                  \"$cmd\" --bind \"$src\" \"$real\" || exit 97; \
                  \"$cmd\" --bind \"$links\" /etc || exit 97; \
                  exec \"$@\"";
    let mut argv: Vec<&OsStr> = vec![
        OsStr::new("-m"),
        OsStr::new("--"),
        config.shell.as_os_str(),
        OsStr::new("-c"),
        OsStr::new(script),
        OsStr::new("sh"),
        config.mount.as_os_str(),
        config.etc_source.as_os_str(),
        config.etc_real.as_os_str(),
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
    let detail = if outcome.exit_code == 97 {
        "could not bind the /etc overlay".to_owned()
    } else {
        format!(
            "wascmd exited {} without '{success_marker}'",
            outcome.exit_code
        )
    };
    Err(WasError::Failed { detail, log_tail })
}

/// Build `<etc_copy>` as a top-level symlink farm for `<etc_source>` without
/// `release`, and create the empty `<etc_real>` mount point alongside it.
///
/// Only top-level entries of `/etc` are inspected (no recursion and no file
/// copies):
/// - existing top-level symlinks (such as `/etc/mtab -> ../proc/mounts`) are
///   recreated with their original target so relative `../` paths still resolve
///   from `/etc`;
/// - every other top-level file or directory is symlinked to
///   `<etc_real>/<name>`, where the child mount namespace binds the real `/etc`.
pub fn prepare_etc(config: &WasConfig) -> io::Result<()> {
    let _guard = PREPARE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if config.etc_copy.exists() {
        fs::remove_dir_all(&config.etc_copy)?;
    }
    if let Some(parent) = config.etc_copy.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = config.etc_real.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&config.etc_real)?;
    fs::set_permissions(&config.etc_real, fs::Permissions::from_mode(0o755))?;
    fs::create_dir(&config.etc_copy)?;
    fs::set_permissions(&config.etc_copy, fs::Permissions::from_mode(0o755))?;

    for entry in fs::read_dir(&config.etc_source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "release" {
            continue;
        }
        let link_path = config.etc_copy.join(&name);
        let file_type = entry.file_type()?;
        let target = if file_type.is_symlink() {
            fs::read_link(entry.path())?
        } else {
            config.etc_real.join(&name)
        };
        std::os::unix::fs::symlink(target, link_path)?;
    }
    Ok(())
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
        symlink("../proc/mounts", etc.join("mtab")).unwrap();
        let config = WasConfig {
            etc_source: etc,
            etc_copy: directory.path().join("links"),
            etc_real: directory.path().join("real"),
            ..WasConfig::new()
        };
        (directory, config)
    }

    #[test]
    fn links_top_level_etc_entries_without_release_or_copying_files() {
        let (_directory, config) = fixture();
        prepare_etc(&config).unwrap();
        let links = &config.etc_copy;
        assert!(config.etc_real.is_dir());
        assert!(links.join("release").symlink_metadata().is_err());
        assert_eq!(
            fs::read_link(links.join("passwd")).unwrap(),
            config.etc_real.join("passwd")
        );
        assert_eq!(
            fs::read_link(links.join("ssl")).unwrap(),
            config.etc_real.join("ssl")
        );
        assert_eq!(
            fs::read_link(links.join("mtab")).unwrap(),
            Path::new("../proc/mounts")
        );
        // Rebuilding replaces the previous symlink farm cleanly.
        prepare_etc(&config).unwrap();
        assert_eq!(
            fs::read_link(links.join("passwd")).unwrap(),
            config.etc_real.join("passwd")
        );
    }

    #[test]
    fn tail_is_bounded() {
        assert_eq!(tail(b"1234567890", 4), b"7890");
        assert_eq!(tail(b"123", 4), b"123");
    }
}
