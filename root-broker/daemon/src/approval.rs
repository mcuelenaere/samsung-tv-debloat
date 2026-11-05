use crate::grants::GrantStore;
use crate::logger::Logger;
use crate::protocol::{write_json, write_response, Decision, PromptMessage, Response};
use std::collections::VecDeque;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TICK_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct ApprovalConfig {
    pub ui_label: String,
    pub ui_app_id: String,
    pub app_launcher: PathBuf,
    pub prompt_timeout: Duration,
    pub ui_launch_timeout: Duration,
    pub launch_cooldown: Duration,
    pub prompt_queue_max: usize,
    pub max_launch_attempts: usize,
}

impl ApprovalConfig {
    pub fn approval_wait_timeout(&self) -> Duration {
        self.prompt_timeout * (self.prompt_queue_max as u32 + 1)
            + self.ui_launch_timeout * (self.max_launch_attempts as u32)
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
    pub request_id: String,
    pub label: String,
    pub command: String,
    pub reply: Sender<ApprovalResult>,
}

enum ApprovalCommand {
    Request(ApprovalRequest),
    AttachUi {
        token: u64,
        label: String,
        writer: Arc<Mutex<UnixStream>>,
        control: UnixStream,
    },
    DetachUi {
        token: u64,
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
    tx: Arc<Mutex<Sender<ApprovalCommand>>>,
}

impl ApprovalHandle {
    pub fn request(&self, request: ApprovalRequest) -> io::Result<()> {
        self.send(ApprovalCommand::Request(request))
    }

    pub fn attach_ui(
        &self,
        token: u64,
        label: String,
        writer: Arc<Mutex<UnixStream>>,
        control: UnixStream,
    ) -> io::Result<()> {
        self.send(ApprovalCommand::AttachUi {
            token,
            label,
            writer,
            control,
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
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
            tx: Arc::new(Mutex::new(tx)),
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
        let _ = self
            .handle
            .tx
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .send(ApprovalCommand::Shutdown);
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
}

struct ActivePrompt {
    prompt_id: String,
    nonce: String,
    request: ApprovalRequest,
    expires_at: Instant,
    ui_token: u64,
}

#[derive(Default)]
struct LaunchState {
    attempts: usize,
    last_attempt: Option<Instant>,
    awaiting_until: Option<Instant>,
}

struct Manager {
    config: ApprovalConfig,
    grants: Arc<Mutex<GrantStore>>,
    logger: Option<Arc<Logger>>,
    ui: Option<UiClient>,
    queue: VecDeque<PendingRequest>,
    active: Option<ActivePrompt>,
    launch: LaunchState,
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
                label,
                writer,
                control,
            } => self.handle_attach(token, label, writer, control),
            ApprovalCommand::DetachUi { token } => self.handle_detach(token),
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
        self.queue.push_back(PendingRequest { request });
        self.ensure_prompt();
    }

    fn handle_attach(
        &mut self,
        token: u64,
        label: String,
        writer: Arc<Mutex<UnixStream>>,
        control: UnixStream,
    ) {
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
            });
            self.log("prompt_requeued_after_ui_attach");
        }
        self.ui = Some(UiClient {
            token,
            writer,
            control,
        });
        self.launch = LaunchState::default();
        self.log(&format!("ui_attached token={token} label={label}"));
        self.ensure_prompt();
    }

    fn handle_detach(&mut self, token: u64) {
        if self.ui.as_ref().map(|ui| ui.token) != Some(token) {
            return;
        }
        self.ui = None;
        self.log(&format!("ui_detached token={token}"));
        if let Some(active) = self.active.take() {
            self.reply(
                active.request,
                ApprovalResult::Denied("ui_unavailable", "ui_disconnected"),
            );
            self.log("prompt_cancelled=ui_disconnected");
        }
        self.launch = LaunchState {
            attempts: 0,
            last_attempt: Some(Instant::now()),
            awaiting_until: None,
        };
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
                    Err(error) => {
                        self.log(&format!(
                            "grant_persist_failed label={} error={error}",
                            active.request.label
                        ));
                        ApprovalResult::Denied("denied", "grant_persist_failed")
                    }
                }
            }
        };
        self.send_to_ui(token, &Response::ok(id));
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
            self.launch = LaunchState::default();
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
                self.queue.push_front(pending);
                let _ = ui.control.shutdown(std::net::Shutdown::Both);
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
        let path = directory.path().join("grants.json");
        std::fs::write(&path, r#"{"version":1,"grants":{}}"#).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let grants = Arc::new(Mutex::new(GrantStore::load(&path).unwrap()));
        let manager = ApprovalManager::start(config, Arc::clone(&grants), None).unwrap();
        Fixture {
            _directory: directory,
            grants,
            manager,
        }
    }

    fn test_config() -> ApprovalConfig {
        ApprovalConfig {
            ui_label: "User::Pkg::org.tvauth.ui".into(),
            ui_app_id: "org.tvauth.ui".into(),
            app_launcher: PathBuf::from("/nonexistent/tv-auth-launcher"),
            prompt_timeout: Duration::from_secs(5),
            ui_launch_timeout: Duration::from_millis(100),
            launch_cooldown: Duration::from_millis(10),
            prompt_queue_max: 4,
            max_launch_attempts: 1,
        }
    }

    fn attach_ui(manager: &ApprovalManager, token: u64) -> BufReader<UnixStream> {
        let (ui, daemon) = UnixStream::pair().unwrap();
        ui.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let control = daemon.try_clone().unwrap();
        let writer = Arc::new(Mutex::new(daemon));
        manager
            .handle()
            .attach_ui(token, "User::Pkg::org.tvauth.ui".into(), writer, control)
            .unwrap();
        BufReader::new(ui)
    }

    fn request(label: &str) -> (ApprovalRequest, mpsc::Receiver<ApprovalResult>) {
        let (tx, rx) = mpsc::channel();
        (
            ApprovalRequest {
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
        assert!(!fixture
            .grants
            .lock()
            .unwrap()
            .is_allowed("User::Pkg::org.example.app"));
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
        assert!(fixture
            .grants
            .lock()
            .unwrap()
            .is_allowed("User::Pkg::org.example.app"));
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

    #[test]
    fn ui_disconnect_denies() {
        let fixture = fixture(test_config());
        let _ui = attach_ui(&fixture.manager, 1);
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        fixture.manager.handle().detach_ui(1).unwrap();
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::Denied("ui_unavailable", "ui_disconnected"))
        );
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
    fn missing_launcher_fails_closed() {
        let fixture = fixture(test_config());
        let (ask, reply) = request("User::Pkg::org.example.app");
        fixture.manager.handle().request(ask).unwrap();
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(2)),
            Ok(ApprovalResult::Denied("ui_unavailable", "ui_launch_failed"))
        );
    }
}
