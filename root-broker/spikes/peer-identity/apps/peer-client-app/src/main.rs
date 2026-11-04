//! Harmless packaged peer-identity probe.
//!
//! Each TPK is built with a compile-time `PEER_PROBE` value. Once AUL execs this
//! binary, the kernel has already applied the package SMACK label. Connecting
//! directly in `main` therefore tests the process identity used by the broker,
//! without requiring a UI lifecycle that this no-display spike does not need.

use std::ffi::CString;
use std::io::{Read, Write};
use std::os::raw::{c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::ptr;

const SOCKET: &str = "/run/tvauth-spike/peer.sock";

#[repr(C)]
struct UiAppLifecycleCallback {
    create: Option<AppCreateCb>,
    terminate: Option<AppLifecycleCb>,
    pause: Option<AppLifecycleCb>,
    resume: Option<AppLifecycleCb>,
    app_control: Option<AppControlCb>,
}

type AppCreateCb = unsafe extern "C" fn(user_data: *mut c_void) -> bool;
type AppLifecycleCb = unsafe extern "C" fn(user_data: *mut c_void);
type AppControlCb = unsafe extern "C" fn(app_control: *mut c_void, user_data: *mut c_void);

#[link(name = "capi-appfw-application")]
extern "C" {
    fn ui_app_main(
        argc: i32,
        argv: *mut *mut c_char,
        callback: *mut UiAppLifecycleCallback,
        user_data: *mut c_void,
    ) -> i32;
}

fn main() {
    let arguments: Vec<CString> = std::env::args_os()
        .map(|value| CString::new(value.as_bytes()).unwrap_or_default())
        .collect();
    let mut argv: Vec<*mut c_char> = arguments
        .iter()
        .map(|value| value.as_ptr() as *mut c_char)
        .collect();
    argv.push(ptr::null_mut());
    let mut callbacks = UiAppLifecycleCallback {
        create: Some(app_create),
        terminate: None,
        pause: None,
        resume: None,
        app_control: None,
    };

    std::process::exit(unsafe {
        ui_app_main(
            arguments.len() as i32,
            argv.as_mut_ptr(),
            &mut callbacks,
            ptr::null_mut(),
        )
    });
}

unsafe extern "C" fn app_create(_user_data: *mut c_void) -> bool {
    let probe = option_env!("PEER_PROBE").unwrap_or("peer-unknown");
    let result = UnixStream::connect(SOCKET).and_then(|mut stream| {
        stream.write_all(probe.as_bytes())?;
        stream.write_all(b"\n")?;
        let mut response = [0_u8; 16];
        let length = stream.read(&mut response)?;
        Ok(String::from_utf8_lossy(&response[..length]).into_owned())
    });

    match result {
        Ok(response) => println!("peer-client {probe}: response={response:?}"),
        Err(error) => eprintln!("peer-client {probe}: error={error}"),
    }

    // Returning false terminates the service immediately after the probe.
    false
}
