use crate::identity::{is_valid_package_label, package_from_label};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
pub const MAX_REQUEST_ID_BYTES: usize = 128;
pub const MAX_PROMPT_COMMAND_BYTES: usize = 4096;
pub const MAX_NOTICE_COMMAND_BYTES: usize = 256;

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
}

impl Request {
    pub fn id(&self) -> &str {
        match self {
            Self::Exec(request) => &request.id,
            Self::UiHello(request) => &request.id,
            Self::UiDecision(request) => &request.id,
            Self::GrantsList(request) => &request.id,
            Self::GrantsRevoke(request) => &request.id,
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
    if peek.version != PROTOCOL_VERSION {
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

pub fn validate_decision(request: &UiDecisionRequest) -> Result<(), &'static str> {
    if !is_valid_request_id(&request.prompt_id) || !is_valid_request_id(&request.nonce) {
        return Err("invalid_prompt_token");
    }
    Ok(())
}

pub fn validate_revoke(request: &GrantsRevokeRequest) -> Result<(), &'static str> {
    if !is_valid_package_label(&request.label) {
        return Err("invalid_label");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct GrantView {
    pub label: String,
    pub package: String,
    pub granted_at: u64,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `ui.hello` only: `"notice"` when the daemon launched the UI just to
    /// show execution notices, so it should stay out of the way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch: Option<&'static str>,
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
            revoked: None,
            error: None,
            launch: None,
        }
    }

    pub fn ok(id: &str) -> Self {
        Self::base(id, "ok")
    }

    pub fn exec_ok(id: &str, outcome: crate::executor::ExecutionOutcome) -> Self {
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
            revoked: None,
            error: None,
            launch: None,
        }
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

    pub fn revoked(id: &str, revoked: bool) -> Self {
        let mut response = Self::base(id, "ok");
        response.revoked = Some(revoked);
        response
    }

    pub fn hello(id: &str, launch: Option<&'static str>) -> Self {
        let mut response = Self::base(id, "ok");
        response.launch = launch;
        response
    }
}

/// One package's granted executions since the last `ui.notice`.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeEntry {
    pub label: String,
    pub package: String,
    pub count: u32,
    /// Display copy of the most recent command.
    pub command: String,
    pub command_truncated: bool,
}

impl NoticeEntry {
    pub fn new(label: &str, command: &str) -> Self {
        let mut entry = Self {
            label: label.to_owned(),
            package: package_from_label(label).unwrap_or(label).to_owned(),
            count: 1,
            command: String::new(),
            command_truncated: false,
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
    pub command: String,
    pub command_bytes: usize,
    pub command_truncated: bool,
    pub timeout_ms: u64,
}

impl PromptMessage {
    pub fn new(
        prompt_id: String,
        nonce: String,
        request_id: &str,
        label: &str,
        command: &str,
        timeout_ms: u64,
    ) -> Self {
        let safe = display_safe(command);
        let (display, truncated) = truncate_text(&safe, MAX_PROMPT_COMMAND_BYTES);
        Self {
            version: PROTOCOL_VERSION,
            message_type: "ui.prompt",
            prompt_id,
            nonce,
            request_id: request_id.to_owned(),
            label: label.to_owned(),
            package: package_from_label(label).unwrap_or(label).to_owned(),
            command: display,
            command_bytes: command.len(),
            command_truncated: truncated,
            timeout_ms,
        }
    }
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
        let available = reader.fill_buf()?;
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
            parse_request(br#"{"version":2,"id":"a","method":"exec"}"#).unwrap_err(),
            ParseError::UnsupportedVersion
        );
        assert_eq!(
            parse_request(b"{not-json}").unwrap_err(),
            ParseError::Malformed
        );
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
        assert_eq!(validate_revoke(&revoke), Ok(()));
        revoke.label = "System::Privileged".into();
        assert_eq!(validate_revoke(&revoke), Err("invalid_label"));
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
            "r",
            "User::Pkg::org.example.app",
            &"x".repeat(MAX_PROMPT_COMMAND_BYTES + 10),
            1000,
        );
        assert!(prompt.command_truncated);
        assert_eq!(prompt.command.len(), MAX_PROMPT_COMMAND_BYTES);
        assert_eq!(prompt.package, "org.example.app");
    }

    #[test]
    fn prompt_escapes_control_and_bidi_characters() {
        let prompt = PromptMessage::new(
            "p".into(),
            "n".into(),
            "r",
            "User::Pkg::org.example.app",
            "one\u{0}two\u{202e}three\nfour",
            1000,
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
