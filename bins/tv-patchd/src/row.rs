//! The flags Samsung's YouTube button passes to Cobalt.
//!
//! The Home tile, the remote's YouTube key, voice and casting all start the
//! wrapper `9Ur5IzDKqV.TizenYouTube`, which hands Cobalt the flags in one row
//! of webappservice's database (checked on T-PTMDEUC-1720.7; Vetro found the
//! same on a Q80A). Samsung's own value is the `native.userdata` metadata in
//! the package manifest, so restoring needs nothing saved.

pub const DATABASE: &str = "/opt/dbspace/.webappservice.db";
pub const WRAPPER: &str = "9Ur5IzDKqV.TizenYouTube";
pub const MANIFEST: &str = "/opt/share/packages/9Ur5IzDKqV.xml";
pub const SELECT: &str = "SELECT reserved_string_6 FROM widgetinfo WHERE tizen_id = ?1";
/// Compare-and-swap: changes nothing if the row moved on since it was read.
pub const UPDATE: &str = "UPDATE widgetinfo SET reserved_string_6 = ?1 \
     WHERE tizen_id = ?2 AND reserved_string_6 = ?3";

pub const PROXY: &str = "127.0.0.2:8102";
/// Cobalt's content folder plus our CA. Short on purpose: see [`MAX_VALUE`].
pub const CONTENT: &str = "/opt/usr/home/owner/share/tvs";
/// The wrapper passes at most this many characters on and cuts the rest
/// silently (Vetro, Q80A).
pub const MAX_VALUE: usize = 127;
/// Also Cobalt's default, so it can make room for our flags.
const BASE_URL: &str = "--base_url=https://www.youtube.com/tv";

/// `native.userdata` from the wrapper's package manifest.
pub fn manifest_userdata(xml: &str) -> Option<String> {
    let key = xml.find("\"http://samsung.com/tv/metadata/native.userdata\"")?;
    let element_start = xml[..key].rfind('<')?;
    let element = &xml[element_start..element_start + xml[element_start..].find('>')?];
    let value = &element[element.find("value=\"")? + 7..];
    let value = &value[..value.find('"')?];
    Some(
        value
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

pub fn our_flags() -> String {
    format!("--proxy={PROXY},direct:// --content={CONTENT}")
}

/// Samsung's flags without the base URL (Cobalt's default anyway) and with
/// ours; `None` if that does not fit.
pub fn patched(stock: &str) -> Option<String> {
    let mut words: Vec<&str> = stock
        .split_whitespace()
        .filter(|w| *w != BASE_URL)
        .collect();
    let ours = our_flags();
    words.push(&ours);
    let value = words.join(" ");
    (value.len() <= MAX_VALUE).then_some(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Samsung's value.
    Stock,
    /// Our flags.
    Ours,
    /// Samsung's flags with someone else's proxy (Vetro's leftover).
    OtherProxy,
    /// Anything else: left alone.
    Unknown,
}

impl Row {
    pub fn name(self) -> &'static str {
        match self {
            Row::Stock => "stock",
            Row::Ours => "patched",
            Row::OtherProxy => "other-proxy",
            Row::Unknown => "unknown",
        }
    }
}

pub fn classify(value: &str, stock: &str) -> Row {
    if value == stock {
        return Row::Stock;
    }
    if patched(stock).as_deref() == Some(value) {
        return Row::Ours;
    }
    let core = |text: &str| -> Vec<String> {
        text.split_whitespace()
            .filter(|w| {
                *w != BASE_URL && !w.starts_with("--proxy=") && !w.starts_with("--content=")
            })
            .map(str::to_owned)
            .collect()
    };
    if core(value) == core(stock) {
        Row::OtherProxy
    } else {
        Row::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOCK: &str =
        "--base_url=https://www.youtube.com/tv --dial_name=YouTube --use_eden --start_frag=#start";

    #[test]
    fn patches_within_the_limit_and_classifies() {
        let ours = patched(STOCK).unwrap();
        assert_eq!(
            ours,
            "--dial_name=YouTube --use_eden --start_frag=#start \
             --proxy=127.0.0.2:8102,direct:// --content=/opt/usr/home/owner/share/tvs"
        );
        assert!(ours.len() <= MAX_VALUE);
        assert_eq!(classify(STOCK, STOCK), Row::Stock);
        assert_eq!(classify(&ours, STOCK), Row::Ours);
        let vetro = "--dial_name=YouTube --use_eden --start_frag=#start \
                     --proxy=127.0.0.2:8101,direct:// --content=/opt/usr/home/owner/share/vyt";
        assert_eq!(classify(vetro, STOCK), Row::OtherProxy);
        assert_eq!(classify("--dial_name=Other", STOCK), Row::Unknown);
        assert_eq!(patched(&"x".repeat(100)), None);
    }

    #[test]
    fn reads_the_manifest_value() {
        let xml = r#"<app><metadata key="http://samsung.com/tv/metadata/MountPath" value="/usr/apps/x"/>
            <metadata key="http://samsung.com/tv/metadata/native.userdata" value="--base_url=https://www.youtube.com/tv --dial_name=YouTube --use_eden --start_frag=#start"/></app>"#;
        assert_eq!(manifest_userdata(xml).as_deref(), Some(STOCK));
        assert_eq!(manifest_userdata("<app/>"), None);
    }
}
