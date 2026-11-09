//! Read-only package queries against the package manager database.
//!
//! `pkg.list` and `pkg.uninstall` need the id, type, version, name, and
//! install origin of installed packages. The platform keeps that in
//! `pkgmgr`'s SQLite database (`package_info` joined with the "No Locale"
//! row of `package_localized_info`), which is what `pkginfo` reads too.
//!
//! Reading it directly matters: `pkginfo --listpkg` enumerates every app over
//! D-Bus and takes ~16 s on this firmware, while the same information is a
//! ~10 ms read-only query. The database is only ever opened with
//! `sqlite3 -readonly`; the daemon never writes to it.

use crate::executor;
use crate::protocol::PackageType;
use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

/// Upper bound on `pkg.list` entries.
pub const MAX_LISTED_PACKAGES: usize = 256;
/// The query is local and indexed; it must never stall a connection.
pub const QUERY_TIMEOUT_MS: u64 = 10_000;
/// Separator for `sqlite3 -separator`; package names may contain `|`.
const SEPARATOR: &str = "\u{1f}";
/// Columns selected by both queries, in `PackageEntry` order.
const COLUMNS: &str = "p.package, p.package_type, p.package_version, \
                       coalesce(p.mainapp_id, ''), coalesce(l.package_label, ''), \
                       p.package_preload, p.package_system, p.package_removable";
const FROM: &str = "FROM package_info p LEFT JOIN package_localized_info l \
                    ON l.package = p.package AND l.package_locale = 'No Locale'";

#[derive(Debug, Clone)]
pub struct PackageDb {
    /// `sqlite3` CLI, normally `/usr/bin/sqlite3`.
    pub sqlite3: PathBuf,
    /// `pkgmgr` parser database, normally `/opt/dbspace/.pkgmgr_parser.db`.
    pub database: PathBuf,
}

impl PackageDb {
    pub fn new() -> Self {
        Self {
            sqlite3: PathBuf::from("/usr/bin/sqlite3"),
            database: PathBuf::from("/opt/dbspace/.pkgmgr_parser.db"),
        }
    }
}

impl Default for PackageDb {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub pkgid: String,
    pub name: String,
    pub pkg_version: String,
    /// Raw `package_type` (`tpk`, `wgt`, ...).
    pub type_label: String,
    pub main_app_id: Option<String>,
    /// Installed as part of the platform image.
    pub system: bool,
    /// Preloaded package (also platform-owned).
    pub preload: bool,
    pub removable: bool,
}

impl PackageEntry {
    /// The daemon only installs and uninstalls wgt/tpk packages.
    pub fn package_type(&self) -> Option<PackageType> {
        match self.type_label.as_str() {
            "tpk" => Some(PackageType::Tpk),
            "wgt" => Some(PackageType::Wgt),
            _ => None,
        }
    }

    /// Third-party packages the user may remove.
    pub fn is_user_package(&self) -> bool {
        !self.system && !self.preload && self.removable && self.package_type().is_some()
    }
}

#[derive(Debug)]
pub enum ListError {
    Spawn(io::Error),
    Failed(String),
}

impl std::fmt::Display for ListError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "cannot run sqlite3: {error}"),
            Self::Failed(detail) => write!(formatter, "package database query failed: {detail}"),
        }
    }
}

/// List installed third-party packages, sorted by package id.
pub fn list(db: &PackageDb, timeout_ms: u64) -> Result<Vec<PackageEntry>, ListError> {
    let sql = format!(
        "SELECT {COLUMNS} {FROM} \
         WHERE p.package_preload = 'false' AND p.package_system = 'false' \
         AND p.package_removable = 'true' AND p.package_readonly = 'false' \
         AND p.package_type IN ('wgt', 'tpk') \
         ORDER BY p.package LIMIT {MAX_LISTED_PACKAGES};"
    );
    let mut entries = query(db, &sql, timeout_ms)?;
    entries.retain(PackageEntry::is_user_package);
    entries.truncate(MAX_LISTED_PACKAGES);
    entries.sort_by(|left, right| left.pkgid.cmp(&right.pkgid));
    entries.dedup_by(|left, right| left.pkgid == right.pkgid);
    Ok(entries)
}

/// Read one installed package's metadata. `Ok(None)` means it is not in the
/// database (not installed, or not a wgt/tpk package).
pub fn get(
    db: &PackageDb,
    pkgid: &str,
    timeout_ms: u64,
) -> Result<Option<PackageEntry>, ListError> {
    if !crate::identity::is_valid_package_id(pkgid) {
        return Ok(None);
    }
    // The id is validated above (alphanumerics, '.', '_', '-'), so it cannot
    // break out of the SQL string literal.
    let sql = format!("SELECT {COLUMNS} {FROM} WHERE p.package = '{pkgid}';");
    let entries = query(db, &sql, timeout_ms)?;
    Ok(entries.into_iter().find(|entry| entry.pkgid == pkgid))
}

fn query(db: &PackageDb, sql: &str, timeout_ms: u64) -> Result<Vec<PackageEntry>, ListError> {
    let outcome = executor::execute_argv(
        &db.sqlite3,
        &[
            OsStr::new("-readonly"),
            OsStr::new("-noheader"),
            OsStr::new("-separator"),
            OsStr::new(SEPARATOR),
            db.database.as_os_str(),
            OsStr::new(sql),
        ],
        timeout_ms,
    )
    .map_err(ListError::Spawn)?;
    if outcome.timed_out {
        return Err(ListError::Failed("sqlite3 query timed out".into()));
    }
    if outcome.truncated {
        return Err(ListError::Failed("sqlite3 output was truncated".into()));
    }
    if outcome.exit_code != 0 {
        let detail = String::from_utf8_lossy(&outcome.stderr);
        return Err(ListError::Failed(format!(
            "sqlite3 exited {}: {}",
            outcome.exit_code,
            detail.trim()
        )));
    }
    let text = String::from_utf8_lossy(&outcome.stdout);
    Ok(parse_rows(&text))
}

/// Split `sqlite3 -noheader -separator` output into entries. Rows that do not
/// have the expected shape or a valid package id are skipped.
pub fn parse_rows(text: &str) -> Vec<PackageEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(SEPARATOR);
        let (Some(pkgid), Some(type_label), Some(pkg_version), Some(main_app_id), Some(name)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let preload = fields.next().is_some_and(|value| value == "true");
        let system = fields.next().is_some_and(|value| value == "true");
        let removable = fields.next().is_some_and(|value| value == "true");
        if !crate::identity::is_valid_package_id(pkgid) {
            continue;
        }
        entries.push(PackageEntry {
            pkgid: pkgid.to_owned(),
            name: name.to_owned(),
            pkg_version: pkg_version.to_owned(),
            type_label: type_label.to_owned(),
            main_app_id: (!main_app_id.is_empty()).then(|| main_app_id.to_owned()),
            system,
            preload,
            removable,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Row<'a> {
        pkgid: &'a str,
        pkg_type: &'a str,
        version: &'a str,
        main_app_id: &'a str,
        name: &'a str,
        preload: &'a str,
        system: &'a str,
        removable: &'a str,
    }

    fn row(row: Row<'_>) -> String {
        let Row {
            pkgid,
            pkg_type,
            version,
            main_app_id,
            name,
            preload,
            system,
            removable,
        } = row;
        format!(
            "{pkgid}{SEPARATOR}{pkg_type}{SEPARATOR}{version}{SEPARATOR}{main_app_id}\
             {SEPARATOR}{name}{SEPARATOR}{preload}{SEPARATOR}{system}{SEPARATOR}{removable}"
        )
    }

    #[test]
    fn parses_database_rows() {
        let text = format!(
            "{}\n{}\n",
            row(Row {
                pkgid: "vetrotvapp",
                pkg_type: "wgt",
                version: "0.5.0",
                main_app_id: "vetrotvapp.app",
                name: "Vetro",
                preload: "false",
                system: "false",
                removable: "true",
            }),
            row(Row {
                pkgid: "com.samsung.tv.coba.smartthings",
                pkg_type: "tpk",
                version: "1.0.1",
                main_app_id: "com.samsung.tv.coba.smartthings",
                name: "CompSmartThings",
                preload: "true",
                system: "true",
                removable: "false",
            }),
        );
        let entries = parse_rows(&text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Vetro");
        assert_eq!(entries[0].pkg_version, "0.5.0");
        assert_eq!(entries[0].main_app_id.as_deref(), Some("vetrotvapp.app"));
        assert_eq!(entries[0].package_type(), Some(PackageType::Wgt));
        assert!(entries[0].is_user_package());
        assert!(!entries[1].is_user_package());
    }

    #[test]
    fn skips_malformed_rows_and_ids() {
        let text = format!(
            "short{SEPARATOR}row\n{}\n",
            row(Row {
                pkgid: "org.example/bad",
                pkg_type: "wgt",
                version: "1",
                main_app_id: "",
                name: "Bad",
                preload: "false",
                system: "false",
                removable: "true",
            })
        );
        // The short row is dropped; the path-like id is rejected.
        assert!(parse_rows(&text).is_empty());
    }

    #[test]
    fn keeps_names_that_contain_pipes() {
        let text = row(Row {
            pkgid: "org.example.app",
            pkg_type: "tpk",
            version: "1.0.0",
            main_app_id: "",
            name: "A | B",
            preload: "false",
            system: "false",
            removable: "true",
        });
        let entries = parse_rows(&text);
        assert_eq!(entries[0].name, "A | B");
        assert_eq!(entries[0].main_app_id, None);
    }

    #[test]
    fn only_wgt_and_tpk_have_a_daemon_type() {
        let text = row(Row {
            pkgid: "org.tizen.cloning",
            pkg_type: "rpm",
            version: "0.1",
            main_app_id: "",
            name: "cloning",
            preload: "false",
            system: "false",
            removable: "true",
        });
        let entry = parse_rows(&text).remove(0);
        assert_eq!(entry.package_type(), None);
        assert!(!entry.is_user_package());
    }
}
