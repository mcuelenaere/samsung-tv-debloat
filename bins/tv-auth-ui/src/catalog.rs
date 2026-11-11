//! The dock's app list: webappservice's catalog (the one Samsung's Home
//! shows, sideloaded apps included), most launched first, then in Home's row
//! order, then by title.
//! And its inputs: the sources in Home's row, named as Home names them.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const DATABASE: &str = "/opt/dbspace/.webappservice.db";
/// Samsung Home's row, mirrored by tv-patchd.
pub const ROW: &str = "/run/tv-suite/home-row";
/// Samsung Home's `.sourceCache`, mirrored by tv-patchd.
pub const SOURCES: &str = "/run/tv-suite/home-sources";

/// One row per app Home shows; fields joined with U+001F.
pub const SQL: &str = "SELECT id || char(31) || title || char(31) || tizen_id || char(31) || \
     icon_path || char(31) || use_count FROM widgetinfo WHERE is_hidden = 0 AND tizen_id != '' \
     AND tizen_id NOT IN ('org.tvauth.ui', 'org.tvauth.home')";

/// Sizes in webappservice's icon folders, best fit for a dock tile first.
const ICONS: &[&str] = &["152x152.png", "224x224.png", "icon.png", "104x104.png"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// webappservice's id, the one Home's row uses.
    pub id: String,
    pub title: String,
    /// What to launch.
    pub app_id: String,
    pub icon: Option<PathBuf>,
    /// webappservice's `use_count`: launches from anywhere (Samsung's Home,
    /// the remote's app keys, the dock).
    pub launches: u32,
}

pub fn parse(rows: &[String]) -> Vec<App> {
    rows.iter()
        .filter_map(|row| {
            let mut fields = row.split('\u{1f}');
            let (id, title, app_id, icons) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            let launches = fields
                .next()
                .and_then(|count| count.parse().ok())
                .unwrap_or(0);
            let icon = ICONS
                .iter()
                .map(|name| Path::new(icons).join(name))
                .find(|path| path.is_file());
            Some(App {
                id: id.to_owned(),
                title: title.to_owned(),
                app_id: app_id.to_owned(),
                icon,
                launches,
            })
        })
        .collect()
}

/// The webapp ids of Home's row (`d/webapp@<id>` lines), in order.
pub fn row_ids(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("d/webapp@"))
        .collect()
}

/// One input, as Samsung's Home shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    /// The cache's `SourceType` (TV 0, HDMI1 13 ... HDMI4 16).
    pub kind: i64,
    /// The cache's `MbrActivityIndex`.
    pub mbr: i64,
    /// Samsung's launcher art, else its source icon (PNG or SVG).
    pub icon: Option<String>,
    /// `IconPath`, which tv-viewer wants as `sourceIcon`.
    pub source_icon: String,
    /// A DLNA media server: its `UniqueKey` and full name, which Samsung's
    /// media browser opens.
    pub dlna: Option<(String, String)>,
}

/// `"172.17.2.219 - Sonos Play:3 Media Server - RINCON_…"` becomes
/// `"Sonos Play:3 (172.17.2.219)"`; other names stay as they are.
fn short_dlna_name(name: &str) -> String {
    let parts: Vec<&str> = name.split(" - ").collect();
    match parts.as_slice() {
        [address, model, ..] if address.parse::<std::net::Ipv4Addr>().is_ok() => {
            format!("{} ({address})", model.trim_end_matches(" Media Server"))
        }
        _ => name.to_owned(),
    }
}

/// The sources in Home's row (`s/source.layer.hdmi@<key>` lines), in order,
/// then TV, then the connected DLNA media servers by name. `cache` is Home's
/// `.sourceCache` JSON.
pub fn sources(cache: &str, row: &str) -> Vec<Source> {
    let Ok(Value::Object(cache)) = serde_json::from_str::<Value>(cache) else {
        return Vec::new();
    };
    let keys = row
        .lines()
        .filter_map(|line| line.trim().strip_prefix("s/source.layer.hdmi@"))
        .chain(["TV"]);
    let mut servers: Vec<&str> = cache
        .iter()
        .filter(|(key, entry)| key.starts_with("DLNA/") && entry["ConnectionState"] == true)
        .map(|(key, _)| key.as_str())
        .collect();
    servers.sort_by_key(|key| {
        cache[*key]["EditName"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
    });
    keys.chain(servers)
        .filter_map(|key| {
            let entry = cache.get(key)?;
            let text = |field: &str| entry[field].as_str().filter(|text| !text.is_empty());
            let name = text("EditName")
                .or(text("DetectedName"))
                .map(str::to_owned)
                .unwrap_or_else(|| key.split('/').next().unwrap_or(key).to_owned());
            let dlna = key
                .starts_with("DLNA/")
                .then(|| Some((text("UniqueKey")?.to_owned(), name.clone())))
                .flatten();
            Some(Source {
                name: if dlna.is_some() {
                    short_dlna_name(&name)
                } else {
                    name
                },
                dlna,
                kind: entry["SourceType"].as_i64()?,
                mbr: entry["MbrActivityIndex"].as_i64().unwrap_or(-2),
                icon: text("LauncherUnfocusedIconPath")
                    .or(text("IconPath"))
                    .map(str::to_owned),
                source_icon: text("IconPath").unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

/// Most launched first; equal counts (most often never launched) in Home's
/// row order, then the rest by title.
pub fn order(mut apps: Vec<App>, row: &[&str]) -> Vec<App> {
    apps.sort_by_cached_key(|app| {
        let position = row
            .iter()
            .position(|id| *id == app.id)
            .unwrap_or(usize::MAX);
        (
            std::cmp::Reverse(app.launches),
            position,
            app.title.to_lowercase(),
        )
    });
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, title: &str) -> App {
        App {
            id: id.into(),
            title: title.into(),
            app_id: format!("{id}.app"),
            icon: None,
            launches: 0,
        }
    }

    #[test]
    fn row_first_then_titles() {
        let row =
            "4538\ns/livetv@TVPLUS\nd/webapp@3\nd/webapp@1\ns/source.layer.hdmi@HDMI2\nEOFCOBA\n";
        let ids = row_ids(row);
        assert_eq!(ids, ["3", "1"]);
        let apps = vec![
            app("1", "Netflix"),
            app("2", "kodi"),
            app("3", "YouTube"),
            app("4", "Apple TV"),
        ];
        let titles: Vec<_> = order(apps, &ids).into_iter().map(|app| app.title).collect();
        assert_eq!(titles, ["YouTube", "Netflix", "Apple TV", "kodi"]);
    }

    #[test]
    fn most_launched_first() {
        let ids = ["3", "1"];
        let mut kodi = app("2", "kodi");
        kodi.launches = 9;
        let mut netflix = app("1", "Netflix");
        netflix.launches = 2;
        let apps = vec![netflix, kodi, app("3", "YouTube"), app("4", "Apple TV")];
        let titles: Vec<_> = order(apps, &ids).into_iter().map(|app| app.title).collect();
        assert_eq!(titles, ["kodi", "Netflix", "YouTube", "Apple TV"]);
    }

    #[test]
    fn sources_follow_the_row_then_tv() {
        let cache = r#"{
            "TV": {"SourceType": 0, "MbrActivityIndex": -2, "IconPath": "/i/tv.svg"},
            "HDMI1/0": {"EditName": "XBOX SeriesX|S", "SourceType": 13, "MbrActivityIndex": 0,
                        "IconPath": "/i/x.png", "LauncherUnfocusedIconPath": "/i/x_n.png"},
            "HDMI2": {"EditName": "", "DetectedName": "PlayStation 5", "SourceType": 14,
                      "MbrActivityIndex": -2, "IconPath": "/i/pad.svg"},
            "HDMI3": {"EditName": "HW-Q930B", "SourceType": 15},
            "HDMI4": {"SourceType": 16},
            "DLNA/uuid:B+eth0": {"EditName": "172.17.2.219 - Sonos Play:3 Media Server - RINCON_B",
                                 "SourceType": 56, "UniqueKey": "uuid:B+eth0", "ConnectionState": true},
            "DLNA/uuid:A+eth0": {"EditName": "Plex: NAS", "SourceType": 56,
                                 "UniqueKey": "uuid:A+eth0", "ConnectionState": true},
            "DLNA/uuid:C+eth0": {"EditName": "Gone", "SourceType": 56, "UniqueKey": "uuid:C",
                                 "ConnectionState": false}
        }"#;
        let row = "s/livetv@TVPLUS\nd/webapp@1\ns/source.layer.hdmi@HDMI1/0\n\
                   s/source.layer.hdmi@HDMI4\ns/source.layer.hdmi@HDMI2\nEOFCOBA";
        let found = sources(cache, row);
        let names: Vec<_> = found.iter().map(|source| source.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "XBOX SeriesX|S",
                "HDMI4",
                "PlayStation 5",
                "TV",
                "Sonos Play:3 (172.17.2.219)",
                "Plex: NAS"
            ]
        );
        assert_eq!(
            found[4].dlna.as_ref().map(|(key, _)| key.as_str()),
            Some("uuid:B+eth0")
        );
        assert!(found[4]
            .dlna
            .as_ref()
            .unwrap()
            .1
            .starts_with("172.17.2.219 - Sonos"));
        assert_eq!(found[0].dlna, None);
        assert_eq!(found[0].icon.as_deref(), Some("/i/x_n.png"));
        assert_eq!((found[0].kind, found[0].mbr), (13, 0));
        assert_eq!(found[2].icon.as_deref(), Some("/i/pad.svg"));
        assert!(sources("not json", row).is_empty());
    }

    #[test]
    fn parses_rows() {
        let rows = vec![
            "111299001912\u{1f}YouTube\u{1f}9Ur5IzDKqV.TizenYouTube\u{1f}/nonexistent/\u{1f}483"
                .to_owned(),
            "broken".to_owned(),
        ];
        let apps = parse(&rows);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].app_id, "9Ur5IzDKqV.TizenYouTube");
        assert_eq!(apps[0].icon, None);
        assert_eq!(apps[0].launches, 483);
    }
}
