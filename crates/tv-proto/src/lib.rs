//! `tv-authd` wire protocol: newline-delimited JSON over
//! `/run/tv-auth/broker.sock`. The daemon parses requests and writes
//! responses/`ui.*` messages with the strict types here; clients read them
//! with the lenient [`ui`] types and talk to the socket with [`call`].

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

pub mod client;
pub mod markup;
pub mod ui;

pub const DEFAULT_SOCKET: &str = "/run/tv-auth/broker.sock";

/// Version written into every response and `ui.*` message. Requests may still
/// use version 1 for the methods that existed in v1.
pub const PROTOCOL_VERSION: u32 = 2;
pub const MIN_REQUEST_VERSION: u32 = 1;
pub const MAX_REQUEST_VERSION: u32 = PROTOCOL_VERSION;
/// `pkg.*` methods require this version; v1 clients cannot reach them.
pub const PACKAGE_PROTOCOL_VERSION: u32 = 2;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
pub const DEFAULT_INSTALL_TIMEOUT_MS: u64 = 120_000;
pub const MAX_INSTALL_TIMEOUT_MS: u64 = 120_000;
pub const MAX_REQUEST_ID_BYTES: usize = 128;
pub const MAX_PROMPT_COMMAND_BYTES: usize = 4096;
pub const MAX_NOTICE_COMMAND_BYTES: usize = 256;
pub const MAX_PACKAGE_PATH_BYTES: usize = 4096;
pub const MAX_PKGID_BYTES: usize = 127;
pub const MAX_PKG_NAME_BYTES: usize = 128;
pub const MAX_PKG_VERSION_BYTES: usize = 64;
pub const MAX_PRIVILEGE_BYTES: usize = 256;
pub const MAX_PRIVILEGES: usize = 128;
pub const MAX_INSTALL_LOG_TAIL_BYTES: usize = 8 * 1024;

/// When a granted package's executions are shown as a UI toast.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyMode {
    /// Every command.
    #[default]
    Every,
    /// The first command after the package was idle for a while.
    First,
    /// Never.
    Never,
}

impl NotifyMode {
    pub fn is_default(&self) -> bool {
        *self == Self::Every
    }
}

/// Result of one `exec` request, as reported in its response.
#[derive(Debug)]
pub struct ExecutionOutcome {
    pub exit_code: i32,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

pub fn package_from_label(label: &str) -> Option<&str> {
    label
        .strip_prefix("User::Pkg::")
        .filter(|package| !package.is_empty())
}

pub fn is_valid_package_id(package: &str) -> bool {
    !package.is_empty()
        && package.len() <= MAX_PKGID_BYTES
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub fn is_valid_package_label(label: &str) -> bool {
    package_from_label(label).is_some_and(is_valid_package_id)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
    pub command: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiHelloRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDecisionRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
    pub prompt_id: String,
    pub nonce: String,
    pub decision: Decision,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantsListRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantsRevokeRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantsNotifyRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
    pub label: String,
    pub notify: NotifyMode,
}

/// Recent approval decisions and package changes from the daemon log.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantsAuditRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
}

/// `access.status` (UI identity only): SSH, SDB and the SSH keys.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessStatusRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
}

/// `access.set` (UI identity only): any of the fields; each change is
/// applied and audited.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessSetRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub ssh: Option<bool>,
    #[serde(default)]
    pub sdb: Option<bool>,
    /// The one host Samsung's developer mode lets use SDB (IPv4).
    #[serde(default)]
    pub sdb_host: Option<String>,
}

/// `access.ssh_remove` (UI identity only).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessSshRemoveRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
    pub fingerprint: String,
}

/// One SSH key, as `access.*` responses list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshKeyView {
    /// `ssh-ed25519`, `ecdsa-sha2-nistp256`, ...
    pub kind: String,
    pub comment: String,
    /// `SHA256:...`, as `ssh-keygen -l` prints it.
    pub fingerprint: String,
}

/// The `access` object of `access.*` responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccessView {
    /// SSH is wanted on (`false`: tv-authd keeps Dropbear stopped).
    pub ssh: bool,
    pub ssh_running: bool,
    pub sdb: bool,
    pub sdb_running: bool,
    pub sdb_host: Option<String>,
    pub keys: Vec<SshKeyView>,
    /// Where `tv ssh-key add` (or `tv-sdb push`) puts a key to add.
    pub key_drop: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
    pub pkg_type: PackageType,
    pub path: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UninstallRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
    pub pkgid: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListPackagesRequest {
    pub version: u32,
    pub id: String,
    pub method: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageType {
    Wgt,
    Tpk,
}

impl PackageType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wgt => "wgt",
            Self::Tpk => "tpk",
        }
    }
}

/// Which kind of approval a prompt is asking for. `exec` keeps the v1
/// once/always semantics; package operations are always one-shot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptKind {
    Exec,
    Install,
    Uninstall,
    /// A key pushed over SDB asks to be added to SSH's authorized keys.
    #[serde(rename = "ssh_key")]
    SshKey,
}

impl PromptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::Install => "install",
            Self::Uninstall => "uninstall",
            Self::SshKey => "ssh_key",
        }
    }

    /// Only root exec has persistent grants (§5.4 of the install plan).
    pub fn allows_always(self) -> bool {
        matches!(self, Self::Exec)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    UnsignedWillResign,
    ForeignWillResign,
    UserCa,
    Samsung,
}

/// Metadata shown on a package approval prompt. Every string field is
/// sanitized and length-limited before it reaches this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallMeta {
    pub pkgid: String,
    pub name: String,
    pub pkg_version: String,
    pub pkg_type: PackageType,
    pub size_bytes: u64,
    pub privileges: Vec<String>,
    pub signature: SignatureStatus,
    pub is_upgrade: bool,
}

/// Metadata shown on a `kind = "uninstall"` prompt. The daemon reads it from
/// the installed package database before asking, so the user sees exactly what
/// will be removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UninstallMeta {
    pub pkgid: String,
    pub name: String,
    pub pkg_version: String,
    pub pkg_type: PackageType,
}

/// One entry of `pkg.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageView {
    pub pkgid: String,
    pub name: String,
    pub pkg_version: String,
    pub pkg_type: PackageType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_app_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Deny,
    Once,
    Always,
}

#[derive(Debug)]
pub enum Request {
    Exec(ExecRequest),
    UiHello(UiHelloRequest),
    UiDecision(UiDecisionRequest),
    GrantsList(GrantsListRequest),
    GrantsRevoke(GrantsRevokeRequest),
    GrantsNotify(GrantsNotifyRequest),
    GrantsAudit(GrantsAuditRequest),
    PkgInstall(InstallRequest),
    PkgUninstall(UninstallRequest),
    PkgList(ListPackagesRequest),
    AccessStatus(AccessStatusRequest),
    AccessSet(AccessSetRequest),
    AccessSshRemove(AccessSshRemoveRequest),
}

impl Request {
    pub fn id(&self) -> &str {
        match self {
            Self::Exec(request) => &request.id,
            Self::UiHello(request) => &request.id,
            Self::UiDecision(request) => &request.id,
            Self::GrantsList(request) => &request.id,
            Self::GrantsRevoke(request) => &request.id,
            Self::GrantsNotify(request) => &request.id,
            Self::GrantsAudit(request) => &request.id,
            Self::PkgInstall(request) => &request.id,
            Self::PkgUninstall(request) => &request.id,
            Self::PkgList(request) => &request.id,
            Self::AccessStatus(request) => &request.id,
            Self::AccessSet(request) => &request.id,
            Self::AccessSshRemove(request) => &request.id,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    Malformed,
    UnsupportedVersion,
    UnsupportedMethod,
}

impl ParseError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed => "malformed_request",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnsupportedMethod => "unsupported_method",
        }
    }
}

#[derive(Deserialize)]
struct MethodPeek {
    version: u32,
    method: String,
}

pub fn parse_request(bytes: &[u8]) -> Result<Request, ParseError> {
    let peek: MethodPeek = serde_json::from_slice(bytes).map_err(|_| ParseError::Malformed)?;
    if peek.version < MIN_REQUEST_VERSION || peek.version > MAX_REQUEST_VERSION {
        return Err(ParseError::UnsupportedVersion);
    }
    let request = match peek.method.as_str() {
        "exec" => serde_json::from_slice::<ExecRequest>(bytes).map(Request::Exec),
        "ui.hello" => serde_json::from_slice::<UiHelloRequest>(bytes).map(Request::UiHello),
        "ui.decision" => {
            serde_json::from_slice::<UiDecisionRequest>(bytes).map(Request::UiDecision)
        }
        "grants.list" => {
            serde_json::from_slice::<GrantsListRequest>(bytes).map(Request::GrantsList)
        }
        "grants.revoke" => {
            serde_json::from_slice::<GrantsRevokeRequest>(bytes).map(Request::GrantsRevoke)
        }
        "grants.notify" => {
            serde_json::from_slice::<GrantsNotifyRequest>(bytes).map(Request::GrantsNotify)
        }
        "grants.audit" if peek.version >= PACKAGE_PROTOCOL_VERSION => {
            serde_json::from_slice::<GrantsAuditRequest>(bytes).map(Request::GrantsAudit)
        }
        "pkg.install" if peek.version >= PACKAGE_PROTOCOL_VERSION => {
            serde_json::from_slice::<InstallRequest>(bytes).map(Request::PkgInstall)
        }
        "pkg.uninstall" if peek.version >= PACKAGE_PROTOCOL_VERSION => {
            serde_json::from_slice::<UninstallRequest>(bytes).map(Request::PkgUninstall)
        }
        "pkg.list" if peek.version >= PACKAGE_PROTOCOL_VERSION => {
            serde_json::from_slice::<ListPackagesRequest>(bytes).map(Request::PkgList)
        }
        "access.status" if peek.version >= PACKAGE_PROTOCOL_VERSION => {
            serde_json::from_slice::<AccessStatusRequest>(bytes).map(Request::AccessStatus)
        }
        "access.set" if peek.version >= PACKAGE_PROTOCOL_VERSION => {
            serde_json::from_slice::<AccessSetRequest>(bytes).map(Request::AccessSet)
        }
        "access.ssh_remove" if peek.version >= PACKAGE_PROTOCOL_VERSION => {
            serde_json::from_slice::<AccessSshRemoveRequest>(bytes).map(Request::AccessSshRemove)
        }
        // A v1 client asking for a v2-only method is a version problem, not an
        // unknown method: the client should learn that it must upgrade.
        "pkg.install" | "pkg.uninstall" | "pkg.list" => return Err(ParseError::UnsupportedVersion),
        _ => return Err(ParseError::UnsupportedMethod),
    };
    request.map_err(|_| ParseError::Malformed)
}

pub fn is_valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_REQUEST_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

pub fn validate_exec(request: &ExecRequest) -> Result<u64, &'static str> {
    if request.command.is_empty() {
        return Err("empty_command");
    }
    let timeout = request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    if timeout == 0 || timeout > MAX_TIMEOUT_MS {
        return Err("invalid_timeout");
    }
    Ok(timeout)
}

pub fn validate_install(request: &InstallRequest) -> Result<u64, &'static str> {
    let path = request.path.as_str();
    if path.is_empty()
        || path.len() > MAX_PACKAGE_PATH_BYTES
        || path.as_bytes().contains(&0)
        || !std::path::Path::new(path).is_absolute()
    {
        return Err("invalid_path");
    }
    let timeout = request.timeout_ms.unwrap_or(DEFAULT_INSTALL_TIMEOUT_MS);
    if timeout == 0 || timeout > MAX_INSTALL_TIMEOUT_MS {
        return Err("invalid_timeout");
    }
    Ok(timeout)
}

pub fn validate_uninstall(request: &UninstallRequest) -> Result<u64, &'static str> {
    if !is_valid_package_id(&request.pkgid) {
        return Err("invalid_pkgid");
    }
    let timeout = request.timeout_ms.unwrap_or(DEFAULT_INSTALL_TIMEOUT_MS);
    if timeout == 0 || timeout > MAX_INSTALL_TIMEOUT_MS {
        return Err("invalid_timeout");
    }
    Ok(timeout)
}

pub fn validate_decision(request: &UiDecisionRequest) -> Result<(), &'static str> {
    if !is_valid_request_id(&request.prompt_id) || !is_valid_request_id(&request.nonce) {
        return Err("invalid_prompt_token");
    }
    Ok(())
}

pub fn validate_label(label: &str) -> Result<(), &'static str> {
    if !is_valid_package_label(label) {
        return Err("invalid_label");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct GrantView {
    pub label: String,
    pub package: String,
    pub granted_at: u64,
    pub notify: NotifyMode,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub version: u32,
    pub id: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_b64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_b64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grants: Option<Vec<GrantView>>,
    /// `grants.audit` result: `"<unix seconds> <event>"` lines, oldest first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pkgid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pkg_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub re_signed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_tail_b64: Option<String>,
    /// `pkg.list` result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<PackageView>>,
    /// Human-readable policy detail (for example the rejected privileges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `access.*` result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessView>,
}

impl Response {
    fn base(id: &str, status: &'static str) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id: id.to_owned(),
            status,
            exit_code: None,
            timed_out: None,
            stdout_b64: None,
            stderr_b64: None,
            truncated: None,
            grants: None,
            audit: None,
            revoked: None,
            pkgid: None,
            pkg_version: None,
            re_signed: None,
            log_tail_b64: None,
            packages: None,
            detail: None,
            error: None,
            access: None,
        }
    }

    pub fn ok(id: &str) -> Self {
        Self::base(id, "ok")
    }

    pub fn exec_ok(id: &str, outcome: ExecutionOutcome) -> Self {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::STANDARD;
        Self {
            version: PROTOCOL_VERSION,
            id: id.to_owned(),
            status: "ok",
            exit_code: Some(outcome.exit_code),
            timed_out: Some(outcome.timed_out),
            stdout_b64: Some(engine.encode(outcome.stdout)),
            stderr_b64: Some(engine.encode(outcome.stderr)),
            truncated: Some(outcome.truncated),
            grants: None,
            audit: None,
            revoked: None,
            pkgid: None,
            pkg_version: None,
            re_signed: None,
            log_tail_b64: None,
            packages: None,
            detail: None,
            error: None,
            access: None,
        }
    }

    pub fn install_ok(
        id: &str,
        pkgid: &str,
        pkg_version: &str,
        re_signed: bool,
        log_tail: &[u8],
    ) -> Self {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::STANDARD;
        let mut response = Self::base(id, "ok");
        response.pkgid = Some(pkgid.to_owned());
        response.pkg_version = Some(pkg_version.to_owned());
        response.re_signed = Some(re_signed);
        response.log_tail_b64 = Some(engine.encode(log_tail));
        response
    }

    pub fn uninstall_ok(id: &str, pkgid: &str, log_tail: &[u8]) -> Self {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::STANDARD;
        let mut response = Self::base(id, "ok");
        response.pkgid = Some(pkgid.to_owned());
        response.log_tail_b64 = Some(engine.encode(log_tail));
        response
    }

    pub fn packages(id: &str, packages: Vec<PackageView>) -> Self {
        let mut response = Self::base(id, "ok");
        response.packages = Some(packages);
        response
    }

    pub fn error_with_detail(
        id: &str,
        status: &'static str,
        error: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        let mut response = Self::error(id, status, error);
        response.detail = detail;
        response
    }

    pub fn error(id: &str, status: &'static str, error: impl Into<String>) -> Self {
        let mut response = Self::base(id, status);
        response.error = Some(error.into());
        response
    }

    pub fn grants(id: &str, grants: Vec<GrantView>) -> Self {
        let mut response = Self::base(id, "ok");
        response.grants = Some(grants);
        response
    }

    pub fn access(id: &str, view: AccessView) -> Self {
        let mut response = Self::base(id, "ok");
        response.access = Some(view);
        response
    }

    pub fn audit(id: &str, lines: Vec<String>) -> Self {
        let mut response = Self::base(id, "ok");
        response.audit = Some(lines);
        response
    }

    pub fn revoked(id: &str, revoked: bool) -> Self {
        let mut response = Self::base(id, "ok");
        response.revoked = Some(revoked);
        response
    }

    pub fn hello(id: &str) -> Self {
        Self::base(id, "ok")
    }
}

/// Which kind of notification a `ui.notice` entry describes. `exec` matches
/// the v1 behaviour ("ran a command as root"); package entries are queued by
/// prompt-free uid-0 operations so the user still sees what changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeKind {
    Exec,
    Install,
    Uninstall,
}

/// One package's notified actions since the last `ui.notice`.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeEntry {
    pub label: String,
    pub package: String,
    pub count: u32,
    /// Display copy of the most recent command.
    pub command: String,
    pub command_truncated: bool,
    pub kind: NoticeKind,
}

impl NoticeEntry {
    pub fn new(label: &str, command: &str) -> Self {
        let mut entry = Self {
            label: label.to_owned(),
            package: package_from_label(label).unwrap_or(label).to_owned(),
            count: 1,
            command: String::new(),
            command_truncated: false,
            kind: NoticeKind::Exec,
        };
        entry.set_command(command);
        entry
    }

    /// A prompt-free package operation performed by a root client. `package`
    /// is the actor shown in the toast (currently `"root"`), not the package
    /// that was changed; the changed package is in `command`.
    pub fn package(label: &str, package: &str, command: &str, kind: NoticeKind) -> Self {
        let mut entry = Self {
            label: label.to_owned(),
            package: display_safe(package),
            count: 1,
            command: String::new(),
            command_truncated: false,
            kind,
        };
        entry.set_command(command);
        entry
    }

    pub fn set_command(&mut self, command: &str) {
        (self.command, self.command_truncated) =
            truncate_text(&display_safe(command), MAX_NOTICE_COMMAND_BYTES);
    }
}

#[derive(Debug, Serialize)]
pub struct NoticeMessage {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub notices: Vec<NoticeEntry>,
}

impl NoticeMessage {
    pub fn new(notices: Vec<NoticeEntry>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            message_type: "ui.notice",
            notices,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PromptMessage {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub prompt_id: String,
    pub nonce: String,
    pub request_id: String,
    pub label: String,
    pub package: String,
    /// `exec`, `install`, or `uninstall`.
    pub kind: &'static str,
    /// Present for `kind = "install"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install: Option<InstallMeta>,
    /// Present for `kind = "uninstall"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uninstall: Option<UninstallMeta>,
    pub command: String,
    pub command_bytes: usize,
    pub command_truncated: bool,
    pub timeout_ms: u64,
}

impl PromptMessage {
    pub fn new(prompt_id: String, nonce: String, input: PromptInput<'_>) -> Self {
        let safe = display_safe(input.command);
        let (display, truncated) = truncate_text(&safe, MAX_PROMPT_COMMAND_BYTES);
        Self {
            version: PROTOCOL_VERSION,
            message_type: "ui.prompt",
            prompt_id,
            nonce,
            request_id: input.request_id.to_owned(),
            label: input.label.to_owned(),
            package: package_from_label(input.label)
                .unwrap_or(input.label)
                .to_owned(),
            kind: input.kind.as_str(),
            install: input.install,
            uninstall: input.uninstall,
            command: display,
            command_bytes: input.command.len(),
            command_truncated: truncated,
            timeout_ms: input.timeout_ms,
        }
    }
}

/// The request-derived part of a prompt. Kept as a struct so the prompt
/// constructor stays readable.
pub struct PromptInput<'a> {
    pub request_id: &'a str,
    pub label: &'a str,
    pub kind: PromptKind,
    pub install: Option<InstallMeta>,
    pub uninstall: Option<UninstallMeta>,
    pub command: &'a str,
    pub timeout_ms: u64,
}

/// Sanitize a package-controlled string for display in a prompt and cut it to
/// `maximum` bytes (on a character boundary).
pub fn prompt_text(value: &str, maximum: usize) -> String {
    let safe = display_safe(value);
    truncate_text(&safe, maximum).0
}

/// Replace characters that could make the displayed command differ from the
/// executed command. The command is never executed from this copy.
/// Control chars (except \n, \t, and \r before \n), Unicode format (Cf)
/// characters and line/paragraph separators become U+FFFD.
pub fn display_safe(value: &str) -> String {
    let mut chars = value.chars().peekable();
    let mut out = String::with_capacity(value.len());
    while let Some(character) = chars.next() {
        let keep = match character {
            '\n' | '\t' => true,
            // A lone CR can overwrite the line in some renderers.
            '\r' => chars.peek() == Some(&'\n'),
            _ => !character.is_control() && !is_invisible_format(character),
        };
        out.push(if keep { character } else { '\u{FFFD}' });
    }
    out
}

/// Unicode General_Category=Cf (no std API) plus U+2028/U+2029 separators.
fn is_invisible_format(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}' // incl. Zl/Zp and bidi embeddings
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

pub fn truncate_text(value: &str, maximum: usize) -> (String, bool) {
    if value.len() <= maximum {
        return (value.to_owned(), false);
    }
    let mut end = maximum;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

pub fn read_message(reader: &mut impl BufRead, maximum: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::with_capacity(maximum.min(1024));
    loop {
        let available = loop {
            match reader.fill_buf() {
                Ok(available) => break available,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        };
        if available.is_empty() {
            if line.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ));
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before newline",
            ));
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            if line.len() + position > maximum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("message exceeds {maximum} bytes"),
                ));
            }
            line.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            return Ok(line);
        }
        if line.len() + available.len() > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("message exceeds {maximum} bytes"),
            ));
        }
        line.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
}

pub fn write_json(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut payload = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_RESPONSE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message exceeds response limit",
        ));
    }
    payload.push(b'\n');
    writer.write_all(&payload)?;
    writer.flush()
}

pub fn write_response(writer: &mut impl Write, response: &Response) -> io::Result<()> {
    let payload = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_RESPONSE_BYTES {
        return write_json(
            writer,
            &Response::error(&response.id, "error", "response_too_large"),
        );
    }
    let mut payload = payload;
    payload.push(b'\n');
    writer.write_all(&payload)?;
    writer.flush()
}

/// One-shot synchronous request: connect, send `request`, return the first
/// response line (bounded by [`MAX_RESPONSE_BYTES`]).
pub fn call(socket: impl AsRef<Path>, request: &impl Serialize) -> io::Result<Vec<u8>> {
    let stream = UnixStream::connect(socket)?;
    write_json(&mut &stream, request)?;
    read_message(&mut BufReader::new(stream), MAX_RESPONSE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn exec_request() -> ExecRequest {
        ExecRequest {
            version: 1,
            id: "request-1".into(),
            method: "exec".into(),
            command: "id".into(),
            timeout_ms: Some(1000),
        }
    }

    #[test]
    fn validates_minimal_request() {
        assert_eq!(validate_exec(&exec_request()), Ok(1000));
    }

    #[test]
    fn rejects_bad_timeouts_and_commands() {
        let mut value = exec_request();
        value.timeout_ms = Some(0);
        assert_eq!(validate_exec(&value), Err("invalid_timeout"));
        value.timeout_ms = Some(MAX_TIMEOUT_MS + 1);
        assert_eq!(validate_exec(&value), Err("invalid_timeout"));
        value.timeout_ms = Some(10);
        value.command.clear();
        assert_eq!(validate_exec(&value), Err("empty_command"));
    }

    #[test]
    fn parses_each_request_method() {
        let exec = parse_request(
            br#"{"version":1,"id":"a","method":"exec","command":"id","timeout_ms":10}"#,
        )
        .unwrap();
        assert!(matches!(exec, Request::Exec(_)));

        let hello = parse_request(br#"{"version":1,"id":"b","method":"ui.hello"}"#).unwrap();
        assert!(matches!(hello, Request::UiHello(_)));

        let decision = parse_request(
            br#"{"version":1,"id":"c","method":"ui.decision","prompt_id":"p","nonce":"n","decision":"always"}"#,
        )
        .unwrap();
        match decision {
            Request::UiDecision(request) => assert_eq!(request.decision, Decision::Always),
            other => panic!("unexpected request: {other:?}"),
        }

        let list = parse_request(br#"{"version":1,"id":"d","method":"grants.list"}"#).unwrap();
        assert!(matches!(list, Request::GrantsList(_)));

        let revoke = parse_request(
            br#"{"version":1,"id":"e","method":"grants.revoke","label":"User::Pkg::org.example.app"}"#,
        )
        .unwrap();
        assert!(matches!(revoke, Request::GrantsRevoke(_)));

        let notify = parse_request(
            br#"{"version":1,"id":"f","method":"grants.notify","label":"User::Pkg::org.example.app","notify":"first"}"#,
        )
        .unwrap();
        match notify {
            Request::GrantsNotify(request) => assert_eq!(request.notify, NotifyMode::First),
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_fields_and_methods() {
        assert_eq!(
            parse_request(
                br#"{"version":1,"id":"a","method":"exec","command":"id","unexpected":true}"#
            )
            .unwrap_err(),
            ParseError::Malformed
        );
        assert_eq!(
            parse_request(br#"{"version":1,"id":"a","method":"shell"}"#).unwrap_err(),
            ParseError::UnsupportedMethod
        );
        assert_eq!(
            parse_request(br#"{"version":3,"id":"a","method":"exec"}"#).unwrap_err(),
            ParseError::UnsupportedVersion
        );
        assert_eq!(
            parse_request(br#"{"version":0,"id":"a","method":"exec"}"#).unwrap_err(),
            ParseError::UnsupportedVersion
        );
        assert_eq!(
            parse_request(b"{not-json}").unwrap_err(),
            ParseError::Malformed
        );
    }

    #[test]
    fn accepts_v1_and_v2_exec_requests() {
        assert!(matches!(
            parse_request(br#"{"version":1,"id":"a","method":"exec","command":"id"}"#).unwrap(),
            Request::Exec(_)
        ));
        assert!(matches!(
            parse_request(br#"{"version":2,"id":"a","method":"exec","command":"id"}"#).unwrap(),
            Request::Exec(_)
        ));
    }

    #[test]
    fn package_installs_require_protocol_v2() {
        let v1 = br#"{"version":1,"id":"a","method":"pkg.install","pkg_type":"wgt","path":"/tmp/a.wgt"}"#;
        assert_eq!(
            parse_request(v1).unwrap_err(),
            ParseError::UnsupportedVersion
        );
        let v2 = br#"{"version":2,"id":"a","method":"pkg.install","pkg_type":"wgt","path":"/tmp/a.wgt"}"#;
        match parse_request(v2).unwrap() {
            Request::PkgInstall(request) => {
                assert_eq!(request.pkg_type, PackageType::Wgt);
                assert_eq!(request.path, "/tmp/a.wgt");
            }
            other => panic!("unexpected request: {other:?}"),
        }
        assert_eq!(
            parse_request(
                br#"{"version":2,"id":"a","method":"pkg.install","pkg_type":"rpm","path":"/tmp/a"}"#
            )
            .unwrap_err(),
            ParseError::Malformed
        );
    }

    #[test]
    fn validates_install_paths_and_timeouts() {
        let mut request = InstallRequest {
            version: 2,
            id: "inst".into(),
            method: "pkg.install".into(),
            pkg_type: PackageType::Wgt,
            path: "/tmp/a.wgt".into(),
            timeout_ms: None,
        };
        assert_eq!(validate_install(&request), Ok(DEFAULT_INSTALL_TIMEOUT_MS));

        request.path = "relative/a.wgt".into();
        assert_eq!(validate_install(&request), Err("invalid_path"));
        request.path = "/tmp/a\0.wgt".into();
        assert_eq!(validate_install(&request), Err("invalid_path"));
        request.path = format!("/{}", "a".repeat(MAX_PACKAGE_PATH_BYTES));
        assert_eq!(validate_install(&request), Err("invalid_path"));

        request.path = "/tmp/a.wgt".into();
        request.timeout_ms = Some(0);
        assert_eq!(validate_install(&request), Err("invalid_timeout"));
        request.timeout_ms = Some(MAX_INSTALL_TIMEOUT_MS + 1);
        assert_eq!(validate_install(&request), Err("invalid_timeout"));
    }

    #[test]
    fn package_uninstall_and_list_require_protocol_v2() {
        for body in [
            br#"{"version":1,"id":"a","method":"pkg.uninstall","pkgid":"org.example.app"}"#
                .as_slice(),
            br#"{"version":1,"id":"a","method":"pkg.list"}"#.as_slice(),
        ] {
            assert_eq!(
                parse_request(body).unwrap_err(),
                ParseError::UnsupportedVersion
            );
        }
        match parse_request(
            br#"{"version":2,"id":"a","method":"pkg.uninstall","pkgid":"org.example.app"}"#,
        )
        .unwrap()
        {
            Request::PkgUninstall(request) => assert_eq!(request.pkgid, "org.example.app"),
            other => panic!("unexpected request: {other:?}"),
        }
        assert!(matches!(
            parse_request(br#"{"version":2,"id":"a","method":"pkg.list"}"#).unwrap(),
            Request::PkgList(_)
        ));
        assert_eq!(
            parse_request(br#"{"version":2,"id":"a","method":"pkg.uninstall"}"#).unwrap_err(),
            ParseError::Malformed
        );
    }

    #[test]
    fn validates_uninstall_package_ids() {
        let mut request = UninstallRequest {
            version: 2,
            id: "uninst".into(),
            method: "pkg.uninstall".into(),
            pkgid: "org.example.app".into(),
            timeout_ms: None,
        };
        assert_eq!(validate_uninstall(&request), Ok(DEFAULT_INSTALL_TIMEOUT_MS));
        for bad in ["", "../etc", "bad id", "a/b"] {
            request.pkgid = bad.into();
            assert_eq!(validate_uninstall(&request), Err("invalid_pkgid"), "{bad}");
        }
        request.pkgid = "org.example.app".into();
        request.timeout_ms = Some(MAX_INSTALL_TIMEOUT_MS + 1);
        assert_eq!(validate_uninstall(&request), Err("invalid_timeout"));
    }

    #[test]
    fn validates_decision_and_revoke_tokens() {
        let mut decision = UiDecisionRequest {
            version: 1,
            id: "d".into(),
            method: "ui.decision".into(),
            prompt_id: "prompt".into(),
            nonce: "nonce".into(),
            decision: Decision::Once,
        };
        assert_eq!(validate_decision(&decision), Ok(()));
        decision.nonce = "bad nonce".into();
        assert_eq!(validate_decision(&decision), Err("invalid_prompt_token"));

        let mut revoke = GrantsRevokeRequest {
            version: 1,
            id: "r".into(),
            method: "grants.revoke".into(),
            label: "User::Pkg::org.example.app".into(),
        };
        assert_eq!(validate_label(&revoke.label), Ok(()));
        revoke.label = "System::Privileged".into();
        assert_eq!(validate_label(&revoke.label), Err("invalid_label"));
    }

    #[test]
    fn framing_accepts_limit_and_rejects_oversize() {
        let mut exact = vec![b'x'; 8];
        exact.push(b'\n');
        assert_eq!(read_message(&mut Cursor::new(exact), 8).unwrap().len(), 8);

        let mut oversized = vec![b'x'; 9];
        oversized.push(b'\n');
        assert!(read_message(&mut Cursor::new(oversized), 8).is_err());
    }

    #[test]
    fn framing_handles_multiple_lines_and_partial_reads() {
        let input = b"one\ntwo\n";
        let mut reader = Cursor::new(&input[..]);
        assert_eq!(read_message(&mut reader, 16).unwrap(), b"one");
        assert_eq!(read_message(&mut reader, 16).unwrap(), b"two");
        assert!(read_message(&mut reader, 16).is_err());
    }

    #[test]
    fn prompt_marks_truncation() {
        let prompt = PromptMessage::new(
            "p".into(),
            "n".into(),
            PromptInput {
                request_id: "r",
                label: "User::Pkg::org.example.app",
                kind: PromptKind::Exec,
                install: None,
                uninstall: None,
                command: &"x".repeat(MAX_PROMPT_COMMAND_BYTES + 10),
                timeout_ms: 1000,
            },
        );
        assert!(prompt.command_truncated);
        assert_eq!(prompt.command.len(), MAX_PROMPT_COMMAND_BYTES);
        assert_eq!(prompt.package, "org.example.app");
        assert_eq!(prompt.kind, "exec");
        assert!(prompt.install.is_none());
    }

    /// What the daemon writes must parse with the clients' `ui` types.
    #[test]
    fn daemon_messages_parse_as_client_views() {
        let prompt = PromptMessage::new(
            "p".into(),
            "n".into(),
            PromptInput {
                request_id: "r",
                label: "User::Pkg::org.example.app",
                kind: PromptKind::Uninstall,
                install: None,
                uninstall: Some(UninstallMeta {
                    pkgid: "org.example.app".into(),
                    name: "Example".into(),
                    pkg_version: "1.0.0".into(),
                    pkg_type: PackageType::Tpk,
                }),
                command: "remove org.example.app",
                timeout_ms: 1000,
            },
        );
        let view: ui::PromptInfo =
            serde_json::from_slice(&serde_json::to_vec(&prompt).unwrap()).unwrap();
        assert_eq!((view.prompt_id.as_str(), view.nonce.as_str()), ("p", "n"));
        assert_eq!(view.package, "org.example.app");
        assert!(view.is_package_operation());
        assert_eq!(view.uninstall.unwrap().pkg_type, "tpk");
        assert_eq!(view.timeout_ms, 1000);

        let mut entry = NoticeEntry::new("User::Pkg::org.example.app", "id");
        entry.count = 3;
        let notice = serde_json::to_value(NoticeMessage::new(vec![entry])).unwrap();
        assert_eq!(notice["type"], "ui.notice");
        let notices: Vec<ui::NoticeInfo> =
            serde_json::from_value(notice["notices"].clone()).unwrap();
        assert_eq!((notices[0].count, notices[0].kind.as_str()), (3, "exec"));

        let grants = Response::grants(
            "g",
            vec![GrantView {
                label: "User::Pkg::org.example.app".into(),
                package: "org.example.app".into(),
                granted_at: 1,
                notify: NotifyMode::First,
            }],
        );
        let grants = serde_json::to_value(grants).unwrap();
        let views: Vec<ui::GrantInfo> = serde_json::from_value(grants["grants"].clone()).unwrap();
        assert_eq!(views[0].notify, "first");
    }

    #[test]
    fn install_prompt_carries_package_metadata() {
        let meta = InstallMeta {
            pkgid: "org.example.app".into(),
            name: "Example".into(),
            pkg_version: "1.0.0".into(),
            pkg_type: PackageType::Wgt,
            size_bytes: 1234,
            privileges: vec!["http://tizen.org/privilege/internet".into()],
            signature: SignatureStatus::UnsignedWillResign,
            is_upgrade: false,
        };
        let prompt = PromptMessage::new(
            "p".into(),
            "n".into(),
            PromptInput {
                request_id: "r",
                label: "User::Pkg::org.example.app",
                kind: PromptKind::Install,
                install: Some(meta),
                uninstall: None,
                command: "install wgt /tmp/app.wgt",
                timeout_ms: 1000,
            },
        );
        assert_eq!(prompt.kind, "install");
        let value = serde_json::to_value(&prompt).unwrap();
        assert_eq!(value["install"]["pkgid"], "org.example.app");
        assert_eq!(value["install"]["signature"], "unsigned_will_resign");
        assert!(!PromptKind::Install.allows_always());
        assert!(PromptKind::Exec.allows_always());
    }

    #[test]
    fn prompt_escapes_control_and_bidi_characters() {
        let prompt = PromptMessage::new(
            "p".into(),
            "n".into(),
            PromptInput {
                request_id: "r",
                label: "User::Pkg::org.example.app",
                kind: PromptKind::Exec,
                install: None,
                uninstall: None,
                command: "one\u{0}two\u{202e}three\nfour",
                timeout_ms: 1000,
            },
        );
        assert_eq!(prompt.command, "one\u{FFFD}two\u{FFFD}three\nfour");
        assert!(!prompt.command_truncated);
    }

    #[test]
    fn display_safe_replaces_invisible_characters() {
        for bad in [
            '\u{200B}',
            '\u{200E}',
            '\u{200F}',
            '\u{2060}',
            '\u{FEFF}',
            '\u{E0041}',
            '\u{2028}',
            '\u{2029}',
            '\u{00AD}',
            '\u{202E}',
        ] {
            assert_eq!(display_safe(&format!("a{bad}b")), "a\u{FFFD}b", "{bad:?}");
        }
        assert_eq!(display_safe("a\rb"), "a\u{FFFD}b");
        assert_eq!(display_safe("a\r\nb"), "a\r\nb");
    }

    #[test]
    fn display_safe_keeps_ordinary_text() {
        let text = "caf\u{e9} \u{65e5}\u{672c} \u{1F600}\tx\ny";
        assert_eq!(display_safe(text), text);
    }
}
