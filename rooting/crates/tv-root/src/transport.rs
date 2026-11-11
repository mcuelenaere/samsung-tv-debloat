//! A small transport boundary so setup orchestration is host-testable without
//! a TV or a real SDB connection.

use crate::error::{Error, Result};
use crate::inject::{self, Capture, CaptureConfig};
use std::path::Path;
use std::time::Duration;
use tv_sdb::{Device, Options};

pub trait Transport {
    fn target(&self) -> &str;
    fn require_shell_injection(&mut self) -> Result<()>;
    fn capture(&mut self, command: &str, timeout: Duration) -> Result<Capture>;
    fn push(&mut self, local: &Path, remote: &str) -> Result<()>;
    fn inject(&mut self, command: &str) -> Result<()>;
}

pub struct SdbTransport {
    target: String,
    options: Options,
    device: Option<Device>,
}

impl SdbTransport {
    pub fn new(target: impl Into<String>, options: Options) -> Self {
        Self {
            target: target.into(),
            options,
            device: None,
        }
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    fn device(&mut self) -> Result<&mut Device> {
        if self.device.is_none() {
            self.device = Some(Device::connect(&self.target, &self.options).map_err(Error::sdb)?);
        }
        Ok(self.device.as_mut().expect("connected above"))
    }
}

impl Transport for SdbTransport {
    fn target(&self) -> &str {
        &self.target
    }

    fn require_shell_injection(&mut self) -> Result<()> {
        inject::require_shell_injection(&self.target, &self.options)
    }

    fn capture(&mut self, command: &str, timeout: Duration) -> Result<Capture> {
        inject::capture(
            &self.target,
            &self.options,
            command,
            &CaptureConfig {
                timeout,
                ..CaptureConfig::default()
            },
        )
    }

    fn push(&mut self, local: &Path, remote: &str) -> Result<()> {
        self.device()?
            .push(local, remote)
            .map(|_| ())
            .map_err(Error::sdb)
    }

    fn inject(&mut self, command: &str) -> Result<()> {
        inject::inject(self.device()?, command).map(|_| ())
    }
}

/// A test transport backed by a queue of captured outputs and a push log.
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
pub struct FakeTransport {
    pub target: String,
    pub captures: std::collections::VecDeque<Result<String>>,
    pub pushed: Vec<(PathBuf, String)>,
    pub injected: Vec<String>,
}

#[cfg(test)]
impl FakeTransport {
    pub fn new(outputs: impl IntoIterator<Item = String>) -> Self {
        Self {
            target: "192.0.2.10".into(),
            captures: outputs.into_iter().map(Ok).collect(),
            pushed: Vec::new(),
            injected: Vec::new(),
        }
    }
}

#[cfg(test)]
impl Transport for FakeTransport {
    fn target(&self) -> &str {
        &self.target
    }

    fn require_shell_injection(&mut self) -> Result<()> {
        Ok(())
    }

    fn capture(&mut self, _command: &str, _timeout: Duration) -> Result<Capture> {
        let output = self
            .captures
            .pop_front()
            .transpose()?
            .ok_or_else(|| Error::protocol("unexpected capture"))?;
        Ok(Capture {
            output,
            marker_token: "test-token".into(),
        })
    }

    fn push(&mut self, local: &Path, remote: &str) -> Result<()> {
        self.pushed.push((local.to_path_buf(), remote.to_owned()));
        Ok(())
    }

    fn inject(&mut self, command: &str) -> Result<()> {
        self.injected.push(command.to_owned());
        Ok(())
    }
}
