//! `libvconf.so.0` key/value settings, e.g.
//! `db/menu/support/softwareupdate/autoupdate`.

use crate::{borrowed, c_string};
use std::os::raw::{c_char, c_int, c_void};

#[link(name = "vconf")]
extern "C" {
    fn vconf_get_int(key: *const c_char, value: *mut c_int) -> c_int;
    fn vconf_set_int(key: *const c_char, value: c_int) -> c_int;
    fn vconf_get_bool(key: *const c_char, value: *mut c_int) -> c_int;
    fn vconf_set_bool(key: *const c_char, value: c_int) -> c_int;
    fn vconf_get_str(key: *const c_char) -> *mut c_char;
    fn vconf_set_str(key: *const c_char, value: *const c_char) -> c_int;
}

fn check(key: &str, status: c_int) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("vconf {key}: error {status}"))
    }
}

pub fn get_int(key: &str) -> Result<i32, String> {
    let name = c_string(key)?;
    let mut value = 0;
    check(key, unsafe { vconf_get_int(name.as_ptr(), &mut value) })?;
    Ok(value)
}

pub fn set_int(key: &str, value: i32) -> Result<(), String> {
    let name = c_string(key)?;
    check(key, unsafe { vconf_set_int(name.as_ptr(), value) })
}

pub fn get_bool(key: &str) -> Result<bool, String> {
    let name = c_string(key)?;
    let mut value = 0;
    check(key, unsafe { vconf_get_bool(name.as_ptr(), &mut value) })?;
    Ok(value != 0)
}

pub fn set_bool(key: &str, value: bool) -> Result<(), String> {
    let name = c_string(key)?;
    check(key, unsafe {
        vconf_set_bool(name.as_ptr(), c_int::from(value))
    })
}

pub fn get_str(key: &str) -> Result<String, String> {
    let name = c_string(key)?;
    let pointer = unsafe { vconf_get_str(name.as_ptr()) };
    if pointer.is_null() {
        return Err(format!("vconf {key}: not a readable string"));
    }
    // SAFETY: vconf returns a malloc'd NUL-terminated copy we must free.
    let value = unsafe { borrowed(pointer) };
    unsafe { libc::free(pointer.cast::<c_void>()) };
    Ok(value)
}

pub fn set_str(key: &str, value: &str) -> Result<(), String> {
    let name = c_string(key)?;
    let value = c_string(value)?;
    check(key, unsafe { vconf_set_str(name.as_ptr(), value.as_ptr()) })
}
