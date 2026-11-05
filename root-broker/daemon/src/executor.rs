use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct ExecutionOutcome {
    pub exit_code: i32,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

pub fn execute(command: &str, timeout_ms: u64) -> io::Result<ExecutionOutcome> {
    let setsid = if Path::new("/usr/bin/setsid").exists() {
        "/usr/bin/setsid"
    } else {
        "/bin/setsid"
    };
    let mut child = Command::new(setsid)
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "missing stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "missing stderr"))?;
    let stdout_fd = stdout.as_raw_fd();
    let stderr_fd = stderr.as_raw_fd();
    set_nonblocking(stdout_fd)?;
    set_nonblocking(stderr_fd)?;

    let mut output = Vec::new();
    let mut error_output = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let mut truncated = false;
    let mut status: Option<ExitStatus> = None;
    let mut timed_out = false;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut drain_deadline: Option<Instant> = None;

    loop {
        if status.is_none() {
            if let Some(exit_status) = child.try_wait()? {
                status = Some(exit_status);
                drain_deadline = Some(Instant::now() + Duration::from_millis(100));
            }
        }

        if !timed_out && status.is_none() && Instant::now() >= deadline {
            timed_out = true;
            kill_process_group(&child);
            let kill_deadline = Instant::now() + Duration::from_secs(2);
            while status.is_none() && Instant::now() < kill_deadline {
                status = child.try_wait()?;
                if status.is_none() {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            if status.is_none() {
                let _ = child.kill();
                status = child.try_wait()?;
            }
            drain_deadline = Some(Instant::now() + Duration::from_millis(100));
        }

        if (status.is_some() || timed_out)
            && ((stdout_closed && stderr_closed)
                || drain_deadline.is_some_and(|deadline| Instant::now() >= deadline))
        {
            break;
        }

        let poll_timeout = if status.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            remaining.as_millis().clamp(1, 50) as i32
        } else {
            10
        };
        let mut fds = [
            pollfd(stdout_fd, stdout_closed),
            pollfd(stderr_fd, stderr_closed),
        ];
        let result =
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, poll_timeout) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result > 0 {
            if !stdout_closed && fds[0].revents != 0 {
                drain_fd(stdout_fd, &mut output, &mut truncated, &mut stdout_closed)?;
            }
            if !stderr_closed && fds[1].revents != 0 {
                drain_fd(
                    stderr_fd,
                    &mut error_output,
                    &mut truncated,
                    &mut stderr_closed,
                )?;
            }
        }
    }

    drop(stdout);
    drop(stderr);

    let exit_code = if timed_out {
        124
    } else {
        status
            .map(|value| {
                value
                    .code()
                    .unwrap_or_else(|| 128 + value.signal().unwrap_or(0))
            })
            .unwrap_or(124)
    };

    Ok(ExecutionOutcome {
        exit_code,
        timed_out,
        stdout: output,
        stderr: error_output,
        truncated,
    })
}

fn pollfd(fd: RawFd, closed: bool) -> libc::pollfd {
    libc::pollfd {
        fd: if closed { -1 } else { fd },
        events: if closed { 0 } else { libc::POLLIN },
        revents: 0,
    }
}

fn drain_fd(
    fd: RawFd,
    output: &mut Vec<u8>,
    truncated: &mut bool,
    closed: &mut bool,
) -> io::Result<()> {
    let mut buffer = [0_u8; 8192];
    // Bound one drain pass so a continuously writing process cannot starve
    // timeout and child-status checks in the caller.
    for _ in 0..64 {
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count > 0 {
            let count = count as usize;
            let remaining = MAX_OUTPUT_BYTES.saturating_sub(output.len());
            let keep = remaining.min(count);
            output.extend_from_slice(&buffer[..keep]);
            if keep < count {
                *truncated = true;
            }
        } else if count == 0 {
            *closed = true;
            return Ok(());
        } else {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) => return Ok(()),
                Some(libc::EINTR) => continue,
                _ => {
                    *closed = true;
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn kill_process_group(child: &Child) {
    let process_group = child.id() as libc::pid_t;
    unsafe {
        libc::kill(-process_group, libc::SIGKILL);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::execute;

    #[test]
    fn captures_stdout_stderr_and_exit_status() {
        let outcome = execute("printf hello; printf error >&2; exit 7", 2_000).unwrap();
        assert_eq!(outcome.stdout, b"hello");
        assert_eq!(outcome.stderr, b"error");
        assert_eq!(outcome.exit_code, 7);
        assert!(!outcome.timed_out);
        assert!(!outcome.truncated);
    }

    #[test]
    fn timeout_kills_the_process_group() {
        let started = std::time::Instant::now();
        let outcome = execute("sleep 30", 100).unwrap();
        assert!(outcome.timed_out);
        assert_eq!(outcome.exit_code, 124);
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
    }

    #[test]
    fn output_is_bounded() {
        let outcome = execute("head -c 1100000 /dev/zero", 5_000).unwrap();
        assert!(outcome.stdout.len() <= super::MAX_OUTPUT_BYTES);
        assert!(outcome.truncated);
    }
}
