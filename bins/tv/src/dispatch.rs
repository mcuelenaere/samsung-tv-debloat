//! Built-in command routing and external `tv-<name>` discovery.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The platform-specific executable suffix (`.exe` on Windows).
pub const EXE_SUFFIX: &str = std::env::consts::EXE_SUFFIX;

pub fn helper_name(subcommand: &str) -> String {
    format!("tv-{subcommand}{EXE_SUFFIX}")
}

/// Finds a helper beside `exe_dir`, then on `PATH`. `PATH` search can be
/// disabled with `TV_NO_PATH_HELPERS=1` for hermetic operation.
fn find_helper(name: &str, exe_dir: &Path) -> Option<PathBuf> {
    let beside = exe_dir.join(name);
    if beside.is_file() {
        return Some(beside);
    }
    if std::env::var_os("TV_NO_PATH_HELPERS").is_some() {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|entry| entry.join(name))
        .find(|candidate| candidate.is_file())
}

pub fn run(args: Vec<OsString>) -> io::Result<u8> {
    let Some(first) = args.first() else {
        eprintln!("{}", crate::USAGE);
        return Ok(2);
    };
    let Some(command) = first.to_str() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command name is not valid UTF-8",
        ));
    };
    let rest = &args[1..];
    match command {
        "sdb" => crate::sdb::run(rest),
        "ssh-key" => crate::ssh_key::run(rest),
        "package" => run_external("tv-pkg", rest),
        "root" | "setup" => run_external(&helper_name(command), rest),
        "-h" | "--help" | "help" => {
            println!("{}", crate::USAGE);
            Ok(0)
        }
        other if other.starts_with('-') => {
            eprintln!("{}\n\ntv: unknown option {other:?}", crate::USAGE);
            Ok(2)
        }
        other => run_external(&helper_name(other), rest),
    }
}

fn run_external(name: &str, args: &[OsString]) -> io::Result<u8> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    let helper = find_helper(name, dir).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "unknown command {:?}: helper {name} was not found beside {} or on PATH",
                name.trim_start_matches("tv-").trim_end_matches(EXE_SUFFIX),
                exe.display()
            ),
        )
    })?;
    let status = Command::new(&helper)
        .args(args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;
    Ok(exit_code(status))
}

fn exit_code(status: std::process::ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code.clamp(0, 255) as u8;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return (128 + signal).clamp(0, 255) as u8;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_names_are_prefixed() {
        assert_eq!(helper_name("root"), format!("tv-root{EXE_SUFFIX}"));
        assert_eq!(helper_name("setup"), format!("tv-setup{EXE_SUFFIX}"));
    }
}
