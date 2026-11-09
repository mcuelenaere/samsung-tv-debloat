//! Native Tizen `type="capp"` approval and grant-manager UI.
//!
//! This is a direct port of the managed `.NET` implementation.  The daemon
//! stages the executable at `/run/tv-auth/ui-bin/tv-auth-ui` (see
//! `daemon/src/ui_alias.rs`) so launchpad can map it despite UEP.

use tv_efl as efl;

use efl::*;
use std::cell::Cell;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use tv_auth_ui::client::{BrokerClient, Event};
use tv_auth_ui::log::log_line;
use tv_auth_ui::view::*;
use tv_proto::ui::{GrantInfo, NoticeInfo, PromptInfo};

const COLOR_CARD: i32 = 0x1c_1f_26;
const COLOR_INSET: i32 = 0x0e_10_14;
const COLOR_ROW: i32 = 0x27_2b_34;
const COLOR_DANGER: i32 = 0xe5_48_4d;
const COLOR_ACCENT: i32 = 0x3e_7b_fa;
const CARD_WIDTH: i32 = 1200;
const CARD_PADDING: i32 = 56;
const MAX_PRIVILEGES_SHOWN: usize = 7;

const TOAST_WIDTH: i32 = 640;
const TOAST_PADDING: i32 = 28;
const TOAST_MARGIN: i32 = 48;
const TOAST_SHOWN_SECONDS: u64 = 6;
const TOAST_MAX_APPS: usize = 3;
const TOAST_COMMAND_CHARS: usize = 52;
/// Keep the UI connected invisibly after a toast so a burst of notices does not
/// relaunch the app. The native cold start is short enough that a long linger
/// (the `.NET` UI used 300 s) is no longer worth the resident memory. Must stay
/// below the daemon's 600 s UI idle timeout.
const IDLE_LINGER_SECONDS: u64 = 60;

const APP_ID: &[u8] = b"org.tvauth.ui\0";
const TOAST_NAME: &[u8] = b"org.tvauth.ui.toast\0";
const DELETE_REQUEST: &[u8] = b"delete,request\0";
const CLICKED: &[u8] = b"clicked\0";
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

struct Ui {
    window: *mut c_void,
    accent_bar: *mut c_void,
    countdown_label: *mut c_void,
    last_countdown: i32,
    header_label: *mut c_void,
    status_label: *mut c_void,
    content_box: *mut c_void,
    content_objects: Vec<*mut c_void>,

    broker: Option<BrokerClient>,
    tx: Sender<Event>,
    rx: Receiver<Event>,

    current_prompt: Option<PromptInfo>,
    grants: Vec<GrantInfo>,
    manager_message: Option<String>,
    prompt_expires_at: Option<Instant>,
    exit_at: Option<Instant>,
    exiting: bool,
    back_grabbed: bool,
    connected: bool,
    screen_w: i32,
    screen_h: i32,
    main_shown: bool,
    hello_seen: bool,
    app_control_count: u32,
    exit_when_idle: bool,
    linger_until: Option<Instant>,

    toast_window: *mut c_void,
    toast_label: *mut c_void,
    toast_hide_at: Option<Instant>,
    toast_rows: usize,
    focus_notify_label: Option<String>,
    toast_entries: Vec<NoticeInfo>,
}

impl Ui {
    fn new(tx: Sender<Event>, rx: Receiver<Event>) -> Self {
        Self {
            window: ptr::null_mut(),
            accent_bar: ptr::null_mut(),
            countdown_label: ptr::null_mut(),
            last_countdown: -1,
            header_label: ptr::null_mut(),
            status_label: ptr::null_mut(),
            content_box: ptr::null_mut(),
            content_objects: Vec::new(),
            broker: None,
            tx,
            rx,
            current_prompt: None,
            grants: Vec::new(),
            manager_message: None,
            prompt_expires_at: None,
            exit_at: None,
            exiting: false,
            back_grabbed: false,
            connected: false,
            screen_w: 1920,
            screen_h: 1080,
            main_shown: false,
            hello_seen: false,
            app_control_count: 0,
            exit_when_idle: false,
            linger_until: None,
            toast_window: ptr::null_mut(),
            toast_label: ptr::null_mut(),
            toast_hide_at: None,
            toast_rows: 0,
            focus_notify_label: None,
            toast_entries: Vec::new(),
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
                log_line("set Wayland notification type");
            }

            set_window_title(window, "Root access");
            elm_win_modal_set(window, 1);
            elm_win_floating_mode_set(window, 1);
            set_highest_notification_level(window);

            evas_object_smart_callback_add(
                window,
                DELETE_REQUEST.as_ptr().cast(),
                on_back,
                ptr::null_mut(),
            );
            eext_object_event_callback_add(window, EEXT_CALLBACK_BACK, on_back, ptr::null_mut());

            // Full-screen dimmed scrim with a solid, centered card. The stock
            // elm_bg is transparent in the TV theme, so the home screen would
            // otherwise show through the prompt.
            elm_win_alpha_set(window, 1);
            let scrim = create_rect(window, 0x000000, 140);
            evas_object_size_hint_weight_set(scrim, EVAS_HINT_EXPAND, EVAS_HINT_EXPAND);
            elm_win_resize_object_add(window, scrim);

            let layout = elm_table_add(window);
            if layout.is_null() {
                return false;
            }
            evas_object_size_hint_weight_set(layout, 1.0, 1.0);
            evas_object_size_hint_align_set(layout, EVAS_HINT_FILL, EVAS_HINT_FILL);
            elm_win_resize_object_add(window, layout);
            evas_object_show(layout);

            let (card, card_box) = panel(layout, COLOR_CARD, CARD_PADDING, CARD_PADDING);
            evas_object_size_hint_weight_set(card, 1.0, 1.0);
            evas_object_size_hint_align_set(card, 0.5, 0.5);
            elm_table_pack(layout, card, 0, 0, 1, 1);
            elm_box_padding_set(card_box, 0, 18);

            self.accent_bar = create_rect(card, COLOR_ACCENT, 255);
            evas_object_size_hint_min_set(self.accent_bar, CARD_WIDTH, 8);
            evas_object_size_hint_weight_set(self.accent_bar, 1.0, 1.0);
            evas_object_size_hint_align_set(self.accent_bar, EVAS_HINT_FILL, 0.0);
            elm_table_pack(card, self.accent_bar, 0, 0, 1, 1);

            self.header_label = create_label(card_box, "");
            elm_box_pack_end(card_box, self.header_label);
            self.status_label = create_label(card_box, "");
            elm_box_pack_end(card_box, self.status_label);
            self.set_header("TV-AUTHD", "Root access manager", COLOR_ACCENT);
            self.set_status("Connecting to tv-authd...");

            self.content_box = elm_box_add(card_box);
            evas_object_size_hint_weight_set(self.content_box, 1.0, 1.0);
            evas_object_size_hint_align_set(self.content_box, EVAS_HINT_FILL, EVAS_HINT_FILL);
            elm_box_padding_set(self.content_box, 0, 24);
            elm_box_pack_end(card_box, self.content_box);
            evas_object_show(self.content_box);

            self.render_manager();

            ecore_timer_add(0.1, on_timer, ptr::null_mut());

            self.broker = Some(BrokerClient::start(self.tx.clone()));

            let mut width = 0;
            let mut height = 0;
            elm_win_screen_size_get(
                window,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut width,
                &mut height,
            );
            if width > 0 && height > 0 {
                self.screen_w = width;
                self.screen_h = height;
            }
            log_line(&format!("screen {}x{}", self.screen_w, self.screen_h));
            evas_object_resize(window, self.screen_w, self.screen_h);
            elm_win_center(window, 1, 1);
        }
        true
    }

    fn show_main(&mut self) {
        self.hide_toast();
        self.exit_when_idle = false;
        self.linger_until = None;
        if self.main_shown || self.window.is_null() {
            return;
        }
        self.main_shown = true;
        if self.current_prompt.is_none() {
            log_line("manager grants refresh");
            if let Some(broker) = &self.broker {
                broker.request_grants();
            }
        }
        unsafe {
            self.back_grabbed = eext_win_keygrab_set(self.window, BACK_KEY.as_ptr().cast()) == 1;
            if !self.back_grabbed {
                log_line("XF86Back keygrab failed");
            }
            evas_object_show(self.window);
            elm_win_raise(self.window);
            elm_win_activate(self.window);
        }
        log_line("window shown/raised/activated");
    }

    fn hide_main(&mut self) {
        if !self.main_shown {
            return;
        }
        self.main_shown = false;
        unsafe {
            if self.back_grabbed {
                eext_win_keygrab_unset(self.window, BACK_KEY.as_ptr().cast());
            }
            evas_object_hide(self.window);
        }
        self.back_grabbed = false;
        log_line("window hidden");
    }

    fn terminate(&mut self) {
        if let Some(broker) = self.broker.take() {
            broker.stop();
        }
        if self.window.is_null() {
            return;
        }
        unsafe {
            if self.back_grabbed {
                eext_win_keygrab_unset(self.window, BACK_KEY.as_ptr().cast());
            }
            evas_object_del(self.window);
        }
        self.window = ptr::null_mut();
        if !self.toast_window.is_null() {
            unsafe { evas_object_del(self.toast_window) };
        }
        self.toast_window = ptr::null_mut();
    }

    fn on_timer(&mut self) -> EinaBool {
        while let Ok(event) = self.rx.try_recv() {
            self.handle_event(event);
        }

        let now = Instant::now();
        if let Some(expires) = self.prompt_expires_at {
            if self.current_prompt.is_some() {
                let remaining = expires.saturating_duration_since(now).as_secs_f64().ceil() as i32;
                if remaining != self.last_countdown && remaining >= 0 {
                    self.last_countdown = remaining;
                    if !self.countdown_label.is_null() {
                        set_text(
                            self.countdown_label,
                            &small(&format!("Denied automatically in {remaining} s"), "#8a8f99"),
                        );
                    }
                }
            }
            if now >= expires {
                log_line("prompt expired");
                self.current_prompt = None;
                self.prompt_expires_at = None;
                self.manager_message = Some("The request timed out and was denied.".into());
                self.render_manager();
                self.exit_at = Some(now + Duration::from_secs(2));
            }
        }

        if let Some(exit_at) = self.exit_at {
            if now >= exit_at {
                self.exit_at = None;
                self.hide_main();
                self.linger_until = None;
                self.exit_when_idle = true;
            }
        }

        if let Some(hide_at) = self.toast_hide_at {
            if now >= hide_at {
                self.hide_toast();
                if !self.main_shown {
                    self.exit_when_idle = true;
                    self.linger_until = Some(now + Duration::from_secs(IDLE_LINGER_SECONDS));
                }
            }
        }

        if self.exit_when_idle
            && !self.main_shown
            && self.toast_hide_at.is_none()
            && self.linger_until.is_none_or(|until| now >= until)
        {
            self.exit_now();
            return 0;
        }
        1
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Prompt(prompt) => self.on_prompt(prompt),
            Event::Notice(notices) => self.on_notice(notices),
            Event::HelloAccepted(launch) => self.on_hello_accepted(&launch),
            Event::Grants(grants) => self.on_grants(grants),
            Event::DecisionFinished(ok, error) => self.on_decision_finished(ok, &error),
            Event::RevokeFinished(revoked, error) => self.on_revoke_finished(revoked, &error),
            Event::NotifyFinished(ok, error) => self.on_notify_finished(ok, &error),
            Event::ConnectionChanged(connected) => self.on_connection_changed(connected),
            Event::StatusMessage(message) => self.on_status_message(&message),
        }
    }

    fn on_prompt(&mut self, prompt: PromptInfo) {
        if self.current_prompt.is_some() {
            // The daemon normally keeps one prompt active. Deny a stale one
            // rather than silently dropping it.
            self.send_decision("deny");
        }
        self.current_prompt = Some(prompt);
        self.exit_at = None;
        self.show_main();
        let timeout = self
            .current_prompt
            .as_ref()
            .map(|prompt| prompt.timeout_ms)
            .unwrap_or(0);
        self.prompt_expires_at = Some(
            Instant::now() + Duration::from_millis(if timeout > 0 { timeout } else { 60_000 }),
        );
        if let Some(prompt) = &self.current_prompt {
            log_line(&format!(
                "prompt received package={} request={} kind={}",
                prompt.package, prompt.request_id, prompt.kind
            ));
            if prompt.is_package_operation() {
                self.render_package_prompt();
            } else {
                self.render_prompt();
            }
        }
    }

    fn on_hello_accepted(&mut self, launch: &str) {
        if !self.hello_seen {
            self.hello_seen = true;
            if launch == "notice" && !self.main_shown && self.app_control_count <= 1 {
                log_line("notice-only launch");
                self.exit_when_idle = true;
                self.linger_until = Some(Instant::now() + Duration::from_secs(IDLE_LINGER_SECONDS));
                return;
            }
            self.show_main();
            return;
        }

        if self.main_shown && self.current_prompt.is_none() {
            log_line("manager grants refresh");
            if let Some(broker) = &self.broker {
                broker.request_grants();
            }
        }
    }

    fn on_notice(&mut self, notices: Vec<NoticeInfo>) {
        for notice in notices {
            log_line(&format!(
                "notice package={} count={}",
                notice.package, notice.count
            ));
            if self.main_shown {
                continue;
            }
            if let Some(entry) = self
                .toast_entries
                .iter_mut()
                .find(|entry| entry.label == notice.label && entry.kind == notice.kind)
            {
                entry.count = entry.count.saturating_add(notice.count);
                entry.command = notice.command;
                entry.command_truncated = notice.command_truncated;
            } else {
                self.toast_entries.push(notice);
            }
        }
        if self.main_shown || self.toast_entries.is_empty() {
            return;
        }
        self.show_toast();
    }

    fn show_toast(&mut self) {
        if self.toast_window.is_null() && !self.create_toast_window() {
            return;
        }
        set_text(self.toast_label, &self.toast_markup());
        let rows = self.toast_entries.len().min(TOAST_MAX_APPS + 1);
        if self.toast_hide_at.is_none() || rows != self.toast_rows {
            self.toast_rows = rows;
            unsafe {
                evas_object_resize(self.toast_window, TOAST_WIDTH + 2 * TOAST_PADDING, 1);
                evas_object_move(
                    self.toast_window,
                    self.screen_w - TOAST_WIDTH - 2 * TOAST_PADDING - TOAST_MARGIN,
                    TOAST_MARGIN,
                );
                evas_object_show(self.toast_window);
            }
        }
        self.toast_hide_at = Some(Instant::now() + Duration::from_secs(TOAST_SHOWN_SECONDS));
    }

    fn create_toast_window(&mut self) -> bool {
        unsafe {
            let window = elm_win_add(
                ptr::null_mut(),
                TOAST_NAME.as_ptr().cast(),
                ELM_WIN_NOTIFICATION,
            );
            if window.is_null() {
                log_line("toast elm_win_add failed");
                return false;
            }
            self.toast_window = window;
            let wayland_window = elm_win_wl_window_get(window);
            if !wayland_window.is_null() {
                ecore_wl2_window_type_set(wayland_window, ECORE_WL2_WINDOW_TYPE_NOTIFICATION);
            }
            // Never take focus or keys from the app the user is watching.
            elm_win_prop_focus_skip_set(window, 1);
            set_highest_notification_level(window);

            let hint = efl::c_string("wm.policy.win.user.geometry");
            let one = efl::c_string("1");
            elm_win_aux_hint_add(window, hint.as_ptr(), one.as_ptr());

            let (card, box_) = panel(window, COLOR_CARD, TOAST_PADDING, TOAST_PADDING - 6);
            elm_win_resize_object_add(window, card);
            let bar = create_rect(box_, COLOR_DANGER, 255);
            evas_object_size_hint_min_set(bar, TOAST_WIDTH, 6);
            elm_box_pack_end(box_, bar);
            evas_object_show(bar);
            self.toast_label = create_label(box_, "");
            elm_box_pack_end(box_, self.toast_label);
            elm_label_line_wrap_set(self.toast_label, 0);
        }
        true
    }

    fn hide_toast(&mut self) {
        self.toast_hide_at = None;
        self.toast_entries.clear();
        self.toast_rows = 0;
        if !self.toast_window.is_null() {
            unsafe { evas_object_hide(self.toast_window) };
        }
    }

    fn toast_markup(&self) -> String {
        let package_change = self.toast_entries.iter().any(|entry| entry.kind != "exec");
        let mut builder = String::new();
        if package_change {
            builder
                .push_str("<font_size=18><b><color=#e5484d>PACKAGE CHANGE</color></b></font_size>");
        } else {
            builder
                .push_str("<font_size=18><b><color=#e5484d>ROOT COMMAND</color></b></font_size>");
        }
        for entry in self.toast_entries.iter().take(TOAST_MAX_APPS) {
            let what = if entry.kind == "install" {
                if entry.count == 1 {
                    " installed a package".to_owned()
                } else {
                    format!(" installed {} packages", entry.count)
                }
            } else if entry.kind == "uninstall" {
                if entry.count == 1 {
                    " removed a package".to_owned()
                } else {
                    format!(" removed {} packages", entry.count)
                }
            } else if entry.count == 1 {
                " ran a command as root".to_owned()
            } else {
                format!(" ran {} commands as root", entry.count)
            };
            builder.push_str("<br/><font_size=26><b>");
            builder.push_str(&escape(&shorten(&entry.package, 36, false)));
            builder.push_str("</b>");
            builder.push_str(&what);
            builder.push_str("</font_size><br/>");
            builder.push_str(&small(
                &format!(
                    "$ {}",
                    shorten(&entry.command, TOAST_COMMAND_CHARS, entry.command_truncated)
                ),
                "#8a8f99",
            ));
        }
        if self.toast_entries.len() > TOAST_MAX_APPS {
            let more = self.toast_entries.len() - TOAST_MAX_APPS;
            builder.push_str("<br/>");
            builder.push_str(&small(
                &format!("+ {more} more app{}", if more == 1 { "" } else { "s" }),
                "#8a8f99",
            ));
        }
        builder
    }

    fn on_grants(&mut self, grants: Vec<GrantInfo>) {
        self.grants = grants;
        if self.manager_message.as_deref() == Some("Refreshing...") {
            self.manager_message = None;
        }
        if self.current_prompt.is_none() && self.exit_at.is_none() {
            self.render_manager();
        }
    }

    fn on_decision_finished(&mut self, ok: bool, error: &str) {
        if self.current_prompt.is_some() {
            return;
        }
        if ok {
            self.set_status("Closing...");
            self.exit_at = Some(Instant::now() + Duration::from_millis(1200));
        } else {
            let detail = match error {
                "package_identity_unavailable" => {
                    "the installed package identity could not be verified; choose Allow once or Deny"
                }
                "grant_persist_failed" => {
                    "the grant could not be saved; choose Allow once or Deny"
                }
                "" => "unknown",
                other => other,
            };
            self.set_header("ERROR", "Decision failed", COLOR_DANGER);
            self.set_status(&format!(
                "tv-authd reported: {detail}. The request was not approved."
            ));
            self.exit_at = Some(Instant::now() + Duration::from_millis(2500));
        }
    }

    fn on_notify_finished(&mut self, ok: bool, error: &str) {
        if ok {
            // The daemon writes the new mode; refresh so the button reflects it.
            if let Some(broker) = &self.broker {
                broker.request_grants();
            }
            return;
        }
        if self.current_prompt.is_some() {
            return;
        }
        self.manager_message = Some(format!(
            "Could not change toast setting: {}",
            if error.is_empty() { "unknown" } else { error }
        ));
        self.render_manager();
    }

    fn on_revoke_finished(&mut self, revoked: bool, error: &str) {
        if self.current_prompt.is_some() {
            return;
        }
        self.manager_message = Some(if revoked {
            "Grant revoked.".to_owned()
        } else if error == "not_found" {
            "That grant was already removed.".to_owned()
        } else {
            format!(
                "Revoke failed: {}",
                if error.is_empty() { "unknown" } else { error }
            )
        });
        self.render_manager();
        if revoked {
            if let Some(broker) = &self.broker {
                broker.request_grants();
            }
        }
    }

    fn on_connection_changed(&mut self, connected: bool) {
        self.connected = connected;
        if !connected && !self.hello_seen {
            self.show_main();
        }
        if !connected {
            if self.current_prompt.is_some() {
                self.current_prompt = None;
                self.prompt_expires_at = None;
                self.manager_message =
                    Some("Connection lost. The pending request was cancelled.".into());
            } else if self
                .manager_message
                .as_deref()
                .is_none_or(|message| message.is_empty())
            {
                self.manager_message = Some("Connection lost. Retrying...".into());
            }
        } else if self.manager_message.as_deref() == Some("Connection lost. Retrying...") {
            self.manager_message = None;
        }

        if self.current_prompt.is_none() && self.exit_at.is_none() {
            self.render_manager();
        }
    }

    fn on_status_message(&mut self, message: &str) {
        if self.current_prompt.is_none() && self.exit_at.is_none() {
            self.manager_message = Some(message.to_owned());
            self.render_manager();
        }
    }

    fn send_decision(&mut self, decision: &str) {
        let Some(prompt) = self.current_prompt.take() else {
            return;
        };
        self.prompt_expires_at = None;
        self.exit_at = Some(Instant::now() + Duration::from_secs(5));
        self.clear_content();
        if prompt.is_package_operation() {
            let removing = prompt.kind == "uninstall";
            if decision == "deny" {
                self.set_header(
                    if removing {
                        "REMOVE PACKAGE"
                    } else {
                        "INSTALL PACKAGE"
                    },
                    "Denied",
                    COLOR_DANGER,
                );
                self.set_status("The package was not changed.");
            } else {
                self.set_header(
                    if removing {
                        "REMOVE PACKAGE"
                    } else {
                        "INSTALL PACKAGE"
                    },
                    "Approved",
                    COLOR_ACCENT,
                );
                self.set_status(if removing {
                    "tv-authd is removing the package..."
                } else {
                    "tv-authd is installing the package..."
                });
            }
        } else {
            match decision {
                "once" => self.set_header("ROOT ACCESS", "Allowed once", COLOR_ACCENT),
                "always" => self.set_header("ROOT ACCESS", "Always allowed", COLOR_ACCENT),
                _ => self.set_header("ROOT ACCESS", "Denied", COLOR_DANGER),
            }
            self.set_status("Sending decision...");
        }
        if let Some(broker) = &self.broker {
            broker.decide(&prompt, decision);
        }
        log_line(&format!(
            "decision sent package={} kind={} decision={decision}",
            prompt.package, prompt.kind
        ));
    }

    fn render_prompt(&mut self) {
        let Some(prompt) = self.current_prompt.clone() else {
            return;
        };
        self.clear_content();
        self.set_header(
            "ROOT ACCESS REQUEST",
            "An app wants full root access",
            COLOR_DANGER,
        );
        self.set_status(
            "Root can read, change or erase anything on this TV, including other apps' data \
             and system files. Only allow apps you trust.",
        );

        let (app, app_box) = panel(self.content_box, COLOR_ROW, 32, 24);
        self.add_content(app);
        add_to_box(
            app_box,
            create_label(app_box, &small("APPLICATION", "#8a8f99")),
        );
        add_to_box(
            app_box,
            create_label(
                app_box,
                &format!(
                    "<font_size=34><b>{}</b></font_size>",
                    escape(&prompt.package)
                ),
            ),
        );
        add_to_box(
            app_box,
            create_label(app_box, &small(&prompt.label, "#8a8f99")),
        );

        self.add_content(create_label(self.content_box, &small("COMMAND", "#8a8f99")));
        let (command_panel, command_box) = panel(self.content_box, COLOR_INSET, 28, 20);
        self.add_content(command_panel);
        add_to_box(
            command_box,
            create_label(
                command_box,
                &format!(
                    "<font=Monospace><font_size=22><color=#e8e8e8>{}</color></font_size></font>",
                    command_markup(&prompt.command, prompt.command_truncated)
                ),
            ),
        );

        self.add_content(create_label(
            self.content_box,
            &small_raw(
                "<b>Allow once</b> runs only this command. <b>Always allow</b> lets this app run \
                 any command as root without asking again; revoke it later in Root access \
                 manager.",
                "#b4b9c2",
            ),
        ));

        let footer = create_row(self.content_box);
        self.add_content(footer);
        self.last_countdown = -1;
        self.countdown_label = create_label(footer, "");
        unsafe { elm_box_pack_end(footer, self.countdown_label) };
        let deny = create_button(
            footer,
            "Deny",
            on_decision_clicked,
            std::ptr::without_provenance_mut(1),
        );
        create_button(
            footer,
            "Allow once",
            on_decision_clicked,
            std::ptr::without_provenance_mut(2),
        );
        create_button(
            footer,
            "Always allow",
            on_decision_clicked,
            std::ptr::without_provenance_mut(3),
        );
        unsafe { elm_object_focus_set(deny, 1) };
    }

    fn render_package_prompt(&mut self) {
        let Some(prompt) = self.current_prompt.clone() else {
            return;
        };
        let removing = prompt.kind == "uninstall";
        let info = prompt.install.as_ref();
        let target = prompt.uninstall.as_ref();
        self.clear_content();
        if (!removing && info.is_none()) || (removing && target.is_none()) {
            // Fail closed: never offer an approval button for a package
            // request the daemon could not describe.
            self.set_header(
                if removing {
                    "REMOVE PACKAGE"
                } else {
                    "INSTALL PACKAGE"
                },
                "Malformed package request",
                COLOR_DANGER,
            );
            self.set_status(
                "tv-authd did not describe the package. This request cannot be approved.",
            );
            let row = create_row(self.content_box);
            self.add_content(row);
            let reject = create_button(
                row,
                "Deny",
                on_decision_clicked,
                std::ptr::without_provenance_mut(1),
            );
            unsafe { elm_object_focus_set(reject, 1) };
            return;
        }

        self.set_header(
            if removing {
                "REMOVE PACKAGE"
            } else {
                "INSTALL PACKAGE"
            },
            if removing {
                "An app wants to remove a package"
            } else {
                "An app wants to install a package"
            },
            if removing { COLOR_DANGER } else { COLOR_ACCENT },
        );
        if removing {
            self.set_status("The package and its data will be removed from this TV.");
        } else if info.is_some_and(|info| info.is_upgrade) {
            self.set_status(
                "This replaces the installed version. Check the publisher before continuing.",
            );
        } else {
            self.set_status(
                "Only install packages you trust. Apps can change how this TV behaves and \
                 reach your data.",
            );
        }

        let (app, app_box) = panel(self.content_box, COLOR_ROW, 32, 24);
        self.add_content(app);
        add_to_box(
            app_box,
            create_label(app_box, &small("REQUESTED BY", "#8a8f99")),
        );
        add_to_box(
            app_box,
            create_label(
                app_box,
                &format!(
                    "<font_size=34><b>{}</b></font_size>",
                    escape(&prompt.package)
                ),
            ),
        );
        add_to_box(
            app_box,
            create_label(app_box, &small(&prompt.label, "#8a8f99")),
        );

        let name = if let Some(target) = target {
            if !removing {
                prompt.package.clone()
            } else if !target.name.is_empty() {
                target.name.clone()
            } else {
                target.pkgid.clone()
            }
        } else if let Some(info) = info {
            if removing {
                prompt.package.clone()
            } else if !info.name.is_empty() {
                info.name.clone()
            } else {
                prompt.package.clone()
            }
        } else {
            prompt.package.clone()
        };
        let mut details = Vec::new();
        if removing {
            if let Some(target) = target {
                if !target.pkgid.is_empty() {
                    details.push(target.pkgid.clone());
                }
                if !target.pkg_version.is_empty() {
                    details.push(format!("version {}", target.pkg_version));
                }
                if !target.pkg_type.is_empty() {
                    details.push(target.pkg_type.to_uppercase());
                }
            }
        } else if let Some(info) = info {
            if !info.pkgid.is_empty() {
                details.push(info.pkgid.clone());
            }
            if !info.pkg_version.is_empty() {
                details.push(format!("version {}", info.pkg_version));
            }
            if !info.pkg_type.is_empty() {
                details.push(info.pkg_type.to_uppercase());
            }
            if info.size_bytes > 0 {
                details.push(format_size(info.size_bytes));
            }
        }

        let (package_panel, package_box) = panel(self.content_box, COLOR_INSET, 28, 20);
        self.add_content(package_panel);
        add_to_box(
            package_box,
            create_label(
                package_box,
                &small(
                    if removing {
                        "PACKAGE"
                    } else {
                        "PACKAGE TO INSTALL"
                    },
                    "#8a8f99",
                ),
            ),
        );
        add_to_box(
            package_box,
            create_label(
                package_box,
                &format!("<font_size=32><b>{}</b></font_size>", escape(&name)),
            ),
        );
        if !details.is_empty() {
            add_to_box(
                package_box,
                create_label(package_box, &small(&details.join(" \u{00b7} "), "#b4b9c2")),
            );
        }
        if info.is_some_and(|info| info.is_upgrade) {
            add_to_box(
                package_box,
                create_label(
                    package_box,
                    &small("This upgrades the installed version.", "#ffb74d"),
                ),
            );
        }
        if !removing {
            add_to_box(
                package_box,
                create_label(
                    package_box,
                    &signature_badge(info.map(|info| info.signature.as_str()).unwrap_or("")),
                ),
            );
        }

        if !removing {
            if let Some(info) = info.filter(|info| !info.privileges.is_empty()) {
                self.add_content(create_label(
                    self.content_box,
                    &small(
                        &format!("PRIVILEGES ({})", info.privileges.len()),
                        "#8a8f99",
                    ),
                ));
                let (privileges, privilege_box) = panel(self.content_box, COLOR_ROW, 28, 20);
                self.add_content(privileges);
                for privilege in info.privileges.iter().take(MAX_PRIVILEGES_SHOWN) {
                    add_to_box(
                        privilege_box,
                        create_label(
                            privilege_box,
                            &small(
                                &format!("\u{2022} {}", privilege_label(privilege)),
                                "#e8e8e8",
                            ),
                        ),
                    );
                }
                if info.privileges.len() > MAX_PRIVILEGES_SHOWN {
                    let more = info.privileges.len() - MAX_PRIVILEGES_SHOWN;
                    add_to_box(
                        privilege_box,
                        create_label(
                            privilege_box,
                            &small(
                                &format!(
                                    "+ {more} more privilege{}",
                                    if more == 1 { "" } else { "s" }
                                ),
                                "#8a8f99",
                            ),
                        ),
                    );
                }
            }
        }

        let footer = create_row(self.content_box);
        self.add_content(footer);
        self.last_countdown = -1;
        self.countdown_label = create_label(footer, "");
        unsafe { elm_box_pack_end(footer, self.countdown_label) };
        let deny = create_button(
            footer,
            "Deny",
            on_decision_clicked,
            std::ptr::without_provenance_mut(1),
        );
        create_button(
            footer,
            if removing { "Remove" } else { "Install" },
            on_decision_clicked,
            std::ptr::without_provenance_mut(2),
        );
        unsafe { elm_object_focus_set(deny, 1) };
    }

    fn render_manager(&mut self) {
        if self.content_box.is_null() {
            return;
        }
        self.clear_content();
        self.set_header("TV-AUTHD", "Root access manager", COLOR_ACCENT);
        self.set_status(
            self.manager_message
                .as_deref()
                .unwrap_or("Apps listed here can run commands as root without asking."),
        );

        if !self.connected {
            self.add_content(create_label(
                self.content_box,
                &small(
                    "Not connected to tv-authd. Retrying automatically.",
                    "#ffb74d",
                ),
            ));
        }

        let mut first_button: *mut c_void = ptr::null_mut();
        if self.grants.is_empty() {
            let (empty, empty_box) = panel(self.content_box, COLOR_ROW, 32, 28);
            self.add_content(empty);
            add_to_box(
                empty_box,
                create_label(
                    empty_box,
                    "<font_size=28><b>No apps have permanent root access</b></font_size>",
                ),
            );
            add_to_box(
                empty_box,
                create_label(
                    empty_box,
                    &small(
                        "When an app asks for root and you choose Always allow, it will appear \
                         here.",
                        "#8a8f99",
                    ),
                ),
            );
        } else {
            let visible: Vec<GrantInfo> = self.grants.iter().take(5).cloned().collect();
            for (index, grant) in visible.iter().enumerate() {
                let (row, row_box) = panel(self.content_box, COLOR_ROW, 32, 20);
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

                let data = std::ptr::without_provenance_mut(index + 1);
                let notify = create_button(
                    row_box,
                    &notify_button_text(&grant.notify),
                    on_notify_clicked,
                    data,
                );
                unsafe { evas_object_size_hint_min_set(notify, 320, 84) };
                create_button(row_box, "Revoke", on_revoke_clicked, data);
                // Default focus on the harmless toggle, not Revoke.
                if self.focus_notify_label.as_deref() == Some(grant.label.as_str())
                    || first_button.is_null()
                {
                    first_button = notify;
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
        }

        let footer = create_row(self.content_box);
        self.add_content(footer);
        add_to_box(
            footer,
            create_label(footer, &small("Press BACK to close", "#8a8f99")),
        );
        let refresh = create_button(footer, "Refresh", on_refresh_clicked, ptr::null_mut());
        unsafe {
            elm_object_focus_set(
                if first_button.is_null() {
                    refresh
                } else {
                    first_button
                },
                1,
            )
        };
    }

    fn add_content(&mut self, child: *mut c_void) {
        if child.is_null() {
            return;
        }
        unsafe { elm_box_pack_end(self.content_box, child) };
        self.content_objects.push(child);
    }

    fn clear_content(&mut self) {
        self.countdown_label = ptr::null_mut();
        for object in self.content_objects.drain(..) {
            if !object.is_null() {
                unsafe { evas_object_del(object) };
            }
        }
    }

    fn set_header(&mut self, eyebrow: &str, title: &str, accent: i32) {
        let hex = format!("#{:06x}", accent & 0x00ff_ffff);
        set_text(
            self.header_label,
            &format!(
                "<font_size=20><b><color={hex}>{}</color></b></font_size><br/>\
                 <font_size=46><b>{}</b></font_size>",
                escape(eyebrow),
                escape(title)
            ),
        );
        if !self.accent_bar.is_null() {
            unsafe {
                evas_object_color_set(
                    self.accent_bar,
                    (accent >> 16) & 0xff,
                    (accent >> 8) & 0xff,
                    accent & 0xff,
                    255,
                )
            };
        }
    }

    fn set_status(&self, text: &str) {
        set_text(
            self.status_label,
            &format!(
                "<font_size=24><color=#c9ccd3>{}</color></font_size>",
                escape(text)
            ),
        );
    }

    fn exit_now(&mut self) {
        if self.exiting {
            return;
        }
        self.exiting = true;
        unsafe { ui_app_exit() };
    }
}

unsafe extern "C" fn app_create(user_data: *mut c_void) -> bool {
    UI_PTR.with(|cell| cell.set(user_data.cast()));
    let state = ui();
    log_line("app_create");
    state.create_window()
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
    if state.current_prompt.is_none() {
        state.exit_at = None;
        state.manager_message = None;
        if state.main_shown {
            log_line("manager grants refresh");
            if let Some(broker) = &state.broker {
                broker.request_grants();
            }
        }
        state.render_manager();
    }
    state.show_main();
}

unsafe extern "C" fn on_timer(_data: *mut c_void) -> EinaBool {
    ui().on_timer()
}

unsafe extern "C" fn on_back(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    log_line("back key");
    let state = ui();
    if state.current_prompt.is_some() {
        state.send_decision("deny");
        return;
    }
    state.exit_now();
}

unsafe extern "C" fn on_decision_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let decision = data as usize;
    log_line(&format!("decision clicked {decision}"));
    let state = ui();
    match decision {
        2 => state.send_decision("once"),
        3 => state.send_decision("always"),
        _ => state.send_decision("deny"),
    }
}

unsafe extern "C" fn on_revoke_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let index = data as usize;
    if index == 0 {
        return;
    }
    let label = {
        let state = ui();
        state.grants.get(index - 1).map(|grant| grant.label.clone())
    };
    let Some(label) = label else {
        return;
    };
    log_line(&format!("revoke clicked {label}"));
    let state = ui();
    state.manager_message = Some(format!("Revoking {label}..."));
    state.render_manager();
    if let Some(broker) = &state.broker {
        broker.revoke(&label);
    }
}

unsafe extern "C" fn on_notify_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let index = data as usize;
    if index == 0 {
        return;
    }
    let (label, next) = {
        let state = ui();
        match state.grants.get(index - 1) {
            Some(grant) => (grant.label.clone(), next_notify(&grant.notify)),
            None => return,
        }
    };
    log_line(&format!("notify clicked {label} -> {next}"));
    let state = ui();
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
    log_line("refresh clicked");
    let state = ui();
    state.manager_message = Some("Refreshing...".into());
    state.render_manager();
    if let Some(broker) = &state.broker {
        broker.request_grants();
    }
}

fn create_rect(parent: *mut c_void, rgb: i32, alpha: i32) -> *mut c_void {
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
fn panel(parent: *mut c_void, rgb: i32, pad_x: i32, pad_y: i32) -> (*mut c_void, *mut c_void) {
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
fn create_row(parent: *mut c_void) -> *mut c_void {
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

fn create_label(parent: *mut c_void, markup: &str) -> *mut c_void {
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

fn add_to_box(box_: *mut c_void, child: *mut c_void) -> *mut c_void {
    if !child.is_null() {
        unsafe { elm_box_pack_end(box_, child) };
    }
    child
}

fn create_button(
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

fn set_text(object: *mut c_void, text: &str) {
    if object.is_null() {
        return;
    }
    let value = efl::c_string(text);
    unsafe { elm_object_part_text_set(object, ptr::null(), value.as_ptr()) };
}

fn set_window_title(window: *mut c_void, text: &str) {
    let value = efl::c_string(text);
    unsafe { elm_win_title_set(window, value.as_ptr()) };
}

fn set_highest_notification_level(window: *mut c_void) {
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

fn main() {
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
