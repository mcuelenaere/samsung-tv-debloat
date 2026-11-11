use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 256 * 1024;

pub struct Logger {
    path: PathBuf,
    file: Mutex<File>,
}

impl Logger {
    pub fn new(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = open_private(&path)?;
        // Fix a log file created by an older build with default permissions.
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn log(&self, message: &str) -> io::Result<()> {
        let mut file = self.file.lock().unwrap_or_else(|error| error.into_inner());
        if file.metadata()?.len() >= MAX_LOG_BYTES {
            let rotated = self.path.with_extension("log.1");
            let _ = fs::rename(&self.path, &rotated);
            let _ = fs::set_permissions(&rotated, fs::Permissions::from_mode(0o600));
            *file = open_private(&self.path)?;
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        writeln!(file, "{timestamp} {message}")
    }

    /// Lines worth showing a user from the newest `scan_bytes` of the log:
    /// approval outcomes, prompt decisions, and package changes, without the
    /// per-exec chatter of granted apps. At most `limit`, oldest first.
    pub fn audit(&self, scan_bytes: u64, limit: usize) -> io::Result<Vec<String>> {
        let mut file = File::open(&self.path)?;
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(scan_bytes)))?;
        let mut tail = Vec::new();
        file.take(scan_bytes).read_to_end(&mut tail)?;
        Ok(audit_lines(&String::from_utf8_lossy(&tail), limit))
    }
}

fn audit_lines(log: &str, limit: usize) -> Vec<String> {
    const EVENTS: &[&str] = &[
        " approval=",
        " prompt_decision ",
        " result=installed",
        " result=uninstalled",
        " result=failed",
        " denied=",
        " grant_revoked ",
        " access=",
    ];
    let mut lines: Vec<String> = log
        .lines()
        // The first line of a mid-file tail may be cut; it never parses as
        // "<digits> <text>" reliably, so require the timestamp prefix.
        .filter(|line| {
            line.split(' ')
                .next()
                .is_some_and(|ts| !ts.is_empty() && ts.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter(|line| EVENTS.iter().any(|event| line.contains(event)))
        .map(str::to_owned)
        .collect();
    let excess = lines.len().saturating_sub(limit);
    lines.drain(..excess);
    lines
}

fn open_private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_keeps_decisions_and_package_changes() {
        let log = "ed request=x\n\
                   100 requester=User::Pkg::a request=1 execute\n\
                   101 requester=User::Pkg::a request=1 exit_code=0 timed_out=false\n\
                   102 requester=User::Pkg::b request=2 approval=denied error=user_denied\n\
                   103 prompt_decision label=User::Pkg::c request=3 kind=exec decision=Once\n\
                   104 requester=System::Privileged request=4 pkgid=p version=1 backend=was result=installed\n\
                   105 grant_revoked label=User::Pkg::c revoked=true\n\
                   106 access_enforced ssh=off stopped=dropbear.service\n\
                   107 access=ssh_key_added fingerprint=SHA256:x comment=me by=user\n";
        let lines = audit_lines(log, 3);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("104 "));
        assert!(lines[2].starts_with("107 "), "access changes are audited");
        assert_eq!(audit_lines(log, 10).len(), 5, "enforcement passes are not");
    }

    #[test]
    fn creates_a_private_log_and_fixes_old_modes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tv-authd.log");
        let logger = Logger::new(&path).unwrap();
        logger.log("hello").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let logger = Logger::new(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        logger.log("again").unwrap();
    }

    #[test]
    fn refuses_a_symlinked_log() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::write(&target, b"sentinel").unwrap();
        let path = directory.path().join("tv-authd.log");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(Logger::new(&path).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"sentinel");
    }
}
