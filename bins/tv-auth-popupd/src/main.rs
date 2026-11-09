//! `tv-auth-popupd`: persistent prompt and toast overlay for `tv-authd`.
//!
//! systemd starts it as uid 5001 with the `User::Pkg::org.tvauth.ui` SMACK
//! label, so the broker accepts its `ui.hello`. It stays connected and
//! invisible: no window is mapped and no key is grabbed until a `ui.prompt`
//! (full-screen modal, BACK grabbed) or `ui.notice` (focus-skipping toast)
//! arrives. Broker events wake the EFL loop through an `Ecore_Pipe`; a timer
//! runs only while something is on screen.

use std::cell::Cell;
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tv_efl::log::log_line;
use tv_efl::widgets::*;
use tv_efl::*;
use tv_proto::client::{BrokerClient, Event, Notifier};
use tv_proto::markup::*;
use tv_proto::ui::{NoticeInfo, PromptInfo};

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
const TOAST_SHOWN: Duration = Duration::from_secs(6);
const TOAST_MAX_APPS: usize = 3;
const TOAST_COMMAND_CHARS: usize = 52;
const TICK_SECONDS: f64 = 0.25;

const PROMPT_NAME: &[u8] = b"org.tvauth.popup\0";
const TOAST_NAME: &[u8] = b"org.tvauth.popup.toast\0";
const DELETE_REQUEST: &[u8] = b"delete,request\0";
const BACK_KEY: &[u8] = b"XF86Back\0";

thread_local! {
    static POPUP: Cell<*mut Popup> = const { Cell::new(ptr::null_mut()) };
}

fn popup() -> &'static mut Popup {
    let pointer = POPUP.with(Cell::get);
    assert!(!pointer.is_null(), "popup state used before main");
    // SAFETY: installed on the EFL main thread in `main` before `elm_run` and
    // valid until it returns; every EFL callback runs on that thread.
    unsafe { &mut *pointer }
}

/// Raw `Ecore_Pipe` handle; `ecore_pipe_write` is documented thread-safe.
struct Pipe(*mut c_void);
unsafe impl Send for Pipe {}
unsafe impl Sync for Pipe {}

struct Popup {
    rx: Receiver<Event>,
    broker: Option<BrokerClient>,
    timer: *mut c_void,
    screen_w: i32,
    screen_h: i32,

    window: *mut c_void,
    accent_bar: *mut c_void,
    header_label: *mut c_void,
    status_label: *mut c_void,
    content_box: *mut c_void,
    content_objects: Vec<*mut c_void>,
    countdown_label: *mut c_void,
    last_countdown: i32,
    prompt_shown: bool,
    back_grabbed: bool,
    current_prompt: Option<PromptInfo>,
    prompt_expires_at: Option<Instant>,
    /// Hide the prompt window after showing a decision/timeout result.
    hide_at: Option<Instant>,

    toast_window: *mut c_void,
    toast_label: *mut c_void,
    toast_entries: Vec<NoticeInfo>,
    toast_hide_at: Option<Instant>,
    toast_rows: usize,
}

impl Popup {
    fn new(rx: Receiver<Event>) -> Self {
        Self {
            rx,
            broker: None,
            timer: ptr::null_mut(),
            screen_w: 1920,
            screen_h: 1080,
            window: ptr::null_mut(),
            accent_bar: ptr::null_mut(),
            header_label: ptr::null_mut(),
            status_label: ptr::null_mut(),
            content_box: ptr::null_mut(),
            content_objects: Vec::new(),
            countdown_label: ptr::null_mut(),
            last_countdown: -1,
            prompt_shown: false,
            back_grabbed: false,
            current_prompt: None,
            prompt_expires_at: None,
            hide_at: None,
            toast_window: ptr::null_mut(),
            toast_label: ptr::null_mut(),
            toast_entries: Vec::new(),
            toast_hide_at: None,
            toast_rows: 0,
        }
    }

    fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Prompt(prompt) => self.on_prompt(prompt),
                Event::Notice(notices) => self.on_notice(notices),
                Event::HelloAccepted => log_line("registered with tv-authd"),
                Event::DecisionFinished(ok, error) => self.on_decision_finished(ok, &error),
                Event::ConnectionChanged(connected) => self.on_connection_changed(connected),
                Event::StatusMessage(message) => log_line(&message),
                Event::Grants(_) | Event::RevokeFinished(..) | Event::NotifyFinished(..) => {}
            }
        }
    }

    fn ensure_ticking(&mut self) {
        if self.timer.is_null() {
            self.timer = unsafe { ecore_timer_add(TICK_SECONDS, on_timer, ptr::null_mut()) };
        }
    }

    /// Returns whether the timer should keep running.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        if let (Some(expires), Some(_)) = (self.prompt_expires_at, &self.current_prompt) {
            let remaining = expires.saturating_duration_since(now).as_secs_f64().ceil() as i32;
            if remaining != self.last_countdown {
                self.last_countdown = remaining;
                set_text(
                    self.countdown_label,
                    &small(&format!("Denied automatically in {remaining} s"), "#8a8f99"),
                );
            }
            if now >= expires {
                // The daemon denies on its own clock; just reflect it.
                log_line("prompt expired");
                self.current_prompt = None;
                self.prompt_expires_at = None;
                self.clear_content();
                self.set_header("ROOT ACCESS", "Timed out", COLOR_DANGER);
                self.set_status("The request timed out and was denied.");
                self.hide_at = Some(now + Duration::from_secs(2));
            }
        }
        if self.hide_at.is_some_and(|at| now >= at) {
            self.hide_prompt();
        }
        if self.toast_hide_at.is_some_and(|at| now >= at) {
            self.hide_toast();
        }
        self.prompt_shown || self.toast_hide_at.is_some()
    }

    fn on_prompt(&mut self, prompt: PromptInfo) {
        if self.current_prompt.is_some() {
            // The daemon keeps one prompt active; deny a stale one explicitly.
            self.send_decision("deny");
        }
        if self.window.is_null() && !self.create_window() {
            return;
        }
        log_line(&format!(
            "prompt received package={} request={} kind={}",
            prompt.package, prompt.request_id, prompt.kind
        ));
        let timeout = if prompt.timeout_ms > 0 {
            prompt.timeout_ms
        } else {
            60_000
        };
        let package_operation = prompt.is_package_operation();
        self.current_prompt = Some(prompt);
        self.prompt_expires_at = Some(Instant::now() + Duration::from_millis(timeout));
        self.hide_at = None;
        if package_operation {
            self.render_package_prompt();
        } else {
            self.render_prompt();
        }
        self.show_prompt();
    }

    fn show_prompt(&mut self) {
        self.hide_toast();
        if !self.prompt_shown {
            self.prompt_shown = true;
            unsafe {
                // Keys are only taken while a decision is on screen.
                self.back_grabbed =
                    eext_win_keygrab_set(self.window, BACK_KEY.as_ptr().cast()) == 1;
                if !self.back_grabbed {
                    log_line("XF86Back keygrab failed");
                }
                // The compositor drops the level when the window is unmapped,
                // so a re-shown prompt would sink to NOTIFICATION_LOW (below
                // the manager). Re-apply it on every show.
                set_highest_notification_level(self.window);
                evas_object_show(self.window);
                elm_win_raise(self.window);
                elm_win_activate(self.window);
            }
            log_line("prompt shown");
        }
        self.ensure_ticking();
    }

    fn hide_prompt(&mut self) {
        self.hide_at = None;
        if !self.prompt_shown {
            return;
        }
        self.prompt_shown = false;
        unsafe {
            if self.back_grabbed {
                eext_win_keygrab_unset(self.window, BACK_KEY.as_ptr().cast());
            }
            evas_object_hide(self.window);
        }
        self.back_grabbed = false;
        self.clear_content();
        log_line("prompt hidden");
    }

    fn send_decision(&mut self, decision: &str) {
        let Some(prompt) = self.current_prompt.take() else {
            return;
        };
        self.prompt_expires_at = None;
        // Safety net if the broker never answers.
        self.hide_at = Some(Instant::now() + Duration::from_secs(5));
        self.clear_content();
        let eyebrow = match prompt.kind.as_str() {
            "uninstall" => "REMOVE PACKAGE",
            "install" => "INSTALL PACKAGE",
            _ => "ROOT ACCESS",
        };
        match decision {
            "once" if prompt.is_package_operation() => {
                self.set_header(eyebrow, "Approved", COLOR_ACCENT)
            }
            "once" => self.set_header(eyebrow, "Allowed once", COLOR_ACCENT),
            "always" => self.set_header(eyebrow, "Always allowed", COLOR_ACCENT),
            _ => self.set_header(eyebrow, "Denied", COLOR_DANGER),
        }
        self.set_status("Sending decision...");
        if let Some(broker) = &self.broker {
            broker.decide(&prompt, decision);
        }
        log_line(&format!(
            "decision sent package={} kind={} decision={decision}",
            prompt.package, prompt.kind
        ));
    }

    fn on_decision_finished(&mut self, ok: bool, error: &str) {
        if self.current_prompt.is_some() || !self.prompt_shown {
            return;
        }
        if ok {
            self.set_status("Done.");
            self.hide_at = Some(Instant::now() + Duration::from_millis(1200));
            return;
        }
        let detail = match error {
            "package_identity_unavailable" => {
                "the installed package identity could not be verified; choose Allow once or Deny"
            }
            "grant_persist_failed" => "the grant could not be saved; choose Allow once or Deny",
            "" => "unknown",
            other => other,
        };
        self.set_header("ERROR", "Decision failed", COLOR_DANGER);
        self.set_status(&format!(
            "tv-authd reported: {detail}. The request was not approved."
        ));
        self.hide_at = Some(Instant::now() + Duration::from_millis(2500));
    }

    fn on_connection_changed(&mut self, connected: bool) {
        log_line(if connected {
            "connected"
        } else {
            "disconnected"
        });
        if !connected && self.current_prompt.take().is_some() {
            // The daemon denies (or re-sends) prompts of a vanished UI.
            self.prompt_expires_at = None;
            self.hide_prompt();
        }
    }

    fn on_notice(&mut self, notices: Vec<NoticeInfo>) {
        for notice in notices {
            log_line(&format!(
                "notice package={} count={}",
                notice.package, notice.count
            ));
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
        // A prompt owns the screen; the daemon holds notices meanwhile.
        if !self.prompt_shown && !self.toast_entries.is_empty() {
            self.show_toast();
        }
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
                set_highest_notification_level(self.toast_window);
                evas_object_show(self.toast_window);
            }
        }
        self.toast_hide_at = Some(Instant::now() + TOAST_SHOWN);
        self.ensure_ticking();
    }

    fn hide_toast(&mut self) {
        self.toast_hide_at = None;
        self.toast_entries.clear();
        self.toast_rows = 0;
        if !self.toast_window.is_null() {
            unsafe { evas_object_hide(self.toast_window) };
        }
    }

    fn create_window(&mut self) -> bool {
        unsafe {
            let window = elm_win_add(
                ptr::null_mut(),
                PROMPT_NAME.as_ptr().cast(),
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

            // Dimmed full-screen scrim under a solid centred card; the stock
            // elm_bg is transparent in the TV theme.
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

            self.content_box = elm_box_add(card_box);
            evas_object_size_hint_weight_set(self.content_box, 1.0, 1.0);
            evas_object_size_hint_align_set(self.content_box, EVAS_HINT_FILL, EVAS_HINT_FILL);
            elm_box_padding_set(self.content_box, 0, 24);
            elm_box_pack_end(card_box, self.content_box);
            evas_object_show(self.content_box);

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

            let hint = c_string("wm.policy.win.user.geometry");
            let one = c_string("1");
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
}

unsafe extern "C" fn on_pipe(_data: *mut c_void, _buffer: *mut c_void, _bytes: u32) {
    popup().drain();
}

unsafe extern "C" fn on_timer(_data: *mut c_void) -> EinaBool {
    let state = popup();
    if state.tick() {
        1
    } else {
        state.timer = ptr::null_mut();
        0
    }
}

unsafe extern "C" fn on_back(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    log_line("back key");
    let state = popup();
    if state.current_prompt.is_some() {
        state.send_decision("deny");
    } else {
        state.hide_prompt();
    }
}

unsafe extern "C" fn on_decision_clicked(
    data: *mut c_void,
    _object: *mut c_void,
    _info: *mut c_void,
) {
    let decision = data as usize;
    log_line(&format!("decision clicked {decision}"));
    match decision {
        2 => popup().send_decision("once"),
        3 => popup().send_decision("always"),
        _ => popup().send_decision("deny"),
    }
}

fn main() {
    tv_efl::log::set_path("/tmp/tvauth-popupd.log");
    log_line("starting");
    let arguments: Vec<CString> = std::env::args_os()
        .filter_map(|argument| CString::new(argument.as_encoded_bytes()).ok())
        .collect();
    let mut argv: Vec<*mut c_char> = arguments
        .iter()
        .map(|argument| argument.as_ptr() as *mut c_char)
        .collect();
    argv.push(ptr::null_mut());
    let argc = i32::try_from(arguments.len()).unwrap_or(0);

    // SAFETY: argv outlives elm_init; everything below runs on this thread.
    unsafe {
        if elm_init(argc, argv.as_mut_ptr()) <= 0 {
            log_line("elm_init failed");
            std::process::exit(1);
        }
        let (tx, rx) = mpsc::channel();
        let state = Box::into_raw(Box::new(Popup::new(rx)));
        POPUP.with(|cell| cell.set(state));

        let pipe = ecore_pipe_add(on_pipe, ptr::null());
        if pipe.is_null() {
            log_line("ecore_pipe_add failed");
            std::process::exit(1);
        }
        let pipe = Arc::new(Pipe(pipe));
        let wake = Arc::new(move || {
            ecore_pipe_write(pipe.0, b"!".as_ptr().cast(), 1);
        });
        (*state).broker = Some(BrokerClient::start(Notifier::new(tx, wake, log_line), true));

        elm_run();
        if let Some(broker) = (*state).broker.take() {
            broker.stop();
        }
        elm_shutdown();
    }
}
