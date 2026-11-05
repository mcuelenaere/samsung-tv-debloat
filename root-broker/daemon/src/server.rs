use crate::executor;
use crate::grants::GrantStore;
use crate::identity::PeerIdentity;
use crate::logger::Logger;
use crate::protocol::{
    read_message, validate_request, write_message, ExecRequest, ExecResponse, MAX_REQUEST_BYTES,
    PROTOCOL_VERSION,
};
use std::collections::HashSet;
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub max_connections: usize,
    pub max_requests_per_connection: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_connections: 16,
            max_requests_per_connection: 1024,
        }
    }
}

pub fn handle_connection(
    mut stream: UnixStream,
    identity: PeerIdentity,
    grants: Arc<GrantStore>,
    logger: Option<Arc<Logger>>,
    config: &ServerConfig,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut request_ids = HashSet::new();

    for _ in 0..config.max_requests_per_connection {
        let line = match read_message(&mut stream, MAX_REQUEST_BYTES) {
            Ok(line) => line,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(());
            }
            Err(error) => {
                let _ = write_message(
                    &mut stream,
                    &ExecResponse::error("", "error", "malformed_request"),
                );
                return Err(error);
            }
        };
        let request: ExecRequest = match serde_json::from_slice(&line) {
            Ok(request) => request,
            Err(error) => {
                log(
                    &logger,
                    &format!(
                        "requester={} pid={} malformed_request={error}",
                        identity.label, identity.pid
                    ),
                );
                let _ = write_message(
                    &mut stream,
                    &ExecResponse::error("", "error", "malformed_request"),
                );
                return Err(io::Error::new(io::ErrorKind::InvalidData, error));
            }
        };

        let response_id = if request.id.len() <= 128
            && request.id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':')
            }) {
            request.id.clone()
        } else {
            String::new()
        };

        let timeout_ms = match validate_request(&request) {
            Ok(timeout) => timeout,
            Err(error) => {
                let _ = write_message(
                    &mut stream,
                    &ExecResponse::error(&response_id, "error", error),
                );
                continue;
            }
        };

        if !request_ids.insert(request.id.clone()) {
            let _ = write_message(
                &mut stream,
                &ExecResponse::error(&response_id, "error", "duplicate_request_id"),
            );
            continue;
        }

        if !grants.is_allowed(&identity.label) {
            log(
                &logger,
                &format!(
                    "requester={} uid={} pid={} request={} denied=not_granted",
                    identity.label, identity.uid, identity.pid, response_id
                ),
            );
            let _ = write_message(
                &mut stream,
                &ExecResponse::error(&response_id, "denied", "not_granted"),
            );
            continue;
        }

        log(
            &logger,
            &format!(
                "requester={} uid={} pid={} request={} command_bytes={} timeout_ms={} execute",
                identity.label,
                identity.uid,
                identity.pid,
                response_id,
                request.command.len(),
                timeout_ms
            ),
        );

        match executor::execute(&request.command, timeout_ms) {
            Ok(outcome) => {
                log(
                    &logger,
                    &format!(
                        "requester={} request={} exit_code={} timed_out={} truncated={}",
                        identity.label,
                        response_id,
                        outcome.exit_code,
                        outcome.timed_out,
                        outcome.truncated
                    ),
                );
                write_message(&mut stream, &ExecResponse::ok(&response_id, outcome))?;
            }
            Err(error) => {
                log(
                    &logger,
                    &format!(
                        "requester={} request={} execution_error={error}",
                        identity.label, response_id
                    ),
                );
                write_message(
                    &mut stream,
                    &ExecResponse::error(&response_id, "error", "execution_failed"),
                )?;
            }
        }
    }

    let _ = write_message(
        &mut stream,
        &ExecResponse {
            version: PROTOCOL_VERSION,
            id: String::new(),
            status: "error",
            exit_code: None,
            timed_out: None,
            stdout_b64: None,
            stderr_b64: None,
            truncated: None,
            error: Some("too_many_requests".into()),
        },
    );
    Ok(())
}

fn log(logger: &Option<Arc<Logger>>, message: &str) {
    if let Some(logger) = logger {
        let _ = logger.log(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::thread;

    fn test_grants(allowed: &str) -> (tempfile::TempDir, Arc<GrantStore>) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants.json");
        std::fs::write(
            &path,
            format!("{{\"version\":1,\"grants\":{{\"{allowed}\":{{\"granted_at\":1}}}}}}"),
        )
        .unwrap();
        let store = GrantStore::load(path).unwrap();
        (directory, Arc::new(store))
    }

    fn request_line(id: &str, command: &str) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&json!({
            "version": 1,
            "id": id,
            "method": "exec",
            "command": command,
            "timeout_ms": 1000
        }))
        .unwrap();
        bytes.push(b'\n');
        bytes
    }

    fn read_response(stream: &mut UnixStream) -> Value {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            assert_eq!(count, 1);
            if buffer[0] == b'\n' {
                break;
            }
            bytes.push(buffer[0]);
        }
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn denies_ungranted_package_without_execution() {
        let (_directory, grants) = test_grants("User::Pkg::org.example.allowed");
        let identity = PeerIdentity {
            pid: 123,
            uid: 5001,
            gid: 100,
            label: "User::Pkg::org.example.denied".into(),
        };
        let (mut client, server) = UnixStream::pair().unwrap();
        let handle = thread::spawn(move || {
            handle_connection(server, identity, grants, None, &ServerConfig::default())
        });
        client.write_all(&request_line("deny-1", "id")).unwrap();
        let response = read_response(&mut client);
        assert_eq!(response["status"], "denied");
        assert_eq!(response["error"], "not_granted");
        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn rejects_malformed_json() {
        let (_directory, grants) = test_grants("User::Pkg::org.example.allowed");
        let identity = PeerIdentity {
            pid: 123,
            uid: 5001,
            gid: 100,
            label: "User::Pkg::org.example.allowed".into(),
        };
        let (mut client, server) = UnixStream::pair().unwrap();
        let handle = thread::spawn(move || {
            handle_connection(server, identity, grants, None, &ServerConfig::default())
        });
        client.write_all(b"{not-json}\n").unwrap();
        let response = read_response(&mut client);
        assert_eq!(response["status"], "error");
        assert_eq!(response["error"], "malformed_request");
        drop(client);
        assert!(handle.join().unwrap().is_err());
    }

    #[test]
    fn executes_for_granted_package_on_linux() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let (_directory, grants) = test_grants("User::Pkg::org.example.allowed");
        let identity = PeerIdentity {
            pid: 123,
            uid: 5001,
            gid: 100,
            label: "User::Pkg::org.example.allowed".into(),
        };
        let (mut client, server) = UnixStream::pair().unwrap();
        let handle = thread::spawn(move || {
            handle_connection(server, identity, grants, None, &ServerConfig::default())
        });
        client
            .write_all(&request_line("exec-1", "printf hello"))
            .unwrap();
        let response = read_response(&mut client);
        assert_eq!(response["status"], "ok");
        assert_eq!(response["exit_code"], 0);
        assert_eq!(response["stdout_b64"], "aGVsbG8=");
        drop(client);
        handle.join().unwrap().unwrap();
    }
}
