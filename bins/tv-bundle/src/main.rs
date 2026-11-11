//! Command-line wrapper for the reproducible package and setup builder.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: tv-bundle package <spec.json> --source <dir> --output <file.tvpkg>
       tv-bundle setup --source <dir> --target-build <build> --output <manifest.json> [--order <file>]
       tv-bundle hash <file>

`setup` expects fdet/{modern,legacy}/, tv-pkg, setup-driver.sh and the files
listed in the package order file (one `<id> <path>` pair per line).";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tv-bundle: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    match command.as_str() {
        "package" => package(rest),
        "setup" => setup(rest),
        "hash" => {
            let [path] = rest else {
                return Err(USAGE.into());
            };
            println!(
                "{}",
                tv_bundle::file_sha256(std::path::Path::new(path))
                    .map_err(|error| error.to_string())?
            );
            Ok(())
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

fn package(args: &[String]) -> Result<(), String> {
    let Some(spec) = args.first().filter(|arg| !arg.starts_with('-')) else {
        return Err(USAGE.into());
    };
    let (source, output) = flags(&args[1..], &["--source", "--output"])?;
    let source = source.ok_or("package needs --source <dir>")?;
    let output = output.ok_or("package needs --output <file.tvpkg>")?;
    let digest = tv_bundle::build_package(
        std::path::Path::new(spec),
        std::path::Path::new(&source),
        std::path::Path::new(&output),
    )
    .map_err(|error| error.to_string())?;
    println!("{output} {digest}");
    Ok(())
}

fn setup(args: &[String]) -> Result<(), String> {
    let (source, output) = flags(args, &["--source", "--output", "--target-build", "--order"])?;
    let source = source.ok_or("setup needs --source <dir>")?;
    let output = output.ok_or("setup needs --output <manifest.json>")?;
    let target = args
        .iter()
        .position(|arg| arg == "--target-build")
        .and_then(|index| args.get(index + 1))
        .ok_or("setup needs --target-build <build>")?;
    let order = args
        .iter()
        .position(|arg| arg == "--order")
        .and_then(|index| args.get(index + 1));
    let order = match order {
        Some(path) => {
            tv_bundle::read_order(std::path::Path::new(path)).map_err(|error| error.to_string())?
        }
        None => {
            let mut packages = Vec::new();
            for entry in std::fs::read_dir(PathBuf::from(&source).join("packages"))
                .map_err(|error| error.to_string())?
            {
                let path = entry.map_err(|error| error.to_string())?.path();
                if path.extension().is_some_and(|ext| ext == "tvpkg") {
                    let name = format!("packages/{}", path.file_name().unwrap().to_string_lossy());
                    let id = id_from_package(&path)?;
                    packages.push((id, name));
                }
            }
            packages.sort();
            packages
        }
    };
    let manifest = tv_bundle::build_setup_manifest(
        std::path::Path::new(&source),
        target,
        &order,
        std::path::Path::new(&output),
    )
    .map_err(|error| error.to_string())?;
    println!("{output} {}", manifest.bundle_sha256);
    Ok(())
}

fn flags(args: &[String], names: &[&str]) -> Result<(Option<String>, Option<String>), String> {
    let mut source = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        let name = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{name} needs a value"))?;
        if !names.contains(&name) {
            return Err(format!("unexpected argument {name:?}"));
        }
        match name {
            "--source" => source = Some(value.clone()),
            "--output" => output = Some(value.clone()),
            _ => {}
        }
        index += 2;
    }
    Ok((source, output))
}

/// Reads only `component.json` from a `.tvpkg` archive. This is deliberately
/// not a package parser; it is build-time metadata for ordering.
fn id_from_package(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut manifest = String::new();
    archive
        .by_name("component.json")
        .map_err(|error| error.to_string())?
        .read_to_string(&mut manifest)
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(&manifest).map_err(|error| error.to_string())?;
    value["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} has no component id", path.display()))
}
