//! Host-testable pieces of the native approval UI.
//!
//! The binary owns the EFL main loop and cannot link on a non-TV host; these
//! modules contain the protocol client and display formatting that can be
//! tested with `cargo test`.

pub mod client;
pub mod log;
pub mod model;
pub mod view;
