//! Long-lived, reconnecting `tv-authd` client for the UIs.
//!
//! The reader runs on a blocking thread and forwards parsed messages through a
//! channel, then calls `wake` so an event loop can drain it. Writes happen on
//! the caller's thread through a shared stream.

use crate::ui::{GrantInfo, NoticeInfo, PromptInfo};
use crate::{read_message, MAX_RESPONSE_BYTES};
use serde_json::{json, Value};
use std::io::{BufReader, ErrorKind, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub use crate::DEFAULT_SOCKET;

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    ConnectionChanged(bool),
    Prompt(PromptInfo),
    Notice(Vec<NoticeInfo>),
    /// `ui.hello` accepted: prompts and notices will follow.
    HelloAccepted,
    Grants(Vec<GrantInfo>),
    DecisionFinished(bool, String),
    RevokeFinished(bool, String),
    NotifyFinished(bool, String),
    StatusMessage(String),
}

#[derive(Default)]
struct Counters {
    next_id: AtomicU64,
    last_hello: AtomicU64,
    last_decision: AtomicU64,
    last_grants: AtomicU64,
    last_notify: AtomicU64,
}

impl Counters {
    fn next(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }
}

type SharedSocket = Arc<Mutex<Option<UnixStream>>>;

/// Event sink: the channel plus a hook that wakes the consumer's loop.
#[derive(Clone)]
pub struct Notifier {
    tx: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    log: fn(&str),
}

impl Notifier {
    pub fn new(tx: Sender<Event>, wake: Arc<dyn Fn() + Send + Sync>, log: fn(&str)) -> Self {
        Self { tx, wake, log }
    }

    fn send(&self, event: Event) {
        if self.tx.send(event).is_ok() {
            (self.wake)();
        }
    }
}

pub struct BrokerClient {
    socket: SharedSocket,
    stopped: Arc<AtomicBool>,
    counters: Arc<Counters>,
    events: Notifier,
}

impl BrokerClient {
    /// Connect to [`DEFAULT_SOCKET`], reconnecting forever. With `hello`, the
    /// client registers as the prompt/notice UI after every (re)connect.
    pub fn start(events: Notifier, hello: bool) -> Self {
        Self::start_with(DEFAULT_SOCKET, events, hello)
    }

    pub fn start_with(socket_path: impl Into<PathBuf>, events: Notifier, hello: bool) -> Self {
        let socket_path = socket_path.into();
        let socket: SharedSocket = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let thread_socket = Arc::clone(&socket);
        let thread_stopped = Arc::clone(&stopped);
        let thread_counters = Arc::clone(&counters);
        let thread_events = events.clone();
        thread::Builder::new()
            .name("tv-auth-broker-client".into())
            .spawn(move || {
                run_loop(
                    socket_path,
                    thread_socket,
                    thread_stopped,
                    thread_counters,
                    thread_events,
                    hello,
                );
            })
            .ok();
        Self {
            socket,
            stopped,
            counters,
            events,
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let mut guard = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(stream) = guard.as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        *guard = None;
    }

    pub fn request_grants(&self) {
        let id = self.counters.next();
        self.counters.last_grants.store(id, Ordering::SeqCst);
        let request = json!({
            "version": 2,
            "id": format!("grants-{id}"),
            "method": "grants.list"
        });
        if !self.send_raw(&request.to_string()) {
            self.events
                .send(Event::StatusMessage("Not connected to tv-authd.".into()));
        }
    }

    pub fn revoke(&self, label: &str) {
        let id = self.counters.next();
        let request = json!({
            "version": 2,
            "id": format!("revoke-{id}"),
            "method": "grants.revoke",
            "label": label
        });
        if !self.send_raw(&request.to_string()) {
            self.events
                .send(Event::RevokeFinished(false, "not_connected".into()));
        }
    }

    pub fn set_notify(&self, label: &str, notify: &str) {
        let id = self.counters.next();
        self.counters.last_notify.store(id, Ordering::SeqCst);
        let request = json!({
            "version": 2,
            "id": format!("notify-{id}"),
            "method": "grants.notify",
            "label": label,
            "notify": notify
        });
        if !self.send_raw(&request.to_string()) {
            self.events
                .send(Event::NotifyFinished(false, "not_connected".into()));
        }
    }

    pub fn decide(&self, prompt: &PromptInfo, decision: &str) {
        let id = self.counters.next();
        self.counters.last_decision.store(id, Ordering::SeqCst);
        let request = json!({
            "version": 2,
            "id": format!("decision-{id}"),
            "method": "ui.decision",
            "prompt_id": prompt.prompt_id,
            "nonce": prompt.nonce,
            "decision": decision
        });
        if !self.send_raw(&request.to_string()) {
            self.events
                .send(Event::DecisionFinished(false, "not_connected".into()));
        }
    }

    fn send_raw(&self, text: &str) -> bool {
        let mut guard = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(stream) = guard.as_mut() else {
            return false;
        };
        match stream
            .write_all(text.as_bytes())
            .and_then(|()| stream.write_all(b"\n"))
        {
            Ok(()) => true,
            Err(error) => {
                (self.events.log)(&format!("broker send: {error}"));
                false
            }
        }
    }
}

impl Drop for BrokerClient {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_loop(
    socket_path: PathBuf,
    socket: SharedSocket,
    stopped: Arc<AtomicBool>,
    counters: Arc<Counters>,
    events: Notifier,
    hello: bool,
) {
    while !stopped.load(Ordering::SeqCst) {
        match UnixStream::connect(&socket_path) {
            Ok(stream) => {
                let Ok(reader_stream) = stream.try_clone() else {
                    (events.log)("broker: cannot clone the connection");
                    wait_before_reconnect(&stopped);
                    continue;
                };
                {
                    let mut guard = socket.lock().unwrap_or_else(|error| error.into_inner());
                    *guard = Some(stream);
                }
                events.send(Event::ConnectionChanged(true));
                if hello {
                    send_hello(&socket, &counters, events.log);
                }

                let mut reader = BufReader::new(reader_stream);
                loop {
                    match read_message(&mut reader, MAX_RESPONSE_BYTES) {
                        Ok(line) => {
                            handle_line(&String::from_utf8_lossy(&line), &counters, &events)
                        }
                        Err(error) if error.kind() == ErrorKind::UnexpectedEof => break,
                        Err(error) => {
                            if !stopped.load(Ordering::SeqCst) {
                                (events.log)(&format!("broker read: {error}"));
                            }
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                if !stopped.load(Ordering::SeqCst) {
                    (events.log)(&format!("broker connect: {error}"));
                }
            }
        }

        {
            let mut guard = socket.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(stream) = guard.take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        events.send(Event::ConnectionChanged(false));
        wait_before_reconnect(&stopped);
    }
}

fn wait_before_reconnect(stopped: &AtomicBool) {
    for _ in 0..20 {
        if stopped.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn send_hello(socket: &SharedSocket, counters: &Counters, log: fn(&str)) {
    let id = counters.next();
    counters.last_hello.store(id, Ordering::SeqCst);
    let request = json!({
        "version": 2,
        "id": format!("hello-{id}"),
        "method": "ui.hello"
    });
    let mut guard = socket.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(stream) = guard.as_mut() {
        if let Err(error) = stream
            .write_all(request.to_string().as_bytes())
            .and_then(|()| stream.write_all(b"\n"))
        {
            log(&format!("broker hello: {error}"));
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

fn handle_line(line: &str, counters: &Counters, events: &Notifier) {
    let log = events.log;
    let Ok(root) = serde_json::from_str::<Value>(line) else {
        log(&format!("broker parse: {line}"));
        return;
    };

    if string(&root, "type") == "ui.prompt" {
        match serde_json::from_value::<PromptInfo>(root.clone()) {
            Ok(prompt) => {
                events.send(Event::Prompt(prompt));
            }
            Err(error) => log(&format!("prompt parse: {error}")),
        }
        return;
    }

    if string(&root, "type") == "ui.notice" {
        if let Some(notices) = root.get("notices") {
            match serde_json::from_value::<Vec<NoticeInfo>>(notices.clone()) {
                Ok(notices) => {
                    events.send(Event::Notice(notices));
                }
                Err(error) => log(&format!("notice parse: {error}")),
            }
        }
        return;
    }

    let id = string(&root, "id");
    let status = string(&root, "status");
    let error = string(&root, "error");

    if let Some(grants) = root.get("grants") {
        if let Ok(grants) = serde_json::from_value::<Vec<GrantInfo>>(grants.clone()) {
            events.send(Event::Grants(grants));
        }
        return;
    }

    if let Some(revoked) = root.get("revoked").and_then(Value::as_bool) {
        events.send(Event::RevokeFinished(
            revoked,
            if revoked {
                String::new()
            } else {
                "not_found".into()
            },
        ));
        return;
    }

    if id == format!("hello-{}", counters.last_hello.load(Ordering::SeqCst)) {
        if status != "ok" {
            events.send(Event::StatusMessage(format!(
                "tv-authd rejected this UI ({error})."
            )));
        } else {
            events.send(Event::HelloAccepted);
        }
        return;
    }

    if id == format!("notify-{}", counters.last_notify.load(Ordering::SeqCst)) {
        events.send(Event::NotifyFinished(status == "ok", error));
        return;
    }

    if id == format!("decision-{}", counters.last_decision.load(Ordering::SeqCst)) {
        events.send(Event::DecisionFinished(status == "ok", error));
        return;
    }

    if id == format!("grants-{}", counters.last_grants.load(Ordering::SeqCst)) && status != "ok" {
        events.send(Event::StatusMessage(format!(
            "Could not load grants ({error})."
        )));
    }
}

fn string(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notifier() -> (Notifier, std::sync::mpsc::Receiver<Event>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Notifier::new(tx, Arc::new(|| {}), |_| {}), rx)
    }

    #[test]
    fn parses_prompt_notice_and_grants() {
        let (tx, rx) = notifier();
        let counters = Counters::default();
        handle_line(
            r#"{"version":2,"type":"ui.prompt","prompt_id":"p","nonce":"n","request_id":"r","label":"User::Pkg::org.example.app","package":"org.example.app","kind":"install","install":{"pkgid":"org.example.app","name":"Example","pkg_version":"1.0.0","pkg_type":"wgt","size_bytes":12,"privileges":["http://tizen.org/privilege/internet"],"signature":"user_ca","is_upgrade":false},"command":"install wgt /x","command_bytes":15,"command_truncated":false,"timeout_ms":60000}"#,
            &counters,
            &tx,
        );
        match rx.try_recv().unwrap() {
            Event::Prompt(prompt) => {
                assert_eq!(prompt.kind, "install");
                assert_eq!(prompt.install.unwrap().pkgid, "org.example.app");
            }
            other => panic!("unexpected: {other:?}"),
        }

        handle_line(
            r#"{"version":2,"type":"ui.notice","notices":[{"label":"User::Pkg::org.example.app","package":"org.example.app","count":3,"command":"id","command_truncated":false,"kind":"exec"}]}"#,
            &counters,
            &tx,
        );
        match rx.try_recv().unwrap() {
            Event::Notice(notices) => {
                assert_eq!(notices[0].count, 3);
                assert_eq!(notices[0].kind, "exec");
            }
            other => panic!("unexpected: {other:?}"),
        }

        handle_line(
            r#"{"version":2,"id":"grants-1","status":"ok","grants":[{"label":"User::Pkg::org.example.app","package":"org.example.app","granted_at":1,"notify":"first"}]}"#,
            &counters,
            &tx,
        );
        match rx.try_recv().unwrap() {
            Event::Grants(grants) => assert_eq!(grants[0].notify, "first"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn matches_response_ids() {
        let (tx, rx) = notifier();
        let counters = Counters::default();
        counters.last_hello.store(7, Ordering::SeqCst);
        handle_line(
            r#"{"version":2,"id":"hello-7","status":"ok"}"#,
            &counters,
            &tx,
        );
        match rx.try_recv().unwrap() {
            Event::HelloAccepted => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
