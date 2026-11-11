//! One TCP connection to a TV's sdbd and the single channel open on it.

use crate::frame::{self, Frame, AUTH, CLSE, CNXN, MAX_PAYLOAD, OKAY, OPEN, WRTE};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 26101;
const VERSION: u32 = 0x0100_0000;
const CONNECT_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone)]
pub struct Options {
    /// Applies to connecting and to every individual socket read and write.
    pub timeout: Duration,
    /// Logs frame headers (never payloads) to stderr.
    pub verbose: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            timeout: Duration::from_secs(30),
            verbose: false,
        }
    }
}

pub struct Device {
    stream: TcpStream,
    /// Device banner from the TV's `CNXN`, e.g. `device::QE55S95BATXXH::0`.
    pub banner: String,
    max_data: usize,
    next_id: u32,
    verbose: bool,
}

fn other(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// `tv` or `tv:port`; the port defaults to 26101.
pub fn with_default_port(target: &str) -> String {
    match target.rsplit_once(':') {
        Some((_, port)) if port.parse::<u16>().is_ok() => target.to_owned(),
        _ => format!("{target}:{DEFAULT_PORT}"),
    }
}

impl Device {
    /// Connects and negotiates. Samsung's sdbd sometimes drops a fresh
    /// connection mid-handshake right after a previous one closed, so the
    /// handshake (and only the handshake) is retried on a new socket.
    pub fn connect(target: &str, options: &Options) -> io::Result<Device> {
        let target = with_default_port(target);
        let mut attempt = 1;
        loop {
            match Self::connect_once(&target, options) {
                Ok(device) => return Ok(device),
                Err(e) if attempt < CONNECT_ATTEMPTS && e.kind() != io::ErrorKind::InvalidData => {
                    if options.verbose {
                        eprintln!("connect attempt {attempt} failed: {e}; retrying");
                    }
                    std::thread::sleep(Duration::from_millis(400));
                    attempt += 1;
                }
                Err(e) => return Err(io::Error::new(e.kind(), format!("{target}: {e}"))),
            }
        }
    }

    fn connect_once(target: &str, options: &Options) -> io::Result<Device> {
        let mut last = other("no addresses");
        for addr in target.to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, options.timeout) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(options.timeout))?;
                    stream.set_write_timeout(Some(options.timeout))?;
                    stream.set_nodelay(true)?;
                    let mut device = Device {
                        stream,
                        banner: String::new(),
                        max_data: MAX_PAYLOAD,
                        next_id: 1,
                        verbose: options.verbose,
                    };
                    device.negotiate()?;
                    return Ok(device);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn negotiate(&mut self) -> io::Result<()> {
        self.send(Frame::new(CNXN, VERSION, MAX_PAYLOAD as u32, *b"host::\0"))?;
        let reply = self.recv()?;
        match reply.command {
            CNXN => {
                if reply.arg1 != 0 {
                    self.max_data = self.max_data.min(reply.arg1 as usize);
                }
                self.banner = String::from_utf8_lossy(&reply.data)
                    .trim_end_matches('\0')
                    .to_owned();
                Ok(())
            }
            // ponytail: no ADB-style RSA key auth; Samsung TVs answer CNXN directly. Add it when a TV asks.
            AUTH => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported negotiation: TV requested AUTH type {}",
                    reply.arg0
                ),
            )),
            other_cmd => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported negotiation: expected CNXN, got {}",
                    frame::name(other_cmd)
                ),
            )),
        }
    }

    fn send(&mut self, frame: Frame) -> io::Result<()> {
        if self.verbose {
            eprintln!(
                "> {} {:#x} {:#x} len={}",
                frame::name(frame.command),
                frame.arg0,
                frame.arg1,
                frame.data.len()
            );
        }
        self.stream.write_all(&frame.encode())
    }

    fn recv(&mut self) -> io::Result<Frame> {
        let frame = Frame::read(&mut self.stream).map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted => other("connection closed by TV"),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                io::Error::new(io::ErrorKind::TimedOut, "timed out waiting for TV")
            }
            _ => e,
        })?;
        if self.verbose {
            eprintln!(
                "< {} {:#x} {:#x} len={}",
                frame::name(frame.command),
                frame.arg0,
                frame.arg1,
                frame.data.len()
            );
        }
        Ok(frame)
    }

    /// Opens a service such as `shell:<command>` or `sync:`. One channel at a
    /// time: the returned borrow keeps the device busy until it is dropped.
    pub fn open(&mut self, service: &str) -> io::Result<Channel<'_>> {
        self.open_or_closed(service)?
            .ok_or_else(|| other(format!("TV refused service {service:?}")))
    }

    /// Like [`Device::open`], but a `CLSE` before `OKAY` returns `None` instead
    /// of an error. sdbd uses that shape for shell commands whose output is
    /// redirected elsewhere (the command still ran); treating it as a refusal
    /// loses the completed command. Other services should use [`Device::open`].
    pub fn open_or_closed(&mut self, service: &str) -> io::Result<Option<Channel<'_>>> {
        let local = self.next_id;
        self.next_id += 1;
        let mut payload = service.as_bytes().to_vec();
        payload.push(0);
        self.send(Frame::new(OPEN, local, 0, payload))?;
        loop {
            let frame = self.recv()?;
            if frame.arg1 != local {
                continue; // late frames of an earlier channel
            }
            match frame.command {
                OKAY if frame.arg0 != 0 => {
                    return Ok(Some(Channel {
                        device: self,
                        local,
                        remote: frame.arg0,
                        closed: false,
                        buf: Vec::new(),
                        pos: 0,
                    }))
                }
                CLSE => return Ok(None),
                _ => {
                    return Err(other(format!(
                        "unexpected {} while opening {service:?}",
                        frame::name(frame.command)
                    )))
                }
            }
        }
    }
}

pub struct Channel<'a> {
    device: &'a mut Device,
    local: u32,
    remote: u32,
    closed: bool,
    buf: Vec<u8>,
    pos: usize,
}

impl Channel<'_> {
    /// Next frame addressed to this channel. Incoming data is acknowledged
    /// immediately and buffered; returns `OKAY` frames to the caller.
    fn pump(&mut self) -> io::Result<Option<Frame>> {
        loop {
            let frame = self.device.recv()?;
            if frame.arg1 != self.local {
                continue;
            }
            match frame.command {
                WRTE => {
                    self.device
                        .send(Frame::new(OKAY, self.local, self.remote, Vec::new()))?;
                    self.buf.drain(..self.pos);
                    self.pos = 0;
                    self.buf.extend_from_slice(&frame.data);
                    return Ok(None);
                }
                CLSE => {
                    self.closed = true;
                    return Ok(None);
                }
                OKAY => return Ok(Some(frame)),
                _ => {
                    return Err(other(format!(
                        "unexpected {} on open channel",
                        frame::name(frame.command)
                    )))
                }
            }
        }
    }

    /// Closes our side; idempotent and doesn't wait for the TV's `CLSE`.
    pub fn close(&mut self) -> io::Result<()> {
        if std::mem::replace(&mut self.closed, true) {
            return Ok(());
        }
        self.device
            .send(Frame::new(CLSE, self.local, self.remote, Vec::new()))
    }
}

impl Read for Channel<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.buf.len() && !self.closed {
            self.pump()?;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for Channel<'_> {
    /// Sends up to one negotiated payload and waits for the TV's `OKAY`.
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.closed {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "channel closed by TV",
            ));
        }
        let chunk = &data[..data.len().min(self.device.max_data)];
        self.device
            .send(Frame::new(WRTE, self.local, self.remote, chunk))?;
        loop {
            if self.pump()?.is_some() {
                return Ok(chunk.len());
            }
            if self.closed {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "channel closed by TV",
                ));
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Channel<'_> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
