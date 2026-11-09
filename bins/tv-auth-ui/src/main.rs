//! `org.tvauth.ui`: the user-launched grant manager for `tv-authd`.
//!
//! Prompts and toasts live in the persistent `tv-auth-popupd`; this app only
//! lists, re-modes and revokes persistent grants. It does not send
//! `ui.hello`, so it never competes with the popup daemon for prompts. The
//! daemon stages the executable at `/run/tv-auth/ui-bin/tv-auth-ui` (see
//! `tv-authd`'s `ui_alias.rs`) so launchpad can map it despite UEP.

use std::cell::Cell;
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use tv_efl::log::log_line;
use tv_efl::widgets::*;
use tv_efl::*;
use tv_proto::client::{BrokerClient, Event, Notifier};
use tv_proto::markup::*;
use tv_proto::ui::GrantInfo;

const COLOR_CARD: i32 = 0x1c_1f_26;
const COLOR_ROW: i32 = 0x27_2b_34;
const COLOR_ACCENT: i32 = 0x3e_7b_fa;
const CARD_WIDTH: i32 = 1200;
const CARD_PADDING: i32 = 56;

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

struct Ui {
    window: *mut c_void,
    accent_bar: *mut c_void,
    header_label: *mut c_void,
    status_label: *mut c_void,
    content_box: *mut c_void,
    content_objects: Vec<*mut c_void>,

    broker: Option<BrokerClient>,
    tx: Sender<Event>,
    rx: Receiver<Event>,

    grants: Vec<GrantInfo>,
    manager_message: Option<String>,
    exiting: bool,
    back_grabbed: bool,
    connected: bool,
    screen_w: i32,
    screen_h: i32,
    main_shown: bool,
    app_control_count: u32,
    focus_notify_label: Option<String>,
}

impl Ui {
    fn new(tx: Sender<Event>, rx: Receiver<Event>) -> Self {
        Self {
            window: ptr::null_mut(),
            accent_bar: ptr::null_mut(),
            header_label: ptr::null_mut(),
            status_label: ptr::null_mut(),
            content_box: ptr::null_mut(),
            content_objects: Vec::new(),
            broker: None,
            tx,
            rx,
            grants: Vec::new(),
            manager_message: None,
            exiting: false,
            back_grabbed: false,
            connected: false,
            screen_w: 1920,
            screen_h: 1080,
            main_shown: false,
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

            let events = Notifier::new(self.tx.clone(), Arc::new(|| {}), log_line);
            self.broker = Some(BrokerClient::start(events, false));

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
        if self.main_shown || self.window.is_null() {
            return;
        }
        self.main_shown = true;
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
    }

    fn on_timer(&mut self) -> EinaBool {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Grants(grants) => self.on_grants(grants),
                Event::RevokeFinished(revoked, error) => self.on_revoke_finished(revoked, &error),
                Event::NotifyFinished(ok, error) => self.on_notify_finished(ok, &error),
                Event::ConnectionChanged(connected) => self.on_connection_changed(connected),
                Event::StatusMessage(message) => {
                    self.manager_message = Some(message);
                    self.render_manager();
                }
                Event::Prompt(_)
                | Event::Notice(_)
                | Event::HelloAccepted
                | Event::DecisionFinished(..) => {}
            }
        }
        1
    }

    fn refresh(&self) {
        log_line("manager grants refresh");
        if let Some(broker) = &self.broker {
            broker.request_grants();
        }
    }

    fn on_connection_changed(&mut self, connected: bool) {
        self.connected = connected;
        if connected {
            if self.manager_message.as_deref() == Some("Connection lost. Retrying...") {
                self.manager_message = None;
            }
            self.refresh();
        } else if self.manager_message.as_deref().is_none_or(str::is_empty) {
            self.manager_message = Some("Connection lost. Retrying...".into());
        }
        self.render_manager();
    }

    fn on_grants(&mut self, grants: Vec<GrantInfo>) {
        self.grants = grants;
        if self.manager_message.as_deref() == Some("Refreshing...") {
            self.manager_message = None;
        }
        self.render_manager();
    }

    fn on_notify_finished(&mut self, ok: bool, error: &str) {
        if ok {
            // The daemon writes the new mode; refresh so the button reflects it.
            if let Some(broker) = &self.broker {
                broker.request_grants();
            }
            return;
        }
        self.manager_message = Some(format!(
            "Could not change toast setting: {}",
            if error.is_empty() { "unknown" } else { error }
        ));
        self.render_manager();
    }

    fn on_revoke_finished(&mut self, revoked: bool, error: &str) {
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
    if !state.create_window() {
        return false;
    }
    state.show_main();
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
    state.manager_message = None;
    state.refresh();
    state.render_manager();
    state.show_main();
}

unsafe extern "C" fn on_timer(_data: *mut c_void) -> EinaBool {
    ui().on_timer()
}

unsafe extern "C" fn on_back(_data: *mut c_void, _object: *mut c_void, _info: *mut c_void) {
    log_line("back key");
    ui().exit_now();
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
