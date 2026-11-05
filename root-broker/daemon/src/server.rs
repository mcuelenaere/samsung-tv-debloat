use crate::approval::{ApprovalHandle, ApprovalRequest, ApprovalResult};
use crate::executor;
use crate::grants::GrantStore;
use crate::identity::{package_from_label, PeerIdentity};
use crate::logger::Logger;
use crate::protocol::{
    is_valid_request_id, parse_request, read_message, validate_decision, validate_exec,
    validate_revoke, write_response, ExecRequest, GrantView, ParseError, Request, Response,
    MAX_REQUEST_BYTES,
};
use std::collections::HashSet;
use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub struct Broker {
    pub grants: Arc<Mutex<GrantStore>>,
    pub approvals: ApprovalHandle,
    pub logger: Option<Arc<Logger>>,
    pub next_ui_token: AtomicU64,
}

impl Broker {
    pub fn new(
        grants: Arc<Mutex<GrantStore>>,
        approvals: ApprovalHandle,
        logger: Option<Arc<Logger>>,
    ) -> Self {
        Self {
            grants,
            approvals,
            logger,
            next_ui_token: AtomicU64::new(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub max_connections: usize,
    pub max_requests_per_connection: usize,
    pub ui_label: String,
    pub ui_uid: u32,
    pub approval_wait_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_connections: 16,
            max_requests_per_connection: 1024,
            ui_label: "User::Pkg::org.tvauth.ui".into(),
            ui_uid: 5001,
            approval_wait_timeout: Duration::from_secs(300),
        }
    }
}

pub fn handle_connection(
    stream: UnixStream,
    identity: PeerIdentity,
    broker: Arc<Broker>,
    config: &ServerConfig,
) -> io::Result<()> {
    let is_ui = identity.label == config.ui_label && identity.uid == config.ui_uid;
    let idle_timeout = if is_ui {
        Duration::from_secs(600)
    } else {
        Duration::from_secs(30)
    };
    stream.set_read_timeout(Some(idle_timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let reader_stream = stream.try_clone()?;
    let control = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let writer = Arc::new(Mutex::new(stream));
    let mut request_ids = HashSet::new();
    let mut ui_token: Option<u64> = None;

    let result = loop {
        let line = match read_message(&mut reader, MAX_REQUEST_BYTES) {
            Ok(line) => line,
            Err(error) if is_idle_or_closed(&error) => break Ok(()),
            Err(error) => {
                let _ = send(&writer, &Response::error("", "error", "malformed_request"));
                break Err(error);
            }
        };

        let request = match parse_request(&line) {
            Ok(request) => request,
            Err(ParseError::Malformed) => {
                log(
                    &broker,
                    &format!(
                        "requester={} uid={} pid={} malformed_request",
                        identity.label, identity.uid, identity.pid
                    ),
                );
                let _ = send(&writer, &Response::error("", "error", "malformed_request"));
                break Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed request",
                ));
            }
            Err(error) => {
                let _ = send(&writer, &Response::error("", "error", error.code()));
                continue;
            }
        };

        let response_id = if is_valid_request_id(request.id()) {
            request.id().to_owned()
        } else {
            String::new()
        };
        if response_id.is_empty() {
            let _ = send(&writer, &Response::error("", "error", "invalid_request_id"));
            continue;
        }
        if !request_ids.insert(response_id.clone()) {
            let _ = send(
                &writer,
                &Response::error(&response_id, "error", "duplicate_request_id"),
            );
            continue;
        }

        let result = if is_ui {
            handle_ui_request(
                &writer,
                &identity,
                &broker,
                request,
                &mut ui_token,
                &control,
            )
        } else {
            handle_app_request(&writer, &identity, &broker, config, request)
        };
        if let Err(error) = result {
            break Err(error);
        }
    };

    if let Some(token) = ui_token {
        let _ = broker.approvals.detach_ui(token);
    }
    result
}

fn handle_ui_request(
    writer: &Arc<Mutex<UnixStream>>,
    identity: &PeerIdentity,
    broker: &Arc<Broker>,
    request: Request,
    ui_token: &mut Option<u64>,
    control: &UnixStream,
) -> io::Result<()> {
    match request {
        Request::UiHello(request) => {
            if ui_token.is_some() {
                send(
                    writer,
                    &Response::error(&request.id, "error", "already_registered"),
                )?;
                return Ok(());
            }
            let token = broker.next_ui_token.fetch_add(1, Ordering::Relaxed);
            let control = control.try_clone()?;
            match broker.approvals.attach_ui(
                token,
                identity.label.clone(),
                Arc::clone(writer),
                control,
            ) {
                Ok(()) => {
                    *ui_token = Some(token);
                    send(writer, &Response::ok(&request.id))?;
                }
                Err(error) => {
                    log(
                        broker,
                        &format!("ui_register_failed label={} error={error}", identity.label),
                    );
                    send(
                        writer,
                        &Response::error(&request.id, "error", "ui_register_failed"),
                    )?;
                }
            }
        }
        Request::UiDecision(request) => {
            let Some(token) = *ui_token else {
                send(
                    writer,
                    &Response::error(&request.id, "error", "not_registered"),
                )?;
                return Ok(());
            };
            if let Err(error) = validate_decision(&request) {
                send(writer, &Response::error(&request.id, "error", error))?;
                return Ok(());
            }
            log(
                broker,
                &format!(
                    "ui_decision_received label={} decision={:?}",
                    identity.label, request.decision
                ),
            );
            if let Err(error) = broker.approvals.decide(
                token,
                request.id.clone(),
                request.prompt_id,
                request.nonce,
                request.decision,
            ) {
                log(broker, &format!("decision_forward_failed error={error}"));
                send(
                    writer,
                    &Response::error(&request.id, "error", "approval_unavailable"),
                )?;
            }
        }
        Request::GrantsList(request) => {
            if ui_token.is_none() {
                send(
                    writer,
                    &Response::error(&request.id, "error", "not_registered"),
                )?;
                return Ok(());
            }
            log(broker, &format!("ui_grants_list label={}", identity.label));
            let grants = {
                let store = broker
                    .grants
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                store
                    .entries()
                    .map(|(label, granted_at)| GrantView {
                        label: label.to_owned(),
                        package: package_from_label(label).unwrap_or(label).to_owned(),
                        granted_at,
                    })
                    .collect::<Vec<_>>()
            };
            send(writer, &Response::grants(&request.id, grants))?;
        }
        Request::GrantsRevoke(request) => {
            if ui_token.is_none() {
                send(
                    writer,
                    &Response::error(&request.id, "error", "not_registered"),
                )?;
                return Ok(());
            }
            if let Err(error) = validate_revoke(&request) {
                send(writer, &Response::error(&request.id, "error", error))?;
                return Ok(());
            }
            let result = broker
                .grants
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .revoke(&request.label);
            match result {
                Ok(revoked) => {
                    log(
                        broker,
                        &format!("grant_revoked label={} revoked={revoked}", request.label),
                    );
                    send(writer, &Response::revoked(&request.id, revoked))?;
                }
                Err(error) => {
                    log(
                        broker,
                        &format!("grant_revoke_failed label={} error={error}", request.label),
                    );
                    send(
                        writer,
                        &Response::error(&request.id, "error", "grant_save_failed"),
                    )?;
                }
            }
        }
        Request::Exec(request) => {
            log(
                broker,
                &format!(
                    "requester={} uid={} pid={} request={} denied=ui_role",
                    identity.label, identity.uid, identity.pid, request.id
                ),
            );
            send(writer, &Response::error(&request.id, "denied", "ui_role"))?;
        }
    }
    Ok(())
}

fn handle_app_request(
    writer: &Arc<Mutex<UnixStream>>,
    identity: &PeerIdentity,
    broker: &Arc<Broker>,
    config: &ServerConfig,
    request: Request,
) -> io::Result<()> {
    match request {
        Request::Exec(request) => handle_exec(writer, identity, broker, config, request),
        Request::UiHello(request) => send(writer, &Response::error(&request.id, "error", "not_ui")),
        Request::UiDecision(request) => {
            send(writer, &Response::error(&request.id, "error", "not_ui"))
        }
        Request::GrantsList(request) => {
            send(writer, &Response::error(&request.id, "error", "not_ui"))
        }
        Request::GrantsRevoke(request) => {
            send(writer, &Response::error(&request.id, "error", "not_ui"))
        }
    }
}

fn handle_exec(
    writer: &Arc<Mutex<UnixStream>>,
    identity: &PeerIdentity,
    broker: &Arc<Broker>,
    config: &ServerConfig,
    request: ExecRequest,
) -> io::Result<()> {
    let timeout_ms = match validate_exec(&request) {
        Ok(timeout) => timeout,
        Err(error) => {
            send(writer, &Response::error(&request.id, "error", error))?;
            return Ok(());
        }
    };

    let allowed = broker
        .grants
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_allowed(&identity.label);

    if !allowed {
        log(
            broker,
            &format!(
                "requester={} uid={} pid={} request={} approval_required command_bytes={}",
                identity.label,
                identity.uid,
                identity.pid,
                request.id,
                request.command.len()
            ),
        );
        let (reply, decisions) = mpsc::channel();
        let approval = ApprovalRequest {
            request_id: request.id.clone(),
            label: identity.label.clone(),
            command: request.command.clone(),
            reply,
        };
        if broker.approvals.request(approval).is_err() {
            send(
                writer,
                &Response::error(&request.id, "ui_unavailable", "approval_unavailable"),
            )?;
            return Ok(());
        }
        match decisions.recv_timeout(config.approval_wait_timeout) {
            Ok(ApprovalResult::AllowOnce) | Ok(ApprovalResult::AllowAlways) => {
                log(
                    broker,
                    &format!(
                        "requester={} request={} approval=allowed",
                        identity.label, request.id
                    ),
                );
            }
            Ok(ApprovalResult::Denied(status, error)) => {
                log(
                    broker,
                    &format!(
                        "requester={} request={} approval=denied error={}",
                        identity.label, request.id, error
                    ),
                );
                send(writer, &Response::error(&request.id, status, error))?;
                return Ok(());
            }
            Err(_) => {
                log(
                    broker,
                    &format!(
                        "requester={} request={} approval=timeout",
                        identity.label, request.id
                    ),
                );
                send(
                    writer,
                    &Response::error(&request.id, "ui_unavailable", "approval_wait_timeout"),
                )?;
                return Ok(());
            }
        }
    }

    log(
        broker,
        &format!(
            "requester={} uid={} pid={} request={} command_bytes={} timeout_ms={} execute",
            identity.label,
            identity.uid,
            identity.pid,
            request.id,
            request.command.len(),
            timeout_ms
        ),
    );

    match executor::execute(&request.command, timeout_ms) {
        Ok(outcome) => {
            log(
                broker,
                &format!(
                    "requester={} request={} exit_code={} timed_out={} truncated={}",
                    identity.label,
                    request.id,
                    outcome.exit_code,
                    outcome.timed_out,
                    outcome.truncated
                ),
            );
            send(writer, &Response::exec_ok(&request.id, outcome))?;
        }
        Err(error) => {
            log(
                broker,
                &format!(
                    "requester={} request={} execution_error={error}",
                    identity.label, request.id
                ),
            );
            send(
                writer,
                &Response::error(&request.id, "error", "execution_failed"),
            )?;
        }
    }
    Ok(())
}

fn send(writer: &Arc<Mutex<UnixStream>>, response: &Response) -> io::Result<()> {
    let mut stream = writer.lock().unwrap_or_else(|error| error.into_inner());
    write_response(&mut *stream, response)
}

fn log(broker: &Broker, message: &str) {
    if let Some(logger) = &broker.logger {
        let _ = logger.log(message);
    }
}

fn is_idle_or_closed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{ApprovalConfig, ApprovalManager};
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::thread;

    struct Fixture {
        _directory: tempfile::TempDir,
        broker: Arc<Broker>,
        _manager: ApprovalManager,
        config: ServerConfig,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants.json");
        std::fs::write(
            &path,
            r#"{"version":1,"grants":{"User::Pkg::org.example.allowed":{"granted_at":1}}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let grants = Arc::new(Mutex::new(GrantStore::load(&path).unwrap()));
        let approval_config = ApprovalConfig {
            ui_label: "User::Pkg::org.tvauth.ui".into(),
            ui_app_id: "org.tvauth.ui".into(),
            app_launcher: PathBuf::from("/nonexistent/tv-auth-launcher"),
            prompt_timeout: Duration::from_millis(500),
            ui_launch_timeout: Duration::from_millis(50),
            launch_cooldown: Duration::from_millis(10),
            prompt_queue_max: 4,
            max_launch_attempts: 1,
        };
        let manager = ApprovalManager::start(approval_config, Arc::clone(&grants), None).unwrap();
        let broker = Arc::new(Broker::new(grants, manager.handle(), None));
        let config = ServerConfig {
            max_connections: 16,
            max_requests_per_connection: 16,
            ui_label: "User::Pkg::org.tvauth.ui".into(),
            ui_uid: 5001,
            approval_wait_timeout: Duration::from_secs(3),
        };
        Fixture {
            _directory: directory,
            broker,
            _manager: manager,
            config,
        }
    }

    struct Client {
        writer: UnixStream,
        reader: BufReader<UnixStream>,
    }

    impl Client {
        fn send(&mut self, value: Value) {
            let mut bytes = serde_json::to_vec(&value).unwrap();
            bytes.push(b'\n');
            self.writer.write_all(&bytes).unwrap();
        }

        fn read(&mut self) -> Value {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        }
    }

    fn connect(
        fixture: &Fixture,
        label: &str,
        uid: u32,
    ) -> (Client, thread::JoinHandle<io::Result<()>>) {
        let identity = PeerIdentity {
            pid: 123,
            uid,
            gid: 100,
            label: label.into(),
        };
        let (client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let reader = BufReader::new(client.try_clone().unwrap());
        let broker = Arc::clone(&fixture.broker);
        let config = fixture.config.clone();
        let handle = thread::spawn(move || handle_connection(server, identity, broker, &config));
        (
            Client {
                writer: client,
                reader,
            },
            handle,
        )
    }

    fn exec_request(id: &str, command: &str) -> Value {
        json!({
            "version": 1,
            "id": id,
            "method": "exec",
            "command": command,
            "timeout_ms": 1000
        })
    }

    #[test]
    fn granted_package_executes_on_linux() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.allowed", 5001);
        client.send(exec_request("exec-1", "printf hello"));
        let response = client.read();
        assert_eq!(response["status"], "ok");
        assert_eq!(response["exit_code"], 0);
        assert_eq!(response["stdout_b64"], "aGVsbG8=");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn unknown_package_without_ui_fails_closed() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.unknown", 5001);
        client.send(exec_request("exec-1", "printf should-not-run"));
        let response = client.read();
        assert_eq!(response["status"], "ui_unavailable");
        assert_eq!(response["error"], "ui_launch_failed");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn ui_role_can_list_and_revoke_grants() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        client.send(json!({"version": 1, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(client.read()["status"], "ok");

        client.send(json!({"version": 1, "id": "list-1", "method": "grants.list"}));
        let response = client.read();
        assert_eq!(response["status"], "ok");
        let grants = response["grants"].as_array().unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0]["package"], "org.example.allowed");

        client.send(json!({
            "version": 1,
            "id": "revoke-1",
            "method": "grants.revoke",
            "label": "User::Pkg::org.example.allowed"
        }));
        let response = client.read();
        assert_eq!(response["status"], "ok");
        assert_eq!(response["revoked"], true);
        assert!(!fixture
            .broker
            .grants
            .lock()
            .unwrap()
            .is_allowed("User::Pkg::org.example.allowed"));

        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn ui_methods_require_the_ui_label() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.allowed", 5001);
        client.send(json!({"version": 1, "id": "list-1", "method": "grants.list"}));
        let response = client.read();
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "not_ui");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn ui_label_requires_the_configured_uid() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 6000);
        client.send(json!({"version": 1, "id": "hello-1", "method": "ui.hello"}));
        let response = client.read();
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "not_ui");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn ui_role_cannot_execute_commands() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        client.send(json!({"version": 1, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(client.read()["status"], "ok");
        client.send(exec_request("exec-1", "id"));
        let response = client.read();
        assert_eq!(response["status"], "denied");
        assert_eq!(response["error"], "ui_role");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn revoked_grant_returns_to_approval() {
        let fixture = fixture();
        let (mut ui, ui_handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        ui.send(json!({"version": 1, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(ui.read()["status"], "ok");
        ui.send(json!({
            "version": 1,
            "id": "revoke-1",
            "method": "grants.revoke",
            "label": "User::Pkg::org.example.allowed"
        }));
        assert_eq!(ui.read()["revoked"], true);
        drop(ui);
        ui_handle.join().unwrap().unwrap();
        thread::sleep(Duration::from_millis(100));

        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.allowed", 5001);
        client.send(exec_request("exec-1", "printf should-not-run"));
        let response = client.read();
        assert_eq!(response["status"], "ui_unavailable");
        assert_eq!(response["error"], "ui_launch_failed");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn malformed_json_is_rejected() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.allowed", 5001);
        client.writer.write_all(b"{not-json}\n").unwrap();
        let response = client.read();
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "malformed_request");
        drop(client);
        assert!(handle.join().unwrap().is_err());
    }
}
