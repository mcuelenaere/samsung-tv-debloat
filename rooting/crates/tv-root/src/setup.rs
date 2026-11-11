//! One-shot `tv setup` orchestration.
//!
//! The host only verifies the local bundle, performs the compatibility
//! preflight, stages the selected managed payload and runs the thin on-TV
//! driver through one-shot FDET root. The driver owns suite ordering and calls
//! `tv-pkg`; there is no host SSH session or persistent root process.

use crate::bundle::{Bundle, TargetLock};
use crate::compatibility::{Assessment, QN90B};
use crate::error::{Error, Result};
use crate::fdet::{self, RootEvidence, Runtime};
use crate::inject;
use crate::transport::Transport;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const KEY_DROP: &str = "/home/owner/share/tmp/sdk_tools/ssh-key.pub";
pub const MAX_KEY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub bundle_root: PathBuf,
    /// Optional initial SSH public key installed as setup input.
    pub key: Option<PathBuf>,
    pub lock_dir: Option<PathBuf>,
    pub adopt_existing: bool,
    pub timeout: Duration,
}

impl SetupOptions {
    pub fn new(bundle_root: impl Into<PathBuf>) -> Self {
        Self {
            bundle_root: bundle_root.into(),
            key: None,
            lock_dir: None,
            adopt_existing: false,
            timeout: Duration::from_secs(180),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SetupOutcome {
    pub assessment: Assessment,
    pub bundle_sha256: String,
    pub runtime: Runtime,
    pub output: String,
    pub packages: Vec<String>,
}

pub fn run<T: Transport>(transport: &mut T, options: &SetupOptions) -> Result<SetupOutcome> {
    let bundle = Bundle::load(&options.bundle_root)?;
    let _lock = TargetLock::acquire(transport.target(), options.lock_dir.as_deref())?;
    let key = options.key.as_deref().map(read_key).transpose()?;
    transport.require_shell_injection()?;

    let assessment = preflight(transport)?;
    let build = assessment
        .fingerprint
        .build_id
        .as_deref()
        .ok_or_else(|| Error::compatibility("build ID was not observed"))?;
    if build != bundle.manifest.target_build {
        return Err(Error::compatibility(format!(
            "setup bundle targets {}, TV reports {build}",
            bundle.manifest.target_build
        )));
    }
    assessment.require_compatible()?;

    let runtime = runtime(transport)?;
    let token = inject::random_hex(8)?;
    let staging = format!("/home/owner/share/tmp/sdk_tools/tv-setup-{token}");
    let run_dir = format!("/run/tv-setup-{token}");
    let plan = plan(&bundle, runtime, key.is_some(), &staging, &run_dir);

    transport.inject(&format!("/bin/mkdir -p {}", inject::shell_quote(&staging)))?;
    let result = stage_and_execute(
        transport,
        &bundle,
        runtime,
        key.as_deref(),
        &staging,
        &plan,
        &run_dir,
        options,
    );
    let _ = transport.inject(&format!("/bin/rm -rf -- {}", inject::shell_quote(&staging)));
    result.map(|output| SetupOutcome {
        assessment,
        bundle_sha256: bundle.manifest.bundle_sha256.clone(),
        runtime,
        output,
        packages: bundle
            .manifest
            .packages
            .iter()
            .map(|package| package.id.clone())
            .collect(),
    })
}

fn preflight<T: Transport>(transport: &mut T) -> Result<Assessment> {
    let capture = transport.capture(&QN90B.probe_command(), Duration::from_secs(30))?;
    let (body, _) = inject::validate_exit(&capture, &capture.marker_token)?;
    if body.trim().is_empty() {
        return Err(Error::protocol(
            "preflight callback connected but returned no output",
        ));
    }
    Ok(QN90B.assess(&body))
}

fn runtime<T: Transport>(transport: &mut T) -> Result<Runtime> {
    let capture = transport.capture(fdet::ACTIVE_RUNTIME_PROBE, Duration::from_secs(15))?;
    let (body, _) = inject::validate_exit(&capture, &capture.marker_token)?;
    Ok(fdet::runtime_from_output(&body))
}

#[allow(clippy::too_many_arguments)]
fn stage_and_execute<T: Transport>(
    transport: &mut T,
    bundle: &Bundle,
    runtime: Runtime,
    key: Option<&[u8]>,
    staging: &str,
    plan: &str,
    run_dir: &str,
    options: &SetupOptions,
) -> Result<String> {
    let (dll, config) = bundle.variant(runtime);
    let mut files: Vec<(&str, PathBuf)> = vec![
        ("FdetProbe.dll", bundle.path(&dll.path)?),
        ("FdetProbe.runtimeconfig.json", bundle.path(&config.path)?),
        ("tv-pkg", bundle.path(&bundle.manifest.tv_pkg.path)?),
        (
            "setup-driver.sh",
            bundle.path(&bundle.manifest.driver.path)?,
        ),
        (
            "persistence/parseinfo.sh",
            bundle.path(&bundle.manifest.persistence.hook.path)?,
        ),
        (
            "persistence/tv-apply.sh",
            bundle.path(&bundle.manifest.persistence.apply.path)?,
        ),
        (
            "setup-manifest.json",
            bundle.root().join("setup-manifest.json"),
        ),
    ];
    for unit in &bundle.manifest.persistence.units {
        files.push((&unit.path, bundle.path(&unit.path)?));
    }
    for package in &bundle.manifest.packages {
        files.push((&package.path, bundle.path(&package.path)?));
    }
    for (name, local) in files {
        transport.push(&local, &format!("{staging}/{name}"))?;
    }
    if let Some(key) = key {
        let path = std::env::temp_dir().join(format!(
            "tv-setup-key-{}-{}.pub",
            std::process::id(),
            inject::random_hex(4)?
        ));
        std::fs::write(&path, key)?;
        let pushed = transport.push(&path, &format!("{staging}/ssh-key.pub"));
        let _ = std::fs::remove_file(path);
        pushed?;
    }
    let plan_path = std::env::temp_dir().join(format!(
        "tv-setup-plan-{}-{}.txt",
        std::process::id(),
        inject::random_hex(4)?
    ));
    std::fs::write(&plan_path, plan)?;
    let pushed = transport.push(&plan_path, &format!("{staging}/setup-plan"));
    let _ = std::fs::remove_file(plan_path);
    pushed?;

    let mut command = format!(
        "/bin/sh {}/setup-driver.sh --root {} --run {} --plan {}/setup-plan",
        inject::shell_quote(staging),
        inject::shell_quote(staging),
        inject::shell_quote(run_dir),
        inject::shell_quote(staging)
    );
    if options.adopt_existing {
        command.push_str(" --adopt-existing");
    }
    let invocation = fdet::invocation(&format!("{staging}/FdetProbe.dll"), &command)?;
    let capture = transport.capture(&invocation, options.timeout)?;
    let (body, status) = inject::validate_exit(&capture, &capture.marker_token)?;
    if status != 0 {
        return Err(Error::protocol(format!("setup driver exited {status}")));
    }
    RootEvidence::validate(&body)?;
    if !body.lines().any(|line| line.trim() == "setup-status=ok") {
        return Err(Error::protocol(
            "setup driver did not report setup-status=ok",
        ));
    }
    Ok(body)
}

pub fn plan(
    bundle: &Bundle,
    runtime: Runtime,
    has_key: bool,
    staging: &str,
    run_dir: &str,
) -> String {
    let mut lines = vec![
        "format=1".to_owned(),
        format!("target_build={}", bundle.manifest.target_build),
        format!("bundle_sha256={}", bundle.manifest.bundle_sha256),
        format!("runtime={}", runtime.as_str()),
        format!("staging={staging}"),
        format!("run_dir={run_dir}"),
        format!("key={}", if has_key { "ssh-key.pub" } else { "-" }),
        format!(
            "persistence_hook {} {}",
            bundle.manifest.persistence.hook.path, bundle.manifest.persistence.hook.sha256
        ),
        format!(
            "persistence_apply {} {}",
            bundle.manifest.persistence.apply.path, bundle.manifest.persistence.apply.sha256
        ),
    ];
    for unit in &bundle.manifest.persistence.units {
        lines.push(format!("persistence_unit {} {}", unit.path, unit.sha256));
    }
    for package in &bundle.manifest.packages {
        lines.push(format!(
            "package {} {} {}",
            package.id, package.path, package.sha256
        ));
    }
    for row in crate::bundle::digest_rows(&bundle.manifest) {
        lines.push(format!("digest_row={row}"));
    }
    for (path, sha) in verify_rows(bundle, runtime) {
        lines.push(format!("verify {path} {sha}"));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn verify_rows(bundle: &Bundle, runtime: Runtime) -> Vec<(String, String)> {
    let (dll, config) = bundle.variant(runtime);
    let mut rows = vec![
        ("FdetProbe.dll".to_owned(), dll.sha256.clone()),
        (
            "FdetProbe.runtimeconfig.json".to_owned(),
            config.sha256.clone(),
        ),
        (
            bundle.manifest.tv_pkg.path.clone(),
            bundle.manifest.tv_pkg.sha256.clone(),
        ),
        (
            bundle.manifest.driver.path.clone(),
            bundle.manifest.driver.sha256.clone(),
        ),
        (
            bundle.manifest.persistence.hook.path.clone(),
            bundle.manifest.persistence.hook.sha256.clone(),
        ),
        (
            bundle.manifest.persistence.apply.path.clone(),
            bundle.manifest.persistence.apply.sha256.clone(),
        ),
    ];
    for unit in &bundle.manifest.persistence.units {
        rows.push((unit.path.clone(), unit.sha256.clone()));
    }
    for package in &bundle.manifest.packages {
        rows.push((package.path.clone(), package.sha256.clone()));
    }
    rows
}

fn read_key(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > MAX_KEY_BYTES {
        return Err(Error::payload(format!(
            "SSH key file is larger than {MAX_KEY_BYTES} bytes"
        )));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| Error::payload("SSH key file is not text"))?;
    let mut found = false;
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if tv_ssh_key::parse_line(line).is_none() {
            return Err(Error::payload(format!(
                "line {} is not a valid OpenSSH public key",
                number + 1
            )));
        }
        found = true;
    }
    if !found {
        return Err(Error::payload("no public key found"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::tests::fixture;
    use crate::compatibility::tests::TESTED;
    use crate::transport::FakeTransport;
    use std::fs;

    fn execute_output() -> String {
        "uid_after=0\neuid_after=0\ngid_after=0\negid_after=0\nexec_begin\nsetup-status=ok\n[exit:test-token:0]\n"
            .to_owned()
    }

    fn options(bundle: &Path) -> SetupOptions {
        let mut options = SetupOptions::new(bundle);
        options.lock_dir = Some(bundle.join("locks"));
        options
    }

    #[test]
    fn stages_only_the_selected_payload_and_packages_then_runs_the_driver() {
        let (dir, manifest) = fixture();
        assert_eq!(manifest.target_build, "T-PTMAKUC-REL-202310071804");
        let outputs = vec![
            format!("{TESTED}[exit:test-token:0]\n"),
            "/usr/share/dotnet.tizen/netcoreapp/3.1.3\n[exit:test-token:0]\n".into(),
            execute_output(),
        ];
        let mut transport = FakeTransport::new(outputs);
        let outcome = run(&mut transport, &options(dir.path())).unwrap();
        assert_eq!(outcome.runtime, Runtime::Core31);
        assert_eq!(outcome.bundle_sha256, manifest.bundle_sha256);

        let staged: Vec<&str> = transport
            .pushed
            .iter()
            .map(|(_, remote)| remote.as_str())
            .collect();
        assert!(staged.iter().any(|path| path.ends_with("/FdetProbe.dll")));
        assert!(staged
            .iter()
            .any(|path| path.ends_with("/packages/a.tvpkg")));
        assert!(staged
            .iter()
            .any(|path| path.ends_with("/persistence/parseinfo.sh")));
        assert!(!staged.iter().any(|path| path.contains("fdet/legacy")));
        assert!(transport
            .injected
            .iter()
            .any(|command| command.contains("rm -rf")));
    }

    #[test]
    fn refuses_a_different_target_build_before_staging() {
        let (dir, _) = fixture();
        let sibling = TESTED.replace("T-PTMAKUC-REL-202310071804", "T-PTMDEUC-REL-202602250226");
        let outputs = vec![format!("{sibling}[exit:test-token:0]\n")];
        let mut transport = FakeTransport::new(outputs);
        let error = run(&mut transport, &options(dir.path())).unwrap_err();
        assert!(error.to_string().contains("targets T-PTMAKUC"), "{error}");
        assert!(transport.pushed.is_empty());
    }

    #[test]
    fn a_failed_driver_marker_fails_the_operation() {
        let (dir, _) = fixture();
        let mut failed = execute_output().replace("[exit:test-token:0]", "[exit:test-token:7]");
        failed = failed.replace("setup-status=ok", "setup-status=failed");
        let outputs = vec![
            format!("{TESTED}[exit:test-token:0]\n"),
            "/usr/share/dotnet.tizen/netcoreapp/6.0.0\n[exit:test-token:0]\n".into(),
            failed,
        ];
        let mut transport = FakeTransport::new(outputs);
        let error = run(&mut transport, &options(dir.path())).unwrap_err();
        assert!(error.to_string().contains("exited 7"), "{error}");
    }

    #[test]
    fn plan_carries_the_exact_driver_digest_rows() {
        let (dir, manifest) = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let text = plan(&bundle, Runtime::Net6, true, "/stage", "/run");
        assert!(text.contains("verify FdetProbe.dll"));
        assert!(text.contains("verify packages/a.tvpkg"));
        assert!(text.contains("key=ssh-key.pub"));
        let rows: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("digest_row="))
            .collect();
        assert_eq!(rows, crate::bundle::digest_rows(&manifest));
        let digest = crate::bundle::sha256(
            [manifest.target_build.as_str(), rows.join("\n").as_str()]
                .join("\n")
                .as_bytes(),
        );
        assert_eq!(digest, manifest.bundle_sha256);
    }

    #[test]
    fn key_input_is_validated_locally() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("bad.pub");
        fs::write(&key, "not a key\n").unwrap();
        assert!(read_key(&key).is_err());
        let key = dir.path().join("good.pub");
        fs::write(
            &key,
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDydMB0O7XxHHa64MbIoqwtYBeMEDzkWzaOivH+ATzBI me\n",
        )
        .unwrap();
        assert!(read_key(&key).is_ok());
    }
}
