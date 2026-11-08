use crate::grants::{GrantError, GrantStore};
use crate::logger::Logger;
use crate::protocol::{
    write_json, write_response, Decision, NoticeEntry, NoticeMessage, PromptMessage, Response,
};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TICK_INTERVAL: Duration = Duration::from_millis(200);

/// How many times a prompt is re-shown after the UI disconnected while it was
/// active (the UI exits shortly after each decision, which can race with the
/// next queued prompt).
const MAX_PROMPT_UI_RETRIES: u8 = 2;

/// After the UI could not be launched for notices, wait this long before
/// launching it again, so a broken UI is not respawned for every granted exec.
const NOTICE_RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60);

/// A `first`-mode package is shown again only after being idle this long.
const FIRST_USE_IDLE: Duration = Duration::from_secs(30 * 60);

/// Distinct packages coalesced into one notice.
// ponytail: further packages are dropped until the next flush; only
// user-approved grants reach this path, so 16 concurrently active is plenty.
const MAX_NOTICE_PACKAGES: usize = 16;

#[derive(Debug, Clone)]
pub struct ApprovalConfig {
    pub ui_app_id: String,
    pub app_launcher: PathBuf,
    pub prompt_timeout: Duration,
    pub ui_launch_timeout: Duration,
    pub launch_cooldown: Duration,
    pub prompt_queue_max: usize,
    pub max_launch_attempts: usize,
    /// Minimum gap between `ui.notice` messages for granted executions.
    pub notice_interval: Duration,
}

impl ApprovalConfig {
    pub fn approval_wait_timeout(&self) -> Duration {
        let retries = u32::from(MAX_PROMPT_UI_RETRIES);
        self.prompt_timeout * (self.prompt_queue_max as u32 + 1 + retries)
            + self.ui_launch_timeout * (self.max_launch_attempts as u32) * (1 + retries)
            + Duration::from_secs(30)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResult {
    AllowOnce,
    AllowAlways,
    Denied(&'static str, &'static str),
}

pub struct ApprovalRequest {
    /// Unique internal id, assigned by `ApprovalHandle::request`.
    pub ticket: u64,
    pub request_id: String,
    pub label: String,
    pub command: String,
    pub reply: Sender<ApprovalResult>,
}

enum ApprovalCommand {
    Request(ApprovalRequest),
    AttachUi {
        token: u64,
        hello_id: String,
        label: String,
        writer: Arc<Mutex<UnixStream>>,
        control: UnixStream,
    },
    DetachUi {
        token: u64,
    },
    /// A granted package executed a command without prompting.
    Notice {
        label: String,
        command: String,
        first_use_only: bool,
    },
    /// The requester went away: drop its queued or active prompt.
    Cancel {
        ticket: u64,
    },
    Decision {
        token: u64,
        id: String,
        prompt_id: String,
        nonce: String,
        decision: Decision,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct ApprovalHandle {
    tx: Sender<ApprovalCommand>,
    next_ticket: Arc<AtomicU64>,
}

impl ApprovalHandle {
    /// Queue a request; returns its ticket (the `ticket` field is overwritten).
    pub fn request(&self, mut request: ApprovalRequest) -> io::Result<u64> {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        request.ticket = ticket;
        self.send(ApprovalCommand::Request(request))?;
        Ok(ticket)
    }

    pub fn cancel(&self, ticket: u64) -> io::Result<()> {
        self.send(ApprovalCommand::Cancel { ticket })
    }

    /// Register a UI connection; the manager answers `hello_id` itself so the
    /// response is ordered before any prompt or notice.
    pub fn attach_ui(
        &self,
        token: u64,
        hello_id: String,
        label: String,
        writer: Arc<Mutex<UnixStream>>,
        control: UnixStream,
    ) -> io::Result<()> {
        self.send(ApprovalCommand::AttachUi {
            token,
            hello_id,
            label,
            writer,
            control,
        })
    }

    /// `first_use_only`: show it only if the package was idle for a while.
    pub fn notice(&self, label: String, command: String, first_use_only: bool) -> io::Result<()> {
        self.send(ApprovalCommand::Notice {
            label,
            command,
            first_use_only,
        })
    }

    pub fn detach_ui(&self, token: u64) -> io::Result<()> {
        self.send(ApprovalCommand::DetachUi { token })
    }

    pub fn decide(
        &self,
        token: u64,
        id: String,
        prompt_id: String,
        nonce: String,
        decision: Decision,
    ) -> io::Result<()> {
        self.send(ApprovalCommand::Decision {
            token,
            id,
            prompt_id,
            nonce,
            decision,
        })
    }

    fn send(&self, command: ApprovalCommand) -> io::Result<()> {
        self.tx
            .send(command)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "approval manager stopped"))
    }
}

pub struct ApprovalManager {
    handle: ApprovalHandle,
    join: Option<thread::JoinHandle<()>>,
}

impl ApprovalManager {
    pub fn start(
        config: ApprovalConfig,
        grants: Arc<Mutex<GrantStore>>,
        logger: Option<Arc<Logger>>,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let handle = ApprovalHandle {
            tx,
            next_ticket: Arc::new(AtomicU64::new(1)),
        };
        let thread_handle = thread::Builder::new()
            .name("tv-auth-approval".into())
            .spawn(move || Manager::new(config, grants, logger).run(rx))?;
        Ok(Self {
            handle,
            join: Some(thread_handle),
        })
    }

    pub fn handle(&self) -> ApprovalHandle {
        self.handle.clone()
    }
}

impl Drop for ApprovalManager {
    fn drop(&mut self) {
        let _ = self.handle.tx.send(ApprovalCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct UiClient {
    token: u64,
    writer: Arc<Mutex<UnixStream>>,
    control: UnixStream,
}

struct PendingRequest {
    request: ApprovalRequest,
    ui_retries: u8,
}

struct ActivePrompt {
    prompt_id: String,
    nonce: String,
    request: ApprovalRequest,
    ui_retries: u8,
    expires_at: Instant,
    ui_token: u64,
}

#[derive(Default)]
struct LaunchState {
    attempts: usize,
    last_attempt: Option<Instant>,
    awaiting_until: Option<Instant>,
    /// The outstanding launch was only for notices (no prompt was waiting).
    for_notice: bool,
}

struct Manager {
    config: ApprovalConfig,
    grants: Arc<Mutex<GrantStore>>,
    logger: Option<Arc<Logger>>,
    ui: Option<UiClient>,
    queue: VecDeque<PendingRequest>,
    active: Option<ActivePrompt>,
    launch: LaunchState,
    notices: Vec<NoticeEntry>,
    notice_next_at: Instant,
    notice_retries: u8,
    /// Last granted exec per package, for `first`-mode grants.
    // ponytail: never pruned; bounded by the number of labels ever granted.
    last_exec: HashMap<String, Instant>,
}

impl Manager {
    fn new(
        config: ApprovalConfig,
        grants: Arc<Mutex<GrantStore>>,
        logger: Option<Arc<Logger>>,
    ) -> Self {
        Self {
            config,
            grants,
            logger,
            ui: None,
            queue: VecDeque::new(),
            active: None,
            launch: LaunchState::default(),
            notices: Vec::new(),
            notice_next_at: Instant::now(),
            notice_retries: 0,
            last_exec: HashMap::new(),
        }
    }

    fn run(mut self, rx: Receiver<ApprovalCommand>) {
        loop {
            match rx.recv_timeout(TICK_INTERVAL) {
                Ok(ApprovalCommand::Shutdown) => break,
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
            self.flush_notices();
        }
        if let Some(active) = self.active.take() {
            self.reply(
                active.request,
                ApprovalResult::Denied("ui_unavailable", "approval_stopped"),
            );
        }
        self.fail_all("ui_unavailable");
        self.log("stopped");
    }

    fn handle(&mut self, command: ApprovalCommand) {
        match command {
            ApprovalCommand::Request(request) => self.handle_request(request),
            ApprovalCommand::AttachUi {
                token,
                hello_id,
                label,
                writer,
                control,
            } => self.handle_attach(token, &hello_id, label, writer, control),
            ApprovalCommand::Notice {
                label,
                command,
                first_use_only,
            } => self.handle_notice(&label, &command, first_use_only),
            ApprovalCommand::DetachUi { token } => self.handle_detach(token),
            ApprovalCommand::Cancel { ticket } => self.handle_cancel(ticket),
            ApprovalCommand::Decision {
                token,
                id,
                prompt_id,
                nonce,
                decision,
            } => self.handle_decision(token, &id, &prompt_id, &nonce, decision),
            ApprovalCommand::Shutdown => {}
        }
    }

    fn handle_request(&mut self, request: ApprovalRequest) {
        if self.has_pending(&request.label) {
            self.reply(
                request,
                ApprovalResult::Denied("busy", "request_already_pending"),
            );
            self.log("approval_rejected=request_already_pending");
            return;
        }
        let total = self.queue.len() + usize::from(self.active.is_some());
        if total >= self.config.prompt_queue_max {
            self.reply(request, ApprovalResult::Denied("busy", "prompt_queue_full"));
            self.log("approval_rejected=prompt_queue_full");
            return;
        }
        self.log(&format!(
            "approval_queued label={} request={} command_bytes={}",
            request.label,
            request.request_id,
            request.command.len()
        ));
        self.queue.push_back(PendingRequest {
            request,
            ui_retries: 0,
        });
        self.ensure_prompt();
    }

    fn handle_attach(
        &mut self,
        token: u64,
        hello_id: &str,
        label: String,
        writer: Arc<Mutex<UnixStream>>,
        control: UnixStream,
    ) {
        let notice_only = self.launch.for_notice
            && self.launch.attempts > 0
            && self.active.is_none()
            && self.queue.is_empty();
        let hello = Response::hello(hello_id, notice_only.then_some("notice"));
        let written = {
            let mut stream = writer.lock().unwrap_or_else(|error| error.into_inner());
            write_response(&mut *stream, &hello)
        };
        if let Err(error) = written {
            self.log(&format!(
                "ui_hello_write_failed token={token} error={error}"
            ));
            let _ = control.shutdown(std::net::Shutdown::Both);
            return;
        }
        if let Some(old) = self.ui.take() {
            let _ = old.control.shutdown(std::net::Shutdown::Both);
            self.log(&format!(
                "ui_replaced old_token={} new_token={token}",
                old.token
            ));
        }
        if let Some(active) = self.active.take() {
            self.queue.push_front(PendingRequest {
                request: active.request,
                ui_retries: active.ui_retries,
            });
            self.log("prompt_requeued_after_ui_attach");
        }
        self.ui = Some(UiClient {
            token,
            writer,
            control,
        });
        self.launch = LaunchState::default();
        self.log(&format!(
            "ui_attached token={token} label={label} notice_only={notice_only}"
        ));
        self.ensure_prompt();
        self.flush_notices();
    }

    fn handle_notice(&mut self, label: &str, command: &str, first_use_only: bool) {
        let now = Instant::now();
        let previous = self.last_exec.insert(label.to_owned(), now);
        if first_use_only && previous.is_some_and(|at| now - at < FIRST_USE_IDLE) {
            return;
        }
        if let Some(entry) = self.notices.iter_mut().find(|entry| entry.label == label) {
            entry.count = entry.count.saturating_add(1);
            entry.set_command(command);
        } else if self.notices.len() < MAX_NOTICE_PACKAGES {
            self.notices.push(NoticeEntry::new(label, command));
        }
        self.flush_notices();
    }

    /// Near real-time: the first granted exec is sent at once; execs within
    /// the next `notice_interval` are coalesced per package into one update,
    /// which only caps the message rate during bursts. The UI keeps a single
    /// toast and updates it in place.
    fn flush_notices(&mut self) {
        let now = Instant::now();
        if now < self.notice_next_at || self.notices.is_empty() {
            return;
        }
        // A prompt owns the screen; hold notices until it is decided.
        if self.active.is_some() || !self.queue.is_empty() {
            return;
        }
        let Some(ui) = &self.ui else {
            self.launch_if_needed();
            return;
        };
        let notices = std::mem::take(&mut self.notices);
        let summary = notices
            .iter()
            .map(|entry| format!("{}x{}", entry.label, entry.count))
            .collect::<Vec<_>>()
            .join(",");
        let message = NoticeMessage::new(notices.clone());
        let written = {
            let mut writer = ui.writer.lock().unwrap_or_else(|error| error.into_inner());
            write_json(&mut *writer, &message)
        };
        match written {
            Ok(()) => {
                self.notice_retries = 0;
                self.notice_next_at = now + self.config.notice_interval;
                self.log(&format!("notice_sent {summary}"));
            }
            Err(error) => {
                if let Some(dead) = self.ui.take() {
                    let _ = dead.control.shutdown(std::net::Shutdown::Both);
                }
                self.launch = LaunchState {
                    last_attempt: Some(now),
                    ..LaunchState::default()
                };
                if self.notice_retries < 1 {
                    self.notice_retries += 1;
                    self.notices = notices;
                    self.log(&format!(
                        "notice_write_failed error={error} requeued={summary}"
                    ));
                } else {
                    self.notice_retries = 0;
                    self.notice_next_at = now + NOTICE_RETRY_AFTER_FAILURE;
                    self.log(&format!(
                        "notice_write_failed error={error} dropped={summary}"
                    ));
                }
            }
        }
    }

    fn handle_detach(&mut self, token: u64) {
        if self.ui.as_ref().map(|ui| ui.token) != Some(token) {
            return;
        }
        self.ui = None;
        self.log(&format!("ui_detached token={token}"));
        if let Some(active) = self.active.take() {
            if active.ui_retries < MAX_PROMPT_UI_RETRIES {
                // The UI may simply have been exiting after the previous
                // decision; show the prompt again via a fresh UI.
                self.queue.push_front(PendingRequest {
                    request: active.request,
                    ui_retries: active.ui_retries + 1,
                });
                self.log("prompt_requeued_after_ui_disconnect");
            } else {
                self.reply(
                    active.request,
                    ApprovalResult::Denied("ui_unavailable", "ui_disconnected"),
                );
                self.log("prompt_cancelled=ui_disconnected");
            }
        }
        self.launch = LaunchState {
            last_attempt: Some(Instant::now()),
            ..LaunchState::default()
        };
    }

    fn handle_cancel(&mut self, ticket: u64) {
        self.queue
            .retain(|pending| pending.request.ticket != ticket);
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.request.ticket == ticket)
        {
            self.active = None;
            self.log("prompt_cancelled=requester_gone");
        }
        self.ensure_prompt();
    }

    fn handle_decision(
        &mut self,
        token: u64,
        id: &str,
        prompt_id: &str,
        nonce: &str,
        decision: Decision,
    ) {
        let valid = match (&self.ui, &self.active) {
            (Some(ui), Some(active)) => {
                ui.token == token
                    && active.ui_token == token
                    && active.prompt_id == prompt_id
                    && active.nonce == nonce
                    && Instant::now() <= active.expires_at
            }
            _ => false,
        };
        if !valid {
            self.log(&format!(
                "prompt_decision_rejected token={token} request={id} reason=stale_or_expired"
            ));
            self.send_to_ui(token, &Response::error(id, "error", "stale_prompt"));
            return;
        }

        let active = self
            .active
            .take()
            .expect("validated active prompt must exist");
        let mut ui_error = None;
        let result = match decision {
            Decision::Deny => ApprovalResult::Denied("denied", "user_denied"),
            Decision::Once => ApprovalResult::AllowOnce,
            Decision::Always => {
                let granted = self
                    .grants
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .grant(&active.request.label);
                match granted {
                    Ok(_) => ApprovalResult::AllowAlways,
                    Err(GrantError::PackageIdentity(error)) => {
                        self.log(&format!(
                            "grant_identity_unavailable label={} error={error}",
                            active.request.label
                        ));
                        ui_error = Some("package_identity_unavailable");
                        ApprovalResult::Denied("denied", "package_identity_unavailable")
                    }
                    Err(error) => {
                        self.log(&format!(
                            "grant_persist_failed label={} error={error}",
                            active.request.label
                        ));
                        ui_error = Some("grant_persist_failed");
                        ApprovalResult::Denied("denied", "grant_persist_failed")
                    }
                }
            }
        };
        match ui_error {
            Some(error) => {
                self.send_to_ui(token, &Response::error(id, "error", error));
            }
            None => {
                self.send_to_ui(token, &Response::ok(id));
            }
        }
        self.log(&format!(
            "prompt_decision label={} request={} decision={:?}",
            active.request.label, active.request.request_id, decision
        ));
        self.reply(active.request, result);
        self.ensure_prompt();
    }

    fn tick(&mut self) {
        if let Some(active) = &self.active {
            if Instant::now() >= active.expires_at {
                let active = self.active.take().expect("active prompt exists");
                self.log(&format!(
                    "prompt_timeout request={} label={}",
                    active.request.request_id, active.request.label
                ));
                self.reply(
                    active.request,
                    ApprovalResult::Denied("denied", "prompt_timeout"),
                );
            }
        }
        self.ensure_prompt();
    }

    fn ensure_prompt(&mut self) {
        if self.active.is_some() {
            return;
        }
        if self.queue.is_empty() {
            if self.notices.is_empty() {
                self.launch = LaunchState::default();
            }
            return;
        }
        if self.ui.is_none() {
            self.launch_if_needed();
            return;
        }

        let ui = self.ui.take().expect("ui is connected");
        let pending = self.queue.pop_front().expect("queue is non-empty");
        let prompt_id = match random_token() {
            Ok(token) => token,
            Err(error) => {
                self.log(&format!("random_token_failed={error}"));
                self.reply(
                    pending.request,
                    ApprovalResult::Denied("error", "approval_internal"),
                );
                self.ui = Some(ui);
                return;
            }
        };
        let nonce = match random_token() {
            Ok(token) => token,
            Err(error) => {
                self.log(&format!("random_token_failed={error}"));
                self.reply(
                    pending.request,
                    ApprovalResult::Denied("error", "approval_internal"),
                );
                self.ui = Some(ui);
                return;
            }
        };
        let timeout_ms = self
            .config
            .prompt_timeout
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let message = PromptMessage::new(
            prompt_id.clone(),
            nonce.clone(),
            &pending.request.request_id,
            &pending.request.label,
            &pending.request.command,
            timeout_ms,
        );
        let write_result = {
            let mut writer = ui.writer.lock().unwrap_or_else(|error| error.into_inner());
            write_json(&mut *writer, &message)
        };
        match write_result {
            Ok(()) => {
                self.log(&format!(
                    "prompt_sent label={} request={} prompt={} ui_token={}",
                    pending.request.label, pending.request.request_id, prompt_id, ui.token
                ));
                self.active = Some(ActivePrompt {
                    prompt_id,
                    nonce,
                    request: pending.request,
                    ui_retries: pending.ui_retries,
                    expires_at: Instant::now() + self.config.prompt_timeout,
                    ui_token: ui.token,
                });
                self.ui = Some(ui);
            }
            Err(error) => {
                self.log(&format!(
                    "prompt_write_failed ui_token={} error={error}",
                    ui.token
                ));
                let _ = ui.control.shutdown(std::net::Shutdown::Both);
                self.launch = LaunchState {
                    last_attempt: Some(Instant::now()),
                    ..LaunchState::default()
                };
                if pending.ui_retries < MAX_PROMPT_UI_RETRIES {
                    self.queue.push_front(PendingRequest {
                        request: pending.request,
                        ui_retries: pending.ui_retries + 1,
                    });
                    self.log("prompt_requeued_after_ui_write_error");
                } else {
                    self.reply(
                        pending.request,
                        ApprovalResult::Denied("ui_unavailable", "ui_disconnected"),
                    );
                    self.log("prompt_cancelled=ui_disconnected");
                }
            }
        }
    }

    fn launch_if_needed(&mut self) {
        let now = Instant::now();
        if let Some(until) = self.launch.awaiting_until {
            if now < until {
                return;
            }
            self.launch.awaiting_until = None;
        }
        if self.launch.attempts >= self.config.max_launch_attempts {
            self.fail_all("ui_unavailable");
            return;
        }
        if let Some(last) = self.launch.last_attempt {
            if now < last + self.config.launch_cooldown {
                return;
            }
        }
        match self.spawn_launcher() {
            Ok(()) => {
                self.launch.attempts += 1;
                self.launch.last_attempt = Some(now);
                self.launch.awaiting_until = Some(now + self.config.ui_launch_timeout);
                self.launch.for_notice = self.queue.is_empty();
                self.log(&format!(
                    "ui_launch_attempt={} app={}",
                    self.launch.attempts, self.config.ui_app_id
                ));
            }
            Err(error) => {
                self.log(&format!("ui_launch_failed={error}"));
                self.launch.attempts = self.config.max_launch_attempts;
                self.fail_all("ui_launch_failed");
            }
        }
    }

    fn spawn_launcher(&self) -> io::Result<()> {
        if !self.config.app_launcher.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("launcher not found: {}", self.config.app_launcher.display()),
            ));
        }
        let mut child = Command::new(&self.config.app_launcher)
            .arg("-s")
            .arg(&self.config.ui_app_id)
            // Inherit the systemd environment (notably XDG_RUNTIME_DIR) but
            // make the two values AUL commonly needs explicit.
            .env("HOME", "/root")
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let logger = self.logger.clone();
        let app_id = self.config.ui_app_id.clone();
        thread::Builder::new()
            .name("tv-auth-ui-launch".into())
            .spawn(move || {
                let status = child.wait();
                if let Some(logger) = logger {
                    let message = match status {
                        Ok(status) => format!("ui_launch_exit app={app_id} status={status}"),
                        Err(error) => {
                            format!("ui_launch_wait_error app={app_id} error={error}")
                        }
                    };
                    let _ = logger.log(&message);
                }
            })?;
        Ok(())
    }

    fn has_pending(&self, label: &str) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.request.label == label)
            || self
                .queue
                .iter()
                .any(|pending| pending.request.label == label)
    }

    fn fail_all(&mut self, error: &'static str) {
        while let Some(pending) = self.queue.pop_front() {
            self.reply(
                pending.request,
                ApprovalResult::Denied("ui_unavailable", error),
            );
        }
        if !self.notices.is_empty() {
            // Best effort: never retry a broken UI for every granted exec.
            self.notices.clear();
            self.notice_next_at = Instant::now() + NOTICE_RETRY_AFTER_FAILURE;
            self.log(&format!("notices_dropped={error}"));
        }
        self.launch = LaunchState::default();
    }

    fn reply(&self, request: ApprovalRequest, result: ApprovalResult) {
        let _ = request.reply.send(result);
    }

    fn send_to_ui(&self, token: u64, response: &Response) -> bool {
        let Some(ui) = self.ui.as_ref().filter(|ui| ui.token == token) else {
            return false;
        };
        let mut writer = ui.writer.lock().unwrap_or_else(|error| error.into_inner());
        write_response(&mut *writer, response).is_ok()
    }

    fn log(&self, message: &str) {
        if let Some(logger) = &self.logger {
            let _ = logger.log(message);
        }
    }
}

pub fn random_token() -> io::Result<String> {
    use std::fmt::Write as _;

    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| io::Error::other("getrandom failed"))?;
    let mut token = String::with_capacity(32);
    for byte in bytes {
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::time::Duration;

    struct Fixture {
        _directory: tempfile::TempDir,
        grants: Arc<Mutex<GrantStore>>,
        manager: ApprovalManager,
    }

    fn fixture(config: ApprovalConfig) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let apps = directory.path().join("apps");
        std::fs::create_dir(&apps).unwrap();
        let package = apps.join("org.example.app");
        std::fs::create_dir(&package).unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o755)).unwrap();
        let signature = package.join("author-signature.xml");
        std::fs::write(&signature, b"approval-test-signature").unwrap();
        std::fs::set_permissions(&signature, std::fs::Permissions::from_mode(0o644)).unwrap();
        let path = directory.path().join("grants.json");
        std::fs::write(&path, r#"{"version":2,"grants":{}}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let grants = Arc::new(Mutex::new(
            GrantStore::load_with_roots(&path, vec![apps]).unwrap(),
        ));
        let manager = ApprovalManager::start(config, Arc::clone(&grants), None).unwrap();
        Fixture {
            _directory: directory,
            grants,
            manager,
        }
    }

    fn test_config() -> ApprovalConfig {
        ApprovalConfig {
            ui_app_id: "org.tvauth.ui".into(),
            app_launcher: PathBuf::from("/nonexistent/tv-auth-launcher"),
            prompt_timeout: Duration::from_secs(5),
            ui_launch_timeout: Duration::from_millis(100),
            launch_cooldown: Duration::from_millis(10),
            prompt_queue_max: 4,
            max_launch_attempts: 1,
            notice_interval: Duration::from_millis(300),
        }
    }

    fn attach_ui(manager: &ApprovalManager, token: u64) -> BufReader<UnixStream> {
        let (ui, daemon) = UnixStream::pair().unwrap();
        ui.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let control = daemon.try_clone().unwrap();
        let writer = Arc::new(Mutex::new(daemon));
        manager
            .handle()
            .attach_ui(
                token,
                "hello-1".into(),
                "User::Pkg::org.tvauth.ui".into(),
                writer,
                control,
            )
            .unwrap();
        let mut reader = BufReader::new(ui);
        let hello = read_json(&mut reader);
        assert_eq!(hello["id"], "hello-1");
        assert_eq!(hello["status"], "ok");
        reader
    }

    fn request(label: &str) -> (ApprovalRequest, mpsc::Receiver<ApprovalResult>) {
        let (tx, rx) = mpsc::channel();
        (
            ApprovalRequest {
                ticket: 0,
                request_id: "request-1".into(),
                label: label.into(),
                command: "id".into(),
                reply: tx,
            },
            rx,
        )
    }

    fn read_json(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn allow_once_executes_only_the_pending_request() {
        let fixture = fixture(test_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();

        let prompt = read_json(&mut ui);
        assert_eq!(prompt["type"], "ui.prompt");
        let prompt_id = prompt["prompt_id"].as_str().unwrap().to_owned();
        let nonce = prompt["nonce"].as_str().unwrap().to_owned();
        fixture
            .manager
            .handle()
            .decide(1, "decision-1".into(), prompt_id, nonce, Decision::Once)
            .unwrap();
        assert_eq!(read_json(&mut ui)["status"], "ok");
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::AllowOnce)
        );
        assert_eq!(
            fixture
                .grants
                .lock()
                .unwrap()
                .check("User::Pkg::org.example.app"),
            crate::grants::GrantCheck::NotGranted
        );
    }

    #[test]
    fn always_persists_a_grant() {
        let fixture = fixture(test_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        let prompt = read_json(&mut ui);
        fixture
            .manager
            .handle()
            .decide(
                1,
                "decision-1".into(),
                prompt["prompt_id"].as_str().unwrap().to_owned(),
                prompt["nonce"].as_str().unwrap().to_owned(),
                Decision::Always,
            )
            .unwrap();
        assert_eq!(read_json(&mut ui)["status"], "ok");
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::AllowAlways)
        );
        assert_eq!(
            fixture
                .grants
                .lock()
                .unwrap()
                .check("User::Pkg::org.example.app"),
            crate::grants::GrantCheck::Allowed
        );
    }

    #[test]
    fn always_denies_when_package_identity_is_missing() {
        let fixture = fixture(test_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let (ask, reply) = request("User::Pkg::org.example.missing");
        fixture.manager.handle().request(ask).unwrap();
        let prompt = read_json(&mut ui);
        fixture
            .manager
            .handle()
            .decide(
                1,
                "decision-1".into(),
                prompt["prompt_id"].as_str().unwrap().to_owned(),
                prompt["nonce"].as_str().unwrap().to_owned(),
                Decision::Always,
            )
            .unwrap();
        let response = read_json(&mut ui);
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "package_identity_unavailable");
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::Denied(
                "denied",
                "package_identity_unavailable"
            ))
        );
        assert_eq!(
            fixture
                .grants
                .lock()
                .unwrap()
                .check("User::Pkg::org.example.missing"),
            crate::grants::GrantCheck::NotGranted
        );
    }

    #[test]
    fn deny_never_allows() {
        let fixture = fixture(test_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        let prompt = read_json(&mut ui);
        fixture
            .manager
            .handle()
            .decide(
                1,
                "decision-1".into(),
                prompt["prompt_id"].as_str().unwrap().to_owned(),
                prompt["nonce"].as_str().unwrap().to_owned(),
                Decision::Deny,
            )
            .unwrap();
        assert_eq!(read_json(&mut ui)["status"], "ok");
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::Denied("denied", "user_denied"))
        );
    }

    #[test]
    fn stale_nonce_is_rejected() {
        let fixture = fixture(test_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        let prompt = read_json(&mut ui);
        fixture
            .manager
            .handle()
            .decide(
                1,
                "decision-1".into(),
                prompt["prompt_id"].as_str().unwrap().to_owned(),
                "00".repeat(16),
                Decision::Once,
            )
            .unwrap();
        assert_eq!(read_json(&mut ui)["error"], "stale_prompt");
        assert!(reply.recv_timeout(Duration::from_millis(150)).is_err());
    }

    #[test]
    fn timeout_denies_and_never_allows() {
        let mut config = test_config();
        config.prompt_timeout = Duration::from_millis(200);
        let fixture = fixture(config);
        let mut ui = attach_ui(&fixture.manager, 1);
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        let _prompt = read_json(&mut ui);
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::Denied("denied", "prompt_timeout"))
        );
    }

    fn no_relaunch_config() -> ApprovalConfig {
        let mut config = test_config();
        // Keep the (missing) launcher from failing the queue while a test
        // swaps UI connections.
        config.launch_cooldown = Duration::from_secs(60);
        config
    }

    #[test]
    fn ui_disconnect_requeues_prompt_for_next_ui() {
        let fixture = fixture(no_relaunch_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        let _ = read_json(&mut ui);
        fixture.manager.handle().detach_ui(1).unwrap();

        let mut ui = attach_ui(&fixture.manager, 2);
        let prompt = read_json(&mut ui);
        assert_eq!(prompt["type"], "ui.prompt");
        fixture
            .manager
            .handle()
            .decide(
                2,
                "decision-1".into(),
                prompt["prompt_id"].as_str().unwrap().to_owned(),
                prompt["nonce"].as_str().unwrap().to_owned(),
                Decision::Once,
            )
            .unwrap();
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::AllowOnce)
        );
    }

    #[test]
    fn ui_disconnect_denies_after_retry_limit() {
        let fixture = fixture(no_relaunch_config());
        let (ask, reply) = request("User::Pkg::org.example.app");
        let mut ui = attach_ui(&fixture.manager, 1);
        fixture.manager.handle().request(ask).unwrap();
        for token in 1..=u64::from(MAX_PROMPT_UI_RETRIES) + 1 {
            if token > 1 {
                ui = attach_ui(&fixture.manager, token);
            }
            let _ = read_json(&mut ui);
            assert!(reply.try_recv().is_err());
            fixture.manager.handle().detach_ui(token).unwrap();
        }
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::Denied("ui_unavailable", "ui_disconnected"))
        );
    }

    #[test]
    fn cancel_drops_active_and_queued_requests() {
        let fixture = fixture(no_relaunch_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let handle = fixture.manager.handle();
        let (first, _first_reply) = request("User::Pkg::org.example.one");
        let first_ticket = handle.request(first).unwrap();
        let prompt = read_json(&mut ui);
        let (second, _second_reply) = request("User::Pkg::org.example.two");
        let second_ticket = handle.request(second).unwrap();

        handle.cancel(first_ticket).unwrap();
        // The queued request is promoted once the active one is cancelled.
        let next = read_json(&mut ui);
        assert_eq!(next["label"], "User::Pkg::org.example.two");

        // The cancelled prompt can no longer be approved.
        handle
            .decide(
                1,
                "decision-1".into(),
                prompt["prompt_id"].as_str().unwrap().to_owned(),
                prompt["nonce"].as_str().unwrap().to_owned(),
                Decision::Once,
            )
            .unwrap();
        assert_eq!(read_json(&mut ui)["error"], "stale_prompt");

        handle.cancel(second_ticket).unwrap();
        let (third, _third_reply) = request("User::Pkg::org.example.two");
        handle.request(third).unwrap();
        // Label is free again, so the new request is prompted, not rejected.
        assert_eq!(read_json(&mut ui)["label"], "User::Pkg::org.example.two");
    }

    #[test]
    fn duplicate_pending_label_is_rejected() {
        let fixture = fixture(test_config());
        let _ui = attach_ui(&fixture.manager, 1);
        let (ask, _reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        let (second, second_reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(second).unwrap();
        assert_eq!(
            second_reply.recv_timeout(Duration::from_secs(1)),
            Ok(ApprovalResult::Denied("busy", "request_already_pending"))
        );
    }

    #[test]
    fn prompt_queue_full_is_rejected() {
        let mut config = test_config();
        config.prompt_queue_max = 2;
        let fixture = fixture(config);
        let _ui = attach_ui(&fixture.manager, 1);
        let (first, _first_reply) = request("User::Pkg::org.example.one");
        fixture.manager.handle().request(first).unwrap();
        let (second, _second_reply) = request("User::Pkg::org.example.two");
        fixture.manager.handle().request(second).unwrap();
        let (third, third_reply) = request("User::Pkg::org.example.three");
        fixture.manager.handle().request(third).unwrap();
        assert_eq!(
            third_reply.recv_timeout(Duration::from_secs(1)),
            Ok(ApprovalResult::Denied("busy", "prompt_queue_full"))
        );
    }

    #[test]
    fn missing_launcher_fails_closed() {
        let fixture = fixture(test_config());
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::Denied("ui_unavailable", "ui_launch_failed"))
        );
    }

    #[test]
    fn notices_are_immediate_then_coalesced() {
        let fixture = fixture(test_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let handle = fixture.manager.handle();
        let one = "User::Pkg::org.example.one";
        let two = "User::Pkg::org.example.two";

        handle.notice(one.into(), "first".into(), false).unwrap();
        let notice = read_json(&mut ui);
        assert_eq!(notice["type"], "ui.notice");
        assert_eq!(notice["notices"][0]["package"], "org.example.one");
        assert_eq!(notice["notices"][0]["count"], 1);

        // Within the gap: nothing is sent until it elapses, then one update.
        let started = Instant::now();
        for command in ["second", "third"] {
            handle.notice(one.into(), command.into(), false).unwrap();
        }
        handle
            .notice(two.into(), "other\u{202e}".into(), false)
            .unwrap();
        let batch = read_json(&mut ui);
        assert!(started.elapsed() >= Duration::from_millis(200));
        let notices = batch["notices"].as_array().unwrap();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0]["count"], 2);
        assert_eq!(notices[0]["command"], "third");
        assert_eq!(notices[1]["package"], "org.example.two");
        assert_eq!(notices[1]["command"], "other\u{FFFD}");
    }

    #[test]
    fn notices_wait_for_an_active_prompt() {
        let fixture = fixture(test_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let handle = fixture.manager.handle();
        let (ask, _reply) = request("User::Pkg::org.example.asking");
        handle.request(ask).unwrap();
        let prompt = read_json(&mut ui);
        handle
            .notice("User::Pkg::org.example.granted".into(), "id".into(), false)
            .unwrap();
        handle
            .decide(
                1,
                "decision-1".into(),
                prompt["prompt_id"].as_str().unwrap().to_owned(),
                prompt["nonce"].as_str().unwrap().to_owned(),
                Decision::Deny,
            )
            .unwrap();
        assert_eq!(read_json(&mut ui)["status"], "ok");
        assert_eq!(read_json(&mut ui)["type"], "ui.notice");
    }

    #[test]
    fn notice_only_launch_is_flagged_in_hello() {
        let mut config = test_config();
        config.app_launcher = PathBuf::from("/usr/bin/true");
        config.ui_launch_timeout = Duration::from_secs(5);
        let fixture = fixture(config);
        let handle = fixture.manager.handle();
        handle
            .notice("User::Pkg::org.example.granted".into(), "id".into(), false)
            .unwrap();
        // Let the manager spawn the (dummy) launcher before the UI attaches.
        thread::sleep(Duration::from_millis(100));

        let (ui, daemon) = UnixStream::pair().unwrap();
        ui.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let control = daemon.try_clone().unwrap();
        handle
            .attach_ui(
                1,
                "hello-1".into(),
                "User::Pkg::org.tvauth.ui".into(),
                Arc::new(Mutex::new(daemon)),
                control,
            )
            .unwrap();
        let mut ui = BufReader::new(ui);
        assert_eq!(read_json(&mut ui)["launch"], "notice");
        assert_eq!(read_json(&mut ui)["type"], "ui.notice");
    }

    #[test]
    fn first_use_only_shows_once_while_active() {
        let fixture = fixture(test_config());
        let mut ui = attach_ui(&fixture.manager, 1);
        let handle = fixture.manager.handle();
        let quiet = "User::Pkg::org.example.quiet";
        handle.notice(quiet.into(), "poll".into(), true).unwrap();
        assert_eq!(
            read_json(&mut ui)["notices"][0]["package"],
            "org.example.quiet"
        );

        // Still active: suppressed, while an every-mode package is not.
        handle.notice(quiet.into(), "poll".into(), true).unwrap();
        handle
            .notice("User::Pkg::org.example.loud".into(), "id".into(), false)
            .unwrap();
        let next = read_json(&mut ui);
        let notices = next["notices"].as_array().unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0]["package"], "org.example.loud");
    }

    #[test]
    fn prompt_write_failure_counts_toward_retry_limit() {
        let fixture = fixture(no_relaunch_config());
        let handle = fixture.manager.handle();
        let dead_ui = attach_ui(&fixture.manager, 1);
        drop(dead_ui.into_inner());

        // First attempt hits prompt_write_failed on dead_ui (ui_retries: 0 -> 1).
        let (ask, reply) = request("User::Pkg::org.example.app");
        handle.request(ask).unwrap();

        // Only MAX_PROMPT_UI_RETRIES further disconnects are needed to exhaust retries.
        for token in 2..=u64::from(MAX_PROMPT_UI_RETRIES) + 1 {
            let mut ui = attach_ui(&fixture.manager, token);
            let _ = read_json(&mut ui);
            assert!(reply.try_recv().is_err());
            handle.detach_ui(token).unwrap();
        }
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::Denied("ui_unavailable", "ui_disconnected"))
        );
    }

    #[test]
    fn notice_write_failure_requeues_for_next_ui() {
        let fixture = fixture(no_relaunch_config());
        let handle = fixture.manager.handle();
        let dead_ui = attach_ui(&fixture.manager, 1);
        drop(dead_ui.into_inner());

        handle
            .notice("User::Pkg::org.example.app".into(), "id".into(), false)
            .unwrap();

        let mut next_ui = attach_ui(&fixture.manager, 2);
        let notice = read_json(&mut next_ui);
        assert_eq!(notice["type"], "ui.notice");
        assert_eq!(notice["notices"][0]["package"], "org.example.app");
    }
}
