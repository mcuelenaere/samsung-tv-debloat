//! The dock's app list: webappservice's catalog (the one Samsung's Home
//! shows, sideloaded apps included), in Home's row order, then by title.
use std::path::{Path, PathBuf};

pub const DATABASE: &str = "/opt/dbspace/.webappservice.db";
/// Samsung Home's row, mirrored by tv-patchd.
pub const ROW: &str = "/run/tv-suite/home-row";

/// One row per app Home shows; fields joined with U+001F.
pub const SQL: &str = "SELECT id || char(31) || title || char(31) || tizen_id || char(31) || \
     icon_path FROM widgetinfo WHERE is_hidden = 0 AND tizen_id != ''";

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
            let icon = ICONS
                .iter()
                .map(|name| Path::new(icons).join(name))
                .find(|path| path.is_file());
            Some(App {
                id: id.to_owned(),
                title: title.to_owned(),
                app_id: app_id.to_owned(),
                icon,
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

/// Home's row first, then the rest by title.
pub fn order(mut apps: Vec<App>, row: &[&str]) -> Vec<App> {
    apps.sort_by_cached_key(|app| {
        let position = row
            .iter()
            .position(|id| *id == app.id)
            .unwrap_or(usize::MAX);
        (position, app.title.to_lowercase())
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
    fn parses_rows() {
        let rows = vec![
            "111299001912\u{1f}YouTube\u{1f}9Ur5IzDKqV.TizenYouTube\u{1f}/nonexistent/".to_owned(),
            "broken".to_owned(),
        ];
        let apps = parse(&rows);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].app_id, "9Ur5IzDKqV.TizenYouTube");
        assert_eq!(apps[0].icon, None);
    }
}
