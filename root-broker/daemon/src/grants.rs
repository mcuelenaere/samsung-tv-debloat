use crate::identity::is_valid_package_label;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const GRANT_FILE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantFile {
    version: u32,
    grants: BTreeMap<String, GrantEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantEntry {
    granted_at: u64,
}

pub struct GrantStore {
    path: PathBuf,
    grants: BTreeMap<String, GrantEntry>,
}

impl GrantStore {
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let mut bytes = Vec::new();
        match File::open(&path) {
            Ok(mut file) => {
                let metadata = file.metadata()?;
                if metadata.permissions().mode() & 0o022 != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "grant file is group- or world-writable",
                    ));
                }
                file.read_to_end(&mut bytes)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    grants: BTreeMap::new(),
                });
            }
            Err(error) => return Err(error),
        }

        let file: GrantFile = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if file.version != GRANT_FILE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported grant file version {}", file.version),
            ));
        }
        for label in file.grants.keys() {
            if !is_valid_package_label(label) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid package label in grant file: {label}"),
                ));
            }
        }
        Ok(Self {
            path,
            grants: file.grants,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_allowed(&self, label: &str) -> bool {
        self.grants.contains_key(label)
    }

    pub fn grant(&mut self, label: &str) -> io::Result<bool> {
        if !is_valid_package_label(label) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid package label",
            ));
        }
        if self.grants.contains_key(label) {
            return Ok(false);
        }
        let granted_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.grants
            .insert(label.to_owned(), GrantEntry { granted_at });
        self.save()?;
        Ok(true)
    }

    pub fn revoke(&mut self, label: &str) -> io::Result<bool> {
        let removed = self.grants.remove(label).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> io::Result<()> {
        let file = GrantFile {
            version: GRANT_FILE_VERSION,
            grants: self.grants.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".{}.tmp.{}",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("grants"),
            std::process::id()
        ));
        let mut output = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        fs::rename(&temporary, &self.path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, GrantStore) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants.json");
        fs::write(
            &path,
            r#"{"version":1,"grants":{"User::Pkg::org.example.allowed":{"granted_at":1}}}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let store = GrantStore::load(&path).unwrap();
        (directory, store)
    }

    #[test]
    fn loads_and_matches_exact_label() {
        let (_directory, store) = temp_store();
        assert!(store.is_allowed("User::Pkg::org.example.allowed"));
        assert!(!store.is_allowed("User::Pkg::org.example.other"));
    }

    #[test]
    fn rejects_world_writable_grant_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants.json");
        fs::write(&path, r#"{"version":1,"grants":{}}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(GrantStore::load(path).is_err());
    }

    #[test]
    fn grant_and_revoke_are_atomic_and_persistent() {
        let (directory, mut store) = temp_store();
        assert!(store.grant("User::Pkg::org.example.new").unwrap());
        assert!(!store.grant("User::Pkg::org.example.new").unwrap());
        drop(store);

        let mut store = GrantStore::load(directory.path().join("grants.json")).unwrap();
        assert!(store.is_allowed("User::Pkg::org.example.new"));
        assert!(store.revoke("User::Pkg::org.example.new").unwrap());
        drop(store);

        let store = GrantStore::load(directory.path().join("grants.json")).unwrap();
        assert!(!store.is_allowed("User::Pkg::org.example.new"));
    }
}
