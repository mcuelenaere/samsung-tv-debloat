use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs;
use std::io::{self, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tv_patchd::http::{self, Script};
use tv_patchd::row::{self, Row};
use tv_tizen::tls;

const CONFIG: &str = "/opt/tv-suite/etc/patchd.json";
const STATE: &str = "/opt/tv-suite/var/patchd";
const SCRIPT_INFO: &str = "/opt/tv-suite/var/patchd/script.json";
const LEAF_CERT: &str = "/opt/tv-suite/var/patchd/leaf.crt";
const LEAF_KEY: &str = "/opt/tv-suite/var/patchd/leaf.key";
const RUN_DIR: &str = "/run/tv-suite";
const SOCKET: &str = "/run/tv-suite/patchd.sock";
const LOG: &str = "/run/tv-suite/patchd.log";
const MAX_LOG_BYTES: u64 = 256 * 1024;
const CA_BUNDLE: &str = "/etc/ssl/ca-bundle.pem";
const COBALT_APP: &str = "com.samsung.tv.cobalt-yt";
/// The suite's dock, `org.tvauth.ui`'s second app.
const DOCK_APP: &str = "org.tvauth.home";
/// Samsung Home's row and source caches. Only Home's own SMACK label may read
/// them, so while Custom Home is on they are mirrored for the dock as
/// `/run/tv-suite/home-row` and `home-sources`.
const HOME_DATA: &str = "/opt/usr/home/owner/apps_rw/com.samsung.tv.home.media/data";
const MIRRORS: &[&str] = &["/run/tv-suite/home-row", "/run/tv-suite/home-sources"];
const SHARED_LABEL: &str = "User::App::Shared";
static MIRROR_LOCK: Mutex<()> = Mutex::new(());

const UI_LABEL: &str = "User::Pkg::org.tvauth.ui";
const UI_UID: u32 = 5001;
const USERS_GID: u32 = 100;

const TICK: Duration = Duration::from_secs(60);
/// Certificates and the content copy are rechecked this often.
const SETUP_EVERY: Duration = Duration::from_secs(6 * 3600);
const MAX_CONNECTIONS: usize = 256;
const IDLE: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Opened and rewritten; every other host is a plain tunnel.
const MITM_HOSTS: &[&str] = &["www.youtube.com", "youtube.com"];

/// The release pinned by this build (the one Vetro's plugin pins).
const PINNED_VERSION: &str = "1.15.2";
const PINNED_SHA256: &str = "7a092351e0e348507ef9c97da8833d5f6717de628517f5299053b3c2073f1d69";
const REGISTRY_LATEST: &str = "https://registry.npmjs.org/@foxreis/tizentube/latest";
const MAX_SCRIPT: usize = 3 << 20;

fn script_url(version: &str) -> String {
    format!("https://cdn.jsdelivr.net/npm/@foxreis/tizentube@{version}/dist/userScript.js")
}

fn script_file(version: &str) -> String {
    format!("{STATE}/tizentube-{version}.js")
}

/// Makes the CA (root-only key), the www.youtube.com certificate, and the
/// copy of Cobalt's content folder that trusts the CA. Prints `ready` or
/// `nocobalt`. The share folder is world-writable, so the copy is made in a
/// fresh directory (`mkdir` fails on anything planted there) and moved with
/// `mv -T`.
const SETUP: &str = r#"set -e
umask 022
D=/opt/tv-suite/var/patchd
C=/opt/usr/home/owner/share/tvs
SRC=/usr/apps/com.samsung.tv.cobalt/content/app/cobalt/content
LIB=/usr/apps/com.samsung.tv.cobalt/content/app/cobalt/lib/libcobalt.so
[ -d "$SRC/ssl/certs" ] || { echo nocobalt; exit 0; }
mkdir -p "$D"; chmod 700 "$D"; cd "$D"
if [ ! -f ca.key ] || ! openssl x509 -in ca.crt -noout -checkend 2592000 >/dev/null 2>&1; then
  rm -f ca.key ca.crt ca.srl leaf.key leaf.crt leaf.csr content.stamp
  printf '%s\n' '[req]' 'distinguished_name=dn' 'x509_extensions=v3' 'prompt=no' '[dn]' 'CN=tv-suite YouTube CA' '[v3]' 'basicConstraints=critical,CA:TRUE' 'keyUsage=critical,keyCertSign,cRLSign' 'subjectKeyIdentifier=hash' > ca.cnf
  openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 3650 -sha256 -config ca.cnf >/dev/null 2>&1
fi
if [ ! -f leaf.crt ] || ! openssl x509 -in leaf.crt -noout -checkend 2592000 >/dev/null 2>&1 || ! openssl verify -CAfile ca.crt leaf.crt >/dev/null 2>&1; then
  openssl req -new -newkey rsa:2048 -nodes -keyout leaf.key -subj /CN=www.youtube.com -out leaf.csr >/dev/null 2>&1
  printf '%s\n' 'subjectAltName=DNS:www.youtube.com,DNS:youtube.com' 'basicConstraints=CA:FALSE' 'keyUsage=digitalSignature,keyEncipherment' 'extendedKeyUsage=serverAuth' > leaf.ext
  openssl x509 -req -in leaf.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out leaf.crt -days 397 -sha256 -extfile leaf.ext >/dev/null 2>&1
fi
chmod 600 ca.key leaf.key
STAMP="$(stat -c %Y "$LIB" 2>/dev/null)-$(openssl x509 -in ca.crt -noout -fingerprint -sha256)"
if [ -L "$C" ] || [ ! -d "$C" ] || [ ! -O "$C" ] || [ "$(cat content.stamp 2>/dev/null)" != "$STAMP" ]; then
  rm -rf "$C.new"; mkdir "$C.new"
  cp -R "$SRC/." "$C.new/"
  cp ca.crt "$C.new/ssl/certs/$(openssl x509 -in ca.crt -noout -subject_hash).0"
  cp ca.crt "$C.new/ssl/certs/$(openssl x509 -in ca.crt -noout -subject_hash_old).0"
  rm -rf "$C"; mv -T "$C.new" "$C"
  echo "$STAMP" > content.stamp
fi
echo ready
"#;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Config {
    /// TizenTube in Samsung's YouTube.
    #[serde(default)]
    tizentube: bool,
    /// The Home key opens the suite's dock instead of Samsung's Home.
    #[serde(default)]
    custom_home: bool,
}

/// The TizenTube release in use.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScriptInfo {
    version: String,
    sha256: String,
}

impl Default for ScriptInfo {
    fn default() -> Self {
        Self {
            version: PINNED_VERSION.into(),
            sha256: PINNED_SHA256.into(),
        }
    }
}

/// What the proxy threads read.
struct Shared {
    /// Our www.youtube.com certificate; `None` = tunnel only.
    server: RwLock<Option<Arc<tls::Context>>>,
    script: RwLock<Option<Arc<Script>>>,
    client: tls::Context,
    connections: AtomicUsize,
    tunnels: AtomicUsize,
    opened: AtomicUsize,
    pages: AtomicUsize,
}

#[derive(Debug, Default, Serialize)]
struct Report {
    at: u64,
    /// Certificates, content copy and script ready, proxy patching.
    ready: bool,
    row: String,
    /// The app the Home key launches.
    home: String,
    errors: Vec<String>,
}

struct Daemon {
    config: Config,
    /// `config.custom_home`, for the mirror thread.
    mirroring: Arc<AtomicBool>,
    script: ScriptInfo,
    shared: Arc<Shared>,
    report: Report,
    setup_at: Option<Instant>,
}

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

fn save_json(path: &str, value: &impl Serialize) -> io::Result<()> {
    let temporary = format!("{path}.tmp");
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(&temporary, bytes)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, path)
}

fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &str) -> T {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn main() {
    if let Err(error) = run() {
        log(&format!("fatal: {error}"));
        eprintln!("tv-patchd: {error}");
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
        [method @ ("status" | "undo" | "update")] => return client(json!({"method": method})),
        ["set", config] => {
            let config: Value = serde_json::from_str(config)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            return client(json!({"method": "set", "config": config}));
        }
        _ => {
            println!(
                "usage: tv-patchd            run the daemon\n\
                 \x20      tv-patchd status     print config and state\n\
                 \x20      tv-patchd set JSON   replace the config, e.g. '{{\"tizentube\":true}}'\n\
                 \x20      tv-patchd undo       switch off and restore YouTube\n\
                 \x20      tv-patchd update     install the latest TizenTube release\n\
                 config: {CONFIG}; control socket: {SOCKET}"
            );
            return Ok(());
        }
    }
    fs::create_dir_all(RUN_DIR)?;
    fs::create_dir_all(Path::new(CONFIG).parent().unwrap())?;
    fs::create_dir_all(STATE)?;
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o700))?;
    let shared = Arc::new(Shared {
        server: RwLock::new(None),
        script: RwLock::new(None),
        client: tls::Context::client(CA_BUNDLE).map_err(io::Error::other)?,
        connections: AtomicUsize::new(0),
        tunnels: AtomicUsize::new(0),
        opened: AtomicUsize::new(0),
        pages: AtomicUsize::new(0),
    });
    let proxy = shared.clone();
    thread::spawn(move || serve_proxy(proxy));

    let mirroring = Arc::new(AtomicBool::new(false));
    let watching = mirroring.clone();
    thread::spawn(move || mirror_changes(&watching));

    let mut daemon = Daemon {
        config: load_json(CONFIG),
        mirroring,
        script: load_json(SCRIPT_INFO),
        shared,
        report: Report::default(),
        setup_at: None,
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
            Some("status") => self.status(None),
            Some("set") => match serde_json::from_value::<Config>(request["config"].clone()) {
                Ok(config) => {
                    log(&format!("config set by uid={uid} label={label}"));
                    self.apply(config)
                }
                Err(error) => json!({"status": "error", "error": error.to_string()}),
            },
            Some("undo") => {
                log(&format!("undo by uid={uid} label={label}"));
                self.apply(Config::default())
            }
            Some("update") => {
                let message = self.update_script().unwrap_or_else(|error| {
                    log(&format!("update: {error}"));
                    format!("TizenTube update failed: {error}")
                });
                self.status(Some(&message))
            }
            _ => json!({"status": "error", "error": "unknown method"}),
        };
        tv_proto::write_json(&mut writer, &response)
    }

    fn apply(&mut self, config: Config) -> Value {
        let youtube_changed = config.tizentube != self.config.tizentube;
        self.config = config;
        if let Err(error) = save_json(CONFIG, &self.config) {
            log(&format!("config save: {error}"));
        }
        self.reconcile();
        if youtube_changed {
            // A running YouTube keeps its old flags until it restarts.
            let _ = Command::new("/usr/bin/app_launcher")
                .args(["-t", COBALT_APP])
                .output();
        }
        self.status(None)
    }

    fn status(&self, message: Option<&str>) -> Value {
        let shared = &self.shared;
        json!({
            "status": "ok",
            "config": self.config,
            "report": self.report,
            "script": self.script,
            "pinned": PINNED_VERSION,
            "message": message,
            "proxy": {
                "address": row::PROXY,
                "connections": shared.connections.load(Ordering::Relaxed),
                "tunnels": shared.tunnels.load(Ordering::Relaxed),
                "opened": shared.opened.load(Ordering::Relaxed),
                "pages": shared.pages.load(Ordering::Relaxed),
            },
        })
    }

    fn reconcile(&mut self) {
        let mut report = Report {
            at: unix_now(),
            ..Report::default()
        };
        if self.config.tizentube {
            report.ready = self.prepare(&mut report);
        } else {
            *self.shared.server.write().unwrap() = None;
            self.setup_at = None;
        }
        report.row = self.reconcile_row(report.ready, &mut report).to_owned();
        self.mirroring
            .store(self.config.custom_home, Ordering::Relaxed);
        if let Err(error) = mirror_home(self.config.custom_home) {
            report.errors.push(format!("mirror Home's caches: {error}"));
        }
        report.home = self.reconcile_home(&mut report);
        if !self.config.tizentube {
            remove_content(&mut report);
        }
        for error in &report.errors {
            log(error);
        }
        if report.row != self.report.row
            || report.ready != self.report.ready
            || report.home != self.report.home
        {
            log(&format!(
                "ready={} row={} home={}",
                report.ready, report.row, report.home
            ));
        }
        self.report = report;
    }

    /// Point the Home key at the dock while `custom_home` is on (and the
    /// dock is installed), back at the firmware's default otherwise. Returns
    /// the app the Home key launches.
    fn reconcile_home(&self, report: &mut Report) -> String {
        let current = match tv_tizen::app::home_app() {
            Ok(current) => current,
            Err(error) => {
                report.errors.push(format!("Home app: {error}"));
                return String::new();
            }
        };
        let result = if self.config.custom_home && current != DOCK_APP {
            let installed = tv_tizen::pkgdb::query_text(
                tv_tizen::pkgdb::PKGMGR_PARSER_DB,
                "SELECT app_id FROM package_app_info WHERE app_id = ?1",
                &[DOCK_APP],
            )
            .is_ok_and(|rows| !rows.is_empty());
            if !installed {
                report.errors.push(format!("{DOCK_APP} is not installed"));
                return current;
            }
            tv_tizen::app::set_home_app(DOCK_APP)
        } else if !self.config.custom_home && current == DOCK_APP {
            tv_tizen::app::reset_home_app()
        } else {
            return current;
        };
        match result.and_then(|()| tv_tizen::app::home_app()) {
            Ok(now) => {
                log(&format!("Home key: {current} -> {now}"));
                now
            }
            Err(error) => {
                report.errors.push(format!("Home app: {error}"));
                current
            }
        }
    }

    /// Certificates, content copy, script and TLS server; whether all are
    /// ready.
    fn prepare(&mut self, report: &mut Report) -> bool {
        if self.setup_at.is_none_or(|at| at.elapsed() >= SETUP_EVERY) {
            match Command::new("/bin/sh").args(["-c", SETUP]).output() {
                Ok(output) if output.stdout.starts_with(b"ready") => {
                    self.setup_at = Some(Instant::now());
                    *self.shared.server.write().unwrap() = None; // reload the leaf
                }
                Ok(output) if output.stdout.starts_with(b"nocobalt") => {
                    report
                        .errors
                        .push("Samsung's YouTube (Cobalt) is not installed".into());
                    return false;
                }
                Ok(output) => {
                    report.errors.push(format!(
                        "setup failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                    return false;
                }
                Err(error) => {
                    report.errors.push(format!("setup: {error}"));
                    return false;
                }
            }
        }
        if self.shared.server.read().unwrap().is_none() {
            match tls::Context::server(LEAF_CERT, LEAF_KEY) {
                Ok(context) => *self.shared.server.write().unwrap() = Some(Arc::new(context)),
                Err(error) => {
                    report.errors.push(format!("certificate: {error}"));
                    return false;
                }
            }
        }
        let loaded = self
            .shared
            .script
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|script| script.version == self.script.version);
        if !loaded {
            match ensure_script(&self.script) {
                Ok(body) => {
                    *self.shared.script.write().unwrap() = Some(Arc::new(Script {
                        version: self.script.version.clone(),
                        body,
                    }))
                }
                Err(error) => {
                    report
                        .errors
                        .push(format!("TizenTube {}: {error}", self.script.version));
                    return false;
                }
            }
        }
        true
    }

    /// Point Samsung's YouTube button at the proxy when `on`, at Samsung's
    /// own flags otherwise. Only our value, Samsung's, or Samsung's with
    /// another proxy (Vetro's leftover) are ever replaced.
    fn reconcile_row(&mut self, on: bool, report: &mut Report) -> &'static str {
        let Some(stock) = fs::read_to_string(row::MANIFEST)
            .ok()
            .and_then(|xml| row::manifest_userdata(&xml))
        else {
            return "no-manifest";
        };
        let current = match tv_tizen::pkgdb::query_text(row::DATABASE, row::SELECT, &[row::WRAPPER])
        {
            Ok(rows) if rows.len() == 1 => rows.into_iter().next().unwrap(),
            Ok(rows) if rows.is_empty() => return "absent",
            Ok(_) => return "duplicate",
            Err(error) => {
                report.errors.push(format!("read row: {error}"));
                return "error";
            }
        };
        let state = row::classify(&current, &stock);
        let target = match (on, state) {
            (true, Row::Stock | Row::OtherProxy) => row::patched(&stock),
            (false, Row::Ours) => Some(stock.clone()),
            _ => None,
        };
        let Some(target) = target else {
            if on && state == Row::Stock {
                report
                    .errors
                    .push("YouTube's flags leave no room for the proxy".into());
            }
            return state.name();
        };
        match write_row(&current, &target) {
            Ok(()) => {
                log(&format!("YouTube flags: {target}"));
                row::classify(&target, &stock).name()
            }
            Err(error) => {
                report.errors.push(format!("write row: {error}"));
                state.name()
            }
        }
    }

    /// Install the latest TizenTube release from npm.
    fn update_script(&mut self) -> Result<String, String> {
        let latest: Value =
            serde_json::from_slice(&tv_tizen::http::get(REGISTRY_LATEST, 1 << 20, 30)?)
                .map_err(|error| format!("npm registry: {error}"))?;
        let version = latest["version"].as_str().unwrap_or_default().to_owned();
        if version.is_empty()
            || version.len() > 32
            || !version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        {
            return Err(format!("unexpected version {version:?}"));
        }
        if version == self.script.version {
            return Ok(format!("TizenTube {version} is the latest release."));
        }
        // ponytail: trust on download (TLS to jsdelivr); npm only publishes a
        // hash of the package tarball, not of this file.
        let body = tv_tizen::http::get(&script_url(&version), MAX_SCRIPT, 60)?;
        let info = ScriptInfo {
            version: version.clone(),
            sha256: format!("{:x}", Sha256::digest(&body)),
        };
        fs::write(script_file(&version), &body).map_err(|error| error.to_string())?;
        save_json(SCRIPT_INFO, &info).map_err(|error| error.to_string())?;
        let old = std::mem::replace(&mut self.script, info);
        if old.version != version {
            let _ = fs::remove_file(script_file(&old.version));
        }
        log(&format!("TizenTube {} -> {version}", old.version));
        self.reconcile();
        Ok(format!(
            "TizenTube updated to {version}. Restart YouTube to use it."
        ))
    }
}

/// The script's bytes, downloaded once and checked against its SHA-256 on
/// every load.
fn ensure_script(info: &ScriptInfo) -> Result<Vec<u8>, String> {
    let path = script_file(&info.version);
    let matches = |body: &[u8]| format!("{:x}", Sha256::digest(body)) == info.sha256;
    if let Ok(body) = fs::read(&path) {
        if matches(&body) {
            return Ok(body);
        }
    }
    let body = tv_tizen::http::get(&script_url(&info.version), MAX_SCRIPT, 60)?;
    if !matches(&body) {
        return Err("downloaded file does not match its SHA-256; not used".into());
    }
    fs::write(&path, &body).map_err(|error| error.to_string())?;
    Ok(body)
}

fn smack_label(path: &str) -> Option<Vec<u8>> {
    let path = CString::new(path).ok()?;
    let name = c"security.SMACK64";
    let mut buffer = [0_u8; 256];
    let length = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    (length > 0).then(|| buffer[..length as usize].to_vec())
}

/// Compare-and-swap the YouTube row, then give the database and its journal
/// back the owner and SMACK label webappservice expects.
fn write_row(from: &str, to: &str) -> Result<(), String> {
    let metadata = fs::metadata(row::DATABASE).map_err(|error| error.to_string())?;
    let label = smack_label(row::DATABASE);
    let changed = tv_tizen::pkgdb::execute(row::DATABASE, row::UPDATE, &[to, row::WRAPPER, from])?;
    for path in [
        row::DATABASE.to_owned(),
        format!("{}-journal", row::DATABASE),
    ] {
        let Ok(c_path) = CString::new(path.as_str()) else {
            continue;
        };
        if !Path::new(&path).exists() {
            continue;
        }
        unsafe { libc::chown(c_path.as_ptr(), metadata.uid(), metadata.gid()) };
        if let Some(label) = &label {
            unsafe {
                libc::setxattr(
                    c_path.as_ptr(),
                    c"security.SMACK64".as_ptr(),
                    label.as_ptr().cast(),
                    label.len(),
                    0,
                )
            };
        }
    }
    if changed == 1 {
        Ok(())
    } else {
        Err("the row changed while it was being written".into())
    }
}

/// Copies Samsung Home's caches to [`MIRRORS`] (readable by every app label),
/// or removes the copies when `on` is false.
fn mirror_home(on: bool) -> io::Result<()> {
    let _guard = MIRROR_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if !on {
        for mirror in MIRRORS {
            let _ = fs::remove_file(mirror);
        }
        return Ok(());
    }
    // Home writes `.appLayerCache.1` and `.2` in turn; the newer is current.
    let row = [".appLayerCache.1", ".appLayerCache.2"]
        .iter()
        .map(|name| format!("{HOME_DATA}/{name}"))
        .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
        .max()
        .map(|(_, path)| path);
    let sources = format!("{HOME_DATA}/.sourceCache");
    for (from, to) in [row.as_deref(), Some(sources.as_str())]
        .into_iter()
        .zip(MIRRORS)
    {
        let Some(from) = from else { continue };
        let bytes = fs::read(from)?;
        if fs::read(to).is_ok_and(|current| current == bytes) {
            continue;
        }
        let temporary = format!("{to}.tmp");
        fs::write(&temporary, &bytes)?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644))?;
        let path = CString::new(temporary.as_str()).unwrap();
        unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"security.SMACK64".as_ptr(),
                SHARED_LABEL.as_ptr().cast(),
                SHARED_LABEL.len(),
                0,
            )
        };
        fs::rename(&temporary, to)?;
    }
    Ok(())
}

/// Thread: mirror again whenever Home rewrites a cache.
fn mirror_changes(on: &AtomicBool) {
    let watch = match tv_tizen::watch::Watch::new(&[HOME_DATA]) {
        Ok(watch) => watch,
        Err(error) => return log(&format!("mirror: {error}; mirroring once a minute only")),
    };
    let wanted = |name: &str| name.starts_with(".appLayerCache.") || name == ".sourceCache";
    loop {
        if let Err(error) = watch.wait(wanted, Duration::from_millis(300)) {
            log(&format!("mirror watch: {error}"));
            thread::sleep(TICK);
        }
        if on.load(Ordering::Relaxed) {
            if let Err(error) = mirror_home(true) {
                log(&format!("mirror Home's caches: {error}"));
            }
        }
    }
}

/// Our copy of Cobalt's content folder, once nothing points at it.
fn remove_content(report: &mut Report) {
    match fs::symlink_metadata(row::CONTENT) {
        Ok(metadata) if metadata.is_dir() && metadata.uid() == 0 => {
            match fs::remove_dir_all(row::CONTENT) {
                Ok(()) => log("content copy removed"),
                Err(error) => report
                    .errors
                    .push(format!("remove {}: {error}", row::CONTENT)),
            }
        }
        _ => {}
    }
}

// --- proxy ------------------------------------------------------------

fn serve_proxy(shared: Arc<Shared>) {
    let listener = loop {
        match TcpListener::bind(row::PROXY) {
            Ok(listener) => break listener,
            Err(error) => {
                log(&format!("proxy bind {}: {error}", row::PROXY));
                thread::sleep(Duration::from_secs(5));
            }
        }
    };
    log(&format!("proxy listening on {}", row::PROXY));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if shared.connections.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            continue; // dropped
        }
        shared.connections.fetch_add(1, Ordering::Relaxed);
        let shared = shared.clone();
        let spawned = thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let _ = handle(&shared, stream);
                shared.connections.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            log("proxy: cannot start a thread");
        }
    }
}

fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for address in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// One proxy connection: `CONNECT host:port`, then a tunnel, or our TLS and
/// [`http::relay`] for www.youtube.com.
fn handle(shared: &Shared, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some(head) = http::read_head(&mut reader)? else {
        return Ok(());
    };
    let target = head
        .line
        .strip_prefix("CONNECT ")
        .and_then(|rest| {
            rest.strip_suffix(" HTTP/1.1")
                .or(rest.strip_suffix(" HTTP/1.0"))
        })
        .and_then(|target| target.rsplit_once(':'))
        .and_then(|(host, port)| Some((host.to_ascii_lowercase(), port.parse::<u16>().ok()?)))
        .filter(|(host, _)| {
            !host.is_empty()
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        });
    let Some((host, port)) = target else {
        stream.write_all(
            b"HTTP/1.1 405 Method Not Allowed\r\nconnection: close\r\ncontent-length: 0\r\n\r\n",
        )?;
        return Ok(());
    };
    let leftover = reader.buffer().to_vec();
    drop(reader);

    let server = shared.server.read().unwrap().clone();
    let script = shared.script.read().unwrap().clone();
    if let (Some(server), Some(script), true, true) = (
        server,
        script,
        port == 443 && MITM_HOSTS.contains(&host.as_str()),
        leftover.is_empty(),
    ) {
        shared.opened.fetch_add(1, Ordering::Relaxed);
        stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        let client = server.accept(stream).map_err(io::Error::other)?;
        let result = http::relay(
            client,
            || {
                let socket = connect(&host, 443)?;
                socket.set_read_timeout(Some(IDLE))?;
                shared
                    .client
                    .connect(socket, &host)
                    .map_err(io::Error::other)
            },
            Some(&script),
            || {
                shared.pages.fetch_add(1, Ordering::Relaxed);
                log("YouTube TV page: TizenTube added");
            },
        );
        if let Err(error) = &result {
            // Cobalt hanging up mid-response is routine.
            if !matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) {
                log(&format!("{host} relay: {error}"));
            }
        }
        return result;
    }

    shared.tunnels.fetch_add(1, Ordering::Relaxed);
    let mut upstream = match connect(&host, port) {
        Ok(upstream) => upstream,
        Err(error) => {
            stream.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nconnection: close\r\ncontent-length: 0\r\n\r\n",
            )?;
            return Err(error);
        }
    };
    stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    upstream.write_all(&leftover)?;
    // Media streams can idle in one direction for long; a tunnel ends when
    // either side closes.
    stream.set_read_timeout(None)?;
    let (mut down_from, mut down_to) = (upstream.try_clone()?, stream.try_clone()?);
    let downstream = thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(move || {
            let _ = io::copy(&mut down_from, &mut down_to);
            let _ = down_from.shutdown(Shutdown::Both);
            let _ = down_to.shutdown(Shutdown::Both);
        })?;
    let _ = io::copy(&mut stream, &mut upstream);
    let _ = stream.shutdown(Shutdown::Both);
    let _ = upstream.shutdown(Shutdown::Both);
    let _ = downstream.join();
    Ok(())
}
