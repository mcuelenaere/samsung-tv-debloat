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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tv_blockd::policy::{self, Config, HOSTS_MARKER, HOST_GROUPS};
use tv_blockd::sysinfo;

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
/// Samsung services an option stops, for as long as it is on: their units
/// are runtime-masked (gone at the next boot) and stopped, their processes
/// killed and their apps closed. Switching the option off unmasks the units
/// and starts the ones it stopped. Units and apps that do not exist on a TV
/// are harmless: the lists cover several models.
struct Services {
    /// The option in [`Config`] and the key in the report.
    key: &'static str,
    units: &'static [&'static str],
    processes: &'static [&'static str],
    /// Closed when the option is switched on (and once after each start of
    /// tv-blockd); they are not restarted, their services start them again.
    apps: &'static [&'static str],
    /// `ConditionPathExists=!` files under /tmp (gone at reboot) that keep
    /// units from starting.
    brakes: &'static [&'static str],
    /// Runtime-mask the units. Not for units whose file is itself in
    /// /run/systemd/system, where the mask would go.
    mask: bool,
}

const SERVICES: &[Services] = &[
    Services {
        key: "telemetry",
        units: &[
            "canalysis.service",
            "canalysis-service.service",
            "adagent-service.service",
            "dlog_logger.service",
            "ep-agent.service",
            "faultmgr.service",
            "smart_deadlock.service",
            "trace_manager.service",
            "pisa-service.service",
            "pisa-service.path",
            "vision-data-recognition-service.service",
            "elt-daemon.service",
            "emergency-log-transmitter.service",
            "contents-recognition-service.service",
            "contents-ui-detector-service.service",
            "mirroring-recognition-manager.service",
            "kpi-logging.service",
            "devicetracker.service",
        ],
        processes: &["EdgeScraper", "contents-ott-detector"],
        apps: &[],
        brakes: &["/tmp/eltlaunch.cond"],
        mask: true,
    },
    Services {
        key: "airplay",
        units: &[
            "com.samsung.tv.airplay-service.service",
            "com.samsung.tv.airplay-service.path",
            "com.samsung.tv.airplay-service-2.service",
            "com.samsung.tv.airplay-service-2.path",
        ],
        processes: &[],
        apps: &["com.samsung.tv.airplay-app", "com.samsung.tv.airplay-app-2"],
        brakes: &[],
        mask: true,
    },
    Services {
        key: "multiview_preload",
        units: &["com.samsung.tv.multiscreen.service"],
        processes: &[],
        apps: &["com.samsung.tv.multiscreen"],
        brakes: &[],
        mask: true,
    },
    Services {
        // Starts only while Samsung's developer mode is on (the socket's
        // ConditionPathExists); the USB socket has no port on a TV.
        key: "sdb_off",
        units: &["sdbd_tcp.socket", "sdbd.socket", "sdbd.service"],
        processes: &[],
        apps: &[],
        brakes: &[],
        mask: true,
    },
    Services {
        // The boot hook copies dropbear.service into /run/systemd/system and
        // starts it; nothing else starts it, so a stop is enough.
        key: "ssh_off",
        units: &["dropbear.service"],
        processes: &[],
        apps: &[],
        brakes: &[],
        mask: false,
    },
];

/// Dropbear's key file (`-D /opt/tv-sshd`).
const AUTHORIZED_KEYS: &str = "/opt/tv-sshd/authorized_keys";
/// Where `tv-sdb push` puts a key to add. The directory is world-writable,
/// so the file is only read, never trusted: nothing is added until the
/// user confirms its fingerprints on the TV.
const KEY_DROP: &str = "/home/owner/share/tmp/sdk_tools/ssh-key.pub";
const MAX_KEY_DROP: u64 = 16 * 1024;

/// Permanent options: service apps that amd restarts, disabled in pkgmgr's
/// database with `pkgcmd -D --global`. That outlives a reboot and tv-blockd,
/// so `Changes::disabled` records each app tv-blockd disabled; switching the
/// option off enables and starts exactly those. Apps missing on a TV are
/// skipped.
struct Disable {
    key: &'static str,
    apps: &'static [&'static str],
    /// Closed once after the disable (nothing starts them again).
    close: &'static [&'static str],
}

const DISABLES: &[Disable] = &[
    Disable {
        // hbbtv-service relaunches the HbbTV app at boot and on source changes.
        key: "disable_hbbtv",
        apps: &["org.tizen.hbbtv-service"],
        close: &["org.tizen.hbbtv"],
    },
    Disable {
        key: "disable_acr",
        apps: &["com.samsung.tv.acr"],
        close: &[],
    },
    Disable {
        key: "disable_ads",
        apps: &["com.samsung.tv.targeted-ad-service"],
        close: &[],
    },
];

const PKGMGR_DB: &str = "/opt/dbspace/.pkgmgr_parser.db";

/// The one host Samsung's developer mode lets connect to SDB.
const SDB_HOST_KEY: &str = "db/sdk/develop/ip";

/// Mali's load over its last 20 ms DVFS window; one read is mostly 0.
const GPU_USAGE: &str = "/sys/class/misc/mali0/device/dvfs_usage";
/// Sum and count of GPU readings since the last `system` call, and until
/// when (unix seconds) the sampler runs: only while the System tab asks.
static GPU_SUM: AtomicU64 = AtomicU64::new(0);
static GPU_COUNT: AtomicU64 = AtomicU64::new(0);
static GPU_UNTIL: AtomicU64 = AtomicU64::new(0);

fn gpu_sampler() {
    loop {
        if unix_now() > GPU_UNTIL.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        if let Some(value) = fs::read_to_string(GPU_USAGE)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
        {
            GPU_SUM.fetch_add(value, Ordering::Relaxed);
            GPU_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Brake files older builds made under /opt, where they outlived a reboot;
/// removed once (`Changes::created` records which were tv-blockd's).
const PERSISTENT_PREFIX: &str = "/opt/";

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
    /// Running units of every service option, on or off.
    services: BTreeMap<String, Vec<String>>,
    /// Disabled apps of every permanent option, on or off.
    disabled: BTreeMap<String, Vec<String>>,
    /// Permanent options none of whose apps this TV has.
    missing: Vec<String>,
    /// `db/sdk/develop/ip`.
    sdb_host: Option<String>,
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
    /// Brake files (and directories made for them), in creation order.
    #[serde(default)]
    created: Vec<String>,
    /// Units that were running when tv-blockd stopped them.
    #[serde(default)]
    stopped: Vec<String>,
    /// Apps tv-blockd disabled in pkgmgr's database (permanent options).
    #[serde(default)]
    disabled: Vec<String>,
    /// What each option's processes used when tv-blockd last stopped them.
    #[serde(default)]
    freed: BTreeMap<String, Freed>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Freed {
    processes: usize,
    rss_kb: u64,
    swap_kb: u64,
    at: u64,
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
    /// Service options whose units this process has masked (and whose apps
    /// it has closed).
    masked: std::collections::BTreeSet<&'static str>,
    /// Service options restored since they were last switched off.
    restored: std::collections::BTreeSet<&'static str>,
    list_fetched: BTreeMap<String, Instant>,
    /// Namespaces already reported as failing, so each is logged once.
    reported_failures: std::collections::BTreeSet<String>,
    /// The System tab's previous reading, for rates.
    sampled: Option<Sample>,
}

pub fn main() {
    if let Err(error) = run() {
        // Client commands report to stderr; only the daemon's own failure
        // belongs in its log.
        if std::env::args().len() == 1 {
            log(&format!("fatal: {error}"));
        }
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
        ["sdb-host", host] => return client(json!({"method": "sdb_host", "host": host})),
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
                 \x20      tv-blockd sdb-host IP the host Samsung's developer mode lets use SDB\n\
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
        masked: Default::default(),
        restored: Default::default(),
        list_fetched: BTreeMap::new(),
        reported_failures: std::collections::BTreeSet::new(),
        sampled: None,
    };

    let _ = fs::remove_file(SOCKET);
    let listener = UnixListener::bind(SOCKET)?;
    fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o660))?;
    let path = CString::new(SOCKET).unwrap();
    unsafe { libc::chown(path.as_ptr(), 0, USERS_GID) };
    log("started");
    if Path::new(GPU_USAGE).exists() {
        std::thread::spawn(gpu_sampler);
    }

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

impl Daemon {
    fn serve(&mut self, stream: UnixStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let (uid, label) = tv_tizen::peer::credentials(&stream)?;
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
            Some("system") => self.system(),
            Some("ssh_add") => {
                let wanted: Vec<String> = request["fingerprints"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect();
                log(&format!("ssh_add by uid={uid} label={label}"));
                self.ssh_add(&wanted)
            }
            Some("ssh_reject") => {
                let _ = fs::remove_file(KEY_DROP);
                log(&format!("ssh key file rejected by uid={uid} label={label}"));
                self.status()
            }
            Some("ssh_remove") => match request["fingerprint"].as_str() {
                Some(fingerprint) => {
                    log(&format!(
                        "ssh_remove {fingerprint} by uid={uid} label={label}"
                    ));
                    self.ssh_remove(fingerprint)
                }
                None => json!({"status": "error", "error": "fingerprint missing"}),
            },
            Some("sdb_host") => match request["host"]
                .as_str()
                .and_then(|host| host.trim().parse::<std::net::Ipv4Addr>().ok())
            {
                Some(host) => {
                    log(&format!("sdb host {host} by uid={uid} label={label}"));
                    self.set_sdb_host(host)
                }
                None => json!({"status": "error", "error": "not an IPv4 address"}),
            },
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
                    airplay: false,
                    multiview_preload: false,
                    sdb_off: false,
                    ssh_off: false,
                    disable_hbbtv: false,
                    disable_acr: false,
                    disable_ads: false,
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
               "changes": self.changes, "groups": groups, "ssh": ssh_state()})
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
        self.reconcile_services(&mut report);
        self.reconcile_disables(&mut report);
        report.sdb_host = tv_tizen::vconf::get_str(SDB_HOST_KEY).ok();
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

    /// Removes the brake files tv-blockd made under `prefix`, newest first;
    /// directories go only if empty.
    fn remove_created(&mut self, prefix: &str) {
        let (ours, keep): (Vec<String>, Vec<String>) = self
            .changes
            .created
            .drain(..)
            .partition(|path| path.starts_with(prefix));
        self.changes.created = keep;
        for path in ours.iter().rev() {
            let _ = fs::remove_file(path).or_else(|_| fs::remove_dir(path));
        }
        if !ours.is_empty() {
            self.changes.save();
            log(&format!(
                "removed {} brake files and directories under {prefix}",
                ours.len()
            ));
        }
    }

    /// Unmasks a service option's units and starts the ones tv-blockd
    /// stopped. Processes it killed come back at the next boot.
    fn restore_services(&mut self, services: &Services) {
        let units: Vec<String> = services.units.iter().map(|unit| unit.to_string()).collect();
        for brake in services.brakes {
            if let Some(position) = self.changes.created.iter().position(|path| path == brake) {
                let _ = fs::remove_file(brake);
                self.changes.created.remove(position);
            }
        }
        if services.mask {
            let _ = systemctl(&["unmask", "--runtime"], &units);
        }
        let (start, keep): (Vec<String>, Vec<String>) = self
            .changes
            .stopped
            .drain(..)
            .partition(|unit| units.contains(unit));
        self.changes.stopped = keep;
        if !start.is_empty() {
            let _ = systemctl(&["start", "--no-block"], &start);
        }
        self.changes.save();
        self.masked.remove(services.key);
        log(&format!("{} restored", services.key));
    }

    fn reconcile_services(&mut self, report: &mut Report) {
        // Older builds braked telemetry with files under /opt, which outlived
        // a reboot without tv-blockd; runtime masks do the job now.
        self.remove_created(PERSISTENT_PREFIX);
        for services in SERVICES {
            let units: Vec<String> = services.units.iter().map(|unit| unit.to_string()).collect();
            if !self.config.service(services.key) {
                if self.restored.insert(services.key) {
                    self.restore_services(services);
                }
                report
                    .services
                    .insert(services.key.to_owned(), active_units(&units));
                continue;
            }
            self.restored.remove(services.key);
            for brake in services.brakes {
                if !Path::new(brake).exists() {
                    match fs::write(brake, b"") {
                        Ok(()) => {
                            self.changes.created.push(brake.to_string());
                            self.changes.save();
                        }
                        Err(error) => report.errors.push(format!("brake {brake}: {error}")),
                    }
                }
            }
            // Before closing apps: closing an app can end its unit too, and
            // the unit must still be recorded to be started again on restore.
            let active = active_units(&units);
            if self.masked.insert(services.key) {
                self.measure(services.key);
                // Runtime masks live in /run: re-applied after every boot.
                if services.mask {
                    if let Err(error) = systemctl(&["mask", "--runtime"], &units) {
                        report
                            .errors
                            .push(format!("mask {}: {error}", services.key));
                    }
                }
                for app in services.apps {
                    let _ = Command::new("/usr/bin/app_launcher")
                        .args(["-t", app])
                        .output();
                }
            }
            if !active.is_empty() {
                let _ = systemctl(&["stop", "--no-block"], &active);
                for unit in &active {
                    if !self.changes.stopped.contains(unit) {
                        self.changes.stopped.push(unit.clone());
                    }
                }
                self.changes.save();
                log(&format!("{}: stopped {}", services.key, active.join(" ")));
            }
            for pid in processes_named(services.processes) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
                log(&format!("{}: killed process {pid}", services.key));
            }
            report.services.insert(services.key.to_owned(), active);
        }
        report.telemetry_active = report
            .services
            .get("telemetry")
            .cloned()
            .unwrap_or_default();
    }
}

impl Daemon {
    fn reconcile_disables(&mut self, report: &mut Report) {
        let mut states = app_disabled(DISABLES.iter().flat_map(|disable| disable.apps));
        for disable in DISABLES {
            let present: Vec<&str> = disable
                .apps
                .iter()
                .copied()
                .filter(|app| states.contains_key(*app))
                .collect();
            if present.is_empty() {
                report.missing.push(disable.key.to_owned());
                continue;
            }
            let wanted = self.config.service(disable.key);
            let mut closed = false;
            for app in present {
                let disabled = states[app];
                let ours = self.changes.disabled.iter().any(|known| known == app);
                if wanted && !disabled {
                    if !closed {
                        self.measure(disable.key);
                    }
                    match pkgcmd("-D", app) {
                        Ok(()) => {
                            states.insert(app.to_owned(), true);
                            self.changes.disabled.push(app.to_owned());
                            self.changes.save();
                            log(&format!("{}: disabled {app} (permanent)", disable.key));
                            closed = true;
                        }
                        Err(error) => report.errors.push(error.to_string()),
                    }
                } else if !wanted && disabled && ours {
                    match pkgcmd("-A", app) {
                        Ok(()) => {
                            states.insert(app.to_owned(), false);
                            self.changes.disabled.retain(|known| known != app);
                            self.changes.save();
                            let _ = Command::new("/usr/bin/app_launcher")
                                .args(["-s", app])
                                .output();
                            log(&format!("{}: enabled and started {app}", disable.key));
                        }
                        Err(error) => report.errors.push(error.to_string()),
                    }
                }
            }
            if closed {
                for app in disable.apps.iter().chain(disable.close) {
                    let _ = Command::new("/usr/bin/app_launcher")
                        .args(["-t", app])
                        .output();
                }
            }
            let disabled = disable
                .apps
                .iter()
                .filter(|app| states.get(**app) == Some(&true))
                .map(|app| app.to_string())
                .collect();
            report.disabled.insert(disable.key.to_owned(), disabled);
        }
    }
}

/// One reading of the counters behind the System tab.
struct Sample {
    at: Instant,
    /// `/proc/stat`: all ticks, idle ticks.
    cpu: (u64, u64),
    ticks: BTreeMap<i32, u64>,
    /// Sectors read and written.
    disk: (u64, u64),
    /// Pages swapped in and out.
    swap: (u64, u64),
    /// NPU (Trinity) interrupts so far.
    npu: Option<u64>,
}

struct Process {
    pid: i32,
    comm: String,
    ticks: u64,
    rss_kb: u64,
    swap_kb: u64,
}

fn processes() -> Vec<Process> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let (comm, ticks) =
                sysinfo::process_ticks(&fs::read_to_string(entry.path().join("stat")).ok()?)?;
            let status = fs::read_to_string(entry.path().join("status")).unwrap_or_default();
            Some(Process {
                pid,
                comm,
                ticks,
                rss_kb: sysinfo::kb(&status, "VmRSS"),
                swap_kb: sysinfo::kb(&status, "VmSwap"),
            })
        })
        .collect()
}

fn counters() -> ((u64, u64), (u64, u64), (u64, u64)) {
    let read = |path: &str| fs::read_to_string(path).unwrap_or_default();
    let vmstat = read("/proc/vmstat");
    (
        sysinfo::cpu_ticks(&read("/proc/stat")).unwrap_or((0, 0)),
        sysinfo::disk_sectors(&read("/proc/diskstats")),
        (
            sysinfo::vmstat(&vmstat, "pswpin"),
            sysinfo::vmstat(&vmstat, "pswpout"),
        ),
    )
}

fn running_apps() -> Vec<(String, i32)> {
    Command::new("/usr/bin/app_launcher")
        .arg("-S")
        .output()
        .map(|output| sysinfo::running_apps(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

/// What makes up an option: units, apps and process names.
struct Parts {
    key: &'static str,
    units: &'static [&'static str],
    apps: Vec<&'static str>,
    names: &'static [&'static str],
}

fn option_parts() -> Vec<Parts> {
    let services = SERVICES.iter().map(|services| Parts {
        key: services.key,
        units: services.units,
        apps: services.apps.to_vec(),
        names: services.processes,
    });
    let disables = DISABLES.iter().map(|disable| Parts {
        key: disable.key,
        units: &[],
        apps: disable.apps.iter().chain(disable.close).copied().collect(),
        names: &[],
    });
    services.chain(disables).collect()
}

/// The pids of an option: its units' cgroups, its apps, its process names.
fn option_pids(
    parts: &Parts,
    running: &[(String, i32)],
    processes: &[Process],
) -> std::collections::BTreeSet<i32> {
    let mut pids = std::collections::BTreeSet::new();
    for unit in parts.units {
        let path = format!("/sys/fs/cgroup/systemd/system.slice/{unit}/cgroup.procs");
        if let Ok(text) = fs::read_to_string(path) {
            pids.extend(
                text.lines()
                    .filter_map(|line| line.trim().parse::<i32>().ok()),
            );
        }
    }
    pids.extend(
        running
            .iter()
            .filter(|(app, _)| parts.apps.contains(&app.as_str()))
            .map(|(_, pid)| *pid),
    );
    pids.extend(
        processes
            .iter()
            .filter(|process| parts.names.contains(&process.comm.as_str()))
            .map(|process| process.pid),
    );
    pids
}

/// The keys in `KEY_DROP`; `None` when there is no such file.
fn read_drop() -> Result<Option<Vec<tv_blockd::ssh::Key>>, String> {
    use std::io::Read;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(KEY_DROP);
    let mut file = match file {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("unreadable ({error})")),
        Ok(file) => file,
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_KEY_DROP {
        return Err("not a small regular file".into());
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|_| "not text".to_owned())?;
    let keys = tv_blockd::ssh::parse(&text);
    if keys.is_empty() {
        return Err("no public key in it".into());
    }
    Ok(Some(keys))
}

fn ssh_state() -> Value {
    let describe = |key: &tv_blockd::ssh::Key| json!({"kind": key.kind, "comment": key.comment, "fingerprint": key.fingerprint});
    let keys = fs::read_to_string(AUTHORIZED_KEYS)
        .map(|text| tv_blockd::ssh::parse(&text))
        .unwrap_or_default();
    let (pending, pending_error) = match read_drop() {
        Ok(keys) => (keys.unwrap_or_default(), None),
        Err(error) => (Vec::new(), Some(error)),
    };
    json!({"keys": keys.iter().map(describe).collect::<Vec<_>>(),
           "pending": pending.iter().map(describe).collect::<Vec<_>>(),
           "pending_error": pending_error,
           "drop": KEY_DROP})
}

fn write_authorized_keys(text: &str) -> io::Result<()> {
    let temporary = format!("{AUTHORIZED_KEYS}.tmp");
    fs::write(&temporary, text)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, AUTHORIZED_KEYS)
}

impl Daemon {
    /// Adds the pushed keys whose fingerprints the user confirmed (the file
    /// may have changed since it was shown), then deletes the file.
    fn ssh_add(&mut self, fingerprints: &[String]) -> Value {
        let pushed = match read_drop() {
            Ok(Some(keys)) => keys,
            Ok(None) => return json!({"status": "error", "error": "no key file pushed"}),
            Err(error) => return json!({"status": "error", "error": format!("key file {error}")}),
        };
        let chosen: Vec<_> = pushed
            .into_iter()
            .filter(|key| fingerprints.contains(&key.fingerprint))
            .collect();
        if chosen.is_empty() {
            return json!({"status": "error", "error": "the key file changed; check it again"});
        }
        let existing = fs::read_to_string(AUTHORIZED_KEYS).unwrap_or_default();
        if let Err(error) = write_authorized_keys(&tv_blockd::ssh::add(&existing, &chosen)) {
            return json!({"status": "error", "error": format!("{AUTHORIZED_KEYS}: {error}")});
        }
        let _ = fs::remove_file(KEY_DROP);
        for key in &chosen {
            log(&format!(
                "ssh key added: {} {}",
                key.fingerprint, key.comment
            ));
        }
        self.status()
    }

    /// Removes a key, but never the last one while SSH is on.
    fn ssh_remove(&mut self, fingerprint: &str) -> Value {
        let existing = fs::read_to_string(AUTHORIZED_KEYS).unwrap_or_default();
        if !self.config.ssh_off && tv_blockd::ssh::parse(&existing).len() <= 1 {
            return json!({"status": "error",
                          "error": "the last key stays while SSH is on; add another or switch SSH off first"});
        }
        let Some(text) = tv_blockd::ssh::remove(&existing, fingerprint) else {
            return json!({"status": "error", "error": "no such key"});
        };
        if let Err(error) = write_authorized_keys(&text) {
            return json!({"status": "error", "error": format!("{AUTHORIZED_KEYS}: {error}")});
        }
        log(&format!("ssh key removed: {fingerprint}"));
        self.status()
    }

    /// Samsung's developer-mode host, as its own dialog sets it; SDB is
    /// restarted (if running) to pick it up. Not part of `undo`: it is the
    /// TV's setting, not one of tv-blockd's changes.
    fn set_sdb_host(&mut self, host: std::net::Ipv4Addr) -> Value {
        if let Err(error) = tv_tizen::vconf::set_str(SDB_HOST_KEY, &host.to_string()) {
            return json!({"status": "error", "error": format!("{SDB_HOST_KEY}: {error}")});
        }
        let _ = systemctl(&["try-restart"], &["sdbd.service".to_owned()]);
        self.report.sdb_host = tv_tizen::vconf::get_str(SDB_HOST_KEY).ok();
        self.status()
    }

    /// Records what an option's processes use, just before they are
    /// stopped; nothing running keeps the previous record.
    fn measure(&mut self, key: &str) {
        let Some(parts) = option_parts().into_iter().find(|parts| parts.key == key) else {
            return;
        };
        let processes = processes();
        let pids = option_pids(&parts, &running_apps(), &processes);
        let mine = processes
            .iter()
            .filter(|process| pids.contains(&process.pid));
        let (rss_kb, swap_kb) = mine.fold((0, 0), |(rss, swap), process| {
            (rss + process.rss_kb, swap + process.swap_kb)
        });
        if rss_kb + swap_kb == 0 {
            return;
        }
        log(&format!(
            "{key}: {} processes used {rss_kb} kB + {swap_kb} kB swap",
            pids.len()
        ));
        self.changes.freed.insert(
            key.to_owned(),
            Freed {
                processes: pids.len(),
                rss_kb,
                swap_kb,
                at: unix_now(),
            },
        );
        self.changes.save();
    }

    /// The System tab's snapshot: CPU, memory, GPU and IO, the busiest and
    /// largest processes, and what each option's processes use now (or
    /// freed when stopped). Rates are since the previous call, or over
    /// half a second when that is stale.
    fn system(&mut self) -> Value {
        let take = |processes: &[Process]| {
            let (cpu, disk, swap) = counters();
            Sample {
                at: Instant::now(),
                cpu,
                ticks: processes.iter().map(|p| (p.pid, p.ticks)).collect(),
                disk,
                swap,
                npu: fs::read_to_string("/proc/interrupts")
                    .ok()
                    .and_then(|text| sysinfo::interrupts(&text, "triv2")),
            }
        };
        GPU_UNTIL.store(unix_now() + 10, Ordering::Relaxed);
        let before = match self.sampled.take() {
            Some(sample) if sample.at.elapsed() < Duration::from_secs(30) => sample,
            _ => {
                let sample = take(&processes());
                std::thread::sleep(Duration::from_millis(500));
                sample
            }
        };
        let processes = processes();
        let now = take(&processes);
        let seconds = now.at.duration_since(before.at).as_secs_f64().max(0.1);
        let total = now.cpu.0.saturating_sub(before.cpu.0).max(1) as f64;
        let idle = now.cpu.1.saturating_sub(before.cpu.1) as f64;
        let share = |process: &Process| {
            let was = before
                .ticks
                .get(&process.pid)
                .copied()
                .unwrap_or(process.ticks);
            100.0 * process.ticks.saturating_sub(was) as f64 / total
        };
        let rate = |now: u64, was: u64, kb_per_unit: f64| {
            (now.saturating_sub(was) as f64 * kb_per_unit / seconds).round()
        };
        let running = running_apps();
        let names: BTreeMap<i32, &str> = running
            .iter()
            .map(|(app, pid)| (*pid, app.as_str()))
            .collect();
        let entry = |process: &Process| {
            json!({"pid": process.pid,
                   "name": names.get(&process.pid).copied().unwrap_or(&process.comm),
                   "cpu": (share(process) * 10.0).round() / 10.0,
                   "rss_kb": process.rss_kb, "swap_kb": process.swap_kb})
        };
        let mut by_cpu: Vec<&Process> = processes.iter().collect();
        by_cpu.sort_by(|a, b| share(b).total_cmp(&share(a)));
        let mut by_memory: Vec<&Process> = processes.iter().collect();
        by_memory.sort_by_key(|p| std::cmp::Reverse(p.rss_kb + p.swap_kb));
        let options: serde_json::Map<String, Value> = option_parts()
            .iter()
            .map(|parts| {
                let pids = option_pids(parts, &running, &processes);
                let mine: Vec<&Process> = processes
                    .iter()
                    .filter(|process| pids.contains(&process.pid))
                    .collect();
                let cpu: f64 = mine.iter().map(|process| share(process)).sum();
                (
                    parts.key.to_owned(),
                    json!({"on": self.config.service(parts.key),
                           "processes": mine.len(),
                           "cpu": (cpu * 10.0).round() / 10.0,
                           "rss_kb": mine.iter().map(|p| p.rss_kb).sum::<u64>(),
                           "swap_kb": mine.iter().map(|p| p.swap_kb).sum::<u64>(),
                           "freed": self.changes.freed.get(parts.key)}),
                )
            })
            .collect();
        let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let count = GPU_COUNT.swap(0, Ordering::Relaxed);
        let sum = GPU_SUM.swap(0, Ordering::Relaxed);
        let gpu = (count > 0).then(|| (sum as f64 / count as f64).round() as u64);
        let npu = now
            .npu
            .zip(before.npu)
            .map(|(now, was)| (now.saturating_sub(was) as f64 / seconds).round());
        let system = json!({
            "cores": fs::read_to_string("/proc/cpuinfo")
                .map(|text| text.lines().filter(|line| line.starts_with("processor")).count())
                .unwrap_or(0),
            "cpu": (1000.0 * (1.0 - idle / total)).round() / 10.0,
            "load": fs::read_to_string("/proc/loadavg")
                .map(|text| text.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
                .unwrap_or_default(),
            "mem_total_kb": sysinfo::kb(&meminfo, "MemTotal"),
            "mem_available_kb": sysinfo::kb(&meminfo, "MemAvailable"),
            "swap_total_kb": sysinfo::kb(&meminfo, "SwapTotal"),
            "swap_free_kb": sysinfo::kb(&meminfo, "SwapFree"),
            "gpu": gpu,
            "npu_per_s": npu,
            "disk_read_kbs": rate(now.disk.0, before.disk.0, 0.5),
            "disk_write_kbs": rate(now.disk.1, before.disk.1, 0.5),
            "swap_in_kbs": rate(now.swap.0, before.swap.0, 4.0),
            "swap_out_kbs": rate(now.swap.1, before.swap.1, 4.0),
            "processes": processes.len(),
            "top_cpu": by_cpu.iter().take(8).map(|p| entry(p)).collect::<Vec<_>>(),
            "top_memory": by_memory.iter().take(8).map(|p| entry(p)).collect::<Vec<_>>(),
            "options": options,
            "ssh": ssh_state(),
        });
        self.sampled = Some(now);
        json!({"status": "ok", "system": system})
    }
}

/// `app_disable` of each of `apps` that pkgmgr knows, read with the TV's
/// sqlite3 (the ids are compile-time constants).
fn app_disabled<'a>(apps: impl Iterator<Item = &'a &'a str>) -> BTreeMap<String, bool> {
    let list: Vec<String> = apps.map(|app| format!("'{app}'")).collect();
    let query = format!(
        "SELECT app_id, app_disable FROM package_app_info WHERE app_id IN ({})",
        list.join(",")
    );
    let Ok(output) = Command::new("/usr/bin/sqlite3")
        .args(["-readonly", PKGMGR_DB, &query])
        .output()
    else {
        return BTreeMap::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('|'))
        .map(|(app, disabled)| (app.to_owned(), disabled == "true"))
        .collect()
}

/// `pkgcmd <flag> -t app -n <app> --global`, given 20 s: the package-level
/// form hangs when run as root, so a hang must not stall the daemon.
fn pkgcmd(flag: &str, app: &str) -> io::Result<()> {
    let mut child = Command::new("/usr/bin/pkgcmd")
        .args([flag, "-t", "app", "-n", app, "--global"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    for _ in 0..200 {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("pkgcmd {flag} {app}: {status}")))
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::other(format!("pkgcmd {flag} {app}: timed out")))
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
