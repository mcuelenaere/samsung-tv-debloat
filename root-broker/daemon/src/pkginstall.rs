//! Thin wrapper around the stock `pkgcmd` installer.
//!
//! `pkgcmd` is the same backend the app store uses. It performs signature
//! validation, privilege checks, and the actual install; this module only
//! builds the argument vector, enforces the timeout, and translates its
//! `key[...] val[...]` output into the daemon's result shape.

use crate::executor;
use crate::protocol::{PackageType, MAX_INSTALL_LOG_TAIL_BYTES};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum InstallError {
    /// Could not start `pkgcmd` at all.
    Spawn(io::Error),
    /// `pkgcmd` ran but did not report success (including timeouts).
    Failed { detail: String, log_tail: Vec<u8> },
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "cannot run pkgcmd: {error}"),
            Self::Failed { detail, .. } => write!(formatter, "pkgcmd failed: {detail}"),
        }
    }
}

#[derive(Debug)]
pub struct InstallOutcome {
    pub log_tail: Vec<u8>,
}

/// Install `package` with `pkgcmd -i -t <type> -p <path>`.
pub fn install(
    pkgcmd: &Path,
    pkg_type: PackageType,
    package: &Path,
    timeout_ms: u64,
) -> Result<InstallOutcome, InstallError> {
    let args: [&OsStr; 5] = [
        OsStr::new("-i"),
        OsStr::new("-t"),
        OsStr::new(pkg_type.as_str()),
        OsStr::new("-p"),
        package.as_os_str(),
    ];
    run(pkgcmd, &args, timeout_ms)
}

/// Uninstall `pkgid` with `pkgcmd -u -t <type> -n <pkgid>`.
///
/// `-p` is the *package path* for uninstall, which fails with `realpath fail`
/// when given an id; `-n` takes the package name.
pub fn uninstall(
    pkgcmd: &Path,
    pkg_type: PackageType,
    pkgid: &str,
    timeout_ms: u64,
) -> Result<InstallOutcome, InstallError> {
    let args: [&OsStr; 5] = [
        OsStr::new("-u"),
        OsStr::new("-t"),
        OsStr::new(pkg_type.as_str()),
        OsStr::new("-n"),
        OsStr::new(pkgid),
    ];
    run(pkgcmd, &args, timeout_ms)
}

fn run(pkgcmd: &Path, args: &[&OsStr], timeout_ms: u64) -> Result<InstallOutcome, InstallError> {
    let outcome = executor::execute_argv(pkgcmd, args, timeout_ms).map_err(InstallError::Spawn)?;

    let mut log = outcome.stdout;
    log.extend_from_slice(&outcome.stderr);
    let log_tail = tail(&log, MAX_INSTALL_LOG_TAIL_BYTES);

    if outcome.timed_out {
        return Err(InstallError::Failed {
            detail: "pkgcmd timed out".into(),
            log_tail,
        });
    }

    let end = last_end_value(&log);
    match end {
        Some(status) if status == "ok" && outcome.exit_code == 0 => Ok(InstallOutcome { log_tail }),
        Some(status) => Err(InstallError::Failed {
            detail: format!("pkgcmd reported {status} (exit {})", outcome.exit_code),
            log_tail,
        }),
        None => Err(InstallError::Failed {
            detail: format!("pkgcmd exited {} without a status line", outcome.exit_code),
            log_tail,
        }),
    }
}

/// Last `key[end] val[<value>]` status in `pkgcmd` output. `pkgcmd` prefixes
/// the pair with a `__return_cb ...` prologue, so scan the whole line.
pub fn last_end_value(log: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(log);
    let mut end = None;
    for line in text.lines() {
        let mut rest = line;
        while let Some(position) = rest.find("key[") {
            let after = &rest[position + 4..];
            let Some((key, value)) = after.split_once("] val[") else {
                break;
            };
            let Some(value) = value.split(']').next() else {
                break;
            };
            if key == "end" {
                end = Some(value.trim().to_owned());
            }
            let consumed = position + 4 + key.len() + "] val[".len() + value.len() + 1;
            if consumed >= rest.len() {
                break;
            }
            rest = &rest[consumed..];
        }
    }
    end
}

/// True when a directory for `pkgid` already exists under one of the known
/// install roots. This only drives the `is_upgrade` hint; the installer decides.
pub fn is_installed(pkgid: &str, roots: &[PathBuf]) -> bool {
    if !crate::identity::is_valid_package_id(pkgid) {
        return false;
    }
    roots.iter().any(|root| root.join(pkgid).is_dir())
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

    #[test]
    fn parses_pkgcmd_status_lines() {
        let log = b"key[install path] val[/opt/usr/apps/org.example]\n\
                    key[installed-pkgid] val[org.example.app]\n\
                    key[end] val[ok]\n";
        assert_eq!(last_end_value(log).as_deref(), Some("ok"));
        let log = b"key[start] val[ok]\r\nkey[end] val[-12]\r\n";
        assert_eq!(last_end_value(log).as_deref(), Some("-12"));
        assert_eq!(last_end_value(b"no status here"), None);
        assert_eq!(
            last_end_value(b"key[end] val[ok] trailing").as_deref(),
            Some("ok")
        );
        // Real pkgcmd output prefixes the pair with __return_cb fields.
        let real = b"__return_cb req_id[1] pkg_type[tpk] pkgid[org.example.app] \
                    key[end] val[ok]\n";
        assert_eq!(last_end_value(real).as_deref(), Some("ok"));
        let failed = b"__return_cb req_id[1] pkg_type[] pkgid[] key[error] val[-4] \
                      error message: :Load archive info fail\n\
                      __return_cb req_id[1] pkg_type[] pkgid[] key[end] val[fail]\n";
        assert_eq!(last_end_value(failed).as_deref(), Some("fail"));
    }

    #[test]
    fn tail_is_bounded() {
        let log = vec![b'x'; 100];
        assert_eq!(tail(&log, 10), vec![b'x'; 10]);
        assert_eq!(tail(&log, 1000), log);
    }

    #[test]
    fn detects_installed_packages() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("apps");
        std::fs::create_dir_all(root.join("org.example.app")).unwrap();
        assert!(is_installed("org.example.app", std::slice::from_ref(&root)));
        assert!(!is_installed("org.example.missing", &[root]));
        assert!(!is_installed(
            "../../etc",
            &[directory.path().to_path_buf()]
        ));
    }
}
