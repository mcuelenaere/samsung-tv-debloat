//! Target fingerprint parsing and the QN90B/PontusM compatibility gates.
//!
//! These are host-side checks only. The one-shot FDET payload is fixed-address
//! and build-specific, so an unknown build fails closed instead of guessing.

use crate::error::{Error, Result};

pub const CAPABILITY_PREFIX: &str = "SAMSUNG_TV_ROOT_CAPABILITY_";
pub const KERNEL_MARKER: &str = "SAMSUNG_TV_ROOT_KERNEL=";
pub const ARCHITECTURE_MARKER: &str = "SAMSUNG_TV_ROOT_ARCHITECTURE=";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Tested,
    Compatible,
    Incompatible,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Tested => "tested",
            Status::Compatible => "compatible-untested",
            Status::Incompatible => "incompatible",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub output: String,
    pub sdk_uid: Option<u32>,
    pub build_id: Option<String>,
    pub kernel_release: Option<String>,
    pub architecture: Option<String>,
    pub tizen_release: Option<String>,
    pub capabilities: Vec<String>,
}

impl Fingerprint {
    pub fn parse(output: &str) -> Self {
        let mut fingerprint = Fingerprint {
            output: output.to_owned(),
            sdk_uid: None,
            build_id: None,
            kernel_release: marker_value(output, KERNEL_MARKER),
            architecture: marker_value(output, ARCHITECTURE_MARKER),
            tizen_release: None,
            capabilities: Vec::new(),
        };
        for line in output.lines() {
            let line = line.trim_end();
            if let Some(value) = line.strip_prefix("uid=") {
                if let Some(value) = value.split('(').next() {
                    fingerprint.sdk_uid = value.parse().ok();
                }
            }
            if let Some(value) = line.strip_prefix("BUILD_ID=") {
                let value = value.trim();
                if !value.is_empty() && !value.contains(char::is_whitespace) {
                    fingerprint.build_id = Some(value.to_owned());
                }
            }
            if fingerprint.kernel_release.is_none() {
                if let Some(rest) = line.strip_prefix("Linux ") {
                    if let Some(version) = rest.split_whitespace().nth(1) {
                        if version.chars().filter(|c| *c == '.').count() >= 2 {
                            fingerprint.kernel_release = Some(version.to_owned());
                        }
                    }
                }
            }
            if fingerprint.architecture.is_none()
                && (line.split_whitespace().any(|word| word == "armv7l")
                    || line.split_whitespace().any(|word| word == "aarch64"))
            {
                fingerprint.architecture = line
                    .split_whitespace()
                    .find(|word| *word == "armv7l" || *word == "aarch64")
                    .map(str::to_owned);
            }
            if fingerprint.tizen_release.is_none() && line.starts_with("Tizen") {
                fingerprint.tizen_release = Some(line.to_owned());
            }
            if let Some(rest) = line.strip_prefix(CAPABILITY_PREFIX) {
                if let Some(name) = rest.strip_suffix("=ready") {
                    fingerprint
                        .capabilities
                        .push(name.to_ascii_lowercase().replace('_', "-"));
                }
            }
        }
        fingerprint.capabilities.sort();
        fingerprint.capabilities.dedup();
        fingerprint
    }
}

fn marker_value(output: &str, marker: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.strip_prefix(marker)
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.contains(char::is_whitespace))
            .map(str::to_owned)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub profile: String,
    pub status: Status,
    pub fingerprint: Fingerprint,
    pub tested_build: String,
    pub tested_kernel: String,
    pub differences: Vec<String>,
    pub failures: Vec<String>,
    pub additional_tested_builds: Vec<String>,
}

impl Assessment {
    pub fn require_compatible(&self) -> Result<&Self> {
        if self.status == Status::Incompatible {
            return Err(Error::compatibility(format!(
                "{} target is incompatible: {}",
                self.profile.to_uppercase(),
                self.failures.join("; ")
            )));
        }
        Ok(self)
    }

    pub fn require_tested(&self, operation: &str) -> Result<&Self> {
        self.require_compatible()?;
        if self.status != Status::Tested {
            return Err(Error::compatibility(format!(
                "{operation} uses build-specific addresses and requires tested builds {}; observed {}",
                self.tested_builds().join(", "),
                self.fingerprint.build_id.as_deref().unwrap_or("unknown")
            )));
        }
        Ok(self)
    }

    pub fn tested_builds(&self) -> Vec<String> {
        let mut builds = vec![self.tested_build.clone()];
        builds.extend(self.additional_tested_builds.iter().cloned());
        builds
    }

    pub fn render(&self) -> String {
        let mut lines = vec![
            format!("Compatibility: {}", self.status.as_str()),
            format!("Profile: {}", self.profile),
            format!(
                "Build: {}",
                self.fingerprint.build_id.as_deref().unwrap_or("unknown")
            ),
            format!("Tested builds: {}", self.tested_builds().join(", ")),
            format!(
                "Kernel: {}",
                self.fingerprint
                    .kernel_release
                    .as_deref()
                    .unwrap_or("unknown")
            ),
            format!(
                "Architecture: {}",
                self.fingerprint
                    .architecture
                    .as_deref()
                    .unwrap_or("unknown")
            ),
            format!(
                "Tizen: {}",
                self.fingerprint
                    .tizen_release
                    .as_deref()
                    .unwrap_or("unknown")
            ),
            format!(
                "Capabilities: {}",
                if self.fingerprint.capabilities.is_empty() {
                    "none".to_owned()
                } else {
                    self.fingerprint.capabilities.join(", ")
                }
            ),
        ];
        lines.extend(
            self.differences
                .iter()
                .map(|difference| format!("Difference: {difference}")),
        );
        lines.extend(
            self.failures
                .iter()
                .map(|failure| format!("Failure: {failure}")),
        );
        lines.join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: &'static str,
    pub tested_build: &'static str,
    pub build_family: &'static str,
    pub tested_kernel: &'static str,
    pub kernel_family: &'static str,
    pub architecture: &'static str,
    pub tizen_prefix: &'static str,
    pub additional_tizen_prefixes: &'static [&'static str],
    pub required_capabilities: &'static [&'static str],
    pub capability_probes: &'static [(&'static str, &'static str)],
    pub compatible_builds: &'static [&'static str],
    pub additional_tested_builds: &'static [&'static str],
}

impl Profile {
    pub fn probe_command(&self) -> String {
        let mut commands = vec![
            "id".to_owned(),
            format!("printf '{KERNEL_MARKER}'; uname -r"),
            format!("printf '{ARCHITECTURE_MARKER}'; uname -m"),
            "cat /etc/tizen-release".to_owned(),
            "cat /etc/tizen-build.conf".to_owned(),
            capability_probe("dotnet", "test -x /usr/bin/dotnet"),
            "if test -x /usr/bin/dotnet;then /usr/bin/dotnet --info;fi".to_owned(),
            "cat /proc/cmdline".to_owned(),
        ];
        commands.extend(
            self.capability_probes
                .iter()
                .map(|(name, condition)| capability_probe(name, condition)),
        );
        commands.join(";")
    }

    pub fn assess(&self, output: &str) -> Assessment {
        let fingerprint = Fingerprint::parse(output);
        let mut failures = Vec::new();
        if fingerprint.sdk_uid != Some(901) {
            failures.push(format!(
                "SDK foothold UID is {}, expected 901",
                fingerprint
                    .sdk_uid
                    .map(|uid| uid.to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            ));
        }
        match &fingerprint.build_id {
            None => failures.push("build ID was not observed".to_owned()),
            Some(build)
                if !build.starts_with(self.build_family)
                    && !self.compatible_builds.contains(&build.as_str())
                    && !self.tested_builds().contains(&build.as_str()) =>
            {
                failures.push(format!(
                    "build {build} is outside {} and is not a reviewed compatible build",
                    self.build_family
                ));
            }
            _ => {}
        }
        match &fingerprint.kernel_release {
            None => failures.push("kernel release was not observed".to_owned()),
            Some(kernel) if !kernel.starts_with(self.kernel_family) => {
                failures.push(format!("kernel {kernel} is outside {}", self.kernel_family))
            }
            _ => {}
        }
        if fingerprint.architecture.as_deref() != Some(self.architecture) {
            failures.push(format!(
                "architecture is {}, expected {}",
                fingerprint.architecture.as_deref().unwrap_or("unknown"),
                self.architecture
            ));
        }
        let mut tizen_prefixes = vec![self.tizen_prefix];
        tizen_prefixes.extend_from_slice(self.additional_tizen_prefixes);
        match &fingerprint.tizen_release {
            None => failures.push("Tizen release is not known".to_owned()),
            Some(release)
                if !tizen_prefixes
                    .iter()
                    .any(|prefix| release.starts_with(prefix)) =>
            {
                failures.push(format!(
                    "Tizen release is {release}, expected {}",
                    tizen_prefixes.join(" or ")
                ));
            }
            _ => {}
        }
        let missing: Vec<&str> = self
            .required_capabilities
            .iter()
            .copied()
            .filter(|capability| !fingerprint.capabilities.iter().any(|c| c == capability))
            .collect();
        if !missing.is_empty() {
            failures.push(format!(
                "required runtime probes did not confirm: {}",
                missing.join(", ")
            ));
        }

        let mut differences = Vec::new();
        if !self
            .tested_builds()
            .iter()
            .any(|build| fingerprint.build_id.as_deref() == Some(*build))
        {
            differences.push(format!(
                "build {} differs from {}",
                fingerprint.build_id.as_deref().unwrap_or("unknown"),
                self.tested_build
            ));
        }
        if fingerprint.kernel_release.as_deref() != Some(self.tested_kernel) {
            differences.push(format!(
                "kernel {} differs from {}",
                fingerprint.kernel_release.as_deref().unwrap_or("unknown"),
                self.tested_kernel
            ));
        }
        let status = if !failures.is_empty() {
            Status::Incompatible
        } else if !differences.is_empty() {
            Status::Compatible
        } else {
            Status::Tested
        };
        Assessment {
            profile: self.name.to_owned(),
            status,
            fingerprint,
            tested_build: self.tested_build.to_owned(),
            tested_kernel: self.tested_kernel.to_owned(),
            differences,
            failures,
            additional_tested_builds: self
                .additional_tested_builds
                .iter()
                .map(|build| (*build).to_owned())
                .collect(),
        }
    }

    pub fn tested_builds(&self) -> Vec<&str> {
        let mut builds = vec![self.tested_build];
        builds.extend_from_slice(self.additional_tested_builds);
        builds
    }
}

fn capability_probe(name: &str, condition: &str) -> String {
    let marker = name.to_ascii_uppercase().replace('-', "_");
    format!(
        "if {condition};then printf '{CAPABILITY_PREFIX}{marker}=ready\\n';else printf '{CAPABILITY_PREFIX}{marker}=missing\\n';fi"
    )
}

/// The QN90B/PontusM profile ported from `samsung-tv-root`.
pub const QN90B: Profile = Profile {
    name: "qn90b",
    tested_build: "T-PTMAKUC-REL-202310071804",
    build_family: "T-PTMAKUC-REL-",
    tested_kernel: "5.4.77",
    kernel_family: "5.4.",
    architecture: "armv7l",
    tizen_prefix: "Tizen6.5/TV 6.5.",
    additional_tizen_prefixes: &["Tizen6/TV 6.5."],
    required_capabilities: &["dotnet", "fdet-device", "pontusm-layout"],
    capability_probes: &[
        (
            "fdet-device",
            "test -c /dev/sdp_pqe_fdet && test -r /dev/sdp_pqe_fdet && test -w /dev/sdp_pqe_fdet",
        ),
        (
            "pontusm-layout",
            "grep -q pontusm /proc/cmdline && grep -q sdp_sparsemem /proc/cmdline && grep -q 'model=4kbtin' /proc/cmdline",
        ),
    ],
    compatible_builds: &[],
    additional_tested_builds: &["T-PTMDEUC-REL-202602250226"],
};

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const TESTED: &str = "\
uid=901(sdk) gid=901(sdk) groups=901(sdk)
SAMSUNG_TV_ROOT_KERNEL=5.4.77
SAMSUNG_TV_ROOT_ARCHITECTURE=armv7l
Tizen6.5/TV 6.5.0 (arm)
BUILD_ID=T-PTMAKUC-REL-202310071804
Linux tv 5.4.77 #1 SMP armv7l GNU/Linux
SAMSUNG_TV_ROOT_CAPABILITY_DOTNET=ready
SAMSUNG_TV_ROOT_CAPABILITY_FDET_DEVICE=ready
SAMSUNG_TV_ROOT_CAPABILITY_PONTUSM_LAYOUT=ready
";

    #[test]
    fn parses_the_tested_fingerprint() {
        let fingerprint = Fingerprint::parse(TESTED);
        assert_eq!(fingerprint.sdk_uid, Some(901));
        assert_eq!(
            fingerprint.build_id.as_deref(),
            Some("T-PTMAKUC-REL-202310071804")
        );
        assert_eq!(fingerprint.kernel_release.as_deref(), Some("5.4.77"));
        assert_eq!(fingerprint.architecture.as_deref(), Some("armv7l"));
        assert_eq!(
            fingerprint.tizen_release.as_deref(),
            Some("Tizen6.5/TV 6.5.0 (arm)")
        );
        assert_eq!(
            fingerprint.capabilities,
            vec!["dotnet", "fdet-device", "pontusm-layout"]
        );
    }

    #[test]
    fn tested_build_is_accepted_and_reviewed_sibling_is_compatible() {
        let assessment = QN90B.assess(TESTED);
        assert_eq!(assessment.status, Status::Tested);
        assert!(assessment.require_tested("execute").is_ok());

        let reviewed_sibling = TESTED
            .replace("T-PTMAKUC-REL-202310071804", "T-PTMDEUC-REL-202602250226")
            .replace("Tizen6.5/TV", "Tizen6/TV");
        let assessment = QN90B.assess(&reviewed_sibling);
        assert_eq!(assessment.status, Status::Tested);
        assert!(assessment.require_tested("execute").is_ok());

        let untested_family_build =
            TESTED.replace("T-PTMAKUC-REL-202310071804", "T-PTMAKUC-REL-209901010000");
        let assessment = QN90B.assess(&untested_family_build);
        assert_eq!(assessment.status, Status::Compatible);
        assert!(assessment.require_compatible().is_ok());
        assert!(assessment.require_tested("execute").is_err());
    }

    #[test]
    fn wrong_uid_architecture_or_capability_fails_closed() {
        for (needle, replacement) in [
            ("uid=901", "uid=5001"),
            ("ARCHITECTURE=armv7l", "ARCHITECTURE=aarch64"),
            (
                "CAPABILITY_FDET_DEVICE=ready",
                "CAPABILITY_FDET_DEVICE=missing",
            ),
            ("BUILD_ID=T-PTMAKUC-REL-202310071804", "BUILD_ID=T-UNKNOWN"),
        ] {
            let output = TESTED.replace(needle, replacement);
            let assessment = QN90B.assess(&output);
            assert_eq!(
                assessment.status,
                Status::Incompatible,
                "{needle} should fail: {}",
                assessment.render()
            );
            assert!(assessment.require_compatible().is_err());
        }
    }

    #[test]
    fn fallback_markers_are_parsed() {
        let output = "uid=901(x) gid=901(x)\nLinux tv 5.4.77 armv7l GNU/Linux\n";
        let fingerprint = Fingerprint::parse(output);
        assert_eq!(fingerprint.kernel_release.as_deref(), Some("5.4.77"));
        assert_eq!(fingerprint.architecture.as_deref(), Some("armv7l"));
    }
}
