//! On-TV smoke test for tv-tizen. Copy under /run (UEP) and run, e.g.:
//!
//! ```text
//! probe vconf-int db/menu/support/softwareupdate/autoupdate
//! probe vconf-str db/menu_widget/language
//! probe packages
//! probe running org.tvauth.ui
//! probe launch org.tvauth.ui
//! probe get https://example.com/
//! ```

use std::process::ExitCode;
use tv_tizen::{app, http, pkgdb, vconf};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["vconf-int", key] => vconf::get_int(key).map(|value| value.to_string()),
        ["vconf-bool", key] => vconf::get_bool(key).map(|value| value.to_string()),
        ["vconf-str", key] => vconf::get_str(key),
        ["packages"] => pkgdb::packages(pkgdb::PKGMGR_PARSER_DB).map(|packages| {
            let mut out = format!("{} packages\n", packages.len());
            for package in packages.iter().filter(|package| package.pkg_type != "rpm") {
                out += &format!(
                    "{} {} {} main={} label={:?}\n",
                    package.pkgid, package.pkg_type, package.version, package.main_app_id, package.label
                );
            }
            out
        }),
        ["running", app_id] => app::is_running(app_id).map(|running| running.to_string()),
        ["launch", app_id] => app::launch(app_id).map(|()| "launched".into()),
        ["get", url] => http::get(url, 4 << 20, 20).map(|body| {
            format!(
                "{} bytes: {}",
                body.len(),
                String::from_utf8_lossy(&body[..body.len().min(120)])
            )
        }),
        _ => Err("usage: probe vconf-int|vconf-bool|vconf-str KEY | packages | running|launch APPID | get URL".into()),
    };
    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("probe: {error}");
            ExitCode::FAILURE
        }
    }
}
