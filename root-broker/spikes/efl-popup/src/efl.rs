//! Small, spike-specific Tizen/EFL FFI boundary.
//!
//! The TV declarations intentionally use the legacy EFL C ABI available before
//! EFL 1.26. No generated or broad bindings are involved. The declarations and
//! enum values still need checking against the exact TV sysroot before linking.

use std::ffi::CString;
use std::io::Write as _;
use std::os::raw::{c_char, c_double, c_int, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::ptr;

macro_rules! log {
    ($($arg:tt)*) => {
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    };
}

type EinaBool = u8;
type EvasCoord = c_int;

const EINA_TRUE: EinaBool = 1;

// Elm_Win_Type is a C enum. ELM_WIN_UNKNOWN is -1 and ELM_WIN_BASIC is 0.
const ELM_WIN_NOTIFICATION: c_int = 11;
// Eext_Callback_Type starts with EEXT_CALLBACK_BACK.
const EEXT_CALLBACK_BACK: c_int = 0;
// NONE=0, DEFAULT=1, MEDIUM=2, HIGH=3, TOP=4 in Tizen 6.5.
const EFL_UTIL_NOTIFICATION_LEVEL_TOP: c_int = 4;
const EFL_UTIL_NOTIFICATION_LEVEL_HIGH: c_int = 3;
const EFL_UTIL_NOTIFICATION_LEVEL_MEDIUM: c_int = 2;
const EFL_UTIL_NOTIFICATION_LEVEL_DEFAULT: c_int = 1;

const EVAS_HINT_EXPAND: c_double = 1.0;
const EVAS_HINT_FILL: c_double = -1.0;

const APP_ID: &[u8] = b"org.tvauth.spike.popup\0";
const WINDOW_TITLE: &[u8] = b"Harmless Rust/EFL test\0";
const DELETE_REQUEST: &[u8] = b"delete,request\0";
const CLICKED: &[u8] = b"clicked\0";
const BACK_KEY: &[u8] = b"XF86Back\0";
const BUTTON_TEXT: &[u8] = b"OK / Dismiss\0";
const WARNING_TEXT: &[u8] = b"<align=center><font_size=42><b>Harmless test</b></font_size><br/><br/>This is a harmless Rust/EFL approval-popup spike.<br/>No root access is requested. No commands are run and no settings are changed.<br/><br/>Press OK / Dismiss or RETURN to exit.</align>\0";

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
type EvasSmartCb =
    unsafe extern "C" fn(data: *mut c_void, object: *mut c_void, event_info: *mut c_void);

#[link(name = "capi-appfw-application")]
extern "C" {
    fn ui_app_main(
        argc: c_int,
        argv: *mut *mut c_char,
        callback: *mut UiAppLifecycleCallback,
        user_data: *mut c_void,
    ) -> c_int;
    fn ui_app_exit();
}

#[link(name = "elementary")]
extern "C" {
    fn elm_win_add(parent: *mut c_void, name: *const c_char, window_type: c_int) -> *mut c_void;
    fn elm_win_title_set(window: *mut c_void, title: *const c_char);
    fn elm_win_resize_object_add(window: *mut c_void, child: *mut c_void);
    fn elm_win_center(window: *mut c_void, horizontal: EinaBool, vertical: EinaBool);
    fn elm_win_activate(window: *mut c_void);

    fn elm_bg_add(parent: *mut c_void) -> *mut c_void;
    fn elm_box_add(parent: *mut c_void) -> *mut c_void;
    fn elm_box_padding_set(box_: *mut c_void, horizontal: EvasCoord, vertical: EvasCoord);
    fn elm_box_pack_end(box_: *mut c_void, child: *mut c_void);
    fn elm_label_add(parent: *mut c_void) -> *mut c_void;
    fn elm_button_add(parent: *mut c_void) -> *mut c_void;
    fn elm_object_part_text_set(object: *mut c_void, part: *const c_char, text: *const c_char);
    fn elm_object_focus_set(object: *mut c_void, focus: EinaBool);
}

#[link(name = "evas")]
extern "C" {
    fn evas_object_size_hint_weight_set(object: *mut c_void, x: c_double, y: c_double);
    fn evas_object_size_hint_align_set(object: *mut c_void, x: c_double, y: c_double);
    fn evas_object_size_hint_min_set(object: *mut c_void, width: EvasCoord, height: EvasCoord);
    fn evas_object_resize(object: *mut c_void, width: EvasCoord, height: EvasCoord);
    fn evas_object_show(object: *mut c_void);
    fn evas_object_del(object: *mut c_void);
    fn evas_object_smart_callback_add(
        object: *mut c_void,
        event: *const c_char,
        callback: EvasSmartCb,
        data: *mut c_void,
    );
}

#[link(name = "capi-ui-efl-util")]
extern "C" {
    fn efl_util_set_notification_window_level(window: *mut c_void, level: c_int) -> c_int;
}

#[link(name = "efl-extension")]
extern "C" {
    fn eext_object_event_callback_add(
        object: *mut c_void,
        callback_type: c_int,
        callback: EvasSmartCb,
        data: *mut c_void,
    );
    fn eext_win_keygrab_set(window: *mut c_void, key: *const c_char) -> EinaBool;
    fn eext_win_keygrab_unset(window: *mut c_void, key: *const c_char) -> EinaBool;
}

struct AppState {
    window: *mut c_void,
    back_grabbed: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            window: ptr::null_mut(),
            back_grabbed: false,
        }
    }
}

/// Runs the Tizen UI application and returns `ui_app_main`'s status.
///
/// All raw pointers, callback glue, and C string pointers are confined to this
/// module. `AppState` and the argument storage outlive the blocking C call.
pub fn run() -> i32 {
    let argument_storage = match process_arguments() {
        Some(arguments) => arguments,
        None => return 1,
    };
    let mut argv: Vec<*mut c_char> = argument_storage
        .iter()
        .map(|argument| argument.as_ptr() as *mut c_char)
        .collect();
    argv.push(ptr::null_mut());

    let argc = match c_int::try_from(argument_storage.len()) {
        Ok(argc) => argc,
        Err(_) => {
            log!("efl-popup: too many process arguments");
            return 1;
        }
    };

    let mut state = AppState::default();
    let mut callbacks = UiAppLifecycleCallback {
        create: Some(app_create),
        terminate: Some(app_terminate),
        pause: Some(app_pause),
        resume: Some(app_resume),
        app_control: Some(app_control),
    };

    // SAFETY: `argv`, `callbacks`, and `state` remain alive and at stable
    // addresses until the synchronous `ui_app_main` call returns. The callback
    // functions have the signatures required by Tizen's app.h.
    unsafe {
        ui_app_main(
            argc,
            argv.as_mut_ptr(),
            &mut callbacks,
            &mut state as *mut AppState as *mut c_void,
        )
    }
}

fn process_arguments() -> Option<Vec<CString>> {
    std::env::args_os()
        .map(|argument| {
            CString::new(argument.as_bytes()).map_err(|_| {
                log!("efl-popup: process argument contains an interior NUL byte");
            })
        })
        .collect::<Result<_, _>>()
        .ok()
}

unsafe extern "C" fn app_create(user_data: *mut c_void) -> bool {
    // SAFETY: `run` passes a valid AppState pointer that outlives this callback.
    let state = unsafe { &mut *(user_data as *mut AppState) };
    create_popup(state)
}

unsafe extern "C" fn app_terminate(user_data: *mut c_void) {
    // SAFETY: this is the same pointer supplied to `ui_app_main` by `run`.
    let state = unsafe { &mut *(user_data as *mut AppState) };
    if state.window.is_null() {
        return;
    }

    // SAFETY: the window is owned by this state and is deleted exactly once.
    unsafe {
        if state.back_grabbed {
            eext_win_keygrab_unset(state.window, BACK_KEY.as_ptr().cast());
        }
        evas_object_del(state.window);
    }
    state.window = ptr::null_mut();
}

unsafe extern "C" fn app_pause(_user_data: *mut c_void) {}
unsafe extern "C" fn app_resume(_user_data: *mut c_void) {}
unsafe extern "C" fn app_control(_app_control: *mut c_void, _user_data: *mut c_void) {}

unsafe extern "C" fn dismiss(_data: *mut c_void, _object: *mut c_void, _event_info: *mut c_void) {
    // SAFETY: all dismiss callbacks run inside the active Tizen UI main loop.
    unsafe { ui_app_exit() };
}

fn create_popup(state: &mut AppState) -> bool {
    // SAFETY: every constructor result is checked before use. All string
    // pointers refer to static NUL-terminated byte strings. Object ownership
    // stays with the EFL parent/window hierarchy until app_terminate.
    unsafe {
        let window = elm_win_add(
            ptr::null_mut(),
            APP_ID.as_ptr().cast(),
            ELM_WIN_NOTIFICATION,
        );
        if window.is_null() {
            log!("efl-popup: elm_win_add failed");
            return false;
        }
        state.window = window;

        elm_win_title_set(window, WINDOW_TITLE.as_ptr().cast());
        set_highest_notification_level(window);

        evas_object_smart_callback_add(
            window,
            DELETE_REQUEST.as_ptr().cast(),
            dismiss,
            ptr::null_mut(),
        );
        eext_object_event_callback_add(window, EEXT_CALLBACK_BACK, dismiss, ptr::null_mut());
        if eext_win_keygrab_set(window, BACK_KEY.as_ptr().cast()) == EINA_TRUE {
            state.back_grabbed = true;
        } else {
            log!("efl-popup: XF86Back keygrab failed; the button remains available");
        }

        let background = elm_bg_add(window);
        if background.is_null() {
            log!("efl-popup: elm_bg_add failed");
            return false;
        }
        evas_object_size_hint_weight_set(background, EVAS_HINT_EXPAND, EVAS_HINT_EXPAND);
        elm_win_resize_object_add(window, background);
        evas_object_show(background);

        let box_ = elm_box_add(window);
        if box_.is_null() {
            log!("efl-popup: elm_box_add failed");
            return false;
        }
        evas_object_size_hint_weight_set(box_, EVAS_HINT_EXPAND, EVAS_HINT_EXPAND);
        evas_object_size_hint_align_set(box_, EVAS_HINT_FILL, EVAS_HINT_FILL);
        elm_box_padding_set(box_, 32, 32);
        elm_win_resize_object_add(window, box_);
        evas_object_show(box_);

        let label = elm_label_add(window);
        if label.is_null() {
            log!("efl-popup: elm_label_add failed");
            return false;
        }
        elm_object_part_text_set(label, ptr::null(), WARNING_TEXT.as_ptr().cast());
        evas_object_size_hint_weight_set(label, EVAS_HINT_EXPAND, EVAS_HINT_EXPAND);
        evas_object_size_hint_align_set(label, EVAS_HINT_FILL, EVAS_HINT_FILL);
        elm_box_pack_end(box_, label);
        evas_object_show(label);

        let button = elm_button_add(window);
        if button.is_null() {
            log!("efl-popup: elm_button_add failed");
            return false;
        }
        elm_object_part_text_set(button, ptr::null(), BUTTON_TEXT.as_ptr().cast());
        evas_object_size_hint_min_set(button, 320, 96);
        evas_object_smart_callback_add(button, CLICKED.as_ptr().cast(), dismiss, ptr::null_mut());
        elm_box_pack_end(box_, button);
        evas_object_show(button);
        elm_object_focus_set(button, EINA_TRUE);

        evas_object_resize(window, 1200, 650);
        elm_win_center(window, EINA_TRUE, EINA_TRUE);
        evas_object_show(window);
        elm_win_activate(window);
    }

    true
}

fn set_highest_notification_level(window: *mut c_void) {
    const LEVELS: &[(&str, c_int)] = &[
        ("TOP", EFL_UTIL_NOTIFICATION_LEVEL_TOP),
        ("HIGH", EFL_UTIL_NOTIFICATION_LEVEL_HIGH),
        ("MEDIUM", EFL_UTIL_NOTIFICATION_LEVEL_MEDIUM),
        ("DEFAULT", EFL_UTIL_NOTIFICATION_LEVEL_DEFAULT),
    ];

    for &(name, level) in LEVELS {
        // SAFETY: `window` is a live ELM_WIN_NOTIFICATION window.
        let result = unsafe { efl_util_set_notification_window_level(window, level) };
        if result == 0 {
            log!("efl-popup: notification level {name} accepted");
            return;
        }
        log!("efl-popup: notification level {name} rejected with {result}");
    }

    // Keep this harmless display test usable even if priority is denied. The
    // result is still a notification-type window, with its platform default.
    log!("efl-popup: no requested priority accepted; using the platform default");
}
