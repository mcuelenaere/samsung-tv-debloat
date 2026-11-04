#[cfg(not(target_os = "linux"))]
compile_error!("peer-identity uses Linux SO_PEERCRED and SO_PEERSEC");

use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Write as _};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process;

const MAX_PROBE_BYTES: usize = 128;
const MAX_SECURITY_LABEL_BYTES: usize = 4096;

#[derive(Debug)]
struct PeerCredentials {
    pid: libc::pid_t,
    uid: libc::uid_t,
    gid: libc::gid_t,
}

#[derive(Debug)]
enum PeerSecurity {
    Present(Vec<u8>),
    Missing(&'static str),
    Unsupported(io::Error),
    Error(io::Error),
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args();
    let program = args.next().unwrap_or_else(|| "peer-identity".to_owned());
    let usage = format!(
        "usage:\n  {program} server SOCKET MAX_CONNECTIONS\n  {program} client SOCKET PROBE"
    );

    match args.next().as_deref() {
        Some("server") => {
            let socket = args.next().ok_or_else(|| usage.clone())?;
            let max_connections = args.next().ok_or_else(|| usage.clone())?;
            if args.next().is_some() {
                return Err(usage.into());
            }
            let max_connections = max_connections.parse::<usize>()?;
            if max_connections == 0 {
                return Err("MAX_CONNECTIONS must be greater than zero".into());
            }
            run_server(Path::new(&socket), max_connections)?;
        }
        Some("client") => {
            let socket = args.next().ok_or_else(|| usage.clone())?;
            let probe = args.next().ok_or_else(|| usage.clone())?;
            if args.next().is_some() {
                return Err(usage.into());
            }
            run_client(Path::new(&socket), &probe)?;
        }
        _ => return Err(usage.into()),
    }

    Ok(())
}

fn run_server(path: &Path, max_connections: usize) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => return Err(format!("refusing to replace non-socket {}", path.display()).into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let listener = UnixListener::bind(path)?;
    println!(
        "listening socket=\"{}\" max_connections={max_connections}",
        escape_bytes(path.as_os_str().as_encoded_bytes())
    );

    for connection in 1..=max_connections {
        let (mut stream, _) = listener.accept()?;
        let credentials = match peer_credentials(stream.as_raw_fd()) {
            Ok(credentials) => credentials,
            Err(error) => {
                println!(
                    "connection={connection} so_peercred=error errno={} message=\"{}\"",
                    error.raw_os_error().unwrap_or(-1),
                    escape_bytes(error.to_string().as_bytes())
                );
                continue;
            }
        };

        // Identity must be acquired before consuming any bytes from the peer.
        let security = peer_security(stream.as_raw_fd());
        let security = format_peer_security(&security);
        let identity = format!(
            "connection={connection} so_peercred=present pid={} uid={} gid={} {security}",
            credentials.pid, credentials.uid, credentials.gid
        );

        match read_bounded_line(&mut stream, MAX_PROBE_BYTES) {
            Ok(probe) => {
                println!("{identity} probe=\"{}\"", escape_bytes(&probe));
                stream.write_all(b"ok\n")?;
            }
            Err(error) => {
                println!(
                    "{identity} probe=error message=\"{}\"",
                    escape_bytes(error.to_string().as_bytes())
                );
                stream.write_all(b"error\n")?;
            }
        }
    }

    fs::remove_file(path)?;
    Ok(())
}

fn run_client(path: &Path, probe: &str) -> Result<(), Box<dyn Error>> {
    if probe.is_empty() {
        return Err("PROBE must not be empty".into());
    }
    if probe.len() > MAX_PROBE_BYTES {
        return Err(format!("PROBE exceeds {MAX_PROBE_BYTES} bytes").into());
    }
    if probe.bytes().any(|byte| matches!(byte, b'\n' | b'\r')) {
        return Err("PROBE must be a single line without CR or LF".into());
    }

    let mut stream = UnixStream::connect(path)?;
    stream.write_all(probe.as_bytes())?;
    stream.write_all(b"\n")?;
    let reply = read_bounded_line(&mut stream, 16)?;
    if reply != b"ok" {
        return Err(format!("unexpected server reply: {}", escape_bytes(&reply)).into());
    }

    println!(
        "client_pid={} probe=\"{}\" reply=\"ok\"",
        process::id(),
        escape_bytes(probe.as_bytes())
    );
    Ok(())
}

fn peer_credentials(fd: libc::c_int) -> io::Result<PeerCredentials> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("short SO_PEERCRED result: {length} bytes"),
        ));
    }

    Ok(PeerCredentials {
        pid: credentials.pid,
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

fn peer_security(fd: libc::c_int) -> PeerSecurity {
    let mut buffer = [0_u8; MAX_SECURITY_LABEL_BYTES];
    let mut length = buffer.len() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERSEC,
            buffer.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result == -1 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOPROTOOPT) | Some(libc::EOPNOTSUPP) => PeerSecurity::Unsupported(error),
            Some(libc::ENODATA) => PeerSecurity::Missing("SO_PEERSEC returned ENODATA"),
            _ => PeerSecurity::Error(error),
        };
    }

    let length = length as usize;
    if length == 0 {
        return PeerSecurity::Missing("SO_PEERSEC returned a zero-length label");
    }
    if length > buffer.len() {
        return PeerSecurity::Error(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("SO_PEERSEC returned invalid length {length}"),
        ));
    }

    let mut label = &buffer[..length];
    while label.last() == Some(&0) {
        label = &label[..label.len() - 1];
    }
    if label.is_empty() {
        return PeerSecurity::Missing("SO_PEERSEC returned an empty label");
    }

    PeerSecurity::Present(label.to_vec())
}

fn format_peer_security(security: &PeerSecurity) -> String {
    match security {
        PeerSecurity::Present(label) => {
            format!("so_peersec=present label=\"{}\"", escape_bytes(label))
        }
        PeerSecurity::Missing(reason) => {
            format!(
                "so_peersec=missing reason=\"{}\"",
                escape_bytes(reason.as_bytes())
            )
        }
        PeerSecurity::Unsupported(error) => format!(
            "so_peersec=unsupported errno={} message=\"{}\"",
            error.raw_os_error().unwrap_or(-1),
            escape_bytes(error.to_string().as_bytes())
        ),
        PeerSecurity::Error(error) => format!(
            "so_peersec=error errno={} message=\"{}\"",
            error.raw_os_error().unwrap_or(-1),
            escape_bytes(error.to_string().as_bytes())
        ),
    }
}

fn read_bounded_line(reader: &mut impl Read, maximum: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::with_capacity(maximum.min(64));
    let mut byte = [0_u8; 1];
    loop {
        let count = reader.read(&mut byte)?;
        if count == 0 {
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
                format!("line exceeds {maximum} bytes"),
            ));
        }
        line.push(byte[0]);
    }
}

fn escape_bytes(bytes: &[u8]) -> String {
    let mut escaped = String::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'\\' => escaped.push_str("\\\\"),
            b'"' => escaped.push_str("\\\""),
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            0x20..=0x7e => escaped.push(char::from(byte)),
            _ => write!(escaped, "\\x{byte:02x}").expect("writing to String cannot fail"),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::{escape_bytes, read_bounded_line};
    use std::io::Cursor;

    #[test]
    fn bounded_line_accepts_limit_and_newline() {
        let input = vec![b'x'; 128];
        let mut framed = input.clone();
        framed.push(b'\n');
        assert_eq!(
            read_bounded_line(&mut Cursor::new(framed), 128).unwrap(),
            input
        );
    }

    #[test]
    fn bounded_line_rejects_overlong_input() {
        let mut framed = vec![b'x'; 129];
        framed.push(b'\n');
        assert!(read_bounded_line(&mut Cursor::new(framed), 128).is_err());
    }

    #[test]
    fn escaping_prevents_log_injection() {
        assert_eq!(escape_bytes(b"a\n\"\\\xff"), "a\\n\\\"\\\\\\xff");
    }
}
