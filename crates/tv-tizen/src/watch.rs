//! inotify on a few directories: wait until a file of interest changes and
//! the writes have settled.
use std::ffi::{CStr, CString};
use std::io;
use std::mem::size_of;
use std::ptr;
use std::time::{Duration, Instant};

const MASK: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_TO
    | libc::IN_MOVED_FROM
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MODIFY;

pub struct Watch(libc::c_int);

impl Watch {
    pub fn new(dirs: &[&str]) -> io::Result<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let watch = Self(fd);
        for dir in dirs {
            let path = CString::new(*dir).map_err(|_| io::ErrorKind::InvalidInput)?;
            if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), MASK) } < 0 {
                let error = io::Error::last_os_error();
                return Err(io::Error::new(
                    error.kind(),
                    format!("watch {dir}: {error}"),
                ));
            }
        }
        Ok(watch)
    }

    /// Blocks until a file whose name `wanted` accepts changes, then until
    /// `quiet` has passed without another such change.
    pub fn wait(&self, wanted: impl Fn(&str) -> bool, quiet: Duration) -> io::Result<()> {
        while !self.read(&wanted)? {}
        let mut deadline = Instant::now() + quiet;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut poll = libc::pollfd {
                fd: self.0,
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = left.as_millis().min(i32::MAX as u128) as i32;
            match unsafe { libc::poll(&mut poll, 1, millis) } {
                0 => return Ok(()),
                ready if ready < 0 => {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
                _ => {
                    if self.read(&wanted)? {
                        deadline = Instant::now() + quiet;
                    }
                }
            }
        }
    }

    /// One read of queued events; whether any named a wanted file. A queue
    /// overflow counts as a match.
    fn read(&self, wanted: &impl Fn(&str) -> bool) -> io::Result<bool> {
        let mut buffer = [0_u8; 4096];
        let length = unsafe { libc::read(self.0, buffer.as_mut_ptr().cast(), buffer.len()) };
        if length < 0 {
            let error = io::Error::last_os_error();
            return match error.kind() {
                io::ErrorKind::Interrupted => Ok(false),
                _ => Err(error),
            };
        }
        let length = length as usize;
        let header = size_of::<libc::inotify_event>();
        let (mut offset, mut matched) = (0, false);
        while offset + header <= length {
            // SAFETY: the kernel wrote a whole event header here.
            let event: libc::inotify_event =
                unsafe { ptr::read_unaligned(buffer.as_ptr().add(offset).cast()) };
            let start = offset + header;
            let end = (start + event.len as usize).min(length);
            let name = CStr::from_bytes_until_nul(&buffer[start..end])
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            matched |= event.mask & libc::IN_Q_OVERFLOW != 0 || wanted(&name);
            offset = start + event.len as usize;
        }
        Ok(matched)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}
