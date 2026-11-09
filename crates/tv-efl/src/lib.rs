//! Raw Tizen/EFL FFI shared by the suite's native UIs.
//!
//! The declarations mirror the TV's legacy EFL 1.25 C ABI (the pre-migration
//! managed UI remains in git history). Callers own every pointer, C string and
//! callback; this crate only declares the ABI.

#![allow(clippy::missing_safety_doc)]

pub mod log;
pub mod widgets;

use std::ffi::CString;
use std::os::raw::{c_char, c_double, c_int, c_void};

pub type EinaBool = u8;
pub type EvasCoord = c_int;

pub const EVAS_HINT_EXPAND: c_double = 1.0;
pub const EVAS_HINT_FILL: c_double = -1.0;

/// `Elm_Win_Type`: ELM_WIN_UNKNOWN is -1, basic is 0, notification is 11.
pub const ELM_WIN_BASIC: c_int = 0;
pub const ELM_SCROLLER_POLICY_OFF: c_int = 2;
pub const ELM_ICON_NONE: c_int = 0;
pub const ELM_WIN_NOTIFICATION: c_int = 11;
pub const EEXT_CALLBACK_BACK: c_int = 0;
pub const ECORE_WL2_WINDOW_TYPE_TOPLEVEL: c_int = 1;
pub const ECORE_WL2_WINDOW_TYPE_NOTIFICATION: c_int = 5;
pub const ELM_WRAP_WORD: c_int = 2;
/// `efl_util_notification_level_e`. These go to e20 unchanged; anything else
/// (the deprecated 0-2, or the 1-4 this crate used to send) lands in
/// E_LAYER_CLIENT_NOTIFICATION_LOW or NORMAL, below Samsung's own overlays.
pub const NOTIFICATION_TOP: c_int = 40;
pub const NOTIFICATION_HIGH: c_int = 30;
pub const NOTIFICATION_MEDIUM: c_int = 20;
pub const NOTIFICATION_DEFAULT: c_int = 10;

#[repr(C)]
pub struct UiAppLifecycleCallback {
    pub create: Option<AppCreateCb>,
    pub terminate: Option<AppLifecycleCb>,
    pub pause: Option<AppLifecycleCb>,
    pub resume: Option<AppLifecycleCb>,
    pub app_control: Option<AppControlCb>,
}

pub type AppCreateCb = unsafe extern "C" fn(user_data: *mut c_void) -> bool;
pub type AppLifecycleCb = unsafe extern "C" fn(user_data: *mut c_void);
pub type AppControlCb = unsafe extern "C" fn(app_control: *mut c_void, user_data: *mut c_void);
pub type EvasSmartCb =
    unsafe extern "C" fn(data: *mut c_void, object: *mut c_void, event_info: *mut c_void);
pub type EcoreTimerCb = unsafe extern "C" fn(data: *mut c_void) -> EinaBool;

#[link(name = "capi-appfw-application")]
extern "C" {
    pub fn ui_app_main(
        argc: c_int,
        argv: *mut *mut c_char,
        callback: *mut UiAppLifecycleCallback,
        user_data: *mut c_void,
    ) -> c_int;
    pub fn ui_app_exit();
}

#[link(name = "elementary")]
extern "C" {
    /// Standalone (non-AUL) processes call these instead of `ui_app_main`.
    pub fn elm_init(argc: c_int, argv: *mut *mut c_char) -> c_int;
    pub fn elm_run();
    pub fn elm_exit();
    pub fn elm_shutdown() -> c_int;
    pub fn elm_win_add(parent: *mut c_void, name: *const c_char, window_type: c_int)
        -> *mut c_void;
    pub fn elm_win_title_set(window: *mut c_void, title: *const c_char);
    pub fn elm_win_resize_object_add(window: *mut c_void, child: *mut c_void);
    pub fn elm_win_center(window: *mut c_void, horizontal: EinaBool, vertical: EinaBool);
    pub fn elm_win_activate(window: *mut c_void);
    pub fn elm_win_raise(window: *mut c_void);
    pub fn elm_win_modal_set(window: *mut c_void, modal: EinaBool);
    pub fn elm_win_wl_window_get(window: *mut c_void) -> *mut c_void;
    pub fn elm_win_alpha_set(window: *mut c_void, alpha: EinaBool);
    pub fn elm_win_prop_focus_skip_set(window: *mut c_void, skip: EinaBool);
    pub fn elm_win_aux_hint_add(
        window: *mut c_void,
        hint: *const c_char,
        value: *const c_char,
    ) -> c_int;
    pub fn elm_win_screen_size_get(
        window: *mut c_void,
        x: *mut c_int,
        y: *mut c_int,
        width: *mut c_int,
        height: *mut c_int,
    );

    pub fn elm_table_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_table_pack(
        table: *mut c_void,
        child: *mut c_void,
        column: c_int,
        row: c_int,
        column_span: c_int,
        row_span: c_int,
    );

    pub fn elm_box_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_box_horizontal_set(object: *mut c_void, horizontal: EinaBool);
    pub fn elm_box_padding_set(object: *mut c_void, horizontal: EvasCoord, vertical: EvasCoord);
    pub fn elm_box_pack_end(object: *mut c_void, child: *mut c_void);

    pub fn elm_label_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_label_line_wrap_set(object: *mut c_void, wrap: c_int);
    pub fn elm_label_wrap_width_set(object: *mut c_void, width: EvasCoord);
    pub fn elm_button_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_image_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_object_part_content_set(
        object: *mut c_void,
        part: *const c_char,
        content: *mut c_void,
    );
    pub fn elm_scroller_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_scroller_policy_set(scroller: *mut c_void, horizontal: c_int, vertical: c_int);
    pub fn elm_scroller_bounce_set(scroller: *mut c_void, horizontal: EinaBool, vertical: EinaBool);
    pub fn elm_hoversel_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_hoversel_hover_parent_set(hoversel: *mut c_void, parent: *mut c_void);
    pub fn elm_hoversel_item_add(
        hoversel: *mut c_void,
        label: *const c_char,
        icon_file: *const c_char,
        icon_type: c_int,
        callback: EvasSmartCb,
        data: *const c_void,
    ) -> *mut c_void;
    pub fn elm_image_file_set(
        image: *mut c_void,
        file: *const c_char,
        group: *const c_char,
    ) -> EinaBool;
    pub fn elm_object_part_text_set(object: *mut c_void, part: *const c_char, text: *const c_char);
    pub fn elm_object_focus_set(object: *mut c_void, focus: EinaBool);
}

#[link(name = "evas")]
extern "C" {
    pub fn evas_object_size_hint_weight_set(object: *mut c_void, x: c_double, y: c_double);
    pub fn evas_object_size_hint_align_set(object: *mut c_void, x: c_double, y: c_double);
    pub fn evas_object_size_hint_min_set(object: *mut c_void, width: EvasCoord, height: EvasCoord);
    pub fn evas_object_size_hint_padding_set(
        object: *mut c_void,
        left: EvasCoord,
        right: EvasCoord,
        top: EvasCoord,
        bottom: EvasCoord,
    );
    pub fn evas_object_evas_get(object: *mut c_void) -> *mut c_void;
    pub fn evas_object_rectangle_add(evas: *mut c_void) -> *mut c_void;
    pub fn evas_object_color_set(
        object: *mut c_void,
        red: c_int,
        green: c_int,
        blue: c_int,
        alpha: c_int,
    );
    pub fn evas_object_resize(object: *mut c_void, width: EvasCoord, height: EvasCoord);
    pub fn evas_object_show(object: *mut c_void);
    pub fn evas_object_hide(object: *mut c_void);
    pub fn evas_object_move(object: *mut c_void, x: EvasCoord, y: EvasCoord);
    pub fn evas_object_del(object: *mut c_void);
    pub fn evas_object_smart_callback_add(
        object: *mut c_void,
        event: *const c_char,
        callback: EvasSmartCb,
        data: *mut c_void,
    );
}

#[link(name = "capi-ui-efl-util")]
extern "C" {
    pub fn efl_util_set_notification_window_level(window: *mut c_void, level: c_int) -> c_int;
}

#[link(name = "efl-extension")]
extern "C" {
    pub fn eext_object_event_callback_add(
        object: *mut c_void,
        callback_type: c_int,
        callback: EvasSmartCb,
        data: *mut c_void,
    );
    pub fn eext_win_keygrab_set(window: *mut c_void, key: *const c_char) -> EinaBool;
    pub fn eext_win_keygrab_unset(window: *mut c_void, key: *const c_char) -> EinaBool;
}

pub type EcorePipeCb = unsafe extern "C" fn(data: *mut c_void, buffer: *mut c_void, bytes: u32);

#[link(name = "ecore")]
extern "C" {
    /// `ecore_pipe_write` is the thread-safe way to wake the main loop.
    pub fn ecore_pipe_add(handler: EcorePipeCb, data: *const c_void) -> *mut c_void;
    pub fn ecore_pipe_write(pipe: *mut c_void, buffer: *const c_void, bytes: u32) -> EinaBool;
    pub fn ecore_timer_del(timer: *mut c_void) -> *mut c_void;
    pub fn ecore_timer_add(
        interval: c_double,
        callback: EcoreTimerCb,
        data: *mut c_void,
    ) -> *mut c_void;
}

#[link(name = "ecore_wl2")]
extern "C" {
    pub fn ecore_wl2_window_type_set(window: *mut c_void, window_type: c_int);
}

/// Convert a display string to a NUL-terminated C string.
///
/// Interior NULs are replaced rather than rejected: the daemon already
/// sanitizes prompt text, and a display helper must never abort the UI.
pub fn c_string(value: &str) -> CString {
    let cleaned: String = value
        .chars()
        .map(|character| {
            if character == '\0' {
                '\u{FFFD}'
            } else {
                character
            }
        })
        .collect();
    CString::new(cleaned).unwrap_or_default()
}
