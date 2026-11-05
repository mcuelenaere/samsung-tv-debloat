use crate::identity::is_valid_package_label;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
        let euid = rustix::process::geteuid().as_raw();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path);
        let file_missing = matches!(&opened, Err(e) if e.kind() == io::ErrorKind::NotFound);
        match fs::metadata(parent) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "grant directory is not a directory",
                    ));
                }
                check_trusted(&metadata, euid, "grant directory")?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && file_missing => {}
            Err(error) => return Err(error),
        }
        let mut bytes = Vec::new();
        match opened {
            Ok(mut file) => {
                let metadata = file.metadata()?;
                if !metadata.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "grant file is not a regular file",
                    ));
                }
                check_trusted(&metadata, euid, "grant file")?;
                file.read_to_end(&mut bytes)?;
            }
            Err(_) if file_missing => {
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

    pub fn is_allowed(&self, label: &str) -> bool {
        self.grants.contains_key(label)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, u64)> {
        self.grants
            .iter()
            .map(|(label, entry)| (label.as_str(), entry.granted_at))
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
        if let Err(error) = self.save() {
            self.grants.remove(label);
            return Err(error);
        }
        Ok(true)
    }

    pub fn revoke(&mut self, label: &str) -> io::Result<bool> {
        let Some(removed) = self.grants.remove(label) else {
            return Ok(false);
        };
        if let Err(error) = self.save() {
            self.grants.insert(label.to_owned(), removed);
            return Err(error);
        }
        Ok(true)
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
        let open = || {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)
        };
        let mut output = match open() {
            // Stale temp from a crash (or a planted symlink): remove the entry itself, retry once.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary)?;
                open()?
            }
            result => result?,
        };
        let written = output
            .write_all(&bytes)
            .and_then(|()| output.sync_all())
            .and_then(|()| fs::rename(&temporary, &self.path));
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

fn check_trusted(metadata: &fs::Metadata, euid: u32, what: &str) -> io::Result<()> {
    if metadata.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{what} is not owned by the daemon user"),
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{what} is group- or world-writable"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
    fn rejects_group_or_world_writable_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants.json");
        fs::write(&path, r#"{"version":1,"grants":{}}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        for mode in [0o770, 0o707] {
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(mode)).unwrap();
            assert!(GrantStore::load(&path).is_err());
            // also rejected when the grant file does not exist yet
            assert!(GrantStore::load(directory.path().join("missing.json")).is_err());
        }
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(GrantStore::load(&path).is_ok());
    }

    #[test]
    fn rejects_symlinked_grant_file() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real.json");
        fs::write(&real, r#"{"version":1,"grants":{}}"#).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("grants.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(GrantStore::load(&link).is_err());
    }

    #[test]
    fn save_does_not_follow_symlink_at_temp_path() {
        let (directory, mut store) = temp_store();
        let target = directory.path().join("victim");
        fs::write(&target, b"sentinel").unwrap();
        let temporary = directory
            .path()
            .join(format!(".grants.json.tmp.{}", std::process::id()));
        std::os::unix::fs::symlink(&target, &temporary).unwrap();

        assert!(store.grant("User::Pkg::org.example.new").unwrap());
        assert_eq!(fs::read(&target).unwrap(), b"sentinel");
        assert!(fs::symlink_metadata(&temporary).is_err());
        drop(store);
        let store = GrantStore::load(directory.path().join("grants.json")).unwrap();
        assert!(store.is_allowed("User::Pkg::org.example.new"));
    }

    #[test]
    fn failed_save_rolls_back_grant_and_revoke() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("grants");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("grants.json");
        let mut store = GrantStore::load(&path).unwrap();
        fs::remove_dir(&parent).unwrap();
        fs::write(&parent, b"blocker").unwrap();

        assert!(store.grant("User::Pkg::org.example.app").is_err());
        assert!(!store.is_allowed("User::Pkg::org.example.app"));

        fs::remove_file(&parent).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(
            &path,
            r#"{"version":1,"grants":{"User::Pkg::org.example.keep":{"granted_at":1}}}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut store = GrantStore::load(&path).unwrap();
        fs::remove_dir_all(&parent).unwrap();
        fs::write(&parent, b"blocker").unwrap();
        assert!(store.revoke("User::Pkg::org.example.keep").is_err());
        assert!(store.is_allowed("User::Pkg::org.example.keep"));
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
