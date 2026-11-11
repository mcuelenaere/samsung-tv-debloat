//! `org.tvauth.home`: the dock that answers the Home key
//! (docs/tv-launcher-plan.md). Same executable as the manager; `main`
//! picks this when AUL starts it under the dock's app id.
//!
//! tv-patchd makes this app the default for
//! `http://tizen.org/appcontrol/operation/main`, so tvctx-manager launches
//! or resumes it on every Home press. Each press arrives as `app_control`:
//! it shows the dock, or hides it when it is already shown.
//!
//! A bar at the bottom of the screen over whatever is playing. Three rows
//! ([`core`]): the apps (Samsung Home's catalog, [`catalog`]); Up for the
//! inputs; Down for Samsung Home, Settings and TV suite. No Elementary
//! widgets: plain evas images and text placed by hand, driven by an ecore
//! animator that runs only while something moves and only touches values
//! that changed. A watcher thread rebuilds the rows when webappservice's
//! database or Home's row or source cache change.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};
use tv_auth_ui::catalog::{self, App, Source};
use tv_auth_ui::dock_core::{self as core, Action, Controller, Rect, Row, Strip, Window};
use tv_auth_ui::pixels;
use tv_efl::log::log_line;
use tv_efl::*;

pub const APP_ID: &str = "org.tvauth.home";
const WINDOW_NAME: &[u8] = b"org.tvauth.home\0";
const FONT: &str = "SamsungOneUI 500,Sans";

const COLOR_BAR: u32 = 0x2a_33_46;
/// Behind icons with transparency or another shape than a square.
const COLOR_TILE: u32 = 0x3a_44_58;
/// Behind Samsung's dark SVG icons (devices); light ones (functions) get
/// [`COLOR_TILE`].
const COLOR_SVG_TILE: u32 = 0xe8_ea_ee;
const COLOR_GLOW: u32 = 0xe2_ed_ff;
const COLOR_CURRENT: u32 = 0x4c_9a_ff;
const GLOW_STRENGTH: f32 = 0.5;
/// Icons are finished at up to this side; evas scales them to the tile.
const ICON_SIDE: usize = 2 * core::FOCUS_ICON as usize;
const SVG_SIDE: u32 = 128;
const SHADOW_BLUR: i32 = 28;
const SHADOW_DY: i32 = 10;
const BAR_SLICE: i32 = core::BAR_RADIUS + 4;
const CHEVRON: i32 = 22;
const DOT: i32 = 8;
/// Writes settle within this before the catalog is read again.
const SETTLE: Duration = Duration::from_millis(300);

const SAMSUNG_HOME: &str = "com.samsung.tv.csfs";
const SAMSUNG_HOME_ICON: &str =
    "/usr/apps/com.samsung.tv.theme-resource/shared/res/VD_ICONS/i_icon_function_apps.svg";
const SETTINGS: &str = "org.tizen.menu";
const SETTINGS_ICON: &str =
    "/usr/apps/com.samsung.tv.theme-resource/shared/res/VD_ICONS/i_icon_function_settings.svg";
const MANAGER: &str = "org.tvauth.ui";
const MANAGER_ICON: &str = "/opt/usr/apps/org.tvauth.ui/shared/res/org.tvauth.ui.png";
const TV_VIEWER: &str = "org.tizen.tv-viewer";
const SOURCE_TYPE: &str = "memory/system/source_type";

/// What the dock shows besides its fixed entries.
#[derive(Clone, Default, PartialEq)]
struct Catalog {
    apps: Vec<App>,
    sources: Vec<Source>,
}

#[derive(Clone)]
enum Target {
    App(String),
    Source(Source),
}

#[derive(Clone)]
struct Entry {
    title: String,
    target: Target,
    icon: Option<String>,
}

impl Entry {
    fn app(title: &str, app_id: &str, icon: Option<&str>) -> Self {
        Self {
            title: title.into(),
            target: Target::App(app_id.into()),
            icon: icon.map(Into::into),
        }
    }

    /// Identifies the entry across catalog refreshes.
    fn key(&self) -> String {
        match &self.target {
            Target::App(app_id) => app_id.clone(),
            Target::Source(source) => format!("source:{}", source.kind),
        }
    }
}

fn entries(catalog: &Catalog) -> [Vec<Entry>; 3] {
    let apps = catalog
        .apps
        .iter()
        .map(|app| Entry {
            title: app.title.clone(),
            target: Target::App(app.app_id.clone()),
            icon: app
                .icon
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        })
        .collect();
    let inputs = catalog
        .sources
        .iter()
        .map(|source| Entry {
            title: source.name.clone(),
            target: Target::Source(source.clone()),
            icon: source.icon.clone(),
        })
        .collect();
    let settings = vec![
        Entry::app("Samsung Home", SAMSUNG_HOME, Some(SAMSUNG_HOME_ICON)),
        Entry::app("Settings", SETTINGS, Some(SETTINGS_ICON)),
        Entry::app("TV suite", MANAGER, Some(MANAGER_ICON)),
    ];
    [apps, inputs, settings]
}

/// An evas object and the values last given to it, so a frame only calls
/// into EFL for what changed.
struct Obj {
    raw: *mut c_void,
    rect: Option<Rect>,
    color: Option<[i32; 4]>,
}

impl Obj {
    fn new(raw: *mut c_void) -> Self {
        Self {
            raw,
            rect: None,
            color: None,
        }
    }

    fn place(&mut self, rect: Rect) {
        if self.rect != Some(rect) {
            unsafe {
                evas_object_move(self.raw, rect.x, rect.y);
                evas_object_resize(self.raw, rect.w, rect.h);
            }
            self.rect = Some(rect);
        }
    }

    fn move_to(&mut self, x: i32, y: i32) {
        let rect = Rect { x, y, w: 0, h: 0 };
        if self.rect != Some(rect) {
            unsafe { evas_object_move(self.raw, x, y) };
            self.rect = Some(rect);
        }
    }

    /// Premultiplied tint.
    fn tint(&mut self, color: [i32; 4]) {
        if self.color != Some(color) {
            unsafe { evas_object_color_set(self.raw, color[0], color[1], color[2], color[3]) };
            self.color = Some(color);
        }
    }

    fn delete(&self) {
        unsafe { evas_object_del(self.raw) };
    }
}

/// A text object's colour and shadow at `alpha` (0..255).
fn text_alpha(text: &mut Obj, alpha: i32) {
    if text.color != Some([alpha; 4]) {
        unsafe { evas_object_text_shadow_color_set(text.raw, 0, 0, 0, 150 * alpha / 255) };
    }
    text.tint([alpha; 4]);
}

struct Icon {
    image: Obj,
    glow: u32,
    /// Initials over a plain tile, when the icon could not be loaded.
    initials: Option<(Obj, i32, i32)>,
}

struct Hint {
    text: Obj,
    chevron: Obj,
    width: i32,
    text_h: i32,
}

/// The evas objects of one row.
struct RowView {
    strip: Strip,
    /// Everything of the row is clipped by this; its colour fades the row.
    clip: Obj,
    /// The icons scroll inside this.
    icon_clip: Obj,
    shadow: Obj,
    bar: Obj,
    glow: Obj,
    icons: Vec<Icon>,
    ring: Obj,
    dot: Obj,
    label: Obj,
    label_index: Option<usize>,
    up: Option<Hint>,
    down: Option<Hint>,
}

/// Pictures every row shares, drawn once.
struct Art {
    bar: Vec<u32>,
    shadow: Vec<u32>,
    ring: Vec<u32>,
    glow: Vec<u32>,
    chevron_up: Vec<u32>,
    chevron_down: Vec<u32>,
    dot: Vec<u32>,
}

impl Art {
    fn new() -> Self {
        let slice = BAR_SLICE as usize;
        Self {
            bar: pixels::panel(
                2 * slice + 1,
                core::BAR_RADIUS as f32,
                COLOR_BAR,
                0.97,
                2.0,
                0xff_ff_ff,
                0.22,
            ),
            shadow: pixels::shadow(core::BAR_RADIUS as usize, SHADOW_BLUR as usize, 0.45),
            ring: pixels::ring(core::FOCUS_ICON as usize, core::RING_W as usize),
            glow: pixels::glow(
                core::FOCUS_ICON as usize,
                core::GLOW_SPREAD as usize,
                GLOW_STRENGTH,
            ),
            chevron_up: pixels::chevron(CHEVRON as usize, true, 2.5),
            chevron_down: pixels::chevron(CHEVRON as usize, false, 2.5),
            dot: pixels::dot(DOT as usize, COLOR_CURRENT),
        }
    }
}

/// An icon file and its modification time.
type IconKey = (String, Option<SystemTime>);

struct Dock {
    window: *mut c_void,
    evas: *mut c_void,
    screen: (i32, i32),
    win: Window,
    art: Art,
    entries: [Vec<Entry>; 3],
    rows: Vec<RowView>,
    ctl: Controller,
    animator: *mut c_void,
    last_frame: f64,
    idle: *mut c_void,
    /// Opened once the hide has finished, so the fade stays smooth.
    pending: Option<Target>,
    /// Index of the current input in the inputs row.
    current_input: Option<usize>,
    updates: Option<Receiver<Catalog>>,
    /// Finished icons by path and modification time: a reorder or refresh
    /// only creates objects.
    icons: HashMap<IconKey, Option<(Vec<u32>, u32)>>,
    /// This animation run: frames, longest gap between frames and longest
    /// own work per frame, in seconds.
    run: (u32, f64, f64),
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

/// webappservice's catalog and Home's inputs, in Home's order.
fn load() -> Catalog {
    let rows = match tv_tizen::pkgdb::query_text(catalog::DATABASE, catalog::SQL, &[]) {
        Ok(rows) => rows,
        Err(error) => {
            log_line(&format!("dock: catalog: {error}"));
            Vec::new()
        }
    };
    let row = std::fs::read_to_string(catalog::ROW).unwrap_or_default();
    let cache = std::fs::read_to_string(catalog::SOURCES).unwrap_or_default();
    Catalog {
        apps: catalog::order(catalog::parse(&rows), &catalog::row_ids(&row)),
        sources: catalog::sources(&cache, &row),
    }
}

/// Watcher thread: after every settled change, the catalog if it differs
/// (webappservice writes on every launch too), then a wake-up for the main
/// loop through `pipe`.
fn watch(pipe: usize, updates: mpsc::Sender<Catalog>, mut last: Catalog) {
    let watch = match tv_tizen::watch::Watch::new(&["/opt/dbspace", "/run/tv-suite"]) {
        Ok(watch) => watch,
        Err(error) => return log_line(&format!("dock: {error}; the app list will not refresh")),
    };
    let wanted = |name: &str| {
        name.starts_with(".webappservice.db") || name == "home-row" || name == "home-sources"
    };
    loop {
        if let Err(error) = watch.wait(wanted, SETTLE) {
            return log_line(&format!("dock: watch: {error}"));
        }
        let fresh = load();
        if fresh == last {
            continue;
        }
        last = fresh.clone();
        if updates.send(fresh).is_err() {
            return;
        }
        unsafe { ecore_pipe_write(pipe as *mut c_void, b"r".as_ptr().cast(), 1) };
    }
}

/// An image object showing `pixels` (`width` x `height`, premultiplied).
unsafe fn picture(evas: *mut c_void, pixels: &[u32], width: i32, height: i32) -> *mut c_void {
    let image = evas_object_image_filled_add(evas);
    evas_object_image_size_set(image, width, height);
    evas_object_image_alpha_set(image, 1);
    evas_object_image_data_copy_set(image, pixels.as_ptr() as *mut c_void);
    evas_object_image_data_update_add(image, 0, 0, width, height);
    evas_object_image_smooth_scale_set(image, 1);
    evas_object_show(image);
    image
}

fn side_of(pixels: &[u32]) -> i32 {
    (pixels.len() as f64).sqrt().round() as i32
}

/// A finished icon's pixels (a square, at most [`ICON_SIDE`]): rounded, on a
/// tile; and its glow colour.
unsafe fn load_icon(evas: *mut c_void, path: &str) -> Option<(Vec<u32>, u32)> {
    if path.ends_with(".svg") {
        let art = match tv_tizen::svg::rasterize(path, SVG_SIDE) {
            Ok(art) => art,
            Err(error) => {
                log_line(&format!("dock: {error}"));
                return None;
            }
        };
        let side = ICON_SIDE;
        let backdrop = if pixels::is_light(&art) {
            COLOR_TILE
        } else {
            COLOR_SVG_TILE
        };
        let mut tile = vec![pixels::premultiply(backdrop, 1.0); side * side];
        let offset = (side - SVG_SIDE as usize) / 2;
        pixels::over(&mut tile, side, &art, SVG_SIDE as usize, offset, offset);
        pixels::round_corners(&mut tile, side, side as f32 * pixels::RADIUS_SHARE);
        return Some((tile, COLOR_GLOW));
    }
    let image = evas_object_image_filled_add(evas);
    let file = c_string(path);
    evas_object_image_file_set(image, file.as_ptr(), ptr::null());
    let (mut width, mut height) = (0, 0);
    evas_object_image_size_get(image, &mut width, &mut height);
    let data = evas_object_image_data_get(image, 0);
    if evas_object_image_load_error_get(image) != 0
        || data.is_null()
        || !(1..=4096).contains(&width)
        || !(1..=4096).contains(&height)
    {
        log_line(&format!("dock: icon {path} not loadable"));
        evas_object_del(image);
        return None;
    }
    let (width, height) = (width as usize, height as usize);
    let stride = (evas_object_image_stride_get(image) as usize).max(width * 4);
    let mut source = Vec::with_capacity(width * height);
    for row in 0..height {
        let line = data.cast::<u8>().add(row * stride).cast::<u32>();
        source.extend_from_slice(std::slice::from_raw_parts(line, width));
    }
    let glow = pixels::dominant(&source).unwrap_or(COLOR_GLOW);
    let side = pixels::icon_side(width, height, ICON_SIDE);
    let finished = pixels::icon(&source, width, height, side, COLOR_TILE);
    evas_object_del(image);
    Some((finished, glow))
}

unsafe fn text(evas: *mut c_void, size: i32, value: &str) -> *mut c_void {
    let object = evas_object_text_add(evas);
    let font = c_string(FONT);
    evas_object_text_font_set(object, font.as_ptr(), size);
    evas_object_text_style_set(object, EVAS_TEXT_STYLE_SOFT_SHADOW);
    evas_object_text_shadow_color_set(object, 0, 0, 0, 150);
    let value = c_string(value);
    evas_object_text_text_set(object, value.as_ptr());
    evas_object_show(object);
    object
}

fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
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
            self.evas = evas_object_evas_get(window);
            elm_win_alpha_set(window, 1);
            let wayland_window = elm_win_wl_window_get(window);
            if !wayland_window.is_null() {
                ecore_wl2_window_type_set(wayland_window, ECORE_WL2_WINDOW_TYPE_NOTIFICATION);
            }
            // Above the app being watched, below tv-auth-popupd's prompts (TOP).
            if efl_util_set_notification_window_level(window, NOTIFICATION_HIGH) != 0 {
                log_line("dock: notification level HIGH refused");
            }
            for (hint, value) in [
                ("wm.policy.win.user.geometry", "1"),
                ("wm.comp.win.effect.enable", "0"),
            ] {
                let (hint, value) = (c_string(hint), c_string(value));
                elm_win_aux_hint_add(window, hint.as_ptr(), value.as_ptr());
            }
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
            self.screen = (width, height);
            evas_object_event_callback_add(window, EVAS_CALLBACK_KEY_DOWN, on_key, ptr::null());
        }
        let first = load();
        let last = first.clone();
        self.build(&first);

        let (sender, receiver) = mpsc::channel();
        self.updates = Some(receiver);
        let pipe = unsafe { ecore_pipe_add(on_update, ptr::null()) } as usize;
        std::thread::spawn(move || watch(pipe, sender, last));
        true
    }

    /// (Re)creates every row's objects for `catalog`.
    fn build(&mut self, catalog: &Catalog) {
        let started = unsafe { ecore_time_get() };
        // Keep each row's focus on the same entry.
        let keys: Vec<Option<String>> = Row::ALL
            .iter()
            .map(|row| {
                self.entries[row.index()]
                    .get(self.ctl.focus[row.index()].target)
                    .map(Entry::key)
            })
            .collect();
        for row in self.rows.drain(..) {
            row.delete();
        }
        self.entries = entries(catalog);
        let counts = self.entries.each_ref().map(Vec::len);
        self.win = Window::new(self.screen.0, self.screen.1, counts);
        let rect = self.win.rect;
        unsafe {
            evas_object_move(self.window, rect.x, rect.y);
            evas_object_resize(self.window, rect.w, rect.h);
        }
        for row in Row::ALL {
            let view = self.build_row(row);
            self.rows.push(view);
        }
        self.ctl.set_counts(counts);
        for row in Row::ALL {
            let found = keys[row.index()].as_ref().and_then(|key| {
                self.entries[row.index()]
                    .iter()
                    .position(|entry| &entry.key() == key)
            });
            if let Some(index) = found {
                self.ctl.focus[row.index()].jump(index);
            }
        }
        self.current_input = self.find_current_input();
        self.apply();
        log_line(&format!(
            "dock: {} apps, {} inputs, window {}x{} at {}, built in {:.0} ms",
            counts[0],
            counts[1],
            rect.w,
            rect.h,
            rect.y,
            (unsafe { ecore_time_get() } - started) * 1000.0
        ));
    }

    fn build_row(&mut self, row: Row) -> RowView {
        let evas = self.evas;
        let art = &self.art;
        let cache = &mut self.icons;
        let strip = self
            .win
            .strip(self.entries[row.index()].len(), self.screen.0);
        unsafe {
            let clip = Obj::new(evas_object_rectangle_add(evas));
            let icon_clip = Obj::new(evas_object_rectangle_add(evas));
            evas_object_color_set(icon_clip.raw, 255, 255, 255, 255);
            evas_object_clip_set(icon_clip.raw, clip.raw);
            let shadow_slice = core::BAR_RADIUS + SHADOW_BLUR;
            let shadow = Obj::new(picture(
                evas,
                &art.shadow,
                side_of(&art.shadow),
                side_of(&art.shadow),
            ));
            evas_object_image_border_set(
                shadow.raw,
                shadow_slice,
                shadow_slice,
                shadow_slice,
                shadow_slice,
            );
            let bar = Obj::new(picture(
                evas,
                &art.bar,
                side_of(&art.bar),
                side_of(&art.bar),
            ));
            evas_object_image_border_set(bar.raw, BAR_SLICE, BAR_SLICE, BAR_SLICE, BAR_SLICE);
            let glow = Obj::new(picture(
                evas,
                &art.glow,
                side_of(&art.glow),
                side_of(&art.glow),
            ));
            let icons = self.entries[row.index()]
                .iter()
                .map(|entry| {
                    let loaded = entry.icon.as_deref().and_then(|path| {
                        // Keyed by modification time too, so an updated icon is reloaded.
                        let modified = std::fs::metadata(path)
                            .and_then(|meta| meta.modified())
                            .ok();
                        cache
                            .entry((path.to_owned(), modified))
                            .or_insert_with(|| load_icon(evas, path))
                            .clone()
                    });
                    let (image, glow, initials) = match loaded {
                        Some((pixels, glow)) => {
                            let side = side_of(&pixels);
                            (picture(evas, &pixels, side, side), glow, None)
                        }
                        None => {
                            let tile = pixels::icon(
                                &[pixels::premultiply(COLOR_TILE, 1.0)],
                                1,
                                1,
                                64,
                                COLOR_TILE,
                            );
                            let image = picture(evas, &tile, 64, 64);
                            let letters = text(evas, 34, &initials(&entry.title));
                            let size = (
                                evas_object_text_horiz_advance_get(letters),
                                evas_object_text_vert_advance_get(letters),
                            );
                            (image, COLOR_GLOW, Some((Obj::new(letters), size.0, size.1)))
                        }
                    };
                    evas_object_clip_set(image, icon_clip.raw);
                    if let Some((letters, _, _)) = &initials {
                        evas_object_clip_set(letters.raw, icon_clip.raw);
                    }
                    Icon {
                        image: Obj::new(image),
                        glow,
                        initials,
                    }
                })
                .collect();
            let ring = Obj::new(picture(
                evas,
                &art.ring,
                side_of(&art.ring),
                side_of(&art.ring),
            ));
            let dot = Obj::new(picture(evas, &art.dot, DOT, DOT));
            let label = Obj::new(text(evas, 26, ""));
            let hint = |word: &str, up: bool| {
                let chevron = if up {
                    &art.chevron_up
                } else {
                    &art.chevron_down
                };
                let text = text(evas, 22, word);
                Hint {
                    width: evas_object_text_horiz_advance_get(text) + 4 + CHEVRON,
                    text_h: evas_object_text_vert_advance_get(text),
                    text: Obj::new(text),
                    chevron: Obj::new(picture(evas, chevron, CHEVRON, CHEVRON)),
                }
            };
            let up = row.up().map(|(_, word)| hint(word, true));
            let down = row.down().map(|(_, word)| hint(word, false));
            for object in [&shadow, &bar, &glow, &ring, &dot, &label] {
                evas_object_clip_set(object.raw, clip.raw);
            }
            for hint in up.iter().chain(down.iter()) {
                evas_object_clip_set(hint.text.raw, clip.raw);
                evas_object_clip_set(hint.chevron.raw, clip.raw);
            }
            let mut view = RowView {
                strip,
                clip,
                icon_clip,
                shadow,
                bar,
                glow,
                icons,
                ring,
                dot,
                label,
                label_index: None,
                up,
                down,
            };
            view.clip.place(Rect {
                x: 0,
                y: 0,
                w: self.win.rect.w,
                h: self.win.rect.h,
            });
            view.clip.tint([0; 4]);
            evas_object_show(view.clip.raw);
            evas_object_show(view.icon_clip.raw);
            view
        }
    }

    /// Maps the controller's state onto every row's objects.
    fn apply(&mut self) {
        for row in Row::ALL {
            let fade = self.ctl.fade[row.index()];
            let focus = self.ctl.focus[row.index()];
            let entries = &self.entries[row.index()];
            let current = if row == Row::Inputs {
                self.current_input
            } else {
                None
            };
            let view = &mut self.rows[row.index()];
            let alpha = (fade.presence * 255.0).round() as i32;
            view.clip.tint([alpha; 4]);
            if alpha == 0 {
                continue;
            }
            let dy = fade.offset();
            let shift = |rect: Rect| Rect {
                y: rect.y + dy,
                ..rect
            };
            let strip = view.strip;
            let scroll = strip.scroll(focus.position);
            let bar = shift(strip.bar);
            view.bar.place(bar);
            view.shadow.place(Rect {
                x: bar.x - SHADOW_BLUR,
                y: bar.y - SHADOW_BLUR + SHADOW_DY,
                w: bar.w + 2 * SHADOW_BLUR,
                h: bar.h + 2 * SHADOW_BLUR,
            });
            view.icon_clip.place(strip.clip());
            for (index, icon) in view.icons.iter_mut().enumerate() {
                let rect = shift(strip.icon(index, focus.position, scroll));
                icon.image.place(rect);
                if let Some((letters, width, height)) = &mut icon.initials {
                    letters.move_to(
                        rect.x + (rect.w - *width) / 2,
                        rect.y + (rect.h - *height) / 2,
                    );
                }
            }
            if view.icons.is_empty() {
                view.ring.tint([0; 4]);
                view.glow.tint([0; 4]);
            } else {
                view.ring.tint([255; 4]);
                let ring = core::FOCUS_ICON + 2 * core::RING_W;
                view.ring
                    .place(shift(strip.around_focus(focus.position, scroll, ring)));
                let glow = core::FOCUS_ICON + 2 * core::GLOW_SPREAD;
                view.glow
                    .place(shift(strip.around_focus(focus.position, scroll, glow)));
                // The glow's colour moves from one icon's to the next with the selection.
                let last = view.icons.len() - 1;
                let low = (focus.position.floor() as usize).min(last);
                let high = (low + 1).min(last);
                let t = focus.position - low as f32;
                let (from, to) = (view.icons[low].glow, view.icons[high].glow);
                let channel = |shift: u32| {
                    let (a, b) = (
                        ((from >> shift) & 0xff) as f32,
                        ((to >> shift) & 0xff) as f32,
                    );
                    (a + (b - a) * t).round() as i32
                };
                view.glow.tint([channel(16), channel(8), channel(0), 255]);
            }
            match current {
                Some(index) if index < view.icons.len() => {
                    let icon = strip.icon(index, focus.position, scroll);
                    view.dot.tint([255; 4]);
                    view.dot.place(shift(Rect {
                        x: icon.x + (icon.w - DOT) / 2,
                        y: strip.row_y() + core::FOCUS_ICON / 2 + 1,
                        w: DOT,
                        h: DOT,
                    }));
                }
                _ => view.dot.tint([0; 4]),
            }
            // The name under the focused icon fades in as the selection arrives.
            if let Some(entry) = entries.get(focus.target) {
                if view.label_index != Some(focus.target) {
                    let value = c_string(&entry.title);
                    unsafe { evas_object_text_text_set(view.label.raw, value.as_ptr()) };
                    view.label_index = Some(focus.target);
                }
                let width = unsafe { evas_object_text_horiz_advance_get(view.label.raw) };
                view.label.move_to(
                    strip.label_x(focus.target, scroll, width),
                    strip.label_y() + dy,
                );
                let arrive = ((focus.amount(focus.target) - 0.5) / 0.5).clamp(0.0, 1.0);
                text_alpha(
                    &mut view.label,
                    (arrive * arrive * (3.0 - 2.0 * arrive) * 255.0) as i32,
                );
            }
            let centre = bar.x + bar.w / 2;
            if let Some(hint) = &mut view.up {
                place_hint(hint, centre, bar.y - core::HINT_H / 2 - 4);
            }
            if let Some(hint) = &mut view.down {
                place_hint(hint, centre, bar.y + bar.h + core::HINT_H / 2 + 2);
            }
        }
    }

    fn find_current_input(&self) -> Option<usize> {
        let kind = tv_tizen::vconf::get_int(SOURCE_TYPE).ok()?;
        self.entries[Row::Inputs.index()].iter().position(|entry| {
            matches!(&entry.target, Target::Source(source) if source.kind == i64::from(kind))
        })
    }

    fn start_animator(&mut self) {
        if self.animator.is_null() {
            self.last_frame = unsafe { ecore_time_get() };
            self.animator = unsafe { ecore_animator_add(on_frame, ptr::null()) };
        }
    }

    fn restart_idle(&mut self) {
        unsafe {
            if self.idle.is_null() {
                self.idle = ecore_timer_add(core::IDLE_SECONDS, on_idle, ptr::null_mut());
            } else {
                ecore_timer_reset(self.idle);
            }
        }
    }

    fn show(&mut self) {
        self.pending = None;
        self.ctl.show(0);
        self.apply();
        unsafe {
            evas_object_show(self.window);
            elm_win_raise(self.window);
            elm_win_activate(self.window);
            evas_object_focus_set(self.window, 1);
        }
        self.start_animator();
        self.restart_idle();
    }

    fn hide(&mut self) {
        self.ctl.hide();
        self.start_animator();
        if !self.idle.is_null() {
            unsafe { ecore_timer_del(self.idle) };
            self.idle = ptr::null_mut();
        }
    }

    fn key(&mut self, key: &str) {
        match self.ctl.key(key) {
            Action::None => {}
            Action::Moved => self.start_animator(),
            Action::Hide => return self.hide(),
            Action::Open(row) => {
                let focus = match row {
                    Row::Inputs => {
                        self.current_input = self.find_current_input();
                        self.current_input.unwrap_or(0)
                    }
                    Row::Apps => self.ctl.focus[Row::Apps.index()].target,
                    Row::Settings => 0,
                };
                self.ctl.open(row, focus);
                self.start_animator();
            }
            Action::Activate(row, index) => {
                self.pending = self.entries[row.index()]
                    .get(index)
                    .map(|entry| entry.target.clone());
                return self.hide();
            }
        }
        self.restart_idle();
    }
}

fn place_hint(hint: &mut Hint, centre: i32, middle: i32) {
    let left = centre - hint.width / 2;
    hint.text.move_to(left, middle - hint.text_h / 2);
    hint.chevron.place(Rect {
        x: left + hint.width - CHEVRON,
        y: middle - CHEVRON / 2,
        w: CHEVRON,
        h: CHEVRON,
    });
}

impl RowView {
    fn delete(self) {
        let mut objects = vec![
            &self.shadow,
            &self.bar,
            &self.glow,
            &self.ring,
            &self.dot,
            &self.label,
            &self.icon_clip,
            &self.clip,
        ];
        for icon in &self.icons {
            objects.push(&icon.image);
            if let Some((letters, _, _)) = &icon.initials {
                objects.push(letters);
            }
        }
        for hint in self.up.iter().chain(self.down.iter()) {
            objects.push(&hint.text);
            objects.push(&hint.chevron);
        }
        for object in objects {
            object.delete();
        }
    }
}

fn open(target: &Target) {
    let result = match target {
        Target::App(app_id) => tv_tizen::app::launch(app_id),
        // tv-viewer's own source-change request, as Samsung's Home sends it.
        Target::Source(source) => {
            let kind = source.kind.to_string();
            let mbr = source.mbr.to_string();
            tv_tizen::app::launch_with(
                TV_VIEWER,
                &[
                    ("change_source", &kind),
                    ("sourceType", &kind),
                    ("sourceName", &source.name),
                    ("sourceIcon", &source.source_icon),
                    ("mbrIndex", &mbr),
                    ("__K_EX_LAUNCH_REQUESTOR", "user"),
                ],
            )
        }
    };
    if let Err(error) = result {
        log_line(&format!("dock: {error}"));
    }
}

unsafe extern "C" fn on_frame(_data: *mut c_void) -> EinaBool {
    let dock = dock();
    let now = ecore_time_get();
    let gap = now - dock.last_frame;
    let more = dock.ctl.tick(gap);
    dock.last_frame = now;
    dock.apply();
    let (frames, worst_gap, worst_work) = dock.run;
    dock.run = (
        frames + 1,
        worst_gap.max(gap),
        worst_work.max(ecore_time_get() - now),
    );
    // The last frame: the fade has rendered, and the launch (a blocking
    // request) can no longer hold up a frame.
    if !more && dock.ctl.gone() {
        evas_object_hide(dock.window);
        if let Some(target) = dock.pending.take() {
            open(&target);
        }
    }
    if more {
        return 1;
    }
    dock.animator = ptr::null_mut(); // returning 0 deletes the animator
                                     // Only animations that dropped frames (60 Hz is 16.7 ms) are logged.
    let (frames, worst_gap, worst_work) = std::mem::take(&mut dock.run);
    if worst_gap > 0.025 {
        log_line(&format!(
            "dock: {frames} frames, longest gap {:.0} ms, longest work {:.1} ms",
            worst_gap * 1000.0,
            worst_work * 1000.0
        ));
    }
    0
}

unsafe extern "C" fn on_key(
    _data: *mut c_void,
    _evas: *mut c_void,
    _object: *mut c_void,
    info: *mut c_void,
) {
    // `Evas_Event_Key_Down` starts with `const char *keyname`.
    let name = *(info as *const *const c_char);
    if name.is_null() {
        return;
    }
    let key = CStr::from_ptr(name).to_string_lossy();
    dock().key(&key);
}

unsafe extern "C" fn on_update(_data: *mut c_void, _buffer: *mut c_void, _bytes: u32) {
    let dock = dock();
    let latest = dock
        .updates
        .as_ref()
        .and_then(|updates| updates.try_iter().last());
    if let Some(catalog) = latest {
        dock.build(&catalog);
    }
}

unsafe extern "C" fn on_idle(_data: *mut c_void) -> EinaBool {
    let dock = dock();
    dock.idle = ptr::null_mut(); // returning 0 deletes the timer
    dock.hide();
    0
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
    if dock.ctl.shown().is_some() {
        dock.hide();
    } else {
        dock.show();
    }
}

pub fn run(mut argv: Vec<*mut c_char>) -> i32 {
    let state = Box::into_raw(Box::new(Dock {
        window: ptr::null_mut(),
        evas: ptr::null_mut(),
        screen: (1920, 1080),
        win: Window::new(1920, 1080, [0; 3]),
        art: Art::new(),
        entries: Default::default(),
        rows: Vec::new(),
        ctl: Controller::default(),
        animator: ptr::null_mut(),
        last_frame: 0.0,
        idle: ptr::null_mut(),
        pending: None,
        current_input: None,
        updates: None,
        run: (0, 0.0, 0.0),
        icons: HashMap::new(),
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
