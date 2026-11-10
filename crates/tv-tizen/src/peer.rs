//! Who is on the other end of a Unix socket.
// ponytail: tv-authd has its own rustix-based copy; it is a static musl
// binary and does not link tv-tizen.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// uid and SMACK label of the peer (`SO_PEERCRED`, `SO_PEERSEC`).
pub fn credentials(stream: &UnixStream) -> io::Result<(u32, String)> {
    let fd = stream.as_raw_fd();
    let mut credentials = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut label = [0_u8; 256];
    let mut length = label.len() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERSEC,
            label.as_mut_ptr().cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let label = &label[..(length as usize).min(label.len())];
    let label = String::from_utf8_lossy(label)
        .trim_end_matches('\0')
        .to_owned();
    Ok((credentials.uid, label))
}
