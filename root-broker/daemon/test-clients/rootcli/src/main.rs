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

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

const DEFAULT_SOCKET: &str = "/run/tv-auth/broker.sock";

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

    match send(&socket, &request) {
        Ok(response) => {
            println!("{response}");
            if response.contains("\"status\":\"ok\"") {
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

fn build_request(positional: &[String]) -> Result<String, String> {
    let (method, fields) = match positional {
        [command, rest @ ..] if command == "list" && rest.is_empty() => ("pkg.list", String::new()),
        [command, pkg_type, path] if command == "install" => {
            if !matches!(pkg_type.as_str(), "wgt" | "tpk") {
                return Err(format!("unknown package type {pkg_type}"));
            }
            (
                "pkg.install",
                format!(
                    ",\"pkg_type\":\"{pkg_type}\",\"path\":{}",
                    json_string(path)
                ),
            )
        }
        [command, pkgid] if command == "uninstall" => {
            ("pkg.uninstall", format!(",\"pkgid\":{}", json_string(pkgid)))
        }
        _ => return Err("expected list, install <type> <path>, or uninstall <pkgid>".into()),
    };
    Ok(format!(
        "{{\"version\":2,\"id\":\"cli-{}\",\"method\":\"{method}\"{fields}}}",
        std::process::id()
    ))
}

fn send(socket: &str, request: &str) -> std::io::Result<String> {
    let stream = UnixStream::connect(socket)?;
    let mut writer = stream.try_clone()?;
    writer.write_all(request.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim_end().to_owned())
}

/// JSON string literal for a package id or path (quotes, backslashes, and
/// control characters are the only things a caller can smuggle in).
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
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
    use super::{build_request, json_string};

    #[test]
    fn escapes_json_strings() {
        assert_eq!(json_string("org.example.app"), "\"org.example.app\"");
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn builds_requests() {
        assert!(build_request(&["list".into()])
            .unwrap()
            .contains("\"method\":\"pkg.list\""));
        let install = build_request(&[
            "install".into(),
            "wgt".into(),
            "/tmp/a b.wgt".into(),
        ])
        .unwrap();
        assert!(install.contains("\"pkg_type\":\"wgt\""));
        assert!(install.contains("\"path\":\"/tmp/a b.wgt\""));
        assert!(build_request(&["uninstall".into(), "org.example.app".into()])
            .unwrap()
            .contains("\"pkgid\":\"org.example.app\""));
        assert!(build_request(&["install".into(), "rpm".into(), "/x".into()]).is_err());
        assert!(build_request(&[]).is_err());
    }
}
