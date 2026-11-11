//! The `sync:` file-transfer service: records of a four-byte id, a
//! little-endian `u32`, and an optional payload.

use crate::connection::Device;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const CHUNK: usize = 64 * 1024;
/// Samsung's client sends every file as 0755, so pushed scripts and binaries
/// stay executable; we do the same rather than copy host permissions.
const MODE: u32 = 0o100_755;
const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;

fn record(id: &[u8; 4], arg: u32, payload: &[u8]) -> Vec<u8> {
    [id.as_slice(), &arg.to_le_bytes(), payload].concat()
}

fn read_header(ch: &mut impl Read) -> io::Result<([u8; 4], u32)> {
    let mut h = [0u8; 8];
    ch.read_exact(&mut h)?;
    Ok((
        h[..4].try_into().unwrap(),
        u32::from_le_bytes(h[4..].try_into().unwrap()),
    ))
}

/// Turns a `FAIL` record (or anything unexpected) into an error.
fn fail(ch: &mut impl Read, id: [u8; 4], len: u32) -> io::Error {
    if &id != b"FAIL" {
        return io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected sync record {:?}", String::from_utf8_lossy(&id)),
        );
    }
    let mut msg = Vec::new();
    match ch.take(len.min(CHUNK as u32).into()).read_to_end(&mut msg) {
        Ok(_) => io::Error::other(format!("TV: {}", String::from_utf8_lossy(&msg).trim())),
        Err(e) => e,
    }
}

/// Uploads one file, returning the remote path written. A remote directory
/// gets the local file name appended.
pub fn push(device: &mut Device, local: &Path, remote: &str) -> io::Result<String> {
    let mut file = File::open(local)?;
    let mtime = file
        .metadata()?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
        })
        .as_secs() as u32;
    let mut ch = device.open("sync:")?;

    ch.write_all(&record(b"STAT", remote.len() as u32, remote.as_bytes()))?;
    let (id, mode) = read_header(&mut ch)?;
    let mut rest = [0u8; 8]; // size, mtime
    ch.read_exact(&mut rest)?;
    if &id != b"STAT" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad STAT reply"));
    }
    let mut remote = remote.to_owned();
    if mode & S_IFMT == S_IFDIR {
        let name = local
            .file_name()
            .ok_or_else(|| io::Error::other("local path has no file name"))?;
        remote = format!(
            "{}/{}",
            remote.trim_end_matches('/'),
            name.to_string_lossy()
        );
    }

    let target = format!("{remote},{MODE}");
    ch.write_all(&record(b"SEND", target.len() as u32, target.as_bytes()))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ch.write_all(&record(b"DATA", n as u32, &buf[..n]))?;
    }
    ch.write_all(&record(b"DONE", mtime, &[]))?;
    match read_header(&mut ch)? {
        (id, _) if &id == b"OKAY" => {}
        (id, len) => return Err(fail(&mut ch, id, len)),
    }
    let _ = ch.write_all(&record(b"QUIT", 0, &[]));
    Ok(remote)
}

/// Downloads one file into a temporary sibling, renamed into place only once
/// the TV reports `DONE`. Samsung's sdbd only allows a few paths, e.g.
/// `/opt/usr/apps/<pkg>/author-signature.xml`.
pub fn pull(device: &mut Device, remote: &str, local: &Path) -> io::Result<u64> {
    let mut ch = device.open("sync:")?;
    ch.write_all(&record(b"RECV", remote.len() as u32, remote.as_bytes()))?;

    let tmp = local.with_file_name(format!(
        ".{}.tv-sdb-part",
        local
            .file_name()
            .ok_or_else(|| io::Error::other("local path has no file name"))?
            .to_string_lossy()
    ));
    let result = (|| {
        let mut out = File::create(&tmp)?;
        let mut total = 0u64;
        loop {
            match read_header(&mut ch)? {
                (id, len) if &id == b"DATA" => {
                    if len as usize > CHUNK {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("DATA record of {len} bytes"),
                        ));
                    }
                    let copied = io::copy(&mut (&mut ch).take(len.into()), &mut out)?;
                    if copied != len as u64 {
                        return Err(io::Error::other("TV closed the channel mid-record"));
                    }
                    total += copied;
                }
                (id, _) if &id == b"DONE" => break,
                (id, len) => return Err(fail(&mut ch, id, len)),
            }
        }
        out.sync_all()?;
        Ok(total)
    })();
    match result {
        Ok(total) => {
            let _ = ch.write_all(&record(b"QUIT", 0, &[]));
            std::fs::rename(&tmp, local)?;
            Ok(total)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}
