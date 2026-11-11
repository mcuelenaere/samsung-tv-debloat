//! Native component transactions: declared files, links, directories and
//! systemd units.

use crate::archive::Archive;
use crate::manifest::{Manifest, STATE_DIR, UNIT_MODE, UNIT_SMACK};
use crate::state::{self, inspect, Current, State};
use crate::systemd::Systemd;
use crate::{Context, Error};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

pub const RUNTIME_UNIT_DIR: &str = "/run/systemd/system";
const RUNTIME_UNIT_MODE: u32 = 0o644;

/// What a component recorded at one of its destinations.
#[derive(Debug, PartialEq, Eq)]
enum Expected<'a> {
    File(&'a str),
    Link(&'a str),
}

fn expected(manifest: &Manifest) -> HashMap<String, Expected<'_>> {
    let files = manifest
        .files
        .iter()
        .map(|f| (f.destination.clone(), Expected::File(&f.sha256)));
    let links = manifest
        .links
        .iter()
        .map(|l| (l.path.clone(), Expected::Link(&l.target)));
    let units = manifest
        .units
        .iter()
        .map(|u| (u.destination(), Expected::File(&u.sha256)));
    files.chain(links).chain(units).collect()
}

fn matches(expected: &Expected, current: &Current) -> bool {
    match (expected, current) {
        (Expected::File(hash), Current::File { sha256, .. }) => hash == sha256,
        (Expected::Link(target), Current::Link(actual)) => target == actual,
        _ => false,
    }
}

/// Health problems of an installed native component (empty when healthy).
pub fn health(ctx: &Context, state: &State) -> Result<Vec<String>, Error> {
    let manifest = &state.manifest;
    let mut problems = Vec::new();
    let mut check_file =
        |device_path: &str, hash: &str, mode: u32, smack: &str| -> Result<(), Error> {
            let path = ctx.path(device_path);
            match inspect(&path)? {
                Current::File { sha256, .. } if sha256 != hash => {
                    problems.push(format!("{device_path}: modified"))
                }
                Current::File { mode: actual, .. } if actual != mode => problems.push(format!(
                    "{device_path}: mode {actual:04o}, expected {mode:04o}"
                )),
                Current::File { uid, gid, .. } if ctx.apply_security && (uid, gid) != (0, 0) => {
                    problems.push(format!("{device_path}: owned by {uid}:{gid}"))
                }
                Current::File { .. }
                    if ctx.apply_security && smack_label(&path)?.as_deref() != Some(smack) =>
                {
                    problems.push(format!("{device_path}: wrong SMACK label"))
                }
                Current::File { .. } => {}
                Current::Missing => problems.push(format!("{device_path}: missing")),
                _ => problems.push(format!("{device_path}: not a regular file")),
            }
            Ok(())
        };
    for file in &manifest.files {
        check_file(&file.destination, &file.sha256, file.mode.0, &file.smack)?;
    }
    for unit in &manifest.units {
        check_file(&unit.destination(), &unit.sha256, UNIT_MODE, UNIT_SMACK)?;
    }
    for link in &manifest.links {
        if inspect(&ctx.path(&link.path))? != Current::Link(link.target.clone()) {
            problems.push(format!("{}: link is not -> {}", link.path, link.target));
        }
    }
    let systemd = Systemd(ctx.runner);
    for unit in &manifest.units {
        let runtime = format!("{RUNTIME_UNIT_DIR}/{}", unit.name);
        if !matches!(inspect(&ctx.path(&runtime))?, Current::File { sha256, .. } if sha256 == unit.sha256)
        {
            problems.push(format!("{runtime}: missing or stale"));
        }
        if !systemd.is_active(&unit.name)? {
            problems.push(format!("{}: not active", unit.name));
        }
    }
    Ok(problems)
}

/// Returns whether anything changed.
pub fn install(
    ctx: &Context,
    archive: &mut Archive,
    previous: Option<&State>,
    others: &[State],
    adopt_existing: bool,
) -> Result<bool, Error> {
    let manifest = archive.manifest.clone();
    let new_state = State {
        digest: archive.digest.clone(),
        manifest: manifest.clone(),
    };
    if let Some(previous) = previous {
        if previous.digest == new_state.digest && health(ctx, previous)?.is_empty() {
            return Ok(false);
        }
    }

    // Checks: nothing below here may touch the live tree until staging is done.
    let empty = Manifest {
        files: vec![],
        links: vec![],
        units: vec![],
        ..manifest.clone()
    };
    let previous_manifest = previous.map_or(&empty, |state| &state.manifest);
    let recorded = expected(previous_manifest);
    let new_paths: HashSet<String> = manifest.owned_paths().into_iter().collect();
    for other in others {
        if let Some(path) = other
            .manifest
            .owned_paths()
            .iter()
            .find(|p| new_paths.contains(*p))
        {
            return Err(Error::conflict(format!(
                "{path} is owned by {}",
                other.manifest.id
            )));
        }
    }
    for path in manifest.owned_paths() {
        match (inspect(&ctx.path(&path))?, recorded.get(&path)) {
            (Current::Missing, _) => {}
            (Current::Other, _) => {
                return Err(Error::conflict(format!("{path} is not a file or link")))
            }
            (current, Some(expected)) if matches(expected, &current) => {}
            _ if adopt_existing => {}
            (_, Some(_)) => {
                return Err(Error::conflict(format!(
                    "{path} was modified; use --adopt-existing to replace it"
                )))
            }
            (_, None) => {
                return Err(Error::conflict(format!(
                    "{path} exists without tv-pkg ownership; use --adopt-existing to take it over"
                )))
            }
        }
    }
    let stale: Vec<String> = previous_manifest
        .owned_paths()
        .into_iter()
        .filter(|path| !new_paths.contains(path))
        .collect();
    for path in &stale {
        let current = inspect(&ctx.path(path))?;
        if current != Current::Missing && !matches(&recorded[path], &current) {
            return Err(Error::conflict(format!(
                "{path} was modified; refusing to remove it"
            )));
        }
    }
    check_parents(ctx, &manifest)?;
    let new_units: HashSet<&str> = manifest.units.iter().map(|u| u.name.as_str()).collect();
    let stale_units: Vec<String> = previous_manifest
        .units
        .iter()
        .filter(|unit| !new_units.contains(unit.name.as_str()))
        .map(|unit| unit.name.clone())
        .collect();

    let mut txn = Transaction::begin(ctx, &new_state)?;
    let staged = match stage(ctx, archive, &txn.dir) {
        Ok(staged) => staged,
        Err(error) => {
            let _ = fs::remove_dir_all(&txn.dir);
            return Err(error);
        }
    };
    let applied = apply(ctx, &mut txn, &new_state, &staged, &stale, &stale_units);
    match applied {
        Ok(()) => {
            let _ = fs::remove_dir_all(&txn.dir);
            Ok(true)
        }
        Err(error) => Err(txn.rollback(error)),
    }
}

/// Every parent of a destination must already be a real directory or be
/// declared; declared directories must not be symlinks.
fn check_parents(ctx: &Context, manifest: &Manifest) -> Result<(), Error> {
    let declared: HashSet<&str> = manifest
        .directories
        .iter()
        .map(|d| d.path.as_str())
        .collect();
    let mut paths = manifest.owned_paths();
    paths.extend(manifest.directories.iter().map(|d| d.path.clone()));
    for path in &paths {
        let mut ancestors: Vec<&Path> = Path::new(path).ancestors().skip(1).collect();
        ancestors.pop(); // "/"
        for ancestor in ancestors.into_iter().rev() {
            let text = ancestor.to_str().expect("UTF-8 manifest path");
            match fs::symlink_metadata(ctx.path(text)) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(Error::conflict(format!("{text} is not a real directory"))),
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && declared.contains(text) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Err(Error::conflict(format!(
                        "{text} does not exist and is not declared"
                    )))
                }
                Err(error) => return Err(Error::io(format!("inspect {text}"), error)),
            }
        }
    }
    for directory in &manifest.directories {
        match fs::symlink_metadata(ctx.path(&directory.path)) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(Error::conflict(format!(
                    "{} is not a real directory",
                    directory.path
                )))
            }
            _ => {}
        }
    }
    Ok(())
}

struct Staged {
    files: Vec<PathBuf>,
    links: Vec<PathBuf>,
    units: Vec<(PathBuf, Vec<u8>)>,
}

/// Extract and hash every file into the transaction directory with its final
/// owner, mode and label.
fn stage(ctx: &Context, archive: &mut Archive, dir: &Path) -> Result<Staged, Error> {
    let manifest = archive.manifest.clone();
    let mut extract = |name: String,
                       source: &str,
                       hash: &str,
                       mode: u32,
                       smack: &str|
     -> Result<PathBuf, Error> {
        let path = dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| Error::io(format!("stage {source}"), e))?;
        archive.copy_verified(source, hash, &mut file)?;
        file.sync_all()
            .map_err(|e| Error::io(format!("stage {source}"), e))?;
        set_mode(&path, mode)?;
        secure(ctx, &path, Some(smack))?;
        Ok(path)
    };
    let mut staged = Staged {
        files: vec![],
        links: vec![],
        units: vec![],
    };
    for (index, file) in manifest.files.iter().enumerate() {
        staged.files.push(extract(
            format!("file-{index}"),
            &file.source,
            &file.sha256,
            file.mode.0,
            &file.smack,
        )?);
    }
    for (index, unit) in manifest.units.iter().enumerate() {
        let path = extract(
            format!("unit-{index}"),
            &unit.source,
            &unit.sha256,
            UNIT_MODE,
            UNIT_SMACK,
        )?;
        let bytes = fs::read(&path).map_err(|e| Error::io("read staged unit", e))?;
        staged.units.push((path, bytes));
    }
    for (index, link) in manifest.links.iter().enumerate() {
        let path = dir.join(format!("link-{index}"));
        std::os::unix::fs::symlink(&link.target, &path).map_err(|e| Error::io("stage link", e))?;
        secure(ctx, &path, None)?;
        staged.links.push(path);
    }
    Ok(staged)
}

fn apply(
    ctx: &Context,
    txn: &mut Transaction,
    new_state: &State,
    staged: &Staged,
    stale: &[String],
    stale_units: &[String],
) -> Result<(), Error> {
    let manifest = &new_state.manifest;
    let systemd = Systemd(ctx.runner);
    let declared_units = manifest.units.iter().map(|u| u.name.clone());
    for name in declared_units.chain(stale_units.iter().cloned()).rev() {
        if systemd.is_active(&name)? {
            txn.journal.was_running.push(name.clone());
            txn.save()?;
            systemd.stop(&name)?;
        }
    }

    let mut directories = manifest.directories.clone();
    directories.sort_by_key(|d| d.path.matches('/').count());
    for directory in &directories {
        let live = ctx.path(&directory.path);
        match fs::symlink_metadata(&live) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                txn.record(Undo::RemoveDir(live.clone()))?;
                DirBuilder::new()
                    .mode(0o700)
                    .create(&live)
                    .map_err(|e| Error::io(format!("create {}", directory.path), e))?;
            }
            Err(error) => return Err(Error::io(format!("inspect {}", directory.path), error)),
            Ok(metadata) => {
                // ponytail: owner and label of an existing directory are not
                // rolled back, only its mode.
                txn.record(Undo::Mode(live.clone(), metadata.mode() & 0o7777))?;
            }
        }
        set_mode(&live, directory.mode.0)?;
        secure(ctx, &live, Some(&directory.smack))?;
    }

    let files = manifest
        .files
        .iter()
        .map(|f| f.destination.clone())
        .zip(&staged.files);
    let units = manifest
        .units
        .iter()
        .map(|u| u.destination())
        .zip(staged.units.iter().map(|(p, _)| p));
    let links = manifest
        .links
        .iter()
        .map(|l| l.path.clone())
        .zip(&staged.links);
    for (destination, staged_path) in files.chain(units).chain(links) {
        txn.replace(&ctx.path(&destination), staged_path)?;
    }
    for path in stale {
        txn.retire(&ctx.path(path))?;
    }

    let runtime_dir = ctx.path(RUNTIME_UNIT_DIR);
    fs::create_dir_all(&runtime_dir).map_err(|e| Error::io("create runtime unit directory", e))?;
    for (unit, (_, bytes)) in manifest.units.iter().zip(&staged.units) {
        txn.write_runtime(&runtime_dir.join(&unit.name), Some(bytes))?;
    }
    for name in stale_units {
        txn.write_runtime(&runtime_dir.join(name), None)?;
    }
    if !manifest.units.is_empty() || !stale_units.is_empty() {
        systemd.daemon_reload()?;
    }
    for unit in &manifest.units {
        systemd.start(&unit.name)?;
    }
    wait_settled(ctx, manifest)?;

    let problems = health(ctx, new_state)?;
    if !problems.is_empty() {
        return Err(Error::failed(format!(
            "post-install check failed: {}",
            problems.join("; ")
        )));
    }
    // Committing: recovery treats a journal whose digest is recorded as done.
    state::save(ctx, new_state)
}

/// A unit that dies (or enters a restart loop) within `ctx.settle` of being
/// started fails the install.
fn wait_settled(ctx: &Context, manifest: &Manifest) -> Result<(), Error> {
    let systemd = Systemd(ctx.runner);
    let deadline = Instant::now() + ctx.settle;
    loop {
        for unit in &manifest.units {
            if !systemd.is_active(&unit.name)? {
                return Err(Error::failed(format!(
                    "{} stopped right after starting",
                    unit.name
                )));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        thread::sleep((deadline - now).min(Duration::from_millis(500)));
    }
}

/// One reversible step. Every step is written to the journal before it is
/// performed, so undoing a step that never happened must be harmless.
#[derive(Debug, Serialize, Deserialize)]
enum Undo {
    /// Move the backup back over the live path.
    Restore {
        live: PathBuf,
        backup: PathBuf,
    },
    /// The path did not exist before.
    Remove(PathBuf),
    RemoveDir(PathBuf),
    Mode(PathBuf, u32),
    /// Runtime unit copy on /run, with a backup of the previous copy if any.
    Runtime {
        live: PathBuf,
        backup: Option<PathBuf>,
    },
}

/// `journal.json` in the transaction directory.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    id: String,
    /// Digest being installed; once the state records it, the transaction
    /// committed and only the directory is left to delete.
    digest: String,
    /// Units the new version starts, stopped again on rollback.
    units: Vec<String>,
    was_running: Vec<String>,
    undo: Vec<Undo>,
}

const JOURNAL: &str = "journal.json";

/// A private directory on /opt holding staged files, backups and the journal.
struct Transaction<'a> {
    ctx: &'a Context<'a>,
    dir: PathBuf,
    journal: Journal,
}

impl<'a> Transaction<'a> {
    fn begin(ctx: &'a Context<'a>, state: &State) -> Result<Self, Error> {
        let dir = ctx
            .path(STATE_DIR)
            .join(format!("txn-{}", std::process::id()));
        state::create_private_dir(dir.parent().expect("state directory"))?;
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| Error::io(format!("create {}", dir.display()), e))?;
        let units = state
            .manifest
            .units
            .iter()
            .map(|u| u.name.clone())
            .collect();
        Ok(Self {
            ctx,
            dir,
            journal: Journal {
                id: state.manifest.id.clone(),
                digest: state.digest.clone(),
                units,
                was_running: Vec::new(),
                undo: Vec::new(),
            },
        })
    }

    fn save(&self) -> Result<(), Error> {
        let bytes = serde_json::to_vec(&self.journal).expect("journal serializes");
        state::write_atomic(&self.dir.join(JOURNAL), &bytes, 0o600)
    }

    /// Journal `step` before the caller performs it.
    fn record(&mut self, step: Undo) -> Result<(), Error> {
        self.journal.undo.push(step);
        self.save()
    }

    fn backup_path(&self) -> PathBuf {
        self.dir.join(format!("backup-{}", self.journal.undo.len()))
    }

    /// Back up whatever is at `live`, then rename `staged` into place.
    fn replace(&mut self, live: &Path, staged: &Path) -> Result<(), Error> {
        self.retire(live)?;
        fs::rename(staged, live).map_err(|e| Error::io(format!("install {}", live.display()), e))
    }

    /// Move whatever is at `live` into the transaction backup.
    fn retire(&mut self, live: &Path) -> Result<(), Error> {
        if fs::symlink_metadata(live).is_err() {
            return self.record(Undo::Remove(live.to_owned()));
        }
        let backup = self.backup_path();
        self.record(Undo::Restore {
            live: live.to_owned(),
            backup: backup.clone(),
        })?;
        fs::rename(live, &backup).map_err(|e| Error::io(format!("back up {}", live.display()), e))
    }

    fn write_runtime(&mut self, live: &Path, bytes: Option<&[u8]>) -> Result<(), Error> {
        let backup = self.backup_path();
        let backup = match fs::copy(live, &backup) {
            Ok(_) => Some(backup),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::io(format!("back up {}", live.display()), error)),
        };
        self.record(Undo::Runtime {
            live: live.to_owned(),
            backup,
        })?;
        match bytes {
            Some(bytes) => state::write_atomic(live, bytes, RUNTIME_UNIT_MODE),
            None => remove_if_present(live),
        }
    }

    /// Undo the transaction and return `error`, noting any restoration
    /// problems.
    fn rollback(self, error: Error) -> Error {
        match self.undo() {
            Ok(()) => error,
            Err(problems) => Error {
                code: error.code,
                message: format!("{}; {problems}", error.message),
            },
        }
    }

    /// Undo every recorded step and restore the previous unit state. Backups
    /// are kept, and the problems returned, if anything could not be restored.
    fn undo(self) -> Result<(), String> {
        let systemd = Systemd(self.ctx.runner);
        let mut problems = Vec::new();
        for unit in self.journal.units.iter().rev() {
            if systemd.is_active(unit).unwrap_or(true) {
                if let Err(e) = systemd.stop(unit) {
                    problems.push(e.message);
                }
            }
        }
        for step in self.journal.undo.into_iter().rev() {
            let result = match step {
                // A missing backup means the crash came before the move.
                Undo::Restore { live, backup } => match fs::rename(&backup, &live) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => {
                        Err(Error::io(format!("restore {}", live.display()), e))
                    }
                    _ => Ok(()),
                },
                Undo::Remove(live) => remove_if_present(&live),
                Undo::RemoveDir(live) => match fs::remove_dir(&live) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => {
                        Err(Error::io(format!("remove {}", live.display()), e))
                    }
                    _ => Ok(()),
                },
                Undo::Mode(live, mode) => set_mode(&live, mode),
                Undo::Runtime {
                    live,
                    backup: Some(backup),
                } => fs::read(&backup)
                    .map_err(|e| Error::io(format!("read {}", backup.display()), e))
                    .and_then(|bytes| state::write_atomic(&live, &bytes, RUNTIME_UNIT_MODE)),
                Undo::Runtime { live, backup: None } => remove_if_present(&live),
            };
            if let Err(e) = result {
                problems.push(e.message);
            }
        }
        if let Err(e) = systemd.daemon_reload() {
            problems.push(e.message);
        }
        for name in self.journal.was_running.iter().rev() {
            if let Err(e) = systemd.start(name) {
                problems.push(e.message);
            }
        }
        if problems.is_empty() {
            let _ = fs::remove_dir_all(&self.dir);
            return Ok(());
        }
        Err(format!(
            "rollback incomplete ({}); backups kept in {}",
            problems.join("; "),
            self.dir.display()
        ))
    }
}

/// Finish transactions left by a tv-pkg process that died (the caller holds
/// the lock, so no live process owns one): roll back uncommitted ones and
/// delete committed ones. Fails closed if anything cannot be restored.
pub fn recover(ctx: &Context) -> Result<(), Error> {
    let parent = ctx.path(STATE_DIR);
    let entries = match fs::read_dir(&parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::io(format!("read {}", parent.display()), error)),
    };
    for entry in entries {
        let dir = entry
            .map_err(|e| Error::io("read state directory", e))?
            .path();
        if !dir
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("txn-"))
        {
            continue;
        }
        let journal = match fs::read(dir.join(JOURNAL)) {
            // Nothing outside the directory was touched yet.
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::io(format!("read {}", dir.display()), error)),
            Ok(bytes) => Some(serde_json::from_slice::<Journal>(&bytes).map_err(|e| {
                Error::invalid(format!(
                    "interrupted transaction {} is malformed: {e}",
                    dir.display()
                ))
            })?),
        };
        let committed = match &journal {
            None => true,
            Some(journal) => {
                state::load(ctx, &journal.id)?.is_some_and(|state| state.digest == journal.digest)
            }
        };
        match journal {
            Some(journal) if !committed => {
                let id = journal.id.clone();
                Transaction { ctx, dir, journal }
                    .undo()
                    .map_err(|problems| {
                        Error::failed(format!("interrupted {id} install: {problems}"))
                    })?;
                eprintln!("tv-pkg: rolled back an interrupted {id} install");
            }
            _ => fs::remove_dir_all(&dir)
                .map_err(|e| Error::io(format!("remove {}", dir.display()), e))?,
        }
    }
    Ok(())
}

pub fn remove(ctx: &Context, state: &State) -> Result<(), Error> {
    let manifest = &state.manifest;
    let recorded = expected(manifest);
    for (path, expected) in &recorded {
        let current = inspect(&ctx.path(path))?;
        if current != Current::Missing && !matches(expected, &current) {
            return Err(Error::conflict(format!(
                "{path} was modified; refusing to remove it"
            )));
        }
    }
    if let Some(argv) = &manifest.undo_command {
        let code = ctx
            .runner
            .run(argv)
            .map_err(|e| Error::io(format!("run {}", argv[0]), e))?;
        if code != 0 {
            return Err(Error::failed(format!("undo command exited with {code}")));
        }
    }
    let systemd = Systemd(ctx.runner);
    for unit in manifest.units.iter().rev() {
        if systemd.is_active(&unit.name)? {
            systemd.stop(&unit.name)?;
        }
        remove_if_present(&ctx.path(&format!("{RUNTIME_UNIT_DIR}/{}", unit.name)))?;
    }
    for path in recorded.keys() {
        remove_if_present(&ctx.path(path))?;
    }
    let mut directories: Vec<&str> = manifest
        .directories
        .iter()
        .map(|d| d.path.as_str())
        .collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
    for directory in directories {
        // Only empty directories go; anything unlisted inside keeps it.
        let _ = fs::remove_dir(ctx.path(directory));
    }
    if !manifest.units.is_empty() {
        systemd.daemon_reload()?;
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(Error::io(format!("remove {}", path.display()), error))
        }
        _ => Ok(()),
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| Error::io(format!("chmod {}", path.display()), e))
}

/// Owner 0:0 and, when given, the SMACK access label. Does not follow a final
/// symlink. A no-op unless `ctx.apply_security`.
fn secure(ctx: &Context, path: &Path, smack: Option<&str>) -> Result<(), Error> {
    if !ctx.apply_security {
        return Ok(());
    }
    let c_path = c_path(path)?;
    // SAFETY: valid NUL-terminated path.
    if unsafe { libc::lchown(c_path.as_ptr(), 0, 0) } != 0 {
        return Err(Error::io(
            format!("chown {}", path.display()),
            io::Error::last_os_error(),
        ));
    }
    match smack {
        Some(label) => {
            set_smack(&c_path, label).map_err(|e| Error::io(format!("label {}", path.display()), e))
        }
        None => Ok(()),
    }
}

fn c_path(path: &Path) -> Result<std::ffi::CString, Error> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::invalid("path contains NUL"))
}

#[cfg(target_os = "linux")]
fn set_smack(path: &std::ffi::CStr, label: &str) -> io::Result<()> {
    // SAFETY: valid path, attribute name and value buffer.
    let result = unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            c"security.SMACK64".as_ptr(),
            label.as_ptr().cast(),
            label.len(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn set_smack(_: &std::ffi::CStr, _: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SMACK needs Linux",
    ))
}

fn smack_label(path: &Path) -> Result<Option<String>, Error> {
    let c_path = c_path(path)?;
    read_smack(&c_path).map_err(|e| Error::io(format!("read label {}", path.display()), e))
}

#[cfg(target_os = "linux")]
fn read_smack(path: &std::ffi::CStr) -> io::Result<Option<String>> {
    let mut buffer = [0_u8; 256];
    // SAFETY: valid path and attribute name; the buffer length is passed.
    let length = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            c"security.SMACK64".as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if length < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENODATA) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let label = &buffer[..length as usize];
    let label = label.strip_suffix(b"\0").unwrap_or(label);
    Ok(Some(String::from_utf8_lossy(label).into_owned()))
}

#[cfg(not(target_os = "linux"))]
fn read_smack(_: &std::ffi::CStr) -> io::Result<Option<String>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SMACK needs Linux",
    ))
}
