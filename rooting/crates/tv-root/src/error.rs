//! Errors for the one-shot root and setup paths.
//!
//! Failures are intentionally typed enough for the CLI to explain whether a
//! problem is local, in SDB transport, a compatibility gate or a protocol
//! violation. No variant implies a fallback that would bypass a gate.

use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Sdb(io::Error),
    Compatibility(String),
    Protocol(String),
    Bundle(String),
    Payload(String),
    Busy(String),
}

impl Error {
    pub fn protocol(message: impl Into<String>) -> Self {
        Error::Protocol(message.into())
    }

    pub fn compatibility(message: impl Into<String>) -> Self {
        Error::Compatibility(message.into())
    }

    pub fn bundle(message: impl Into<String>) -> Self {
        Error::Bundle(message.into())
    }

    pub fn payload(message: impl Into<String>) -> Self {
        Error::Payload(message.into())
    }

    pub fn sdb(error: io::Error) -> Self {
        Error::Sdb(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(f, "{error}"),
            Error::Sdb(error) => write!(f, "SDB: {error}"),
            Error::Compatibility(message) => write!(f, "compatibility: {message}"),
            Error::Protocol(message) => write!(f, "protocol: {message}"),
            Error::Bundle(message) => write!(f, "bundle: {message}"),
            Error::Payload(message) => write!(f, "payload: {message}"),
            Error::Busy(message) => write!(f, "busy: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) | Error::Sdb(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}
