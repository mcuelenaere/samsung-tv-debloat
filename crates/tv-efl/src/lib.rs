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
/// `Elm_Focus_Autoscroll_Mode`: SHOW jumps a scroller to the focused widget;
/// BRING_IN (2) animates there.
pub const ELM_FOCUS_AUTOSCROLL_MODE_SHOW: c_int = 0;
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
pub type EvasObjectEventCb = unsafe extern "C" fn(
    data: *mut c_void,
    evas: *mut c_void,
    object: *mut c_void,
    event_info: *mut c_void,
);

/// `Evas_Callback_Type` `EVAS_CALLBACK_KEY_DOWN`; the event info starts with
/// `const char *keyname`.
pub const EVAS_CALLBACK_KEY_DOWN: c_int = 10;
/// `EVAS_CALLBACK_KEY_UP`; its event info starts with `keyname` too.
pub const EVAS_CALLBACK_KEY_UP: c_int = 11;
/// `Evas_Text_Style_Type` soft shadow.
pub const EVAS_TEXT_STYLE_SOFT_SHADOW: c_int = 8;

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
    /// Deletes every child.
    pub fn elm_box_clear(object: *mut c_void);

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
    pub fn elm_box_pack_after(object: *mut c_void, child: *mut c_void, after: *mut c_void);
    pub fn elm_box_unpack(object: *mut c_void, child: *mut c_void);
    pub fn elm_progressbar_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_progressbar_value_set(object: *mut c_void, value: c_double);
    pub fn elm_progressbar_pulse_set(object: *mut c_void, pulse: EinaBool);
    pub fn elm_progressbar_pulse(object: *mut c_void, state: EinaBool);
    pub fn elm_entry_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_entry_single_line_set(object: *mut c_void, single_line: EinaBool);
    pub fn elm_entry_scrollable_set(object: *mut c_void, scrollable: EinaBool);
    pub fn elm_object_part_text_get(object: *const c_void, part: *const c_char) -> *const c_char;
    pub fn elm_scroller_add(parent: *mut c_void) -> *mut c_void;
    pub fn elm_scroller_policy_set(scroller: *mut c_void, horizontal: c_int, vertical: c_int);
    pub fn elm_scroller_bounce_set(scroller: *mut c_void, horizontal: EinaBool, vertical: EinaBool);
    /// Take the content's minimum size in that direction (a scroller
    /// otherwise computes its own and ignores size hints).
    pub fn elm_scroller_content_min_limit(scroller: *mut c_void, width: EinaBool, height: EinaBool);
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
    pub fn elm_object_scale_set(object: *mut c_void, scale: c_double);
    pub fn elm_config_focus_autoscroll_mode_set(mode: c_int);
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
    pub fn evas_object_image_filled_add(evas: *mut c_void) -> *mut c_void;
    pub fn evas_object_image_file_set(object: *mut c_void, file: *const c_char, key: *const c_char);
    /// `EVAS_LOAD_ERROR_NONE` is 0.
    pub fn evas_object_image_load_error_get(object: *const c_void) -> c_int;
    pub fn evas_object_image_size_get(object: *const c_void, width: *mut c_int, height: *mut c_int);
    /// Row stride in bytes.
    pub fn evas_object_image_stride_get(object: *const c_void) -> c_int;
    pub fn evas_object_image_data_get(object: *const c_void, for_writing: EinaBool) -> *mut c_void;
    pub fn evas_object_image_data_set(object: *mut c_void, data: *mut c_void);
    pub fn evas_object_image_smooth_scale_set(object: *mut c_void, smooth: EinaBool);
    /// Nine-slice: these edges keep their size when the image is scaled.
    pub fn evas_object_image_border_set(
        object: *mut c_void,
        left: c_int,
        right: c_int,
        top: c_int,
        bottom: c_int,
    );
    pub fn evas_object_clip_set(object: *mut c_void, clip: *mut c_void);
    pub fn evas_object_focus_set(object: *mut c_void, focus: EinaBool);
    pub fn evas_object_event_callback_add(
        object: *mut c_void,
        kind: c_int,
        callback: EvasObjectEventCb,
        data: *const c_void,
    );
    pub fn evas_object_text_add(evas: *mut c_void) -> *mut c_void;
    pub fn evas_object_text_font_set(object: *mut c_void, font: *const c_char, size: c_int);
    pub fn evas_object_text_text_set(object: *mut c_void, text: *const c_char);
    pub fn evas_object_text_horiz_advance_get(object: *const c_void) -> c_int;
    pub fn evas_object_text_vert_advance_get(object: *const c_void) -> c_int;
    pub fn evas_object_text_style_set(object: *mut c_void, style: c_int);
    pub fn evas_object_text_shadow_color_set(
        object: *mut c_void,
        r: c_int,
        g: c_int,
        b: c_int,
        a: c_int,
    );
    /// 0.0..1.0 cuts the text to its width with an ellipsis (1.0 at the
    /// end); -1.0 turns it off.
    pub fn evas_object_text_ellipsis_set(object: *mut c_void, ellipsis: c_double);
    /// Decodes the file at this size (the PNG and JPEG loaders scale on
    /// load).
    pub fn evas_object_image_load_size_set(object: *mut c_void, width: c_int, height: c_int);
    pub fn evas_object_textblock_add(evas: *mut c_void) -> *mut c_void;
    pub fn evas_textblock_style_new() -> *mut c_void;
    pub fn evas_textblock_style_set(style: *mut c_void, text: *const c_char);
    pub fn evas_object_textblock_style_set(object: *mut c_void, style: *mut c_void);
    pub fn evas_object_textblock_text_markup_set(object: *mut c_void, text: *const c_char);
    pub fn evas_object_textblock_size_formatted_get(
        object: *const c_void,
        width: *mut c_int,
        height: *mut c_int,
    );
    pub fn evas_object_image_size_set(object: *mut c_void, width: c_int, height: c_int);
    pub fn evas_object_image_alpha_set(object: *mut c_void, alpha: EinaBool);
    /// Copies premultiplied ARGB pixels of the image's size.
    pub fn evas_object_image_data_copy_set(object: *mut c_void, data: *mut c_void);
    pub fn evas_object_image_data_update_add(
        object: *mut c_void,
        x: c_int,
        y: c_int,
        width: c_int,
        height: c_int,
    );
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
    pub fn evas_object_raise(object: *mut c_void);
    pub fn evas_object_lower(object: *mut c_void);
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
    pub fn ecore_timer_reset(timer: *mut c_void);
    /// Runs `callback` once per display frame while it returns 1.
    pub fn ecore_animator_add(callback: EcoreTimerCb, data: *const c_void) -> *mut c_void;
    pub fn ecore_time_get() -> c_double;
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
