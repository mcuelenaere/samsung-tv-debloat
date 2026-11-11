//! A small, blocking SDB client that talks straight to a Samsung TV's sdbd
//! over TCP, without Tizen Studio or a host-side sdb server.
//!
//! Each [`Device`] is one TCP connection carrying one channel at a time.
//! Verbs and paths follow what Samsung's own `sdb` sends (captured against a
//! QE55S95B, sdbd 2.2.31 / plugin 3.8.5); sdbd's `shell:` is a fixed verb
//! vocabulary (`0 appinstall`, `0 execute`, ...), not a real shell.

pub mod connection;
pub mod frame;
pub mod sync;

pub use connection::{Channel, Device, Options, DEFAULT_PORT};

use std::io::{self, Read, Write};
use std::path::Path;

/// Where sdbd's `0 appinstall` looks for staged packages.
pub const STAGING_DIR: &str = "/home/owner/share/tmp/sdk_tools";

/// sdbd splits verb arguments on whitespace without quoting, and some
/// firmware rejects shell metacharacters, so ids and file names stay plain.
fn is_plain(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Remote basename for a package: anything outside `[A-Za-z0-9._-]` becomes `_`.
pub fn staged_name(local: &Path) -> String {
    let name = local
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.chars()
        .map(|c| {
            if is_plain(c.encode_utf8(&mut [0; 4])) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn check_id(id: &str) -> io::Result<()> {
    if is_plain(id) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid package/app id {id:?}"),
        ))
    }
}

/// pkgcmd, behind `0 appinstall`/`0 appuninstall`, ends with this on success.
fn pkgcmd_ok(output: &[u8]) -> bool {
    String::from_utf8_lossy(output).contains("key[end] val[ok]")
}

impl Device {
    /// The raw `key:value` lines from the `capability:` service.
    pub fn capabilities(&mut self) -> io::Result<String> {
        let mut raw = Vec::new();
        self.open("capability:")?.read_to_end(&mut raw)?;
        // sdbd prefixes the text with its length as a little-endian u16.
        if raw.len() >= 2 && u16::from_le_bytes([raw[0], raw[1]]) as usize == raw.len() - 2 {
            raw.drain(..2);
        }
        Ok(String::from_utf8_lossy(&raw)
            .trim_end_matches('\0')
            .to_owned())
    }

    /// Runs a non-interactive `shell:` command, streaming its output to `out`
    /// until the TV closes the channel, and returns a copy of that output.
    /// sdbd reports no exit status.
    pub fn shell(&mut self, command: &str, out: &mut dyn Write) -> io::Result<Vec<u8>> {
        let mut ch = self.open(&format!("shell:{command}"))?;
        let (mut all, mut buf) = (Vec::new(), [0u8; 16 * 1024]);
        loop {
            let n = ch.read(&mut buf)?;
            if n == 0 {
                return Ok(all);
            }
            out.write_all(&buf[..n])?;
            out.flush()?;
            all.extend_from_slice(&buf[..n]);
        }
    }

    pub fn push(&mut self, local: &Path, remote: &str) -> io::Result<String> {
        sync::push(self, local, remote)
    }

    pub fn pull(&mut self, remote: &str, local: &Path) -> io::Result<u64> {
        sync::pull(self, remote, local)
    }

    /// Stages a `.tpk`/`.wgt` and installs it the way Samsung's `sdb install`
    /// does (`0 appinstall <type> <file>`, then `0 rmfile`).
    pub fn install(&mut self, package: &Path, out: &mut dyn Write) -> io::Result<()> {
        let kind = match package
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
        {
            Some(ext) if ext == "tpk" || ext == "wgt" => ext,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "package must be a .tpk or .wgt",
                ))
            }
        };
        let name = staged_name(package);
        let remote = self.push(package, &format!("{STAGING_DIR}/{name}"))?;
        let result = self.shell(&format!("0 appinstall {kind} {name}"), out);
        let _ = self.shell(&format!("0 rmfile \"{remote}\""), &mut io::sink());
        if pkgcmd_ok(&result?) {
            Ok(())
        } else {
            Err(io::Error::other("installation failed"))
        }
    }

    pub fn uninstall(&mut self, package_id: &str, out: &mut dyn Write) -> io::Result<()> {
        check_id(package_id)?;
        if pkgcmd_ok(&self.shell(&format!("0 appuninstall {package_id}"), out)?) {
            Ok(())
        } else {
            Err(io::Error::other("uninstall failed"))
        }
    }

    /// Starts an app through `app_launcher` (`0 execute`). Smart Hub web apps
    /// may need `shell 0 was_execute <app-id>` instead.
    pub fn launch(&mut self, app_id: &str, out: &mut dyn Write) -> io::Result<()> {
        check_id(app_id)?;
        let output = self.shell(&format!("0 execute {app_id}"), out)?;
        if String::from_utf8_lossy(&output).contains("successfully launched") {
            Ok(())
        } else {
            Err(io::Error::other("launch failed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_names_and_ids_stay_plain() {
        assert_eq!(
            staged_name(Path::new("/x/my app (1).tpk")),
            "my_app__1_.tpk"
        );
        assert_eq!(
            staged_name(Path::new("org.tvauth.ui-0.1.0.tpk")),
            "org.tvauth.ui-0.1.0.tpk"
        );
        assert!(check_id("org.tvauth.ui").is_ok());
        assert!(check_id("a b").is_err());
        assert!(check_id("a;reboot").is_err());
        assert!(check_id("").is_err());
    }
}
