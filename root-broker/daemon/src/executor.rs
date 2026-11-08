use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::io::Errno;
use rustix::process::{kill_process_group, Pid, Signal};
use std::ffi::OsStr;
use std::io;
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsFd, BorrowedFd};
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

/// Run a shell command in its own session with a cleared environment.
pub fn execute(command: &str, timeout_ms: u64) -> io::Result<ExecutionOutcome> {
    let mut child = isolated_command();
    child.arg("/bin/sh").arg("-c").arg(command);
    run(child, timeout_ms)
}

/// Run an absolute program with arguments, without a shell.
pub fn execute_argv(
    program: &Path,
    args: &[&OsStr],
    timeout_ms: u64,
) -> io::Result<ExecutionOutcome> {
    let mut child = isolated_command();
    child.arg(program).args(args);
    run(child, timeout_ms)
}

/// Like [`execute_argv`], but runs the program in `current_dir`. `xmlsec1`
/// resolves relative file references against the working directory.
pub fn execute_argv_in(
    program: &Path,
    args: &[&OsStr],
    current_dir: &Path,
    timeout_ms: u64,
) -> io::Result<ExecutionOutcome> {
    let mut child = isolated_command();
    child.current_dir(current_dir);
    child.arg(program).args(args);
    run(child, timeout_ms)
}

fn isolated_command() -> Command {
    let setsid = if Path::new("/usr/bin/setsid").exists() {
        "/usr/bin/setsid"
    } else {
        "/bin/setsid"
    };
    let mut command = Command::new(setsid);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run(mut command: Command, timeout_ms: u64) -> io::Result<ExecutionOutcome> {
    let mut child = ChildGuard(Some(command.spawn()?));

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "missing stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "missing stderr"))?;
    let stdout_fd = stdout.as_fd();
    let stderr_fd = stderr.as_fd();
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
            kill_group(&child);
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
            remaining.as_millis().clamp(1, 50) as i64
        } else {
            10
        };
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: poll_timeout * 1_000_000,
        };
        // Only poll pipes that are still open: an EOF'd pipe reports POLLHUP
        // forever and would turn this into a busy loop.
        let mut fds = Vec::with_capacity(2);
        if !stdout_closed {
            fds.push(PollFd::new(&stdout_fd, PollFlags::IN));
        }
        if !stderr_closed {
            fds.push(PollFd::new(&stderr_fd, PollFlags::IN));
        }
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
        let mut ready = fds.iter().map(|fd| !fd.revents().is_empty());
        let stdout_ready = !stdout_closed && ready.next().unwrap_or(false);
        let stderr_ready = !stderr_closed && ready.next().unwrap_or(false);
        drop(fds);
        if stdout_ready {
            drain_fd(stdout_fd, &mut output, &mut truncated, &mut stdout_closed);
        }
        if stderr_ready {
            drain_fd(
                stderr_fd,
                &mut error_output,
                &mut truncated,
                &mut stderr_closed,
            );
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

fn drain_fd(fd: BorrowedFd<'_>, output: &mut Vec<u8>, truncated: &mut bool, closed: &mut bool) {
    let mut buffer = [0_u8; 8192];
    // Bound one drain pass so a continuously writing process cannot starve
    // timeout and child-status checks in the caller.
    for _ in 0..64 {
        match rustix::io::read(fd, &mut buffer) {
            Ok(0) => {
                *closed = true;
                return;
            }
            Ok(count) => {
                let remaining = MAX_OUTPUT_BYTES.saturating_sub(output.len());
                let keep = remaining.min(count);
                output.extend_from_slice(&buffer[..keep]);
                if keep < count {
                    *truncated = true;
                }
            }
            Err(Errno::AGAIN) => return,
            Err(Errno::INTR) => continue,
            Err(_) => {
                *closed = true;
                return;
            }
        }
    }
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    let flags = fcntl_getfl(fd)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK)?;
    Ok(())
}

/// Guarantees the child is reaped on every exit path (including `?` errors).
/// If it is still unreaped on drop (e.g. D-state), kill it and hand it to a
/// detached thread that blocks in `wait()`, so the request never blocks on it.
struct ChildGuard(Option<Child>);

impl Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        self.0.as_ref().expect("child present until drop")
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child present until drop")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        kill_group(&child);
        let _ = child.kill();
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        // If the thread cannot be spawned the closure (and child) is dropped;
        // nothing more can be done.
        let _ = thread::Builder::new()
            .name("child-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
    }
}

/// SIGKILL the child's process group (it is the session leader via setsid).
fn kill_group(child: &Child) {
    if let Some(pid) = Pid::from_raw(child.id() as i32) {
        let _ = kill_process_group(pid, Signal::KILL);
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

    #[test]
    fn stdout_and_stderr_are_independently_bounded() {
        let outcome = execute(
            "head -c 1100000 /dev/zero; head -c 1100000 /dev/zero >&2",
            5_000,
        )
        .unwrap();
        assert_eq!(outcome.stdout.len(), super::MAX_OUTPUT_BYTES);
        assert_eq!(outcome.stderr.len(), super::MAX_OUTPUT_BYTES);
        assert!(outcome.truncated);
    }
}
