//! Root acquisition, one-shot setup orchestration and enrollment helpers.
//!
//! This crate is deliberately isolated in the nested `rooting/` workspace.
//! The ordinary host CLI reaches it only through the external `tv-root` and
//! `tv-setup` executables, so the main workspace builds with this directory
//! physically absent.

pub mod bundle;
pub mod compatibility;
pub mod error;
pub mod fdet;
pub mod inject;
pub mod setup;
pub mod transport;

pub use error::{Error, Result};

#[cfg(test)]
mod payload_tests {
    #[test]
    fn setup_driver_installs_packages_before_persistence() {
        let driver = include_str!("../../../payloads/setup/setup-driver.sh");
        let packages = driver.find("tv-pkg\" install").unwrap();
        let persistence = driver.find("persistence is deliberately last").unwrap();
        assert!(packages < persistence);
        assert!(driver.contains("setup-status=ok"));
        assert!(driver.contains("action=install") || driver.contains("ACTION=install"));
        assert!(driver.contains("chsmack"));
        assert!(driver.contains("authorized_keys"));
    }

    #[test]
    fn reviewed_order_puts_persistence_outside_tvpkg() {
        let order = include_str!("../../../packages/package-order.txt");
        let ids: Vec<&str> = order
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
            .map(|line| line.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(ids.first(), Some(&"tv-dropbear"));
        assert_eq!(ids.last(), Some(&"tv-patchd"));
        assert!(!ids.iter().any(|id| id.contains("persistence")));
        assert_eq!(ids.len(), 8);
    }
}
