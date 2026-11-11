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
//! the app or input behind it stays visible. Its apps are Samsung Home's
//! catalog ([`catalog`]); a watcher thread rebuilds the tiles whenever
//! webappservice's database or Home's row changes.

use std::cell::Cell;
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use tv_auth_ui::catalog::{self, App};
use tv_efl::log::log_line;
use tv_efl::widgets::*;
use tv_efl::*;
use tv_proto::markup::escape;

pub const APP_ID: &str = "org.tvauth.home";
const WINDOW_NAME: &[u8] = b"org.tvauth.home\0";
const DELETE_REQUEST: &[u8] = b"delete,request\0";
const FOCUSED: &[u8] = b"focused\0";
const CLICKED: &[u8] = b"clicked\0";
const UNFOCUSED: &[u8] = b"unfocused\0";

const COLOR_DOCK: i32 = 0x10_12_17;
const COLOR_FOCUS: i32 = 0xff_ff_ff;
const DOCK_HEIGHT: i32 = 300;
const TEXT_TILE_WIDTH: i32 = 300;
const TILE_SIZE: i32 = 168;
const ICON_SIZE: i32 = 128;
const IDLE_SECONDS: f64 = 20.0;
/// Writes settle within this before the catalog is read again.
const SETTLE: Duration = Duration::from_millis(300);

const SAMSUNG_HOME: &str = "com.samsung.tv.csfs";
const MANAGER: &str = "org.tvauth.ui";

struct Tile {
    title: String,
    app_id: String,
    button: *mut c_void,
    /// Icon tiles: the focus frame behind the icon.
    frame: *mut c_void,
}

struct Dock {
    window: *mut c_void,
    name: *mut c_void,
    row: *mut c_void,
    tiles: Vec<Tile>,
    /// App id of the focused tile, kept across rebuilds.
    focused: String,
    shown: bool,
    idle: *mut c_void,
    updates: Option<Receiver<Vec<App>>>,
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

/// webappservice's catalog in Home's order.
fn load() -> Vec<App> {
    let rows = match tv_tizen::pkgdb::query_text(catalog::DATABASE, catalog::SQL, &[]) {
        Ok(rows) => rows,
        Err(error) => {
            log_line(&format!("dock: catalog: {error}"));
            Vec::new()
        }
    };
    let row = std::fs::read_to_string(catalog::ROW).unwrap_or_default();
    catalog::order(catalog::parse(&rows), &catalog::row_ids(&row))
}

/// Watcher thread: a fresh catalog after every settled change, then a wake-up
/// for the main loop through `pipe`.
fn watch(pipe: usize, updates: mpsc::Sender<Vec<App>>) {
    let watch = match tv_tizen::watch::Watch::new(&["/opt/dbspace", "/run/tv-suite"]) {
        Ok(watch) => watch,
        Err(error) => return log_line(&format!("dock: {error}; the app list will not refresh")),
    };
    let wanted = |name: &str| name.starts_with(".webappservice.db") || name == "home-row";
    loop {
        if let Err(error) = watch.wait(wanted, SETTLE) {
            return log_line(&format!("dock: watch: {error}"));
        }
        if updates.send(load()).is_err() {
            return;
        }
        unsafe { ecore_pipe_write(pipe as *mut c_void, b"r".as_ptr().cast(), 1) };
    }
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
            let (bar, inner) = panel(window, COLOR_DOCK, 72, 24);
            elm_win_resize_object_add(window, bar);
            self.name = add_to_box(inner, create_label(inner, ""));

            let scroller = elm_scroller_add(inner);
            elm_scroller_policy_set(scroller, ELM_SCROLLER_POLICY_OFF, ELM_SCROLLER_POLICY_OFF);
            elm_scroller_bounce_set(scroller, 0, 0);
            elm_scroller_content_min_limit(scroller, 0, 1);
            evas_object_size_hint_weight_set(scroller, EVAS_HINT_EXPAND, EVAS_HINT_EXPAND);
            evas_object_size_hint_align_set(scroller, EVAS_HINT_FILL, EVAS_HINT_FILL);
            evas_object_show(scroller);
            add_to_box(inner, scroller);

            self.row = elm_box_add(scroller);
            elm_box_horizontal_set(self.row, 1);
            elm_box_padding_set(self.row, 24, 0);
            evas_object_size_hint_align_set(self.row, 0.0, 0.5);
            evas_object_show(self.row);
            elm_object_part_content_set(scroller, ptr::null(), self.row);
        }
        self.build(load());

        let (sender, receiver) = mpsc::channel();
        self.updates = Some(receiver);
        let pipe = unsafe { ecore_pipe_add(on_update, ptr::null()) } as usize;
        std::thread::spawn(move || watch(pipe, sender));
        true
    }

    /// (Re)creates every tile: Samsung Home, the catalog, TV suite.
    fn build(&mut self, apps: Vec<App>) {
        // Tiles first: deleting the focused button moves focus through the
        // old ones, and their callbacks must not find them (or overwrite
        // `focused`).
        self.tiles.clear();
        unsafe { elm_box_clear(self.row) };
        let mut tiles = vec![(String::from("Samsung Home"), SAMSUNG_HOME.to_owned(), None)];
        tiles.extend(
            apps.into_iter()
                .map(|app| (app.title, app.app_id, app.icon)),
        );
        tiles.push(("TV suite".into(), MANAGER.into(), None));
        for (index, (title, app_id, icon)) in tiles.into_iter().enumerate() {
            let data = ptr::without_provenance_mut(index + 1);
            let (button, frame) = match &icon {
                None => {
                    let button = create_button(self.row, &title, on_tile_clicked, data);
                    unsafe { evas_object_size_hint_min_set(button, TEXT_TILE_WIDTH, TILE_SIZE) };
                    (button, ptr::null_mut())
                }
                // The TV's button theme has no icon part and draws its focus
                // above any content, so the button is invisible (it still
                // takes focus and OK) and a frame of ours marks focus.
                Some(path) => unsafe {
                    let cell = elm_table_add(self.row);
                    let frame = create_rect(cell, COLOR_FOCUS, 255);
                    evas_object_size_hint_min_set(frame, TILE_SIZE, TILE_SIZE);
                    evas_object_hide(frame);
                    elm_table_pack(cell, frame, 0, 0, 1, 1);
                    // Not create_button: that packs into a box.
                    let button = elm_button_add(cell);
                    evas_object_smart_callback_add(
                        button,
                        CLICKED.as_ptr().cast(),
                        on_tile_clicked,
                        data,
                    );
                    evas_object_color_set(button, 0, 0, 0, 0);
                    // Its theme minimum is wider than a tile.
                    elm_object_scale_set(button, 0.5);
                    evas_object_show(button);
                    elm_table_pack(cell, button, 0, 0, 1, 1);
                    let image = elm_image_add(cell);
                    evas_object_size_hint_min_set(image, ICON_SIZE, ICON_SIZE);
                    let file = c_string(&path.to_string_lossy());
                    elm_image_file_set(image, file.as_ptr(), ptr::null());
                    evas_object_show(image);
                    elm_table_pack(cell, image, 0, 0, 1, 1);
                    evas_object_show(cell);
                    elm_box_pack_end(self.row, cell);
                    (button, frame)
                },
            };
            unsafe {
                evas_object_smart_callback_add(button, FOCUSED.as_ptr().cast(), on_focus, data);
                evas_object_smart_callback_add(button, UNFOCUSED.as_ptr().cast(), on_unfocus, data);
            }
            self.tiles.push(Tile {
                title,
                app_id,
                button,
                frame,
            });
        }
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        log_line(&format!("dock: {} tiles at {millis} ms", self.tiles.len()));
        if self.shown {
            self.focus();
        }
    }

    /// Focus the tile focused before (by app id), else the first.
    fn focus(&self) {
        let tile = self
            .tiles
            .iter()
            .find(|tile| tile.app_id == self.focused)
            .or(self.tiles.first());
        if let Some(tile) = tile {
            unsafe { elm_object_focus_set(tile.button, 1) };
        }
    }

    fn show(&mut self) {
        self.shown = true;
        self.focused.clear();
        unsafe {
            evas_object_show(self.window);
            elm_win_raise(self.window);
            elm_win_activate(self.window);
        }
        self.focus();
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

fn tile(data: *mut c_void) -> Option<&'static Tile> {
    dock().tiles.get((data as usize).wrapping_sub(1))
}

unsafe extern "C" fn on_update(_data: *mut c_void, _buffer: *mut c_void, _bytes: u32) {
    let dock = dock();
    let latest = dock
        .updates
        .as_ref()
        .and_then(|updates| updates.try_iter().last());
    if let Some(apps) = latest {
        dock.build(apps);
    }
}

unsafe extern "C" fn on_idle(_data: *mut c_void) -> EinaBool {
    let dock = dock();
    dock.idle = ptr::null_mut(); // returning 0 deletes the timer
    dock.hide();
    0
}

unsafe extern "C" fn on_focus(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    let Some(tile) = tile(data) else { return };
    if !tile.frame.is_null() {
        evas_object_show(tile.frame);
    }
    let markup = format!("<font_size=30><b>{}</b></font_size>", escape(&tile.title));
    let app_id = tile.app_id.clone();
    let dock = dock();
    dock.focused = app_id;
    set_text(dock.name, &markup);
    dock.restart_idle();
}

unsafe extern "C" fn on_unfocus(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    if let Some(tile) = tile(data).filter(|tile| !tile.frame.is_null()) {
        evas_object_hide(tile.frame);
    }
}

unsafe extern "C" fn on_back(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    dock().hide();
}

unsafe extern "C" fn on_tile_clicked(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    let Some(tile) = tile(data) else { return };
    let (title, app_id) = (tile.title.clone(), tile.app_id.clone());
    dock().hide();
    if let Err(error) = tv_tizen::app::launch(&app_id) {
        log_line(&format!("dock: open {title} ({app_id}): {error}"));
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
        name: ptr::null_mut(),
        row: ptr::null_mut(),
        tiles: Vec::new(),
        focused: String::new(),
        shown: false,
        idle: ptr::null_mut(),
        updates: None,
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
