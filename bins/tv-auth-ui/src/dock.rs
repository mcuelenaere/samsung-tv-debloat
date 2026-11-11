//! `org.tvauth.home`: the dock that answers the Home key
//! (docs/tv-launcher-plan.md). Same executable as the manager; `main`
//! picks this when AUL starts it under the dock's app id.
//!
//! tv-patchd makes this app the default for
//! `http://tizen.org/appcontrol/operation/main`, so tvctx-manager launches
//! or resumes it on every Home press. Each press arrives as `app_control`:
//! it shows the dock, or hides it when it is already shown.
//!
//! The dock is a notification window across the bottom of the screen, so
//! the app or input behind it stays visible.

use std::cell::Cell;
use std::os::raw::{c_char, c_void};
use std::ptr;
use tv_efl::log::log_line;
use tv_efl::widgets::*;
use tv_efl::*;

pub const APP_ID: &str = "org.tvauth.home";
const WINDOW_NAME: &[u8] = b"org.tvauth.home\0";
const DELETE_REQUEST: &[u8] = b"delete,request\0";
const FOCUSED: &[u8] = b"focused\0";

const COLOR_DOCK: i32 = 0x10_12_17;
const DOCK_HEIGHT: i32 = 220;
const TILE_WIDTH: i32 = 360;
const TILE_HEIGHT: i32 = 96;
const IDLE_SECONDS: f64 = 20.0;

const SAMSUNG_HOME: &str = "com.samsung.tv.csfs";
const MANAGER: &str = "org.tvauth.ui";

/// The tiles of step 1: (label, app id).
const TILES: &[(&str, &str)] = &[("Samsung Home", SAMSUNG_HOME), ("TV suite", MANAGER)];

struct Dock {
    window: *mut c_void,
    first: *mut c_void,
    shown: bool,
    idle: *mut c_void,
}

thread_local! {
    static DOCK: Cell<*mut Dock> = const { Cell::new(ptr::null_mut()) };
}

fn dock() -> &'static mut Dock {
    let pointer = DOCK.with(Cell::get);
    assert!(!pointer.is_null(), "dock used outside ui_app_main");
    // SAFETY: installed in `run` on the EFL main thread; valid until
    // `ui_app_main` returns.
    unsafe { &mut *pointer }
}

impl Dock {
    fn create_window(&mut self) -> bool {
        unsafe {
            let window = elm_win_add(
                ptr::null_mut(),
                WINDOW_NAME.as_ptr().cast(),
                ELM_WIN_NOTIFICATION,
            );
            if window.is_null() {
                log_line("dock: elm_win_add failed");
                return false;
            }
            self.window = window;
            let wayland_window = elm_win_wl_window_get(window);
            if !wayland_window.is_null() {
                ecore_wl2_window_type_set(wayland_window, ECORE_WL2_WINDOW_TYPE_NOTIFICATION);
            }
            // Above the app being watched, below tv-auth-popupd's prompts (TOP).
            if efl_util_set_notification_window_level(window, NOTIFICATION_HIGH) != 0 {
                log_line("dock: notification level HIGH refused");
            }
            let hint = c_string("wm.policy.win.user.geometry");
            let one = c_string("1");
            elm_win_aux_hint_add(window, hint.as_ptr(), one.as_ptr());
            evas_object_smart_callback_add(
                window,
                DELETE_REQUEST.as_ptr().cast(),
                on_back,
                ptr::null_mut(),
            );
            eext_object_event_callback_add(window, EEXT_CALLBACK_BACK, on_back, ptr::null_mut());

            let (mut width, mut height) = (0, 0);
            elm_win_screen_size_get(
                window,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut width,
                &mut height,
            );
            if width <= 0 || height <= 0 {
                (width, height) = (1920, 1080);
            }
            evas_object_resize(window, width, DOCK_HEIGHT);
            evas_object_move(window, 0, height - DOCK_HEIGHT);

            // A rectangle keeps its minimum (a table recomputes its own), so
            // the window stays DOCK_HEIGHT tall and reaches the bottom edge.
            let background = create_rect(window, COLOR_DOCK, 255);
            evas_object_size_hint_min_set(background, width, DOCK_HEIGHT);
            evas_object_size_hint_weight_set(background, EVAS_HINT_EXPAND, EVAS_HINT_EXPAND);
            elm_win_resize_object_add(window, background);
            let (bar, row) = panel(window, COLOR_DOCK, 72, 40);
            elm_win_resize_object_add(window, bar);
            elm_box_horizontal_set(row, 1);
            elm_box_padding_set(row, 24, 0);
            evas_object_size_hint_align_set(row, 0.0, 0.5);
            for (index, (label, _)) in TILES.iter().enumerate() {
                let button = create_button(
                    row,
                    label,
                    on_tile_clicked,
                    ptr::without_provenance_mut(index + 1),
                );
                evas_object_size_hint_min_set(button, TILE_WIDTH, TILE_HEIGHT);
                evas_object_smart_callback_add(
                    button,
                    FOCUSED.as_ptr().cast(),
                    on_focus,
                    ptr::null_mut(),
                );
                if index == 0 {
                    self.first = button;
                }
            }
        }
        true
    }

    fn show(&mut self) {
        self.shown = true;
        unsafe {
            evas_object_show(self.window);
            elm_win_raise(self.window);
            elm_win_activate(self.window);
            if !self.first.is_null() {
                elm_object_focus_set(self.first, 1);
            }
        }
        self.restart_idle();
    }

    fn hide(&mut self) {
        self.shown = false;
        unsafe { evas_object_hide(self.window) };
        if !self.idle.is_null() {
            unsafe { ecore_timer_del(self.idle) };
            self.idle = ptr::null_mut();
        }
    }

    fn restart_idle(&mut self) {
        unsafe {
            if !self.idle.is_null() {
                ecore_timer_del(self.idle);
            }
            self.idle = ecore_timer_add(IDLE_SECONDS, on_idle, ptr::null_mut());
        }
    }
}

fn c_string(text: &str) -> std::ffi::CString {
    std::ffi::CString::new(text).unwrap_or_default()
}

unsafe extern "C" fn on_idle(_data: *mut c_void) -> EinaBool {
    let dock = dock();
    dock.idle = ptr::null_mut(); // returning 0 deletes the timer
    dock.hide();
    0
}

unsafe extern "C" fn on_focus(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    dock().restart_idle();
}

unsafe extern "C" fn on_back(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    dock().hide();
}

unsafe extern "C" fn on_tile_clicked(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    let Some((label, app)) = TILES.get((data as usize).wrapping_sub(1)) else {
        return;
    };
    dock().hide();
    if let Err(error) = tv_tizen::app::launch(app) {
        log_line(&format!("dock: open {label} ({app}): {error}"));
    }
}

unsafe extern "C" fn app_create(user_data: *mut c_void) -> bool {
    DOCK.with(|cell| cell.set(user_data.cast()));
    log_line("dock: app_create");
    dock().create_window()
}

unsafe extern "C" fn app_terminate(_user_data: *mut c_void) {}
unsafe extern "C" fn app_pause(_user_data: *mut c_void) {}
unsafe extern "C" fn app_resume(_user_data: *mut c_void) {}

/// Every Home press (and the first launch).
unsafe extern "C" fn app_control(_app_control: *mut c_void, _user_data: *mut c_void) {
    let dock = dock();
    if dock.shown {
        dock.hide();
    } else {
        dock.show();
    }
}

pub fn run(mut argv: Vec<*mut c_char>) -> i32 {
    let state = Box::into_raw(Box::new(Dock {
        window: ptr::null_mut(),
        first: ptr::null_mut(),
        shown: false,
        idle: ptr::null_mut(),
    }));
    DOCK.with(|cell| cell.set(state));
    let argc = i32::try_from(argv.len().saturating_sub(1)).unwrap_or(1);
    let mut callbacks = UiAppLifecycleCallback {
        create: Some(app_create),
        terminate: Some(app_terminate),
        pause: Some(app_pause),
        resume: Some(app_resume),
        app_control: Some(app_control),
    };
    // SAFETY: argv, callbacks and the state outlive the synchronous call.
    let status = unsafe { ui_app_main(argc, argv.as_mut_ptr(), &mut callbacks, state.cast()) };
    unsafe { drop(Box::from_raw(state)) };
    DOCK.with(|cell| cell.set(ptr::null_mut()));
    status
}
