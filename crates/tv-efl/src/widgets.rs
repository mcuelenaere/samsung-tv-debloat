//! Small Elementary/Evas building blocks shared by the suite's UIs. All take
//! and return raw EFL object pointers and must run on the EFL main thread.

// The pointers are opaque EFL handles passed straight to C, never
// dereferenced in Rust; EFL checks object validity itself.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use crate::log::log_line;
use crate::*;
use std::os::raw::{c_int, c_void};
use std::ptr;

const CLICKED: &[u8] = b"clicked\0";

pub fn create_rect(parent: *mut c_void, rgb: i32, alpha: i32) -> *mut c_void {
    unsafe {
        let evas = evas_object_evas_get(parent);
        let rect = evas_object_rectangle_add(evas);
        // Evas colours are premultiplied.
        evas_object_color_set(
            rect,
            ((rgb >> 16) & 0xff) * alpha / 255,
            ((rgb >> 8) & 0xff) * alpha / 255,
            (rgb & 0xff) * alpha / 255,
            alpha,
        );
        evas_object_show(rect);
        rect
    }
}

/// A solid rectangle with a padded vertical box stacked on top.
pub fn panel(parent: *mut c_void, rgb: i32, pad_x: i32, pad_y: i32) -> (*mut c_void, *mut c_void) {
    unsafe {
        let table = elm_table_add(parent);
        evas_object_size_hint_weight_set(table, 1.0, 0.0);
        evas_object_size_hint_align_set(table, EVAS_HINT_FILL, 0.5);

        let rect = create_rect(table, rgb, 255);
        evas_object_size_hint_weight_set(rect, 1.0, 1.0);
        evas_object_size_hint_align_set(rect, EVAS_HINT_FILL, EVAS_HINT_FILL);
        elm_table_pack(table, rect, 0, 0, 1, 1);

        let inner = elm_box_add(table);
        evas_object_size_hint_weight_set(inner, 1.0, 1.0);
        evas_object_size_hint_align_set(inner, EVAS_HINT_FILL, EVAS_HINT_FILL);
        evas_object_size_hint_padding_set(inner, pad_x, pad_x, pad_y, pad_y);
        elm_box_padding_set(inner, 0, 6);
        elm_table_pack(table, inner, 0, 0, 1, 1);
        evas_object_show(inner);

        evas_object_show(table);
        (table, inner)
    }
}

/// Horizontal row: the first label stretches, buttons hug the right edge.
pub fn create_row(parent: *mut c_void) -> *mut c_void {
    unsafe {
        let row = elm_box_add(parent);
        elm_box_horizontal_set(row, 1);
        elm_box_padding_set(row, 16, 0);
        evas_object_size_hint_weight_set(row, 1.0, 0.0);
        evas_object_size_hint_align_set(row, EVAS_HINT_FILL, 0.5);
        evas_object_size_hint_padding_set(row, 0, 0, 12, 0);
        evas_object_show(row);
        row
    }
}

pub fn create_label(parent: *mut c_void, markup: &str) -> *mut c_void {
    unsafe {
        let label = elm_label_add(parent);
        if label.is_null() {
            return ptr::null_mut();
        }
        set_text(label, markup);
        elm_label_line_wrap_set(label, ELM_WRAP_WORD);
        evas_object_size_hint_weight_set(label, 1.0, 0.0);
        evas_object_size_hint_align_set(label, EVAS_HINT_FILL, 0.5);
        evas_object_show(label);
        label
    }
}

pub fn add_to_box(box_: *mut c_void, child: *mut c_void) -> *mut c_void {
    if !child.is_null() {
        unsafe { elm_box_pack_end(box_, child) };
    }
    child
}

pub fn create_button(
    parent: *mut c_void,
    text: &str,
    callback: EvasSmartCb,
    data: *mut c_void,
) -> *mut c_void {
    unsafe {
        let button = elm_button_add(parent);
        if button.is_null() {
            return ptr::null_mut();
        }
        set_text(button, text);
        evas_object_size_hint_min_set(button, 260, 84);
        evas_object_size_hint_weight_set(button, 0.0, 0.0);
        evas_object_size_hint_align_set(button, 0.5, 0.5);
        evas_object_smart_callback_add(button, CLICKED.as_ptr().cast(), callback, data);
        elm_box_pack_end(parent, button);
        evas_object_show(button);
        button
    }
}

pub fn set_text(object: *mut c_void, text: &str) {
    if object.is_null() {
        return;
    }
    let value = c_string(text);
    unsafe { elm_object_part_text_set(object, ptr::null(), value.as_ptr()) };
}

pub fn set_window_title(window: *mut c_void, text: &str) {
    let value = c_string(text);
    unsafe { elm_win_title_set(window, value.as_ptr()) };
}

pub fn set_highest_notification_level(window: *mut c_void) {
    const LEVELS: &[(&str, c_int)] = &[
        ("TOP", NOTIFICATION_TOP),
        ("HIGH", NOTIFICATION_HIGH),
        ("MEDIUM", NOTIFICATION_MEDIUM),
        ("DEFAULT", NOTIFICATION_DEFAULT),
    ];
    for &(name, level) in LEVELS {
        let result = unsafe { efl_util_set_notification_window_level(window, level) };
        if result == 0 {
            log_line(&format!("notification level {name} accepted"));
            return;
        }
        log_line(&format!("notification level {name} rejected with {result}"));
    }
    log_line("using platform-default notification level");
}
