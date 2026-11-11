//! `tv ssh-key add`: validate a public key on the host and drop it for the
//! TV's own approval prompt.
//!
//! The host never edits `authorized_keys`. It pushes the exact file to the
//! world-writable drop path watched by `tv-authd`, which parses it, shows the
//! fingerprint and merges it only after the user allows it on the TV.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tv_sdb::{Device, Options};

/// The path `tv-authd` watches for a pushed key.
pub const KEY_DROP: &str = "/home/owner/share/tmp/sdk_tools/ssh-key.pub";
/// `tv-authd` refuses larger drop files.
pub const MAX_KEY_DROP_BYTES: usize = 16 * 1024;

const USAGE: &str = "\
usage: tv ssh-key add <tv>[:port] <public-key-file>

The key is validated locally, pushed to the TV and shown in an approval prompt.
Nothing is authorized until the prompt is allowed on the TV. Use `-` to read
from standard input.";

pub fn run(args: &[OsString]) -> io::Result<u8> {
    let args: Vec<String> = args
        .iter()
        .map(|arg| {
            arg.to_str().map(str::to_owned).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "argument is not valid UTF-8")
            })
        })
        .collect::<io::Result<_>>()?;
    match args.as_slice() {
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            Ok(0)
        }
        [subcommand, tv, file] if subcommand == "add" => add(tv, file),
        [] | [_] => usage("missing `ssh-key add` arguments"),
        [subcommand, ..] if subcommand == "add" => usage("usage: tv ssh-key add <tv> <file>"),
        _ => usage("unknown ssh-key command"),
    }
}

fn add(tv: &str, file: &str) -> io::Result<u8> {
    let (bytes, temporary) = if file == "-" {
        let mut bytes = Vec::new();
        io::stdin()
            .take(MAX_KEY_DROP_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        let path = temporary_path();
        std::fs::write(&path, &bytes)?;
        (bytes, Some(path))
    } else {
        (std::fs::read(file)?, None)
    };
    let result = add_bytes(tv, &bytes, temporary.as_deref().unwrap_or(Path::new(file)));
    if let Some(path) = temporary {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn add_bytes(tv: &str, bytes: &[u8], local: &Path) -> io::Result<u8> {
    if bytes.len() > MAX_KEY_DROP_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("public-key file is larger than {MAX_KEY_DROP_BYTES} bytes"),
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "public-key file is not text"))?;
    let mut keys = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let key = tv_ssh_key::parse_line(trimmed).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("line {} is not a valid OpenSSH public key", number + 1),
            )
        })?;
        keys.push(key);
    }
    if keys.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no public key found",
        ));
    }

    let options = Options {
        timeout: Duration::from_secs(30),
        verbose: false,
    };
    let mut device = Device::connect(tv, &options)?;
    device.push(local, KEY_DROP)?;

    for key in &keys {
        println!("{} {}", key.fingerprint, key.comment);
    }
    println!("pushed to {KEY_DROP}; allow it on the TV to authorize the key");
    io::stdout().flush()?;
    Ok(0)
}

fn temporary_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("tv-ssh-key-{}-{stamp}.pub", std::process::id()))
}

fn usage(error: &str) -> io::Result<u8> {
    eprintln!("tv ssh-key: {error}\n\n{USAGE}");
    Ok(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDydMB0O7XxHHa64MbIoqwtYBeMEDzkWzaOivH+ATzBI test@one";

    #[test]
    fn accepts_one_or_more_valid_keys() {
        assert_eq!(
            tv_ssh_key::parse_line(ED).unwrap().fingerprint,
            "SHA256:0qlx9o3R5rGeEgOu0wD439QAdcF3PiJEw2Za4Ka5SDQ"
        );
        assert!(tv_ssh_key::parse_line("ssh-rsa not-a-key").is_none());
    }

    #[test]
    fn temporary_names_are_distinct() {
        assert_ne!(temporary_path(), temporary_path());
    }
}
