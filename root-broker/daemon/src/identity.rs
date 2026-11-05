use std::io;
use std::os::fd::RawFd;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub pid: libc::pid_t,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub label: String,
}

pub fn is_valid_package_label(label: &str) -> bool {
    const PREFIX: &str = "User::Pkg::";
    let Some(package) = label.strip_prefix(PREFIX) else {
        return false;
    };
    !package.is_empty()
        && package.len() <= 127
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(target_os = "linux")]
pub fn from_fd(fd: RawFd) -> io::Result<PeerIdentity> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut lengths = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut lengths,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    if lengths as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short SO_PEERCRED result",
        ));
    }

    let mut buffer = [0_u8; 4096];
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
        return Err(io::Error::last_os_error());
    }
    if length as usize > buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SO_PEERSEC returned an invalid length",
        ));
    }
    let mut bytes = &buffer[..length as usize];
    while bytes.last() == Some(&0) {
        bytes = &bytes[..bytes.len() - 1];
    }
    let label = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "SO_PEERSEC is not UTF-8"))?;
    if label.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SO_PEERSEC returned an empty label",
        ));
    }

    Ok(PeerIdentity {
        pid: credentials.pid,
        uid: credentials.uid,
        gid: credentials.gid,
        label: label.to_owned(),
    })
}

#[cfg(not(target_os = "linux"))]
pub fn from_fd(_fd: RawFd) -> io::Result<PeerIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer security labels require Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::is_valid_package_label;

    #[test]
    fn accepts_package_labels() {
        assert!(is_valid_package_label("User::Pkg::org.example.app"));
        assert!(is_valid_package_label("User::Pkg::abc_123"));
    }

    #[test]
    fn rejects_non_package_labels() {
        for label in [
            "",
            "System",
            "System::Privileged",
            "User::App::Shared",
            "User::Pkg::",
            "User::Pkg::bad/label",
            "User::Pkg::bad label",
        ] {
            assert!(!is_valid_package_label(label), "{label}");
        }
    }
}
