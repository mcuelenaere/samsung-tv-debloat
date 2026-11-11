//! Local setup-bundle validation and the per-target setup lock.
//!
//! The manifest is self-describing: every file is addressed relative to the
//! bundle root and hashed. Verification happens before any network activity,
//! and the same digest is recomputed by the on-TV driver.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundlePackage {
    pub id: String,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FdetVariant {
    pub dll: BundleFile,
    pub runtime_config: BundleFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FdetBundle {
    pub modern: FdetVariant,
    pub legacy: FdetVariant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersistenceBundle {
    pub hook: BundleFile,
    pub apply: BundleFile,
    pub units: Vec<BundleFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupManifest {
    pub format: u32,
    pub target_build: String,
    pub bundle_sha256: String,
    pub fdet: FdetBundle,
    pub tv_pkg: BundleFile,
    pub driver: BundleFile,
    pub packages: Vec<BundlePackage>,
    pub persistence: PersistenceBundle,
}

impl SetupManifest {
    pub fn files(&self) -> Vec<BundleFile> {
        let mut files = vec![
            self.fdet.modern.dll.clone(),
            self.fdet.modern.runtime_config.clone(),
            self.fdet.legacy.dll.clone(),
            self.fdet.legacy.runtime_config.clone(),
            self.tv_pkg.clone(),
            self.driver.clone(),
            self.persistence.hook.clone(),
            self.persistence.apply.clone(),
        ];
        files.extend(self.packages.iter().map(|package| BundleFile {
            path: package.path.clone(),
            sha256: package.sha256.clone(),
        }));
        files.extend(self.persistence.units.iter().cloned());
        files
    }
}

#[derive(Debug, Clone)]
pub struct Bundle {
    root: PathBuf,
    pub manifest: SetupManifest,
}

impl Bundle {
    pub fn load(root: &Path) -> Result<Self> {
        let manifest_path = root.join("setup-manifest.json");
        require_regular(&manifest_path)?;
        let bytes = std::fs::read(&manifest_path)?;
        let manifest: SetupManifest = serde_json::from_slice(&bytes)
            .map_err(|error| Error::bundle(format!("invalid setup-manifest.json: {error}")))?;
        if manifest.format != FORMAT {
            return Err(Error::bundle(format!(
                "unsupported setup manifest format {}",
                manifest.format
            )));
        }
        if manifest.target_build.trim().is_empty() {
            return Err(Error::bundle("setup manifest has an empty target build"));
        }
        let expected = bundle_digest(&manifest);
        if manifest.bundle_sha256 != expected {
            return Err(Error::bundle(format!(
                "bundle digest is {}, manifest records {}",
                expected, manifest.bundle_sha256
            )));
        }
        let bundle = Bundle {
            root: root.to_path_buf(),
            manifest,
        };
        bundle.verify()?;
        Ok(bundle)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn verify(&self) -> Result<()> {
        for file in self.manifest.files() {
            self.verify_file(&file)?;
        }
        if self.manifest.packages.is_empty() {
            return Err(Error::bundle("setup bundle contains no packages"));
        }
        if self.manifest.persistence.units.len() != 5 {
            return Err(Error::bundle(
                "setup bundle must contain all five persistence unit files",
            ));
        }
        Ok(())
    }

    pub fn path(&self, relative: &str) -> Result<PathBuf> {
        safe_join(&self.root, relative)
    }

    pub fn verify_file(&self, file: &BundleFile) -> Result<()> {
        let path = self.path(&file.path)?;
        require_regular(&path)?;
        let actual = file_sha256(&path)?;
        if actual != file.sha256 {
            return Err(Error::bundle(format!(
                "{} has hash {actual}, expected {}",
                file.path, file.sha256
            )));
        }
        Ok(())
    }

    pub fn variant(&self, runtime: crate::fdet::Runtime) -> (&BundleFile, &BundleFile) {
        let variant = match runtime {
            crate::fdet::Runtime::Net6 => &self.manifest.fdet.modern,
            crate::fdet::Runtime::Core31 => &self.manifest.fdet.legacy,
        };
        (&variant.dll, &variant.runtime_config)
    }
}

fn require_regular(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|error| Error::bundle(format!("{}: {error}", path.display())))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(Error::bundle(format!(
            "{} must be a regular non-symlink file",
            path.display()
        )));
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(Error::bundle(format!(
            "bundle path must be relative and plain: {relative:?}"
        )));
    }
    Ok(root.join(relative))
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn file_sha256(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(sha256(&bytes))
}

/// The sorted digest rows also emitted into the on-TV plan.
pub fn digest_rows(manifest: &SetupManifest) -> Vec<String> {
    let mut rows = Vec::new();
    for file in [
        &manifest.fdet.modern.dll,
        &manifest.fdet.modern.runtime_config,
        &manifest.fdet.legacy.dll,
        &manifest.fdet.legacy.runtime_config,
        &manifest.tv_pkg,
        &manifest.driver,
    ] {
        rows.push(format!("file {} {}", file.path, file.sha256));
    }
    for package in &manifest.packages {
        rows.push(format!(
            "package {} {} {}",
            package.id, package.path, package.sha256
        ));
    }
    rows.push(format!(
        "persistence-hook {} {}",
        manifest.persistence.hook.path, manifest.persistence.hook.sha256
    ));
    rows.push(format!(
        "persistence-apply {} {}",
        manifest.persistence.apply.path, manifest.persistence.apply.sha256
    ));
    for unit in &manifest.persistence.units {
        rows.push(format!("persistence-unit {} {}", unit.path, unit.sha256));
    }
    rows.sort();
    rows
}

/// Must match `tv-bundle`'s digest exactly.
pub fn bundle_digest(manifest: &SetupManifest) -> String {
    let rows = digest_rows(manifest);
    sha256(
        [manifest.target_build.as_str(), rows.join("\n").as_str()]
            .join("\n")
            .as_bytes(),
    )
}

/// A non-blocking, per-target setup lock. The file lives in the host temporary
/// directory so separate invocations cannot race.
#[derive(Debug)]
pub struct TargetLock {
    path: PathBuf,
    _file: std::fs::File,
}

impl TargetLock {
    pub fn acquire(target: &str, directory: Option<&Path>) -> Result<Self> {
        let directory = directory
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&directory)?;
        let safe: String = target
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = directory.join(format!("tv-setup-{safe}.lock"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                let _ = writeln!(file, "pid={}", std::process::id());
                Ok(TargetLock { path, _file: file })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::Busy(
                format!("another setup for {target} holds {}", path.display()),
            )),
            Err(error) => Err(Error::Io(error)),
        }
    }
}

impl Drop for TargetLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn fixture() -> (tempfile::TempDir, SetupManifest) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut files = Vec::new();
        for (path, bytes) in [
            ("fdet/modern/FdetProbe.dll", b"m".as_slice()),
            ("fdet/modern/FdetProbe.runtimeconfig.json", b"{}".as_slice()),
            ("fdet/legacy/FdetProbe.dll", b"l".as_slice()),
            ("fdet/legacy/FdetProbe.runtimeconfig.json", b"{}".as_slice()),
            ("tv-pkg", b"pkg".as_slice()),
            ("setup-driver.sh", b"#!/bin/sh\n".as_slice()),
            ("persistence/parseinfo.sh", b"hook".as_slice()),
            ("persistence/tv-apply.sh", b"apply".as_slice()),
            ("packages/a.tvpkg", b"A".as_slice()),
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), bytes).unwrap();
            files.push(BundleFile {
                path: path.into(),
                sha256: sha256(bytes),
            });
        }
        for name in [
            "dropbear.service",
            "tv-authd.service",
            "tv-auth-popupd.service",
            "tv-blockd.service",
            "tv-patchd.service",
        ] {
            let path = format!("persistence/units/{name}");
            std::fs::create_dir_all(root.join("persistence/units")).unwrap();
            std::fs::write(root.join(&path), name.as_bytes()).unwrap();
            files.push(BundleFile {
                path,
                sha256: sha256(name.as_bytes()),
            });
        }
        let find = |path: &str| files.iter().find(|file| file.path == path).unwrap().clone();
        let units = files
            .iter()
            .filter(|file| file.path.starts_with("persistence/units/"))
            .cloned()
            .collect();
        let mut manifest = SetupManifest {
            format: FORMAT,
            target_build: "T-PTMAKUC-REL-202310071804".into(),
            bundle_sha256: String::new(),
            fdet: FdetBundle {
                modern: FdetVariant {
                    dll: find("fdet/modern/FdetProbe.dll"),
                    runtime_config: find("fdet/modern/FdetProbe.runtimeconfig.json"),
                },
                legacy: FdetVariant {
                    dll: find("fdet/legacy/FdetProbe.dll"),
                    runtime_config: find("fdet/legacy/FdetProbe.runtimeconfig.json"),
                },
            },
            tv_pkg: find("tv-pkg"),
            driver: find("setup-driver.sh"),
            packages: vec![BundlePackage {
                id: "a".into(),
                path: "packages/a.tvpkg".into(),
                sha256: find("packages/a.tvpkg").sha256,
            }],
            persistence: PersistenceBundle {
                hook: find("persistence/parseinfo.sh"),
                apply: find("persistence/tv-apply.sh"),
                units,
            },
        };
        manifest.bundle_sha256 = bundle_digest(&manifest);
        std::fs::write(
            root.join("setup-manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        (dir, manifest)
    }

    #[test]
    fn load_verifies_every_hash() {
        let (dir, _) = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        assert_eq!(bundle.manifest.packages.len(), 1);

        std::fs::write(dir.path().join("tv-pkg"), b"changed").unwrap();
        let error = Bundle::load(dir.path()).unwrap_err();
        assert!(error.to_string().contains("has hash"), "{error}");
    }

    #[test]
    fn traversal_and_digest_changes_fail_closed() {
        let (dir, mut manifest) = fixture();
        manifest.tv_pkg.path = "../tv-pkg".into();
        manifest.bundle_sha256 = bundle_digest(&manifest);
        std::fs::write(
            dir.path().join("setup-manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let error = Bundle::load(dir.path()).unwrap_err();
        assert!(error.to_string().contains("relative and plain"), "{error}");

        let (dir, mut manifest) = fixture();
        manifest.target_build = "T-OTHER".into();
        std::fs::write(
            dir.path().join("setup-manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let error = Bundle::load(dir.path()).unwrap_err();
        assert!(error.to_string().contains("bundle digest"), "{error}");
    }

    #[test]
    fn target_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let first = TargetLock::acquire("192.0.2.10", Some(dir.path())).unwrap();
        let error = TargetLock::acquire("192.0.2.10", Some(dir.path())).unwrap_err();
        assert!(matches!(error, Error::Busy(_)), "{error}");
        drop(first);
        TargetLock::acquire("192.0.2.10", Some(dir.path())).unwrap();
    }
}
