//! Built-in host groups, the essential hosts that are never blocked, the
//! configuration, and the generated hosts file.
//!
//! The host groups follow Vetro's `blocklist.js` (evidence-labelled names
//! found in the firmware or in public reports) plus Samsung's OTN firmware
//! update hosts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub struct HostGroup {
    pub id: &'static str,
    pub label: &'static str,
    /// `firmware`, `reported` or `unverified`.
    pub evidence: &'static str,
    pub note: &'static str,
    /// Off by default.
    pub optional: bool,
    pub hosts: &'static [&'static str],
}

pub const HOST_GROUPS: &[HostGroup] = &[
    HostGroup {
        id: "ads",
        label: "Advertising",
        evidence: "firmware",
        note: "Ad configuration, ad logs and interactive ads (ad agent, ad framework, Samsung TV Plus).",
        optional: false,
        hosts: &[
            "rtb-config.samsungads.com",
            "tvp-ads-log.samsungads.com",
            "tvp-interactive-ad.samsungads.com",
            "rtb-config-canary.samsungads.com",
        ],
    },
    HostGroup {
        id: "analytics",
        label: "Analytics and KPI",
        evidence: "firmware",
        note: "Usage analytics and the KPI collector.",
        optional: false,
        hosts: &[
            "dc.di.atlas.samsung.com",
            "regi.di.atlas.samsung.com",
            "regi.bigdata.samsung.com",
            "lcprd1.samsungcloudsolution.net",
        ],
    },
    HostGroup {
        id: "logs",
        label: "Diagnostic logs and monitoring",
        evidence: "firmware",
        note: "Emergency log transmitter and health monitors; effect on support features unknown.",
        optional: false,
        hosts: &[
            "fc.samsungcloud.tv",
            "www.samsungrm.net",
            "devicelog.samsungcloudsolution.net",
        ],
    },
    HostGroup {
        id: "qbe",
        label: "Samsung QBE services",
        evidence: "firmware",
        note: "Found in the firmware; which feature uses each is unknown.",
        optional: false,
        hosts: &["osb.samsungqbe.com", "rm.samsungqbe.com", "scs.samsungqbe.com"],
    },
    HostGroup {
        id: "reported",
        label: "Viewing data (ACR) and ads, reported",
        evidence: "reported",
        note: "Names from public reports; none appears in the firmware.",
        optional: false,
        hosts: &[
            "samsungads.com",
            "config.samsungads.com",
            "ads.samsung.com",
            "samsungacr.com",
            "log-config.samsungacr.com",
            "log-ingestion.samsungacr.com",
            "acr0.samsungcloudsolution.com",
        ],
    },
    HostGroup {
        id: "probable",
        label: "Probable ACR starting point, unverified",
        evidence: "unverified",
        note: "Likely where the viewing-data service learns its servers; a guess.",
        optional: false,
        hosts: &["gpm.samsungqbe.com"],
    },
    HostGroup {
        id: "otn",
        label: "Firmware updates (OTN)",
        evidence: "firmware",
        note: "Samsung's firmware update servers. Blocking them stops automatic and manual updates.",
        optional: false,
        hosts: &[
            "otn.samsungcloudcdn.com",
            "otnprd8.samsungcloudsolution.net",
            "otnprd9.samsungcloudsolution.net",
            "otnprd10.samsungcloudsolution.net",
            "otnprd11.samsungcloudsolution.net",
            "www.samsungotn.net",
            "test.samsungotn.net",
        ],
    },
    HostGroup {
        id: "home-images",
        label: "Home screen ad pictures, reported once",
        evidence: "reported",
        note: "Picture server of the ads in the Home row; may hide other Home pictures too.",
        optional: true,
        hosts: &["cdn-stv.adgrx.com"],
    },
];

/// Hosts the TV needs: never written to the hosts file, whatever the config.
pub const ESSENTIAL_HOSTS: &[(&str, &str)] = &[
    ("cdn.samsungcloudsolution.com", "connectivity check"),
    ("cdn.samsungqbe.cn", "connectivity check"),
    ("time.samsungcloudsolution.com", "clock"),
    ("time.samsungcloudsolution.net", "clock"),
    ("time.samsungqbe.cn", "clock"),
    ("svdca.samsungqbe.com", "developer certificate"),
];

fn default_groups() -> Vec<String> {
    HOST_GROUPS
        .iter()
        .filter(|group| !group.optional)
        .map(|group| group.id.to_owned())
        .collect()
}

fn yes() -> bool {
    true
}

/// `/opt/tv-suite/etc/blockd.json`; missing fields take the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Sinkhole the selected hosts through `/etc/hosts`.
    #[serde(default = "yes")]
    pub hosts: bool,
    /// Enabled host group ids.
    #[serde(default = "default_groups")]
    pub groups: Vec<String>,
    /// Extra hosts to block.
    #[serde(default)]
    pub custom: Vec<String>,
    /// HTTPS URLs of hosts-format or domain-per-line block lists, refreshed
    /// daily.
    #[serde(default)]
    pub lists: Vec<String>,
    /// Keep `db/menu/support/softwareupdate/autoupdate` at 0.
    #[serde(default = "yes")]
    pub otn_lock: bool,
    /// Samsung's telemetry units stopped and runtime-masked (see
    /// `SERVICES` in the daemon).
    #[serde(default = "yes")]
    pub telemetry: bool,
    /// The AirPlay receiver stopped and runtime-masked.
    #[serde(default)]
    pub airplay: bool,
    /// Multi View's preloaded app closed and its unit runtime-masked.
    #[serde(default)]
    pub multiview_preload: bool,
    /// Permanent: apps disabled in pkgmgr's database (`pkgcmd -D --global`),
    /// which outlives a reboot and tv-blockd. See `DISABLES` in the daemon.
    #[serde(default)]
    pub disable_hbbtv: bool,
    #[serde(default)]
    pub disable_acr: bool,
    #[serde(default)]
    pub disable_ads: bool,
}

impl Config {
    /// A service or disable option by its key.
    pub fn service(&self, key: &str) -> bool {
        match key {
            "telemetry" => self.telemetry,
            "airplay" => self.airplay,
            "multiview_preload" => self.multiview_preload,
            "disable_hbbtv" => self.disable_hbbtv,
            "disable_acr" => self.disable_acr,
            "disable_ads" => self.disable_ads,
            _ => false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        serde_json::from_str("{}").expect("defaults parse")
    }
}

pub const MAX_CUSTOM_HOSTS: usize = 500;
pub const MAX_LISTS: usize = 8;
/// Cap on the domains written to the hosts file (glibc reads it linearly).
pub const MAX_BLOCKED_HOSTS: usize = 200_000;

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        for group in &self.groups {
            if !HOST_GROUPS.iter().any(|known| known.id == group) {
                return Err(format!("unknown host group {group}"));
            }
        }
        if self.custom.len() > MAX_CUSTOM_HOSTS {
            return Err(format!("at most {MAX_CUSTOM_HOSTS} custom hosts"));
        }
        for host in &self.custom {
            if !is_hostname(host) {
                return Err(format!("not a host name: {host}"));
            }
        }
        if self.lists.len() > MAX_LISTS {
            return Err(format!("at most {MAX_LISTS} lists"));
        }
        for url in &self.lists {
            if !url.starts_with("https://") || url.len() > 2048 || url.contains(char::is_whitespace)
            {
                return Err(format!("lists must be https URLs: {url}"));
            }
        }
        Ok(())
    }

    /// Every host to sinkhole: enabled groups, custom hosts and the given
    /// remote-list hosts, minus the essential ones; sorted, capped.
    pub fn blocked_hosts<'a>(&self, remote: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut hosts = BTreeSet::new();
        for group in HOST_GROUPS
            .iter()
            .filter(|group| self.groups.iter().any(|id| id == group.id))
        {
            hosts.extend(group.hosts.iter().map(|host| host.to_string()));
        }
        hosts.extend(self.custom.iter().map(|host| host.to_ascii_lowercase()));
        hosts.extend(remote.into_iter().map(str::to_owned));
        hosts.retain(|host| {
            !ESSENTIAL_HOSTS
                .iter()
                .any(|(essential, _)| essential == host)
        });
        hosts.into_iter().take(MAX_BLOCKED_HOSTS).collect()
    }
}

/// A plain DNS name: labels of `[a-z0-9-]`, at least one dot, not an IP.
pub fn is_hostname(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    host.len() <= 253
        && host.contains('.')
        && !host
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// Hosts from a hosts-format (`0.0.0.0 name`, `127.0.0.1 name`) or
/// domain-per-line list. Comments, IPs and invalid names are skipped.
pub fn parse_list(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or("").trim();
            let mut fields = line.split_whitespace();
            let first = fields.next()?;
            let name = match fields.next() {
                Some(second) if matches!(first, "0.0.0.0" | "127.0.0.1" | "::" | "::1") => second,
                Some(_) => return None,
                None => first,
            };
            let name = name.to_ascii_lowercase();
            (is_hostname(&name) && name != "localhost").then_some(name)
        })
        .collect()
}

/// First line of every hosts file `tv-blockd` mounts.
pub const HOSTS_MARKER: &str = "# managed by tv-blockd";

/// The mounted `/etc/hosts`: the marker, the TV's original entries, then a
/// v4 and v6 sinkhole line per blocked host.
pub fn hosts_file(original: &str, blocked: &[String]) -> String {
    let mut out = String::with_capacity(original.len() + blocked.len() * 48);
    out.push_str(HOSTS_MARKER);
    out.push('\n');
    for line in original.lines().filter(|line| *line != HOSTS_MARKER) {
        out.push_str(line);
        out.push('\n');
    }
    for host in blocked {
        out.push_str("0.0.0.0 ");
        out.push_str(host);
        out.push_str("\n:: ");
        out.push_str(host);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_block_every_non_optional_group() {
        let config = Config::default();
        assert!(config.hosts && config.otn_lock && config.telemetry);
        // The services that break features are opt-in.
        assert!(!config.airplay && !config.multiview_preload);
        // Nothing permanent unless asked for.
        assert!(!config.disable_hbbtv && !config.disable_acr && !config.disable_ads);
        assert!(
            config.service("telemetry")
                && !config.service("airplay")
                && !config.service("nonsense")
        );
        assert!(config.groups.contains(&"reported".to_owned()));
        assert!(!config.groups.contains(&"home-images".to_owned()));
        assert!(config.validate().is_ok());
        let blocked = config.blocked_hosts([]);
        assert!(blocked.contains(&"samsungacr.com".to_owned()));
        assert!(blocked.contains(&"otnprd9.samsungcloudsolution.net".to_owned()));
        assert!(!blocked.contains(&"cdn-stv.adgrx.com".to_owned()));
    }

    #[test]
    fn essential_hosts_are_never_blocked() {
        let config = Config {
            custom: vec![
                "time.samsungcloudsolution.com".into(),
                "Ads.Example.com".into(),
            ],
            ..Config::default()
        };
        let blocked = config.blocked_hosts(["cdn.samsungqbe.cn", "tracker.example.net"]);
        assert!(!blocked.iter().any(|host| host.contains("time.samsung")));
        assert!(!blocked.contains(&"cdn.samsungqbe.cn".to_owned()));
        assert!(blocked.contains(&"ads.example.com".to_owned()));
        assert!(blocked.contains(&"tracker.example.net".to_owned()));
    }

    #[test]
    fn validation_refuses_bad_input() {
        let bad = |config: Config| config.validate().is_err();
        assert!(bad(Config {
            groups: vec!["nope".into()],
            ..Config::default()
        }));
        assert!(bad(Config {
            custom: vec!["1.2.3.4".into()],
            ..Config::default()
        }));
        assert!(bad(Config {
            custom: vec!["no spaces.com x".into()],
            ..Config::default()
        }));
        assert!(bad(Config {
            lists: vec!["http://insecure/list".into()],
            ..Config::default()
        }));
        // Missing fields take defaults; unknown ones (an old
        // "disabled_apps") are ignored.
        let parsed: Config =
            serde_json::from_str(r#"{"telemetry": false, "disabled_apps": []}"#).unwrap();
        assert!(!parsed.telemetry && parsed.hosts);
    }

    #[test]
    fn parses_hosts_and_domain_lists() {
        let list = "# comment\n0.0.0.0 ads.example.com\n127.0.0.1  T.Example.org # x\n\
                    plain.example.net\n1.2.3.4 other.example.com\n0.0.0.0 localhost\n\
                    0.0.0.0 bad_name.com\n\n:: v6.example.com\n";
        assert_eq!(
            parse_list(list),
            [
                "ads.example.com",
                "t.example.org",
                "plain.example.net",
                "v6.example.com"
            ]
        );
    }

    #[test]
    fn hosts_file_keeps_the_original_and_marks_itself() {
        let original = "127.0.0.1 localhost\n::1 localhost\n";
        let file = hosts_file(original, &["a.example.com".into()]);
        assert!(file.starts_with(HOSTS_MARKER));
        assert!(file.contains("127.0.0.1 localhost\n::1 localhost\n"));
        assert!(file.ends_with("0.0.0.0 a.example.com\n:: a.example.com\n"));
        // Rebuilding from a file we generated does not stack markers.
        assert_eq!(hosts_file(&file, &[]).matches(HOSTS_MARKER).count(), 1);
    }
}
