//! `authorized_keys` handling for the System tab's SSH section: parsing,
//! OpenSSH-style fingerprints, adding and removing keys.

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use sha2::{Digest, Sha256};

/// One public key line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// `ssh-ed25519`, `ecdsa-sha2-nistp256`, ...
    pub kind: String,
    /// The base64 key blob, as written.
    pub blob: String,
    pub comment: String,
    /// `SHA256:...`, as `ssh-keygen -l` prints it.
    pub fingerprint: String,
}

impl Key {
    pub fn line(&self) -> String {
        if self.comment.is_empty() {
            format!("{} {}", self.kind, self.blob)
        } else {
            format!("{} {} {}", self.kind, self.blob, self.comment)
        }
    }
}

/// Parses one key line: optional options, then type, blob and comment. The
/// blob must decode and name the same type.
pub fn parse_line(line: &str) -> Option<Key> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let fields: Vec<&str> = line.split_whitespace().collect();
    let at = fields.iter().position(|field| is_kind(field))?;
    let (kind, blob) = (fields[at], *fields.get(at + 1)?);
    let bytes = STANDARD.decode(blob).ok()?;
    let length = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    if bytes.get(4..4 + length)? != kind.as_bytes() {
        return None;
    }
    Some(Key {
        kind: kind.to_owned(),
        blob: blob.to_owned(),
        comment: fields[at + 2..].join(" "),
        fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&bytes))),
    })
}

fn is_kind(field: &str) -> bool {
    field.starts_with("ssh-") || field.starts_with("ecdsa-sha2-") || field.starts_with("sk-")
}

pub fn parse(text: &str) -> Vec<Key> {
    text.lines().filter_map(parse_line).collect()
}

/// `existing` with the keys of `new` it lacks appended; other lines kept.
pub fn add(existing: &str, new: &[Key]) -> String {
    let mut have: Vec<String> = parse(existing).into_iter().map(|key| key.blob).collect();
    let mut out = existing.trim_end().to_owned();
    for key in new {
        if !have.contains(&key.blob) {
            have.push(key.blob.clone());
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&key.line());
        }
    }
    out.push('\n');
    out
}

/// `existing` without the key with `fingerprint`, or `None` if it has none.
pub fn remove(existing: &str, fingerprint: &str) -> Option<String> {
    let mut found = false;
    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| {
            let hit = parse_line(line).is_some_and(|key| key.fingerprint == fingerprint);
            found |= hit;
            !hit
        })
        .collect();
    found.then(|| kept.iter().map(|line| format!("{line}\n")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDydMB0O7XxHHa64MbIoqwtYBeMEDzkWzaOivH+ATzBI test@one";
    const EC: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBDOay4Ymby1zfmsO0GSNWSrJpgYKSv0f2MfTso6bXLwnhpC3m5fESfZFbBG1f2ttlUHIaal9dVgnuv0LQO5A6Cc= test two";

    #[test]
    fn fingerprints_add_and_remove() {
        // Fingerprints as `ssh-keygen -lf` printed them.
        let ed = parse_line(ED).unwrap();
        assert_eq!(
            ed.fingerprint,
            "SHA256:0qlx9o3R5rGeEgOu0wD439QAdcF3PiJEw2Za4Ka5SDQ"
        );
        assert_eq!(ed.comment, "test@one");
        let ec = parse_line(&format!("no-pty {EC}")).unwrap();
        assert_eq!(
            ec.fingerprint,
            "SHA256:nl8DwsDS+Y/wYbicsWgRrAZEl8OKC6zAwtgBZuU9NoI"
        );
        // A blob that does not name its type, junk, comments: refused.
        assert!(parse_line(
            "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIDydMB0O7XxHHa64MbIoqwtYBeMEDzkWzaOivH+ATzBI"
        )
        .is_none());
        assert!(parse_line("ssh-ed25519 not-base64!").is_none());
        assert!(parse_line("# ssh-ed25519 x").is_none());

        let file = add("# mine\n", &[ed.clone(), ec.clone(), ed.clone()]);
        assert_eq!(file, format!("# mine\n{ED}\n{EC}\n"));
        assert_eq!(add(&file, &[ed.clone()]), file, "no duplicates");
        assert_eq!(
            remove(&file, &ed.fingerprint).unwrap(),
            format!("# mine\n{EC}\n")
        );
        assert!(remove(&file, "SHA256:nope").is_none());
    }
}
