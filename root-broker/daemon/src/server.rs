use crate::approval::{ApprovalHandle, ApprovalRequest, ApprovalResult};
use crate::executor;
use crate::grants::{GrantCheck, GrantStore, NotifyMode};
use crate::identity::{package_from_label, PeerIdentity};
use crate::logger::Logger;
use crate::pkginstall::{self, InstallError};
use crate::pkgmeta::{self, MetaError, SignatureContext};
use crate::pkgstage::{self, StageConfig, StageError};
use crate::protocol::{
    is_valid_request_id, parse_request, read_message, validate_decision, validate_exec,
    validate_install, validate_label, write_response, ExecRequest, GrantView, InstallRequest,
    ParseError, PromptKind, Request, Response, MAX_REQUEST_BYTES,
};
use crate::trust::{self, TrustPaths};
use rustix::io::Errno;
use rustix::net::RecvFlags;
use std::collections::HashSet;
use std::io::{self, BufReader, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// How often a blocked exec checks whether its client hung up.
const HANGUP_CHECK_INTERVAL: Duration = Duration::from_millis(200);

/// Default roots checked for the `is_upgrade` hint on the prompt.
pub const DEFAULT_INSTALLED_ROOTS: &[&str] = &["/opt/usr/apps", "/usr/apps"];

#[derive(Debug, Clone)]
pub struct PackageConfig {
    pub staging: StageConfig,
    /// Stock installer, normally `/usr/bin/pkgcmd`.
    pub pkgcmd: PathBuf,
    /// `None` disables trust enrollment (host tests and dry runs only).
    pub trust: Option<TrustPaths>,
    /// Directories that indicate an already installed package.
    pub installed_roots: Vec<PathBuf>,
}

impl PackageConfig {
    pub fn new() -> Self {
        Self {
            staging: StageConfig::default(),
            pkgcmd: PathBuf::from("/usr/bin/pkgcmd"),
            trust: Some(TrustPaths::new(trust::DEFAULT_TRUST_ROOT)),
            installed_roots: DEFAULT_INSTALLED_ROOTS.iter().map(PathBuf::from).collect(),
        }
    }

    /// Best-effort signature label for the prompt. Enrollment (the actual bind
    /// mounts) happens only after approval.
    pub fn signature_context(&self) -> SignatureContext {
        self.trust
            .as_ref()
            .and_then(|paths| trust::load_material(paths).ok())
            .map(|material| material.signature_context())
            .unwrap_or_default()
    }
}

impl Default for PackageConfig {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Broker {
    pub grants: Arc<Mutex<GrantStore>>,
    pub approvals: ApprovalHandle,
    pub logger: Option<Arc<Logger>>,
    pub next_ui_token: AtomicU64,
    /// Serializes package installs (staging + prompt + pkgcmd) globally.
    install_lock: Mutex<()>,
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
            install_lock: Mutex::new(()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Concurrent ordinary-app connections.
    pub max_connections: usize,
    /// Separate allowance for the approval UI so apps cannot lock it out.
    pub max_ui_connections: usize,
    pub max_requests_per_connection: usize,
    pub ui_label: String,
    pub ui_uid: u32,
    pub approval_wait_timeout: Duration,
    /// Present when package installation is enabled.
    pub packages: Option<PackageConfig>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_connections: 16,
            max_ui_connections: 2,
            max_requests_per_connection: 1024,
            ui_label: "User::Pkg::org.tvauth.ui".into(),
            ui_uid: 5001,
            approval_wait_timeout: Duration::from_secs(300),
            packages: None,
        }
    }
}

impl ServerConfig {
    pub fn is_ui(&self, identity: &PeerIdentity) -> bool {
        identity.label == self.ui_label && identity.uid == self.ui_uid
    }
}

/// Reader that enforces an absolute deadline across all underlying reads, so
/// a peer trickling bytes cannot extend a message's idle timeout forever.
struct DeadlineReader {
    stream: UnixStream,
    deadline: Option<Instant>,
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(deadline) = self.deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "message deadline"));
            }
            // macOS rejects setsockopt (EINVAL) once the peer has closed; the
            // read below then reports EOF immediately.
            match self.stream.set_read_timeout(Some(remaining)) {
                Err(error) if error.kind() != io::ErrorKind::InvalidInput => return Err(error),
                _ => {}
            }
        }
        loop {
            match self.stream.read(buf) {
                // A handled signal must not abort a request mid-frame.
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }
}

/// True if the peer has closed its end (non-consuming check).
fn peer_closed(writer: &Arc<Mutex<UnixStream>>) -> bool {
    let stream = writer.lock().unwrap_or_else(|error| error.into_inner());
    let mut byte = [0_u8; 1];
    match rustix::net::recv(&*stream, &mut byte, RecvFlags::PEEK | RecvFlags::DONTWAIT) {
        Ok((0, _)) => true,
        Ok(_) => false,
        Err(error) => !matches!(error, Errno::AGAIN | Errno::INTR),
    }
}

pub fn handle_connection(
    stream: UnixStream,
    identity: PeerIdentity,
    broker: Arc<Broker>,
    config: &ServerConfig,
) -> io::Result<()> {
    let is_ui = config.is_ui(&identity);
    let idle_timeout = if is_ui {
        Duration::from_secs(600)
    } else {
        Duration::from_secs(30)
    };
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let reader_stream = stream.try_clone()?;
    let control = stream.try_clone()?;
    let mut reader = BufReader::new(DeadlineReader {
        stream: reader_stream,
        deadline: None,
    });
    let writer = Arc::new(Mutex::new(stream));
    let mut request_ids = HashSet::new();
    let mut ui_token: Option<u64> = None;

    let result = loop {
        // The whole message must arrive within the idle timeout.
        reader.get_mut().deadline = Some(Instant::now() + idle_timeout);
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
        if request_ids.contains(&response_id) {
            let _ = send(
                &writer,
                &Response::error(&response_id, "error", "duplicate_request_id"),
            );
            continue;
        }
        if request_ids.len() >= config.max_requests_per_connection {
            let _ = send(
                &writer,
                &Response::error(&response_id, "error", "too_many_requests"),
            );
            break Ok(());
        }
        request_ids.insert(response_id.clone());

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
                request.id.clone(),
                identity.label.clone(),
                Arc::clone(writer),
                control,
            ) {
                // The approval manager writes the hello response.
                Ok(()) => *ui_token = Some(token),
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
                    .map(|(label, granted_at, notify)| GrantView {
                        label: label.to_owned(),
                        package: package_from_label(label).unwrap_or(label).to_owned(),
                        granted_at,
                        notify,
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
            if let Err(error) = validate_label(&request.label) {
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
        Request::GrantsNotify(request) => {
            if ui_token.is_none() {
                send(
                    writer,
                    &Response::error(&request.id, "error", "not_registered"),
                )?;
                return Ok(());
            }
            if let Err(error) = validate_label(&request.label) {
                send(writer, &Response::error(&request.id, "error", error))?;
                return Ok(());
            }
            let result = broker
                .grants
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .set_notify(&request.label, request.notify);
            let response = match result {
                Ok(true) => Response::ok(&request.id),
                Ok(false) => Response::error(&request.id, "error", "not_found"),
                Err(_) => Response::error(&request.id, "error", "grant_save_failed"),
            };
            log(
                broker,
                &format!(
                    "grant_notify label={} notify={:?} status={}",
                    request.label, request.notify, response.status
                ),
            );
            send(writer, &response)?;
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
        Request::PkgInstall(request) => {
            log(
                broker,
                &format!(
                    "requester={} uid={} pid={} request={} denied=ui_role_method=pkg.install",
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
        Request::PkgInstall(request) => handle_install(writer, identity, broker, config, request),
        other => send(writer, &Response::error(other.id(), "error", "not_ui")),
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

    let (grant_check, notify) = {
        let mut store = broker
            .grants
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let grant_check = store.check(&identity.label);
        (grant_check, store.notify_mode(&identity.label))
    };

    if grant_check.is_allowed() {
        if grant_check == GrantCheck::Migrated {
            log(
                broker,
                &format!("grant_fingerprint_migrated label={}", identity.label),
            );
        }
        // Granted packages run without asking, but the user still sees a
        // (coalesced) notice of who ran what, subject to the per-grant mode.
        if let Some(mode) = notify {
            if mode != NotifyMode::Never {
                let _ = broker.approvals.notice(
                    identity.label.clone(),
                    request.command.clone(),
                    mode == NotifyMode::First,
                );
            }
        }
    } else {
        match grant_check {
            GrantCheck::Stale => log(
                broker,
                &format!(
                    "grant_stale label={} reason=package_identity_changed",
                    identity.label
                ),
            ),
            GrantCheck::IdentityUnavailable => log(
                broker,
                &format!(
                    "grant_stale label={} reason=package_identity_unavailable",
                    identity.label
                ),
            ),
            _ => {}
        }
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
            ticket: 0,
            request_id: request.id.clone(),
            label: identity.label.clone(),
            kind: PromptKind::Exec,
            install: None,
            command: request.command.clone(),
            reply,
        };
        let ticket = match broker.approvals.request(approval) {
            Ok(ticket) => ticket,
            Err(_) => {
                send(
                    writer,
                    &Response::error(&request.id, "ui_unavailable", "approval_unavailable"),
                )?;
                return Ok(());
            }
        };
        match await_decision(writer, identity, broker, config, ticket, decisions)? {
            ApprovalOutcome::Allowed => {
                log(
                    broker,
                    &format!(
                        "requester={} request={} approval=allowed",
                        identity.label, request.id
                    ),
                );
            }
            ApprovalOutcome::Denied(status, error) => {
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
            ApprovalOutcome::ClientGone => return Ok(()),
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

enum ApprovalOutcome {
    Allowed,
    Denied(&'static str, &'static str),
    /// The requester hung up; the prompt was cancelled and nothing ran.
    ClientGone,
}

/// Wait for a decision in short slices so a client hangup cancels the prompt
/// instead of leaving it to be approved for a dead peer.
fn await_decision(
    writer: &Arc<Mutex<UnixStream>>,
    identity: &PeerIdentity,
    broker: &Arc<Broker>,
    config: &ServerConfig,
    ticket: u64,
    decisions: mpsc::Receiver<ApprovalResult>,
) -> io::Result<ApprovalOutcome> {
    let deadline = Instant::now() + config.approval_wait_timeout;
    let decision = loop {
        match decisions.recv_timeout(HANGUP_CHECK_INTERVAL) {
            Ok(result) => break Some(result),
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if peer_closed(writer) {
                    let _ = broker.approvals.cancel(ticket);
                    log(
                        broker,
                        &format!(
                            "requester={} approval=cancelled client_gone",
                            identity.label
                        ),
                    );
                    return Ok(ApprovalOutcome::ClientGone);
                }
                if Instant::now() >= deadline {
                    let _ = broker.approvals.cancel(ticket);
                    break None;
                }
            }
        }
    };
    match decision {
        Some(ApprovalResult::AllowOnce) | Some(ApprovalResult::AllowAlways) => {
            // A decision may race with a hangup; never run for a closed client.
            if peer_closed(writer) {
                log(
                    broker,
                    &format!(
                        "requester={} approval=allowed_but_client_gone",
                        identity.label
                    ),
                );
                return Ok(ApprovalOutcome::ClientGone);
            }
            Ok(ApprovalOutcome::Allowed)
        }
        Some(ApprovalResult::Denied(status, error)) => Ok(ApprovalOutcome::Denied(status, error)),
        None => {
            log(
                broker,
                &format!("requester={} approval=timeout", identity.label),
            );
            Ok(ApprovalOutcome::Denied(
                "ui_unavailable",
                "approval_wait_timeout",
            ))
        }
    }
}

/// Stage, inspect, prompt for, and install one package. The global install
/// lock is held for the whole operation, so only one install (including its
/// prompt) exists at a time.
fn handle_install(
    writer: &Arc<Mutex<UnixStream>>,
    identity: &PeerIdentity,
    broker: &Arc<Broker>,
    config: &ServerConfig,
    request: InstallRequest,
) -> io::Result<()> {
    let timeout_ms = match validate_install(&request) {
        Ok(timeout) => timeout,
        Err(error) => {
            send(writer, &Response::error(&request.id, "error", error))?;
            return Ok(());
        }
    };
    let Some(packages) = config.packages.as_ref() else {
        log(
            broker,
            &format!(
                "requester={} request={} method=pkg.install disabled",
                identity.label, request.id
            ),
        );
        send(
            writer,
            &Response::error(&request.id, "error", "package_support_disabled"),
        )?;
        return Ok(());
    };
    let _install = match broker.install_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            log(
                broker,
                &format!(
                    "requester={} request={} method=pkg.install result=busy",
                    identity.label, request.id
                ),
            );
            send(
                writer,
                &Response::error(&request.id, "busy", "install_busy"),
            )?;
            return Ok(());
        }
    };

    log(
        broker,
        &format!(
            "requester={} uid={} pid={} request={} method=pkg.install type={} path_bytes={} attempt",
            identity.label,
            identity.uid,
            identity.pid,
            request.id,
            request.pkg_type.as_str(),
            request.path.len()
        ),
    );

    let staged = match pkgstage::stage(&packages.staging, &request.path, request.pkg_type) {
        Ok(staged) => staged,
        Err(error) => {
            let (status, code) = match error {
                StageError::InvalidPath => ("denied", "invalid_path"),
                StageError::TooLarge => ("denied", "too_large"),
                StageError::Io(_) => ("error", "staging_failed"),
            };
            log(
                broker,
                &format!(
                    "requester={} request={} method=pkg.install staged=false error={code} detail={error}",
                    identity.label, request.id
                ),
            );
            send(
                writer,
                &Response::error_with_detail(&request.id, status, code, Some(error.to_string())),
            )?;
            return Ok(());
        }
    };

    let signature_context = packages.signature_context();
    let meta = match pkgmeta::inspect(staged.path(), request.pkg_type, &signature_context) {
        Ok(meta) => meta,
        Err(error) => {
            let (status, code, detail) = match error {
                MetaError::BadPackage(reason) => ("error", "bad_package", reason.to_owned()),
                MetaError::Forbidden(detail) => ("denied", "manifest_forbidden", detail),
            };
            log(
                broker,
                &format!(
                    "requester={} request={} method=pkg.install inspect=false error={code} detail={detail}",
                    identity.label, request.id
                ),
            );
            send(
                writer,
                &Response::error_with_detail(&request.id, status, code, Some(detail)),
            )?;
            return Ok(());
        }
    };

    let is_upgrade = pkginstall::is_installed(&meta.pkgid, &packages.installed_roots);
    let install_meta = meta.into_install_meta(staged.size_bytes(), is_upgrade);
    log(
        broker,
        &format!(
            "requester={} request={} method=pkg.install staged=true sha256={} pkgid={} version={} size={} signature={:?} upgrade={} approval_required",
            identity.label,
            request.id,
            staged.sha256(),
            install_meta.pkgid,
            install_meta.pkg_version,
            staged.size_bytes(),
            install_meta.signature,
            is_upgrade
        ),
    );

    let command = format!(
        "install {} {}",
        request.pkg_type.as_str(),
        staged.display_path()
    );
    let (reply, decisions) = mpsc::channel();
    let approval = ApprovalRequest {
        ticket: 0,
        request_id: request.id.clone(),
        label: identity.label.clone(),
        kind: PromptKind::Install,
        install: Some(install_meta.clone()),
        command,
        reply,
    };
    let ticket = match broker.approvals.request(approval) {
        Ok(ticket) => ticket,
        Err(_) => {
            send(
                writer,
                &Response::error(&request.id, "ui_unavailable", "approval_unavailable"),
            )?;
            return Ok(());
        }
    };
    match await_decision(writer, identity, broker, config, ticket, decisions)? {
        ApprovalOutcome::Allowed => {
            log(
                broker,
                &format!(
                    "requester={} request={} pkgid={} approval=allowed",
                    identity.label, request.id, install_meta.pkgid
                ),
            );
        }
        ApprovalOutcome::Denied(status, error) => {
            log(
                broker,
                &format!(
                    "requester={} request={} pkgid={} approval=denied error={}",
                    identity.label, request.id, install_meta.pkgid, error
                ),
            );
            send(writer, &Response::error(&request.id, status, error))?;
            return Ok(());
        }
        ApprovalOutcome::ClientGone => return Ok(()),
    }

    if let Some(paths) = &packages.trust {
        match trust::ensure_enrolled(paths) {
            Ok(enrollment) => log(
                broker,
                &format!(
                    "requester={} request={} trust_enrollment={enrollment:?}",
                    identity.label, request.id
                ),
            ),
            Err(error) => {
                log(
                    broker,
                    &format!(
                        "requester={} request={} pkgid={} trust_enrollment_failed={error}",
                        identity.label, request.id, install_meta.pkgid
                    ),
                );
                send(
                    writer,
                    &Response::error_with_detail(
                        &request.id,
                        "error",
                        "enroll_failed",
                        Some(error.to_string()),
                    ),
                )?;
                return Ok(());
            }
        }
    }

    match pkginstall::install(
        &packages.pkgcmd,
        request.pkg_type,
        staged.path(),
        timeout_ms,
    ) {
        Ok(outcome) => {
            log(
                broker,
                &format!(
                    "requester={} request={} pkgid={} version={} sha256={} result=installed",
                    identity.label,
                    request.id,
                    install_meta.pkgid,
                    install_meta.pkg_version,
                    staged.sha256()
                ),
            );
            send(
                writer,
                &Response::install_ok(
                    &request.id,
                    &install_meta.pkgid,
                    &install_meta.pkg_version,
                    false,
                    &outcome.log_tail,
                ),
            )?;
        }
        Err(error) => {
            log(
                broker,
                &format!(
                    "requester={} request={} pkgid={} sha256={} result=install_failed detail={error}",
                    identity.label,
                    request.id,
                    install_meta.pkgid,
                    staged.sha256()
                ),
            );
            let (detail, log_tail) = match error {
                InstallError::Spawn(error) => (format!("cannot run pkgcmd: {error}"), Vec::new()),
                InstallError::Failed { detail, log_tail } => (detail, log_tail),
            };
            let mut response =
                Response::error_with_detail(&request.id, "error", "install_failed", Some(detail));
            response.pkgid = Some(install_meta.pkgid.clone());
            response.pkg_version = Some(install_meta.pkg_version.clone());
            if !log_tail.is_empty() {
                use base64::Engine as _;
                response.log_tail_b64 =
                    Some(base64::engine::general_purpose::STANDARD.encode(log_tail));
            }
            send(writer, &response)?;
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
        apps: PathBuf,
        broker: Arc<Broker>,
        _manager: ApprovalManager,
        config: ServerConfig,
    }

    fn fixture() -> Fixture {
        fixture_with(Duration::from_millis(500))
    }

    fn fixture_with(prompt_timeout: Duration) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let apps = directory.path().join("apps");
        std::fs::create_dir(&apps).unwrap();
        let package = apps.join("org.example.allowed");
        std::fs::create_dir(&package).unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o755)).unwrap();
        let signature = package.join("author-signature.xml");
        std::fs::write(&signature, b"server-test-signature").unwrap();
        std::fs::set_permissions(&signature, std::fs::Permissions::from_mode(0o644)).unwrap();
        let fingerprint = crate::package_identity::package_fingerprint(
            "User::Pkg::org.example.allowed",
            std::slice::from_ref(&apps),
        )
        .unwrap();
        let path = directory.path().join("grants.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"version":2,"grants":{{"User::Pkg::org.example.allowed":{{"granted_at":1,"package_fingerprint":"{fingerprint}"}}}}}}"#
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let grants = Arc::new(Mutex::new(
            GrantStore::load_with_roots(&path, vec![apps.clone()]).unwrap(),
        ));
        let approval_config = ApprovalConfig {
            ui_app_id: "org.tvauth.ui".into(),
            app_launcher: PathBuf::from("/nonexistent/tv-auth-launcher"),
            prompt_timeout,
            ui_launch_timeout: Duration::from_millis(50),
            launch_cooldown: Duration::from_millis(10),
            prompt_queue_max: 4,
            max_launch_attempts: 1,
            notice_interval: Duration::from_millis(250),
        };
        let manager = ApprovalManager::start(approval_config, Arc::clone(&grants), None).unwrap();
        let broker = Arc::new(Broker::new(grants, manager.handle(), None));
        let config = ServerConfig {
            max_connections: 16,
            max_ui_connections: 2,
            max_requests_per_connection: 16,
            ui_label: "User::Pkg::org.tvauth.ui".into(),
            ui_uid: 5001,
            approval_wait_timeout: Duration::from_secs(3),
            packages: None,
        };
        Fixture {
            _directory: directory,
            apps,
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
            self.send_raw(&bytes);
        }

        fn send_raw(&mut self, bytes: &[u8]) {
            loop {
                match self.writer.write_all(bytes) {
                    Ok(()) => return,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => panic!("send: {error}"),
                }
            }
        }

        fn read(&mut self) -> Value {
            loop {
                let mut line = String::new();
                match self.reader.read_line(&mut line) {
                    Ok(0) => panic!("connection closed before a response"),
                    Ok(_) => return serde_json::from_str(&line).unwrap(),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => panic!("read: {error}"),
                }
            }
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
    fn changed_package_identity_returns_to_approval() {
        let fixture = fixture();
        std::fs::write(
            fixture
                .apps
                .join("org.example.allowed/author-signature.xml"),
            b"changed-package-identity",
        )
        .unwrap();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.allowed", 5001);
        client.send(exec_request("exec-1", "printf should-not-run"));
        let response = client.read();
        // The stale grant must not execute. With no UI and a missing launcher,
        // the approval path fails closed.
        assert_eq!(response["status"], "ui_unavailable");
        assert_eq!(response["error"], "ui_launch_failed");
        drop(client);
        handle.join().unwrap().unwrap();
        assert_eq!(
            fixture
                .broker
                .grants
                .lock()
                .unwrap()
                .check("User::Pkg::org.example.allowed"),
            GrantCheck::NotGranted
        );
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
        assert_eq!(grants[0]["notify"], "every");

        client.send(json!({
            "version": 1,
            "id": "notify-1",
            "method": "grants.notify",
            "label": "User::Pkg::org.example.allowed",
            "notify": "never"
        }));
        assert_eq!(client.read()["status"], "ok");
        client.send(json!({
            "version": 1,
            "id": "notify-2",
            "method": "grants.notify",
            "label": "User::Pkg::org.example.missing",
            "notify": "first"
        }));
        assert_eq!(client.read()["error"], "not_found");
        client.send(json!({"version": 1, "id": "list-2", "method": "grants.list"}));
        assert_eq!(client.read()["grants"][0]["notify"], "never");

        client.send(json!({
            "version": 1,
            "id": "revoke-1",
            "method": "grants.revoke",
            "label": "User::Pkg::org.example.allowed"
        }));
        let response = client.read();
        assert_eq!(response["status"], "ok");
        assert_eq!(response["revoked"], true);
        assert_eq!(
            fixture
                .broker
                .grants
                .lock()
                .unwrap()
                .check("User::Pkg::org.example.allowed"),
            GrantCheck::NotGranted
        );

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
        client.send_raw(b"{not-json}\n");
        let response = client.read();
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "malformed_request");
        drop(client);
        assert!(handle.join().unwrap().is_err());
    }

    #[test]
    fn oversized_frame_is_rejected_and_closes() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.allowed", 5001);
        let mut line = vec![b'x'; MAX_REQUEST_BYTES + 1];
        line.push(b'\n');
        client.send_raw(&line);
        let response = client.read();
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "malformed_request");
        drop(client);
        assert!(handle.join().unwrap().is_err());
    }

    #[test]
    fn too_many_requests_closes_the_connection() {
        let mut fixture = fixture();
        fixture.config.max_requests_per_connection = 2;
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.allowed", 5001);
        for id in ["list-1", "list-2"] {
            client.send(json!({"version": 1, "id": id, "method": "grants.list"}));
            assert_eq!(client.read()["error"], "not_ui");
        }
        // Duplicates do not count against the limit.
        client.send(json!({"version": 1, "id": "list-2", "method": "grants.list"}));
        assert_eq!(client.read()["error"], "duplicate_request_id");
        client.send(json!({"version": 1, "id": "list-3", "method": "grants.list"}));
        assert_eq!(client.read()["error"], "too_many_requests");
        handle.join().unwrap().unwrap();
        let mut rest = String::new();
        assert_eq!(client.reader.read_line(&mut rest).unwrap(), 0);
    }

    #[test]
    fn trickled_message_hits_the_absolute_deadline() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut reader = BufReader::new(DeadlineReader {
            stream: server,
            deadline: Some(Instant::now() + Duration::from_millis(400)),
        });
        let trickle = thread::spawn(move || {
            let mut client = client;
            for _ in 0..20 {
                if client.write_all(b"x").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        let started = Instant::now();
        let error = read_message(&mut reader, MAX_REQUEST_BYTES).unwrap_err();
        assert!(is_idle_or_closed(&error), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(reader);
        trickle.join().unwrap();
    }

    #[test]
    fn granted_exec_notifies_the_ui() {
        let fixture = fixture();
        let (mut ui, ui_handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        ui.send(json!({"version": 1, "id": "hello-1", "method": "ui.hello"}));
        let hello = ui.read();
        assert_eq!(hello["status"], "ok");
        assert!(hello.get("launch").is_none());

        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.allowed", 5001);
        client.send(exec_request("exec-1", "printf hello"));
        // The command result itself is platform dependent (setsid on Linux).
        let _ = client.read();
        let notice = ui.read();
        assert_eq!(notice["type"], "ui.notice");
        assert_eq!(notice["notices"][0]["package"], "org.example.allowed");
        assert_eq!(notice["notices"][0]["command"], "printf hello");
        assert_eq!(notice["notices"][0]["count"], 1);
        drop(client);
        handle.join().unwrap().unwrap();
        drop(ui);
        ui_handle.join().unwrap().unwrap();
    }

    #[test]
    fn client_hangup_cancels_pending_approval() {
        let fixture = fixture_with(Duration::from_secs(10));
        let (mut ui, ui_handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        ui.send(json!({"version": 1, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(ui.read()["status"], "ok");

        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.unknown", 5001);
        client.send(exec_request("exec-1", "printf should-not-run"));
        let prompt = ui.read();
        assert_eq!(prompt["type"], "ui.prompt");
        drop(client);
        let started = Instant::now();
        handle.join().unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));

        // The cancel was queued before the connection thread returned, so a
        // late decision is stale.
        ui.send(json!({
            "version": 1,
            "id": "decision-1",
            "method": "ui.decision",
            "prompt_id": prompt["prompt_id"],
            "nonce": prompt["nonce"],
            "decision": "once"
        }));
        assert_eq!(ui.read()["error"], "stale_prompt");
        drop(ui);
        ui_handle.join().unwrap().unwrap();
    }

    fn write_wgt(path: &PathBuf, pkgid: &str) {
        use std::io::Write as _;
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("config.xml", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<widget xmlns="http://www.w3.org/ns/widgets"
        xmlns:tizen="http://tizen.org/ns/widgets"
        id="http://example.com/Example" version="1.0.0">
  <name>Example</name>
  <tizen:application id="{pkgid}.App" package="{pkgid}"/>
  <tizen:privilege>http://tizen.org/privilege/internet</tizen:privilege>
</widget>"#
                )
                .as_bytes(),
            )
            .unwrap();
        writer.finish().unwrap();
    }

    /// Wire package support into a fixture and return the source package path.
    fn with_packages(fixture: &mut Fixture, pkgcmd_body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let directory = fixture._directory.path();
        let home = directory.join("home/owner");
        std::fs::create_dir_all(&home).unwrap();
        let source = home.join("org.example.app-1.0.0.wgt");
        write_wgt(&source, "org.example.app");

        let pkgcmd = directory.join("pkgcmd");
        std::fs::write(&pkgcmd, pkgcmd_body).unwrap();
        std::fs::set_permissions(&pkgcmd, std::fs::Permissions::from_mode(0o755)).unwrap();

        fixture.config.packages = Some(PackageConfig {
            staging: StageConfig {
                root: directory.join("staging"),
                allowed_roots: vec![home],
                max_bytes: 1024 * 1024,
            },
            pkgcmd,
            trust: None,
            installed_roots: Vec::new(),
        });
        source
    }

    fn install_request(id: &str, path: &str) -> Value {
        json!({
            "version": 2,
            "id": id,
            "method": "pkg.install",
            "pkg_type": "wgt",
            "path": path,
            "timeout_ms": 5000
        })
    }

    #[test]
    fn approved_install_runs_pkgcmd_and_returns_metadata() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let mut fixture = fixture();
        let source = with_packages(&mut fixture, "#!/bin/sh\nprintf 'key[end] val[ok]\\n'\n");
        let (mut ui, ui_handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        ui.send(json!({"version": 2, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(ui.read()["status"], "ok");

        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.app", 5001);
        client.send(install_request("inst-1", source.to_str().unwrap()));

        let prompt = ui.read();
        assert_eq!(prompt["type"], "ui.prompt");
        assert_eq!(prompt["kind"], "install");
        assert_eq!(prompt["install"]["pkgid"], "org.example.app");
        assert_eq!(prompt["install"]["pkg_version"], "1.0.0");
        assert_eq!(
            prompt["install"]["privileges"][0],
            "http://tizen.org/privilege/internet"
        );
        assert_eq!(prompt["install"]["signature"], "unsigned_will_resign");
        assert_eq!(prompt["install"]["is_upgrade"], false);
        assert!(prompt["command"]
            .as_str()
            .unwrap()
            .starts_with("install wgt "));

        ui.send(json!({
            "version": 2,
            "id": "decision-1",
            "method": "ui.decision",
            "prompt_id": prompt["prompt_id"],
            "nonce": prompt["nonce"],
            "decision": "once"
        }));
        assert_eq!(ui.read()["status"], "ok");

        let response = client.read();
        assert_eq!(response["status"], "ok");
        assert_eq!(response["pkgid"], "org.example.app");
        assert_eq!(response["pkg_version"], "1.0.0");
        assert_eq!(response["re_signed"], false);
        use base64::Engine as _;
        let log = base64::engine::general_purpose::STANDARD
            .decode(response["log_tail_b64"].as_str().unwrap())
            .unwrap();
        assert!(String::from_utf8_lossy(&log).contains("val[ok]"));

        drop(client);
        handle.join().unwrap().unwrap();
        drop(ui);
        ui_handle.join().unwrap().unwrap();

        // Staging is wiped once the request completes.
        let staging = fixture
            .config
            .packages
            .as_ref()
            .unwrap()
            .staging
            .root
            .clone();
        assert_eq!(std::fs::read_dir(staging).unwrap().count(), 0);
    }

    #[test]
    fn denied_install_never_runs_pkgcmd() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let mut fixture = fixture();
        let marker = fixture._directory.path().join("pkgcmd-ran");
        let source = with_packages(
            &mut fixture,
            &format!(
                "#!/bin/sh\ntouch '{}'\nprintf 'key[end] val[ok]\\n'\n",
                marker.display()
            ),
        );
        let (mut ui, ui_handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        ui.send(json!({"version": 2, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(ui.read()["status"], "ok");

        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.app", 5001);
        client.send(install_request("inst-1", source.to_str().unwrap()));
        let prompt = ui.read();
        assert_eq!(prompt["kind"], "install");
        ui.send(json!({
            "version": 2,
            "id": "decision-1",
            "method": "ui.decision",
            "prompt_id": prompt["prompt_id"],
            "nonce": prompt["nonce"],
            "decision": "deny"
        }));
        assert_eq!(ui.read()["status"], "ok");

        let response = client.read();
        assert_eq!(response["status"], "denied");
        assert_eq!(response["error"], "user_denied");
        assert!(!marker.exists());

        drop(client);
        handle.join().unwrap().unwrap();
        drop(ui);
        ui_handle.join().unwrap().unwrap();
    }

    #[test]
    fn install_outside_the_allowlist_is_rejected_without_a_prompt() {
        let mut fixture = fixture();
        let outside = fixture._directory.path().join("outside.wgt");
        write_wgt(&outside, "org.example.outside");
        with_packages(&mut fixture, "#!/bin/sh\nprintf 'key[end] val[ok]\\n'\n");

        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.app", 5001);
        client.send(install_request("inst-1", outside.to_str().unwrap()));
        let response = client.read();
        assert_eq!(response["status"], "denied");
        assert_eq!(response["error"], "invalid_path");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn install_failure_reports_the_log_tail() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let mut fixture = fixture();
        let source = with_packages(
            &mut fixture,
            "#!/bin/sh\nprintf 'key[end] val[-12]\\n'\nexit 1\n",
        );
        let (mut ui, ui_handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        ui.send(json!({"version": 2, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(ui.read()["status"], "ok");
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.app", 5001);
        client.send(install_request("inst-1", source.to_str().unwrap()));
        let prompt = ui.read();
        ui.send(json!({
            "version": 2,
            "id": "decision-1",
            "method": "ui.decision",
            "prompt_id": prompt["prompt_id"],
            "nonce": prompt["nonce"],
            "decision": "once"
        }));
        assert_eq!(ui.read()["status"], "ok");

        let response = client.read();
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "install_failed");
        assert!(response["detail"].as_str().unwrap().contains("-12"));
        assert!(response["log_tail_b64"].is_string());
        drop(client);
        handle.join().unwrap().unwrap();
        drop(ui);
        ui_handle.join().unwrap().unwrap();
    }

    #[test]
    fn second_install_is_busy_while_one_is_pending() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let mut fixture = fixture();
        let source = with_packages(&mut fixture, "#!/bin/sh\nprintf 'key[end] val[ok]\\n'\n");
        let (mut ui, ui_handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        ui.send(json!({"version": 2, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(ui.read()["status"], "ok");

        let (mut first, first_handle) = connect(&fixture, "User::Pkg::org.example.app", 5001);
        first.send(install_request("inst-1", source.to_str().unwrap()));
        let prompt = ui.read();
        assert_eq!(prompt["kind"], "install");

        let (mut second, second_handle) = connect(&fixture, "User::Pkg::org.example.app", 5001);
        second.send(install_request("inst-2", source.to_str().unwrap()));
        let response = second.read();
        assert_eq!(response["status"], "busy");
        assert_eq!(response["error"], "install_busy");

        ui.send(json!({
            "version": 2,
            "id": "decision-1",
            "method": "ui.decision",
            "prompt_id": prompt["prompt_id"],
            "nonce": prompt["nonce"],
            "decision": "deny"
        }));
        assert_eq!(ui.read()["status"], "ok");
        assert_eq!(first.read()["error"], "user_denied");

        drop(first);
        drop(second);
        first_handle.join().unwrap().unwrap();
        second_handle.join().unwrap().unwrap();
        drop(ui);
        ui_handle.join().unwrap().unwrap();
    }

    #[test]
    fn the_approved_install_uses_the_staged_copy() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let mut fixture = fixture();
        let capture = fixture._directory.path().join("captured.wgt");
        let source = with_packages(
            &mut fixture,
            &format!(
                "#!/bin/sh\ncp \"$5\" '{}'\nprintf 'key[end] val[ok]\\n'\n",
                capture.display()
            ),
        );
        let original = std::fs::read(&source).unwrap();
        let (mut ui, ui_handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        ui.send(json!({"version": 2, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(ui.read()["status"], "ok");

        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.app", 5001);
        client.send(install_request("inst-1", source.to_str().unwrap()));
        let prompt = ui.read();
        assert_eq!(prompt["kind"], "install");

        // The source is app-writable; changing it after the prompt must not
        // change what gets installed.
        std::fs::write(&source, b"tampered after staging").unwrap();

        ui.send(json!({
            "version": 2,
            "id": "decision-1",
            "method": "ui.decision",
            "prompt_id": prompt["prompt_id"],
            "nonce": prompt["nonce"],
            "decision": "once"
        }));
        assert_eq!(ui.read()["status"], "ok");
        assert_eq!(client.read()["status"], "ok");
        assert_eq!(std::fs::read(&capture).unwrap(), original);

        drop(client);
        handle.join().unwrap().unwrap();
        drop(ui);
        ui_handle.join().unwrap().unwrap();
    }

    #[test]
    fn package_installs_are_refused_without_package_support() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.example.app", 5001);
        client.send(install_request("inst-1", "/opt/media/app.wgt"));
        let response = client.read();
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "package_support_disabled");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn ui_role_cannot_install_packages() {
        let fixture = fixture();
        let (mut client, handle) = connect(&fixture, "User::Pkg::org.tvauth.ui", 5001);
        client.send(json!({"version": 2, "id": "hello-1", "method": "ui.hello"}));
        assert_eq!(client.read()["status"], "ok");
        client.send(install_request("inst-1", "/opt/media/app.wgt"));
        let response = client.read();
        assert_eq!(response["status"], "denied");
        assert_eq!(response["error"], "ui_role");
        drop(client);
        handle.join().unwrap().unwrap();
    }
}
