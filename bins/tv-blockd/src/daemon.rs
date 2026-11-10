use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::io::{self, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tv_blockd::policy::{self, Config, HOSTS_MARKER, HOST_GROUPS};

const CONFIG: &str = "/opt/tv-suite/etc/blockd.json";
const LIST_CACHE: &str = "/opt/tv-suite/var/lists";
const CHANGES: &str = "/opt/tv-suite/var/changes.json";
const RUN_DIR: &str = "/run/tv-suite";
const HOSTS_FILE: &str = "/run/tv-suite/hosts";
const HOSTS_ORIGINAL: &str = "/run/tv-suite/hosts.original";
const SOCKET: &str = "/run/tv-suite/blockd.sock";
const LOG: &str = "/run/tv-suite/blockd.log";
const MAX_LOG_BYTES: u64 = 256 * 1024;

const TICK: Duration = Duration::from_secs(60);
const LIST_REFRESH: Duration = Duration::from_secs(24 * 3600);
const MAX_LIST_BYTES: usize = 16 << 20;

const UI_LABEL: &str = "User::Pkg::org.tvauth.ui";
const UI_UID: u32 = 5001;
const USERS_GID: u32 = 100;

const AUTOUPDATE_KEY: &str = "db/menu/support/softwareupdate/autoupdate";
/// `ConditionPathExists=!` files of Samsung's telemetry units.
const BRAKES: &[&str] = &[
    "/opt/usr/share/systemd/system/canalysis-daemon",
    "/opt/usr/share/systemd/system/pisa-service",
    "/opt/usr/share/systemd/system/vision-data-recognition-service",
    "/tmp/eltlaunch.cond",
];
const TELEMETRY_UNITS: &[&str] = &[
    "canalysis",
    "dlog_logger",
    "ep-agent",
    "faultmgr",
    "smart_deadlock",
    "trace_manager",
    "pisa-service",
    "vision-data-recognition-service",
    "elt-daemon",
];
const TELEMETRY_PROCESSES: &[&str] = &["EdgeScraper"];

fn log(message: &str) {
    let path = Path::new(LOG);
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    {
        let _ = writeln!(file, "{} {message}", unix_now());
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// The last reconcile pass, for `status`.
#[derive(Debug, Default, Serialize)]
struct Report {
    at: u64,
    blocked_hosts: usize,
    hosts_applied: bool,
    namespaces: usize,
    namespaces_mounted: usize,
    /// Namespaces without `/run/tv-suite/hosts` or `/etc/hosts` to mount on.
    namespaces_skipped: usize,
    namespaces_failed: usize,
    otn_autoupdate: Option<i32>,
    telemetry_active: Vec<String>,
    lists: Vec<ListReport>,
    errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ListReport {
    url: String,
    hosts: usize,
    fetched_at: u64,
    error: Option<String>,
}

/// What tv-blockd changed that a reboot does not reset, or that it must put
/// back: switching an option off (or `undo`) restores exactly this.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Changes {
    /// The boot `stopped` belongs to.
    #[serde(default)]
    boot_id: String,
    /// The auto-update setting before tv-blockd first turned it off.
    #[serde(default)]
    otn_autoupdate: Option<i32>,
    /// Brake files and the directories made for them, in creation order.
    #[serde(default)]
    created: Vec<String>,
    /// Telemetry units that were running when tv-blockd stopped them.
    #[serde(default)]
    stopped: Vec<String>,
}

impl Changes {
    fn load() -> Self {
        let mut changes: Changes = fs::read(CHANGES)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_default()
            .trim()
            .to_owned();
        if changes.boot_id != boot_id {
            // A reboot already restarted them (and dropped the runtime masks).
            changes.stopped.clear();
            changes.boot_id = boot_id;
        }
        changes
    }

    fn save(&self) {
        if let Err(error) = save_json(CHANGES, self) {
            log(&format!("{CHANGES}: {error}"));
        }
    }
}

struct Daemon {
    config: Config,
    report: Report,
    changes: Changes,
    written_hosts: Option<String>,
    /// Telemetry units masked by this process.
    masked: bool,
    /// Telemetry changes undone since telemetry was last switched off.
    telemetry_restored: bool,
    list_fetched: BTreeMap<String, Instant>,
    /// Namespaces already reported as failing, so each is logged once.
    reported_failures: std::collections::BTreeSet<String>,
}

pub fn main() {
    if let Err(error) = run() {
        log(&format!("fatal: {error}"));
        eprintln!("tv-blockd: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {}
        ["status"] => return client(json!({"method": "status"})),
        ["undo"] => return client(json!({"method": "undo"})),
        ["set", config] => {
            let config: Value = serde_json::from_str(config)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            return client(json!({"method": "set", "config": config}));
        }
        _ => {
            println!(
                "usage: tv-blockd            run the daemon\n\
                 \x20      tv-blockd status     print config and last reconcile\n\
                 \x20      tv-blockd set JSON   replace the whole config\n\
                 \x20      tv-blockd undo       switch everything off and restore what it changed\n\
                 config: {CONFIG}; control socket: {SOCKET}"
            );
            return Ok(());
        }
    }
    fs::create_dir_all(RUN_DIR)?;
    fs::create_dir_all(Path::new(CONFIG).parent().unwrap())?;
    fs::create_dir_all(LIST_CACHE)?;
    let config = match fs::read(CONFIG) {
        Ok(bytes) => serde_json::from_slice::<Config>(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let config = Config::default();
            save_json(CONFIG, &config)?;
            config
        }
        Err(error) => return Err(error),
    };
    if let Err(error) = config.validate() {
        log(&format!("config invalid ({error}); using defaults"));
    }
    let mut daemon = Daemon {
        config: if config.validate().is_ok() {
            config
        } else {
            Config::default()
        },
        report: Report::default(),
        changes: Changes::load(),
        written_hosts: None,
        masked: false,
        telemetry_restored: false,
        list_fetched: BTreeMap::new(),
        reported_failures: std::collections::BTreeSet::new(),
    };

    let _ = fs::remove_file(SOCKET);
    let listener = UnixListener::bind(SOCKET)?;
    fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o660))?;
    let path = CString::new(SOCKET).unwrap();
    unsafe { libc::chown(path.as_ptr(), 0, USERS_GID) };
    log("started");

    daemon.reconcile();
    let mut next = Instant::now() + TICK;
    loop {
        let timeout = next.saturating_duration_since(Instant::now());
        if wait_readable(&listener, timeout) {
            if let Ok((stream, _)) = listener.accept() {
                if let Err(error) = daemon.serve(stream) {
                    log(&format!("client: {error}"));
                }
            }
        }
        if Instant::now() >= next {
            daemon.reconcile();
            next = Instant::now() + TICK;
        }
    }
}

/// Client mode: one request over the control socket, response to stdout.
fn client(request: Value) -> io::Result<()> {
    let response = tv_proto::call(SOCKET, &request)?;
    let value: Value = serde_json::from_slice(&response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
    if value["status"] == "ok" {
        Ok(())
    } else {
        Err(io::Error::other(
            value["error"].as_str().unwrap_or("failed").to_owned(),
        ))
    }
}

fn wait_readable(listener: &UnixListener, timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    unsafe { libc::poll(&mut poll, 1, millis) > 0 }
}

fn save_json(path: &str, value: &impl Serialize) -> io::Result<()> {
    let temporary = format!("{path}.tmp");
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(&temporary, bytes)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, path)
}

/// uid and SMACK label of the peer (`SO_PEERCRED`, `SO_PEERSEC`).
// ponytail: tv-authd has the same check (rustix-based); share it if a third
// daemon needs it.
fn peer(stream: &UnixStream) -> io::Result<(u32, String)> {
    let fd = stream.as_raw_fd();
    let mut credentials = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut label = [0_u8; 256];
    let mut length = label.len() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERSEC,
            label.as_mut_ptr().cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let label = &label[..(length as usize).min(label.len())];
    let label = String::from_utf8_lossy(label)
        .trim_end_matches('\0')
        .to_owned();
    Ok((credentials.uid, label))
}

impl Daemon {
    fn serve(&mut self, stream: UnixStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let (uid, label) = peer(&stream)?;
        let mut writer = stream.try_clone()?;
        if !(uid == 0 || (uid == UI_UID && label == UI_LABEL)) {
            log(&format!("refused uid={uid} label={label}"));
            return tv_proto::write_json(&mut writer, &json!({"status": "denied"}));
        }
        let line = tv_proto::read_message(&mut BufReader::new(stream), 64 * 1024)?;
        let request: Value = serde_json::from_slice(&line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let response = match request["method"].as_str() {
            Some("status") => self.status(),
            Some("set") => match serde_json::from_value::<Config>(request["config"].clone()) {
                Ok(config) => match config.validate() {
                    Ok(()) => {
                        log(&format!("config set by uid={uid} label={label}"));
                        self.apply(config)
                    }
                    Err(error) => json!({"status": "error", "error": error}),
                },
                Err(error) => json!({"status": "error", "error": error.to_string()}),
            },
            Some("undo") => {
                log(&format!("undo by uid={uid} label={label}"));
                let config = Config {
                    hosts: false,
                    otn_lock: false,
                    telemetry: false,
                    ..self.config.clone()
                };
                self.apply(config)
            }
            _ => json!({"status": "error", "error": "unknown method"}),
        };
        tv_proto::write_json(&mut writer, &response)
    }

    fn apply(&mut self, config: Config) -> Value {
        self.config = config;
        if let Err(error) = save_json(CONFIG, &self.config) {
            log(&format!("config save: {error}"));
        }
        self.reconcile();
        self.status()
    }

    fn status(&self) -> Value {
        let groups: Vec<Value> = HOST_GROUPS
            .iter()
            .map(|group| {
                json!({"id": group.id, "label": group.label, "evidence": group.evidence,
                       "note": group.note, "optional": group.optional, "hosts": group.hosts})
            })
            .collect();
        json!({"status": "ok", "config": self.config, "report": self.report,
               "changes": self.changes, "groups": groups})
    }

    fn reconcile(&mut self) {
        let started = Instant::now();
        let mut report = Report {
            at: unix_now(),
            ..Report::default()
        };
        let remote = self.remote_hosts(&mut report);
        self.reconcile_hosts(&remote, &mut report);
        self.reconcile_otn(&mut report);
        self.reconcile_telemetry(&mut report);
        for error in &report.errors {
            log(error);
        }
        log(&format!(
            "reconciled blocked={} namespaces={} mounted={} skipped={} failed={} otn={:?} telemetry_active={} in {} ms",
            report.blocked_hosts,
            report.namespaces,
            report.namespaces_mounted,
            report.namespaces_skipped,
            report.namespaces_failed,
            report.otn_autoupdate,
            report.telemetry_active.len(),
            started.elapsed().as_millis()
        ));
        self.report = report;
    }

    // --- hosts ---------------------------------------------------------

    fn remote_hosts(&mut self, report: &mut Report) -> Vec<String> {
        let mut hosts = Vec::new();
        for url in self.config.lists.clone() {
            let cache = Path::new(LIST_CACHE).join(format!("{:016x}.txt", fnv(&url)));
            let stale = self
                .list_fetched
                .get(&url)
                .is_none_or(|at| at.elapsed() >= LIST_REFRESH);
            let mut error = None;
            if stale {
                match tv_tizen::http::get(&url, MAX_LIST_BYTES, 60) {
                    Ok(body) => {
                        let _ = fs::write(&cache, body);
                        self.list_fetched.insert(url.clone(), Instant::now());
                    }
                    Err(failure) => error = Some(failure),
                }
            }
            let text = fs::read_to_string(&cache).unwrap_or_default();
            let parsed = policy::parse_list(&text);
            report.lists.push(ListReport {
                url: url.clone(),
                hosts: parsed.len(),
                fetched_at: fs::metadata(&cache)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |age| age.as_secs()),
                error,
            });
            hosts.extend(parsed);
        }
        hosts
    }

    fn reconcile_hosts(&mut self, remote: &[String], report: &mut Report) {
        let namespaces = mount_namespaces();
        report.namespaces = namespaces.len();
        if !self.config.hosts {
            if self.written_hosts.take().is_some() || any_marked(&namespaces) {
                for pid in namespaces.values() {
                    if let Err(error) = unmount_hosts_in(*pid) {
                        report.namespaces_failed += 1;
                        report.errors.push(format!("unmount in pid {pid}: {error}"));
                    }
                }
            }
            return;
        }
        let blocked = self.config.blocked_hosts(remote.iter().map(String::as_str));
        report.blocked_hosts = blocked.len();
        let content = policy::hosts_file(&original_hosts(), &blocked);
        if self.written_hosts.as_deref() != Some(content.as_str()) {
            // Rewrite in place: existing bind mounts follow the same inode.
            let written = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o644)
                .open(HOSTS_FILE)
                .and_then(|mut file| file.write_all(content.as_bytes()));
            if let Err(error) = written {
                report.errors.push(format!("write {HOSTS_FILE}: {error}"));
                return;
            }
            let _ = fs::set_permissions(HOSTS_FILE, fs::Permissions::from_mode(0o644));
            self.written_hosts = Some(content);
        }
        for ((_, namespace), pid) in &namespaces {
            match mount_hosts_in(*pid) {
                Ok(Mounted::Now) => report.namespaces_mounted += 1,
                Ok(Mounted::Already) => {}
                Ok(Mounted::NoTarget) => report.namespaces_skipped += 1,
                Err(error) => {
                    report.namespaces_failed += 1;
                    if self.reported_failures.insert(namespace.clone()) {
                        report.errors.push(format!(
                            "mount in {namespace} (pid {pid} {}): {error}",
                            fs::read_to_string(format!("/proc/{pid}/comm"))
                                .unwrap_or_default()
                                .trim()
                        ));
                    }
                }
            }
        }
        report.hosts_applied = report.namespaces_failed == 0;
    }

    // --- OTN, telemetry -------------------------------------------

    fn reconcile_otn(&mut self, report: &mut Report) {
        match tv_tizen::vconf::get_int(AUTOUPDATE_KEY) {
            Ok(value) => {
                report.otn_autoupdate = Some(value);
                if !self.config.otn_lock {
                    self.restore_otn(report);
                } else if value != 0 {
                    if self.changes.otn_autoupdate.is_none() {
                        self.changes.otn_autoupdate = Some(value);
                        self.changes.save();
                    }
                    match tv_tizen::vconf::set_int(AUTOUPDATE_KEY, 0) {
                        Ok(()) => {
                            report.otn_autoupdate = Some(0);
                            log("otn autoupdate reset to 0");
                        }
                        Err(error) => report.errors.push(error),
                    }
                }
            }
            Err(error) => report.errors.push(error),
        }
    }

    /// Put back the auto-update setting tv-blockd found.
    fn restore_otn(&mut self, report: &mut Report) {
        let Some(original) = self.changes.otn_autoupdate else {
            return;
        };
        match tv_tizen::vconf::set_int(AUTOUPDATE_KEY, original) {
            Ok(()) => {
                report.otn_autoupdate = Some(original);
                self.changes.otn_autoupdate = None;
                self.changes.save();
                log(&format!("otn autoupdate restored to {original}"));
            }
            Err(error) => report.errors.push(error),
        }
    }

    /// Remove the brake files tv-blockd made, unmask, and restart the units
    /// it stopped. `EdgeScraper` has no unit; it returns at the next boot.
    fn restore_telemetry(&mut self, units: &[String]) {
        for path in self.changes.created.drain(..).rev() {
            // Directories go only if empty.
            let _ = fs::remove_file(&path).or_else(|_| fs::remove_dir(&path));
        }
        let _ = systemctl(&["unmask", "--runtime"], units);
        self.masked = false;
        if !self.changes.stopped.is_empty() {
            let _ = systemctl(&["start", "--no-block"], &self.changes.stopped);
            self.changes.stopped.clear();
        }
        self.changes.save();
        log("telemetry restored");
    }

    fn reconcile_telemetry(&mut self, report: &mut Report) {
        let units: Vec<String> = TELEMETRY_UNITS
            .iter()
            .map(|unit| format!("{unit}.service"))
            .collect();
        if !self.config.telemetry {
            if !self.telemetry_restored {
                self.restore_telemetry(&units);
                self.telemetry_restored = true;
            }
            report.telemetry_active = active_units(&units);
            return;
        }
        self.telemetry_restored = false;
        for brake in BRAKES {
            let path = Path::new(brake);
            if path.exists() {
                continue;
            }
            let mut missing: Vec<&Path> = path
                .ancestors()
                .skip(1)
                .take_while(|dir| !dir.exists())
                .collect();
            missing.reverse();
            missing.push(path);
            for (index, created) in missing.iter().enumerate() {
                let result = if index + 1 == missing.len() {
                    fs::write(created, b"")
                } else {
                    fs::create_dir(created)
                };
                if let Err(error) = result {
                    report
                        .errors
                        .push(format!("brake {}: {error}", created.display()));
                    break;
                }
                self.changes.created.push(created.display().to_string());
            }
            self.changes.save();
        }
        if !self.masked {
            // Runtime masks live in /run: re-applied on every boot.
            if let Err(error) = systemctl(&["mask", "--runtime"], &units) {
                report.errors.push(format!("mask: {error}"));
            }
            self.masked = true;
        }
        let active = active_units(&units);
        if !active.is_empty() {
            let _ = systemctl(&["stop", "--no-block"], &active);
            for unit in &active {
                if !self.changes.stopped.contains(unit) {
                    self.changes.stopped.push(unit.clone());
                }
            }
            self.changes.save();
        }
        report.telemetry_active = active;
        for pid in processes_named(TELEMETRY_PROCESSES) {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            log(&format!("killed telemetry process {pid}"));
        }
    }
}

/// The TV's own `/etc/hosts`, saved before the first mount.
fn original_hosts() -> String {
    if let Ok(saved) = fs::read_to_string(HOSTS_ORIGINAL) {
        return saved;
    }
    let current = fs::read_to_string("/proc/1/root/etc/hosts").unwrap_or_default();
    let original = if current.is_empty() || current.starts_with(HOSTS_MARKER) {
        "127.0.0.1 localhost\n::1 localhost\n".to_owned()
    } else {
        current
    };
    let _ = fs::write(HOSTS_ORIGINAL, &original);
    original
}

/// One pid per distinct mount namespace, PID 1's first.
fn mount_namespaces() -> BTreeMap<(bool, String), i32> {
    let mut namespaces = BTreeMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return namespaces;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(namespace) = fs::read_link(entry.path().join("ns/mnt")) else {
            continue;
        };
        namespaces
            .entry((pid != 1, namespace.to_string_lossy().into_owned()))
            .or_insert(pid);
    }
    // PID 1's namespace sorts first ((false, _) < (true, _)); a namespace
    // also seen with another pid keeps whichever came first, so drop
    // duplicates of PID 1's.
    let first = namespaces
        .iter()
        .find(|((other, _), _)| !other)
        .map(|((_, name), _)| name.clone());
    if let Some(first) = first {
        namespaces.retain(|(other, name), _| !*other || *name != first);
    }
    namespaces
}

fn any_marked(namespaces: &BTreeMap<(bool, String), i32>) -> bool {
    namespaces.values().any(|pid| {
        fs::read(format!("/proc/{pid}/root/etc/hosts"))
            .is_ok_and(|bytes| bytes.starts_with(HOSTS_MARKER.as_bytes()))
    })
}

enum Mounted {
    Now,
    Already,
    /// The namespace has no `/run/tv-suite/hosts` or no `/etc/hosts`.
    NoTarget,
}

/// Bind-mount `/run/tv-suite/hosts` over `/etc/hosts` in `pid`'s mount
/// namespace unless ours is already there.
///
/// A forked child does the `setns`: a multithreaded process cannot change
/// its mount namespace, and this one must stay in its own. A bind mount's
/// source has to belong to the caller's current namespace, so the source is
/// looked up by path inside the target (`/run` is the same tmpfs nearly
/// everywhere); namespaces without it are skipped.
fn mount_hosts_in(pid: i32) -> io::Result<Mounted> {
    let namespace = CString::new(format!("/proc/{pid}/ns/mnt")).unwrap();
    let source = CString::new(HOSTS_FILE).unwrap();
    let marker = HOSTS_MARKER.as_bytes();
    let code = in_child(|| unsafe {
        if !enter(&namespace) {
            return 10;
        }
        if hosts_marked(marker) {
            return 0;
        }
        if libc::access(source.as_ptr(), libc::R_OK) != 0
            || libc::access(c"/etc/hosts".as_ptr(), libc::F_OK) != 0
        {
            return 2;
        }
        if libc::mount(
            source.as_ptr(),
            c"/etc/hosts".as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        ) != 0
        {
            // Exit code 20 + errno (the errnos mount returns are small).
            return 20 + (*libc::__errno_location()).clamp(0, 200);
        }
        1
    })?;
    match code {
        0 => Ok(Mounted::Already),
        1 => Ok(Mounted::Now),
        2 => Ok(Mounted::NoTarget),
        10 => Err(io::Error::other("setns failed")),
        code => Err(io::Error::from_raw_os_error(code - 20)),
    }
}

fn unmount_hosts_in(pid: i32) -> io::Result<()> {
    let namespace = CString::new(format!("/proc/{pid}/ns/mnt")).unwrap();
    let marker = HOSTS_MARKER.as_bytes();
    let code = in_child(|| unsafe {
        if !enter(&namespace) {
            return 10;
        }
        // Propagation can stack copies; peel ours off.
        for _ in 0..4 {
            if !hosts_marked(marker) {
                return 0;
            }
            if libc::umount2(c"/etc/hosts".as_ptr(), libc::MNT_DETACH) != 0 {
                return 12;
            }
        }
        0
    })?;
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!("child exited {code}")))
    }
}

/// Run `work` in a forked child (raw syscalls only) and return its exit code.
fn in_child(work: impl FnOnce() -> i32) -> io::Result<i32> {
    match unsafe { libc::fork() } {
        -1 => Err(io::Error::last_os_error()),
        0 => unsafe { libc::_exit(work()) },
        child => {
            let mut status = 0;
            if unsafe { libc::waitpid(child, &mut status, 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                255
            })
        }
    }
}

unsafe fn enter(namespace: &CString) -> bool {
    let fd = libc::open(namespace.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
    fd >= 0 && libc::setns(fd, libc::CLONE_NEWNS) == 0
}

unsafe fn hosts_marked(marker: &[u8]) -> bool {
    let fd = libc::open(c"/etc/hosts".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
    if fd < 0 {
        return false;
    }
    let mut buffer = [0_u8; 64];
    let read = libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len());
    libc::close(fd);
    read >= marker.len() as isize && &buffer[..marker.len()] == marker
}

fn systemctl(verb: &[&str], units: &[String]) -> io::Result<()> {
    let output = Command::new("/usr/bin/systemctl")
        .args(verb)
        .args(units)
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
fn active_units(units: &[String]) -> Vec<String> {
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
        .map(|(_, unit)| unit.clone())
        .collect()
}

fn processes_named(names: &[&str]) -> Vec<i32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let comm = fs::read_to_string(entry.path().join("comm")).ok()?;
            names.contains(&comm.trim()).then_some(pid)
        })
        .collect()
}

/// FNV-1a, for cache file names.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
