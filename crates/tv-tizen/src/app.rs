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

/// The operation tvctx-manager launches on the Home key.
pub const OPERATION_MAIN: &str = "http://tizen.org/appcontrol/operation/main";

#[link(name = "capi-appfw-app-operation-tv")]
extern "C" {
    fn app_operation_get_current_app(operation: *const c_char, app_id: *mut *mut c_char) -> c_int;
    fn app_operation_set_current_app(operation: *const c_char, app_id: *const c_char) -> c_int;
    fn app_operation_reset_current_app(operation: *const c_char) -> c_int;
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

/// The app the Home key launches (the current default for
/// [`OPERATION_MAIN`] in the user's appsvc database).
pub fn home_app() -> Result<String, String> {
    let operation = c_string(OPERATION_MAIN)?;
    let mut app_id = ptr::null_mut();
    let status = unsafe { app_operation_get_current_app(operation.as_ptr(), &mut app_id) };
    if status != 0 || app_id.is_null() {
        return Err(format!("app_operation_get_current_app: {status}"));
    }
    let value = unsafe { crate::borrowed(app_id) };
    unsafe { libc::free(app_id.cast()) };
    Ok(value)
}

/// Make `app_id` the Home key's target (root only).
pub fn set_home_app(app_id: &str) -> Result<(), String> {
    let (operation, id) = (c_string(OPERATION_MAIN)?, c_string(app_id)?);
    match unsafe { app_operation_set_current_app(operation.as_ptr(), id.as_ptr()) } {
        0 => Ok(()),
        status => Err(format!("app_operation_set_current_app: {status}")),
    }
}

/// Give the Home key back to the firmware's default (root only).
pub fn reset_home_app() -> Result<(), String> {
    let operation = c_string(OPERATION_MAIN)?;
    match unsafe { app_operation_reset_current_app(operation.as_ptr()) } {
        0 => Ok(()),
        status => Err(format!("app_operation_reset_current_app: {status}")),
    }
}

pub fn is_running(app_id: &str) -> Result<bool, String> {
    let id = c_string(app_id)?;
    Ok(unsafe { aul_app_is_running(id.as_ptr()) } > 0)
}
