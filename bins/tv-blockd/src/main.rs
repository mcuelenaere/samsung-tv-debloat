//! `tv-blockd`: keeps Samsung's ads, tracking, telemetry and firmware
//! updates switched off, as configured in `/opt/tv-suite/etc/blockd.json`.
//!
//! Every minute (and after each change) it reconciles:
//! - **hosts**: writes `/run/tv-suite/hosts` (the TV's own entries plus
//!   `0.0.0.0`/`::` lines) and bind-mounts it over `/etc/hosts` in every
//!   mount namespace, including ones created since the last pass;
//! - **OTN lock**: `db/menu/support/softwareupdate/autoupdate` stays 0;
//! - **telemetry**: brake files, telemetry units stopped and runtime-masked,
//!   no EdgeScraper;
//!
//! Root and the `org.tvauth.ui` manager read and change the configuration
//! over `/run/tv-suite/blockd.sock` (newline-delimited JSON).

// The daemon uses Linux-only interfaces (setns, SO_PEERCRED, SO_PEERSEC);
// on other hosts only the library builds and tests.
#[cfg(target_os = "linux")]
mod daemon;

fn main() {
    #[cfg(target_os = "linux")]
    daemon::main();
    #[cfg(not(target_os = "linux"))]
    eprintln!("tv-blockd runs on the TV (Linux) only");
}
