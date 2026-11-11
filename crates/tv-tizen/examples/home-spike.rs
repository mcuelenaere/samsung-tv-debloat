//! Phase 6 spike (docs/tv-launcher-plan.md, step 0): what a uid-5001
//! process with the manager's SMACK label can do about Samsung's Home.
//! Copy under /run (UEP) and run it through systemd-run, e.g.:
//!
//! ```text
//! home-spike kill com.samsung.tv.csfs ...   terminate through AUL / app_manager
//! home-spike watch 20                       print app launch/dead events for 20 s
//! home-spike open main | app ID | appmain ID
//! home-spike source TYPE NAME ICON MBR      switch input through tv-viewer
//! home-spike svg IN.svg SIZE                rasterise with thorvg
//! home-spike home get | set APPID | reset   the default app for operation/main
//! ```

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::ptr;
use std::time::Instant;

#[link(name = "aul")]
extern "C" {
    fn aul_app_get_pid(appid: *const c_char) -> c_int;
    fn aul_terminate_pid(pid: c_int) -> c_int;
    fn aul_app_get_status(appid: *const c_char) -> c_int;
    fn aul_listen_app_launch_signal_v2(
        callback: extern "C" fn(c_int, *const c_char, *mut c_void) -> c_int,
        data: *mut c_void,
    ) -> c_int;
    fn aul_listen_app_dead_signal(
        callback: extern "C" fn(c_int, *mut c_void) -> c_int,
        data: *mut c_void,
    ) -> c_int;
}

#[link(name = "capi-appfw-app-manager")]
extern "C" {
    fn app_manager_get_app_context(appid: *const c_char, context: *mut *mut c_void) -> c_int;
    fn app_manager_request_terminate_bg_app(context: *mut c_void) -> c_int;
    fn app_manager_terminate_app(context: *mut c_void) -> c_int;
    fn app_context_destroy(context: *mut c_void) -> c_int;
}

#[link(name = "capi-appfw-app-control")]
extern "C" {
    fn app_control_create(handle: *mut *mut c_void) -> c_int;
    fn app_control_set_operation(handle: *mut c_void, operation: *const c_char) -> c_int;
    fn app_control_set_app_id(handle: *mut c_void, appid: *const c_char) -> c_int;
    fn app_control_add_extra_data(
        handle: *mut c_void,
        key: *const c_char,
        value: *const c_char,
    ) -> c_int;
    fn app_control_send_launch_request(
        handle: *mut c_void,
        reply: *const c_void,
        data: *mut c_void,
    ) -> c_int;
    fn app_control_destroy(handle: *mut c_void) -> c_int;
}

#[link(name = "ecore")]
extern "C" {
    fn ecore_init() -> c_int;
    fn ecore_main_loop_glib_integrate() -> u8;
    fn ecore_main_loop_begin();
    fn ecore_main_loop_quit();
    fn ecore_timer_add(
        seconds: f64,
        callback: extern "C" fn(*mut c_void) -> u8,
        data: *const c_void,
    ) -> *mut c_void;
}

#[link(name = "capi-appfw-app-operation-tv")]
extern "C" {
    fn app_operation_get_current_app(operation: *const c_char, app_id: *mut *mut c_char) -> c_int;
    fn app_operation_get_default_app(operation: *const c_char, app_id: *mut *mut c_char) -> c_int;
    fn app_operation_set_current_app(operation: *const c_char, app_id: *const c_char) -> c_int;
    fn app_operation_reset_current_app(operation: *const c_char) -> c_int;
}

#[link(name = "thorvg")]
extern "C" {
    fn tvg_engine_init(engine: c_int, threads: c_uint) -> c_int;
    fn tvg_swcanvas_create() -> *mut c_void;
    fn tvg_swcanvas_set_target(
        canvas: *mut c_void,
        buffer: *mut u32,
        stride: u32,
        width: u32,
        height: u32,
        colorspace: c_int,
    ) -> c_int;
    fn tvg_picture_new() -> *mut c_void;
    fn tvg_picture_load(picture: *mut c_void, path: *const c_char) -> c_int;
    fn tvg_picture_set_size(picture: *mut c_void, width: f32, height: f32) -> c_int;
    fn tvg_canvas_push(canvas: *mut c_void, paint: *mut c_void) -> c_int;
    fn tvg_canvas_draw(canvas: *mut c_void) -> c_int;
    fn tvg_canvas_sync(canvas: *mut c_void) -> c_int;
}

fn c(text: &str) -> CString {
    CString::new(text).unwrap()
}

static mut START: Option<Instant> = None;

fn ms() -> u128 {
    unsafe { (*ptr::addr_of!(START)).map_or(0, |start| start.elapsed().as_millis()) }
}

extern "C" fn on_launch(pid: c_int, appid: *const c_char, _: *mut c_void) -> c_int {
    let appid = unsafe { CStr::from_ptr(appid) }.to_string_lossy();
    println!("{:>6} ms launch {pid} {appid}", ms());
    0
}

extern "C" fn on_dead(pid: c_int, _: *mut c_void) -> c_int {
    println!("{:>6} ms dead   {pid}", ms());
    0
}

extern "C" fn on_quit(_: *mut c_void) -> u8 {
    unsafe { ecore_main_loop_quit() };
    0
}

fn launch(app_id: Option<&str>, operation: Option<&str>, extras: &[(&str, &str)]) -> c_int {
    unsafe {
        let mut handle = ptr::null_mut();
        app_control_create(&mut handle);
        if let Some(operation) = operation {
            println!(
                "set_operation {}",
                app_control_set_operation(handle, c(operation).as_ptr())
            );
        }
        if let Some(app_id) = app_id {
            println!(
                "set_app_id {}",
                app_control_set_app_id(handle, c(app_id).as_ptr())
            );
        }
        for (key, value) in extras {
            app_control_add_extra_data(handle, c(key).as_ptr(), c(value).as_ptr());
        }
        let status = app_control_send_launch_request(handle, ptr::null(), ptr::null_mut());
        app_control_destroy(handle);
        status
    }
}

const MAIN: &str = "http://tizen.org/appcontrol/operation/main";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    unsafe { START = Some(Instant::now()) };
    match args.as_slice() {
        ["kill", apps @ ..] => {
            for app in apps {
                let id = c(app);
                let pid = unsafe { aul_app_get_pid(id.as_ptr()) };
                let mut context = ptr::null_mut();
                let got = unsafe { app_manager_get_app_context(id.as_ptr(), &mut context) };
                let bg = if got == 0 {
                    unsafe { app_manager_request_terminate_bg_app(context) }
                } else {
                    got
                };
                let term = if got == 0 {
                    unsafe { app_manager_terminate_app(context) }
                } else {
                    got
                };
                if got == 0 {
                    unsafe { app_context_destroy(context) };
                }
                let aul = if pid > 0 {
                    unsafe { aul_terminate_pid(pid) }
                } else {
                    pid
                };
                println!(
                    "{app}: pid={pid} get_context={got} terminate_bg={bg} terminate_app={term} aul_terminate_pid={aul}"
                );
            }
        }
        ["watch", seconds] => unsafe {
            ecore_init();
            println!("glib integrate {}", ecore_main_loop_glib_integrate());
            println!(
                "listen launch={} dead={}",
                aul_listen_app_launch_signal_v2(on_launch, ptr::null_mut()),
                aul_listen_app_dead_signal(on_dead, ptr::null_mut())
            );
            ecore_timer_add(seconds.parse().unwrap_or(20.0), on_quit, ptr::null());
            ecore_main_loop_begin();
        },
        ["open", "main"] => println!("launch {}", launch(None, Some(MAIN), &[])),
        ["open", "app", app] => println!("launch {}", launch(Some(app), None, &[])),
        ["open", "appmain", app] => println!("launch {}", launch(Some(app), Some(MAIN), &[])),
        ["source", kind, name, icon, mbr] => println!(
            "launch {}",
            launch(
                Some("org.tizen.tv-viewer"),
                None,
                &[
                    ("change_source", kind),
                    ("sourceType", kind),
                    ("sourceName", name),
                    ("sourceIcon", icon),
                    ("mbrIndex", mbr),
                    ("__K_EX_LAUNCH_REQUESTOR", "user"),
                ],
            )
        ),
        ["svg", path, size] => unsafe {
            let size: u32 = size.parse().unwrap_or(96);
            for engine in [2, 1] {
                println!("tvg_engine_init({engine}) {}", tvg_engine_init(engine, 0));
            }
            let mut buffer = vec![0_u32; (size * size) as usize];
            let canvas = tvg_swcanvas_create();
            println!(
                "set_target {}",
                tvg_swcanvas_set_target(canvas, buffer.as_mut_ptr(), size, size, size, 1)
            );
            let picture = tvg_picture_new();
            println!("load {}", tvg_picture_load(picture, c(path).as_ptr()));
            println!(
                "size {}",
                tvg_picture_set_size(picture, size as f32, size as f32)
            );
            println!(
                "push {} draw {} sync {}",
                tvg_canvas_push(canvas, picture),
                tvg_canvas_draw(canvas),
                tvg_canvas_sync(canvas)
            );
            let opaque = buffer.iter().filter(|pixel| *pixel >> 24 != 0).count();
            println!("{opaque} of {} pixels drawn", buffer.len());
        },
        // 0 launching, 1 created, 2 focus, 3 visible, 4 bg, 5 dying (aul.h)
        ["status", apps @ ..] => {
            for app in apps {
                println!("{app}: {}", unsafe { aul_app_get_status(c(app).as_ptr()) });
            }
        }
        ["home", action @ ..] => unsafe {
            let operation = c(MAIN);
            match action {
                ["set", app] => println!(
                    "set {}",
                    app_operation_set_current_app(operation.as_ptr(), c(app).as_ptr())
                ),
                ["reset"] => println!(
                    "reset {}",
                    app_operation_reset_current_app(operation.as_ptr())
                ),
                _ => {}
            }
            let (mut current, mut default) = (ptr::null_mut(), ptr::null_mut());
            let got_current = app_operation_get_current_app(operation.as_ptr(), &mut current);
            let got_default = app_operation_get_default_app(operation.as_ptr(), &mut default);
            let text = |p: *mut c_char| {
                if p.is_null() {
                    "-".to_owned()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            println!(
                "current {got_current} {} default {got_default} {}",
                text(current),
                text(default)
            );
        },
        _ => eprintln!("see the module doc"),
    }
}
