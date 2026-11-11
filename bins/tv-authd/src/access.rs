//! Remote access to the TV, kept by the broker so every change is in the
//! audit log: SSH (Dropbear) on or off and its authorized keys, and SDB on or
//! off with the one host Samsung's developer mode lets connect.
//!
//! A key is added by pushing it with `tv-sdb push` to `key_drop`. That folder
//! is world-writable, so the file is opened without following symlinks and
//! nothing is added until the user allows it on a `kind = "ssh_key"` prompt.
//! The keys added are the ones parsed before the prompt (what the user saw),
//! and the file is deleted whatever the answer.

use crate::approval::{ApprovalHandle, ApprovalRequest, ApprovalResult};
use crate::logger::Logger;
use crate::protocol::{display_safe, AccessView, PromptKind, SshKeyView};
use crate::ssh_keys::{self, Key};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::net::Ipv4Addr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SSH_UNITS: &[&str] = &["dropbear.service"];
/// The TCP socket starts only while Samsung's developer mode is on.
const SDB_UNITS: &[&str] = &["sdbd_tcp.socket", "sdbd.socket", "sdbd.service"];
const SDB_HOST_KEY: &str = "db/sdk/develop/ip";
const MAX_KEY_DROP: u64 = 16 * 1024;
/// The requester shown on a pushed key's prompt.
const PUSH_LABEL: &str = "sdb-push";
/// How often a pushed key is looked for, and the off switches re-applied
/// (the boot hook starts Dropbear after tv-authd).
const POLL: Duration = Duration::from_secs(2);
const ENFORCE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct AccessPaths {
    pub state: PathBuf,
    pub authorized_keys: PathBuf,
    pub key_drop: PathBuf,
}

impl Default for AccessPaths {
    fn default() -> Self {
        Self {
            state: "/opt/tv-auth/access.json".into(),
            authorized_keys: "/opt/tv-sshd/authorized_keys".into(),
            key_drop: "/home/owner/share/tmp/sdk_tools/ssh-key.pub".into(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    ssh_off: bool,
    #[serde(default)]
    sdb_off: bool,
}

pub struct Access {
    paths: AccessPaths,
    state: Mutex<State>,
    logger: Option<Arc<Logger>>,
    sdb_masked: AtomicBool,
}

impl Access {
    pub fn new(paths: AccessPaths, logger: Option<Arc<Logger>>) -> Self {
        let state = fs::read(&paths.state)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            paths,
            state: Mutex::new(state),
            logger,
            sdb_masked: AtomicBool::new(false),
        }
    }

    /// Re-applies the off switches and asks about pushed keys, forever.
    pub fn start(self: &Arc<Self>, approvals: ApprovalHandle, wait: Duration) {
        let access = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("access".into())
            .spawn(move || access.watch(&approvals, wait));
        if let Err(error) = spawned {
            self.log(&format!("access_thread_failed error={error}"));
        }
    }

    pub fn view(&self) -> AccessView {
        let state = self.state();
        let keys = fs::read_to_string(&self.paths.authorized_keys)
            .map(|text| ssh_keys::parse(&text))
            .unwrap_or_default();
        AccessView {
            ssh: !state.ssh_off,
            ssh_running: !active(SSH_UNITS).is_empty(),
            sdb: !state.sdb_off,
            sdb_running: !active(SDB_UNITS).is_empty(),
            sdb_host: sdb_host(),
            keys: keys.iter().map(key_view).collect(),
            key_drop: self.paths.key_drop.display().to_string(),
        }
    }

    /// Applies and audits each given change; `by` is the requester's label.
    pub fn set(
        &self,
        ssh: Option<bool>,
        sdb: Option<bool>,
        sdb_host: Option<&str>,
        by: &str,
    ) -> Result<AccessView, &'static str> {
        let host = match sdb_host {
            Some(host) => Some(
                host.trim()
                    .parse::<Ipv4Addr>()
                    .map_err(|_| "invalid_host")?,
            ),
            None => None,
        };
        let mut state = self.state();
        if let Some(on) = ssh.filter(|on| *on == state.ssh_off) {
            state.ssh_off = !on;
            self.save(state)?;
            if on {
                let _ = systemctl(&["start"], SSH_UNITS);
            } else {
                let _ = systemctl(&["stop"], SSH_UNITS);
            }
            self.log(&format!("access=ssh_{} by={by}", on_off(on)));
        }
        if let Some(on) = sdb.filter(|on| *on == state.sdb_off) {
            state.sdb_off = !on;
            self.save(state)?;
            if on {
                self.unmask_sdb();
            } else {
                self.mask_sdb();
            }
            self.log(&format!("access=sdb_{} by={by}", on_off(on)));
        }
        if let Some(host) = host {
            let output = Command::new("/usr/bin/vconftool")
                .args(["set", "-f", "-t", "string", SDB_HOST_KEY, &host.to_string()])
                .output();
            if !output.is_ok_and(|output| output.status.success()) {
                return Err("sdb_host_failed");
            }
            // sdbd reads the host when it starts.
            let _ = systemctl(&["try-restart"], &["sdbd.service"]);
            self.log(&format!("access=sdb_host host={host} by={by}"));
        }
        Ok(self.view())
    }

    /// Removes a key, but never the last one while SSH is on.
    pub fn remove_key(&self, fingerprint: &str, by: &str) -> Result<AccessView, &'static str> {
        let existing = fs::read_to_string(&self.paths.authorized_keys).unwrap_or_default();
        let keys = ssh_keys::parse(&existing);
        let key = keys
            .iter()
            .find(|key| key.fingerprint == fingerprint)
            .ok_or("not_found")?;
        if !self.state().ssh_off && keys.len() <= 1 {
            return Err("last_key");
        }
        let text = ssh_keys::remove(&existing, fingerprint).ok_or("not_found")?;
        self.write_keys(&text).map_err(|_| "keys_save_failed")?;
        self.log(&format!(
            "access=ssh_key_removed fingerprint={} comment={} by={by}",
            key.fingerprint,
            safe(&key.comment)
        ));
        Ok(self.view())
    }

    fn watch(&self, approvals: &ApprovalHandle, wait: Duration) {
        let mut enforced: Option<Instant> = None;
        // A file that could not be deleted is not asked about again.
        let mut skip = None;
        loop {
            if enforced.is_none_or(|at| at.elapsed() >= ENFORCE) {
                self.enforce();
                enforced = Some(Instant::now());
            }
            match read_drop(&self.paths) {
                Ok(None) => skip = None,
                Ok(Some((identity, _))) | Err((identity, _)) if skip == Some(identity) => {}
                Ok(Some((identity, keys))) => {
                    self.ask(approvals, &keys, wait);
                    if fs::remove_file(&self.paths.key_drop).is_err() {
                        skip = Some(identity);
                    }
                }
                Err((identity, reason)) => {
                    self.log(&format!("access=ssh_key_file_ignored reason={reason}"));
                    if fs::remove_file(&self.paths.key_drop).is_err() {
                        skip = Some(identity);
                    }
                }
            }
            thread::sleep(POLL);
        }
    }

    /// Shows the pushed keys on a prompt and adds them if the user allows.
    fn ask(&self, approvals: &ApprovalHandle, keys: &[Key], wait: Duration) {
        let shown: Vec<String> = keys
            .iter()
            .map(|key| format!("{} {} {}", key.kind, key.fingerprint, key.comment))
            .collect();
        let fingerprints: Vec<&str> = keys.iter().map(|key| key.fingerprint.as_str()).collect();
        let (reply, answer) = mpsc::channel();
        let request = ApprovalRequest {
            ticket: 0,
            request_id: format!("ssh-key-{}", unix_now()),
            label: PUSH_LABEL.into(),
            kind: PromptKind::SshKey,
            install: None,
            uninstall: None,
            command: shown.join("\n"),
            reply,
        };
        let ticket = match approvals.request(request) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.log(&format!(
                    "access=ssh_key_denied fingerprint={} reason=approval_unavailable error={error}",
                    fingerprints.join(",")
                ));
                return;
            }
        };
        let reason = match answer.recv_timeout(wait) {
            Ok(ApprovalResult::AllowOnce | ApprovalResult::AllowAlways) => {
                let existing = fs::read_to_string(&self.paths.authorized_keys).unwrap_or_default();
                match self.write_keys(&ssh_keys::add(&existing, keys)) {
                    Ok(()) => {
                        for key in keys {
                            self.log(&format!(
                                "access=ssh_key_added fingerprint={} comment={} by=user",
                                key.fingerprint,
                                safe(&key.comment)
                            ));
                        }
                        return;
                    }
                    Err(error) => format!("keys_save_failed error={error}"),
                }
            }
            Ok(ApprovalResult::Denied(_, reason)) => reason.to_owned(),
            Err(_) => {
                let _ = approvals.cancel(ticket);
                "approval_wait_timeout".to_owned()
            }
        };
        self.log(&format!(
            "access=ssh_key_denied fingerprint={} reason={reason}",
            fingerprints.join(",")
        ));
    }

    fn enforce(&self) {
        let state = self.state();
        if state.ssh_off {
            let running = active(SSH_UNITS);
            if !running.is_empty() && systemctl(&["stop"], SSH_UNITS).is_ok() {
                self.log("access_enforced ssh=off stopped=dropbear.service");
            }
        }
        if state.sdb_off {
            if !self.sdb_masked.load(Ordering::Relaxed) || !active(SDB_UNITS).is_empty() {
                self.mask_sdb();
                self.log("access_enforced sdb=off");
            }
        } else if self.sdb_masked.load(Ordering::Relaxed) {
            self.unmask_sdb();
        }
    }

    /// Runtime masks live in /run: a reboot without tv-authd brings SDB back.
    fn mask_sdb(&self) {
        let _ = systemctl(&["mask", "--runtime"], SDB_UNITS);
        let running = active(SDB_UNITS);
        if !running.is_empty() {
            let _ = systemctl(&["stop"], &running[..]);
        }
        self.sdb_masked.store(true, Ordering::Relaxed);
    }

    fn unmask_sdb(&self) {
        let _ = systemctl(&["unmask", "--runtime"], SDB_UNITS);
        self.sdb_masked.store(false, Ordering::Relaxed);
        // The socket's condition (developer mode) decides whether it starts.
        if systemctl(&["start"], &["sdbd_tcp.socket"]).is_ok()
            && !active(&["sdbd_tcp.socket"]).is_empty()
        {
            let _ = systemctl(&["start"], &["sdbd.service"]);
        }
    }

    fn state(&self) -> State {
        *self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn save(&self, state: State) -> Result<(), &'static str> {
        let bytes = serde_json::to_vec_pretty(&state).map_err(|_| "state_save_failed")?;
        write_private(&self.paths.state, &bytes).map_err(|_| "state_save_failed")?;
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = state;
        Ok(())
    }

    fn write_keys(&self, text: &str) -> io::Result<()> {
        write_private(&self.paths.authorized_keys, text.as_bytes())
    }

    fn log(&self, message: &str) {
        if let Some(logger) = &self.logger {
            let _ = logger.log(message);
        }
    }
}

/// The pushed file's identity (device, inode, size, mtime) and its keys, or
/// why it is ignored; `Ok(None)` when there is none.
type DropIdentity = (u64, u64, u64, i64);

/// The pushed keys, or why the file is ignored.
type Pushed = Result<Option<(DropIdentity, Vec<Key>)>, (DropIdentity, String)>;

fn read_drop(paths: &AccessPaths) -> Pushed {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&paths.key_drop);
    let mut file = match file {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            let identity = fs::symlink_metadata(&paths.key_drop)
                .map(|meta| (meta.dev(), meta.ino(), meta.len(), meta.mtime()))
                .unwrap_or_default();
            return Err((identity, format!("unreadable({})", error.kind())));
        }
        Ok(file) => file,
    };
    let meta = file
        .metadata()
        .map_err(|_| ((0, 0, 0, 0), "unreadable".to_owned()))?;
    let identity = (meta.dev(), meta.ino(), meta.len(), meta.mtime());
    if !meta.is_file() || meta.len() > MAX_KEY_DROP {
        return Err((identity, "not_a_small_regular_file".into()));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|_| (identity, "not_text".to_owned()))?;
    let keys = ssh_keys::parse(&text);
    if keys.is_empty() {
        return Err((identity, "no_public_key".into()));
    }
    Ok(Some((identity, keys)))
}

fn key_view(key: &Key) -> SshKeyView {
    SshKeyView {
        kind: key.kind.clone(),
        comment: display_safe(&key.comment),
        fingerprint: key.fingerprint.clone(),
    }
}

/// A key comment for one log line: no spaces or control characters.
fn safe(comment: &str) -> String {
    display_safe(comment).replace(char::is_whitespace, "_")
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

fn sdb_host() -> Option<String> {
    let output = Command::new("/usr/bin/vconftool")
        .args(["get", SDB_HOST_KEY])
        .output()
        .ok()?;
    // `db/sdk/develop/ip, value = 172.17.2.206 (String)`
    let text = String::from_utf8_lossy(&output.stdout);
    let value = text.split_once("value = ")?.1;
    Some(value.split(" (").next()?.trim().to_owned()).filter(|value| !value.is_empty())
}

fn write_private(path: &PathBuf, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(&temporary, path)
}

fn systemctl(verb: &[&str], units: &[impl AsRef<str>]) -> io::Result<()> {
    let output = Command::new("/usr/bin/systemctl")
        .args(verb)
        .args(units.iter().map(AsRef::as_ref))
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

/// The units among `units` that are active (or activating).
fn active(units: &[&str]) -> Vec<String> {
    let Ok(output) = Command::new("/usr/bin/systemctl")
        .arg("is-active")
        .args(units)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .zip(units)
        .filter(|(state, _)| matches!(state.trim(), "active" | "activating" | "reloading"))
        .map(|(_, unit)| unit.to_string())
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDydMB0O7XxHHa64MbIoqwtYBeMEDzkWzaOivH+ATzBI test@one";

    fn paths(dir: &std::path::Path) -> AccessPaths {
        AccessPaths {
            state: dir.join("access.json"),
            authorized_keys: dir.join("authorized_keys"),
            key_drop: dir.join("ssh-key.pub"),
        }
    }

    #[test]
    fn pushed_files_are_read_only_when_safe() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        assert!(matches!(read_drop(&paths), Ok(None)));
        fs::write(&paths.key_drop, format!("{ED}\n")).unwrap();
        let (_, keys) = read_drop(&paths).unwrap().unwrap();
        assert_eq!(keys[0].comment, "test@one");
        fs::write(&paths.key_drop, "hello").unwrap();
        assert_eq!(read_drop(&paths).unwrap_err().1, "no_public_key");
        fs::remove_file(&paths.key_drop).unwrap();
        std::os::unix::fs::symlink(&paths.state, &paths.key_drop).unwrap();
        assert!(read_drop(&paths).unwrap_err().1.starts_with("unreadable"));
    }

    #[test]
    fn the_last_key_stays_while_ssh_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        fs::write(&paths.authorized_keys, format!("{ED}\n")).unwrap();
        let access = Access::new(paths.clone(), None);
        let fingerprint = ssh_keys::parse(ED)[0].fingerprint.clone();
        assert_eq!(
            access.remove_key(&fingerprint, "test").unwrap_err(),
            "last_key"
        );
        assert_eq!(
            access.remove_key("SHA256:nope", "test").unwrap_err(),
            "not_found"
        );
        assert_eq!(
            access
                .set(None, None, Some("not-an-ip"), "test")
                .unwrap_err(),
            "invalid_host"
        );
        assert!(fs::read_to_string(&paths.authorized_keys)
            .unwrap()
            .contains("test@one"));
    }

    #[test]
    fn comments_are_one_log_token() {
        assert_eq!(safe("my laptop\tkey"), "my_laptop_key");
    }
}
