use crate::identity::{is_valid_package_label, package_from_label};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Installed TPKs keep their signature at the package root; installed WGTs
/// keep the signed web bundle under `res/wgt`.
const SIGNATURE_PATHS: &[&str] = &["author-signature.xml", "res/wgt/author-signature.xml"];
const MAX_SIGNATURE_BYTES: u64 = 2 * 1024 * 1024;

pub fn default_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/opt/usr/apps"), PathBuf::from("/usr/apps")]
}

/// Hash the installed package's author signature. The signature covers the
/// manifest and every signed file, so a package upgrade, reinstall with
/// different content, or signer change produces a different fingerprint.
///
/// The file and its package directory must not be group- or world-writable.
/// Missing or untrusted identity fails closed.
pub fn package_fingerprint(label: &str, roots: &[PathBuf]) -> io::Result<String> {
    if !is_valid_package_label(label) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid package label",
        ));
    }
    let package = package_from_label(label).expect("validated package label");

    let mut package_seen = false;
    for root in roots {
        let directory = root.join(package);
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        package_seen = true;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "package path is not a real directory",
            ));
        }
        if metadata.mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "package directory is group- or world-writable",
            ));
        }
        for relative in SIGNATURE_PATHS {
            match read_signature(&directory.join(relative)) {
                Ok(bytes) => return Ok(digest(&bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        if package_seen {
            "package author signature not found"
        } else {
            "package installation not found"
        },
    ))
}

fn read_signature(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "package signature is not a regular file",
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "package signature is group- or world-writable",
        ));
    }
    if metadata.len() > MAX_SIGNATURE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "package signature is too large",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(7 + 64);
    value.push_str("sha256:");
    for byte in digest.iter() {
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("apps");
        let package = root.join("org.example.app");
        fs::create_dir_all(&package).unwrap();
        fs::set_permissions(&package, fs::Permissions::from_mode(0o755)).unwrap();
        let signature = package.join("author-signature.xml");
        fs::write(&signature, b"signed package v1").unwrap();
        fs::set_permissions(&signature, fs::Permissions::from_mode(0o644)).unwrap();
        (directory, root, signature)
    }

    #[test]
    fn fingerprints_tpk_and_wgt_signature_paths() {
        let (_directory, root, signature) = fixture();
        let first =
            package_fingerprint("User::Pkg::org.example.app", std::slice::from_ref(&root)).unwrap();
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), 7 + 64);

        // WGT layout.
        let wgt = root.join("org.example.wgt/res/wgt");
        fs::create_dir_all(&wgt).unwrap();
        fs::write(wgt.join("author-signature.xml"), b"signed wgt").unwrap();
        let wgt_fp =
            package_fingerprint("User::Pkg::org.example.wgt", std::slice::from_ref(&root)).unwrap();
        assert_ne!(first, wgt_fp);

        // Same bytes produce the same fingerprint.
        assert_eq!(
            package_fingerprint("User::Pkg::org.example.app", std::slice::from_ref(&root)).unwrap(),
            first
        );

        // Content changes produce a different fingerprint.
        fs::write(&signature, b"signed package v2").unwrap();
        assert_ne!(
            package_fingerprint("User::Pkg::org.example.app", std::slice::from_ref(&root)).unwrap(),
            first
        );
    }

    #[test]
    fn rejects_missing_untrusted_and_symlinked_signatures() {
        let (_directory, root, signature) = fixture();
        let label = "User::Pkg::org.example.app";
        assert_eq!(
            package_fingerprint(
                "User::Pkg::org.example.missing",
                std::slice::from_ref(&root)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::NotFound
        );

        fs::set_permissions(&signature, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            package_fingerprint(label, std::slice::from_ref(&root))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::remove_file(&signature).unwrap();

        let target = root.join("target.xml");
        fs::write(&target, b"target").unwrap();
        symlink(&target, &signature).unwrap();
        assert!(package_fingerprint(label, &[root]).is_err());
    }

    #[test]
    fn rejects_group_or_world_writable_package_directory() {
        let (_directory, root, _signature) = fixture();
        let package = root.join("org.example.app");
        fs::set_permissions(&package, fs::Permissions::from_mode(0o775)).unwrap();
        assert_eq!(
            package_fingerprint("User::Pkg::org.example.app", &[root])
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
