//! Client-side views of `tv-authd`'s `ui.prompt`, `ui.notice` and
//! `grants.list` messages. Every field defaults, so a newer daemon adding
//! fields or an older one omitting them never breaks a UI.

use serde::Deserialize;

fn default_kind() -> String {
    "exec".to_owned()
}

fn one() -> u32 {
    1
}

fn every() -> String {
    "every".to_owned()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InstallInfo {
    #[serde(default)]
    pub pkgid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pkg_version: String,
    #[serde(default)]
    pub pkg_type: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub privileges: Vec<String>,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub is_upgrade: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UninstallInfo {
    #[serde(default)]
    pub pkgid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pkg_version: String,
    #[serde(default)]
    pub pkg_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PromptInfo {
    #[serde(default)]
    pub prompt_id: String,
    #[serde(default)]
    pub nonce: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub package: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub install: Option<InstallInfo>,
    #[serde(default)]
    pub uninstall: Option<UninstallInfo>,
    #[serde(default)]
    pub command: String,
    /// Kept for protocol completeness; the UI only displays the sanitized copy.
    #[serde(default)]
    #[allow(dead_code)]
    pub command_bytes: usize,
    #[serde(default)]
    pub command_truncated: bool,
    #[serde(default)]
    pub timeout_ms: u64,
}

impl PromptInfo {
    pub fn is_package_operation(&self) -> bool {
        self.kind == "install" || self.kind == "uninstall"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NoticeInfo {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub package: String,
    #[serde(default = "one")]
    pub count: u32,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub command_truncated: bool,
    #[serde(default = "default_kind")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrantInfo {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub package: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub granted_at: u64,
    #[serde(default = "every")]
    pub notify: String,
}
