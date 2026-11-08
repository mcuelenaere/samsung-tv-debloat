use crate::identity::is_valid_package_label;
use crate::package_identity::{default_roots, package_fingerprint};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const GRANT_FILE_VERSION: u32 = 2;
const MIN_GRANT_FILE_VERSION: u32 = 1;

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
    /// Omitted when `every`, so files without custom modes stay readable by
    /// older daemons.
    #[serde(default, skip_serializing_if = "NotifyMode::is_default")]
    notify: NotifyMode,
    /// Hash of the installed package's author signature. Version-1 grants
    /// predate this field and are migrated on first use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_fingerprint: Option<String>,
}

/// When a granted package's executions are shown as a UI toast.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyMode {
    /// Every command.
    #[default]
    Every,
    /// The first command after the package was idle for a while.
    First,
    /// Never.
    Never,
}

impl NotifyMode {
    fn is_default(&self) -> bool {
        *self == Self::Every
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantCheck {
    Allowed,
    /// A legacy label-only grant was bound to the current package identity.
    Migrated,
    NotGranted,
    /// The installed package changed since the grant was recorded.
    Stale,
    /// The installed package identity could not be established.
    IdentityUnavailable,
}

impl GrantCheck {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed | Self::Migrated)
    }
}

#[derive(Debug)]
pub enum GrantError {
    InvalidLabel,
    /// The installed package's author signature was missing or untrusted.
    PackageIdentity(io::Error),
    /// The grant was bound to the package but could not be persisted.
    Save(io::Error),
}

impl fmt::Display for GrantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLabel => write!(formatter, "invalid package label"),
            Self::PackageIdentity(error) => {
                write!(formatter, "package identity unavailable: {error}")
            }
            Self::Save(error) => write!(formatter, "grant save failed: {error}"),
        }
    }
}

impl std::error::Error for GrantError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidLabel => None,
            Self::PackageIdentity(error) | Self::Save(error) => Some(error),
        }
    }
}

pub struct GrantStore {
    path: PathBuf,
    grants: BTreeMap<String, GrantEntry>,
    package_roots: Vec<PathBuf>,
}

impl GrantStore {
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        Self::load_with_roots(path, default_roots())
    }

    pub fn load_with_roots(
        path: impl Into<PathBuf>,
        package_roots: Vec<PathBuf>,
    ) -> io::Result<Self> {
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
                    package_roots,
                });
            }
            Err(error) => return Err(error),
        }

        let file: GrantFile = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !(MIN_GRANT_FILE_VERSION..=GRANT_FILE_VERSION).contains(&file.version) {
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
            package_roots,
        })
    }

    /// Check a grant against the currently installed package identity.
    /// Legacy grants are migrated in place; changed or unverifiable package
    /// identities remove the grant and deny.
    pub fn check(&mut self, label: &str) -> GrantCheck {
        if !is_valid_package_label(label) {
            return GrantCheck::NotGranted;
        }
        let Some(entry) = self.grants.get(label) else {
            return GrantCheck::NotGranted;
        };
        let stored = entry.package_fingerprint.clone();
        match package_fingerprint(label, &self.package_roots) {
            Ok(current) => match stored {
                Some(stored) if stored == current => GrantCheck::Allowed,
                Some(_) => {
                    self.remove_and_save(label);
                    GrantCheck::Stale
                }
                None => {
                    if let Some(entry) = self.grants.get_mut(label) {
                        entry.package_fingerprint = Some(current);
                    }
                    let _ = self.save();
                    GrantCheck::Migrated
                }
            },
            Err(_) => {
                self.remove_and_save(label);
                GrantCheck::IdentityUnavailable
            }
        }
    }

    /// The grant's notify mode, or `None` when the label has no grant.
    pub fn notify_mode(&self, label: &str) -> Option<NotifyMode> {
        self.grants.get(label).map(|entry| entry.notify)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, u64, NotifyMode)> {
        self.grants
            .iter()
            .map(|(label, entry)| (label.as_str(), entry.granted_at, entry.notify))
    }

    /// Returns `false` if the label has no grant.
    pub fn set_notify(&mut self, label: &str, mode: NotifyMode) -> io::Result<bool> {
        let Some(entry) = self.grants.get_mut(label) else {
            return Ok(false);
        };
        let previous = std::mem::replace(&mut entry.notify, mode);
        if let Err(error) = self.save() {
            if let Some(entry) = self.grants.get_mut(label) {
                entry.notify = previous;
            }
            return Err(error);
        }
        Ok(true)
    }

    pub fn grant(&mut self, label: &str) -> Result<bool, GrantError> {
        if !is_valid_package_label(label) {
            return Err(GrantError::InvalidLabel);
        }
        // Bind the grant to the installed package before persisting anything.
        // A missing/untrusted signature is an error, so "Always allow" fails
        // closed instead of creating a label-only grant.
        let fingerprint =
            package_fingerprint(label, &self.package_roots).map_err(GrantError::PackageIdentity)?;
        let granted_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let notify = self
            .grants
            .get(label)
            .map(|entry| entry.notify)
            .unwrap_or_default();
        let previous = self.grants.insert(
            label.to_owned(),
            GrantEntry {
                granted_at,
                notify,
                package_fingerprint: Some(fingerprint),
            },
        );
        if let Err(error) = self.save() {
            match previous {
                Some(previous) => {
                    self.grants.insert(label.to_owned(), previous);
                }
                None => {
                    self.grants.remove(label);
                }
            }
            return Err(GrantError::Save(error));
        }
        Ok(previous.is_none())
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

    fn remove_and_save(&mut self, label: &str) {
        self.grants.remove(label);
        let _ = self.save();
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

    fn write_signature(root: &Path, package: &str, contents: &str) -> PathBuf {
        let directory = root.join(package);
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        let signature = directory.join("author-signature.xml");
        fs::write(&signature, contents).unwrap();
        fs::set_permissions(&signature, fs::Permissions::from_mode(0o644)).unwrap();
        signature
    }

    fn temp_store() -> (tempfile::TempDir, GrantStore) {
        let directory = tempfile::tempdir().unwrap();
        let apps = directory.path().join("apps");
        fs::create_dir(&apps).unwrap();
        write_signature(&apps, "org.example.allowed", "sig-allowed");
        write_signature(&apps, "org.example.new", "sig-new");
        let path = directory.path().join("grants.json");
        fs::write(
            &path,
            r#"{"version":1,"grants":{"User::Pkg::org.example.allowed":{"granted_at":1}}}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let store = GrantStore::load_with_roots(&path, vec![apps]).unwrap();
        (directory, store)
    }

    #[test]
    fn loads_and_matches_exact_label() {
        let (_directory, mut store) = temp_store();
        assert_eq!(
            store.check("User::Pkg::org.example.allowed"),
            GrantCheck::Migrated
        );
        assert_eq!(
            store.check("User::Pkg::org.example.allowed"),
            GrantCheck::Allowed
        );
        assert_eq!(
            store.check("User::Pkg::org.example.other"),
            GrantCheck::NotGranted
        );
    }

    #[test]
    fn legacy_grant_is_migrated_and_preserves_notify_mode() {
        let (directory, mut store) = temp_store();
        let label = "User::Pkg::org.example.allowed";
        assert_eq!(store.notify_mode(label), Some(NotifyMode::Every));
        assert_eq!(store.check(label), GrantCheck::Migrated);
        assert_eq!(store.check(label), GrantCheck::Allowed);
        drop(store);

        let bytes = fs::read(directory.path().join("grants.json")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["version"], 2);
        assert!(value["grants"][label]["package_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));

        let apps = directory.path().join("apps");
        let mut store =
            GrantStore::load_with_roots(directory.path().join("grants.json"), vec![apps]).unwrap();
        assert_eq!(store.check(label), GrantCheck::Allowed);
    }

    #[test]
    fn migration_preserves_a_custom_notify_mode() {
        let directory = tempfile::tempdir().unwrap();
        let apps = directory.path().join("apps");
        fs::create_dir(&apps).unwrap();
        write_signature(&apps, "org.example.allowed", "sig-allowed");
        let path = directory.path().join("grants.json");
        fs::write(
            &path,
            r#"{"version":1,"grants":{"User::Pkg::org.example.allowed":{"granted_at":1,"notify":"first"}}}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut store = GrantStore::load_with_roots(&path, vec![apps]).unwrap();
        assert_eq!(
            store.check("User::Pkg::org.example.allowed"),
            GrantCheck::Migrated
        );
        assert_eq!(
            store.notify_mode("User::Pkg::org.example.allowed"),
            Some(NotifyMode::First)
        );
        drop(store);

        let bytes = fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["version"], 2);
        assert_eq!(
            value["grants"]["User::Pkg::org.example.allowed"]["notify"],
            "first"
        );
        assert!(
            value["grants"]["User::Pkg::org.example.allowed"]["package_fingerprint"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn changed_package_identity_invalidates_the_grant() {
        let (directory, mut store) = temp_store();
        let label = "User::Pkg::org.example.new";
        assert!(store.grant(label).unwrap());
        assert_eq!(store.check(label), GrantCheck::Allowed);

        let apps = directory.path().join("apps");
        fs::write(
            apps.join("org.example.new/author-signature.xml"),
            b"sig-new-replaced",
        )
        .unwrap();
        assert_eq!(store.check(label), GrantCheck::Stale);
        assert_eq!(store.check(label), GrantCheck::NotGranted);
    }

    #[test]
    fn missing_package_identity_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let apps = directory.path().join("apps");
        fs::create_dir(&apps).unwrap();
        let path = directory.path().join("grants.json");
        fs::write(
            &path,
            r#"{"version":2,"grants":{"User::Pkg::org.example.gone":{"granted_at":1,"package_fingerprint":"sha256:old"}}}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut store = GrantStore::load_with_roots(&path, vec![apps]).unwrap();
        assert_eq!(
            store.check("User::Pkg::org.example.gone"),
            GrantCheck::IdentityUnavailable
        );
        assert_eq!(
            store.check("User::Pkg::org.example.gone"),
            GrantCheck::NotGranted
        );
    }

    #[test]
    fn grant_requires_a_verifiable_package_identity() {
        let (_directory, mut store) = temp_store();
        assert!(matches!(
            store.grant("User::Pkg::org.example.missing"),
            Err(GrantError::PackageIdentity(_))
        ));
    }

    #[test]
    fn rejects_corrupt_and_unsupported_grant_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants.json");
        for body in [
            "{not-json}".as_bytes(),
            br#"{"version":3,"grants":{}}"#.as_slice(),
            br#"{"version":2,"grants":{"User::Pkg::bad/label":{"granted_at":1}}}"#.as_slice(),
        ] {
            fs::write(&path, body).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(GrantStore::load(&path).is_err());
        }
    }

    #[test]
    fn rejects_world_writable_grant_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants.json");
        fs::write(&path, r#"{"version":2,"grants":{}}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(GrantStore::load(path).is_err());
    }

    #[test]
    fn rejects_group_or_world_writable_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants.json");
        fs::write(&path, r#"{"version":2,"grants":{}}"#).unwrap();
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
        fs::write(&real, r#"{"version":2,"grants":{}}"#).unwrap();
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
        let apps = directory.path().join("apps");
        let mut store =
            GrantStore::load_with_roots(directory.path().join("grants.json"), vec![apps]).unwrap();
        assert_eq!(
            store.check("User::Pkg::org.example.new"),
            GrantCheck::Allowed
        );
    }

    #[test]
    fn failed_save_rolls_back_grant_and_revoke() {
        let directory = tempfile::tempdir().unwrap();
        let apps = directory.path().join("apps");
        fs::create_dir(&apps).unwrap();
        write_signature(&apps, "org.example.app", "sig-app");
        let parent = directory.path().join("grants");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("grants.json");
        let mut store = GrantStore::load_with_roots(&path, vec![apps]).unwrap();
        fs::remove_dir(&parent).unwrap();
        fs::write(&parent, b"blocker").unwrap();

        assert!(store.grant("User::Pkg::org.example.app").is_err());
        assert_eq!(
            store.check("User::Pkg::org.example.app"),
            GrantCheck::NotGranted
        );

        fs::remove_file(&parent).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(
            &path,
            r#"{"version":2,"grants":{"User::Pkg::org.example.keep":{"granted_at":1}}}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let apps = directory.path().join("apps");
        let mut store = GrantStore::load_with_roots(&path, vec![apps]).unwrap();
        fs::remove_dir_all(&parent).unwrap();
        fs::write(&parent, b"blocker").unwrap();
        assert!(store.revoke("User::Pkg::org.example.keep").is_err());
        assert!(store
            .entries()
            .any(|(label, _, _)| label == "User::Pkg::org.example.keep"));
    }

    #[test]
    fn notify_mode_persists_and_default_is_not_written() {
        let (directory, mut store) = temp_store();
        let label = "User::Pkg::org.example.allowed";
        assert_eq!(store.notify_mode(label), Some(NotifyMode::Every));
        assert!(!store
            .set_notify("User::Pkg::org.example.none", NotifyMode::Never)
            .unwrap());
        assert!(store.set_notify(label, NotifyMode::First).unwrap());
        drop(store);

        let path = directory.path().join("grants.json");
        let apps = directory.path().join("apps");
        let mut store = GrantStore::load_with_roots(&path, vec![apps]).unwrap();
        assert_eq!(store.notify_mode(label), Some(NotifyMode::First));
        store.set_notify(label, NotifyMode::Every).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("notify"));
    }

    #[test]
    fn grant_and_revoke_are_atomic_and_persistent() {
        let (directory, mut store) = temp_store();
        assert!(store.grant("User::Pkg::org.example.new").unwrap());
        assert!(!store.grant("User::Pkg::org.example.new").unwrap());
        drop(store);

        let apps = directory.path().join("apps");
        let mut store =
            GrantStore::load_with_roots(directory.path().join("grants.json"), vec![apps]).unwrap();
        assert_eq!(
            store.check("User::Pkg::org.example.new"),
            GrantCheck::Allowed
        );
        assert!(store.revoke("User::Pkg::org.example.new").unwrap());
        drop(store);

        let apps = directory.path().join("apps");
        let mut store =
            GrantStore::load_with_roots(directory.path().join("grants.json"), vec![apps]).unwrap();
        assert_eq!(
            store.check("User::Pkg::org.example.new"),
            GrantCheck::NotGranted
        );
    }
}
