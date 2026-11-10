//! SQLite through the TV's `libsqlite3.so.0`: pkgmgr's package list (the
//! rows `tv-authd`'s `pkg.list` reads with the `sqlite3` CLI) and small
//! queries with bound parameters.
use crate::{borrowed, c_string};
use std::os::raw::{c_char, c_int, c_uchar, c_void};
use std::ptr;

pub const PKGMGR_PARSER_DB: &str = "/opt/dbspace/.pkgmgr_parser.db";

const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
/// `SQLITE_TRANSIENT`: sqlite copies bound text.
const SQLITE_TRANSIENT: isize = -1;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
/// pkgmgr writes in rollback-journal mode; wait out short write locks.
const BUSY_TIMEOUT_MS: c_int = 2_000;

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        db: *mut *mut c_void,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_busy_timeout(db: *mut c_void, ms: c_int) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut c_void,
        sql: *const c_char,
        bytes: c_int,
        statement: *mut *mut c_void,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(statement: *mut c_void) -> c_int;
    fn sqlite3_bind_text(
        statement: *mut c_void,
        index: c_int,
        text: *const c_char,
        bytes: c_int,
        destructor: isize,
    ) -> c_int;
    fn sqlite3_changes(db: *mut c_void) -> c_int;
    fn sqlite3_column_text(statement: *mut c_void, column: c_int) -> *const c_uchar;
    fn sqlite3_finalize(statement: *mut c_void) -> c_int;
    fn sqlite3_errmsg(db: *mut c_void) -> *const c_char;
    fn sqlite3_close(db: *mut c_void) -> c_int;
}

/// One installed package, as `pkgmgr` records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub pkgid: String,
    /// `tpk`, `wgt`, `rpm`, ...
    pub pkg_type: String,
    pub version: String,
    pub main_app_id: String,
    /// The "No Locale" label; empty when the package has none.
    pub label: String,
}

const PACKAGES_SQL: &str = "SELECT p.package, p.package_type, coalesce(p.package_version, ''), \
     coalesce(p.mainapp_id, ''), coalesce(l.package_label, '') \
     FROM package_info p LEFT JOIN package_localized_info l \
     ON l.package = p.package AND l.package_locale = 'No Locale' \
     ORDER BY p.package";

/// Every package in `database` (normally [`PKGMGR_PARSER_DB`]).
pub fn packages(database: &str) -> Result<Vec<Package>, String> {
    let db = Db::open(database, SQLITE_OPEN_READONLY)?;
    let sql = c_string(PACKAGES_SQL)?;
    let mut statement = ptr::null_mut();
    if unsafe { sqlite3_prepare_v2(db.0, sql.as_ptr(), -1, &mut statement, ptr::null_mut()) } != 0 {
        return Err(db.error());
    }
    let mut rows = Vec::new();
    let result = loop {
        match unsafe { sqlite3_step(statement) } {
            SQLITE_ROW => {
                // SAFETY: column text stays valid until the next step.
                let text = |column| unsafe {
                    borrowed(sqlite3_column_text(statement, column).cast::<c_char>())
                };
                rows.push(Package {
                    pkgid: text(0),
                    pkg_type: text(1),
                    version: text(2),
                    main_app_id: text(3),
                    label: text(4),
                });
            }
            SQLITE_DONE => break Ok(rows),
            _ => break Err(db.error()),
        }
    };
    unsafe { sqlite3_finalize(statement) };
    result
}

/// The first column of every row `sql` returns, with `?1`, `?2`, ... bound
/// to `parameters`.
pub fn query_text(database: &str, sql: &str, parameters: &[&str]) -> Result<Vec<String>, String> {
    let db = Db::open(database, SQLITE_OPEN_READONLY)?;
    let statement = db.prepare(sql, parameters)?;
    let mut rows = Vec::new();
    let result = loop {
        match unsafe { sqlite3_step(statement) } {
            SQLITE_ROW => {
                rows.push(unsafe { borrowed(sqlite3_column_text(statement, 0).cast::<c_char>()) })
            }
            SQLITE_DONE => break Ok(rows),
            _ => break Err(db.error()),
        }
    };
    unsafe { sqlite3_finalize(statement) };
    result
}

/// Runs one write statement; returns the number of changed rows.
pub fn execute(database: &str, sql: &str, parameters: &[&str]) -> Result<usize, String> {
    let db = Db::open(database, SQLITE_OPEN_READWRITE)?;
    let statement = db.prepare(sql, parameters)?;
    let result = match unsafe { sqlite3_step(statement) } {
        SQLITE_DONE => Ok(unsafe { sqlite3_changes(db.0) } as usize),
        _ => Err(db.error()),
    };
    unsafe { sqlite3_finalize(statement) };
    result
}

struct Db(*mut c_void);

impl Db {
    fn prepare(&self, sql: &str, parameters: &[&str]) -> Result<*mut c_void, String> {
        let sql = c_string(sql)?;
        let mut statement = ptr::null_mut();
        if unsafe { sqlite3_prepare_v2(self.0, sql.as_ptr(), -1, &mut statement, ptr::null_mut()) }
            != 0
        {
            return Err(self.error());
        }
        for (index, value) in parameters.iter().enumerate() {
            let length = c_int::try_from(value.len()).map_err(|_| "parameter too long")?;
            let status = unsafe {
                sqlite3_bind_text(
                    statement,
                    index as c_int + 1,
                    value.as_ptr().cast(),
                    length,
                    SQLITE_TRANSIENT,
                )
            };
            if status != 0 {
                unsafe { sqlite3_finalize(statement) };
                return Err(self.error());
            }
        }
        Ok(statement)
    }

    fn open(path: &str, flags: c_int) -> Result<Self, String> {
        let name = c_string(path)?;
        let mut handle = ptr::null_mut();
        let status = unsafe { sqlite3_open_v2(name.as_ptr(), &mut handle, flags, ptr::null()) };
        // sqlite3 allocates a handle even on failure; Drop closes it.
        let db = Self(handle);
        if status != 0 {
            return Err(format!("open {path}: {}", db.error()));
        }
        unsafe { sqlite3_busy_timeout(db.0, BUSY_TIMEOUT_MS) };
        Ok(db)
    }

    fn error(&self) -> String {
        unsafe { borrowed(sqlite3_errmsg(self.0)) }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        unsafe { sqlite3_close(self.0) };
    }
}
