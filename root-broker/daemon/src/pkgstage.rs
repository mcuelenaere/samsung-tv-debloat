//! Root-only staging for package installs.
//!
//! The daemon copies the requester's package into a private directory before
//! the approval prompt is shown. The user therefore approves, and `pkgcmd`
//! later installs, exactly the bytes that were hashed here — the source path is
//! not touched again after staging (no TOCTOU on an app-writable file).

use crate::protocol::{prompt_text, PackageType, MAX_PACKAGE_PATH_BYTES};
use rustix::fs::{open, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Default cap on a staged package (128 MiB). Staging lives on the /run
/// tmpfs; 128 MiB keeps a large package from pressuring the ~890 MiB tmpfs
/// and the TV's ~550 MiB of available RAM. The CLI can raise it to 256 MiB.
pub const DEFAULT_MAX_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_PACKAGE_BYTES_LIMIT: u64 = 256 * 1024 * 1024;

const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Default source allowlist. Anything else is rejected before the file is read.
pub const DEFAULT_ALLOWED_ROOTS: &[&str] = &["/opt/usr/home/owner", "/opt/media", "/tmp"];

#[derive(Debug, Clone)]
pub struct StageConfig {
    /// Root-only directory that holds one subdirectory per staged package.
    pub root: PathBuf,
    /// Directories the source package may live under (after symlink
    /// resolution).
    pub allowed_roots: Vec<PathBuf>,
    pub max_bytes: u64,
}

impl Default for StageConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/opt/tv-auth/staging"),
            allowed_roots: DEFAULT_ALLOWED_ROOTS.iter().map(PathBuf::from).collect(),
            max_bytes: DEFAULT_MAX_PACKAGE_BYTES,
        }
    }
}

#[derive(Debug)]
pub enum StageError {
    /// Outside the allowlist, not a regular file, a symlink, or malformed.
    InvalidPath,
    TooLarge,
    Io(io::Error),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath => formatter.write_str("invalid package path"),
            Self::TooLarge => formatter.write_str("package exceeds the size limit"),
            Self::Io(error) => write!(formatter, "staging I/O error: {error}"),
        }
    }
}

/// A package copied into daemon-owned storage. Dropping it removes the staging
/// directory and the package, whether the install succeeded, failed, or was
/// denied.
#[derive(Debug)]
pub struct StagedPackage {
    directory: PathBuf,
    path: PathBuf,
    display_path: String,
    pkg_type: PackageType,
    size_bytes: u64,
    sha256: String,
}

impl StagedPackage {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sanitized copy of the requester-supplied path, safe for the prompt.
    pub fn display_path(&self) -> &str {
        &self.display_path
    }

    pub fn pkg_type(&self) -> PackageType {
        self.pkg_type
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

impl Drop for StagedPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Create the staging root (mode 0711) and delete anything left over from a
/// previous daemon run. Called once at startup.
pub fn prepare_staging_root(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staging root must be a directory, not a symlink",
        ));
    }
    // 0711 lets the UID-5001 webappservice traverse into the (randomly named)
    // package directory when the WAS install path is used; the file itself is
    // 0644 and the containing package directory is also 0711.
    fs::set_permissions(root, std::os::unix::fs::PermissionsExt::from_mode(0o711))?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {
                let _ = fs::remove_dir_all(&path);
            }
            _ => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    Ok(())
}

/// Copy `source` into a fresh subdirectory of `config.root`.
///
/// The source must be a regular file reached through the allowlist without a
/// symlink at the final component. The copy is streamed with a hard byte cap
/// and hashed with SHA-256 as it is written.
pub fn stage(
    config: &StageConfig,
    source: &str,
    pkg_type: PackageType,
) -> Result<StagedPackage, StageError> {
    let source_path = Path::new(source);
    if source.is_empty()
        || source.len() > MAX_PACKAGE_PATH_BYTES
        || source.as_bytes().contains(&0)
        || !source_path.is_absolute()
    {
        return Err(StageError::InvalidPath);
    }

    // Reject a symlink at the final component before touching the file.
    let link_metadata = fs::symlink_metadata(source_path).map_err(map_io)?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
        return Err(StageError::InvalidPath);
    }

    // Resolve parent directories (an intermediate symlink is fine) and check
    // the resolved location against the allowlist before revealing its size.
    let canonical = fs::canonicalize(source_path).map_err(map_io)?;
    if !is_allowed(&config.allowed_roots, &canonical) {
        return Err(StageError::InvalidPath);
    }
    if link_metadata.len() > config.max_bytes {
        return Err(StageError::TooLarge);
    }

    // O_NOFOLLOW closes the lstat/open race for the final component.
    let fd = open(
        &canonical,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| map_io(error.into()))?;
    let mut source_file = File::from(fd);
    let metadata = source_file.metadata().map_err(map_io)?;
    if !metadata.is_file() {
        return Err(StageError::InvalidPath);
    }
    if metadata.len() > config.max_bytes {
        return Err(StageError::TooLarge);
    }

    fs::create_dir_all(&config.root).map_err(map_io)?;
    fs::set_permissions(
        &config.root,
        std::os::unix::fs::PermissionsExt::from_mode(0o711),
    )
    .map_err(map_io)?;
    let directory = config.root.join(format!("stage-{}", random_hex()?));
    fs::create_dir(&directory).map_err(map_io)?;
    fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o711),
    )
    .map_err(map_io)?;

    let path = directory.join(format!("package.{}", pkg_type.as_str()));
    let staged = match copy_capped(&mut source_file, &path, config.max_bytes) {
        Ok(staged) => staged,
        Err(error) => {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
    };

    Ok(StagedPackage {
        directory,
        path,
        display_path: prompt_text(source, MAX_PACKAGE_PATH_BYTES),
        pkg_type,
        size_bytes: staged.size_bytes,
        sha256: staged.sha256,
    })
}

struct Copied {
    size_bytes: u64,
    sha256: String,
}

fn copy_capped(
    source: &mut File,
    destination: &Path,
    max_bytes: u64,
) -> Result<Copied, StageError> {
    let mut output = File::create(destination).map_err(map_io)?;
    // The installer backend may run as owner (UID 5001) and read the file.
    fs::set_permissions(
        destination,
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .map_err(map_io)?;

    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut size_bytes = 0_u64;
    loop {
        let read = source.read(&mut buffer).map_err(map_io)?;
        if read == 0 {
            break;
        }
        size_bytes = size_bytes.saturating_add(read as u64);
        if size_bytes > max_bytes {
            return Err(StageError::TooLarge);
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read]).map_err(map_io)?;
    }
    output.sync_all().map_err(map_io)?;
    drop(output);

    if size_bytes == 0 {
        // An empty file is never a package; reject it before the zip parser.
        return Err(StageError::InvalidPath);
    }

    Ok(Copied {
        size_bytes,
        sha256: hex(&hasher.finalize()),
    })
}

fn is_allowed(roots: &[PathBuf], canonical: &Path) -> bool {
    roots.iter().any(|root| {
        let resolved = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        canonical.starts_with(&resolved)
    })
}

fn map_io(error: io::Error) -> StageError {
    match error.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound => StageError::InvalidPath,
        io::ErrorKind::InvalidInput => StageError::InvalidPath,
        _ => StageError::Io(error),
    }
}

fn random_hex() -> Result<String, StageError> {
    use std::fmt::Write as _;

    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| StageError::Io(io::Error::other(format!("getrandom: {error}"))))?;
    let mut token = String::with_capacity(32);
    for byte in bytes {
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture {
        _directory: tempfile::TempDir,
        config: StageConfig,
        source_root: PathBuf,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let source_root = directory.path().join("home/owner");
        fs::create_dir_all(&source_root).unwrap();
        let config = StageConfig {
            root: directory.path().join("staging"),
            allowed_roots: vec![source_root.clone()],
            max_bytes: 1024,
        };
        Fixture {
            _directory: directory,
            config,
            source_root,
        }
    }

    #[test]
    fn stages_and_hashes_an_allowed_file() {
        let fixture = fixture();
        let source = fixture.source_root.join("app.wgt");
        fs::write(&source, b"package bytes").unwrap();

        let staged = stage(&fixture.config, source.to_str().unwrap(), PackageType::Wgt).unwrap();
        assert_eq!(staged.size_bytes(), 13);
        assert_eq!(fs::read(staged.path()).unwrap(), b"package bytes");
        assert!(staged.path().starts_with(&fixture.config.root));
        assert!(staged.path().to_string_lossy().ends_with("package.wgt"));
        assert_eq!(staged.display_path(), source.to_str().unwrap());
        assert_eq!(staged.sha256().len(), 64);

        let path = staged.path().to_path_buf();
        let directory = path.parent().unwrap().to_path_buf();
        drop(staged);
        assert!(!directory.exists(), "staging directory must be removed");
    }

    #[test]
    fn rejects_paths_outside_the_allowlist() {
        let fixture = fixture();
        let outside = fixture._directory.path().join("outside.wgt");
        fs::write(&outside, b"x").unwrap();
        assert!(matches!(
            stage(&fixture.config, outside.to_str().unwrap(), PackageType::Wgt),
            Err(StageError::InvalidPath)
        ));
        assert!(matches!(
            stage(&fixture.config, "relative.wgt", PackageType::Wgt),
            Err(StageError::InvalidPath)
        ));
    }

    #[test]
    fn rejects_symlinks_and_directories() {
        let fixture = fixture();
        let real = fixture.source_root.join("real.wgt");
        fs::write(&real, b"x").unwrap();
        let link = fixture.source_root.join("link.wgt");
        symlink(&real, &link).unwrap();

        assert!(matches!(
            stage(&fixture.config, link.to_str().unwrap(), PackageType::Wgt),
            Err(StageError::InvalidPath)
        ));
        assert!(matches!(
            stage(
                &fixture.config,
                fixture.source_root.to_str().unwrap(),
                PackageType::Wgt
            ),
            Err(StageError::InvalidPath)
        ));
    }

    #[test]
    fn rejects_oversized_and_empty_files() {
        let fixture = fixture();
        let large = fixture.source_root.join("large.wgt");
        fs::write(&large, vec![0_u8; 1025]).unwrap();
        assert!(matches!(
            stage(&fixture.config, large.to_str().unwrap(), PackageType::Wgt),
            Err(StageError::TooLarge)
        ));

        let empty = fixture.source_root.join("empty.wgt");
        fs::write(&empty, b"").unwrap();
        assert!(matches!(
            stage(&fixture.config, empty.to_str().unwrap(), PackageType::Wgt),
            Err(StageError::InvalidPath)
        ));
    }

    #[test]
    fn preparing_the_root_wipes_old_entries() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = fixture();
        fs::create_dir_all(fixture.config.root.join("stage-old")).unwrap();
        fs::write(fixture.config.root.join("stage-old/package.wgt"), b"old").unwrap();
        prepare_staging_root(&fixture.config.root).unwrap();
        assert_eq!(fs::read_dir(&fixture.config.root).unwrap().count(), 0);
        let mode = fs::metadata(&fixture.config.root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o711);
    }
}
