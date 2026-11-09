//! The Apps2Samsung catalog (`resolved.json.signed`), reduced to what the
//! store screen shows and installs. Pure functions, host-tested; the network,
//! the RSA check (TV libcrypto) and the UI live in `main.rs`.
//!
//! Trust chain: TLS-validated download → RSA-SHA256 signature over the
//! payload by the pinned Apps2Samsung key → per-file SHA-256 from the signed
//! payload → `tv-authd` re-signs and the user approves the install prompt.

use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::HashMap;

pub const CATALOG_URL: &str = "https://apps2samsung.com/catalog/resolved.json.signed";
/// Public half of the Apps2Samsung catalog signing key, as pinned by their
/// TV-Store (`tizen-app/service/catalog-key.js`).
pub const CATALOG_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAuSxPAaQONXHVJJ2gsndQ
IHyTpG1CZP9lScSqCXFQad0h+m5xFtQ03mKGk67am7X5P7+SzOSPEBp+XIFjVOEe
EfeLtm/+kX4M96CpgjtZLiuKa2EYd/1OFt9+ZYFaVUaJlprpQieLLFXpeENiG6MX
AVbB8UCMkIGJz+eigeIkYU7HYO+9+HUS9oTvqeKoXZVUnDSNHYTCVWmzprIbXgLO
va/bsHLXhGBl3IeNbfkC1nsHZjbcNxSFw6E+DLWv+2uZGM2Avx9vgYrMjgmbIyXc
spu4wGJBs4p33lKCFltwXSuDrN8KBiLluK9pjXYLaOgzUhrKN0yzdAZewhXDD5tK
zBUROgMjMDa+gc4CE0zmEfcbZ/mQU1FNt+y/TVig4tt86x7UoOyW+mLub58tOC/b
eLANPPgLtNE+IOKOaaY3Y/4u9epiwPbYZR6lXo2T5NvDQjwSrOQ9l+I8imhrhupy
bY4CTb76QMVny2rt5cQXnHasIhZkf6oi5m8KrePWCGVHAgMBAAE=
-----END PUBLIC KEY-----
";
pub const MAX_CATALOG_BYTES: usize = 8 << 20;
const MAX_AGE_SECONDS: i64 = 7 * 24 * 3600;
const MAX_FUTURE_SECONDS: i64 = 3600;
pub const MAX_PACKAGE_BYTES: u64 = 256 << 20;
/// ponytail: the suite targets Tizen 6.5 sets only; read the platform
/// version (capi-system-info) if other firmware generations matter.
pub const TIZEN_VERSION: &str = "6.5";

/// One installable file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub name: String,
    /// The app this file is a build of (rows group by it).
    pub group: String,
    /// What distinguishes this build within its group (empty if it is the
    /// only one the catalog describes that way).
    pub variant: String,
    pub description: String,
    pub category: String,
    pub package_id: String,
    pub version: String,
    /// `wgt` or `tpk`.
    pub pkg_type: String,
    pub url: String,
    pub size: u64,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
    /// Icon URL and its lower-case hex SHA-256, when the catalog has both.
    pub icon: Option<(String, String)>,
    /// Needs a Partner-level signature (`cert_level: partner`).
    pub partner: bool,
}

/// Where a catalog app stands against the installed packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    NotInstalled,
    Installed,
    Upgradable {
        installed: String,
    },
    /// Installed version is newer than the catalog's.
    Newer {
        installed: String,
    },
}

pub fn state(app: &App, installed: &HashMap<String, String>) -> State {
    match installed.get(&app.package_id) {
        None => State::NotInstalled,
        Some(version) => match compare_versions(version, &app.version) {
            Ordering::Less => State::Upgradable {
                installed: version.clone(),
            },
            Ordering::Equal => State::Installed,
            Ordering::Greater => State::Newer {
                installed: version.clone(),
            },
        },
    }
}

/// Split `<base64 signature>\n<payload>`; the signature line must end in the
/// first 4096 bytes.
pub fn envelope(bytes: &[u8]) -> Result<(Vec<u8>, &[u8]), String> {
    use base64::Engine as _;
    let newline = bytes
        .iter()
        .take(4096)
        .position(|byte| *byte == b'\n')
        .ok_or("the catalog has no signature line")?;
    let line = std::str::from_utf8(&bytes[..newline])
        .map_err(|_| "the catalog signature line is not text")?
        .trim();
    let signature = base64::engine::general_purpose::STANDARD
        .decode(line)
        .map_err(|_| "the catalog signature line is not base64")?;
    Ok((signature, &bytes[newline + 1..]))
}

#[derive(Deserialize)]
struct Catalog {
    #[serde(rename = "generatedAt", default)]
    generated_at: String,
    #[serde(default)]
    providers: Vec<Provider>,
}

#[derive(Deserialize)]
struct Provider {
    #[serde(default)]
    id: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(default)]
    releases: Vec<Release>,
    #[serde(default)]
    catalog: Option<Community>,
}

#[derive(Deserialize)]
struct Release {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize, Clone)]
struct Asset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: String,
    #[serde(default)]
    package_version: String,
    #[serde(default)]
    package_id: String,
    #[serde(default)]
    required_version: String,
    #[serde(default)]
    icon: String,
    #[serde(default)]
    icon_digest: String,
}

#[derive(Deserialize)]
struct Community {
    #[serde(default)]
    apps: Vec<CommunityApp>,
}

#[derive(Deserialize)]
struct CommunityApp {
    #[serde(default)]
    file: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    variant: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    adult: bool,
    #[serde(default)]
    cert_level: String,
}

/// Parse a verified payload into installable apps, sorted by name. Rejects a
/// catalog older than 7 days or more than 1 hour in the future (`now` is Unix
/// seconds), like Apps2Samsung's own TV-Store.
pub fn apps(payload: &[u8], now: i64) -> Result<Vec<App>, String> {
    let catalog: Catalog =
        serde_json::from_slice(payload).map_err(|error| format!("bad catalog JSON: {error}"))?;
    let generated = parse_rfc3339(&catalog.generated_at)
        .ok_or_else(|| format!("bad catalog date {:?}", catalog.generated_at))?;
    if now - generated > MAX_AGE_SECONDS {
        return Err(format!(
            "the catalog is from {}, more than 7 days old (check the TV clock)",
            catalog.generated_at
        ));
    }
    if generated - now > MAX_FUTURE_SECONDS {
        return Err(format!(
            "the catalog is dated {} in the future (check the TV clock)",
            catalog.generated_at
        ));
    }

    let mut apps = Vec::new();
    for provider in &catalog.providers {
        let Some(release) = provider.releases.first() else {
            continue;
        };
        match &provider.catalog {
            Some(community) => {
                let by_name: HashMap<&str, &Asset> = release
                    .assets
                    .iter()
                    .map(|asset| (asset.name.as_str(), asset))
                    .collect();
                for entry in &community.apps {
                    if entry.adult {
                        continue;
                    }
                    let Some(asset) = by_name.get(entry.file.as_str()) else {
                        continue;
                    };
                    let name = if !entry.group.is_empty() && !entry.variant.is_empty() {
                        format!("{} ({})", entry.group, entry.variant)
                    } else if entry.name.is_empty() {
                        entry.file.clone()
                    } else {
                        entry.name.clone()
                    };
                    let category = if entry.category.is_empty() {
                        "other"
                    } else {
                        &entry.category
                    };
                    let partner = entry.cert_level.eq_ignore_ascii_case("partner");
                    let (group, variant) = if entry.group.is_empty() {
                        (name.clone(), String::new())
                    } else {
                        (entry.group.clone(), entry.variant.clone())
                    };
                    apps.extend(app(asset, name, &entry.description, category, partner).map(
                        |app| App {
                            group,
                            variant,
                            ..app
                        },
                    ));
                }
            }
            None => {
                let title = if provider.display_name.is_empty() {
                    title_case(&provider.id)
                } else {
                    provider.display_name.clone()
                };
                for asset in &release.assets {
                    let stem = asset
                        .name
                        .rsplit_once('.')
                        .map_or(asset.name.as_str(), |(stem, _)| stem);
                    let description = format!("{title} release {}", release.tag_name);
                    apps.extend(
                        app(asset, stem.to_owned(), &description, "media", false).map(|app| App {
                            group: title.clone(),
                            variant: stem.to_owned(),
                            ..app
                        }),
                    );
                }
            }
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    Ok(apps)
}

fn app(
    asset: &Asset,
    name: String,
    description: &str,
    category: &str,
    partner: bool,
) -> Option<App> {
    let pkg_type = asset.name.rsplit_once('.')?.1.to_ascii_lowercase();
    if !matches!(pkg_type.as_str(), "wgt" | "tpk")
        || !asset.browser_download_url.starts_with("https://")
        || asset.package_id.is_empty()
        || asset.size == 0
        || asset.size > MAX_PACKAGE_BYTES
        || (!asset.required_version.is_empty()
            && compare_versions(&asset.required_version, TIZEN_VERSION) == Ordering::Greater)
    {
        return None;
    }
    let sha256 = hex_digest(&asset.digest)?;
    let icon = (asset.icon.starts_with("https://"))
        .then(|| hex_digest(&asset.icon_digest).map(|digest| (asset.icon.clone(), digest)))
        .flatten();
    Some(App {
        group: name.clone(),
        variant: String::new(),
        name,
        description: description.to_owned(),
        category: category.to_owned(),
        package_id: asset.package_id.clone(),
        version: asset.package_version.clone(),
        pkg_type,
        url: asset.browser_download_url.clone(),
        size: asset.size,
        sha256,
        icon,
        partner,
    })
}

/// `jellyfin-tizen-builds` → `Jellyfin Tizen Builds`.
fn title_case(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or(String::new(), |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// All builds of one app, as the store lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub description: String,
    pub category: String,
    /// Icon URL and SHA-256 of the first build that has one.
    pub icon: Option<(String, String)>,
    /// Sorted by name.
    pub variants: Vec<App>,
}

impl Group {
    /// Whether any build's package is installed.
    pub fn installed(&self, installed: &HashMap<String, String>) -> bool {
        self.variants
            .iter()
            .any(|app| installed.contains_key(&app.package_id))
    }

    fn matches(&self, query: &str) -> bool {
        let contains = |text: &str| text.to_lowercase().contains(query);
        query.is_empty()
            || contains(&self.name)
            || contains(&self.description)
            || self
                .variants
                .iter()
                .any(|app| contains(&app.variant) || contains(&app.package_id))
    }
}

/// Collect builds into groups, in first-seen order.
pub fn groups(apps: Vec<App>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for app in apps {
        let slot = *index.entry(app.group.clone()).or_insert_with(|| {
            groups.push(Group {
                name: app.group.clone(),
                description: app.description.clone(),
                category: app.category.clone(),
                icon: None,
                variants: Vec::new(),
            });
            groups.len() - 1
        });
        let group = &mut groups[slot];
        if group.icon.is_none() {
            group.icon = app.icon.clone();
        }
        group.variants.push(app);
    }
    for group in &mut groups {
        group
            .variants
            .sort_by_key(|app| (app.variant.to_lowercase(), app.name.to_lowercase()));
    }
    groups
}

/// Indices of the groups to list: in `category` (empty = all) and matching
/// `query` (case-insensitive substring), installed ones first, then by name.
pub fn arrange(
    groups: &[Group],
    installed: &HashMap<String, String>,
    category: &str,
    query: &str,
) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let mut indices: Vec<usize> = (0..groups.len())
        .filter(|&index| category.is_empty() || groups[index].category == category)
        .filter(|&index| groups[index].matches(&query))
        .collect();
    indices.sort_by_key(|&index| {
        (
            !groups[index].installed(installed),
            groups[index].name.to_lowercase(),
        )
    });
    indices
}

/// `sha256:<64 hex>` → lower-case hex.
fn hex_digest(value: &str) -> Option<String> {
    let hex = value.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

/// Dotted numeric versions (`1.10.0` > `1.9`); non-numeric parts compare as
/// text, missing parts as 0.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (l, r) => {
                let (l, r) = (l.unwrap_or("0"), r.unwrap_or("0"));
                let order = match (l.parse::<u64>(), r.parse::<u64>()) {
                    (Ok(l), Ok(r)) => l.cmp(&r),
                    _ => l.cmp(r),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// `2026-11-09T20:53:47.6130349+00:00` / `...Z` → Unix seconds.
pub fn parse_rfc3339(value: &str) -> Option<i64> {
    let (date, rest) = value.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (clock, offset) = if let Some(clock) = rest.strip_suffix('Z') {
        (clock, 0)
    } else {
        let split = rest.rfind(['+', '-'])?;
        let (clock, zone) = rest.split_at(split);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = zone[1..].split_once(':')?;
        (
            clock,
            sign * (hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60),
        )
    };
    let clock = clock.split('.').next()?;
    let mut clock = clock.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (
        clock.next()?.ok()?,
        clock.next()?.ok()?,
        clock.next()?.ok()?,
    );
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_794_260_400; // 2026-11-09T21:40:00Z

    fn asset(name: &str, package_id: &str, version: &str, required: &str) -> serde_json::Value {
        json!({
            "name": name,
            "browser_download_url": format!("https://example.com/{name}"),
            "size": 1234,
            "digest": format!("sha256:{}", "AB".repeat(32)),
            "package_version": version,
            "package_id": package_id,
            "required_version": required,
            "icon": "https://example.com/i.png",
            "icon_digest": format!("sha256:{}", "cd".repeat(32)),
        })
    }

    fn catalog(generated: &str) -> Vec<u8> {
        json!({
            "schemaVersion": 1,
            "generatedAt": generated,
            "providers": [
                {"id": "jellyfin", "releases": [
                    {"tag_name": "t2", "assets": [asset("Jellyfin-A.wgt", "AprZAARz4r", "0.2.0", "2.3"), asset("notes.txt", "x", "1", "")]},
                    {"tag_name": "t1", "assets": [asset("Old.wgt", "OldOld0000", "0.1.0", "2.3")]}
                ]},
                {"id": "tizen-community", "catalog": {"apps": [
                    {"file": "Zed.wgt", "name": "Zed", "description": "z", "category": "games"},
                    {"file": "Adult.wgt", "name": "Adult", "adult": true},
                    {"file": "New.wgt", "name": "TooNew"},
                    {"file": "Grp.tpk", "name": "x", "group": "Group", "variant": "Tizen 6", "cert_level": "partner"},
                    {"file": "Missing.wgt", "name": "Missing"}
                ]}, "releases": [{"tag_name": "community-1", "assets": [
                    asset("Zed.wgt", "ZedZedZed0", "1.0.0", "5.5"),
                    asset("Adult.wgt", "AdultAdult", "1.0.0", "5.5"),
                    asset("New.wgt", "NewNewNew0", "1.0.0", "7.0"),
                    asset("Grp.tpk", "org.ex.grp", "2.0", "6.5")
                ]}]}
            ]
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn flattens_providers_and_filters() {
        let apps = apps(&catalog("2026-11-09T20:53:47.6130349+00:00"), NOW).unwrap();
        let names: Vec<&str> = apps.iter().map(|app| app.name.as_str()).collect();
        assert_eq!(names, ["Group (Tizen 6)", "Jellyfin-A", "Zed"]);
        assert_eq!(
            (apps[0].group.as_str(), apps[0].variant.as_str()),
            ("Group", "Tizen 6")
        );
        assert_eq!(
            (apps[1].group.as_str(), apps[1].variant.as_str()),
            ("Jellyfin", "Jellyfin-A")
        );
        assert_eq!(
            (apps[2].group.as_str(), apps[2].variant.as_str()),
            ("Zed", "")
        );
        let group = &apps[0];
        assert!(group.partner);
        assert_eq!(group.pkg_type, "tpk");
        let jellyfin = &apps[1];
        assert_eq!(jellyfin.description, "Jellyfin release t2");
        assert_eq!(jellyfin.sha256, "ab".repeat(32));
        assert_eq!(jellyfin.icon.as_ref().unwrap().1, "cd".repeat(32));
        assert_eq!(apps[2].category, "games");
    }

    fn build(group: &str, variant: &str, package_id: &str, category: &str) -> App {
        App {
            name: format!("{group} {variant}"),
            group: group.into(),
            variant: variant.into(),
            description: format!("about {group}"),
            category: category.into(),
            package_id: package_id.into(),
            version: "1.0".into(),
            pkg_type: "wgt".into(),
            url: String::new(),
            size: 1,
            sha256: String::new(),
            icon: None,
            partner: false,
        }
    }

    #[test]
    fn groups_sorts_and_filters() {
        let groups = groups(vec![
            build("Zed", "", "Zed", "games"),
            build("Jellyfin", "B", "Jelly", "media"),
            build("Alpha", "", "Alpha", "media"),
            build("Jellyfin", "A", "Jelly", "media"),
        ]);
        assert_eq!(groups.len(), 3);
        let jellyfin = &groups[1];
        let variants: Vec<&str> = jellyfin
            .variants
            .iter()
            .map(|app| app.variant.as_str())
            .collect();
        assert_eq!(variants, ["A", "B"]);

        let names = |indices: Vec<usize>| -> Vec<String> {
            indices
                .into_iter()
                .map(|index| groups[index].name.clone())
                .collect()
        };
        let mut installed = HashMap::new();
        assert_eq!(
            names(arrange(&groups, &installed, "", "")),
            ["Alpha", "Jellyfin", "Zed"]
        );
        installed.insert("Zed".to_owned(), "1.0".to_owned());
        assert_eq!(
            names(arrange(&groups, &installed, "", "")),
            ["Zed", "Alpha", "Jellyfin"]
        );
        assert_eq!(
            names(arrange(&groups, &installed, "media", "")),
            ["Alpha", "Jellyfin"]
        );
        assert_eq!(
            names(arrange(&groups, &installed, "", " JELLY ")),
            ["Jellyfin"]
        );
        assert_eq!(names(arrange(&groups, &installed, "", "about z")), ["Zed"]);
        assert_eq!(title_case("jellyfin-tizen-builds"), "Jellyfin Tizen Builds");
    }

    #[test]
    fn rejects_stale_and_future_catalogs() {
        assert!(apps(&catalog("2026-11-01T00:00:00Z"), NOW)
            .unwrap_err()
            .contains("7 days"));
        assert!(apps(&catalog("2026-11-10T00:00:00Z"), NOW)
            .unwrap_err()
            .contains("future"));
        assert!(apps(&catalog("yesterday"), NOW).is_err());
    }

    #[test]
    fn splits_the_signed_envelope() {
        let (signature, payload) = envelope(b"AAEC\n{\"a\":1}").unwrap();
        assert_eq!(signature, [0, 1, 2]);
        assert_eq!(payload, b"{\"a\":1}");
        assert!(envelope(b"no newline").is_err());
        assert!(envelope(b"!!!\n{}").is_err());
    }

    #[test]
    fn compares_versions_and_states() {
        assert_eq!(compare_versions("1.10.0", "1.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("2.3", "6.5"), Ordering::Less);
        let app = apps(&catalog("2026-11-09T21:00:00Z"), NOW)
            .unwrap()
            .remove(2);
        let mut installed = HashMap::new();
        assert_eq!(state(&app, &installed), State::NotInstalled);
        installed.insert("ZedZedZed0".to_owned(), "0.9".to_owned());
        assert_eq!(
            state(&app, &installed),
            State::Upgradable {
                installed: "0.9".into()
            }
        );
        installed.insert("ZedZedZed0".to_owned(), "1.0.0".to_owned());
        assert_eq!(state(&app, &installed), State::Installed);
    }

    /// `STORE_CATALOG=/path/resolved.json.signed cargo test -p tv-auth-ui
    /// --lib -- --ignored` checks a real catalog (skipping the age check).
    #[test]
    #[ignore]
    fn parses_a_real_catalog() {
        let bytes = std::fs::read(std::env::var("STORE_CATALOG").unwrap()).unwrap();
        let (signature, payload) = envelope(&bytes).unwrap();
        assert_eq!(signature.len(), 384); // RSA-3072
        let generated: Catalog = serde_json::from_slice(payload).unwrap();
        let apps = apps(payload, parse_rfc3339(&generated.generated_at).unwrap()).unwrap();
        let installable = apps.len();
        let icons = apps.iter().filter(|app| app.icon.is_some()).count();
        eprintln!("{installable} apps, {icons} with icons");
        let groups = groups(apps);
        let mut sizes: Vec<(usize, &str)> = groups
            .iter()
            .map(|group| (group.variants.len(), group.name.as_str()))
            .collect();
        sizes.sort();
        eprintln!(
            "{} groups, largest {:?}",
            groups.len(),
            &sizes[sizes.len() - 6..]
        );
        assert!(installable > 100);
    }

    #[test]
    fn parses_rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-09T21:40:00Z"), Some(NOW));
        assert_eq!(parse_rfc3339("2026-11-09T22:40:00+01:00"), Some(NOW));
        assert_eq!(
            parse_rfc3339("2000-02-29T00:00:00.5-00:30"),
            Some(951_784_200)
        );
    }
}
