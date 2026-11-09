use std::io;
use std::os::fd::AsFd;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub pid: libc::pid_t,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub label: String,
}

/// SMACK label of privileged system processes (root shells, init, services).
pub const ROOT_LABEL: &str = "System::Privileged";

/// The local administrator escape hatch: uid 0 with the system label. Such
/// clients may use `pkg.*` without a prompt (never `exec`).
pub fn is_root_identity(identity: &PeerIdentity) -> bool {
    identity.uid == 0 && identity.label == ROOT_LABEL
}

pub use tv_proto::{is_valid_package_id, is_valid_package_label, package_from_label};

#[cfg(target_os = "linux")]
pub fn from_fd(socket: impl AsFd) -> io::Result<PeerIdentity> {
    use std::os::fd::AsRawFd;

    let credentials = rustix::net::sockopt::socket_peercred(&socket)?;

    // SO_PEERSEC (the SMACK label) has no rustix wrapper.
    let mut buffer = [0_u8; 4096];
    let mut length = buffer.len() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            socket.as_fd().as_raw_fd(),
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
        pid: credentials.pid.as_raw_nonzero().get(),
        uid: credentials.uid.as_raw(),
        gid: credentials.gid.as_raw(),
        label: label.to_owned(),
    })
}

#[cfg(not(target_os = "linux"))]
pub fn from_fd(_socket: impl AsFd) -> io::Result<PeerIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer security labels require Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        is_root_identity, is_valid_package_id, is_valid_package_label, package_from_label,
    };

    fn identity(uid: u32, label: &str) -> super::PeerIdentity {
        super::PeerIdentity {
            pid: 1,
            uid,
            gid: 0,
            label: label.into(),
        }
    }

    #[test]
    fn only_uid_zero_with_the_system_label_is_root() {
        assert!(is_root_identity(&identity(0, "System::Privileged")));
        assert!(!is_root_identity(&identity(0, "User::Pkg::org.example")));
        assert!(!is_root_identity(&identity(5001, "System::Privileged")));
        assert!(!is_root_identity(&identity(0, "System")));
    }

    #[test]
    fn accepts_package_labels() {
        assert!(is_valid_package_label("User::Pkg::org.example.app"));
        assert!(is_valid_package_label("User::Pkg::abc_123"));
        assert!(is_valid_package_id("org.example.app"));
        assert_eq!(
            package_from_label("User::Pkg::org.example.app"),
            Some("org.example.app")
        );
        assert_eq!(package_from_label("System::Privileged"), None);
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
