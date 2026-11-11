//! SDB's outer packet: a 24-byte header of six little-endian `u32`s
//! (command, arg0, arg1, data length, additive data checksum,
//! `command ^ 0xffffffff`) followed by the payload.

use std::io::{self, Read};

pub const CNXN: u32 = u32::from_le_bytes(*b"CNXN");
pub const AUTH: u32 = u32::from_le_bytes(*b"AUTH");
pub const OPEN: u32 = u32::from_le_bytes(*b"OPEN");
pub const OKAY: u32 = u32::from_le_bytes(*b"OKAY");
pub const WRTE: u32 = u32::from_le_bytes(*b"WRTE");
pub const CLSE: u32 = u32::from_le_bytes(*b"CLSE");

/// Largest payload we offer in `CNXN` and accept from the peer. Samsung's own
/// client offers the same 256 KiB; the TV answers with slightly less.
pub const MAX_PAYLOAD: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub command: u32,
    pub arg0: u32,
    pub arg1: u32,
    pub data: Vec<u8>,
}

pub fn checksum(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |sum, &b| sum.wrapping_add(b.into()))
}

pub fn name(command: u32) -> String {
    String::from_utf8_lossy(&command.to_le_bytes()).into_owned()
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl Frame {
    pub fn new(command: u32, arg0: u32, arg1: u32, data: impl Into<Vec<u8>>) -> Self {
        Frame {
            command,
            arg0,
            arg1,
            data: data.into(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24 + self.data.len());
        for field in [
            self.command,
            self.arg0,
            self.arg1,
            self.data.len() as u32,
            checksum(&self.data),
            self.command ^ 0xffff_ffff,
        ] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&self.data);
        out
    }

    /// Reads exactly one frame, validating the header before allocating the
    /// payload and the checksum after reading it.
    pub fn read(reader: &mut impl Read) -> io::Result<Frame> {
        let mut header = [0u8; 24];
        reader.read_exact(&mut header)?;
        let field = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
        let (command, len, sum, magic) = (field(0), field(3) as usize, field(4), field(5));
        if magic != command ^ 0xffff_ffff {
            return Err(invalid(format!(
                "bad magic {magic:#010x} for command {command:#010x}"
            )));
        }
        if ![CNXN, AUTH, OPEN, OKAY, WRTE, CLSE].contains(&command) {
            return Err(invalid(format!("unknown command {command:#010x}")));
        }
        if len > MAX_PAYLOAD {
            return Err(invalid(format!(
                "{} payload of {len} bytes exceeds {MAX_PAYLOAD}",
                name(command)
            )));
        }
        let mut data = vec![0u8; len];
        reader.read_exact(&mut data)?;
        // adb-derived peers may send 0 instead of a checksum; Samsung's sdbd sends the real sum.
        if sum != 0 && sum != checksum(&data) {
            return Err(invalid(format!("bad checksum on {} frame", name(command))));
        }
        Ok(Frame {
            command,
            arg0: field(1),
            arg1: field(2),
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hands out one byte per read, like a maximally fragmented TCP stream.
    struct Trickle<'a>(&'a [u8]);
    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.0.len().min(buf.len()).min(1);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn golden_open_frame() {
        // Captured from Samsung's sdb: OPEN 0x1b5 0 "capability:\0".
        let bytes = Frame::new(OPEN, 0x1b5, 0, *b"capability:\0").encode();
        assert_eq!(&bytes[..4], b"OPEN");
        assert_eq!(bytes[4..8], 0x1b5u32.to_le_bytes());
        assert_eq!(bytes[12..16], 12u32.to_le_bytes());
        assert_eq!(bytes[16..20], 0x45cu32.to_le_bytes());
        assert_eq!(bytes[20..24], (OPEN ^ 0xffff_ffff).to_le_bytes());
        assert_eq!(&bytes[24..], b"capability:\0");
    }

    #[test]
    fn fragmented_and_coalesced_input() {
        let a = Frame::new(WRTE, 2, 7, *b"hello");
        let b = Frame::new(CLSE, 0, 7, Vec::new());
        let stream = [a.encode(), b.encode()].concat();
        let mut reader = Trickle(&stream);
        assert_eq!(Frame::read(&mut reader).unwrap(), a);
        assert_eq!(Frame::read(&mut reader).unwrap(), b);
        assert!(Frame::read(&mut reader).is_err());
    }

    #[test]
    fn rejects_corrupt_frames() {
        let good = Frame::new(WRTE, 1, 2, *b"data").encode();
        let corrupt = |at: usize, value: u8| {
            let mut bytes = good.clone();
            bytes[at] = value;
            Frame::read(&mut bytes.as_slice()).unwrap_err().kind()
        };
        assert_eq!(corrupt(20, 0), io::ErrorKind::InvalidData); // magic
        assert_eq!(corrupt(24, b'X'), io::ErrorKind::InvalidData); // checksum
        let unknown = Frame::new(u32::from_le_bytes(*b"SYNC"), 0, 0, Vec::new()).encode();
        assert_eq!(
            Frame::read(&mut unknown.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut huge = Frame::new(WRTE, 0, 0, Vec::new()).encode();
        huge[12..16].copy_from_slice(&(MAX_PAYLOAD as u32 + 1).to_le_bytes());
        assert_eq!(
            Frame::read(&mut huge.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
