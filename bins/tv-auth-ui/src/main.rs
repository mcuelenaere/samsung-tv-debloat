//! `org.tvauth.ui`: the user-launched TV suite manager.
//!
//! A rail of tabs and pages drawn with plain evas objects (`screen.rs`,
//! from page data built here; see `page.rs`). Three tabs:
//! - **Store**: the signed Apps2Samsung catalog (see `store.rs`); installs and
//!   removals go through `tv-authd`'s `pkg.*` like any package's, so every
//!   change is shown as a prompt by `tv-auth-popupd` first;
//! - **Permissions**: persistent root grants (toast mode, revoke) and the
//!   daemon's recent decisions (`grants.audit`);
//! - **Tweaks**: tv-blockd's and tv-patchd's settings.
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

use screen::{Event as ScreenEvent, Screen};
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
use tv_auth_ui::page::{Block, Card, Chip, Control, Row, Tone};
use tv_auth_ui::store::{self, App, Group, State};
use tv_efl::log::log_line;
use tv_efl::widgets::*;
use tv_efl::*;
use tv_proto::client::{BrokerClient, Event, Notifier};
use tv_proto::markup::*;
use tv_proto::ui::GrantInfo;
use tv_proto::DEFAULT_SOCKET;

mod dock;
mod kit;
mod screen;

const AUDIT_SHOWN: usize = 8;

/// Downloads and icons live in the app's corner of `/tmp`, which `tv-authd`'s
/// staging accepts as a package source.
const STORE_DIR: &str = "/tmp/tv-store";
const BLOCKD_SOCKET: &str = "/run/tv-suite/blockd.sock";
const PATCHD_SOCKET: &str = "/run/tv-suite/patchd.sock";

/// The daemons behind the Tweaks tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Service {
    Block,
    Patch,
}

impl Service {
    fn socket(self) -> &'static str {
        match self {
            Service::Block => BLOCKD_SOCKET,
            Service::Patch => PATCHD_SOCKET,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Service::Block => "tv-blockd",
            Service::Patch => "tv-patchd",
        }
    }
}
/// Seconds between `system` snapshots while the System tab is open.
const SYSTEM_REFRESH: f64 = 3.0;
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
    System,
}

/// What a focusable part of a page does on OK.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Search,
    Reload,
    Category(usize),
    /// A store group's details page.
    Details(usize),
    /// Install or update a group's build.
    Install(usize, usize),
    /// Open a group's build, or its installed one.
    Open(usize, Option<usize>),
    Remove(usize, usize),
    Notify(usize),
    Revoke(usize),
    Refresh,
    /// A Tweaks key: `hosts`, `group:<id>`, `otn_lock`, `telemetry`,
    /// `patch:tizentube`, `patch:update`, `patch:custom_home`.
    Tweak(String),
    Undo,
}

/// Results from worker threads.
enum Job {
    Catalog(Result<Vec<App>, String>),
    Installed(Result<HashMap<String, Installed>, String>),
    /// An icon with this SHA-256 is now cached.
    Icon,
    Status(String),
    /// Download fraction, or `None` while waiting on the prompt/installer.
    Progress(Option<f64>),
    Finished(Result<String, String>),
    Audit(Vec<String>),
    /// A Tweaks daemon's status, with the message for a change it answers.
    Tweaks(Service, Option<String>, Result<Value, String>),
    /// tv-blockd's `system` snapshot.
    System(Result<Value, String>),
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
    /// Group indexes in the grid's order (see `Ui::arrange`).
    order: Vec<usize>,
    /// The group whose details page was opened from the grid.
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

struct Ui {
    window: *mut c_void,
    screen: Option<Screen<Action>>,
    /// Focus for the next render (back from a details page).
    pending_focus: Option<Action>,

    broker: Option<BrokerClient>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    jobs_tx: Sender<Job>,
    jobs_rx: Receiver<Job>,

    tab: Tab,
    status: String,
    store: Store,
    grants: Vec<GrantInfo>,
    audit: Vec<String>,
    /// tv-blockd's last `status` response.
    tweaks: Option<Value>,
    /// tv-patchd's.
    patch: Option<Value>,
    /// tv-blockd's last `system` snapshot, and when the next was asked for.
    system: Option<Value>,
    system_asked: Option<Instant>,
    exiting: bool,
    connected: bool,
    app_control_count: u32,
}

impl Ui {
    fn new(tx: Sender<Event>, rx: Receiver<Event>) -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel();
        Self {
            window: ptr::null_mut(),
            screen: None,
            pending_focus: None,
            broker: None,
            tx,
            rx,
            jobs_tx,
            jobs_rx,
            tab: Tab::Store,
            status: String::new(),
            store: Store::default(),
            grants: Vec::new(),
            audit: Vec::new(),
            tweaks: None,
            patch: None,
            system: None,
            system_asked: None,
            exiting: false,
            connected: false,
            app_control_count: 0,
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
                on_delete,
                ptr::null_mut(),
            );
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
            evas_object_resize(window, width, height);
            let mut screen = Screen::new(window, width, height, on_frame);
            screen.set_tabs(&[
                ("Store", true),
                ("Permissions", true),
                ("Tweaks", true),
                ("System", true),
            ]);
            self.screen = Some(screen);
            evas_object_event_callback_add(window, EVAS_CALLBACK_KEY_DOWN, on_key, ptr::null());

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
            evas_object_focus_set(self.window, 1);
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
        if self.tab == Tab::System
            && self
                .system_asked
                .is_none_or(|asked| asked.elapsed().as_secs_f64() >= SYSTEM_REFRESH)
        {
            self.system_asked = Some(Instant::now());
            let jobs = self.jobs_tx.clone();
            spawn(move || {
                let result = tv_proto::call(BLOCKD_SOCKET, &json!({"method": "system"}))
                    .map_err(|error| format!("Could not reach tv-blockd: {error}"))
                    .and_then(|bytes| {
                        serde_json::from_slice::<Value>(&bytes)
                            .map_err(|error| format!("tv-blockd sent bad JSON: {error}"))
                    });
                let _ = jobs.send(Job::System(result));
            });
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
        if let Some(screen) = self.screen.as_mut() {
            screen.set_status(&self.status);
        }
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
                    self.arrange();
                }
                // The order stays; buttons and badges follow the new state.
                return true;
            }
            Job::Icon => return self.tab == Tab::Store,
            Job::Status(message) => {
                self.set_status(message);
                return false;
            }
            Job::Progress(fraction) => {
                self.store.progress_value = fraction;
                return self.tab == Tab::Store && self.store.detail.is_some();
            }
            Job::Finished(result) => {
                self.store.busy = false;
                self.store.changing = None;
                self.show();
                self.set_status(result.unwrap_or_else(|error| error));
                log_line(&self.status);
                // The row's buttons change once the installed list is back.
                self.list_installed();
                return true;
            }
            Job::Audit(lines) => {
                self.audit = lines;
                return self.tab == Tab::Permissions;
            }
            Job::Tweaks(service, message, Ok(status)) => {
                let message = message.or_else(|| status["message"].as_str().map(str::to_owned));
                match service {
                    Service::Block => self.tweaks = Some(status),
                    Service::Patch => self.patch = Some(status),
                }
                if let Some(message) = message {
                    self.set_status(message);
                }
                // Re-rendered in place: focus and scroll position stay.
                return matches!(self.tab, Tab::Tweaks | Tab::System);
            }
            Job::System(Ok(response)) => {
                self.system = Some(response["system"].clone());
                return self.tab == Tab::System;
            }
            Job::System(Err(error)) => {
                log_line(&error);
                return false;
            }
            Job::Tweaks(_, _, Err(error)) => {
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
                        let _ = jobs.send(Job::Icon);
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
        self.render();
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

    /// Shows the current tab's page; focus stays on the same action.
    fn render(&mut self) {
        if self.screen.is_none() {
            return;
        }
        let keep = self.pending_focus.take().or_else(|| {
            self.screen
                .as_ref()
                .and_then(|screen| screen.focused().cloned())
        });
        let (title, blocks) = match (self.tab, self.store.detail) {
            (Tab::Store, Some(group)) => ("Store".to_owned(), self.details_page(group)),
            (Tab::Store, None) => ("Store".to_owned(), self.store_page()),
            (Tab::Permissions, _) => ("Permissions".to_owned(), self.permissions_page()),
            (Tab::Tweaks, _) => ("Tweaks".to_owned(), self.tweaks_page()),
            (Tab::System, _) => ("System".to_owned(), self.system_page()),
        };
        let status = self.status.clone();
        if let Some(screen) = self.screen.as_mut() {
            screen.set_status(&status);
            screen.set_page(self.tab as usize, &title, &blocks, keep.as_ref());
        }
    }

    /// The store's order for the current category and search: installed
    /// apps first, then by name. It is kept until the filter changes, so an
    /// install does not reshuffle the grid under the user.
    fn arrange(&mut self) {
        self.store.order = store::arrange(
            &self.store.groups,
            &self.store.versions(),
            CATEGORIES[self.store.category].0,
            &self.store.query,
        );
    }

    fn store_page(&mut self) -> Vec<Block<Action>> {
        let mut blocks = vec![
            Block::Toolbar {
                field: (
                    self.store.query.clone(),
                    "Search apps".into(),
                    Action::Search,
                ),
                buttons: vec![("Reload".into(), Action::Reload)],
            },
            Block::Chips(
                CATEGORIES
                    .iter()
                    .enumerate()
                    .map(|(index, (_, label))| Chip {
                        label: (*label).into(),
                        on: index == self.store.category,
                        action: Action::Category(index),
                    })
                    .collect(),
            ),
        ];
        if !self.store.loaded {
            return blocks;
        }
        if !self.store.arranged {
            return blocks;
        }
        let versions = self.store.versions();
        let mut icons = Vec::new();
        let mut cards = Vec::new();
        for &group_index in &self.store.order {
            let group = &self.store.groups[group_index];
            icons.extend(group.icon.clone());
            let installed = group.variants.iter().find_map(|app| {
                versions
                    .get(&app.package_id)
                    .map(|version| (app, version, store::state(app, &versions)))
            });
            let (subtitle, badge) = match installed {
                Some((_, _, State::Upgradable { .. })) => {
                    (String::new(), Some("Update available".into()))
                }
                Some((_, version, _)) => (format!("v{version} installed"), None),
                None if group.variants.len() > 1 => {
                    (format!("{} builds", group.variants.len()), None)
                }
                None => (
                    group
                        .variants
                        .first()
                        .map_or(String::new(), |app| format!("v{}", app.version)),
                    None,
                ),
            };
            cards.push(Card {
                icon: group
                    .icon
                    .as_ref()
                    .map(|(_, digest)| icon_path(digest).to_string_lossy().into_owned()),
                title: group.name.clone(),
                subtitle,
                badge,
                action: Action::Details(group_index),
            });
        }
        blocks.push(Block::Section(format!("{} apps", cards.len())));
        if cards.is_empty() {
            blocks.push(Block::Paragraph {
                markup: "No apps match.".into(),
                size: 26,
            });
        } else {
            blocks.push(Block::Grid(cards));
        }
        self.fetch_icons(icons);
        blocks
    }

    fn details_page(&mut self, group_index: usize) -> Vec<Block<Action>> {
        let group = self.store.groups[group_index].clone();
        let versions = self.store.versions();
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
        let mut ids: Vec<&str> = group
            .variants
            .iter()
            .map(|app| app.package_id.as_str())
            .collect();
        ids.dedup();
        info.push(format!("Package {}", ids.join(", ")));
        let mut repos: Vec<&str> = group
            .variants
            .iter()
            .map(|app| app.repo.as_str())
            .filter(|repo| !repo.is_empty())
            .collect();
        repos.sort_unstable();
        repos.dedup();
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
        let mut details = info
            .iter()
            .map(|line| format!("<muted>{}</muted>", escape(line)))
            .collect::<Vec<_>>()
            .join("<br/>");
        if !flags.is_empty() {
            details.push_str(&format!(
                "<br/><warn>{}</warn>",
                escape(&flags.join(" \u{00b7} "))
            ));
        }
        let rows = group
            .variants
            .iter()
            .enumerate()
            .map(|(build, app)| {
                let state = store::state(app, &versions);
                let mut facts = vec![
                    state_text(app, &state),
                    app.pkg_type.to_uppercase(),
                    format_size(app.size),
                ];
                if app.partner {
                    facts.push("partner".into());
                }
                let title = if app.variant.is_empty() {
                    &app.name
                } else {
                    &app.variant
                };
                let mut row = Row::text(title.clone(), facts.join(" \u{00b7} "));
                if self.store.changing.as_ref() == Some(&app.url) {
                    let (fraction, text) = match self.store.progress_value {
                        Some(fraction) => (
                            fraction.clamp(0.0, 1.0) as f32,
                            format!("Downloading {:.0}%", fraction.clamp(0.0, 1.0) * 100.0),
                        ),
                        None => (1.0, "Waiting for approval...".to_owned()),
                    };
                    row.controls.push(Control::Progress { fraction, text });
                } else {
                    let main = match state {
                        State::NotInstalled => ("Install", Action::Install(group_index, build)),
                        State::Upgradable { .. } => ("Update", Action::Install(group_index, build)),
                        State::Installed | State::Newer { .. } => {
                            ("Open", Action::Open(group_index, Some(build)))
                        }
                    };
                    row.controls.push(Control::Button {
                        label: main.0.into(),
                        tone: Tone::Primary,
                        action: main.1,
                    });
                    if !matches!(state, State::NotInstalled) {
                        row.controls.push(Control::Button {
                            label: "Remove".into(),
                            tone: Tone::Danger,
                            action: Action::Remove(group_index, build),
                        });
                    }
                }
                row
            })
            .collect();
        vec![
            Block::Header {
                icon: group
                    .icon
                    .as_ref()
                    .map(|(_, digest)| icon_path(digest).to_string_lossy().into_owned()),
                title: group.name.clone(),
                facts: facts.join(" \u{00b7} "),
            },
            Block::Paragraph {
                markup: escape(&group.description),
                size: 26,
            },
            Block::Paragraph {
                markup: details,
                size: 22,
            },
            Block::Section(if group.variants.len() == 1 {
                "Build".into()
            } else {
                "Builds".into()
            }),
            Block::Rows(rows),
        ]
    }

    fn permissions_page(&mut self) -> Vec<Block<Action>> {
        let mut blocks = Vec::new();
        if !self.connected {
            blocks.push(Block::Paragraph {
                markup: "<warn>Not connected to tv-authd. Retrying automatically.</warn>".into(),
                size: 24,
            });
        }
        blocks.push(Block::Section(
            "Apps that run commands as root without asking".into(),
        ));
        let rows = if self.grants.is_empty() {
            vec![Row::text(
                "No apps have permanent root access",
                "Apps you allow for good on a prompt appear here.",
            )]
        } else {
            self.grants
                .iter()
                .enumerate()
                .map(|(index, grant)| {
                    let mut row = Row::text(
                        grant.package.clone(),
                        format!(
                            "{} \u{00b7} {}",
                            grant.label,
                            notify_description(&grant.notify)
                        ),
                    );
                    row.controls = vec![
                        Control::Button {
                            label: notify_button_text(&grant.notify),
                            tone: Tone::Normal,
                            action: Action::Notify(index),
                        },
                        Control::Button {
                            label: "Revoke".into(),
                            tone: Tone::Danger,
                            action: Action::Revoke(index),
                        },
                    ];
                    row
                })
                .collect()
        };
        blocks.push(Block::Rows(rows));
        blocks.push(Block::Section("Recent activity".into()));
        let mut header = Row::text("Root decisions", "Newest first, from tv-authd's audit log");
        header.controls.push(Control::Button {
            label: "Refresh".into(),
            tone: Tone::Normal,
            action: Action::Refresh,
        });
        blocks.push(Block::Rows(vec![header]));
        let now = unix_now();
        let lines: Vec<String> = self
            .audit
            .iter()
            .rev()
            .take(AUDIT_SHOWN)
            .map(|line| audit_markup(line, now))
            .collect();
        blocks.push(Block::Paragraph {
            markup: if lines.is_empty() {
                "<muted>No recent decisions.</muted>".into()
            } else {
                lines.join("<br/>")
            },
            size: 22,
        });
        blocks
    }

    fn system_page(&mut self) -> Vec<Block<Action>> {
        match &self.tweaks {
            Some(status) => system_page(status, self.system.as_ref()),
            None => vec![Block::Paragraph {
                markup: "<muted>Asking tv-blockd...</muted>".into(),
                size: 24,
            }],
        }
    }

    fn tweaks_page(&mut self) -> Vec<Block<Action>> {
        match &self.tweaks {
            Some(status) => tweak_page(status, self.patch.as_ref()),
            None => vec![Block::Paragraph {
                markup: "<muted>Asking tv-blockd...</muted>".into(),
                size: 24,
            }],
        }
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.store.detail = None;
        self.status.clear();
        self.tab = tab;
        match tab {
            Tab::Permissions => self.refresh_grants(),
            Tab::Tweaks => {
                self.load_tweaks(Service::Block, None, None);
                self.load_tweaks(Service::Patch, None, None);
            }
            Tab::System => self.load_tweaks(Service::Block, None, None),
            Tab::Store => {}
        }
        self.render();
    }

    fn key(&mut self, key: &str) {
        let Some(screen) = self.screen.as_mut() else {
            return;
        };
        match screen.key(key) {
            ScreenEvent::None => {}
            ScreenEvent::Action(action) => self.act(action),
            ScreenEvent::Tab(index) => {
                let tab = [Tab::Store, Tab::Permissions, Tab::Tweaks, Tab::System]
                    .get(index)
                    .copied();
                if let Some(tab) = tab {
                    self.switch_tab(tab);
                }
            }
            ScreenEvent::Back { in_rail: true } => self.exit_now(),
            ScreenEvent::Back { in_rail: false } => {
                if self.tab == Tab::Store && self.store.detail.is_some() {
                    self.store.detail = None;
                    self.pending_focus = self.store.list_key.take().map(Action::Details);
                    self.render();
                } else if let Some(screen) = self.screen.as_mut() {
                    screen.focus_rail();
                }
            }
        }
    }

    /// The single app behind an action: a build, or a group's installed
    /// build.
    fn app_for(&self, group: usize, build: Option<usize>) -> Option<App> {
        let variants = &self.store.groups.get(group)?.variants;
        match build {
            Some(build) => variants.get(build).cloned(),
            None => variants
                .iter()
                .find(|app| self.store.installed.contains_key(&app.package_id))
                .cloned(),
        }
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Search => {
                let query = self.store.query.clone();
                if let Some(screen) = self.screen.as_mut() {
                    screen.start_field(&query, on_field_done);
                }
            }
            Action::Reload => {
                self.store.icons_requested.clear();
                self.load_catalog();
                self.render();
            }
            Action::Category(index) => {
                self.store.category = index.min(CATEGORIES.len() - 1);
                self.arrange();
                self.render();
            }
            Action::Details(group) => {
                self.store.list_key = Some(group);
                self.store.detail = Some(group);
                self.render();
            }
            Action::Install(group, build) => {
                if let Some(app) = self.app_for(group, Some(build)) {
                    log_line(&format!("install clicked {}", app.package_id));
                    self.install(app);
                }
            }
            Action::Open(group, build) => {
                if let Some(app) = self.app_for(group, build) {
                    self.open(&app);
                }
            }
            Action::Remove(group, build) => {
                if let Some(app) = self.app_for(group, Some(build)) {
                    log_line(&format!("remove clicked {}", app.package_id));
                    self.uninstall(app);
                }
            }
            Action::Notify(index) => {
                let Some((label, next)) = self
                    .grants
                    .get(index)
                    .map(|grant| (grant.label.clone(), next_notify(&grant.notify)))
                else {
                    return;
                };
                log_line(&format!("notify clicked {label} -> {next}"));
                if let Some(broker) = &self.broker {
                    broker.set_notify(&label, &next);
                }
            }
            Action::Revoke(index) => {
                let Some(label) = self.grants.get(index).map(|grant| grant.label.clone()) else {
                    return;
                };
                log_line(&format!("revoke clicked {label}"));
                self.set_status(format!("Revoking {label}..."));
                if let Some(broker) = &self.broker {
                    broker.revoke(&label);
                }
            }
            Action::Refresh => {
                self.set_status("Refreshing...");
                self.refresh_grants();
            }
            Action::Tweak(key) => self.apply_tweak(&key),
            Action::Undo => {
                self.set_status("Undoing...");
                self.load_tweaks(Service::Patch, Some(json!({"method": "undo"})), None);
                self.load_tweaks(
                    Service::Block,
                    Some(json!({"method": "undo"})),
                    Some("Everything the suite changed is undone."),
                );
            }
        }
    }

    fn apply_tweak(&mut self, key: &str) {
        match key {
            "patch:update" => {
                self.set_status("Checking for a TizenTube update...");
                self.load_tweaks(Service::Patch, Some(json!({"method": "update"})), None);
                return;
            }
            "patch:tizentube" | "patch:custom_home" => {
                let field = &key["patch:".len()..];
                // `set` replaces the whole config: keep the other settings.
                let mut config = self
                    .patch
                    .as_ref()
                    .map_or_else(|| json!({}), |patch| patch["config"].clone());
                let on = config[field] == true;
                config[field] = json!(!on);
                let message = match (field, on) {
                    ("tizentube", true) => "TizenTube is off; YouTube is back to normal.",
                    ("tizentube", false) => "TizenTube is on. Open YouTube to use it.",
                    (_, true) => "Custom Home is off; the Home key opens Samsung's Home.",
                    (_, false) => "Custom Home is on; press Home for the dock.",
                };
                self.set_status("Applying...");
                self.load_tweaks(
                    Service::Patch,
                    Some(json!({"method": "set", "config": config})),
                    Some(message),
                );
                return;
            }
            _ => {}
        }
        let Some(status) = &self.tweaks else { return };
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
                let on = config[key] == true;
                config[key] = json!(!on);
            }
        }
        self.set_status("Applying...");
        self.load_tweaks(
            Service::Block,
            Some(json!({"method": "set", "config": config})),
            Some("Saved and applied."),
        );
    }

    fn field_done(&mut self) {
        let Some(text) = self.screen.as_mut().and_then(Screen::finish_field) else {
            return;
        };
        if text != self.store.query {
            self.store.query = text;
            self.arrange();
        }
        self.render();
    }

    /// Sends a Tweaks daemon a request (`status` by default); `message` is
    /// shown once it succeeds.
    fn load_tweaks(
        &mut self,
        service: Service,
        request: Option<Value>,
        message: Option<&'static str>,
    ) {
        let jobs = self.jobs_tx.clone();
        spawn(move || {
            let name = service.name();
            let request = request.unwrap_or_else(|| json!({"method": "status"}));
            let result = tv_proto::call(service.socket(), &request)
                .map_err(|error| format!("Could not reach {name}: {error}"))
                .and_then(|bytes| {
                    serde_json::from_slice::<Value>(&bytes)
                        .map_err(|error| format!("{name} sent bad JSON: {error}"))
                })
                .and_then(|response| {
                    if response["status"] == "ok" {
                        Ok(response)
                    } else {
                        Err(format!(
                            "{name} refused: {}",
                            response["error"].as_str().unwrap_or("denied")
                        ))
                    }
                });
            let _ = jobs.send(Job::Tweaks(service, message.map(str::to_owned), result));
        });
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

/// The Tweaks page from tv-blockd's and tv-patchd's `status`.
fn tweak_page(status: &Value, patch: Option<&Value>) -> Vec<Block<Action>> {
    let config = &status["config"];
    let report = &status["report"];
    let switch = |key: &str, title: &str, subtitle: &str, on: bool| {
        let mut row = Row::text(title, subtitle);
        row.controls.push(Control::Switch {
            on,
            action: Action::Tweak(key.into()),
        });
        row
    };
    let hosts_on = config["hosts"] == true;
    let home = patch
        .and_then(|patch| patch["report"]["home"].as_str())
        .unwrap_or_default();
    let mut blocks = vec![Block::Tiles(vec![
        (
            if hosts_on {
                report["blocked_hosts"].to_string()
            } else {
                "Off".into()
            },
            "hosts blocked".into(),
        ),
        (
            match report["otn_autoupdate"].as_i64() {
                Some(0) => "Off".into(),
                Some(_) => "On".into(),
                None => "?".into(),
            },
            "auto-update".into(),
        ),
        (
            report["telemetry_active"]
                .as_array()
                .map_or(0, Vec::len)
                .to_string(),
            "telemetry services running".into(),
        ),
        (
            match home {
                "org.tvauth.home" => "Dock".into(),
                "com.samsung.tv.csfs" => "Samsung".into(),
                "" => "?".into(),
                other => other.into(),
            },
            "Home key".into(),
        ),
    ])];
    let errors: Vec<String> = [report, patch.map_or(&Value::Null, |patch| &patch["report"])]
        .iter()
        .filter_map(|report| {
            report["errors"]
                .as_array()
                .and_then(|errors| errors.first())
        })
        .filter_map(|error| error.as_str().map(str::to_owned))
        .collect();
    if !errors.is_empty() {
        blocks.push(Block::Paragraph {
            markup: format!("<warn>{}</warn>", escape(&errors.join(" \u{00b7} "))),
            size: 22,
        });
    }
    let enabled = config["groups"].as_array().cloned().unwrap_or_default();
    let mut hosts = switch(
        "hosts",
        "Block tracking hosts",
        "Points the groups below at nothing, for every app on the TV",
        hosts_on,
    );
    hosts.chips = status["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|group| {
            let id = group["id"].as_str().unwrap_or_default();
            Chip {
                label: group["label"].as_str().unwrap_or(id).to_owned(),
                on: enabled.iter().any(|value| value == id),
                action: Action::Tweak(format!("group:{id}")),
            }
        })
        .collect();
    hosts.dim = !hosts_on;
    blocks.push(Block::Section("Blocking".into()));
    blocks.push(Block::Rows(vec![hosts]));
    blocks.push(Block::Section("Updates and telemetry".into()));
    blocks.push(Block::Rows(vec![
        switch(
            "otn_lock",
            "Keep automatic updates off",
            "Switches the TV's auto-update setting back off whenever it is turned on",
            config["otn_lock"] == true,
        ),
        switch(
            "telemetry",
            "Stop telemetry services",
            "Samsung's log, crash and analytics services, again after every boot",
            config["telemetry"] == true,
        ),
    ]));
    let running = |key: &str| report["services"][key].as_array().map_or(0, Vec::len);
    blocks.push(Block::Section("Background services".into()));
    blocks.push(Block::Rows(vec![
        switch(
            "airplay",
            "Turn off the AirPlay receiver",
            &format!(
                "Frees memory; iPhones and Macs can no longer cast to the TV \u{00b7} {} running",
                running("airplay")
            ),
            config["airplay"] == true,
        ),
        switch(
            "multiview_preload",
            "Don't preload Multi View",
            &format!(
                "Frees memory; Multi View starts a little slower the first time \u{00b7} {} running",
                running("multiview_preload")
            ),
            config["multiview_preload"] == true,
        ),
    ]));
    blocks.push(Block::Section("Permanent changes".into()));
    blocks.push(Block::Paragraph {
        markup: "<warn>Permanent:</warn> these disable apps in the TV's app registry. They stay \
                 after a reboot and after TV suite is removed, until switched off here or with Undo."
            .into(),
        size: 22,
    });
    let missing = |key: &str| {
        report["missing"]
            .as_array()
            .is_some_and(|keys| keys.iter().any(|value| value == key))
    };
    let permanent = |key: &str, title: &str, what: &str| {
        let state = if missing(key) {
            "not on this TV"
        } else if report["disabled"][key]
            .as_array()
            .is_some_and(|apps| !apps.is_empty())
        {
            "disabled"
        } else {
            "running normally"
        };
        let mut row = switch(
            key,
            title,
            &format!("Permanent \u{00b7} {what} \u{00b7} {state}"),
            config[key] == true,
        );
        row.dim = missing(key);
        row
    };
    blocks.push(Block::Rows(vec![
        permanent(
            "disable_hbbtv",
            "Disable HbbTV",
            "red-button apps on broadcasts; may stop Samsung TV Plus",
        ),
        permanent(
            "disable_acr",
            "Disable ACR",
            "reports what is on screen to Samsung",
        ),
        permanent(
            "disable_ads",
            "Disable targeted ads",
            "Samsung's ad-targeting service",
        ),
    ]));
    if let Some(patch) = patch {
        let on = patch["config"]["tizentube"] == true;
        let version = patch["script"]["version"].as_str().unwrap_or("?");
        let state = if on && patch["report"]["ready"] == true {
            "on; its options are in YouTube's settings"
        } else if on {
            "starting"
        } else {
            "off; Samsung's YouTube as it was"
        };
        let mut update = Row::text(
            "Update TizenTube",
            format!(
                "Newest release from npm (this build pins {})",
                patch["pinned"].as_str().unwrap_or("?")
            ),
        );
        update.controls.push(Control::Button {
            label: "Update".into(),
            tone: Tone::Normal,
            action: Action::Tweak("patch:update".into()),
        });
        blocks.push(Block::Section("YouTube".into()));
        blocks.push(Block::Rows(vec![
            switch(
                "patch:tizentube",
                "TizenTube",
                &format!("Ad blocking and SponsorBlock in Samsung's YouTube \u{00b7} {version} \u{00b7} {state}"),
                on,
            ),
            update,
        ]));
        let custom = patch["config"]["custom_home"] == true;
        blocks.push(Block::Section("Home".into()));
        blocks.push(Block::Rows(vec![switch(
            "patch:custom_home",
            "Custom Home",
            "The Home key opens the dock over what is playing: apps, Up for inputs, Down for Settings",
            custom,
        )]));
    }
    let mut undo = Row::text(
        "Undo all changes",
        "Everything above and SDB back as they were, permanent changes included",
    );
    undo.controls.push(Control::Button {
        label: "Undo".into(),
        tone: Tone::Danger,
        action: Action::Undo,
    });
    blocks.push(Block::Section("Reset".into()));
    blocks.push(Block::Rows(vec![undo]));
    blocks
}

/// The System tab from tv-blockd's `status`.
/// The System tab from tv-blockd's `status` and `system` snapshot.
fn system_page(status: &Value, system: Option<&Value>) -> Vec<Block<Action>> {
    let config = &status["config"];
    let mb = |kb: &Value| format!("{} MB", (kb.as_u64().unwrap_or(0) + 512) / 1024);
    let mut blocks = Vec::new();
    let options = system.map_or(&Value::Null, |system| &system["options"]);
    let usage = |key: &str| {
        let option = &options[key];
        if option["processes"].as_u64().unwrap_or(0) > 0 {
            format!(
                "{} running: {} + {} swap, {}% CPU",
                option["processes"],
                mb(&option["rss_kb"]),
                mb(&option["swap_kb"]),
                option["cpu"]
            )
        } else if option["freed"].is_object() {
            let freed = &option["freed"];
            format!(
                "stopped; freed {} + {} swap {}",
                mb(&freed["rss_kb"]),
                mb(&freed["swap_kb"]),
                ago(freed["at"].as_i64().unwrap_or(0))
            )
        } else if system.is_some() && config[key] == true {
            "stopped (not measured yet)".into()
        } else if system.is_some() {
            "nothing running".into()
        } else {
            "measuring...".into()
        }
    };
    match system {
        Some(system) => {
            let used = |total: &str, free: &str| {
                system[total]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_sub(system[free].as_u64().unwrap_or(0))
            };
            let gb = |kb: u64| format!("{:.1}", kb as f64 / 1_048_576.0);
            let kbs = |key: &str| format!("{:.0}", system[key].as_f64().unwrap_or(0.0));
            blocks.push(Block::Tiles(vec![
                (
                    format!("{}%", system["cpu"]),
                    format!(
                        "CPU \u{00b7} load {}",
                        system["load"].as_str().unwrap_or("?")
                    ),
                ),
                (
                    format!(
                        "{} / {} GB",
                        gb(used("mem_total_kb", "mem_available_kb")),
                        gb(system["mem_total_kb"].as_u64().unwrap_or(0))
                    ),
                    "memory in use".into(),
                ),
                (
                    format!(
                        "{} / {} MB",
                        used("swap_total_kb", "swap_free_kb") / 1024,
                        system["swap_total_kb"].as_u64().unwrap_or(0) / 1024
                    ),
                    "swap in use".into(),
                ),
                (
                    system["gpu"]
                        .as_u64()
                        .map_or("?".into(), |gpu| format!("{gpu}%")),
                    "GPU".into(),
                ),
            ]));
            blocks.push(Block::Paragraph {
                markup: format!(
                    "<muted>{} cores \u{00b7} storage {} KB/s read, {} KB/s written \u{00b7} swap {} KB/s in, {} KB/s out \u{00b7} {} processes</muted>",
                    system["cores"],
                    kbs("disk_read_kbs"),
                    kbs("disk_write_kbs"),
                    kbs("swap_in_kbs"),
                    kbs("swap_out_kbs"),
                    system["processes"]
                ),
                size: 22,
            });
        }
        None => blocks.push(Block::Paragraph {
            markup: "<muted>Measuring...</muted>".into(),
            size: 24,
        }),
    }
    let switch = |key: &str, title: &str| {
        let mut row = Row::text(title, usage(key));
        row.controls.push(Control::Switch {
            on: config[key] == true,
            action: Action::Tweak(key.into()),
        });
        row
    };
    blocks.push(Block::Section("What the tweaks save".into()));
    blocks.push(Block::Rows(vec![
        switch("telemetry", "Stop telemetry services"),
        switch("airplay", "Turn off the AirPlay receiver"),
        switch("multiview_preload", "Don't preload Multi View"),
        switch("disable_hbbtv", "Disable HbbTV (permanent)"),
        switch("disable_acr", "Disable ACR (permanent)"),
        switch("disable_ads", "Disable targeted ads (permanent)"),
    ]));
    if let Some(system) = system {
        let list = |key: &str| {
            let lines: Vec<String> = system[key]
                .as_array()
                .into_iter()
                .flatten()
                .map(|process| {
                    format!(
                        "<b>{}</b>  <muted>{}% CPU \u{00b7} {} + {} swap \u{00b7} pid {}</muted>",
                        escape(&shorten(process["name"].as_str().unwrap_or("?"), 48, false)),
                        process["cpu"],
                        mb(&process["rss_kb"]),
                        mb(&process["swap_kb"]),
                        process["pid"]
                    )
                })
                .collect();
            Block::Paragraph {
                markup: lines.join("<br/>"),
                size: 22,
            }
        };
        blocks.push(Block::Section("Busiest now".into()));
        blocks.push(list("top_cpu"));
        blocks.push(Block::Section("Largest in memory".into()));
        blocks.push(list("top_memory"));
    }
    let mut sdb = Row::text(
        "SDB",
        format!(
            "Samsung's developer bridge, port 26101 \u{00b7} {}",
            usage("sdb_off")
        ),
    );
    sdb.controls.push(Control::Switch {
        on: config["sdb_off"] != true,
        action: Action::Tweak("sdb_off".into()),
    });
    blocks.push(Block::Section("Developer access".into()));
    blocks.push(Block::Rows(vec![sdb]));
    blocks.push(Block::Paragraph {
        markup: "<muted>SDB runs only while Samsung's developer mode is on. Off lasts until \
                 you switch it back on here; a reboot without TV suite turns it back on.</muted>"
            .into(),
        size: 22,
    });
    blocks
}

/// "3 min ago" for a unix time.
fn ago(at: i64) -> String {
    match (unix_now() - at).max(0) {
        0..60 => "just now".into(),
        seconds @ 60..3600 => format!("{} min ago", seconds / 60),
        seconds @ 3600..86_400 => format!("{} h ago", seconds / 3600),
        seconds => format!("{} d ago", seconds / 86_400),
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

unsafe extern "C" fn on_delete(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    ui().exit_now();
}

unsafe extern "C" fn on_key(
    _data: *mut c_void,
    _evas: *mut c_void,
    _object: *mut c_void,
    info: *mut c_void,
) {
    // `Evas_Event_Key_Down` starts with `const char *keyname`.
    let name = *(info as *const *const c_char);
    if !name.is_null() {
        ui().key(&std::ffi::CStr::from_ptr(name).to_string_lossy());
    }
}

unsafe extern "C" fn on_frame(_data: *mut c_void) -> EinaBool {
    match ui().screen.as_mut() {
        Some(screen) => EinaBool::from(screen.frame()),
        None => 0,
    }
}

unsafe extern "C" fn on_field_done(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    ui().field_done();
}

fn main() {
    tv_efl::log::set_path("/tmp/tvauth-ui.log");
    // One executable, two apps: AUL starts the dock under its own app id.
    let status = if std::env::var("AUL_APPID").as_deref() == Ok(dock::APP_ID) {
        run_dock()
    } else {
        run()
    };
    if status != 0 {
        std::process::exit(status);
    }
}

fn run_dock() -> i32 {
    let arguments: Vec<CString> = std::env::args_os()
        .filter_map(|argument| CString::new(argument.as_encoded_bytes()).ok())
        .collect();
    let mut argv: Vec<*mut c_char> = arguments
        .iter()
        .map(|argument| argument.as_ptr() as *mut c_char)
        .collect();
    argv.push(ptr::null_mut());
    dock::run(argv)
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
