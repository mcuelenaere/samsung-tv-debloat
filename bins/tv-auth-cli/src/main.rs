//! Minimal root-role client for tv-authd's package methods.
//!
//! The daemon gives uid 0 (`System::Privileged`) prompt-free access to
//! `pkg.install`, `pkg.uninstall`, and `pkg.list`. This tiny static binary
//! exercises that role from a TV root shell, where the stock shell tooling is
//! gated by UEP.
//!
//! ```text
//! tvauth-cli [--socket PATH] list
//! tvauth-cli [--socket PATH] install <wgt|tpk> <path>
//! tvauth-cli [--socket PATH] uninstall <pkgid>
//! ```
//!
//! It prints the raw response line and exits 0 when the status is `ok`.

use serde_json::{json, Value};
use std::process::ExitCode;
use tv_proto::DEFAULT_SOCKET;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut socket = DEFAULT_SOCKET.to_owned();
    let mut positional = Vec::new();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--socket" => match args.next() {
                Some(path) => socket = path,
                None => return usage("--socket needs a path"),
            },
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other => positional.push(other.to_owned()),
        }
    }

    let request = match build_request(&positional) {
        Ok(request) => request,
        Err(error) => return usage(&error),
    };

    match tv_proto::call(&socket, &request) {
        Ok(response) => {
            println!("{}", String::from_utf8_lossy(&response));
            let status = serde_json::from_slice::<Value>(&response)
                .ok()
                .and_then(|value| value.get("status").cloned());
            if status == Some(json!("ok")) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("tvauth-cli: {error}");
            ExitCode::FAILURE
        }
    }
}

fn build_request(positional: &[String]) -> Result<Value, String> {
    let id = format!("cli-{}", std::process::id());
    match positional {
        [command] if command == "list" => Ok(json!({"version": 2, "id": id, "method": "pkg.list"})),
        [command, pkg_type, path] if command == "install" => {
            if !matches!(pkg_type.as_str(), "wgt" | "tpk") {
                return Err(format!("unknown package type {pkg_type}"));
            }
            Ok(json!({"version": 2, "id": id, "method": "pkg.install",
                      "pkg_type": pkg_type, "path": path}))
        }
        [command, pkgid] if command == "uninstall" => {
            Ok(json!({"version": 2, "id": id, "method": "pkg.uninstall", "pkgid": pkgid}))
        }
        _ => Err("expected list, install <type> <path>, or uninstall <pkgid>".into()),
    }
}

fn usage(error: &str) -> ExitCode {
    eprintln!("tvauth-cli: {error}");
    print_usage();
    ExitCode::FAILURE
}

fn print_usage() {
    eprintln!(
        "usage: tvauth-cli [--socket PATH] list\n\
         \x20      tvauth-cli [--socket PATH] install <wgt|tpk> <path>\n\
         \x20      tvauth-cli [--socket PATH] uninstall <pkgid>"
    );
}

#[cfg(test)]
mod tests {
    use super::build_request;

    /// Requests must parse with the daemon's own (strict) parser.
    #[test]
    fn builds_requests_the_daemon_accepts() {
        let parse = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            let request = build_request(&args).unwrap().to_string();
            tv_proto::parse_request(request.as_bytes()).unwrap()
        };
        assert!(matches!(parse(&["list"]), tv_proto::Request::PkgList(_)));
        match parse(&["install", "wgt", "/tmp/a \"b\".wgt"]) {
            tv_proto::Request::PkgInstall(request) => {
                assert_eq!(request.pkg_type, tv_proto::PackageType::Wgt);
                assert_eq!(request.path, "/tmp/a \"b\".wgt");
            }
            other => panic!("unexpected {other:?}"),
        }
        match parse(&["uninstall", "org.example.app"]) {
            tv_proto::Request::PkgUninstall(request) => {
                assert_eq!(request.pkgid, "org.example.app")
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(build_request(&["install".into(), "rpm".into(), "/x".into()]).is_err());
        assert!(build_request(&[]).is_err());
    }
}
