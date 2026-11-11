//! Small evas helpers shared by the dock and the manager: an object that
//! remembers what it was given (so a frame only calls into EFL for what
//! changed), images from pixels, text objects.

use std::os::raw::c_void;
use tv_efl::*;

/// Position and size, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl From<tv_auth_ui::dock_core::Rect> for Bounds {
    fn from(rect: tv_auth_ui::dock_core::Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        }
    }
}

impl From<tv_auth_ui::page::Rect> for Bounds {
    fn from(rect: tv_auth_ui::page::Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        }
    }
}

/// An evas object and the values last given to it.
pub struct Obj {
    pub raw: *mut c_void,
    bounds: Option<Bounds>,
    color: Option<[i32; 4]>,
    visible: Option<bool>,
}

impl Obj {
    pub fn new(raw: *mut c_void) -> Self {
        Self {
            raw,
            bounds: None,
            color: None,
            visible: None,
        }
    }

    pub fn place(&mut self, bounds: impl Into<Bounds>) {
        let bounds = bounds.into();
        if self.bounds != Some(bounds) {
            unsafe {
                evas_object_move(self.raw, bounds.x, bounds.y);
                evas_object_resize(self.raw, bounds.w, bounds.h);
            }
            self.bounds = Some(bounds);
        }
    }

    pub fn move_to(&mut self, x: i32, y: i32) {
        let bounds = Bounds { x, y, w: -1, h: -1 };
        if self.bounds != Some(bounds) {
            unsafe { evas_object_move(self.raw, x, y) };
            self.bounds = Some(bounds);
        }
    }

    /// Premultiplied tint.
    pub fn tint(&mut self, color: [i32; 4]) {
        if self.color != Some(color) {
            unsafe { evas_object_color_set(self.raw, color[0], color[1], color[2], color[3]) };
            self.color = Some(color);
        }
    }

    pub fn tinted(&self) -> Option<[i32; 4]> {
        self.color
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.visible != Some(visible) {
            unsafe {
                if visible {
                    evas_object_show(self.raw);
                } else {
                    evas_object_hide(self.raw);
                }
            }
            self.visible = Some(visible);
        }
    }

    pub fn delete(&self) {
        unsafe { evas_object_del(self.raw) };
    }
}

/// `rgb` at `alpha` (0..255), premultiplied, for [`Obj::tint`].
pub fn premultiplied(rgb: u32, alpha: i32) -> [i32; 4] {
    let channel = |shift: u32| ((rgb >> shift) & 0xff) as i32 * alpha / 255;
    [channel(16), channel(8), channel(0), alpha]
}

/// An image object showing `pixels` (`width` x `height`, premultiplied).
///
/// # Safety
/// `evas` is a live canvas; call on the EFL main thread.
pub unsafe fn picture(evas: *mut c_void, pixels: &[u32], width: i32, height: i32) -> *mut c_void {
    let image = evas_object_image_filled_add(evas);
    evas_object_image_size_set(image, width, height);
    evas_object_image_alpha_set(image, 1);
    evas_object_image_data_copy_set(image, pixels.as_ptr() as *mut c_void);
    evas_object_image_data_update_add(image, 0, 0, width, height);
    evas_object_image_smooth_scale_set(image, 1);
    evas_object_show(image);
    image
}

/// [`picture`] of a square nine-slice: `border` pixels of each edge keep
/// their size when it is stretched.
///
/// # Safety
/// As [`picture`].
pub unsafe fn sliced(evas: *mut c_void, pixels: &[u32], border: i32) -> *mut c_void {
    let side = side_of(pixels);
    let image = picture(evas, pixels, side, side);
    evas_object_image_border_set(image, border, border, border, border);
    image
}

pub fn side_of(pixels: &[u32]) -> i32 {
    (pixels.len() as f64).sqrt().round() as i32
}

/// A text object in `font` at `size`, with a soft shadow when `shadow`.
///
/// # Safety
/// As [`picture`].
pub unsafe fn text(
    evas: *mut c_void,
    font: &str,
    size: i32,
    value: &str,
    shadow: bool,
) -> *mut c_void {
    let object = evas_object_text_add(evas);
    let font = c_string(font);
    evas_object_text_font_set(object, font.as_ptr(), size);
    if shadow {
        evas_object_text_style_set(object, EVAS_TEXT_STYLE_SOFT_SHADOW);
        evas_object_text_shadow_color_set(object, 0, 0, 0, 150);
    }
    set_text_value(object, value);
    evas_object_show(object);
    object
}

/// # Safety
/// `object` is a live evas text object.
pub unsafe fn set_text_value(object: *mut c_void, value: &str) {
    let value = c_string(value);
    evas_object_text_text_set(object, value.as_ptr());
}

/// Width a text object would take for its current text.
///
/// # Safety
/// `object` is a live evas text object.
pub unsafe fn advance(object: *mut c_void) -> i32 {
    evas_object_text_horiz_advance_get(object)
}
