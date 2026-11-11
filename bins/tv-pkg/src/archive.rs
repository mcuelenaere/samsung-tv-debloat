//! Bounded `.tvpkg` (ZIP) reading and payload hashing.

use crate::manifest::Manifest;
use crate::Error;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

pub const MANIFEST_NAME: &str = "component.json";
pub const MAX_ENTRIES: usize = 256;
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// Matches tv-authd's largest accepted package.
pub const MAX_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;

pub struct Archive {
    zip: zip::ZipArchive<File>,
    pub manifest: Manifest,
    /// SHA-256 of `component.json`. Every payload entry is listed there with
    /// its own hash and unlisted entries are rejected, so this covers the
    /// whole package once [`Archive::verify`] has passed.
    pub digest: String,
}

impl Archive {
    /// Validate the entry list and the manifest. Payload hashes are checked
    /// separately by [`Archive::verify`] or while extracting.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let file =
            File::open(path).map_err(|e| Error::io(format!("open {}", path.display()), e))?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| Error::invalid(format!("not a ZIP archive: {e}")))?;
        if zip.len() > MAX_ENTRIES {
            return Err(Error::invalid(format!("more than {MAX_ENTRIES} entries")));
        }
        let mut names = HashSet::new();
        let mut payload = HashSet::new();
        let mut total = 0_u64;
        for index in 0..zip.len() {
            let entry = zip
                .by_index_raw(index)
                .map_err(|e| Error::invalid(format!("entry {index}: {e}")))?;
            let name = entry.name().to_owned();
            if !names.insert(name.clone()) {
                return Err(Error::invalid(format!("duplicate entry {name:?}")));
            }
            if entry.is_symlink() || entry.encrypted() {
                return Err(Error::invalid(format!("{name:?} is a link or encrypted")));
            }
            let (body, is_dir) = match name.strip_suffix('/') {
                Some(body) => (body, true),
                None => (name.as_str(), false),
            };
            let normal = !body.is_empty()
                && !body.contains(['\0', '\\'])
                && body.split('/').all(|part| !matches!(part, "" | "." | ".."));
            let placed = body == MANIFEST_NAME && !is_dir
                || body == "payload"
                || body.starts_with("payload/");
            if !normal || !placed || is_dir != entry.is_dir() {
                return Err(Error::invalid(format!("unexpected entry {name:?}")));
            }
            total = total.saturating_add(entry.size());
            if !is_dir && body != MANIFEST_NAME {
                payload.insert(name);
            }
        }
        if total > MAX_EXPANDED_BYTES {
            return Err(Error::invalid("package expands beyond the size limit"));
        }

        let mut bytes = Vec::new();
        let entry = zip
            .by_name(MANIFEST_NAME)
            .map_err(|_| Error::invalid("missing component.json"))?;
        entry
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::invalid(format!("component.json: {e}")))?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(Error::invalid("component.json is too large"));
        }
        let manifest = Manifest::parse(&bytes)?;
        let sources: HashSet<String> = manifest
            .sources()
            .iter()
            .map(|(s, _)| s.to_string())
            .collect();
        if let Some(missing) = sources.iter().find(|s| !payload.contains(*s)) {
            return Err(Error::invalid(format!("missing payload {missing}")));
        }
        if let Some(extra) = payload.iter().find(|p| !sources.contains(*p)) {
            return Err(Error::invalid(format!("unlisted payload {extra}")));
        }
        Ok(Self {
            zip,
            manifest,
            digest: hex(&Sha256::digest(&bytes)),
        })
    }

    /// Hash every payload entry against the manifest without writing.
    pub fn verify(&mut self) -> Result<(), Error> {
        let sources: Vec<(String, String)> = self
            .manifest
            .sources()
            .into_iter()
            .map(|(source, hash)| (source.to_owned(), hash.to_owned()))
            .collect();
        for (source, hash) in sources {
            self.copy_verified(&source, &hash, &mut io::sink())?;
        }
        Ok(())
    }

    /// Stream `source` into `output`, failing if its SHA-256 is not `expected`.
    pub fn copy_verified(
        &mut self,
        source: &str,
        expected: &str,
        output: &mut impl Write,
    ) -> Result<(), Error> {
        let entry = self
            .zip
            .by_name(source)
            .map_err(|e| Error::invalid(format!("{source}: {e}")))?;
        // The declared size was bounded in `open`; never read past it.
        let declared = entry.size();
        let mut reader = entry.take(declared.saturating_add(1));
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut read_total = 0_u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|e| Error::invalid(format!("{source}: {e}")))?;
            if read == 0 {
                break;
            }
            read_total += read as u64;
            if read_total > declared {
                return Err(Error::invalid(format!("{source} is larger than declared")));
            }
            hasher.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .map_err(|e| Error::io(format!("write {source}"), e))?;
        }
        if hex(&hasher.finalize()) != expected {
            return Err(Error::invalid(format!("{source}: SHA-256 mismatch")));
        }
        Ok(())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}
