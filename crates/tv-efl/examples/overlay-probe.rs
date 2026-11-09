//! On-TV check that a standalone (systemd-started, non-AUL) process can show
//! a TOP-level notification window and grab a remote key. Prints one line per
//! step and quits after the given seconds (default 5).

use std::os::raw::c_void;
use std::ptr;
use tv_efl::*;

unsafe extern "C" fn quit(_data: *mut c_void) -> EinaBool {
    elm_exit();
    0
}

fn main() {
    let seconds: f64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(5.0);
    unsafe {
        println!("elm_init={}", elm_init(0, ptr::null_mut()));
        let window = elm_win_add(
            ptr::null_mut(),
            c"overlay-probe".as_ptr(),
            ELM_WIN_NOTIFICATION,
        );
        println!("window={window:?}");
        if window.is_null() {
            return;
        }
        let wayland = elm_win_wl_window_get(window);
        if !wayland.is_null() {
            ecore_wl2_window_type_set(wayland, ECORE_WL2_WINDOW_TYPE_NOTIFICATION);
        }
        println!(
            "level_top={}",
            efl_util_set_notification_window_level(window, NOTIFICATION_TOP)
        );
        let rect = evas_object_rectangle_add(evas_object_evas_get(window));
        evas_object_color_set(rect, 0xe5, 0x48, 0x4d, 255);
        elm_win_resize_object_add(window, rect);
        evas_object_show(rect);
        evas_object_resize(window, 640, 160);
        evas_object_move(window, 1200, 48);
        evas_object_show(window);
        println!(
            "keygrab_back={}",
            eext_win_keygrab_set(window, c"XF86Back".as_ptr())
        );
        ecore_timer_add(seconds, quit, ptr::null_mut());
        elm_run();
        println!(
            "keygrab_unset={}",
            eext_win_keygrab_unset(window, c"XF86Back".as_ptr())
        );
        elm_shutdown();
    }
}
