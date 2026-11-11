//! ```text
//! tv-pkg verify <package.tvpkg> [--json]
//! tv-pkg install <package.tvpkg> [--adopt-existing] [--json]
//! tv-pkg list [--json]
//! tv-pkg status <component-id> [--json]
//! tv-pkg remove <component-id> [--json]
//! ```

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tv_pkg::systemd::ProcessRunner;
use tv_pkg::tizen::SocketBroker;
use tv_pkg::{Context, Error, Outcome};

const USAGE: &str = "usage: tv-pkg verify <package.tvpkg> [--json]
       tv-pkg install <package.tvpkg> [--adopt-existing] [--json]
       tv-pkg list [--json]
       tv-pkg status <component-id> [--json]
       tv-pkg remove <component-id> [--json]";

fn main() -> ExitCode {
    let mut json = false;
    let mut adopt = false;
    let mut positional = Vec::new();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--json" => json = true,
            "--adopt-existing" => adopt = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ if argument.starts_with('-') => return usage(&format!("unknown option {argument}")),
            _ => positional.push(argument),
        }
    }
    let positional: Vec<&str> = positional.iter().map(String::as_str).collect();
    if adopt && positional.first() != Some(&"install") {
        return usage("--adopt-existing only applies to install");
    }

    let runner = ProcessRunner;
    let broker = SocketBroker(PathBuf::from(tv_proto::DEFAULT_SOCKET));
    let ctx = Context {
        root: PathBuf::from("/"),
        apply_security: true,
        runner: &runner,
        broker: &broker,
    };

    let result: Result<Map<String, Value>, Error> = match positional[..] {
        ["verify", package] => tv_pkg::verify(Path::new(package)).map(outcome),
        ["install", package] => tv_pkg::install(&ctx, Path::new(package), adopt).map(outcome),
        ["remove", id] => tv_pkg::remove(&ctx, id).map(outcome),
        ["list"] => tv_pkg::list(&ctx).map(|states| {
            let components: Vec<Value> = states
                .iter()
                .map(|s| {
                    json!({"id": s.manifest.id, "version": s.manifest.version,
                           "kind": s.manifest.kind.as_str(), "digest": s.digest})
                })
                .collect();
            Map::from_iter([("components".to_owned(), Value::from(components))])
        }),
        ["status", id] => tv_pkg::status(&ctx, id).map(|(state, problems)| {
            let mut fields = outcome(Outcome {
                id: state.manifest.id.clone(),
                version: state.manifest.version.clone(),
                digest: state.digest.clone(),
                changed: None,
            });
            fields.insert("kind".into(), state.manifest.kind.as_str().into());
            fields.insert("healthy".into(), problems.is_empty().into());
            fields.insert("problems".into(), problems.into());
            fields
        }),
        _ => return usage("unknown command or wrong arguments"),
    };

    let (mut fields, status, code) = match result {
        Ok(fields) => {
            let status = if fields.get("changed") == Some(&Value::Bool(false)) {
                "unchanged"
            } else {
                "ok"
            };
            let unhealthy = fields.get("healthy") == Some(&Value::Bool(false));
            (
                fields,
                status,
                if unhealthy {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                },
            )
        }
        Err(error) if error.code == "busy" => (error_fields(&error), "busy", ExitCode::from(75)),
        Err(error) => (error_fields(&error), "error", ExitCode::FAILURE),
    };
    fields.insert("status".into(), status.into());
    if json {
        fields.insert("format".into(), 1.into());
        println!("{}", Value::Object(fields));
    } else {
        print_text(&fields);
    }
    code
}

fn outcome(outcome: Outcome) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("id".into(), outcome.id.into());
    fields.insert("version".into(), outcome.version.into());
    fields.insert("digest".into(), outcome.digest.into());
    if let Some(changed) = outcome.changed {
        fields.insert("changed".into(), changed.into());
    }
    fields
}

fn error_fields(error: &Error) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert(
        "error".into(),
        format!("{}: {}", error.code, error.message).into(),
    );
    fields
}

fn print_text(fields: &Map<String, Value>) {
    let text = |name: &str| {
        fields
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    if let Some(error) = fields.get("error").and_then(Value::as_str) {
        eprintln!("tv-pkg: {error}");
        return;
    }
    if let Some(components) = fields.get("components").and_then(Value::as_array) {
        for c in components {
            println!(
                "{}\t{}\t{}\t{}",
                c["id"].as_str().unwrap_or(""),
                c["version"].as_str().unwrap_or(""),
                c["kind"].as_str().unwrap_or(""),
                c["digest"].as_str().unwrap_or("")
            );
        }
        return;
    }
    println!(
        "{} {} {} {}",
        text("status"),
        text("id"),
        text("version"),
        text("digest")
    );
    for problem in fields
        .get("problems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        println!("  {}", problem.as_str().unwrap_or(""));
    }
}

fn usage(error: &str) -> ExitCode {
    eprintln!("tv-pkg: {error}\n{USAGE}");
    ExitCode::from(2)
}
