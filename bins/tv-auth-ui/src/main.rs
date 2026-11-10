//! `org.tvauth.ui`: the user-launched TV suite manager.
//!
//! Two tabs:
//! - **Store**: the signed Apps2Samsung catalog (see `store.rs`); installs and
//!   removals go through `tv-authd`'s `pkg.*` like any package's, so every
//!   change is shown as a prompt by `tv-auth-popupd` first;
//! - **Permissions**: persistent root grants (toast mode, revoke) and the
//!   daemon's recent decisions (`grants.audit`).
//!
//! It never sends `ui.hello`, so it does not compete with the popup daemon.
//! It is an ordinary app window (`E_LAYER_CLIENT_NORMAL`), so opening it
//! obscures and pauses the previous app, Samsung's Home bar
//! (`NOTIFICATION_LOW`) and the popup's prompt (`NOTIFICATION_TOP`) open above
//! it, and BACK arrives through focus without a keygrab. Network, hashing and broker calls run on worker threads
//! that report through a channel drained by a 100 ms timer.
//!
//! The daemon stages the executable at `/run/tv-auth/ui-bin/tv-auth-ui` (see
//! `tv-authd`'s `ui_alias.rs`) so launchpad can map it despite UEP.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::raw::{c_char, c_void};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};
use tv_auth_ui::store::{self, App, Group, State};
use tv_efl::log::log_line;
use tv_efl::widgets::*;
use tv_efl::*;
use tv_proto::client::{BrokerClient, Event, Notifier};
use tv_proto::markup::*;
use tv_proto::ui::GrantInfo;
use tv_proto::DEFAULT_SOCKET;

const COLOR_BACKGROUND: i32 = 0x10_12_17;
const COLOR_ROW: i32 = 0x1c_1f_26;
const COLOR_BUILD_ROW: i32 = 0x16_19_1f;
/// Rows are appended in chunks as focus approaches the end of the list.
const CHUNK_ROWS: usize = 16;
const LOOKAHEAD_ROWS: usize = 6;
const ICON_SIZE: i32 = 96;
const DETAIL_ICON_SIZE: i32 = 200;
/// Inner width available to wrapped text on the details page.
const DETAIL_TEXT_WIDTH: i32 = 1500;
const DESCRIPTION_CHARS: usize = 110;
const AUDIT_SHOWN: usize = 8;

/// Downloads and icons live in the app's corner of `/tmp`, which `tv-authd`'s
/// staging accepts as a package source.
const STORE_DIR: &str = "/tmp/tv-store";
const BLOCKD_SOCKET: &str = "/run/tv-suite/blockd.sock";
const CATEGORIES: &[(&str, &str)] = &[
    ("", "All"),
    ("media", "Media"),
    ("streaming", "Streaming"),
    ("iptv", "IPTV"),
    ("games", "Games"),
    ("casting", "Casting"),
    ("tools", "Tools"),
    ("other", "Other"),
];

const APP_ID: &[u8] = b"org.tvauth.ui\0";
const DELETE_REQUEST: &[u8] = b"delete,request\0";
const FOCUSED: &[u8] = b"focused\0";
const CHANGED_USER: &[u8] = b"changed,user\0";
const GUIDE_PART: &[u8] = b"elm.guide\0";

thread_local! {
    static UI_PTR: Cell<*mut Ui> = const { Cell::new(ptr::null_mut()) };
}

fn ui() -> &'static mut Ui {
    let pointer = UI_PTR.with(Cell::get);
    assert!(!pointer.is_null(), "UI state used before app_create");
    // SAFETY: the pointer is installed on the EFL main thread in `main` and is
    // valid until `ui_app_main` returns.
    unsafe { &mut *pointer }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Store,
    Permissions,
    Tweaks,
}

/// Results from worker threads.
enum Job {
    Catalog(Result<Vec<App>, String>),
    Installed(Result<HashMap<String, Installed>, String>),
    /// An icon with this SHA-256 is now cached.
    Icon(String),
    Status(String),
    /// Download fraction, or `None` while waiting on the prompt/installer.
    Progress(Option<f64>),
    Finished(Result<String, String>),
    Audit(Vec<String>),
    /// tv-blockd's status, with the message for a change it answers.
    Tweaks(Option<&'static str>, Result<Value, String>),
}

#[derive(Clone)]
struct Installed {
    version: String,
    main_app_id: String,
}

#[derive(Default)]
struct Store {
    groups: Vec<Group>,
    installed: HashMap<String, Installed>,
    loaded: bool,
    /// The list has been arranged once with the installed packages known.
    arranged: bool,
    category: usize,
    query: String,
    /// The app whose details page is open.
    detail: Option<usize>,
    /// Row key that last had focus, kept across in-place updates.
    focus_key: Option<usize>,
    /// The focused button was a row's action (Install/Update/Open/Remove),
    /// not its Details button.
    focus_on_action: bool,
    /// The list row to return to when the details page closes.
    list_key: Option<usize>,
    busy: bool,
    /// Download URL of the build being installed or removed: its rows show
    /// a progress bar instead of buttons.
    changing: Option<String>,
    /// Latest download fraction (`None`: pulsing).
    progress_value: Option<f64>,
    icons_requested: HashSet<String>,
}

impl Store {
    fn versions(&self) -> HashMap<String, String> {
        self.installed
            .iter()
            .map(|(pkgid, installed)| (pkgid.clone(), installed.version.clone()))
            .collect()
    }
}

/// A list row is keyed by its group and build: `(group << 8 | slot) + 1`,
/// where slot 0 is the group row and `n + 1` its n-th build. The key is also
/// the button callback data (never 0).
fn row_key(group: usize, build: Option<usize>) -> usize {
    ((group << 8) | build.map_or(0, |build| build + 1)) + 1
}

fn split_key(key: usize) -> (usize, Option<usize>) {
    let raw = key - 1;
    (raw >> 8, (raw & 0xff).checked_sub(1))
}

struct Row {
    key: usize,
    widget: *mut c_void,
    primary: *mut c_void,
    /// The row's first action button (Install/Update/Open/Remove), if any.
    action: *mut c_void,
    icon: Option<(String, *mut c_void)>,
}

/// The store list, set aside while a details page is shown.
struct ParkedList {
    objects: Vec<*mut c_void>,
    entries: Vec<usize>,
    rows: Vec<Row>,
    list_box: *mut c_void,
    count_label: *mut c_void,
}

struct Ui {
    window: *mut c_void,
    status_label: *mut c_void,
    content_box: *mut c_void,
    content_objects: Vec<*mut c_void>,

    broker: Option<BrokerClient>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    jobs_tx: Sender<Job>,
    jobs_rx: Receiver<Job>,

    tab: Tab,
    status: String,
    store: Store,
    /// Keys of every listed row, in order; `rows` holds the rendered prefix.
    entries: Vec<usize>,
    rows: Vec<Row>,
    list_box: *mut c_void,
    count_label: *mut c_void,
    parked_list: Option<ParkedList>,
    /// The on-screen progress bar and its label in the changing build's row,
    /// if rendered.
    progress: *mut c_void,
    progress_label: *mut c_void,
    grants: Vec<GrantInfo>,
    audit: Vec<String>,
    /// tv-blockd's last `status` response.
    tweaks: Option<Value>,
    /// Toggle keys (`hosts`, `group:<id>`, `otn_lock`, `telemetry`) and
    /// their rendered buttons, in screen order.
    tweak_keys: Vec<String>,
    tweak_buttons: Vec<*mut c_void>,
    tweak_summary: *mut c_void,
    tweak_focus: usize,
    exiting: bool,
    connected: bool,
    app_control_count: u32,
    focus_notify_label: Option<String>,
}

impl Ui {
    fn new(tx: Sender<Event>, rx: Receiver<Event>) -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel();
        Self {
            window: ptr::null_mut(),
            status_label: ptr::null_mut(),
            content_box: ptr::null_mut(),
            content_objects: Vec::new(),
            broker: None,
            tx,
            rx,
            jobs_tx,
            jobs_rx,
            tab: Tab::Store,
            status: String::new(),
            store: Store::default(),
            entries: Vec::new(),
            rows: Vec::new(),
            list_box: ptr::null_mut(),
            count_label: ptr::null_mut(),
            parked_list: None,
            progress: ptr::null_mut(),
            progress_label: ptr::null_mut(),
            grants: Vec::new(),
            audit: Vec::new(),
            tweaks: None,
            tweak_keys: Vec::new(),
            tweak_buttons: Vec::new(),
            tweak_summary: ptr::null_mut(),
            tweak_focus: 0,
            exiting: false,
            connected: false,
            app_control_count: 0,
            focus_notify_label: None,
        }
    }

    fn create_window(&mut self) -> bool {
        unsafe {
            let window = elm_win_add(ptr::null_mut(), APP_ID.as_ptr().cast(), ELM_WIN_BASIC);
            if window.is_null() {
                log_line("elm_win_add failed");
                return false;
            }
            self.window = window;
            // ELM_WIN_BASIC leaves the Wayland type at NONE on this firmware,
            // and such a surface never gets an xdg toplevel (never mapped).
            let wayland_window = elm_win_wl_window_get(window);
            if !wayland_window.is_null() {
                ecore_wl2_window_type_set(wayland_window, ECORE_WL2_WINDOW_TYPE_TOPLEVEL);
            }
            set_window_title(window, "TV suite");
            evas_object_smart_callback_add(
                window,
                DELETE_REQUEST.as_ptr().cast(),
                on_back,
                ptr::null_mut(),
            );
            eext_object_event_callback_add(window, EEXT_CALLBACK_BACK, on_back, ptr::null_mut());

            // The stock elm_bg is transparent in the TV theme.
            let background = create_rect(window, COLOR_BACKGROUND, 255);
            evas_object_size_hint_weight_set(background, EVAS_HINT_EXPAND, EVAS_HINT_EXPAND);
            elm_win_resize_object_add(window, background);

            // panel() pads its inner box; a resize object's own padding hint
            // is ignored.
            let (page, main) = panel(window, COLOR_BACKGROUND, 72, 40);
            evas_object_size_hint_weight_set(page, 1.0, 1.0);
            evas_object_size_hint_align_set(page, EVAS_HINT_FILL, EVAS_HINT_FILL);
            evas_object_size_hint_align_set(main, EVAS_HINT_FILL, EVAS_HINT_FILL);
            elm_box_padding_set(main, 0, 20);
            elm_win_resize_object_add(window, page);

            let header = create_row(main);
            elm_box_pack_end(main, header);
            add_to_box(
                header,
                create_label(header, "<font_size=44><b>TV suite</b></font_size>"),
            );
            create_button(
                header,
                "Store",
                on_tab_clicked,
                ptr::without_provenance_mut(1),
            );
            create_button(
                header,
                "Permissions",
                on_tab_clicked,
                ptr::without_provenance_mut(2),
            );
            create_button(
                header,
                "Tweaks",
                on_tab_clicked,
                ptr::without_provenance_mut(3),
            );

            self.status_label = add_to_box(main, create_label(main, ""));

            self.content_box = elm_box_add(main);
            evas_object_size_hint_weight_set(self.content_box, 1.0, 1.0);
            evas_object_size_hint_align_set(self.content_box, EVAS_HINT_FILL, EVAS_HINT_FILL);
            elm_box_padding_set(self.content_box, 0, 14);
            elm_box_pack_end(main, self.content_box);
            evas_object_show(self.content_box);

            let (mut width, mut height) = (0, 0);
            elm_win_screen_size_get(
                window,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut width,
                &mut height,
            );
            if width > 0 && height > 0 {
                evas_object_resize(window, width, height);
            } else {
                evas_object_resize(window, 1920, 1080);
            }

            ecore_timer_add(0.1, on_timer, ptr::null_mut());
            let events = Notifier::new(self.tx.clone(), Arc::new(|| {}), log_line);
            self.broker = Some(BrokerClient::start(events, false));
        }
        self.load_catalog();
        self.render();
        true
    }

    fn show(&mut self) {
        unsafe {
            evas_object_show(self.window);
            elm_win_raise(self.window);
            elm_win_activate(self.window);
        }
    }

    /// Hold BACK only while no store change waits on the popup's prompt,
    /// which needs the key (and focus) for itself.
    fn terminate(&mut self) {
        if let Some(broker) = self.broker.take() {
            broker.stop();
        }
        if !self.window.is_null() {
            unsafe { evas_object_del(self.window) };
            self.window = ptr::null_mut();
        }
    }

    fn on_timer(&mut self) -> EinaBool {
        let mut dirty = false;
        while let Ok(event) = self.rx.try_recv() {
            dirty |= self.on_broker_event(event);
        }
        while let Ok(job) = self.jobs_rx.try_recv() {
            dirty |= self.on_job(job);
        }
        if dirty {
            self.render();
        }
        1
    }

    /// Returns whether the screen needs a re-render.
    fn on_broker_event(&mut self, event: Event) -> bool {
        match event {
            Event::Grants(grants) => {
                self.grants = grants;
                self.tab == Tab::Permissions
            }
            Event::RevokeFinished(revoked, error) => {
                self.status = if revoked {
                    "Grant revoked.".into()
                } else if error == "not_found" {
                    "That grant was already removed.".into()
                } else {
                    format!("Revoke failed: {}", or_unknown(&error))
                };
                self.refresh_grants();
                true
            }
            Event::NotifyFinished(ok, error) => {
                if !ok {
                    self.status = format!("Could not change toast setting: {}", or_unknown(&error));
                }
                self.refresh_grants();
                !ok
            }
            Event::ConnectionChanged(connected) => {
                self.connected = connected;
                if connected {
                    self.refresh_grants();
                }
                self.tab == Tab::Permissions
            }
            Event::StatusMessage(message) => {
                self.status = message;
                true
            }
            Event::Prompt(_)
            | Event::Notice(_)
            | Event::HelloAccepted
            | Event::DecisionFinished(..) => false,
        }
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = text.into();
        set_text(
            self.status_label,
            &format!(
                "<font_size=24><color=#c9ccd3>{}</color></font_size>",
                escape(&self.status)
            ),
        );
    }

    fn on_job(&mut self, job: Job) -> bool {
        match job {
            Job::Catalog(Ok(apps)) => {
                self.store.groups = store::groups(apps);
                self.status = format!(
                    "{} apps in the Apps2Samsung catalog.",
                    self.store.groups.len()
                );
                self.store.loaded = true;
                self.store.arranged = false;
                self.store.focus_key = None;
                // Listed once the installed packages are known, so they
                // can sort first without the list jumping.
                self.list_installed();
                return false;
            }
            Job::Catalog(Err(error)) => {
                log_line(&format!("catalog: {error}"));
                self.status = format!("Could not load the catalog: {error}");
            }
            Job::Installed(result) => {
                match result {
                    Ok(installed) => self.store.installed = installed,
                    Err(error) => {
                        self.status = format!("Could not read installed packages: {error}");
                    }
                }
                if !self.store.loaded || self.tab != Tab::Store {
                    return false;
                }
                if !self.store.arranged {
                    self.store.arranged = true;
                    return true;
                }
                // Keep order, scroll position and focus: rebuild rows in place.
                self.refresh_rows();
                return false;
            }
            Job::Icon(digest) => {
                self.show_icon(&digest);
                return false;
            }
            Job::Status(message) => {
                self.set_status(message);
                return false;
            }
            Job::Progress(fraction) => {
                self.store.progress_value = fraction;
                self.update_progress();
                return false;
            }
            Job::Finished(result) => {
                self.store.busy = false;
                self.store.changing = None;
                self.refresh_rows();
                self.show();
                self.set_status(result.unwrap_or_else(|error| error));
                log_line(&self.status);
                // The row's buttons change once the installed list is back.
                self.list_installed();
                return false;
            }
            Job::Audit(lines) => {
                self.audit = lines;
                return self.tab == Tab::Permissions;
            }
            Job::Tweaks(message, Ok(status)) => {
                let first = self.tweaks.is_none();
                self.tweaks = Some(status);
                if let Some(message) = message {
                    self.set_status(message);
                }
                if self.tab != Tab::Tweaks {
                    return false;
                }
                if first || self.tweak_buttons.is_empty() {
                    return true;
                }
                // In place, so focus and scroll position stay.
                self.update_tweaks();
                return false;
            }
            Job::Tweaks(_, Err(error)) => {
                log_line(&error);
                self.set_status(error);
                return false;
            }
        }
        true
    }

    fn refresh_grants(&mut self) {
        if let Some(broker) = &self.broker {
            broker.request_grants();
        }
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            let lines = broker_call(json!({"version": 2, "id": "audit", "method": "grants.audit"}))
                .ok()
                .and_then(|response| serde_json::from_value(response["audit"].clone()).ok())
                .unwrap_or_default();
            let _ = jobs.send(Job::Audit(lines));
        });
    }

    fn load_catalog(&mut self) {
        self.status = "Loading the Apps2Samsung catalog...".into();
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            let _ = jobs.send(Job::Catalog(fetch_catalog()));
        });
    }

    fn list_installed(&mut self) {
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            let _ = jobs.send(Job::Installed(installed_packages()));
        });
    }

    fn fetch_icons(&mut self, icons: Vec<(String, String)>) {
        let mut wanted = Vec::new();
        for (url, digest) in icons {
            if self.store.icons_requested.insert(digest.clone()) && !icon_path(&digest).exists() {
                wanted.push((url, digest));
            }
        }
        if wanted.is_empty() {
            return;
        }
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            for (url, digest) in wanted {
                match fetch_verified(&url, &digest, 1 << 20, &icon_path(&digest), None) {
                    Ok(_) => {
                        let _ = jobs.send(Job::Icon(digest));
                    }
                    Err(error) => log_line(&format!("icon {url}: {error}")),
                }
            }
        });
    }

    fn start_change(&mut self) -> bool {
        if self.store.busy {
            self.set_status("Another change is still in progress.");
            return false;
        }
        self.store.busy = true;
        true
    }

    /// Mark `app` as changing and swap its rows' buttons for a progress bar.
    fn begin_change(&mut self, app: &App, fraction: Option<f64>) {
        self.store.changing = Some(app.url.clone());
        self.store.progress_value = fraction;
        self.refresh_rows();
    }

    /// A filled bar with a short label; the TV theme draws nothing for a
    /// pulsing (indeterminate) bar, so the waiting phase is a full bar and
    /// words instead.
    fn update_progress(&mut self) {
        if self.progress.is_null() {
            return;
        }
        let (fraction, text) = match self.store.progress_value {
            Some(fraction) => (
                fraction.clamp(0.0, 1.0),
                format!("Downloading {:.0}%", fraction.clamp(0.0, 1.0) * 100.0),
            ),
            None => (1.0, "Waiting for approval...".to_owned()),
        };
        unsafe { elm_progressbar_value_set(self.progress, fraction) };
        set_text(self.progress_label, &small(&text, "#c9ccd3"));
    }

    fn install(&mut self, app: App) {
        if !self.start_change() {
            return;
        }
        self.begin_change(&app, Some(0.0));
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            let _ = jobs.send(Job::Finished(install_app(&app, &jobs)));
        });
    }

    fn uninstall(&mut self, app: App) {
        if !self.start_change() {
            return;
        }
        self.set_status(format!("Removing {}: approve it on the prompt.", app.name));
        self.begin_change(&app, None);
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            let request = json!({"version": 2, "id": "store-uninstall", "method": "pkg.uninstall",
                                 "pkgid": app.package_id, "timeout_ms": 120_000});
            let result = broker_call(request)
                .and_then(|response| ok_or_error(&response))
                .map(|()| format!("Removed {}.", app.name));
            let _ = jobs.send(Job::Finished(result));
        });
    }

    fn open(&mut self, app: &App) {
        let Some(installed) = self.store.installed.get(&app.package_id) else {
            return;
        };
        let status = match tv_tizen::app::launch(&installed.main_app_id) {
            Ok(()) => format!("Opening {}...", app.name),
            Err(error) => format!("Could not open {}: {error}", app.name),
        };
        self.set_status(status);
    }

    fn render(&mut self) {
        if self.content_box.is_null() {
            return;
        }
        if let Some(parked) = self.parked_list.take() {
            for object in parked.objects {
                unsafe { evas_object_del(object) };
            }
        }
        self.clear_content();
        self.progress = ptr::null_mut();
        self.progress_label = ptr::null_mut();
        self.tweak_buttons.clear();
        self.tweak_summary = ptr::null_mut();
        let focus = match (self.tab, self.store.detail) {
            (Tab::Store, Some(group)) => self.render_details(group),
            (Tab::Store, None) => self.render_store(),
            (Tab::Permissions, _) => self.render_permissions(),
            (Tab::Tweaks, _) => self.render_tweaks(),
        };
        self.set_status(self.status.clone());
        if !focus.is_null() {
            unsafe { elm_object_focus_set(focus, 1) };
        }
    }

    /// Returns the widget to focus.
    fn render_store(&mut self) -> *mut c_void {
        let toolbar = create_row(self.content_box);
        self.add_content(toolbar);
        self.count_label = add_to_box(toolbar, create_label(toolbar, ""));
        let search = unsafe {
            // A scrollable entry resets its own minimum size whenever its
            // text changes (and the toolbar then squeezes it to nothing), so
            // size the cell with a fixed transparent rectangle instead.
            let cell = elm_table_add(toolbar);
            let sizer = create_rect(cell, 0, 0);
            evas_object_size_hint_min_set(sizer, 460, 72);
            elm_table_pack(cell, sizer, 0, 0, 1, 1);
            let entry = elm_entry_add(cell);
            elm_entry_single_line_set(entry, 1);
            elm_entry_scrollable_set(entry, 1);
            let guide = c_string("Search apps");
            elm_object_part_text_set(entry, GUIDE_PART.as_ptr().cast(), guide.as_ptr());
            set_text(entry, &escape(&self.store.query));
            evas_object_size_hint_weight_set(entry, 1.0, 1.0);
            evas_object_size_hint_align_set(entry, EVAS_HINT_FILL, EVAS_HINT_FILL);
            evas_object_smart_callback_add(
                entry,
                CHANGED_USER.as_ptr().cast(),
                on_search_changed,
                ptr::null_mut(),
            );
            elm_table_pack(cell, entry, 0, 0, 1, 1);
            evas_object_show(entry);
            evas_object_show(cell);
            elm_box_pack_end(toolbar, cell);
            entry
        };
        unsafe {
            let hoversel = elm_hoversel_add(toolbar);
            elm_hoversel_hover_parent_set(hoversel, self.window);
            set_text(
                hoversel,
                &format!("Category: {}", CATEGORIES[self.store.category].1),
            );
            for (index, (_, label)) in CATEGORIES.iter().enumerate() {
                let label = c_string(label);
                elm_hoversel_item_add(
                    hoversel,
                    label.as_ptr(),
                    ptr::null(),
                    ELM_ICON_NONE,
                    on_category_selected,
                    ptr::without_provenance(index + 1),
                );
            }
            evas_object_size_hint_min_set(hoversel, 340, 72);
            elm_box_pack_end(toolbar, hoversel);
            evas_object_show(hoversel);
        }
        create_button(toolbar, "Reload", on_reload_clicked, ptr::null_mut());

        unsafe {
            let scroller = elm_scroller_add(self.content_box);
            elm_scroller_policy_set(scroller, ELM_SCROLLER_POLICY_OFF, ELM_SCROLLER_POLICY_OFF);
            elm_scroller_bounce_set(scroller, 0, 0);
            evas_object_size_hint_weight_set(scroller, 1.0, 1.0);
            evas_object_size_hint_align_set(scroller, EVAS_HINT_FILL, EVAS_HINT_FILL);
            self.list_box = elm_box_add(scroller);
            evas_object_size_hint_weight_set(self.list_box, 1.0, 0.0);
            evas_object_size_hint_align_set(self.list_box, EVAS_HINT_FILL, 0.0);
            elm_box_padding_set(self.list_box, 0, 14);
            elm_object_part_content_set(scroller, ptr::null(), self.list_box);
            evas_object_show(self.list_box);
            evas_object_show(scroller);
            self.add_content(scroller);
        }
        self.rows.clear();
        self.rebuild_list();
        let focus = self
            .store
            .focus_key
            .and_then(|key| self.rows.iter().find(|row| row.key == key))
            .or_else(|| self.rows.first())
            .map(|row| row.primary);
        focus.unwrap_or(search)
    }

    /// (Re)list the arranged groups for the current category and search,
    /// keeping the toolbar (and its focus) intact.
    fn rebuild_list(&mut self) {
        for row in self.rows.drain(..) {
            unsafe { evas_object_del(row.widget) };
        }
        if self.list_box.is_null() {
            return;
        }
        let order = store::arrange(
            &self.store.groups,
            &self.store.versions(),
            CATEGORIES[self.store.category].0,
            &self.store.query,
        );
        self.entries.clear();
        for &group in &order {
            self.entries.push(row_key(group, None));
        }
        set_text(
            self.count_label,
            &small(&format!("{} apps", order.len()), "#8a8f99"),
        );
        let focus = self
            .store
            .focus_key
            .and_then(|key| self.entries.iter().position(|&entry| entry == key))
            .unwrap_or(0);
        self.append_rows(focus + CHUNK_ROWS);
        if self.entries.is_empty() && self.store.loaded {
            let label = create_label(self.list_box, &small("No apps match.", "#8a8f99"));
            add_to_box(self.list_box, label);
            self.rows.push(Row {
                key: 0,
                widget: label,
                primary: ptr::null_mut(),
                action: ptr::null_mut(),
                icon: None,
            });
        }
    }

    /// Render entries up to `upto` (exclusive) at the end of the list.
    fn append_rows(&mut self, upto: usize) {
        let end = upto.min(self.entries.len());
        let mut icons = Vec::new();
        while self.rows.len() < end {
            let key = self.entries[self.rows.len()];
            let row = self.build_row(key, ptr::null_mut());
            icons.extend(self.row_icon(key));
            self.rows.push(row);
        }
        self.fetch_icons(icons);
    }

    fn row_icon(&self, key: usize) -> Option<(String, String)> {
        if self.store.detail.is_some() {
            return None;
        }
        let (group, build) = split_key(key);
        match build {
            None => self.store.groups[group].icon.clone(),
            Some(_) => None,
        }
    }

    /// The single app behind a row: a build row, or a group with one build.
    fn row_app(&self, key: usize) -> Option<&App> {
        let (group, build) = split_key(key);
        let variants = &self.store.groups.get(group)?.variants;
        match build {
            Some(build) => variants.get(build),
            None if variants.len() == 1 => variants.first(),
            None => None,
        }
    }

    /// Build one row and pack it after `after` (or at the end when null).
    fn build_row(&mut self, key: usize, after: *mut c_void) -> Row {
        let (group_index, build) = split_key(key);
        let group = self.store.groups[group_index].clone();
        let versions = self.store.versions();
        let (row, row_box) = panel(
            self.list_box,
            if build.is_some() {
                COLOR_BUILD_ROW
            } else {
                COLOR_ROW
            },
            24,
            if build.is_some() { 8 } else { 12 },
        );
        unsafe {
            if after.is_null() {
                elm_box_pack_end(self.list_box, row);
            } else {
                elm_box_pack_after(self.list_box, row, after);
            }
            elm_box_horizontal_set(row_box, 1);
            elm_box_padding_set(row_box, 24, 0);
        }
        let mut icon = None;
        if build.is_none() {
            let image = unsafe {
                let image = elm_image_add(row_box);
                evas_object_size_hint_min_set(image, ICON_SIZE, ICON_SIZE);
                evas_object_show(image);
                image
            };
            if let Some((_, digest)) = &group.icon {
                set_image_file(image, &icon_path(digest));
                icon = Some((digest.clone(), image));
            }
            add_to_box(row_box, image);
        } else {
            // Indent builds under their app.
            let spacer = create_rect(row_box, COLOR_BUILD_ROW, 255);
            unsafe { evas_object_size_hint_min_set(spacer, ICON_SIZE, 1) };
            add_to_box(row_box, spacer);
        }

        let data = ptr::without_provenance_mut(key);
        let mut buttons = Vec::new();
        let in_list = build.is_none();
        // Rows of the build being changed show progress instead of actions.
        let changing = self
            .store
            .changing
            .as_ref()
            .is_some_and(|url| match self.row_app(key) {
                Some(app) => &app.url == url,
                None => group.variants.iter().any(|app| &app.url == url),
            });
        match self.row_app(key).cloned() {
            Some(app) => {
                let state = store::state(&app, &versions);
                let mut facts = vec![
                    state_text(&app, &state),
                    app.pkg_type.to_uppercase(),
                    format_size(app.size),
                ];
                if app.partner {
                    facts.push("partner".into());
                }
                let markup = if build.is_some() {
                    let title = if app.variant.is_empty() {
                        &app.name
                    } else {
                        &app.variant
                    };
                    format!(
                        "<font_size=26><b>{}</b></font_size><br/>{}",
                        escape(&shorten(title, 70, false)),
                        small(&facts.join(" \u{00b7} "), "#8a8f99")
                    )
                } else {
                    format!(
                        "<font_size=28><b>{}</b></font_size>  {}<br/>{}",
                        escape(&shorten(&group.name, 48, false)),
                        small(&facts.join(" \u{00b7} "), "#8a8f99"),
                        small(
                            &shorten(&group.description, DESCRIPTION_CHARS, false),
                            "#b4b9c2"
                        )
                    )
                };
                add_to_box(row_box, create_label(row_box, &markup));
                if in_list {
                    buttons.push(create_button(row_box, "Details", on_details_clicked, data));
                }
                // In the list, an app that is not installed only offers
                // Details; installing happens on its page.
                if !changing && !(in_list && state == State::NotInstalled) {
                    buttons.push(match state {
                        State::NotInstalled => {
                            create_button(row_box, "Install", on_install_clicked, data)
                        }
                        State::Upgradable { .. } => {
                            create_button(row_box, "Update", on_install_clicked, data)
                        }
                        State::Installed | State::Newer { .. } => {
                            create_button(row_box, "Open", on_open_clicked, data)
                        }
                    });
                    if !in_list && !matches!(state, State::NotInstalled) {
                        buttons.push(create_button(row_box, "Remove", on_remove_clicked, data));
                    }
                }
            }
            None => {
                // An app with several builds: a summary; builds are on its
                // details page.
                let installed = group
                    .variants
                    .iter()
                    .find_map(|app| versions.get(&app.package_id));
                let mut facts = vec![format!("{} builds", group.variants.len())];
                if let Some(version) = installed {
                    facts.push(format!("v{version} installed"));
                }
                add_to_box(
                    row_box,
                    create_label(
                        row_box,
                        &format!(
                            "<font_size=28><b>{}</b></font_size>  {}<br/>{}",
                            escape(&shorten(&group.name, 48, false)),
                            small(&facts.join(" \u{00b7} "), "#8a8f99"),
                            small(
                                &shorten(&group.description, DESCRIPTION_CHARS, false),
                                "#b4b9c2"
                            )
                        ),
                    ),
                );
                buttons.push(create_button(row_box, "Details", on_details_clicked, data));
                if installed.is_some() && !changing {
                    buttons.push(create_button(row_box, "Open", on_open_clicked, data));
                }
            }
        };
        if changing {
            unsafe {
                let column = elm_box_add(row_box);
                elm_box_padding_set(column, 0, 6);
                evas_object_size_hint_min_set(column, 360, 1);
                evas_object_size_hint_weight_set(column, 0.0, 0.0);
                evas_object_size_hint_align_set(column, 0.5, 0.5);
                let label = create_label(column, "");
                elm_box_pack_end(column, label);
                let bar = elm_progressbar_add(column);
                evas_object_size_hint_min_set(bar, 360, 24);
                evas_object_size_hint_align_set(bar, EVAS_HINT_FILL, 0.5);
                elm_box_pack_end(column, bar);
                evas_object_show(bar);
                elm_box_pack_end(row_box, column);
                evas_object_show(column);
                self.progress = bar;
                self.progress_label = label;
            }
            self.update_progress();
        }
        let has_details = in_list || self.row_app(key).is_none();
        for (index, button) in buttons.iter().enumerate() {
            let is_action = !(has_details && index == 0);
            unsafe {
                evas_object_smart_callback_add(
                    *button,
                    FOCUSED.as_ptr().cast(),
                    if is_action {
                        on_action_focused
                    } else {
                        on_row_focused
                    },
                    data,
                );
            }
        }
        let action = buttons
            .get(usize::from(has_details))
            .copied()
            .unwrap_or(ptr::null_mut());
        Row {
            key,
            widget: row,
            primary: buttons.first().copied().unwrap_or(ptr::null_mut()),
            action,
            icon,
        }
    }

    /// Re-create every rendered row in place (states changed), keeping the
    /// order, scroll position and focus.
    fn refresh_rows(&mut self) {
        for index in 0..self.rows.len() {
            let old_key = self.rows[index].key;
            if old_key == 0 {
                continue;
            }
            let old = self.rows[index].widget;
            let row = self.build_row(old_key, old);
            let focused = self.store.focus_key == Some(old_key);
            // Focusing below fires the row's focus callback, which would
            // forget which button the user was on.
            let on_action = self.store.focus_on_action;
            // Back onto the button the user pressed (it may have become
            // Open, or a progress bar; then the row's first button).
            let target = if self.store.focus_on_action && !row.action.is_null() {
                row.action
            } else {
                row.primary
            };
            unsafe {
                evas_object_del(old);
                if focused && !target.is_null() {
                    elm_object_focus_set(target, 1);
                }
            }
            self.store.focus_on_action = on_action;
            self.rows[index] = row;
        }
    }

    /// The details page: what the catalog says about one app, and its builds
    /// with their actions. Returns the widget to focus.
    fn render_details(&mut self, group_index: usize) -> *mut c_void {
        let group = self.store.groups[group_index].clone();
        let versions = self.store.versions();
        let (header, header_box) = panel(self.content_box, COLOR_ROW, 32, 24);
        self.add_content(header);
        unsafe {
            elm_box_horizontal_set(header_box, 1);
            elm_box_padding_set(header_box, 32, 0);
            let image = elm_image_add(header_box);
            evas_object_size_hint_min_set(image, DETAIL_ICON_SIZE, DETAIL_ICON_SIZE);
            evas_object_show(image);
            if let Some((_, digest)) = &group.icon {
                set_image_file(image, &icon_path(digest));
            }
            elm_box_pack_end(header_box, image);
        }
        let installed = group
            .variants
            .iter()
            .find_map(|app| versions.get(&app.package_id).map(|version| (app, version)));
        let category = CATEGORIES
            .iter()
            .find(|(id, _)| *id == group.category)
            .map_or(group.category.as_str(), |(_, label)| label);
        let mut facts = vec![category.to_owned()];
        facts.push(if group.variants.len() == 1 {
            "1 build".to_owned()
        } else {
            format!("{} builds", group.variants.len())
        });
        if let Some((_, version)) = installed {
            facts.push(format!("v{version} installed"));
        }
        let mut info = Vec::new();
        let ids: Vec<&str> = {
            let mut ids: Vec<&str> = group
                .variants
                .iter()
                .map(|app| app.package_id.as_str())
                .collect();
            ids.dedup();
            ids
        };
        info.push(format!("Package {}", ids.join(", ")));
        let repos: Vec<&str> = {
            let mut repos: Vec<&str> = group
                .variants
                .iter()
                .map(|app| app.repo.as_str())
                .filter(|repo| !repo.is_empty())
                .collect();
            repos.sort_unstable();
            repos.dedup();
            repos
        };
        if !repos.is_empty() {
            info.push(format!("Source github.com/{}", repos.join(", github.com/")));
        }
        let required = group
            .variants
            .iter()
            .map(|app| app.required_version.as_str())
            .filter(|version| !version.is_empty())
            .min_by(|a, b| store::compare_versions(a, b));
        if let Some(required) = required {
            info.push(format!("Tizen {required} or newer"));
        }
        let mut flags: Vec<&str> = group
            .variants
            .iter()
            .flat_map(|app| app.flags.iter().map(String::as_str))
            .collect();
        flags.sort_unstable();
        flags.dedup();
        if group.variants.iter().any(|app| app.partner) {
            flags.push("needs a Partner certificate");
        }
        let text = create_label(
            header_box,
            &format!(
                "<font_size=44><b>{}</b></font_size><br/>{}<br/><br/>\
                 <font_size=24><color=#e8e8e8>{}</color></font_size><br/><br/>{}{}",
                escape(&group.name),
                small(&facts.join(" \u{00b7} "), "#8a8f99"),
                escape(&group.description),
                info.iter()
                    .map(|line| small(line, "#b4b9c2"))
                    .collect::<Vec<_>>()
                    .join("<br/>"),
                if flags.is_empty() {
                    String::new()
                } else {
                    format!("<br/>{}", small(&flags.join(" \u{00b7} "), "#ffb74d"))
                }
            ),
        );
        unsafe { elm_label_wrap_width_set(text, DETAIL_TEXT_WIDTH - DETAIL_ICON_SIZE) };
        add_to_box(header_box, text);

        self.add_content(create_label(
            self.content_box,
            &small(
                if group.variants.len() == 1 {
                    "BUILD"
                } else {
                    "BUILDS"
                },
                "#8a8f99",
            ),
        ));
        unsafe {
            let scroller = elm_scroller_add(self.content_box);
            elm_scroller_policy_set(scroller, ELM_SCROLLER_POLICY_OFF, ELM_SCROLLER_POLICY_OFF);
            elm_scroller_bounce_set(scroller, 0, 0);
            evas_object_size_hint_weight_set(scroller, 1.0, 1.0);
            evas_object_size_hint_align_set(scroller, EVAS_HINT_FILL, EVAS_HINT_FILL);
            self.list_box = elm_box_add(scroller);
            evas_object_size_hint_weight_set(self.list_box, 1.0, 0.0);
            evas_object_size_hint_align_set(self.list_box, EVAS_HINT_FILL, 0.0);
            elm_box_padding_set(self.list_box, 0, 10);
            elm_object_part_content_set(scroller, ptr::null(), self.list_box);
            evas_object_show(self.list_box);
            evas_object_show(scroller);
            self.add_content(scroller);
        }
        self.rows.clear();
        self.entries = (0..group.variants.len())
            .map(|build| row_key(group_index, Some(build)))
            .collect();
        self.append_rows(self.entries.len());
        self.rows
            .iter()
            .map(|row| row.primary)
            .find(|primary| !primary.is_null())
            .unwrap_or(ptr::null_mut())
    }

    /// Open an app's details page. The list stays alive (unpacked and
    /// hidden) so closing the page restores it exactly: scroll position,
    /// loaded rows and focus.
    fn open_details(&mut self, key: usize) {
        let (group, _) = split_key(key);
        self.store.list_key = Some(row_key(group, None));
        self.store.focus_key = None;
        self.store.detail = Some(group);
        let objects = std::mem::take(&mut self.content_objects);
        for &object in &objects {
            unsafe {
                elm_box_unpack(self.content_box, object);
                evas_object_hide(object);
            }
        }
        self.parked_list = Some(ParkedList {
            objects,
            entries: std::mem::take(&mut self.entries),
            rows: std::mem::take(&mut self.rows),
            list_box: self.list_box,
            count_label: self.count_label,
        });
        self.progress = ptr::null_mut();
        self.progress_label = ptr::null_mut();
        let focus = self.render_details(group);
        if !focus.is_null() {
            unsafe { elm_object_focus_set(focus, 1) };
        }
    }

    /// Back to the list, focused on the app whose details were open.
    fn close_details(&mut self) {
        self.store.detail = None;
        self.store.focus_key = self.store.list_key.take();
        self.store.focus_on_action = false;
        let Some(parked) = self.parked_list.take() else {
            self.render();
            return;
        };
        // The details rows die with their scroller.
        self.rows.clear();
        self.clear_content();
        for &object in &parked.objects {
            unsafe {
                elm_box_pack_end(self.content_box, object);
                evas_object_show(object);
            }
        }
        self.content_objects = parked.objects;
        self.entries = parked.entries;
        self.rows = parked.rows;
        self.list_box = parked.list_box;
        self.count_label = parked.count_label;
        self.progress = ptr::null_mut();
        self.progress_label = ptr::null_mut();
        // Installs or removals on the details page changed some rows'
        // buttons; rebuilding in place also refocuses the app's row.
        self.refresh_rows();
    }

    /// Focus moved to a row: remember it, and extend the list before the
    /// user reaches its end.
    fn row_focused(&mut self, key: usize) {
        self.store.focus_key = Some(key);
        let rendered = self.rows.len();
        if let Some(position) = self.entries.iter().position(|&entry| entry == key) {
            if position + LOOKAHEAD_ROWS >= rendered {
                self.append_rows(rendered + CHUNK_ROWS);
            }
        }
    }

    fn show_icon(&mut self, digest: &str) {
        let path = icon_path(digest);
        for (_, image) in self
            .rows
            .iter()
            .filter_map(|row| row.icon.as_ref())
            .filter(|(d, _)| d == digest)
        {
            set_image_file(*image, &path);
        }
    }

    fn render_permissions(&mut self) -> *mut c_void {
        if !self.connected {
            self.add_content(create_label(
                self.content_box,
                &small(
                    "Not connected to tv-authd. Retrying automatically.",
                    "#ffb74d",
                ),
            ));
        }
        self.add_content(create_label(
            self.content_box,
            &small(
                "Apps listed here can run commands as root without asking.",
                "#8a8f99",
            ),
        ));
        let mut focus: *mut c_void = ptr::null_mut();
        if self.grants.is_empty() {
            self.add_content(create_label(
                self.content_box,
                "<font_size=28><b>No apps have permanent root access</b></font_size>",
            ));
        }
        for (index, grant) in self.grants.clone().iter().take(5).enumerate() {
            let (row, row_box) = panel(self.content_box, COLOR_ROW, 32, 16);
            self.add_content(row);
            unsafe {
                elm_box_horizontal_set(row_box, 1);
                elm_box_padding_set(row_box, 24, 0);
            }
            add_to_box(
                row_box,
                create_label(
                    row_box,
                    &format!(
                        "<font_size=28><b>{}</b></font_size><br/>{}<br/>{}",
                        escape(&grant.package),
                        small(&grant.label, "#8a8f99"),
                        small(&notify_description(&grant.notify), "#b4b9c2")
                    ),
                ),
            );
            let data = ptr::without_provenance_mut(index + 1);
            let notify = create_button(
                row_box,
                &notify_button_text(&grant.notify),
                on_notify_clicked,
                data,
            );
            unsafe { evas_object_size_hint_min_set(notify, 320, 84) };
            create_button(row_box, "Revoke", on_revoke_clicked, data);
            // Default focus on the harmless toggle, not Revoke.
            if self.focus_notify_label.as_deref() == Some(grant.label.as_str()) || focus.is_null() {
                focus = notify;
            }
        }
        if self.grants.len() > 5 {
            self.add_content(create_label(
                self.content_box,
                &small(
                    &format!("Only the first 5 of {} apps are shown.", self.grants.len()),
                    "#8a8f99",
                ),
            ));
        }

        let footer = create_row(self.content_box);
        self.add_content(footer);
        add_to_box(
            footer,
            create_label(footer, "<font_size=28><b>Recent activity</b></font_size>"),
        );
        let refresh = create_button(footer, "Refresh", on_refresh_clicked, ptr::null_mut());
        let now = unix_now();
        let lines: Vec<String> = self
            .audit
            .iter()
            .rev()
            .take(AUDIT_SHOWN)
            .map(|line| audit_markup(line, now))
            .collect();
        self.add_content(create_label(
            self.content_box,
            &if lines.is_empty() {
                small("No recent decisions.", "#8a8f99")
            } else {
                lines.join("<br/>")
            },
        ));
        if focus.is_null() {
            refresh
        } else {
            focus
        }
    }

    /// Send tv-blockd a request (`status` by default); `message` is shown
    /// once it succeeds.
    fn load_tweaks(&mut self, request: Option<Value>, message: Option<&'static str>) {
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            let request = request.unwrap_or_else(|| json!({"method": "status"}));
            let result = tv_proto::call(BLOCKD_SOCKET, &request)
                .map_err(|error| format!("Could not reach tv-blockd: {error}"))
                .and_then(|bytes| {
                    serde_json::from_slice::<Value>(&bytes)
                        .map_err(|error| format!("tv-blockd sent bad JSON: {error}"))
                })
                .and_then(|response| {
                    if response["status"] == "ok" {
                        Ok(response)
                    } else {
                        Err(format!(
                            "tv-blockd refused: {}",
                            response["error"].as_str().unwrap_or("denied")
                        ))
                    }
                });
            let _ = jobs.send(Job::Tweaks(message, result));
        });
    }

    /// Returns the widget to focus.
    fn render_tweaks(&mut self) -> *mut c_void {
        let Some(status) = self.tweaks.clone() else {
            self.add_content(create_label(
                self.content_box,
                &small("Asking tv-blockd...", "#8a8f99"),
            ));
            return ptr::null_mut();
        };
        self.tweak_summary = create_label(self.content_box, &tweaks_summary(&status));
        self.add_content(self.tweak_summary);
        let list = unsafe {
            let scroller = elm_scroller_add(self.content_box);
            elm_scroller_policy_set(scroller, ELM_SCROLLER_POLICY_OFF, ELM_SCROLLER_POLICY_OFF);
            elm_scroller_bounce_set(scroller, 0, 0);
            evas_object_size_hint_weight_set(scroller, 1.0, 1.0);
            evas_object_size_hint_align_set(scroller, EVAS_HINT_FILL, EVAS_HINT_FILL);
            let list = elm_box_add(scroller);
            evas_object_size_hint_weight_set(list, 1.0, 0.0);
            evas_object_size_hint_align_set(list, EVAS_HINT_FILL, 0.0);
            elm_box_padding_set(list, 0, 10);
            elm_object_part_content_set(scroller, ptr::null(), list);
            evas_object_show(list);
            evas_object_show(scroller);
            self.add_content(scroller);
            list
        };
        self.tweak_keys.clear();
        for (index, tweak) in tweaks(&status).into_iter().enumerate() {
            let (row, row_box) = panel(list, COLOR_ROW, 32, 12);
            unsafe {
                elm_box_pack_end(list, row);
                elm_box_horizontal_set(row_box, 1);
                elm_box_padding_set(row_box, 24, 0);
            }
            add_to_box(
                row_box,
                create_label(
                    row_box,
                    &format!(
                        "<font_size=26><b>{}</b></font_size><br/>{}",
                        escape(&tweak.title),
                        small(&tweak.note, "#8a8f99")
                    ),
                ),
            );
            let button = create_button(
                row_box,
                on_off(tweak.on),
                on_tweak_clicked,
                ptr::without_provenance_mut(index + 1),
            );
            unsafe { evas_object_size_hint_min_set(button, 180, 72) };
            self.tweak_keys.push(tweak.key);
            self.tweak_buttons.push(button);
        }
        let (row, row_box) = panel(list, COLOR_ROW, 32, 12);
        unsafe {
            elm_box_pack_end(list, row);
            elm_box_horizontal_set(row_box, 1);
            elm_box_padding_set(row_box, 24, 0);
        }
        add_to_box(
            row_box,
            create_label(
                row_box,
                &format!(
                    "<font_size=26><b>Undo all changes</b></font_size><br/>{}",
                    small(
                        "Switches everything above off and puts back what tv-blockd changed: \
                         hosts, brake files, masked or stopped services, the auto-update setting.",
                        "#8a8f99"
                    )
                ),
            ),
        );
        let undo = create_button(row_box, "Undo", on_undo_clicked, ptr::null_mut());
        unsafe { evas_object_size_hint_min_set(undo, 180, 72) };
        self.tweak_buttons
            .get(self.tweak_focus)
            .or(self.tweak_buttons.first())
            .copied()
            .unwrap_or(ptr::null_mut())
    }

    fn update_tweaks(&mut self) {
        let Some(status) = &self.tweaks else { return };
        let tweaks = tweaks(status);
        if tweaks.len() != self.tweak_buttons.len() {
            self.render();
            return;
        }
        set_text(self.tweak_summary, &tweaks_summary(status));
        for (button, tweak) in self.tweak_buttons.iter().zip(&tweaks) {
            set_text(*button, on_off(tweak.on));
        }
    }

    fn add_content(&mut self, child: *mut c_void) {
        if child.is_null() {
            return;
        }
        unsafe { elm_box_pack_end(self.content_box, child) };
        self.content_objects.push(child);
    }

    fn clear_content(&mut self) {
        for object in self.content_objects.drain(..) {
            unsafe { evas_object_del(object) };
        }
    }

    fn exit_now(&mut self) {
        if self.exiting {
            return;
        }
        self.exiting = true;
        unsafe { ui_app_exit() };
    }
}

fn state_text(app: &App, state: &State) -> String {
    match state {
        State::NotInstalled => format!("v{}", app.version),
        State::Installed => format!("v{} \u{00b7} installed", app.version),
        State::Upgradable { installed } => format!("v{installed} \u{2192} v{}", app.version),
        State::Newer { installed } => format!("v{installed} installed (catalog v{})", app.version),
    }
}

struct Tweak {
    key: String,
    title: String,
    note: String,
    on: bool,
}

/// The toggles for a tv-blockd `status` response, in screen order.
fn tweaks(status: &Value) -> Vec<Tweak> {
    let config = &status["config"];
    let tweak = |key: &str, title: &str, note: &str| Tweak {
        key: key.into(),
        title: title.into(),
        note: note.into(),
        on: config[key] == true,
    };
    let mut list = vec![tweak(
        "hosts",
        "Block tracking hosts",
        "Points the host groups below at nothing, for every app on the TV.",
    )];
    let enabled = config["groups"].as_array().cloned().unwrap_or_default();
    for group in status["groups"].as_array().into_iter().flatten() {
        let id = group["id"].as_str().unwrap_or_default();
        let hosts = group["hosts"].as_array().map_or(0, Vec::len);
        list.push(Tweak {
            key: format!("group:{id}"),
            title: format!("  {}", group["label"].as_str().unwrap_or(id)),
            note: format!(
                "{} {hosts} host{}, evidence: {}.",
                group["note"].as_str().unwrap_or_default(),
                if hosts == 1 { "" } else { "s" },
                group["evidence"].as_str().unwrap_or("unknown")
            ),
            on: enabled.iter().any(|value| value == id),
        });
    }
    list.push(tweak(
        "otn_lock",
        "Keep automatic updates off",
        "Turns the TV's auto-update setting back off whenever it is switched on.",
    ));
    list.push(tweak(
        "telemetry",
        "Stop telemetry services",
        "Stops Samsung's log, crash and analytics services, again after every reboot.",
    ));
    list
}

fn tweaks_summary(status: &Value) -> String {
    let report = &status["report"];
    let mut parts = vec![if status["config"]["hosts"] == true {
        format!("{} hosts blocked", report["blocked_hosts"])
    } else {
        "Host blocking off".into()
    }];
    parts.push(match report["otn_autoupdate"].as_i64() {
        Some(0) => "auto-update off".into(),
        Some(_) => "auto-update on".into(),
        None => "auto-update unknown".into(),
    });
    let running = report["telemetry_active"].as_array().map_or(0, Vec::len);
    parts.push(format!("{running} telemetry services running"));
    let failed = report["namespaces_failed"].as_u64().unwrap_or(0);
    if failed > 0 {
        parts.push(format!("{failed} namespaces not covered"));
    }
    let mut markup = small(&parts.join(" · "), "#8a8f99");
    if let Some(error) = report["errors"]
        .as_array()
        .and_then(|errors| errors.first())
    {
        markup.push_str("<br/>");
        markup.push_str(&small(error.as_str().unwrap_or_default(), "#ffb74d"));
    }
    markup
}

fn on_off(on: bool) -> &'static str {
    if on {
        "On"
    } else {
        "Off"
    }
}

fn or_unknown(error: &str) -> &str {
    if error.is_empty() {
        "unknown"
    } else {
        error
    }
}

fn spawn(work: impl FnOnce() + Send + 'static) {
    if let Err(error) = thread::Builder::new()
        .name("tv-store-job".into())
        .spawn(work)
    {
        log_line(&format!("spawn: {error}"));
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// `"<unix seconds> <event>"` → "12 min ago  event".
fn audit_markup(line: &str, now: i64) -> String {
    let (stamp, event) = line.split_once(' ').unwrap_or(("", line));
    let age = stamp.parse::<i64>().map_or(String::new(), |at| {
        let seconds = (now - at).max(0);
        match seconds {
            0..60 => "just now".into(),
            60..3600 => format!("{} min ago", seconds / 60),
            3600..86_400 => format!("{} h ago", seconds / 3600),
            _ => format!("{} d ago", seconds / 86_400),
        }
    });
    format!(
        "{}  {}",
        small(&age, "#8a8f99"),
        small(&shorten(event, 120, false), "#e8e8e8")
    )
}

/// Show a cached icon; a missing file leaves the image empty.
fn set_image_file(image: *mut c_void, path: &Path) {
    if !path.exists() {
        return;
    }
    if let Ok(file) = CString::new(path.as_os_str().as_encoded_bytes()) {
        unsafe { elm_image_file_set(image, file.as_ptr(), ptr::null()) };
    }
}

fn icon_path(digest: &str) -> PathBuf {
    Path::new(STORE_DIR)
        .join("icons")
        .join(format!("{digest}.png"))
}

/// One-shot broker request on its own connection (blocks; worker threads).
fn broker_call(request: Value) -> Result<Value, String> {
    let bytes =
        tv_proto::call(DEFAULT_SOCKET, &request).map_err(|error| format!("tv-authd: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("tv-authd sent bad JSON: {error}"))
}

fn ok_or_error(response: &Value) -> Result<(), String> {
    if response["status"] == "ok" {
        return Ok(());
    }
    let error = response["error"].as_str().unwrap_or("unknown");
    Err(match (error, response["detail"].as_str()) {
        ("user_denied", _) => "You denied the change.".into(),
        ("prompt_timeout", _) => "The prompt timed out; nothing was changed.".into(),
        (error, Some(detail)) => format!("tv-authd refused: {error} ({detail})"),
        (error, None) => format!("tv-authd refused: {error}"),
    })
}

fn fetch_catalog() -> Result<Vec<App>, String> {
    let bytes = tv_tizen::http::get(store::CATALOG_URL, store::MAX_CATALOG_BYTES, 30)?;
    let (signature, payload) = store::envelope(&bytes)?;
    if !tv_tizen::crypto::verify_rsa_sha256(store::CATALOG_KEY_PEM, payload, &signature)? {
        return Err("the catalog signature does not match the Apps2Samsung key".into());
    }
    store::apps(payload, unix_now())
}

fn installed_packages() -> Result<HashMap<String, Installed>, String> {
    let response = broker_call(json!({"version": 2, "id": "store-list", "method": "pkg.list"}))?;
    ok_or_error(&response)?;
    let mut installed = HashMap::new();
    for package in response["packages"].as_array().into_iter().flatten() {
        let text = |name: &str| package[name].as_str().unwrap_or("").to_owned();
        installed.insert(
            text("pkgid"),
            Installed {
                version: text("pkg_version"),
                main_app_id: text("main_app_id"),
            },
        );
    }
    Ok(installed)
}

/// Writes through to a file while hashing, optionally reporting progress.
struct Hashing {
    file: fs::File,
    hasher: Sha256,
    progress: Option<Progress>,
}

struct Progress {
    jobs: Sender<Job>,
    total: u64,
    written: u64,
    last: Instant,
}

impl Write for Hashing {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.file.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        if let Some(progress) = &mut self.progress {
            progress.written += written as u64;
            if progress.last.elapsed() >= std::time::Duration::from_millis(200) {
                progress.last = Instant::now();
                let fraction = progress.written as f64 / progress.total.max(1) as f64;
                let _ = progress.jobs.send(Job::Progress(Some(fraction)));
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Download `url` to `path` (through a `.part` file) only if it hashes to
/// `sha256` and is at most `max_bytes`; returns the size.
fn fetch_verified(
    url: &str,
    sha256: &str,
    max_bytes: u64,
    path: &Path,
    jobs: Option<&Sender<Job>>,
) -> Result<u64, String> {
    if let Some(parent) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let partial = path.with_extension("part");
    let _ = fs::remove_file(&partial);
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|error| format!("{}: {error}", partial.display()))?;
    let mut sink = Hashing {
        file,
        hasher: Sha256::new(),
        progress: jobs.map(|jobs| Progress {
            jobs: jobs.clone(),
            total: max_bytes,
            written: 0,
            last: Instant::now(),
        }),
    };
    let result = tv_tizen::http::get_to(url, &mut sink, max_bytes, 600).and_then(|written| {
        let digest: String = sink
            .hasher
            .finalize_reset()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if digest != sha256 {
            return Err(format!("{url}: SHA-256 mismatch, not using it"));
        }
        // tv-authd (root) stages the file; the backend later reads it as owner.
        let _ = fs::set_permissions(&partial, fs::Permissions::from_mode(0o644));
        fs::rename(&partial, path).map_err(|error| error.to_string())?;
        Ok(written)
    });
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

fn install_app(app: &App, jobs: &Sender<Job>) -> Result<String, String> {
    let _ = jobs.send(Job::Status(format!(
        "Downloading {} ({})...",
        app.name,
        format_size(app.size)
    )));
    let path = Path::new(STORE_DIR).join(format!("{}.{}", app.package_id, app.pkg_type));
    let written = fetch_verified(&app.url, &app.sha256, app.size, &path, Some(jobs))?;
    if written != app.size {
        let _ = fs::remove_file(&path);
        return Err(format!(
            "{}: expected {} bytes, got {written}",
            app.name, app.size
        ));
    }
    let _ = jobs.send(Job::Status(format!(
        "Verified {}. Approve the install on the prompt.",
        app.name
    )));
    let _ = jobs.send(Job::Progress(None));
    let request = json!({"version": 2, "id": "store-install", "method": "pkg.install",
                         "pkg_type": app.pkg_type, "path": path.to_string_lossy(),
                         "timeout_ms": 120_000});
    let result = broker_call(request);
    let _ = fs::remove_file(&path);
    let response = result?;
    ok_or_error(&response)?;
    Ok(format!(
        "Installed {} {}{}.",
        app.name,
        response["pkg_version"].as_str().unwrap_or(&app.version),
        if response["re_signed"] == true {
            " (re-signed on this TV)"
        } else {
            ""
        }
    ))
}

unsafe extern "C" fn app_create(user_data: *mut c_void) -> bool {
    UI_PTR.with(|cell| cell.set(user_data.cast()));
    let state = ui();
    log_line("app_create");
    if !state.create_window() {
        return false;
    }
    state.show();
    true
}

unsafe extern "C" fn app_terminate(_user_data: *mut c_void) {
    ui().terminate();
}

unsafe extern "C" fn app_pause(_user_data: *mut c_void) {}
unsafe extern "C" fn app_resume(_user_data: *mut c_void) {}

unsafe extern "C" fn app_control(_app_control: *mut c_void, _user_data: *mut c_void) {
    let state = ui();
    let first = state.app_control_count == 0;
    state.app_control_count = state.app_control_count.saturating_add(1);
    if first {
        return;
    }
    log_line("relaunched while running");
    state.refresh_grants();
    state.list_installed();
    state.show();
}

unsafe extern "C" fn on_timer(_data: *mut c_void) -> EinaBool {
    ui().on_timer()
}

unsafe extern "C" fn on_back(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    log_line("back key");
    let state = ui();
    if state.tab == Tab::Store && state.store.detail.is_some() {
        state.close_details();
    } else {
        state.exit_now();
    }
}

unsafe extern "C" fn on_tab_clicked(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    let state = ui();
    state.store.detail = None;
    state.status.clear();
    state.tab = match data as usize {
        2 => Tab::Permissions,
        3 => Tab::Tweaks,
        _ => Tab::Store,
    };
    match state.tab {
        Tab::Permissions => state.refresh_grants(),
        Tab::Tweaks => state.load_tweaks(None, None),
        Tab::Store => {}
    }
    state.render();
}

unsafe extern "C" fn on_category_selected(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let state = ui();
    state.store.category = (data as usize).saturating_sub(1).min(CATEGORIES.len() - 1);
    state.store.focus_key = None;
    state.render();
}

unsafe extern "C" fn on_search_changed(
    _data: *mut c_void,
    object: *mut c_void,
    _info: *mut c_void,
) {
    let state = ui();
    let text = elm_object_part_text_get(object, ptr::null());
    let markup = if text.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(text)
            .to_string_lossy()
            .into_owned()
    };
    state.store.query = markup
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    state.store.focus_key = None;
    state.rebuild_list();
}

unsafe extern "C" fn on_row_focused(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    if data as usize != 0 {
        let state = ui();
        state.store.focus_on_action = false;
        state.row_focused(data as usize);
    }
}

unsafe extern "C" fn on_action_focused(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    if data as usize != 0 {
        let state = ui();
        state.store.focus_on_action = true;
        state.row_focused(data as usize);
    }
}

unsafe extern "C" fn on_details_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    if data as usize != 0 {
        ui().open_details(data as usize);
    }
}

unsafe extern "C" fn on_reload_clicked(
    _data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let state = ui();
    state.store.icons_requested.clear();
    state.load_catalog();
    state.set_status(state.status.clone());
}

unsafe extern "C" fn on_tweak_clicked(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    let state = ui();
    let index = (data as usize).wrapping_sub(1);
    let (Some(key), Some(status)) = (state.tweak_keys.get(index), &state.tweaks) else {
        return;
    };
    let mut config = status["config"].clone();
    match key.strip_prefix("group:") {
        Some(id) => {
            if let Some(groups) = config["groups"].as_array_mut() {
                if let Some(position) = groups.iter().position(|value| value == id) {
                    groups.remove(position);
                } else {
                    groups.push(json!(id));
                }
            }
        }
        None => {
            let on = config[key.as_str()] == true;
            config[key.as_str()] = json!(!on);
        }
    }
    state.tweak_focus = index;
    state.set_status("Applying...");
    state.load_tweaks(
        Some(json!({"method": "set", "config": config})),
        Some("Saved and applied."),
    );
}

unsafe extern "C" fn on_undo_clicked(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    let state = ui();
    state.set_status("Undoing...");
    state.load_tweaks(
        Some(json!({"method": "undo"})),
        Some("Everything tv-blockd changed is undone."),
    );
}

fn clicked_app(data: *mut c_void) -> Option<App> {
    let key = data as usize;
    if key == 0 {
        return None;
    }
    let state = ui();
    if let Some(app) = state.row_app(key) {
        return Some(app.clone());
    }
    // A group row's "Open": its installed build.
    let (group, _) = split_key(key);
    let group = state.store.groups.get(group)?;
    group
        .variants
        .iter()
        .find(|app| state.store.installed.contains_key(&app.package_id))
        .cloned()
}

unsafe extern "C" fn on_install_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    if let Some(app) = clicked_app(data) {
        log_line(&format!("install clicked {}", app.package_id));
        ui().install(app);
    }
}

unsafe extern "C" fn on_open_clicked(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    if let Some(app) = clicked_app(data) {
        ui().open(&app);
    }
}

unsafe extern "C" fn on_remove_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    if let Some(app) = clicked_app(data) {
        log_line(&format!("remove clicked {}", app.package_id));
        ui().uninstall(app);
    }
}

unsafe extern "C" fn on_revoke_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let state = ui();
    let Some(label) = (data as usize)
        .checked_sub(1)
        .and_then(|index| state.grants.get(index))
        .map(|grant| grant.label.clone())
    else {
        return;
    };
    log_line(&format!("revoke clicked {label}"));
    state.status = format!("Revoking {label}...");
    state.render();
    if let Some(broker) = &state.broker {
        broker.revoke(&label);
    }
}

unsafe extern "C" fn on_notify_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let state = ui();
    let Some((label, next)) = (data as usize)
        .checked_sub(1)
        .and_then(|index| state.grants.get(index))
        .map(|grant| (grant.label.clone(), next_notify(&grant.notify)))
    else {
        return;
    };
    log_line(&format!("notify clicked {label} -> {next}"));
    state.focus_notify_label = Some(label.clone());
    if let Some(broker) = &state.broker {
        broker.set_notify(&label, &next);
    }
}

unsafe extern "C" fn on_refresh_clicked(
    _data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let state = ui();
    state.status = "Refreshing...".into();
    state.refresh_grants();
    state.render();
}

fn main() {
    tv_efl::log::set_path("/tmp/tvauth-ui.log");
    let status = run();
    if status != 0 {
        std::process::exit(status);
    }
}

fn run() -> i32 {
    let (tx, rx) = mpsc::channel();
    let state = Box::into_raw(Box::new(Ui::new(tx, rx)));
    UI_PTR.with(|cell| cell.set(state));

    let arguments: Vec<CString> = std::env::args_os()
        .filter_map(|argument| CString::new(argument.as_encoded_bytes()).ok())
        .collect();
    let mut argv: Vec<*mut c_char> = arguments
        .iter()
        .map(|argument| argument.as_ptr() as *mut c_char)
        .collect();
    argv.push(ptr::null_mut());
    let argc = i32::try_from(arguments.len()).unwrap_or(1);
    let mut callbacks = UiAppLifecycleCallback {
        create: Some(app_create),
        terminate: Some(app_terminate),
        pause: Some(app_pause),
        resume: Some(app_resume),
        app_control: Some(app_control),
    };

    // SAFETY: argv, callbacks and the heap-allocated state stay alive for the
    // whole synchronous `ui_app_main` call. The callbacks have the signatures
    // declared by Tizen's app.h.
    let status = unsafe { ui_app_main(argc, argv.as_mut_ptr(), &mut callbacks, state.cast()) };
    unsafe { drop(Box::from_raw(state)) };
    UI_PTR.with(|cell| cell.set(ptr::null_mut()));
    status
}
