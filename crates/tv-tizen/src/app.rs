//! Launch and inspect applications through `app_control` and AUL. Both talk
//! to the caller's user session, so they work from UID-5001 processes.

use crate::c_string;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

#[link(name = "capi-appfw-app-control")]
extern "C" {
    fn app_control_create(handle: *mut *mut c_void) -> c_int;
    fn app_control_set_app_id(handle: *mut c_void, app_id: *const c_char) -> c_int;
    fn app_control_send_launch_request(
        handle: *mut c_void,
        reply: *const c_void,
        data: *mut c_void,
    ) -> c_int;
    fn app_control_destroy(handle: *mut c_void) -> c_int;
}

#[link(name = "aul")]
extern "C" {
    fn aul_app_is_running(app_id: *const c_char) -> c_int;
}

/// Ask `amd` to launch (or resume) `app_id`.
pub fn launch(app_id: &str) -> Result<(), String> {
    let id = c_string(app_id)?;
    let mut handle = ptr::null_mut();
    let status = unsafe { app_control_create(&mut handle) };
    if status != 0 {
        return Err(format!("app_control_create: {status}"));
    }
    let mut status = unsafe { app_control_set_app_id(handle, id.as_ptr()) };
    if status == 0 {
        status = unsafe { app_control_send_launch_request(handle, ptr::null(), ptr::null_mut()) };
    }
    unsafe { app_control_destroy(handle) };
    if status == 0 {
        Ok(())
    } else {
        Err(format!("launch {app_id}: app_control error {status}"))
    }
}

pub fn is_running(app_id: &str) -> Result<bool, String> {
    let id = c_string(app_id)?;
    Ok(unsafe { aul_app_is_running(id.as_ptr()) } > 0)
}
