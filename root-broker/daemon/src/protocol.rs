use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
pub const MAX_REQUEST_ID_BYTES: usize = 128;

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

#[derive(Debug, Serialize)]
pub struct ExecResponse {
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
    pub error: Option<String>,
}

impl ExecResponse {
    pub fn ok(id: &str, outcome: crate::executor::ExecutionOutcome) -> Self {
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
            error: None,
        }
    }

    pub fn error(id: &str, status: &'static str, error: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id: id.to_owned(),
            status,
            exit_code: None,
            timed_out: None,
            stdout_b64: None,
            stderr_b64: None,
            truncated: None,
            error: Some(error.into()),
        }
    }
}

pub fn validate_request(request: &ExecRequest) -> Result<u64, &'static str> {
    if request.version != PROTOCOL_VERSION {
        return Err("unsupported_version");
    }
    if request.method != "exec" {
        return Err("unsupported_method");
    }
    if request.id.is_empty()
        || request.id.len() > MAX_REQUEST_ID_BYTES
        || !request
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err("invalid_request_id");
    }
    if request.command.is_empty() {
        return Err("empty_command");
    }
    let timeout = request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    if timeout == 0 || timeout > MAX_TIMEOUT_MS {
        return Err("invalid_timeout");
    }
    Ok(timeout)
}

pub fn read_message(reader: &mut impl Read, maximum: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::with_capacity(maximum.min(1024));
    let mut byte = [0_u8; 1];
    loop {
        let count = reader.read(&mut byte)?;
        if count == 0 {
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
        if byte[0] == b'\n' {
            return Ok(line);
        }
        if line.len() == maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("message exceeds {maximum} bytes"),
            ));
        }
        line.push(byte[0]);
    }
}

pub fn write_message(writer: &mut impl Write, response: &ExecResponse) -> io::Result<()> {
    let mut payload = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_RESPONSE_BYTES {
        payload = serde_json::to_vec(&ExecResponse::error(
            &response.id,
            "error",
            "response_too_large",
        ))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }
    payload.push(b'\n');
    writer.write_all(&payload)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn request() -> ExecRequest {
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
        assert_eq!(validate_request(&request()), Ok(1000));
    }

    #[test]
    fn rejects_unknown_version_and_method() {
        let mut value = request();
        value.version = 2;
        assert_eq!(validate_request(&value), Err("unsupported_version"));
        value.version = 1;
        value.method = "shell".into();
        assert_eq!(validate_request(&value), Err("unsupported_method"));
    }

    #[test]
    fn rejects_bad_ids_timeouts_and_commands() {
        let mut value = request();
        value.id = "bad id".into();
        assert_eq!(validate_request(&value), Err("invalid_request_id"));
        value.id = "ok".into();
        value.timeout_ms = Some(0);
        assert_eq!(validate_request(&value), Err("invalid_timeout"));
        value.timeout_ms = Some(MAX_TIMEOUT_MS + 1);
        assert_eq!(validate_request(&value), Err("invalid_timeout"));
        value.timeout_ms = Some(10);
        value.command.clear();
        assert_eq!(validate_request(&value), Err("empty_command"));
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
}
