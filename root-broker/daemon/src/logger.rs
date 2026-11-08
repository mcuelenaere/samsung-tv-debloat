use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
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
