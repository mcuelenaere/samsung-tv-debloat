//! One-time trust enrollment for the stock package installer.
//!
//! The firmware validates package signatures against files on the read-only
//! rootfs. Enrolling a user-owned CA therefore means bind-mounting our CA set
//! and fingerprint list over the stock paths. `ensure_enrolled` is idempotent:
//! it verifies the material, checks `/proc/self/mounts`, and repairs missing
//! bind mounts. It is called at daemon start, before each approved install, and
//! from `tv-authd --enroll-trust`.

use crate::pkgmeta::{sha1_digest, SignatureContext};
use base64::Engine as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_TRUST_ROOT: &str = "/opt/tv-auth/trust";
pub const DEFAULT_FINGERPRINT_FILE: &str =
    "/usr/share/ca-certificates/fingerprint/fingerprint_list.xml";
pub const DEFAULT_TIZEN_CA_DIR: &str = "/usr/share/ca-certificates/tizen";

#[derive(Debug, Clone)]
pub struct TrustPaths {
    /// Holds `user-root-ca.pem`, `fingerprint_list.xml`, and `tizen-ca/`.
    pub root: PathBuf,
    /// Stock fingerprint list to replace with our copy.
    pub fingerprint_file: PathBuf,
    /// Stock Tizen CA directory to replace with our overlay.
    pub tizen_ca_dir: PathBuf,
}

impl TrustPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fingerprint_file: PathBuf::from(DEFAULT_FINGERPRINT_FILE),
            tizen_ca_dir: PathBuf::from(DEFAULT_TIZEN_CA_DIR),
        }
    }

    pub fn ca_file(&self) -> PathBuf {
        self.root.join("user-root-ca.pem")
    }

    pub fn fingerprint_source(&self) -> PathBuf {
        self.root.join("fingerprint_list.xml")
    }

    pub fn tizen_ca_source(&self) -> PathBuf {
        self.root.join("tizen-ca")
    }
}

#[derive(Debug)]
pub enum TrustError {
    Missing(String),
    FingerprintMissing,
    MountFailed(String),
    Io(io::Error),
}

impl std::fmt::Display for TrustError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(what) => write!(formatter, "missing trust material: {what}"),
            Self::FingerprintMissing => formatter
                .write_str("fingerprint_list.xml does not contain the user CA SHA-1 fingerprint"),
            Self::MountFailed(detail) => write!(formatter, "trust bind mount failed: {detail}"),
            Self::Io(error) => write!(formatter, "trust I/O error: {error}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Enrollment {
    AlreadyEnrolled,
    Mounted,
}

impl std::fmt::Display for Enrollment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::AlreadyEnrolled => "already enrolled",
            Self::Mounted => "bind mounts created",
        })
    }
}

#[derive(Debug, Clone)]
pub struct TrustMaterial {
    pub user_ca_sha1: [u8; 20],
    /// Every SHA-1 in the enrolled fingerprint list (stock roots + ours).
    pub known_fingerprints: Vec<[u8; 20]>,
}

impl TrustMaterial {
    pub fn signature_context(&self) -> SignatureContext {
        SignatureContext {
            user_ca_sha1: Some(self.user_ca_sha1),
            known_fingerprints: self.known_fingerprints.clone(),
        }
    }
}

/// Read and validate the deployed material. Does not touch mounts.
pub fn load_material(paths: &TrustPaths) -> Result<TrustMaterial, TrustError> {
    let ca_pem = fs::read(paths.ca_file()).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => TrustError::Missing("user-root-ca.pem".into()),
        _ => TrustError::Io(error),
    })?;
    let ca_der = certificate_der(&ca_pem)?;
    let user_ca_sha1 = sha1_digest(&ca_der);

    let fingerprint_path = paths.fingerprint_source();
    let fingerprint_text =
        fs::read_to_string(&fingerprint_path).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => TrustError::Missing("fingerprint_list.xml".into()),
            _ => TrustError::Io(error),
        })?;
    let known_fingerprints = fingerprints_in(&fingerprint_text);
    if !known_fingerprints.contains(&user_ca_sha1) {
        return Err(TrustError::FingerprintMissing);
    }

    let tizen_ca = paths.tizen_ca_source();
    if !tizen_ca.is_dir() {
        return Err(TrustError::Missing("tizen-ca/".into()));
    }

    Ok(TrustMaterial {
        user_ca_sha1,
        known_fingerprints,
    })
}

/// Make sure both bind mounts are active, repairing any that are missing or
/// point at a stale source.
///
/// Verification is by content rather than by `/proc/self/mounts`: this
/// firmware's VDFS reports a bind mount with the *source* path as the mount
/// point, so the mount table cannot be matched reliably. The target paths are
/// read-only stock files, so the only way they can contain our CA is that a
/// bind mount (or an earlier repair) is active.
pub fn ensure_enrolled(paths: &TrustPaths) -> Result<Enrollment, TrustError> {
    let material = load_material(paths)?;
    if is_enrolled(paths, &material.user_ca_sha1) {
        return Ok(Enrollment::AlreadyEnrolled);
    }

    let pairs = [
        (paths.fingerprint_source(), paths.fingerprint_file.clone()),
        (paths.tizen_ca_source(), paths.tizen_ca_dir.clone()),
    ];
    for (source, target) in &pairs {
        let source = fs::canonicalize(source).map_err(TrustError::Io)?;
        if !target.exists() {
            return Err(TrustError::Missing(format!(
                "mount target {} does not exist",
                target.display()
            )));
        }
        // Drop a stale mount first (best effort: there may be none).
        let _ = run_umount(target);
        run_mount(&source, target)?;
    }

    if !is_enrolled(paths, &material.user_ca_sha1) {
        return Err(TrustError::MountFailed(
            "the target files still do not contain the enrolled CA".into(),
        ));
    }
    Ok(Enrollment::Mounted)
}

fn is_enrolled(paths: &TrustPaths, user_ca_sha1: &[u8; 20]) -> bool {
    is_enrolled_with(paths, user_ca_sha1, read_in_root_namespace)
}

fn is_enrolled_with(
    paths: &TrustPaths,
    user_ca_sha1: &[u8; 20],
    read: impl Fn(&Path) -> Result<Vec<u8>, TrustError>,
) -> bool {
    let fingerprint_ok = read(&paths.fingerprint_file)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| fingerprints_in(&text).contains(user_ca_sha1))
        .unwrap_or(false);
    let ca_target = paths.tizen_ca_dir.join("user-root-ca.pem");
    let ca_ok = read(&ca_target)
        .ok()
        .and_then(|pem| certificate_der(&pem).ok())
        .map(|der| &sha1_digest(&der) == user_ca_sha1)
        .unwrap_or(false);
    fingerprint_ok && ca_ok
}

/// Command that runs in PID 1's mount namespace. `tv-authd.service` uses
/// `BindReadOnlyPaths=`, so systemd puts the daemon in a private mount
/// namespace; `pkgmgr-server` (the process that validates signatures during
/// `pkgcmd -i`) runs in the root namespace with `PrivateMounts=no`. Mounts made
/// from the daemon's own namespace are therefore invisible to the installer.
fn root_namespace_command() -> Command {
    let nsenter = if Path::new("/usr/bin/nsenter").exists() {
        "/usr/bin/nsenter"
    } else {
        "/bin/nsenter"
    };
    let mut command = Command::new(nsenter);
    command.args(["-t", "1", "-m", "--"]);
    command
}

fn read_in_root_namespace(path: &Path) -> Result<Vec<u8>, TrustError> {
    let output = root_namespace_command()
        .arg("/bin/cat")
        .arg(path)
        .output()
        .map_err(TrustError::Io)?;
    if !output.status.success() {
        return Err(TrustError::MountFailed(format!(
            "cannot read {} in the root mount namespace: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn run_umount(target: &Path) -> Result<(), TrustError> {
    let output = root_namespace_command()
        .arg("/bin/umount")
        .arg(target)
        .output()
        .map_err(TrustError::Io)?;
    if !output.status.success() {
        return Err(TrustError::MountFailed(format!(
            "umount {}: {}",
            target.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn run_mount(source: &Path, target: &Path) -> Result<(), TrustError> {
    let output = root_namespace_command()
        .args(["/bin/mount", "--bind"])
        .arg(source)
        .arg(target)
        .output()
        .map_err(TrustError::Io)?;
    if !output.status.success() {
        return Err(TrustError::MountFailed(format!(
            "mount {} -> {}: {}",
            source.display(),
            target.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Extract the DER bytes from a PEM certificate, or return the input when it is
/// already DER.
pub fn certificate_der(bytes: &[u8]) -> Result<Vec<u8>, TrustError> {
    // DER is binary, so test the SEQUENCE tag before requiring UTF-8.
    if bytes.first() == Some(&0x30) {
        return Ok(bytes.to_vec());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| TrustError::Missing("user-root-ca.pem is not a certificate".into()))?;
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let Some(start) = text.find(begin) else {
        return Err(TrustError::Missing(
            "user-root-ca.pem is not a PEM certificate".into(),
        ));
    };
    let body_start = start + begin.len();
    let Some(relative_end) = text[body_start..].find(end) else {
        return Err(TrustError::Missing(
            "user-root-ca.pem has no END CERTIFICATE marker".into(),
        ));
    };
    let compact: String = text[body_start..body_start + relative_end]
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|_| TrustError::Missing("user-root-ca.pem is not valid base64".into()))
}

/// Collect every 40-hex-digit SHA-1 fingerprint in the file. Tizen's list uses
/// colon-separated uppercase hex; accept plain lowercase too.
pub fn fingerprints_in(text: &str) -> Vec<[u8; 20]> {
    let mut fingerprints = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_hexdigit() {
            index += 1;
            continue;
        }
        let start = index;
        let mut digits = Vec::with_capacity(40);
        while index < bytes.len() {
            let byte = bytes[index];
            if byte.is_ascii_hexdigit() {
                digits.push(byte);
                index += 1;
            } else if byte == b':' && digits.len() < 40 {
                index += 1;
            } else {
                break;
            }
        }
        if digits.len() == 40 {
            if let Some(fingerprint) = parse_hex_20(&digits) {
                fingerprints.push(fingerprint);
            }
        } else if index == start {
            index += 1;
        }
    }
    fingerprints
}

fn parse_hex_20(digits: &[u8]) -> Option<[u8; 20]> {
    let text = std::str::from_utf8(digits).ok()?;
    let mut fingerprint = [0_u8; 20];
    for (index, chunk) in text.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk).ok()?;
        fingerprint[index] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(fingerprint)
}

/// Source path currently mounted at `target`, if any. Later entries win: the
/// mount table lists stacked mounts in the order they were applied.
pub fn mount_source(mounts: &str, target: &Path) -> Option<PathBuf> {
    let target = target.to_string_lossy();
    let mut found = None;
    for line in mounts.lines() {
        let mut fields = line.split_ascii_whitespace();
        let source = fields.next()?;
        let mount_point = fields.next()?;
        if unescape_mount_field(mount_point) == target {
            found = Some(PathBuf::from(unescape_mount_field(source)));
        }
    }
    found
}

fn unescape_mount_field(field: &str) -> String {
    let mut output = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let digits: String = chars.by_ref().take(3).collect();
        match digits.as_str() {
            "040" => output.push(' '),
            "011" => output.push('\t'),
            "012" => output.push('\n'),
            "134" => output.push('\\'),
            _ => {
                output.push('\\');
                output.push_str(&digits);
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_material(root: &Path, ca_der: &[u8], fingerprints: &str) {
        fs::create_dir_all(root.join("tizen-ca")).unwrap();
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::engine::general_purpose::STANDARD.encode(ca_der)
        );
        fs::write(root.join("user-root-ca.pem"), pem).unwrap();
        fs::write(root.join("fingerprint_list.xml"), fingerprints).unwrap();
        fs::set_permissions(
            root.join("user-root-ca.pem"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }

    fn colon_hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    #[test]
    fn parses_colon_and_plain_fingerprints() {
        let one = "AB".repeat(20);
        let two = "cd".repeat(20);
        let text = format!(
            "<domain name=\"tizen-developer\"><fingerprint>{}</fingerprint></domain>\
             <domain name=\"tizen-public\"><fingerprint>{}</fingerprint></domain>",
            one.replace("AB", "AB:").trim_end_matches(':'),
            two
        );
        let parsed = fingerprints_in(&text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], [0xAB; 20]);
        assert_eq!(parsed[1], [0xCD; 20]);
        assert!(fingerprints_in("AB:CD").is_empty());
        assert!(fingerprints_in(&"AB".repeat(21)).is_empty());
    }

    #[test]
    fn loads_and_validates_material() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let ca_der = b"not really a certificate, but stable bytes";
        let fingerprint = colon_hex(&sha1_digest(ca_der));
        write_material(
            root,
            ca_der,
            &format!("<fingerprint>{fingerprint}</fingerprint>"),
        );

        let paths = TrustPaths::new(root);
        let material = load_material(&paths).unwrap();
        assert_eq!(material.user_ca_sha1, sha1_digest(ca_der));
        assert_eq!(
            material.signature_context().user_ca_sha1,
            Some(sha1_digest(ca_der))
        );
        assert_eq!(material.known_fingerprints.len(), 1);
    }

    #[test]
    fn detects_enrollment_by_target_content() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("trust");
        let ca_der = b"target certificate bytes";
        let sha1 = sha1_digest(ca_der);
        write_material(
            &root,
            ca_der,
            &format!("<FingerprintSHA1>{}</FingerprintSHA1>", colon_hex(&sha1)),
        );

        let mut paths = TrustPaths::new(&root);
        paths.fingerprint_file = directory.path().join("target-fingerprint.xml");
        paths.tizen_ca_dir = directory.path().join("target-tizen-ca");
        fs::create_dir_all(&paths.tizen_ca_dir).unwrap();
        assert!(!is_enrolled_with(&paths, &sha1, |path| {
            fs::read(path).map_err(TrustError::Io)
        }));

        fs::copy(root.join("fingerprint_list.xml"), &paths.fingerprint_file).unwrap();
        fs::copy(
            root.join("user-root-ca.pem"),
            paths.tizen_ca_dir.join("user-root-ca.pem"),
        )
        .unwrap();
        assert!(is_enrolled_with(&paths, &sha1, |path| {
            fs::read(path).map_err(TrustError::Io)
        }));
    }

    #[test]
    fn rejects_material_without_our_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        write_material(
            root,
            b"certificate",
            &format!("<fingerprint>{}</fingerprint>", "00:".repeat(19) + "00"),
        );
        assert!(matches!(
            load_material(&TrustPaths::new(root)),
            Err(TrustError::FingerprintMissing)
        ));
    }

    #[test]
    fn reports_missing_material() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            load_material(&TrustPaths::new(directory.path())),
            Err(TrustError::Missing(_))
        ));
    }

    #[test]
    fn finds_active_mounts_and_unescapes_paths() {
        let mounts = "\
/dev/root / ext4 rw 0 0
/dev/sda1 /mnt/with\\040space vfat rw 0 0
/opt/tv-auth/trust/fingerprint_list.xml /usr/share/ca-certificates/fingerprint/fingerprint_list.xml none ro 0 0
";
        assert_eq!(
            mount_source(mounts, Path::new("/")),
            Some(PathBuf::from("/dev/root"))
        );
        assert_eq!(
            mount_source(mounts, Path::new("/mnt/with space")),
            Some(PathBuf::from("/dev/sda1"))
        );
        assert_eq!(
            mount_source(
                mounts,
                Path::new("/usr/share/ca-certificates/fingerprint/fingerprint_list.xml")
            ),
            Some(PathBuf::from("/opt/tv-auth/trust/fingerprint_list.xml"))
        );
        assert_eq!(mount_source(mounts, Path::new("/not/mounted")), None);
    }

    #[test]
    fn decodes_pem_and_der() {
        let der: &[u8] = b"\x30\x82fake certificate bytes";
        assert_eq!(certificate_der(der).unwrap(), der);
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
            base64::engine::general_purpose::STANDARD.encode(der)
        );
        assert_eq!(certificate_der(pem.as_bytes()).unwrap(), der);
        assert!(certificate_der(b"-----BEGIN CERTIFICATE-----").is_err());
        assert!(certificate_der(b"not a certificate").is_err());
    }
}
