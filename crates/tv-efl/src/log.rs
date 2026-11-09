//! Best-effort diagnostic log that must never affect the consent UI.
//! Each binary picks its file once with [`set_path`].

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_PATH: OnceLock<&'static str> = OnceLock::new();
const MAX_LOG_BYTES: u64 = 64 * 1024;

/// Set the log file (first call wins). Logging is a no-op until set.
pub fn set_path(path: &'static str) {
    let _ = LOG_PATH.set(path);
}

pub fn log_line(message: &str) {
    let _ = append(message);
}

fn append(message: &str) -> std::io::Result<()> {
    let Some(path) = LOG_PATH.get() else {
        return Ok(());
    };
    let path = Path::new(path);
    if let Ok(metadata) = fs::symlink_metadata(path) {
        // Never follow a planted symlink; /tmp is shared with every app.
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        if metadata.len() > MAX_LOG_BYTES {
            let _ = fs::remove_file(path);
        }
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    set_mode(&file)?;
    writeln!(file, "{timestamp} {message}")
}

#[cfg(unix)]
fn set_mode(file: &File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_mode(_file: &File) -> std::io::Result<()> {
    Ok(())
}
