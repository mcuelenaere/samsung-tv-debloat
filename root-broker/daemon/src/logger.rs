use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
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
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn log(&self, message: &str) -> io::Result<()> {
        let mut file = self.file.lock().unwrap_or_else(|error| error.into_inner());
        if file.metadata()?.len() >= MAX_LOG_BYTES {
            let _ = fs::rename(&self.path, self.path.with_extension("log.1"));
            *file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        writeln!(file, "{timestamp} {message}")
    }
}
