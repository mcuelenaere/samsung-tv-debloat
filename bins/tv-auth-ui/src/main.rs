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
//! It is a full-screen notification window holding the BACK keygrab (a plain
//! `ELM_WIN_BASIC` window never reached the compositor on this firmware). The
//! compositor keeps the systemd-started popup below notification windows, so
//! the manager hides itself, keygrab released, while a store change waits for
//! the popup's prompt, and returns with the result. Network, hashing and broker calls run on worker threads
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
use std::time::{SystemTime, UNIX_EPOCH};
use tv_auth_ui::store::{self, App, State};
use tv_efl::log::log_line;
use tv_efl::widgets::*;
use tv_efl::*;
use tv_proto::client::{BrokerClient, Event, Notifier};
use tv_proto::markup::*;
use tv_proto::ui::GrantInfo;
use tv_proto::DEFAULT_SOCKET;

const COLOR_BACKGROUND: i32 = 0x10_12_17;
const COLOR_ROW: i32 = 0x1c_1f_26;
const PAGE_ROWS: usize = 6;
const ICON_SIZE: i32 = 96;
const DESCRIPTION_CHARS: usize = 110;
const AUDIT_SHOWN: usize = 8;

/// Downloads and icons live in the app's corner of `/tmp`, which `tv-authd`'s
/// staging accepts as a package source.
const STORE_DIR: &str = "/tmp/tv-store";
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
const BACK_KEY: &[u8] = b"XF86Back\0";

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
}

/// Results from worker threads.
enum Job {
    Catalog(Result<Vec<App>, String>),
    Installed(Result<HashMap<String, Installed>, String>),
    Icon,
    Status(String),
    Finished(Result<String, String>),
    Audit(Vec<String>),
}

#[derive(Clone)]
struct Installed {
    version: String,
    main_app_id: String,
}

#[derive(Default)]
struct Store {
    apps: Vec<App>,
    installed: HashMap<String, Installed>,
    loaded: bool,
    category: usize,
    page: usize,
    busy: bool,
    icons_requested: HashSet<String>,
}

impl Store {
    fn visible(&self) -> Vec<&App> {
        let category = CATEGORIES[self.category].0;
        self.apps
            .iter()
            .filter(|app| category.is_empty() || app.category == category)
            .collect()
    }

    fn pages(&self) -> usize {
        self.visible().len().div_ceil(PAGE_ROWS).max(1)
    }

    fn page_apps(&self) -> Vec<App> {
        self.visible()
            .into_iter()
            .skip(self.page * PAGE_ROWS)
            .take(PAGE_ROWS)
            .cloned()
            .collect()
    }

    fn versions(&self) -> HashMap<String, String> {
        self.installed
            .iter()
            .map(|(pkgid, installed)| (pkgid.clone(), installed.version.clone()))
            .collect()
    }
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
    /// The apps on screen, indexed by button data (row + 1).
    rows: Vec<App>,
    grants: Vec<GrantInfo>,
    audit: Vec<String>,
    exiting: bool,
    back_grabbed: bool,
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
            rows: Vec::new(),
            grants: Vec::new(),
            audit: Vec::new(),
            exiting: false,
            back_grabbed: false,
            connected: false,
            app_control_count: 0,
            focus_notify_label: None,
        }
    }

    fn create_window(&mut self) -> bool {
        unsafe {
            let window = elm_win_add(
                ptr::null_mut(),
                APP_ID.as_ptr().cast(),
                ELM_WIN_NOTIFICATION,
            );
            if window.is_null() {
                log_line("elm_win_add failed");
                return false;
            }
            self.window = window;
            let wayland_window = elm_win_wl_window_get(window);
            if !wayland_window.is_null() {
                ecore_wl2_window_type_set(wayland_window, ECORE_WL2_WINDOW_TYPE_NOTIFICATION);
            }
            set_window_title(window, "TV suite");
            // The lowest notification level: above apps such as YouTube, but
            // always below tv-auth-popupd's TOP-level consent prompt.
            let level = efl_util_set_notification_window_level(window, NOTIFICATION_DEFAULT);
            log_line(&format!("notification level DEFAULT result={level}"));
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
            evas_object_size_hint_align_set(main, EVAS_HINT_FILL, 0.0);
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

            self.status_label = add_to_box(main, create_label(main, ""));

            self.content_box = elm_box_add(main);
            evas_object_size_hint_weight_set(self.content_box, 1.0, 1.0);
            evas_object_size_hint_align_set(self.content_box, EVAS_HINT_FILL, 0.0);
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
        self.grab_back(true);
        unsafe {
            evas_object_show(self.window);
            elm_win_raise(self.window);
            elm_win_activate(self.window);
        }
    }

    /// Hold BACK only while no store change waits on the popup's prompt,
    /// which needs the key (and focus) for itself.
    fn grab_back(&mut self, grab: bool) {
        if grab == self.back_grabbed || self.window.is_null() {
            return;
        }
        unsafe {
            if grab {
                self.back_grabbed =
                    eext_win_keygrab_set(self.window, BACK_KEY.as_ptr().cast()) == 1;
                if !self.back_grabbed {
                    log_line("XF86Back keygrab failed");
                }
                elm_win_activate(self.window);
            } else {
                eext_win_keygrab_unset(self.window, BACK_KEY.as_ptr().cast());
                self.back_grabbed = false;
            }
        }
    }

    fn terminate(&mut self) {
        if let Some(broker) = self.broker.take() {
            broker.stop();
        }
        self.grab_back(false);
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

    fn on_job(&mut self, job: Job) -> bool {
        match job {
            Job::Catalog(Ok(apps)) => {
                self.status = format!("{} apps in the Apps2Samsung catalog.", apps.len());
                self.store.apps = apps;
                self.store.loaded = true;
                self.store.page = 0;
                self.list_installed();
            }
            Job::Catalog(Err(error)) => {
                log_line(&format!("catalog: {error}"));
                self.status = format!("Could not load the catalog: {error}");
            }
            Job::Installed(Ok(installed)) => self.store.installed = installed,
            Job::Installed(Err(error)) => {
                self.status = format!("Could not read installed packages: {error}");
            }
            // A re-render picks up the cached file.
            Job::Icon => return self.tab == Tab::Store,
            Job::Status(message) => self.status = message,
            Job::Finished(result) => {
                self.store.busy = false;
                self.show();
                self.status = result.unwrap_or_else(|error| error);
                log_line(&self.status);
                self.list_installed();
            }
            Job::Audit(lines) => {
                self.audit = lines;
                return self.tab == Tab::Permissions;
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

    fn fetch_icons(&mut self) {
        let mut wanted = Vec::new();
        for (url, digest) in self.rows.iter().filter_map(|app| app.icon.clone()) {
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
                match fetch_verified(&url, &digest, 1 << 20, &icon_path(&digest)) {
                    Ok(_) => {
                        let _ = jobs.send(Job::Icon);
                    }
                    Err(error) => log_line(&format!("icon {url}: {error}")),
                }
            }
        });
    }

    fn start_change(&mut self) -> bool {
        if self.store.busy {
            self.status = "Another change is still in progress.".into();
            self.render();
            return false;
        }
        self.store.busy = true;
        self.grab_back(false);
        unsafe { evas_object_hide(self.window) };
        true
    }

    fn install(&mut self, app: App) {
        if !self.start_change() {
            return;
        }
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            let _ = jobs.send(Job::Finished(install_app(&app, &jobs)));
        });
    }

    fn uninstall(&mut self, app: App) {
        if !self.start_change() {
            return;
        }
        self.status = format!("Removing {}: approve it on the prompt.", app.name);
        self.render();
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
        self.status = match tv_tizen::app::launch(&installed.main_app_id) {
            Ok(()) => format!("Opening {}...", app.name),
            Err(error) => format!("Could not open {}: {error}", app.name),
        };
        self.render();
    }

    fn render(&mut self) {
        if self.content_box.is_null() {
            return;
        }
        self.clear_content();
        let focus = match self.tab {
            Tab::Store => self.render_store(),
            Tab::Permissions => self.render_permissions(),
        };
        set_text(
            self.status_label,
            &format!(
                "<font_size=24><color=#c9ccd3>{}</color></font_size>",
                escape(&self.status)
            ),
        );
        if !focus.is_null() {
            unsafe { elm_object_focus_set(focus, 1) };
        }
    }

    /// Returns the widget to focus.
    fn render_store(&mut self) -> *mut c_void {
        let toolbar = create_row(self.content_box);
        self.add_content(toolbar);
        let pages = self.store.pages();
        self.store.page = self.store.page.min(pages - 1);
        add_to_box(
            toolbar,
            create_label(
                toolbar,
                &small(
                    &format!(
                        "{} apps \u{00b7} page {} of {}",
                        self.store.visible().len(),
                        self.store.page + 1,
                        pages
                    ),
                    "#8a8f99",
                ),
            ),
        );
        let category = create_button(
            toolbar,
            &format!("Category: {}", CATEGORIES[self.store.category].1),
            on_category_clicked,
            ptr::null_mut(),
        );
        unsafe { evas_object_size_hint_min_set(category, 340, 72) };
        create_button(
            toolbar,
            "Previous",
            on_page_clicked,
            ptr::without_provenance_mut(1),
        );
        create_button(
            toolbar,
            "Next",
            on_page_clicked,
            ptr::without_provenance_mut(2),
        );
        create_button(toolbar, "Reload", on_reload_clicked, ptr::null_mut());

        self.rows = self.store.page_apps();
        let versions = self.store.versions();
        let mut focus: *mut c_void = ptr::null_mut();
        for (index, app) in self.rows.clone().iter().enumerate() {
            let (row, row_box) = panel(self.content_box, COLOR_ROW, 24, 12);
            self.add_content(row);
            unsafe {
                elm_box_horizontal_set(row_box, 1);
                elm_box_padding_set(row_box, 24, 0);
            }
            add_to_box(row_box, icon(row_box, app));

            let state = store::state(app, &versions);
            let state_text = match &state {
                State::NotInstalled => format!("v{}", app.version),
                State::Installed => format!("v{} \u{00b7} installed", app.version),
                State::Upgradable { installed } => {
                    format!("v{installed} \u{2192} v{}", app.version)
                }
                State::Newer { installed } => {
                    format!("v{installed} installed (catalog v{})", app.version)
                }
            };
            let mut facts = vec![
                state_text,
                app.pkg_type.to_uppercase(),
                format_size(app.size),
            ];
            if app.partner {
                facts.push("partner".into());
            }
            add_to_box(
                row_box,
                create_label(
                    row_box,
                    &format!(
                        "<font_size=28><b>{}</b></font_size>  {}<br/>{}",
                        escape(&shorten(&app.name, 48, false)),
                        small(&facts.join(" \u{00b7} "), "#8a8f99"),
                        small(
                            &shorten(&app.description, DESCRIPTION_CHARS, false),
                            "#b4b9c2"
                        )
                    ),
                ),
            );

            let data = ptr::without_provenance_mut(index + 1);
            let primary = match state {
                State::NotInstalled => create_button(row_box, "Install", on_install_clicked, data),
                State::Upgradable { .. } => {
                    create_button(row_box, "Update", on_install_clicked, data)
                }
                State::Installed | State::Newer { .. } => {
                    create_button(row_box, "Open", on_open_clicked, data)
                }
            };
            if !matches!(state, State::NotInstalled) {
                create_button(row_box, "Remove", on_remove_clicked, data);
            }
            if focus.is_null() {
                focus = primary;
            }
        }
        if self.rows.is_empty() && self.store.loaded {
            self.add_content(create_label(
                self.content_box,
                &small("No apps in this category.", "#8a8f99"),
            ));
        }
        self.fetch_icons();
        if focus.is_null() {
            category
        } else {
            focus
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

fn icon(parent: *mut c_void, app: &App) -> *mut c_void {
    let file = app
        .icon
        .as_ref()
        .map(|(_, digest)| icon_path(digest))
        .filter(|path| path.exists())
        .and_then(|path| CString::new(path.into_os_string().into_encoded_bytes()).ok());
    unsafe {
        if let Some(file) = file {
            let image = elm_image_add(parent);
            if !image.is_null() && elm_image_file_set(image, file.as_ptr(), ptr::null()) == 1 {
                evas_object_size_hint_min_set(image, ICON_SIZE, ICON_SIZE);
                evas_object_show(image);
                return image;
            }
            if !image.is_null() {
                evas_object_del(image);
            }
        }
        let placeholder = create_rect(parent, 0x2c_31_3c, 255);
        evas_object_size_hint_min_set(placeholder, ICON_SIZE, ICON_SIZE);
        placeholder
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

/// Writes through to a file while hashing.
struct Hashing {
    file: fs::File,
    hasher: Sha256,
}

impl Write for Hashing {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.file.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Download `url` to `path` (through a `.part` file) only if it hashes to
/// `sha256` and is at most `max_bytes`; returns the size.
fn fetch_verified(url: &str, sha256: &str, max_bytes: u64, path: &Path) -> Result<u64, String> {
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
    let written = fetch_verified(&app.url, &app.sha256, app.size, &path)?;
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
    ui().exit_now();
}

unsafe extern "C" fn on_tab_clicked(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    let state = ui();
    state.tab = if data as usize == 2 {
        Tab::Permissions
    } else {
        Tab::Store
    };
    if state.tab == Tab::Permissions {
        state.refresh_grants();
    }
    state.render();
}

unsafe extern "C" fn on_category_clicked(
    _data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let state = ui();
    state.store.category = (state.store.category + 1) % CATEGORIES.len();
    state.store.page = 0;
    state.render();
}

unsafe extern "C" fn on_page_clicked(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    let state = ui();
    let pages = state.store.pages();
    state.store.page = if data as usize == 1 {
        (state.store.page + pages - 1) % pages
    } else {
        (state.store.page + 1) % pages
    };
    state.render();
}

unsafe extern "C" fn on_reload_clicked(
    _data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let state = ui();
    state.store.icons_requested.clear();
    state.load_catalog();
    state.render();
}

fn row_app(data: *mut c_void) -> Option<App> {
    let index = (data as usize).checked_sub(1)?;
    ui().rows.get(index).cloned()
}

unsafe extern "C" fn on_install_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    if let Some(app) = row_app(data) {
        log_line(&format!("install clicked {}", app.package_id));
        let state = ui();
        state.install(app);
        state.render();
    }
}

unsafe extern "C" fn on_open_clicked(data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    if let Some(app) = row_app(data) {
        ui().open(&app);
    }
}

unsafe extern "C" fn on_remove_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    if let Some(app) = row_app(data) {
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
