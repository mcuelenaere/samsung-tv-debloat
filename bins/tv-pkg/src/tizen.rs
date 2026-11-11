//! TPK/WGT components: delegated to tv-authd's `pkg.*` methods.

use crate::archive::Archive;
use crate::state::{self, State};
use crate::{Context, Error};
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// tv-authd only stages packages from a few roots; /tmp is one of them.
pub const EXTRACT_ROOT: &str = "/tmp";

pub trait Broker {
    fn call(&self, request: &Value) -> io::Result<Value>;
}

pub struct SocketBroker(pub PathBuf);

impl Broker for SocketBroker {
    fn call(&self, request: &Value) -> io::Result<Value> {
        let bytes = tv_proto::call(&self.0, request)?;
        serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// Send one request; the response may still carry an error status.
fn request(ctx: &Context, method: &str, fields: Value) -> Result<Value, Error> {
    let mut request = json!({
        "version": tv_proto::PACKAGE_PROTOCOL_VERSION,
        "id": format!("tv-pkg-{}", std::process::id()),
        "method": method,
    });
    request
        .as_object_mut()
        .expect("object")
        .extend(fields.as_object().expect("object").clone());
    ctx.broker
        .call(&request)
        .map_err(|e| Error::io(format!("tv-authd {method}"), e))
}

fn call(ctx: &Context, method: &str, fields: Value) -> Result<Value, Error> {
    checked(method, request(ctx, method, fields)?)
}

fn checked(method: &str, response: Value) -> Result<Value, Error> {
    if response["status"] != "ok" {
        let detail = response["detail"]
            .as_str()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();
        return Err(Error {
            code: "failed",
            message: format!(
                "tv-authd {method}: {}{detail}",
                response["error"].as_str().unwrap_or("error")
            ),
        });
    }
    Ok(response)
}

pub fn health(ctx: &Context, state: &State) -> Result<Vec<String>, Error> {
    let response = call(ctx, "pkg.list", json!({}))?;
    let kind = state.manifest.kind.as_str();
    let installed = response["packages"].as_array().is_some_and(|packages| {
        packages
            .iter()
            .any(|p| p["pkgid"] == state.manifest.id.as_str() && p["pkg_type"] == kind)
    });
    Ok(if installed {
        Vec::new()
    } else {
        vec![format!("{} is not installed as {kind}", state.manifest.id)]
    })
}

pub fn install(
    ctx: &Context,
    archive: &mut Archive,
    previous: Option<&State>,
) -> Result<bool, Error> {
    let new_state = State {
        digest: archive.digest.clone(),
        manifest: archive.manifest.clone(),
    };
    if let Some(previous) = previous {
        if previous.digest == new_state.digest && health(ctx, previous)?.is_empty() {
            return Ok(false);
        }
    }
    let package = new_state
        .manifest
        .package
        .as_ref()
        .expect("validated package");
    let kind = new_state.manifest.kind.as_str();
    let extract = ExtractDir::create(ctx)?;
    let path = extract.0.join(format!("package.{kind}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&path)
        .map_err(|e| Error::io("extract package", e))?;
    archive.copy_verified(&package.source, &package.sha256, &mut file)?;
    file.sync_all()
        .map_err(|e| Error::io("extract package", e))?;
    drop(file);

    let path_text = path
        .to_str()
        .ok_or_else(|| Error::invalid("non-UTF-8 extraction path"))?;
    let response = call(
        ctx,
        "pkg.install",
        json!({ "pkg_type": kind, "path": path_text }),
    )?;
    if response["pkgid"]
        .as_str()
        .is_some_and(|pkgid| pkgid != package.pkgid)
    {
        return Err(Error::failed(format!(
            "tv-authd installed {} instead of {}",
            response["pkgid"], package.pkgid
        )));
    }
    state::save(ctx, &new_state)?;
    Ok(true)
}

pub fn remove(ctx: &Context, state: &State) -> Result<(), Error> {
    let response = request(ctx, "pkg.uninstall", json!({ "pkgid": state.manifest.id }))?;
    // Already gone: only the local record is left.
    if response["error"] == "unknown_package" {
        return Ok(());
    }
    checked("pkg.uninstall", response).map(drop)
}

/// Private extraction directory, removed on every exit path.
struct ExtractDir(PathBuf);

impl ExtractDir {
    fn create(ctx: &Context) -> Result<Self, Error> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = ctx
            .path(EXTRACT_ROOT)
            .join(format!("tv-pkg-{}-{nanos:08x}", std::process::id()));
        // `create`, not `recursive`: fails if anything already sits there.
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| Error::io(format!("create {}", dir.display()), e))?;
        Ok(Self(dir))
    }
}

impl Drop for ExtractDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
