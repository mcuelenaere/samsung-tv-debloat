//! Installed state: one JSON file per component, written atomically.

use crate::archive::sha256_file;
use crate::manifest::{Manifest, STATE_DIR};
use crate::{Context, Error};
use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The installed manifest plus its digest. Owned destinations, installed
/// hashes, links, directories, units and the undo command all come from the
/// manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub digest: String,
    pub manifest: Manifest,
}

pub fn components_dir(ctx: &Context) -> PathBuf {
    ctx.path(STATE_DIR).join("components")
}

fn malformed(id: &str, detail: impl std::fmt::Display) -> Error {
    Error::invalid(format!("state for {id} is malformed: {detail}"))
}

pub fn load(ctx: &Context, id: &str) -> Result<Option<State>, Error> {
    if !tv_proto::is_valid_package_id(id) {
        return Err(Error::invalid(format!("invalid component id {id:?}")));
    }
    let path = components_dir(ctx).join(format!("{id}.json"));
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::io(format!("read {}", path.display()), error)),
    };
    let state: State = serde_json::from_slice(&bytes).map_err(|e| malformed(id, e))?;
    state.manifest.validate().map_err(|e| malformed(id, e))?;
    if state.manifest.id != id || state.digest.len() != 64 {
        return Err(malformed(id, "id or digest mismatch"));
    }
    Ok(Some(state))
}

/// Every recorded component, sorted by id. Any malformed file fails the call.
pub fn load_all(ctx: &Context) -> Result<Vec<State>, Error> {
    let dir = components_dir(ctx);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::io(format!("read {}", dir.display()), error)),
    };
    let mut states = Vec::new();
    for entry in entries {
        let name = entry
            .map_err(|e| Error::io("read state directory", e))?
            .file_name();
        let name = name.to_string_lossy();
        // Dot files are temporaries from an interrupted write.
        if let Some(id) = name
            .strip_suffix(".json")
            .filter(|_| !name.starts_with('.'))
        {
            states.extend(load(ctx, id)?);
        }
    }
    states.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(states)
}

pub fn save(ctx: &Context, state: &State) -> Result<(), Error> {
    let dir = components_dir(ctx);
    create_private_dir(&dir)?;
    let bytes = serde_json::to_vec_pretty(state).expect("state serializes");
    write_atomic(
        &dir.join(format!("{}.json", state.manifest.id)),
        &bytes,
        0o600,
    )
}

pub fn delete(ctx: &Context, id: &str) -> Result<(), Error> {
    let path = components_dir(ctx).join(format!("{id}.json"));
    fs::remove_file(&path).map_err(|e| Error::io(format!("remove {}", path.display()), e))
}

pub fn create_private_dir(path: &Path) -> Result<(), Error> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| Error::io(format!("create {}", path.display()), e))
}

/// Write a temporary dot-sibling, fsync it, then rename it over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    let name = path.file_name().expect("file path").to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    let result = (|| {
        let _ = fs::remove_file(&temporary);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    result.map_err(|error| {
        let _ = fs::remove_file(&temporary);
        Error::io(format!("write {}", path.display()), error)
    })
}

/// What is at a destination right now, without following a final symlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Current {
    Missing,
    File {
        sha256: String,
        mode: u32,
        uid: u32,
        gid: u32,
    },
    Link(String),
    /// A directory, device, socket, ...
    Other,
}

pub fn inspect(path: &Path) -> Result<Current, Error> {
    let context = || format!("inspect {}", path.display());
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Current::Missing),
        Err(error) => return Err(Error::io(context(), error)),
    };
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|e| Error::io(context(), e))?;
        return Ok(Current::Link(target.to_string_lossy().into_owned()));
    }
    if !metadata.is_file() {
        return Ok(Current::Other);
    }
    Ok(Current::File {
        sha256: sha256_file(path).map_err(|e| Error::io(context(), e))?,
        mode: metadata.mode() & 0o7777,
        uid: metadata.uid(),
        gid: metadata.gid(),
    })
}
