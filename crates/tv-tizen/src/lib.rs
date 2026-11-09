//! Thin synchronous bindings to Tizen platform libraries on the TV.
//!
//! Each module wraps only the calls the suite uses. Errors are plain strings:
//! callers log them, they never branch on them.

pub mod app;
pub mod http;
pub mod pkgdb;
pub mod vconf;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

fn c_string(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| format!("{value:?} contains NUL"))
}

/// Copy a C string the library owns; NULL becomes empty.
///
/// # Safety
/// `pointer` is NULL or a valid NUL-terminated string.
unsafe fn borrowed(pointer: *const c_char) -> String {
    if pointer.is_null() {
        String::new()
    } else {
        CStr::from_ptr(pointer).to_string_lossy().into_owned()
    }
}
