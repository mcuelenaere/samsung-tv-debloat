//! Strict `component.json` types and validation.

use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const FORMAT: u32 = 1;
/// Native destinations must live strictly below one of these (a declared
/// directory may be the root itself).
pub const ALLOWED_ROOTS: &[&str] = &[
    "/opt/tv-suite",
    "/opt/tv-auth",
    "/opt/tv-sshd",
    "/opt/tv-units",
];
pub const UNIT_DIR: &str = "/opt/tv-units";
pub const UNIT_SMACK: &str = "System::Shared";
pub const UNIT_MODE: u32 = 0o600;
/// tv-pkg's own state; no component may write here.
pub const STATE_DIR: &str = "/opt/tv-suite/var/tv-pkg";
const MAX_VERSION_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Native,
    Tpk,
    Wgt,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Tpk => "tpk",
            Self::Wgt => "wgt",
        }
    }
}

/// Permission bits written as an octal string (`"0755"`). No setuid, setgid
/// or sticky bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Mode(pub u32);

impl TryFrom<String> for Mode {
    type Error = String;
    fn try_from(text: String) -> Result<Self, String> {
        let valid =
            (3..=4).contains(&text.len()) && text.bytes().all(|b| (b'0'..=b'7').contains(&b));
        match u32::from_str_radix(&text, 8) {
            Ok(mode) if valid && mode <= 0o777 => Ok(Self(mode)),
            _ => Err(format!("invalid mode {text:?}")),
        }
    }
}

impl From<Mode> for String {
    fn from(mode: Mode) -> Self {
        format!("{:04o}", mode.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Directory {
    pub path: String,
    pub mode: Mode,
    pub smack: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub source: String,
    pub destination: String,
    pub sha256: String,
    pub mode: Mode,
    pub smack: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unit {
    pub source: String,
    pub name: String,
    pub sha256: String,
}

impl Unit {
    pub fn destination(&self) -> String {
        format!("{UNIT_DIR}/{}", self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub source: String,
    pub sha256: String,
    pub pkgid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub id: String,
    pub version: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<Directory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub units: Vec<Unit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Package>,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| Error::invalid(format!("component.json: {error}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Every destination this component owns, as absolute device paths.
    pub fn owned_paths(&self) -> Vec<String> {
        let files = self.files.iter().map(|file| file.destination.clone());
        let links = self.links.iter().map(|link| link.path.clone());
        let units = self.units.iter().map(Unit::destination);
        files.chain(links).chain(units).collect()
    }

    /// Payload entries the archive must contain.
    pub fn sources(&self) -> Vec<(&str, &str)> {
        let files = self
            .files
            .iter()
            .map(|f| (f.source.as_str(), f.sha256.as_str()));
        let units = self
            .units
            .iter()
            .map(|u| (u.source.as_str(), u.sha256.as_str()));
        let package = self
            .package
            .iter()
            .map(|p| (p.source.as_str(), p.sha256.as_str()));
        files.chain(units).chain(package).collect()
    }

    pub fn validate(&self) -> Result<(), Error> {
        let fail = |message: String| Err(Error::invalid(message));
        if self.format != FORMAT {
            return fail(format!("unsupported format {}", self.format));
        }
        if !tv_proto::is_valid_package_id(&self.id) {
            return fail(format!("invalid id {:?}", self.id));
        }
        if self.version.is_empty()
            || self.version.len() > MAX_VERSION_BYTES
            || !self.version.bytes().all(|b| b.is_ascii_graphic())
        {
            return fail(format!("invalid version {:?}", self.version));
        }
        let native_fields = !self.directories.is_empty()
            || !self.files.is_empty()
            || !self.links.is_empty()
            || !self.units.is_empty()
            || self.undo_command.is_some();
        match (self.kind, &self.package) {
            (Kind::Native, None) => {}
            (Kind::Native, Some(_)) => {
                return fail("native components cannot have a package".into())
            }
            (_, None) => return fail(format!("{} components need a package", self.kind.as_str())),
            (_, Some(package)) => {
                if native_fields {
                    return fail(format!(
                        "{} components only have a package",
                        self.kind.as_str()
                    ));
                }
                if package.pkgid != self.id {
                    return fail(format!("pkgid {:?} does not match id", package.pkgid));
                }
                check_source(&package.source)?;
                check_sha256(&package.sha256)?;
                return Ok(());
            }
        }

        for directory in &self.directories {
            check_destination(&directory.path, true)?;
            check_smack(&directory.smack)?;
        }
        let mut seen = HashSet::new();
        for path in self.owned_paths() {
            if !seen.insert(path.clone()) {
                return fail(format!("{path} is declared twice"));
            }
        }
        for directory in &self.directories {
            if !seen.insert(directory.path.clone()) {
                return fail(format!("{} is declared twice", directory.path));
            }
        }
        for file in &self.files {
            check_source(&file.source)?;
            check_destination(&file.destination, false)?;
            check_not_unit_dir(&file.destination)?;
            check_sha256(&file.sha256)?;
            check_smack(&file.smack)?;
        }
        for link in &self.links {
            check_destination(&link.path, false)?;
            check_not_unit_dir(&link.path)?;
            // A plain relative target stays in the link's own directory.
            let target = &link.target;
            if target.is_empty()
                || target.len() > MAX_PATH_BYTES
                || target.starts_with('/')
                || target
                    .split('/')
                    .any(|part| matches!(part, "" | "." | ".."))
                || target.contains('\0')
            {
                return fail(format!("invalid link target {target:?}"));
            }
        }
        for unit in &self.units {
            check_source(&unit.source)?;
            check_sha256(&unit.sha256)?;
            let name = &unit.name;
            let stem = name.strip_suffix(".service").unwrap_or("");
            if stem.is_empty()
                || name.len() > 128
                || !stem
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@'))
                || stem.starts_with('.')
            {
                return fail(format!("invalid unit name {name:?}"));
            }
        }
        if let Some(argv) = &self.undo_command {
            let valid = argv
                .first()
                .is_some_and(|program| is_normal_absolute(program))
                && argv
                    .iter()
                    .all(|arg| !arg.contains('\0') && arg.len() <= 4096)
                && argv.len() <= 64;
            if !valid {
                return fail(
                    "undo_command must be an argv array starting with an absolute path".into(),
                );
            }
        }
        Ok(())
    }
}

/// Absolute, without empty, `.` or `..` components or a trailing slash.
pub fn is_normal_absolute(path: &str) -> bool {
    path.len() <= MAX_PATH_BYTES
        && !path.contains('\0')
        && path
            .strip_prefix('/')
            .is_some_and(|rest| rest.split('/').all(|part| !matches!(part, "" | "." | "..")))
}

fn check_destination(path: &str, allow_root: bool) -> Result<(), Error> {
    if !is_normal_absolute(path) {
        return Err(Error::invalid(format!("invalid path {path:?}")));
    }
    let allowed = ALLOWED_ROOTS.iter().any(|root| {
        (allow_root && path == *root)
            || path
                .strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('/'))
    });
    let reserved = path == STATE_DIR || path.starts_with(&format!("{STATE_DIR}/"));
    if !allowed || reserved {
        return Err(Error::invalid(format!(
            "{path} is outside the allowed roots"
        )));
    }
    Ok(())
}

fn check_not_unit_dir(path: &str) -> Result<(), Error> {
    if path.starts_with(&format!("{UNIT_DIR}/")) {
        return Err(Error::invalid(format!(
            "{path}: declare unit files under `units`"
        )));
    }
    Ok(())
}

fn check_source(source: &str) -> Result<(), Error> {
    let valid = source
        .strip_prefix("payload/")
        .is_some_and(|rest| rest.split('/').all(|part| !matches!(part, "" | "." | "..")))
        && !source.contains(['\0', '\\']);
    if !valid {
        return Err(Error::invalid(format!("invalid source {source:?}")));
    }
    Ok(())
}

fn check_sha256(hash: &str) -> Result<(), Error> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::invalid(format!("invalid sha256 {hash:?}")));
    }
    Ok(())
}

/// SMACK labels: 1..=255 printable bytes without `/ \ ' "` and not starting
/// with `-`.
fn check_smack(label: &str) -> Result<(), Error> {
    let valid = !label.is_empty()
        && label.len() <= 255
        && !label.starts_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'/' | b'\\' | b'\'' | b'"'));
    if !valid {
        return Err(Error::invalid(format!("invalid SMACK label {label:?}")));
    }
    Ok(())
}
