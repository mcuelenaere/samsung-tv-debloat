//! Small command-backed systemd adapter.

use crate::Error;
use std::io;
use std::process::{Command, Stdio};

pub const SYSTEMCTL: &str = "/usr/bin/systemctl";

/// Runs an argv without a shell and returns its exit code (-1 for a signal).
pub trait Runner {
    fn run(&self, argv: &[String]) -> io::Result<i32>;
}

pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, argv: &[String]) -> io::Result<i32> {
        let (program, args) = argv.split_first().ok_or(io::ErrorKind::InvalidInput)?;
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()?;
        Ok(status.code().unwrap_or(-1))
    }
}

pub struct Systemd<'a>(pub &'a dyn Runner);

impl Systemd<'_> {
    fn systemctl(&self, args: &[&str]) -> Result<i32, Error> {
        let mut argv = vec![SYSTEMCTL.to_owned()];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        self.0
            .run(&argv)
            .map_err(|e| Error::io(format!("systemctl {}", args.join(" ")), e))
    }

    fn checked(&self, args: &[&str]) -> Result<(), Error> {
        match self.systemctl(args)? {
            0 => Ok(()),
            code => Err(Error::failed(format!(
                "systemctl {} exited with {code}",
                args.join(" ")
            ))),
        }
    }

    pub fn is_active(&self, unit: &str) -> Result<bool, Error> {
        Ok(self.systemctl(&["is-active", "--quiet", unit])? == 0)
    }

    pub fn start(&self, unit: &str) -> Result<(), Error> {
        self.checked(&["start", unit])
    }

    pub fn stop(&self, unit: &str) -> Result<(), Error> {
        self.checked(&["stop", unit])
    }

    pub fn daemon_reload(&self) -> Result<(), Error> {
        self.checked(&["daemon-reload"])
    }
}
