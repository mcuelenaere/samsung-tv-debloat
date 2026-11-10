//! `tv-patchd`: TizenTube inside Samsung's own YouTube app, without
//! installing anything (the approach of Vetro's YouTube plugin).
//!
//! - A local proxy on `127.0.0.2:8102` tunnels everything except
//!   www.youtube.com, which it opens with a certificate of its own to add
//!   TizenTube to the `/tv` page.
//! - Only Cobalt trusts that certificate: its CA is added to a copy of
//!   Cobalt's content folder (`--content`), never to the TV's trust store.
//! - Samsung's YouTube button gets the `--proxy`/`--content` flags through
//!   webappservice's database. The proxy list ends in `direct://`, so with
//!   tv-patchd gone YouTube still opens, unpatched.
//!
//! Configured in `/opt/tv-suite/etc/patchd.json`; root and the
//! `org.tvauth.ui` manager control it over `/run/tv-suite/patchd.sock`.

// The daemon uses Linux-only interfaces; elsewhere only the library builds.
#[cfg(target_os = "linux")]
mod daemon;

fn main() {
    #[cfg(target_os = "linux")]
    daemon::main();
    #[cfg(not(target_os = "linux"))]
    eprintln!("tv-patchd runs on the TV (Linux) only");
}
