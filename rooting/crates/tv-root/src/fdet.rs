//! Managed FDET payload staging and one-shot root command execution.
//!
//! The payload remains the managed .NET assembly from the reference project.
//! Only `FdetProbe.dll` and its runtime configuration are staged for a one-shot
//! command; the unrelated agent and display-control assemblies stay out.

use crate::error::{Error, Result};
use crate::inject::{self, Capture, CaptureConfig};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tv_sdb::{Device, Options};

pub const SCAN_START: &str = "0x20000000";
pub const SCAN_LENGTH: &str = "0x4a000000";
pub const FDET_DLL: &str = "FdetProbe.dll";
pub const FDET_RUNTIME_CONFIG: &str = "FdetProbe.runtimeconfig.json";
pub const ACTIVE_RUNTIME_PROBE: &str = "readlink -f /usr/share/dotnet.tizen/netcoreapp";
pub const REQUIRED_EVIDENCE: &[&str] = &[
    "uid_after=0",
    "euid_after=0",
    "gid_after=0",
    "egid_after=0",
    "exec_begin",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Net6,
    Core31,
}

impl Runtime {
    pub fn as_str(self) -> &'static str {
        match self {
            Runtime::Net6 => "net6",
            Runtime::Core31 => "core31",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PayloadSet {
    pub modern: PathBuf,
    pub legacy: Option<PathBuf>,
}

impl PayloadSet {
    pub fn new(modern: impl Into<PathBuf>, legacy: Option<PathBuf>) -> Self {
        Self {
            modern: modern.into(),
            legacy,
        }
    }

    pub fn directory(&self, runtime: Runtime) -> Result<&Path> {
        match runtime {
            Runtime::Net6 => Ok(&self.modern),
            Runtime::Core31 => self
                .legacy
                .as_deref()
                .ok_or_else(|| Error::payload("no .NET Core 3.1 payload directory")),
        }
    }

    pub fn files(&self, runtime: Runtime) -> Result<(PathBuf, PathBuf)> {
        let directory = self.directory(runtime)?;
        let dll = directory.join(FDET_DLL);
        let config = directory.join(FDET_RUNTIME_CONFIG);
        for path in [&dll, &config] {
            if !path.is_file() {
                return Err(Error::payload(format!(
                    "missing managed FDET payload {}; build the rooting payload first",
                    path.display()
                )));
            }
        }
        Ok((dll, config))
    }
}

/// Selects the legacy payload only after positively identifying a 3.1 CLR.
pub fn runtime_from_output(output: &str) -> Runtime {
    let value = output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or_default();
    let name = Path::new(value)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.strip_prefix("3.1.") {
        Some(patch) if !patch.is_empty() && patch.bytes().all(|byte| byte.is_ascii_digit()) => {
            Runtime::Core31
        }
        _ => Runtime::Net6,
    }
}

pub fn select_runtime(target: &str, options: &Options) -> Result<Runtime> {
    let capture = inject::capture(
        target,
        options,
        ACTIVE_RUNTIME_PROBE,
        &CaptureConfig::default(),
    )?;
    let (body, _) = inject::validate_exit(&capture, &capture.marker_token)?;
    Ok(runtime_from_output(&body))
}

/// Creates the staging directory and pushes exactly the files supplied.
pub fn stage_files(
    device: &mut Device,
    remote_dir: &str,
    files: &[(PathBuf, String)],
) -> Result<()> {
    inject::inject(
        device,
        &format!("/bin/mkdir -p {}", inject::shell_quote(remote_dir)),
    )?;
    for (local, name) in files {
        if name.contains('/') || name == "." || name == ".." {
            return Err(Error::protocol(format!(
                "invalid staged file name {name:?}"
            )));
        }
        device
            .push(local, &format!("{remote_dir}/{name}"))
            .map_err(Error::sdb)?;
    }
    Ok(())
}

pub fn stage_payload(
    device: &mut Device,
    remote_dir: &str,
    payloads: &PayloadSet,
    runtime: Runtime,
) -> Result<()> {
    let (dll, config) = payloads.files(runtime)?;
    stage_files(
        device,
        remote_dir,
        &[
            (dll, FDET_DLL.to_owned()),
            (config, FDET_RUNTIME_CONFIG.to_owned()),
        ],
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootEvidence {
    pub output: String,
}

impl RootEvidence {
    pub fn validate(output: &str) -> Result<Self> {
        let lines: Vec<&str> = output.lines().map(str::trim).collect();
        let missing: Vec<&str> = REQUIRED_EVIDENCE
            .iter()
            .copied()
            .filter(|marker| !lines.contains(marker))
            .collect();
        if !missing.is_empty() {
            return Err(Error::protocol(format!(
                "FDET did not establish a complete root identity; missing {}",
                missing.join(", ")
            )));
        }
        Ok(RootEvidence {
            output: output.to_owned(),
        })
    }
}

/// Runs one command through the managed FDET payload and requires both root
/// evidence and exit status zero.
pub fn invocation(remote_probe: &str, command: &str) -> Result<String> {
    let tag = format!("qnr{}", inject::random_hex(4)?);
    Ok([
        "/usr/bin/dotnet".to_owned(),
        remote_probe.to_owned(),
        "srs".to_owned(),
        SCAN_START.to_owned(),
        SCAN_LENGTH.to_owned(),
        tag,
        command.to_owned(),
    ]
    .iter()
    .map(|argument| inject::shell_quote(argument))
    .collect::<Vec<_>>()
    .join(" "))
}

pub fn execute_once(
    target: &str,
    options: &Options,
    remote_probe: &str,
    command: &str,
    timeout: Duration,
) -> Result<(Capture, RootEvidence)> {
    let invocation = invocation(remote_probe, command)?;
    let capture = inject::capture(
        target,
        options,
        &invocation,
        &CaptureConfig {
            timeout,
            ..CaptureConfig::default()
        },
    )?;
    let (body, status) = inject::validate_exit(&capture, &capture.marker_token)?;
    if status != 0 {
        return Err(Error::protocol(format!("remote command exited {status}")));
    }
    let evidence = RootEvidence::validate(&body)?;
    Ok((capture, evidence))
}

pub fn cleanup(device: &mut Device, remote_dir: &str) -> Result<()> {
    inject::inject(
        device,
        &format!("/bin/rm -rf -- {}", inject::shell_quote(remote_dir)),
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_selection_requires_a_positive_3_1_match() {
        assert_eq!(
            runtime_from_output("/usr/share/dotnet.tizen/netcoreapp/3.1.3\n"),
            Runtime::Core31
        );
        assert_eq!(
            runtime_from_output("/usr/share/dotnet.tizen/netcoreapp/6.0.0\n"),
            Runtime::Net6
        );
        assert_eq!(runtime_from_output("3.1.x\n"), Runtime::Net6);
        assert_eq!(runtime_from_output(""), Runtime::Net6);
    }

    #[test]
    fn root_evidence_requires_every_identity_marker() {
        let output = "\
uid_before=901
uid_after=0
euid_after=0
gid_after=0
egid_after=0
exec_command=id
exec_begin
uid=0(root)
";
        assert!(RootEvidence::validate(output).is_ok());
        assert!(RootEvidence::validate(&output.replace("gid_after=0", "gid_after=901")).is_err());
    }

    #[test]
    fn payload_set_only_needs_the_selected_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FDET_DLL), b"dll").unwrap();
        std::fs::write(dir.path().join(FDET_RUNTIME_CONFIG), b"{}").unwrap();
        let payloads = PayloadSet::new(dir.path(), None);
        assert!(payloads.files(Runtime::Net6).is_ok());
        assert!(payloads.files(Runtime::Core31).is_err());
    }
}
