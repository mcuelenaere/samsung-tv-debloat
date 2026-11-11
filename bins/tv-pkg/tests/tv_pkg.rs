//! Host tests: a temporary root with fake systemd and tv-authd adapters.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use tv_pkg::manifest::Manifest;
use tv_pkg::systemd::{Runner, SYSTEMCTL};
use tv_pkg::tizen::Broker;
use tv_pkg::Context;

#[derive(Default)]
struct FakeRunner {
    log: RefCell<Vec<String>>,
    active: RefCell<BTreeSet<String>>,
    /// Command line that exits 1 instead of succeeding.
    fail: RefCell<Option<String>>,
}

impl Runner for FakeRunner {
    fn run(&self, argv: &[String]) -> io::Result<i32> {
        let mut words: Vec<&str> = argv.iter().map(String::as_str).collect();
        if words[0] == SYSTEMCTL {
            words[0] = "systemctl";
        }
        let line = words.join(" ");
        self.log.borrow_mut().push(line.clone());
        // One-shot, so the rollback's own commands succeed.
        if self.fail.borrow().as_deref() == Some(line.as_str()) {
            self.fail.borrow_mut().take();
            return Ok(1);
        }
        let mut active = self.active.borrow_mut();
        Ok(match words[..] {
            ["systemctl", "is-active", "--quiet", unit] => {
                if active.contains(unit) {
                    0
                } else {
                    3
                }
            }
            ["systemctl", "start", unit] => {
                active.insert(unit.to_owned());
                0
            }
            ["systemctl", "stop", unit] => {
                active.remove(unit);
                0
            }
            _ => 0,
        })
    }
}

impl FakeRunner {
    fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.borrow_mut())
    }
}

#[derive(Default)]
struct FakeBroker {
    requests: RefCell<Vec<Value>>,
    /// (pkgid, pkg_type)
    installed: RefCell<Vec<(String, String)>>,
    /// Bytes of the package file as tv-authd would have staged it.
    received: RefCell<Vec<Vec<u8>>>,
    fail_install: Cell<bool>,
}

impl Broker for FakeBroker {
    fn call(&self, request: &Value) -> io::Result<Value> {
        self.requests.borrow_mut().push(request.clone());
        assert_eq!(request["version"], 2);
        // The broker's own strict parser must accept the request.
        tv_proto::parse_request(request.to_string().as_bytes()).expect("tv-authd parses it");
        let mut installed = self.installed.borrow_mut();
        Ok(match request["method"].as_str().unwrap() {
            "pkg.list" => {
                let packages: Vec<Value> = installed
                    .iter()
                    .map(|(id, kind)| json!({"pkgid": id, "name": id, "pkg_version": "1", "pkg_type": kind}))
                    .collect();
                json!({"status": "ok", "packages": packages})
            }
            "pkg.install" => {
                let path = request["path"].as_str().unwrap();
                self.received.borrow_mut().push(fs::read(path)?);
                if self.fail_install.get() {
                    json!({"status": "error", "error": "install_failed", "detail": "pkgcmd said no"})
                } else {
                    installed.push((
                        "org.tvauth.ui".into(),
                        request["pkg_type"].as_str().unwrap().into(),
                    ));
                    json!({"status": "ok", "pkgid": "org.tvauth.ui"})
                }
            }
            "pkg.uninstall" => {
                let pkgid = request["pkgid"].as_str().unwrap();
                let before = installed.len();
                installed.retain(|(id, _)| id != pkgid);
                if installed.len() == before {
                    json!({"status": "error", "error": "unknown_package"})
                } else {
                    json!({"status": "ok", "pkgid": pkgid})
                }
            }
            other => panic!("unexpected method {other}"),
        })
    }
}

struct Env {
    dir: tempfile::TempDir,
    runner: FakeRunner,
    broker: FakeBroker,
}

impl Env {
    fn new() -> Self {
        let env = Self {
            dir: tempfile::tempdir().unwrap(),
            runner: FakeRunner::default(),
            broker: FakeBroker::default(),
        };
        for dir in [
            "opt/tv-suite",
            "opt/tv-units",
            "opt/tv-sshd",
            "opt/tv-auth",
            "tmp",
            "run",
        ] {
            fs::create_dir_all(env.root().join(dir)).unwrap();
        }
        env
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn path(&self, device: &str) -> PathBuf {
        self.root().join(device.trim_start_matches('/'))
    }

    fn ctx(&self) -> Context<'_> {
        Context {
            root: self.root().to_owned(),
            apply_security: false,
            runner: &self.runner,
            broker: &self.broker,
        }
    }

    fn package(&self, name: &str, manifest: &Value, payload: &[(&str, &[u8])]) -> PathBuf {
        let path = self.root().join(name);
        write_zip(
            &path,
            &[("component.json", manifest.to_string().as_bytes())],
            payload,
        );
        path
    }
}

fn write_zip(path: &Path, first: &[(&str, &[u8])], rest: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in first.iter().chain(rest) {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const UNIT: &[u8] = b"[Service]\nExecStart=/run/opt/tv-suite/bin/tv-blockd\n";

fn native(binary: &[u8]) -> Value {
    json!({
        "format": 1,
        "id": "tv-blockd",
        "version": "0.1.0",
        "kind": "native",
        "directories": [{"path": "/opt/tv-suite/bin", "mode": "0755", "smack": "System::Shared"}],
        "files": [{
            "source": "payload/tv-blockd",
            "destination": "/opt/tv-suite/bin/tv-blockd",
            "sha256": sha(binary),
            "mode": "0755",
            "smack": "System::Shared"
        }],
        "units": [{"source": "payload/tv-blockd.service", "name": "tv-blockd.service", "sha256": sha(UNIT)}],
        "undo_command": ["/opt/tv-suite/bin/tv-blockd", "undo"]
    })
}

fn native_package(env: &Env, name: &str, binary: &[u8]) -> PathBuf {
    env.package(
        name,
        &native(binary),
        &[
            ("payload/tv-blockd", binary),
            ("payload/tv-blockd.service", UNIT),
        ],
    )
}

/// Every path under the root (except the lock file) with its content.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let mode = metadata.permissions().mode() & 0o7777;
            if relative == "run/tv-pkg.lock" || relative.ends_with(".tvpkg") {
                continue;
            }
            let value = if metadata.file_type().is_symlink() {
                format!("link {}", fs::read_link(&path).unwrap().display())
            } else if metadata.is_dir() {
                walk(root, &path, out);
                format!("dir {mode:o}")
            } else {
                format!("file {mode:o} {}", sha(&fs::read(&path).unwrap()))
            };
            out.insert(relative, value);
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

// ---------------------------------------------------------------- manifest

#[test]
fn manifest_validation() {
    let ok = |value: Value| Manifest::parse(value.to_string().as_bytes());
    assert!(ok(native(b"x")).is_ok());

    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut value = native(b"x");
        f(&mut value);
        ok(value).unwrap_err().code
    };
    assert_eq!(mutate(&|v| v["surprise"] = json!(1)), "invalid");
    assert_eq!(
        mutate(&|v| v["files"][0]["owner"] = json!("root")),
        "invalid"
    );
    assert_eq!(mutate(&|v| v["format"] = json!(2)), "invalid");
    assert_eq!(mutate(&|v| v["id"] = json!("../x")), "invalid");
    for destination in [
        "/etc/passwd",
        "/opt/tv-suite",
        "/opt/tv-suite/../etc/x",
        "/opt/tv-suite//x",
        "/opt/tv-suiteX/x",
        "opt/tv-suite/x",
        "/opt/tv-suite/var/tv-pkg/components/x.json",
        "/opt/tv-units/evil.service",
    ] {
        assert_eq!(
            mutate(&|v| v["files"][0]["destination"] = json!(destination)),
            "invalid",
            "{destination}"
        );
    }
    for source in ["tv-blockd", "payload/../x", "/payload/x", "payload/"] {
        assert_eq!(
            mutate(&|v| v["files"][0]["source"] = json!(source)),
            "invalid",
            "{source}"
        );
    }
    for bad_mode in ["4755", "755x", "75", "07555", "0800"] {
        assert_eq!(
            mutate(&|v| v["files"][0]["mode"] = json!(bad_mode)),
            "invalid",
            "{bad_mode}"
        );
    }
    assert_eq!(
        mutate(&|v| v["files"][0]["sha256"] = json!("AB".repeat(32))),
        "invalid"
    );
    assert_eq!(
        mutate(&|v| v["files"][0]["smack"] = json!("a/b")),
        "invalid"
    );
    assert_eq!(
        mutate(&|v| v["units"][0]["name"] = json!("../x.service")),
        "invalid"
    );
    assert_eq!(
        mutate(&|v| v["units"][0]["name"] = json!("x.socket")),
        "invalid"
    );
    assert_eq!(
        mutate(&|v| v["undo_command"] = json!(["relative", "undo"])),
        "invalid"
    );
    assert_eq!(mutate(&|v| v["undo_command"] = json!([])), "invalid");
    assert_eq!(
        mutate(&|v| v["package"] =
            json!({"source": "payload/a", "sha256": sha(b"a"), "pkgid": "tv-blockd"})),
        "invalid"
    );
    // A destination may only be claimed once.
    assert_eq!(
        mutate(&|v| v["links"] = json!([{"path": "/opt/tv-suite/bin/tv-blockd", "target": "x"}])),
        "invalid"
    );
    for target in [
        "/opt/tv-sshd/bin/dropbear",
        "../dropbear",
        "a/../b",
        "",
        "./x",
    ] {
        assert_eq!(
            mutate(&|v| v["links"] = json!([{"path": "/opt/tv-sshd/bin/k", "target": target}])),
            "invalid",
            "{target:?}"
        );
    }

    let tpk = json!({"format": 1, "id": "org.tvauth.ui", "version": "0.1.0", "kind": "tpk",
                     "package": {"source": "payload/ui.tpk", "sha256": sha(b"x"), "pkgid": "org.tvauth.ui"}});
    assert!(ok(tpk.clone()).is_ok());
    let mut mismatch = tpk.clone();
    mismatch["package"]["pkgid"] = json!("org.other");
    assert!(ok(mismatch).is_err());
    let mut with_files = tpk.clone();
    with_files["files"] = native(b"x")["files"].clone();
    assert!(ok(with_files).is_err());
    let mut no_package = tpk;
    no_package.as_object_mut().unwrap().remove("package");
    assert!(ok(no_package).is_err());
}

// ---------------------------------------------------------------- archive

#[test]
fn archive_validation() {
    let env = Env::new();
    let manifest = native(b"bin");
    let manifest_bytes = manifest.to_string();
    let good: Vec<(&str, &[u8])> = vec![
        ("payload/tv-blockd", b"bin"),
        ("payload/tv-blockd.service", UNIT),
    ];
    let check = |name: &str, entries: &[(&str, &[u8])], expected: &str| {
        let path = env.root().join(name);
        write_zip(
            &path,
            &[("component.json", manifest_bytes.as_bytes())],
            entries,
        );
        let error = tv_pkg::verify(&path).unwrap_err();
        assert!(
            error.message.contains(expected),
            "{name}: {}",
            error.message
        );
    };

    assert_eq!(
        tv_pkg::verify(&native_package(&env, "ok.tvpkg", b"bin"))
            .unwrap()
            .id,
        "tv-blockd"
    );
    let mut traversal = good.clone();
    traversal.push(("payload/../../etc/passwd", b"x"));
    check("traversal.tvpkg", &traversal, "unexpected entry");
    let mut absolute = good.clone();
    absolute.push(("/payload/x", b"x"));
    check("absolute.tvpkg", &absolute, "");
    let mut outside = good.clone();
    outside.push(("other/x", b"x"));
    check("outside.tvpkg", &outside, "unexpected entry");
    let mut unlisted = good.clone();
    unlisted.push(("payload/extra", b"x"));
    check("unlisted.tvpkg", &unlisted, "unlisted payload");
    check("missing.tvpkg", &good[..1], "missing payload");
    check(
        "hash.tvpkg",
        &[("payload/tv-blockd", b"evil"), good[1]],
        "SHA-256 mismatch",
    );

    // Symlink entry.
    let path = env.root().join("symlink.tvpkg");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("component.json", options).unwrap();
    zip.write_all(manifest_bytes.as_bytes()).unwrap();
    zip.add_symlink("payload/tv-blockd", "/etc/shadow", options)
        .unwrap();
    zip.finish().unwrap();
    assert!(tv_pkg::verify(&path).unwrap_err().message.contains("link"));

    // Duplicate entry: write two distinct names, then make them equal.
    let path = env.root().join("dup.tvpkg");
    write_zip(
        &path,
        &[("component.json", manifest_bytes.as_bytes())],
        &[("payload/tv-blockd", b"bin"), ("payload/tv-blockX", b"bin")],
    );
    let mut bytes = fs::read(&path).unwrap();
    let mut patched = 0;
    while let Some(at) = bytes.windows(17).position(|w| w == b"payload/tv-blockX") {
        bytes[at + 16] = b'd';
        patched += 1;
    }
    assert_eq!(patched, 2, "local and central headers");
    fs::write(&path, &bytes).unwrap();
    assert!(tv_pkg::verify(&path).is_err());

    // Entry-count limit.
    let many: Vec<(String, Vec<u8>)> = (0..300).map(|i| (format!("payload/{i}"), vec![])).collect();
    let many: Vec<(&str, &[u8])> = many
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    check("many.tvpkg", &many, "entries");

    // Manifest size limit.
    let mut big = manifest.clone();
    big["version"] = json!("1");
    let padding = " ".repeat(70 * 1024);
    let path = env.root().join("bigmanifest.tvpkg");
    write_zip(
        &path,
        &[("component.json", format!("{big}{padding}").as_bytes())],
        &good,
    );
    assert!(tv_pkg::verify(&path)
        .unwrap_err()
        .message
        .contains("too large"));

    // Expanded-size limit: claim 2 GiB in the central directory.
    let path = native_package(&env, "bomb.tvpkg", b"bin");
    let mut bytes = fs::read(&path).unwrap();
    let central = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    bytes[central + 24..central + 28].copy_from_slice(&0x7fff_ffff_u32.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    assert!(tv_pkg::verify(&path)
        .unwrap_err()
        .message
        .contains("size limit"));
}

// ---------------------------------------------------------------- native

#[test]
fn fresh_install_writes_exact_metadata_and_state() {
    let env = Env::new();
    let package = native_package(&env, "v1.tvpkg", b"binary-v1");
    let outcome = tv_pkg::install(&env.ctx(), &package, false).unwrap();
    assert_eq!(outcome.changed, Some(true));
    assert_eq!(outcome.version, "0.1.0");

    let binary = env.path("/opt/tv-suite/bin/tv-blockd");
    assert_eq!(fs::read(&binary).unwrap(), b"binary-v1");
    assert_eq!(mode(&binary), 0o755);
    assert_eq!(mode(&env.path("/opt/tv-suite/bin")), 0o755);
    let unit = env.path("/opt/tv-units/tv-blockd.service");
    assert_eq!(fs::read(&unit).unwrap(), UNIT);
    assert_eq!(mode(&unit), 0o600);
    let runtime = env.path("/run/systemd/system/tv-blockd.service");
    assert_eq!(fs::read(&runtime).unwrap(), UNIT);
    assert_eq!(mode(&runtime), 0o644);

    assert_eq!(
        env.runner.take_log(),
        [
            "systemctl is-active --quiet tv-blockd.service",
            "systemctl daemon-reload",
            "systemctl start tv-blockd.service",
            "systemctl is-active --quiet tv-blockd.service",
        ]
    );
    let state_path = env.path("/opt/tv-suite/var/tv-pkg/components/tv-blockd.json");
    assert_eq!(mode(&state_path), 0o600);
    let states = tv_pkg::list(&env.ctx()).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].digest, outcome.digest);
    // The transaction directory is gone.
    let leftovers: Vec<_> = fs::read_dir(env.path("/opt/tv-suite/var/tv-pkg"))
        .unwrap()
        .collect();
    assert_eq!(leftovers.len(), 1);

    let (_, problems) = tv_pkg::status(&env.ctx(), "tv-blockd").unwrap();
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn identical_content_is_a_no_op_and_changed_content_reinstalls() {
    let env = Env::new();
    let v1 = native_package(&env, "v1.tvpkg", b"binary-v1");
    tv_pkg::install(&env.ctx(), &v1, false).unwrap();
    env.runner.take_log();

    let before = snapshot(env.root());
    let again = tv_pkg::install(&env.ctx(), &v1, false).unwrap();
    assert_eq!(again.changed, Some(false));
    assert_eq!(
        env.runner.take_log(),
        ["systemctl is-active --quiet tv-blockd.service"]
    );
    assert_eq!(snapshot(env.root()), before);

    // Same version, different content: a development reinstall.
    let v1b = native_package(&env, "v1b.tvpkg", b"binary-v1-rebuilt");
    let outcome = tv_pkg::install(&env.ctx(), &v1b, false).unwrap();
    assert_eq!(outcome.changed, Some(true));
    assert_eq!(outcome.version, "0.1.0");
    assert_eq!(
        fs::read(env.path("/opt/tv-suite/bin/tv-blockd")).unwrap(),
        b"binary-v1-rebuilt"
    );
    let log = env.runner.take_log();
    assert!(
        log.contains(&"systemctl stop tv-blockd.service".to_owned()),
        "{log:?}"
    );
    assert!(
        log.contains(&"systemctl start tv-blockd.service".to_owned()),
        "{log:?}"
    );
    assert_eq!(tv_pkg::list(&env.ctx()).unwrap()[0].digest, outcome.digest);
}

#[test]
fn unhealthy_install_with_same_digest_is_repaired() {
    let env = Env::new();
    let v1 = native_package(&env, "v1.tvpkg", b"binary-v1");
    tv_pkg::install(&env.ctx(), &v1, false).unwrap();
    fs::remove_file(env.path("/run/systemd/system/tv-blockd.service")).unwrap();
    env.runner.active.borrow_mut().clear();
    let (_, problems) = tv_pkg::status(&env.ctx(), "tv-blockd").unwrap();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert_eq!(
        tv_pkg::install(&env.ctx(), &v1, false).unwrap().changed,
        Some(true)
    );
    assert!(tv_pkg::status(&env.ctx(), "tv-blockd")
        .unwrap()
        .1
        .is_empty());
}

#[test]
fn unexpected_or_modified_destinations_need_explicit_adoption() {
    let env = Env::new();
    fs::create_dir_all(env.path("/opt/tv-suite/bin")).unwrap();
    fs::write(env.path("/opt/tv-suite/bin/tv-blockd"), b"hand-deployed").unwrap();
    let v1 = native_package(&env, "v1.tvpkg", b"binary-v1");
    let before = snapshot(env.root());
    let error = tv_pkg::install(&env.ctx(), &v1, false).unwrap_err();
    assert_eq!(error.code, "conflict");
    assert!(error.message.contains("--adopt-existing"));
    assert_eq!(snapshot(env.root()), before);
    assert!(env.runner.take_log().is_empty());

    tv_pkg::install(&env.ctx(), &v1, true).unwrap();
    assert_eq!(
        fs::read(env.path("/opt/tv-suite/bin/tv-blockd")).unwrap(),
        b"binary-v1"
    );

    // A later local edit is refused on update, then adoptable.
    fs::write(env.path("/opt/tv-suite/bin/tv-blockd"), b"edited").unwrap();
    let v2 = native_package(&env, "v2.tvpkg", b"binary-v2");
    let error = tv_pkg::install(&env.ctx(), &v2, false).unwrap_err();
    assert!(error.message.contains("modified"), "{}", error.message);
    tv_pkg::install(&env.ctx(), &v2, true).unwrap();
    assert_eq!(
        fs::read(env.path("/opt/tv-suite/bin/tv-blockd")).unwrap(),
        b"binary-v2"
    );

    // A directory where a file belongs is never adopted.
    let env = Env::new();
    fs::create_dir_all(env.path("/opt/tv-suite/bin/tv-blockd")).unwrap();
    let v1 = native_package(&env, "v1.tvpkg", b"binary-v1");
    assert_eq!(
        tv_pkg::install(&env.ctx(), &v1, true).unwrap_err().code,
        "conflict"
    );
}

#[test]
fn failure_before_mutation_leaves_the_live_tree_untouched() {
    let env = Env::new();
    tv_pkg::install(
        &env.ctx(),
        &native_package(&env, "v1.tvpkg", b"binary-v1"),
        false,
    )
    .unwrap();
    env.runner.take_log();
    let before = snapshot(env.root());

    // Payload that does not match its declared hash.
    let mut manifest = native(b"binary-v2");
    manifest["version"] = json!("0.2.0");
    let bad = env.package(
        "bad.tvpkg",
        &manifest,
        &[
            ("payload/tv-blockd", b"tampered"),
            ("payload/tv-blockd.service", UNIT),
        ],
    );
    assert_eq!(
        tv_pkg::install(&env.ctx(), &bad, false).unwrap_err().code,
        "invalid"
    );
    // Parent missing and undeclared.
    let mut manifest = native(b"x");
    manifest["files"][0]["destination"] = json!("/opt/tv-suite/nope/tv-blockd");
    let orphan = env.package(
        "orphan.tvpkg",
        &manifest,
        &[
            ("payload/tv-blockd", b"x"),
            ("payload/tv-blockd.service", UNIT),
        ],
    );
    assert_eq!(
        tv_pkg::install(&env.ctx(), &orphan, false)
            .unwrap_err()
            .code,
        "conflict"
    );

    assert_eq!(snapshot(env.root()), before);
    assert!(env.runner.take_log().is_empty());
}

#[test]
fn failure_after_mutation_restores_files_and_units() {
    let env = Env::new();
    tv_pkg::install(
        &env.ctx(),
        &native_package(&env, "v1.tvpkg", b"binary-v1"),
        false,
    )
    .unwrap();
    let before = snapshot(env.root());
    let digest = tv_pkg::list(&env.ctx()).unwrap()[0].digest.clone();
    env.runner.take_log();

    // v2 adds a file and a directory, then its unit fails to start.
    let mut manifest = native(b"binary-v2");
    manifest["directories"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path": "/opt/tv-suite/share", "mode": "0700", "smack": "_"}));
    manifest["files"].as_array_mut().unwrap().push(json!({
        "source": "payload/extra", "destination": "/opt/tv-suite/share/extra",
        "sha256": sha(b"extra"), "mode": "0644", "smack": "_"}));
    let v2 = env.package(
        "v2.tvpkg",
        &manifest,
        &[
            ("payload/tv-blockd", b"binary-v2"),
            ("payload/tv-blockd.service", UNIT),
            ("payload/extra", b"extra"),
        ],
    );
    *env.runner.fail.borrow_mut() = Some("systemctl start tv-blockd.service".into());
    let error = tv_pkg::install(&env.ctx(), &v2, false).unwrap_err();
    assert_eq!(error.code, "failed");
    assert!(
        !error.message.contains("rollback incomplete"),
        "{}",
        error.message
    );

    assert_eq!(snapshot(env.root()), before);
    assert_eq!(tv_pkg::list(&env.ctx()).unwrap()[0].digest, digest);
    assert!(
        env.runner.active.borrow().contains("tv-blockd.service"),
        "previous unit restarted"
    );
    let log = env.runner.take_log();
    assert_eq!(
        log.iter()
            .filter(|l| *l == "systemctl daemon-reload")
            .count(),
        2,
        "{log:?}"
    );
    assert_eq!(log.last().unwrap(), "systemctl start tv-blockd.service");
}

#[test]
fn failed_post_install_check_rolls_back() {
    let env = Env::new();
    // The unit "starts" but never reports active.
    struct NeverActive(FakeRunner);
    impl Runner for NeverActive {
        fn run(&self, argv: &[String]) -> io::Result<i32> {
            let code = self.0.run(argv)?;
            Ok(if argv[1] == "is-active" { 3 } else { code })
        }
    }
    let runner = NeverActive(FakeRunner::default());
    let ctx = Context {
        runner: &runner,
        ..env.ctx()
    };
    let before = snapshot(env.root());
    let error =
        tv_pkg::install(&ctx, &native_package(&env, "v1.tvpkg", b"binary-v1"), false).unwrap_err();
    assert!(error.message.contains("not active"), "{}", error.message);
    let mut after = snapshot(env.root());
    // tv-pkg's own (empty) state directories may remain.
    after.retain(|path, _| {
        !path.starts_with("opt/tv-suite/var")
            && path != "run/systemd/system"
            && path != "run/systemd"
    });
    assert_eq!(after, before);
    assert!(tv_pkg::list(&ctx).unwrap().is_empty());
}

#[test]
fn symlinked_parents_are_rejected_and_links_are_relative() {
    let env = Env::new();
    fs::create_dir_all(env.path("/opt/tv-auth/real")).unwrap();
    symlink(env.path("/opt/tv-auth/real"), env.path("/opt/tv-suite/bin")).unwrap();
    let error =
        tv_pkg::install(&env.ctx(), &native_package(&env, "v1.tvpkg", b"x"), false).unwrap_err();
    assert_eq!(error.code, "conflict");
    assert!(fs::read_dir(env.path("/opt/tv-auth/real"))
        .unwrap()
        .next()
        .is_none());

    let env = Env::new();
    let dropbear = b"dropbear-binary";
    let manifest = json!({
        "format": 1, "id": "dropbear", "version": "2024.86", "kind": "native",
        "directories": [{"path": "/opt/tv-sshd/bin", "mode": "0700", "smack": "System::Shared"}],
        "files": [{"source": "payload/dropbear", "destination": "/opt/tv-sshd/bin/dropbear",
                   "sha256": sha(dropbear), "mode": "0755", "smack": "System::Shared"}],
        "links": [{"path": "/opt/tv-sshd/bin/dropbearkey", "target": "dropbear"}]
    });
    let package = env.package(
        "dropbear.tvpkg",
        &manifest,
        &[("payload/dropbear", dropbear)],
    );
    tv_pkg::install(&env.ctx(), &package, false).unwrap();
    let link = env.path("/opt/tv-sshd/bin/dropbearkey");
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("dropbear"));
    assert_eq!(fs::read(&link).unwrap(), dropbear);
    assert!(
        env.runner.take_log().is_empty(),
        "no units, no systemd calls"
    );

    // A replaced link target is a modification.
    fs::remove_file(&link).unwrap();
    symlink("/bin/sh", &link).unwrap();
    assert!(!tv_pkg::status(&env.ctx(), "dropbear").unwrap().1.is_empty());
    assert_eq!(
        tv_pkg::remove(&env.ctx(), "dropbear").unwrap_err().code,
        "conflict"
    );
}

#[test]
fn unlisted_files_survive_install_update_and_removal() {
    let env = Env::new();
    let keep = [
        "/opt/tv-suite/etc/blockd.json",
        "/opt/tv-suite/bin/other-tool",
        "/opt/tv-sshd/dropbear_ed25519_host_key",
        "/opt/tv-auth/grants.json",
        "/opt/tv-units/dropbear.service",
    ];
    for path in keep {
        fs::create_dir_all(env.path(path).parent().unwrap()).unwrap();
        fs::write(env.path(path), path).unwrap();
    }
    tv_pkg::install(
        &env.ctx(),
        &native_package(&env, "v1.tvpkg", b"binary-v1"),
        false,
    )
    .unwrap();
    tv_pkg::install(
        &env.ctx(),
        &native_package(&env, "v2.tvpkg", b"binary-v2"),
        false,
    )
    .unwrap();
    tv_pkg::remove(&env.ctx(), "tv-blockd").unwrap();
    for path in keep {
        assert_eq!(fs::read_to_string(env.path(path)).unwrap(), path);
    }
    assert!(!env.path("/opt/tv-suite/bin/tv-blockd").exists());
    assert!(
        env.path("/opt/tv-suite/bin").is_dir(),
        "non-empty directory kept"
    );
}

#[test]
fn update_removes_files_dropped_from_the_manifest() {
    let env = Env::new();
    let mut manifest = native(b"v1");
    manifest["files"].as_array_mut().unwrap().push(json!({
        "source": "payload/helper", "destination": "/opt/tv-suite/bin/helper",
        "sha256": sha(b"helper"), "mode": "0755", "smack": "_"}));
    let v1 = env.package(
        "v1.tvpkg",
        &manifest,
        &[
            ("payload/tv-blockd", b"v1"),
            ("payload/tv-blockd.service", UNIT),
            ("payload/helper", b"helper"),
        ],
    );
    tv_pkg::install(&env.ctx(), &v1, false).unwrap();
    assert!(env.path("/opt/tv-suite/bin/helper").exists());

    // v2 also drops the unit.
    let mut manifest = native(b"v2");
    manifest.as_object_mut().unwrap().remove("units");
    let v2 = env.package("v2.tvpkg", &manifest, &[("payload/tv-blockd", b"v2")]);
    env.runner.take_log();
    tv_pkg::install(&env.ctx(), &v2, false).unwrap();
    assert!(!env.path("/opt/tv-suite/bin/helper").exists());
    assert!(!env.path("/opt/tv-units/tv-blockd.service").exists());
    assert!(!env.path("/run/systemd/system/tv-blockd.service").exists());
    assert_eq!(
        env.runner.take_log(),
        [
            "systemctl is-active --quiet tv-blockd.service",
            "systemctl stop tv-blockd.service",
            "systemctl daemon-reload"
        ]
    );
}

#[test]
fn another_components_files_are_never_claimed() {
    let env = Env::new();
    tv_pkg::install(&env.ctx(), &native_package(&env, "v1.tvpkg", b"v1"), false).unwrap();
    let mut manifest = native(b"v1");
    manifest["id"] = json!("tv-blockd-fork");
    manifest["units"] = json!([]);
    let fork = env.package("fork.tvpkg", &manifest, &[("payload/tv-blockd", b"v1")]);
    let error = tv_pkg::install(&env.ctx(), &fork, true).unwrap_err();
    assert!(
        error.message.contains("owned by tv-blockd"),
        "{}",
        error.message
    );
}

#[test]
fn removal_runs_only_declared_operations() {
    let env = Env::new();
    tv_pkg::install(
        &env.ctx(),
        &native_package(&env, "v1.tvpkg", b"binary-v1"),
        false,
    )
    .unwrap();
    env.runner.take_log();

    // A modified owned file blocks removal before anything runs.
    fs::write(env.path("/opt/tv-suite/bin/tv-blockd"), b"edited").unwrap();
    assert_eq!(
        tv_pkg::remove(&env.ctx(), "tv-blockd").unwrap_err().code,
        "conflict"
    );
    assert!(env.runner.take_log().is_empty());
    fs::write(env.path("/opt/tv-suite/bin/tv-blockd"), b"binary-v1").unwrap();

    // A failing undo command stops the removal.
    *env.runner.fail.borrow_mut() = Some("/opt/tv-suite/bin/tv-blockd undo".into());
    assert_eq!(
        tv_pkg::remove(&env.ctx(), "tv-blockd").unwrap_err().code,
        "failed"
    );
    assert!(env.path("/opt/tv-suite/bin/tv-blockd").exists());
    env.runner.take_log();

    tv_pkg::remove(&env.ctx(), "tv-blockd").unwrap();
    assert_eq!(
        env.runner.take_log(),
        [
            "/opt/tv-suite/bin/tv-blockd undo",
            "systemctl is-active --quiet tv-blockd.service",
            "systemctl stop tv-blockd.service",
            "systemctl daemon-reload",
        ]
    );
    for path in [
        "/opt/tv-suite/bin/tv-blockd",
        "/opt/tv-suite/bin",
        "/opt/tv-units/tv-blockd.service",
        "/run/systemd/system/tv-blockd.service",
        "/opt/tv-suite/var/tv-pkg/components/tv-blockd.json",
    ] {
        assert!(!env.path(path).exists(), "{path}");
    }
    assert!(env.path("/opt/tv-units").is_dir() && env.path("/opt/tv-suite").is_dir());
    assert_eq!(
        tv_pkg::remove(&env.ctx(), "tv-blockd").unwrap_err().code,
        "not_installed"
    );
}

// ---------------------------------------------------------------- tpk/wgt

fn tpk_package(env: &Env, name: &str, kind: &str, inner: &[u8]) -> PathBuf {
    let source = format!("payload/org.tvauth.ui-0.1.0.{kind}");
    let manifest = json!({"format": 1, "id": "org.tvauth.ui", "version": "0.1.0", "kind": kind,
                          "package": {"source": source, "sha256": sha(inner), "pkgid": "org.tvauth.ui"}});
    env.package(name, &manifest, &[(source.as_str(), inner)])
}

#[test]
fn tpk_install_and_remove_go_through_the_broker() {
    let env = Env::new();
    let package = tpk_package(&env, "ui.tvpkg", "tpk", b"tpk-bytes");
    let outcome = tv_pkg::install(&env.ctx(), &package, false).unwrap();
    assert_eq!(outcome.changed, Some(true));

    let requests = env.broker.requests.borrow().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "pkg.install");
    assert_eq!(requests[0]["pkg_type"], "tpk");
    let path = PathBuf::from(requests[0]["path"].as_str().unwrap());
    assert!(path.starts_with(env.path("/tmp")), "{}", path.display());
    assert!(path.to_string_lossy().ends_with("/package.tpk"));
    assert_eq!(env.broker.received.borrow()[0], b"tpk-bytes");
    assert!(
        !path.parent().unwrap().exists(),
        "extraction directory cleaned up"
    );
    assert_eq!(
        tv_pkg::list(&env.ctx()).unwrap()[0].manifest.id,
        "org.tvauth.ui"
    );

    // Identical content and installed: unchanged, with only a pkg.list check.
    assert_eq!(
        tv_pkg::install(&env.ctx(), &package, false)
            .unwrap()
            .changed,
        Some(false)
    );
    assert_eq!(
        env.broker.requests.borrow().last().unwrap()["method"],
        "pkg.list"
    );
    assert!(tv_pkg::status(&env.ctx(), "org.tvauth.ui")
        .unwrap()
        .1
        .is_empty());

    tv_pkg::remove(&env.ctx(), "org.tvauth.ui").unwrap();
    let last = env.broker.requests.borrow().last().unwrap().clone();
    assert_eq!(last["method"], "pkg.uninstall");
    assert_eq!(last["pkgid"], "org.tvauth.ui");
    assert!(tv_pkg::list(&env.ctx()).unwrap().is_empty());
    assert!(env.runner.take_log().is_empty(), "no systemd for packages");
}

#[test]
fn wgt_reinstalls_when_the_broker_lost_the_package_and_failures_keep_no_state() {
    let env = Env::new();
    let package = tpk_package(&env, "ui.tvpkg", "wgt", b"wgt-bytes");
    env.broker.fail_install.set(true);
    let error = tv_pkg::install(&env.ctx(), &package, false).unwrap_err();
    assert!(error.message.contains("install_failed") && error.message.contains("pkgcmd said no"));
    assert!(
        tv_pkg::list(&env.ctx()).unwrap().is_empty(),
        "no state without success"
    );
    assert_eq!(fs::read_dir(env.path("/tmp")).unwrap().count(), 0);

    env.broker.fail_install.set(false);
    tv_pkg::install(&env.ctx(), &package, false).unwrap();
    assert_eq!(
        env.broker.requests.borrow().last().unwrap()["pkg_type"],
        "wgt"
    );
    env.broker.installed.borrow_mut().clear();
    assert_eq!(
        tv_pkg::install(&env.ctx(), &package, false)
            .unwrap()
            .changed,
        Some(true)
    );

    // Removing a package the broker no longer knows still clears local state.
    env.broker.installed.borrow_mut().clear();
    tv_pkg::remove(&env.ctx(), "org.tvauth.ui").unwrap();
    assert!(tv_pkg::list(&env.ctx()).unwrap().is_empty());
}

#[test]
fn kind_changes_are_refused() {
    let env = Env::new();
    tv_pkg::install(
        &env.ctx(),
        &tpk_package(&env, "a.tvpkg", "tpk", b"a"),
        false,
    )
    .unwrap();
    let error = tv_pkg::install(
        &env.ctx(),
        &tpk_package(&env, "b.tvpkg", "wgt", b"b"),
        false,
    )
    .unwrap_err();
    assert_eq!(error.code, "conflict");
}

// ---------------------------------------------------------------- state, lock

#[test]
fn malformed_state_fails_closed() {
    let env = Env::new();
    tv_pkg::install(&env.ctx(), &native_package(&env, "v1.tvpkg", b"v1"), false).unwrap();
    let state = env.path("/opt/tv-suite/var/tv-pkg/components/tv-blockd.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    value["manifest"]["files"][0]["destination"] = json!("/etc/shadow");
    fs::write(&state, value.to_string()).unwrap();
    for error in [
        tv_pkg::list(&env.ctx()).unwrap_err(),
        tv_pkg::status(&env.ctx(), "tv-blockd").unwrap_err(),
        tv_pkg::remove(&env.ctx(), "tv-blockd").unwrap_err(),
        tv_pkg::install(&env.ctx(), &native_package(&env, "v2.tvpkg", b"v2"), false).unwrap_err(),
    ] {
        assert!(error.message.contains("malformed"), "{}", error.message);
    }
    fs::write(&state, b"{not json").unwrap();
    assert!(tv_pkg::list(&env.ctx()).is_err());
    assert!(env.path("/opt/tv-suite/bin/tv-blockd").exists());
}

#[test]
fn concurrent_installation_reports_busy() {
    let env = Env::new();
    let held = fs::File::create(env.path("/run/tv-pkg.lock")).unwrap();
    // Another process holding the lock.
    // SAFETY: flock on an open descriptor.
    assert_eq!(
        unsafe {
            libc::flock(
                std::os::unix::io::AsRawFd::as_raw_fd(&held),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        },
        0
    );
    let package = native_package(&env, "v1.tvpkg", b"v1");
    assert_eq!(
        tv_pkg::install(&env.ctx(), &package, false)
            .unwrap_err()
            .code,
        "busy"
    );
    assert_eq!(
        tv_pkg::remove(&env.ctx(), "tv-blockd").unwrap_err().code,
        "busy"
    );
    // Read-only commands do not take the lock.
    assert!(tv_pkg::list(&env.ctx()).unwrap().is_empty());
    drop(held);
    tv_pkg::install(&env.ctx(), &package, false).unwrap();
}
