//! SVG to pixels with the firmware's thorvg (evas has no SVG loader).
use crate::c_string;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::Once;

const ENGINE_SW: c_int = 2;
/// Premultiplied ARGB, what an evas image takes.
const COLORSPACE_ARGB8888: c_int = 1;

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
    fn tvg_canvas_destroy(canvas: *mut c_void) -> c_int;
}

/// `path` drawn into a `size`×`size` premultiplied ARGB buffer.
pub fn rasterize(path: &str, size: u32) -> Result<Vec<u32>, String> {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        tvg_engine_init(ENGINE_SW, 0);
    });
    let file = c_string(path)?;
    let mut pixels = vec![0_u32; (size * size) as usize];
    unsafe {
        let canvas = tvg_swcanvas_create();
        if canvas.is_null() {
            return Err("tvg_swcanvas_create failed".into());
        }
        let picture = tvg_picture_new();
        let steps = [
            tvg_swcanvas_set_target(
                canvas,
                pixels.as_mut_ptr(),
                size,
                size,
                size,
                COLORSPACE_ARGB8888,
            ),
            tvg_picture_load(picture, file.as_ptr()),
            tvg_picture_set_size(picture, size as f32, size as f32),
            tvg_canvas_push(canvas, picture), // the canvas now owns the picture
            tvg_canvas_draw(canvas),
            tvg_canvas_sync(canvas),
        ];
        tvg_canvas_destroy(canvas);
        if let Some(status) = steps.iter().find(|status| **status != 0) {
            return Err(format!("thorvg {path}: error {status}"));
        }
    }
    Ok(pixels)
}
