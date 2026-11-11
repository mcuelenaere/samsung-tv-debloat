//! `tv-pkg`: install, list and remove the suite's components from local
//! `.tvpkg` archives. See `docs/tv-pkg-plan.md`.
//!
//! Every device path goes through [`Context::path`], so tests run the real
//! code against a temporary root with fake systemd and broker adapters.

pub mod archive;
pub mod manifest;
pub mod native;
pub mod state;
pub mod systemd;
pub mod tizen;

use archive::Archive;
use manifest::Kind;
use state::State;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use systemd::Runner;
use tizen::Broker;

pub const LOCK_PATH: &str = "/run/tv-pkg.lock";

#[derive(Debug)]
pub struct Error {
    /// `busy`, `invalid`, `conflict`, `failed`, `io` or `not_installed`.
    pub code: &'static str,
    pub message: String,
}

impl Error {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new("conflict", message)
    }
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new("failed", message)
    }
    pub fn io(context: impl Into<String>, error: io::Error) -> Self {
        Self::new("io", format!("{}: {error}", context.into()))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

pub struct Context<'a> {
    /// `/` on the TV; a temporary directory in tests.
    pub root: PathBuf,
    /// Set owner 0:0 and SMACK labels (needs root on Linux).
    pub apply_security: bool,
    /// How long started units must stay active before an install succeeds.
    pub settle: std::time::Duration,
    pub runner: &'a dyn Runner,
    pub broker: &'a dyn Broker,
}

impl Context<'_> {
    /// Map an absolute device path into `root`.
    pub fn path(&self, device_path: &str) -> PathBuf {
        self.root.join(device_path.trim_start_matches('/'))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub changed: Option<bool>,
}

pub fn verify(package: &Path) -> Result<Outcome, Error> {
    let mut archive = Archive::open(package)?;
    archive.verify()?;
    Ok(outcome(&archive.manifest, &archive.digest, None))
}

pub fn install(ctx: &Context, package: &Path, adopt_existing: bool) -> Result<Outcome, Error> {
    let _lock = lock(ctx)?;
    native::recover(ctx)?;
    let mut archive = Archive::open(package)?;
    archive.verify()?;
    let manifest = archive.manifest.clone();
    let states = state::load_all(ctx)?;
    let (previous, others): (Vec<State>, Vec<State>) = states
        .into_iter()
        .partition(|state| state.manifest.id == manifest.id);
    let previous = previous.into_iter().next();
    if let Some(previous) = &previous {
        if previous.manifest.kind != manifest.kind {
            return Err(Error::conflict(format!(
                "{} is installed as {}; remove it first",
                manifest.id,
                previous.manifest.kind.as_str()
            )));
        }
    }
    let changed = match manifest.kind {
        Kind::Native => native::install(
            ctx,
            &mut archive,
            previous.as_ref(),
            &others,
            adopt_existing,
        )?,
        Kind::Tpk | Kind::Wgt => tizen::install(ctx, &mut archive, previous.as_ref())?,
    };
    Ok(outcome(&manifest, &archive.digest, Some(changed)))
}

pub fn list(ctx: &Context) -> Result<Vec<State>, Error> {
    state::load_all(ctx)
}

/// The recorded state and a list of health problems (empty when healthy).
pub fn status(ctx: &Context, id: &str) -> Result<(State, Vec<String>), Error> {
    let state = state::load(ctx, id)?
        .ok_or_else(|| Error::new("not_installed", format!("{id} is not installed")))?;
    let problems = health(ctx, &state)?;
    Ok((state, problems))
}

pub fn remove(ctx: &Context, id: &str) -> Result<Outcome, Error> {
    let _lock = lock(ctx)?;
    native::recover(ctx)?;
    let state = state::load(ctx, id)?
        .ok_or_else(|| Error::new("not_installed", format!("{id} is not installed")))?;
    match state.manifest.kind {
        Kind::Native => native::remove(ctx, &state)?,
        Kind::Tpk | Kind::Wgt => tizen::remove(ctx, &state)?,
    }
    state::delete(ctx, id)?;
    Ok(outcome(&state.manifest, &state.digest, Some(true)))
}

fn health(ctx: &Context, state: &State) -> Result<Vec<String>, Error> {
    match state.manifest.kind {
        Kind::Native => native::health(ctx, state),
        Kind::Tpk | Kind::Wgt => tizen::health(ctx, state),
    }
}

fn outcome(manifest: &manifest::Manifest, digest: &str, changed: Option<bool>) -> Outcome {
    Outcome {
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        digest: digest.to_owned(),
        changed,
    }
}

/// One non-blocking process lock; dropping the file releases it.
fn lock(ctx: &Context) -> Result<File, Error> {
    let path = ctx.path(LOCK_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io("create lock directory", e))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|e| Error::io(format!("open {}", path.display()), e))?;
    // SAFETY: flock on a descriptor owned by `file`.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Err(Error::new("busy", "another tv-pkg operation is running"));
        }
        return Err(Error::io("lock", error));
    }
    Ok(file)
}
