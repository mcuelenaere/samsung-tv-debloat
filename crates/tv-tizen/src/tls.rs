//! TLS through the TV's OpenSSL 1.1 (`libssl`): a server with our own
//! certificate and a verifying client, both on blocking sockets.

use crate::c_string;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::raw::{c_char, c_int, c_long, c_ulong, c_void};
use std::ptr;

const SSL_FILETYPE_PEM: c_int = 1;
const SSL_VERIFY_PEER: c_int = 1;
const SSL_CTRL_SET_TLSEXT_HOSTNAME: c_int = 55;
const SSL_CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
const TLSEXT_NAMETYPE_HOST_NAME: c_long = 0;
const TLS1_2_VERSION: c_long = 0x0303;
const SSL_ERROR_SYSCALL: c_int = 5;
const SSL_ERROR_ZERO_RETURN: c_int = 6;

#[link(name = "ssl")]
extern "C" {
    fn TLS_server_method() -> *const c_void;
    fn TLS_client_method() -> *const c_void;
    fn SSL_CTX_new(method: *const c_void) -> *mut c_void;
    fn SSL_CTX_free(context: *mut c_void);
    fn SSL_CTX_ctrl(
        context: *mut c_void,
        command: c_int,
        larg: c_long,
        parg: *mut c_void,
    ) -> c_long;
    fn SSL_CTX_use_certificate_chain_file(context: *mut c_void, file: *const c_char) -> c_int;
    fn SSL_CTX_use_PrivateKey_file(context: *mut c_void, file: *const c_char, kind: c_int)
        -> c_int;
    fn SSL_CTX_check_private_key(context: *const c_void) -> c_int;
    fn SSL_CTX_load_verify_locations(
        context: *mut c_void,
        file: *const c_char,
        directory: *const c_char,
    ) -> c_int;
    fn SSL_CTX_set_verify(context: *mut c_void, mode: c_int, callback: *const c_void);
    fn SSL_new(context: *mut c_void) -> *mut c_void;
    fn SSL_free(ssl: *mut c_void);
    fn SSL_set_fd(ssl: *mut c_void, fd: c_int) -> c_int;
    fn SSL_ctrl(ssl: *mut c_void, command: c_int, larg: c_long, parg: *mut c_void) -> c_long;
    fn SSL_set1_host(ssl: *mut c_void, host: *const c_char) -> c_int;
    fn SSL_accept(ssl: *mut c_void) -> c_int;
    fn SSL_connect(ssl: *mut c_void) -> c_int;
    fn SSL_read(ssl: *mut c_void, buffer: *mut c_void, length: c_int) -> c_int;
    fn SSL_write(ssl: *mut c_void, buffer: *const c_void, length: c_int) -> c_int;
    fn SSL_shutdown(ssl: *mut c_void) -> c_int;
    fn SSL_get_error(ssl: *const c_void, result: c_int) -> c_int;
}

#[link(name = "crypto")]
extern "C" {
    fn ERR_get_error() -> c_ulong;
    fn ERR_error_string_n(error: c_ulong, buffer: *mut c_char, length: usize);
}

/// OpenSSL's queued errors, oldest first (and clears the queue).
fn openssl_error(what: &str) -> String {
    let mut messages = vec![what.to_owned()];
    loop {
        let code = unsafe { ERR_get_error() };
        if code == 0 {
            break;
        }
        let mut buffer = [0 as c_char; 256];
        unsafe { ERR_error_string_n(code, buffer.as_mut_ptr(), buffer.len()) };
        messages.push(unsafe { crate::borrowed(buffer.as_ptr()) });
    }
    messages.join(": ")
}

/// An `SSL_CTX`. After setup it is only used to make connections, which
/// OpenSSL allows from several threads.
pub struct Context(*mut c_void);

unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Context {
    fn new(method: *const c_void) -> Result<Self, String> {
        let context = unsafe { SSL_CTX_new(method) };
        if context.is_null() {
            return Err(openssl_error("SSL_CTX_new"));
        }
        unsafe {
            SSL_CTX_ctrl(
                context,
                SSL_CTRL_SET_MIN_PROTO_VERSION,
                TLS1_2_VERSION,
                ptr::null_mut(),
            )
        };
        Ok(Self(context))
    }

    /// A server presenting `certificate` (PEM chain) with `key` (PEM).
    pub fn server(certificate: &str, key: &str) -> Result<Self, String> {
        let context = Self::new(unsafe { TLS_server_method() })?;
        let (certificate, key) = (c_string(certificate)?, c_string(key)?);
        unsafe {
            if SSL_CTX_use_certificate_chain_file(context.0, certificate.as_ptr()) != 1
                || SSL_CTX_use_PrivateKey_file(context.0, key.as_ptr(), SSL_FILETYPE_PEM) != 1
                || SSL_CTX_check_private_key(context.0) != 1
            {
                return Err(openssl_error("server certificate"));
            }
        }
        Ok(context)
    }

    /// A client that verifies servers against the PEM bundle `ca_file`.
    pub fn client(ca_file: &str) -> Result<Self, String> {
        let context = Self::new(unsafe { TLS_client_method() })?;
        let file = c_string(ca_file)?;
        unsafe {
            if SSL_CTX_load_verify_locations(context.0, file.as_ptr(), ptr::null()) != 1 {
                return Err(openssl_error("CA bundle"));
            }
            SSL_CTX_set_verify(context.0, SSL_VERIFY_PEER, ptr::null());
        }
        Ok(context)
    }

    fn stream(&self, socket: TcpStream) -> Result<Stream, String> {
        let ssl = unsafe { SSL_new(self.0) };
        if ssl.is_null() {
            return Err(openssl_error("SSL_new"));
        }
        let stream = Stream { ssl, socket };
        if unsafe { SSL_set_fd(ssl, stream.socket.as_raw_fd()) } != 1 {
            return Err(openssl_error("SSL_set_fd"));
        }
        Ok(stream)
    }

    /// The server side of a TLS handshake on `socket`.
    pub fn accept(&self, socket: TcpStream) -> Result<Stream, String> {
        let stream = self.stream(socket)?;
        if unsafe { SSL_accept(stream.ssl) } != 1 {
            return Err(openssl_error("TLS accept"));
        }
        Ok(stream)
    }

    /// The client side of a TLS handshake with `host` (SNI and name check).
    pub fn connect(&self, socket: TcpStream, host: &str) -> Result<Stream, String> {
        let stream = self.stream(socket)?;
        let name = c_string(host)?;
        unsafe {
            SSL_ctrl(
                stream.ssl,
                SSL_CTRL_SET_TLSEXT_HOSTNAME,
                TLSEXT_NAMETYPE_HOST_NAME,
                name.as_ptr() as *mut c_void,
            );
            if SSL_set1_host(stream.ssl, name.as_ptr()) != 1 {
                return Err(openssl_error("SSL_set1_host"));
            }
            if SSL_connect(stream.ssl) != 1 {
                return Err(openssl_error(&format!("TLS connect to {host}")));
            }
        }
        Ok(stream)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { SSL_CTX_free(self.0) };
    }
}

/// One TLS connection; reads and writes must come from one thread at a time.
pub struct Stream {
    ssl: *mut c_void,
    socket: TcpStream,
}

unsafe impl Send for Stream {}

impl Stream {
    pub fn socket(&self) -> &TcpStream {
        &self.socket
    }

    fn failure(&self, result: c_int) -> io::Result<usize> {
        match unsafe { SSL_get_error(self.ssl, result) } {
            // A peer that closes without close_notify is treated as EOF.
            SSL_ERROR_ZERO_RETURN => Ok(0),
            SSL_ERROR_SYSCALL if result == 0 => Ok(0),
            SSL_ERROR_SYSCALL => Err(io::Error::last_os_error()),
            code => Err(io::Error::other(openssl_error(&format!(
                "TLS error {code}"
            )))),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = buffer.len().min(c_int::MAX as usize) as c_int;
        let result = unsafe { SSL_read(self.ssl, buffer.as_mut_ptr().cast(), length) };
        if result > 0 {
            Ok(result as usize)
        } else {
            self.failure(result)
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let length = buffer.len().min(c_int::MAX as usize) as c_int;
        let result = unsafe { SSL_write(self.ssl, buffer.as_ptr().cast(), length) };
        if result > 0 {
            return Ok(result as usize);
        }
        match self.failure(result)? {
            0 => Err(io::ErrorKind::WriteZero.into()),
            written => Ok(written),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            SSL_shutdown(self.ssl);
            SSL_free(self.ssl);
        }
    }
}
