//! Build `.tvpkg` component packages and setup-bundle manifests.
//!
//! This is a host build tool. It does not install anything and does not
//! duplicate `tv-pkg`'s on-TV transaction logic; it only emits the declarative
//! archives and hashes that `tv-pkg` and `tv-setup` consume.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use zip::write::SimpleFileOptions;

pub const PACKAGE_FORMAT: u32 = 1;
pub const SETUP_FORMAT: u32 = 1;
pub const REPRO_TIME: (u16, u8, u8, u8, u8, u8) = (1980, 1, 1, 0, 0, 0);

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSpec {
    pub format: u32,
    pub id: String,
    pub version: String,
    pub kind: Kind,
    #[serde(default)]
    pub directories: Vec<Directory>,
    #[serde(default)]
    pub files: Vec<FileSpec>,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub units: Vec<Unit>,
    #[serde(default)]
    pub undo_command: Vec<String>,
    #[serde(default)]
    pub package: Option<PackageSource>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Native,
    Tpk,
    Wgt,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Directory {
    pub path: String,
    pub mode: String,
    pub smack: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    pub source: String,
    pub destination: String,
    pub mode: String,
    pub smack: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unit {
    pub source: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSource {
    pub source: String,
    pub pkgid: String,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestFile {
    source: String,
    destination: String,
    sha256: String,
    mode: String,
    smack: String,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestUnit {
    source: String,
    name: String,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestPackage {
    source: String,
    sha256: String,
    pkgid: String,
}

#[derive(Debug, Clone, Serialize)]
struct ComponentManifest {
    format: u32,
    id: String,
    version: String,
    kind: Kind,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    directories: Vec<Directory>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<ManifestFile>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    links: Vec<Link>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    units: Vec<ManifestUnit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    undo_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<ManifestPackage>,
}

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

pub fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_regular(root: &Path, relative: &str) -> io::Result<(String, Vec<u8>)> {
    let path = safe_join(root, relative)?;
    let meta = fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("source must be a regular file: {}", path.display()),
        ));
    }
    Ok((path.display().to_string(), fs::read(path)?))
}

fn safe_join(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("source path must be relative and plain: {relative:?}"),
        ));
    }
    Ok(root.join(relative))
}

fn payload_name(source: &str) -> io::Result<String> {
    let name = Path::new(source)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("source has no plain file name: {source:?}"),
            )
        })?;
    Ok(format!("payload/{name}"))
}

const ALLOWED_ROOTS: &[&str] = &[
    "/opt/tv-suite",
    "/opt/tv-auth",
    "/opt/tv-sshd",
    "/opt/tv-units",
];

fn allowed_path(path: &str) -> bool {
    ALLOWED_ROOTS
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}

fn validate_spec(spec: &PackageSpec) -> io::Result<()> {
    if spec.format != PACKAGE_FORMAT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported package format {}", spec.format),
        ));
    }
    if spec.id.is_empty()
        || !spec
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid component id {:?}", spec.id),
        ));
    }
    if spec.version.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "component version must not be empty",
        ));
    }
    match spec.kind {
        Kind::Native if spec.package.is_some() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native packages must not carry a TPK/WGT source",
        )),
        Kind::Tpk | Kind::Wgt => {
            let package = spec.package.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "TPK/WGT packages need a package source",
                )
            })?;
            if package.pkgid != spec.id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "component id and package id must match",
                ));
            }
            if !spec.files.is_empty() || !spec.units.is_empty() || !spec.links.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "TPK/WGT packages may not declare native files",
                ));
            }
            Ok(())
        }
        Kind::Native => {
            if spec.files.is_empty()
                && spec.units.is_empty()
                && spec.links.is_empty()
                && spec.directories.is_empty()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "native package declares no files, links, units or directories",
                ));
            }
            for path in spec
                .files
                .iter()
                .map(|file| file.destination.as_str())
                .chain(
                    spec.directories
                        .iter()
                        .map(|directory| directory.path.as_str()),
                )
                .chain(spec.links.iter().map(|link| link.path.as_str()))
            {
                if !allowed_path(path) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("destination outside the allowed roots: {path}"),
                    ));
                }
            }
            for link in &spec.links {
                if Path::new(&link.target).is_absolute() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("link target must be relative: {}", link.target),
                    ));
                }
            }
            Ok(())
        }
    }
}

/// Builds one `.tvpkg` from a spec and a source directory.
pub fn build_package(spec_path: &Path, source_dir: &Path, output: &Path) -> io::Result<String> {
    let spec: PackageSpec = serde_json::from_slice(&fs::read(spec_path)?)?;
    validate_spec(&spec)?;
    let mut payloads = Vec::new();
    let mut used = HashSet::new();

    let mut add = |source: &str, bytes: Vec<u8>| -> io::Result<(String, String)> {
        let name = payload_name(source)?;
        if !used.insert(name.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("duplicate payload name {name}"),
            ));
        }
        let digest = sha256(&bytes);
        payloads.push((name.clone(), bytes));
        Ok((name, digest))
    };

    let mut files = Vec::new();
    for file in &spec.files {
        let (_, bytes) = read_regular(source_dir, &file.source)?;
        let (name, digest) = add(&file.source, bytes)?;
        files.push(ManifestFile {
            source: name,
            destination: file.destination.clone(),
            sha256: digest,
            mode: file.mode.clone(),
            smack: file.smack.clone(),
        });
    }
    let mut units = Vec::new();
    for unit in &spec.units {
        let (_, bytes) = read_regular(source_dir, &unit.source)?;
        let (name, digest) = add(&unit.source, bytes)?;
        units.push(ManifestUnit {
            source: name,
            name: unit.name.clone(),
            sha256: digest,
        });
    }
    let package = match &spec.package {
        Some(package) => {
            let (_, bytes) = read_regular(source_dir, &package.source)?;
            let (name, digest) = add(&package.source, bytes)?;
            Some(ManifestPackage {
                source: name,
                sha256: digest,
                pkgid: package.pkgid.clone(),
            })
        }
        None => None,
    };

    let manifest = ComponentManifest {
        format: spec.format,
        id: spec.id.clone(),
        version: spec.version.clone(),
        kind: spec.kind,
        directories: spec.directories.clone(),
        files,
        links: spec.links.clone(),
        units,
        undo_command: spec.undo_command.clone(),
        package,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut writer = zip::ZipWriter::new(File::create(output)?);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644)
        .last_modified_time(zip_datetime());
    writer.start_file("component.json", options)?;
    writer.write_all(&manifest_bytes)?;
    writer.write_all(b"\n")?;
    for (name, bytes) in payloads {
        writer.start_file(name, options)?;
        writer.write_all(&bytes)?;
    }
    writer.finish()?;
    Ok(sha256(&manifest_bytes))
}

fn zip_datetime() -> zip::DateTime {
    let (year, month, day, hour, minute, second) = REPRO_TIME;
    zip::DateTime::from_date_and_time(year, month, day, hour, minute, second)
        .expect("fixed reproducible timestamp")
}

/// Input read by `build_setup_manifest`: a directory laid out as:
///
/// ```text
/// fdet/modern/FdetProbe.dll
/// fdet/modern/FdetProbe.runtimeconfig.json
/// fdet/legacy/FdetProbe.dll
/// fdet/legacy/FdetProbe.runtimeconfig.json
/// tv-pkg
/// setup-driver.sh
/// packages/<ordered .tvpkg files>
/// persistence/parseinfo.sh
/// persistence/tv-apply.sh
/// persistence/units/<five project unit files>
/// ```
pub fn build_setup_manifest(
    root: &Path,
    target_build: &str,
    order: &[(String, String)],
    output: &Path,
) -> io::Result<SetupManifest> {
    if target_build.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target build must not be empty",
        ));
    }
    let file = |path: &str| -> io::Result<BundleFile> {
        let (_, bytes) = read_regular(root, path)?;
        Ok(BundleFile {
            path: path.to_owned(),
            sha256: sha256(&bytes),
        })
    };
    let variant = |prefix: &str| -> io::Result<FdetVariant> {
        Ok(FdetVariant {
            dll: file(&format!("{prefix}/FdetProbe.dll"))?,
            runtime_config: file(&format!("{prefix}/FdetProbe.runtimeconfig.json"))?,
        })
    };
    let packages = order
        .iter()
        .map(|(id, path)| {
            let (_, bytes) = read_regular(root, path)?;
            Ok(BundlePackage {
                id: id.clone(),
                path: path.clone(),
                sha256: sha256(&bytes),
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    if packages.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "setup bundle needs at least one package",
        ));
    }
    let unit_names = [
        "dropbear.service",
        "tv-authd.service",
        "tv-auth-popupd.service",
        "tv-blockd.service",
        "tv-patchd.service",
    ];
    let mut units = unit_names
        .iter()
        .map(|name| file(&format!("persistence/units/{name}")))
        .collect::<io::Result<Vec<_>>>()?;
    units.sort_by(|left, right| left.path.cmp(&right.path));
    let mut manifest = SetupManifest {
        format: SETUP_FORMAT,
        target_build: target_build.to_owned(),
        bundle_sha256: String::new(),
        fdet: FdetBundle {
            modern: variant("fdet/modern")?,
            legacy: variant("fdet/legacy")?,
        },
        tv_pkg: file("tv-pkg")?,
        driver: file("setup-driver.sh")?,
        packages,
        persistence: PersistenceBundle {
            hook: file("persistence/parseinfo.sh")?,
            apply: file("persistence/tv-apply.sh")?,
            units,
        },
    };
    manifest.bundle_sha256 = bundle_digest(&manifest);
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, [bytes.as_slice(), b"\n"].concat())?;
    Ok(manifest)
}

/// One digest over every path/hash pair in a setup manifest.
pub fn bundle_digest(manifest: &SetupManifest) -> String {
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
    sha256(
        [manifest.target_build.as_str(), rows.join("\n").as_str()]
            .join("\n")
            .as_bytes(),
    )
}

/// Reads a package-order file: one `<id> <path>` pair per line; `#` comments
/// and blank lines are ignored.
pub fn read_order(path: &Path) -> io::Result<Vec<(String, String)>> {
    let text = fs::read_to_string(path)?;
    let mut order = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(id), Some(path), None) = (fields.next(), fields.next(), fields.next()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("bad package order line {}", number + 1),
            ));
        };
        order.push((id.to_owned(), path.to_owned()));
    }
    Ok(order)
}

/// Small helper for scripts: print a file's SHA-256.
pub fn file_sha256(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(sha256(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use zip::ZipArchive;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn native_package_is_reproducible_and_hashes_payloads() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tv-blockd"), b"binary");
        write(&dir.path().join("tv-blockd.service"), b"[Service]\n");
        let spec = serde_json::json!({
            "format": 1,
            "id": "tv-blockd",
            "version": "0.1.0",
            "kind": "native",
            "directories": [{"path": "/opt/tv-suite/bin", "mode": "0755", "smack": "System::Shared"}],
            "files": [{
                "source": "tv-blockd",
                "destination": "/opt/tv-suite/bin/tv-blockd",
                "mode": "0755",
                "smack": "System::Shared"
            }],
            "units": [{"source": "tv-blockd.service", "name": "tv-blockd.service"}],
            "undo_command": ["/run/opt/tv-suite/bin/tv-blockd", "undo"]
        });
        let spec_path = dir.path().join("spec.json");
        fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
        let one = dir.path().join("one.tvpkg");
        let two = dir.path().join("two.tvpkg");
        build_package(&spec_path, dir.path(), &one).unwrap();
        build_package(&spec_path, dir.path(), &two).unwrap();
        assert_eq!(fs::read(&one).unwrap(), fs::read(&two).unwrap());

        let mut archive = ZipArchive::new(File::open(&one).unwrap()).unwrap();
        let mut manifest = String::new();
        archive
            .by_name("component.json")
            .unwrap()
            .read_to_string(&mut manifest)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["id"], "tv-blockd");
        assert_eq!(manifest["files"][0]["sha256"], sha256(b"binary"));
        assert_eq!(manifest["units"][0]["sha256"], sha256(b"[Service]\n"));
        assert!(archive.by_name("payload/tv-blockd").is_ok());
        assert!(archive.by_name("payload/tv-blockd.service").is_ok());
    }

    #[test]
    fn destinations_outside_the_roots_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("x"), b"x");
        let spec = serde_json::json!({
            "format": 1,
            "id": "x",
            "version": "1",
            "kind": "native",
            "files": [{
                "source": "x",
                "destination": "/usr/bin/x",
                "mode": "0644",
                "smack": "System::Shared"
            }]
        });
        let spec_path = dir.path().join("spec.json");
        fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
        let error = build_package(&spec_path, dir.path(), &dir.path().join("x.tvpkg")).unwrap_err();
        assert!(
            error.to_string().contains("outside the allowed roots"),
            "{error}"
        );
    }

    #[test]
    fn traversal_and_duplicate_names_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let spec = serde_json::json!({
            "format": 1,
            "id": "x",
            "version": "1",
            "kind": "native",
            "files": [{
                "source": "../outside",
                "destination": "/opt/tv-suite/x",
                "mode": "0644",
                "smack": "System::Shared"
            }]
        });
        let spec_path = dir.path().join("spec.json");
        fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
        let error = build_package(&spec_path, dir.path(), &dir.path().join("x.tvpkg")).unwrap_err();
        assert!(error.to_string().contains("relative and plain"), "{error}");

        write(&dir.path().join("a/one"), b"one");
        write(&dir.path().join("b/one"), b"two");
        let spec = serde_json::json!({
            "format": 1,
            "id": "x",
            "version": "1",
            "kind": "native",
            "files": [
                {"source": "a/one", "destination": "/opt/tv-suite/one", "mode": "0644", "smack": "System::Shared"},
                {"source": "b/one", "destination": "/opt/tv-suite/two", "mode": "0644", "smack": "System::Shared"}
            ]
        });
        fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
        let error = build_package(&spec_path, dir.path(), &dir.path().join("x.tvpkg")).unwrap_err();
        assert!(error.to_string().contains("duplicate payload"), "{error}");
    }

    #[test]
    fn setup_manifest_hashes_every_input_and_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("fdet/modern/FdetProbe.dll"), b"m");
        write(
            &dir.path().join("fdet/modern/FdetProbe.runtimeconfig.json"),
            b"{}",
        );
        write(&dir.path().join("fdet/legacy/FdetProbe.dll"), b"l");
        write(
            &dir.path().join("fdet/legacy/FdetProbe.runtimeconfig.json"),
            b"{}",
        );
        write(&dir.path().join("tv-pkg"), b"pkg");
        write(&dir.path().join("setup-driver.sh"), b"#!/bin/sh\n");
        write(&dir.path().join("packages/a.tvpkg"), b"A");
        write(&dir.path().join("persistence/parseinfo.sh"), b"hook");
        write(&dir.path().join("persistence/tv-apply.sh"), b"apply");
        for name in [
            "dropbear.service",
            "tv-authd.service",
            "tv-auth-popupd.service",
            "tv-blockd.service",
            "tv-patchd.service",
        ] {
            write(
                &dir.path().join("persistence/units").join(name),
                name.as_bytes(),
            );
        }
        let output = dir.path().join("setup-manifest.json");
        let manifest = build_setup_manifest(
            dir.path(),
            "T-TEST",
            &[("a".into(), "packages/a.tvpkg".into())],
            &output,
        )
        .unwrap();
        assert_eq!(manifest.bundle_sha256, bundle_digest(&manifest));
        assert_eq!(manifest.fdet.modern.dll.sha256, sha256(b"m"));
        let parsed: SetupManifest = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_eq!(parsed, manifest);
    }
}
