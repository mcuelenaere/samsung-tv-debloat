//! `tv-setup`: one-shot suite installation; reached as `tv setup`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use tv_root::setup::{self, SetupOptions};
use tv_root::transport::SdbTransport;
use tv_sdb::Options;

const USAGE: &str = "\
usage: tv-setup [options] <tv>

  --bundle <dir>       verified setup bundle (default: $TV_SETUP_BUNDLE or ./tv-setup-bundle)
  --key <file>         initial SSH public key to install as setup input
  --adopt-existing     one-time migration aid passed to tv-pkg
  --timeout <seconds>  overall driver deadline (default 180)
  --verbose            log SDB frame headers to stderr

The host never opens SSH and never keeps a root process alive.";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tv-setup: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse()?;
    let mut transport = SdbTransport::new(&args.target, args.options);
    let mut options = SetupOptions::new(&args.bundle);
    options.key = args.key;
    options.adopt_existing = args.adopt_existing;
    options.timeout = args.timeout;
    let outcome = setup::run(&mut transport, &options).map_err(|error| error.to_string())?;
    println!("{}", outcome.assessment.render());
    println!("bundle_sha256={}", outcome.bundle_sha256);
    println!("runtime={}", outcome.runtime.as_str());
    for package in &outcome.packages {
        println!("component={package}");
    }
    println!("setup=ok");
    Ok(())
}

struct Args {
    target: String,
    bundle: PathBuf,
    key: Option<PathBuf>,
    adopt_existing: bool,
    timeout: Duration,
    options: Options,
}

fn parse() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut target = None;
    let mut bundle = std::env::var_os("TV_SETUP_BUNDLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tv-setup-bundle"));
    let mut key = None;
    let mut adopt_existing = false;
    let mut timeout = Duration::from_secs(180);
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--bundle" => bundle = PathBuf::from(args.next().ok_or("--bundle needs a value")?),
            "--key" => key = Some(PathBuf::from(args.next().ok_or("--key needs a value")?)),
            "--adopt-existing" => adopt_existing = true,
            "--timeout" => {
                let seconds = args
                    .next()
                    .ok_or("--timeout needs a value")?
                    .parse::<f64>()
                    .map_err(|_| "--timeout needs seconds")?;
                if seconds <= 0.0 {
                    return Err("--timeout must be positive".into());
                }
                timeout = Duration::from_secs_f64(seconds);
            }
            "--verbose" => options.verbose = true,
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => {
                if target.is_some() {
                    return Err(format!("unexpected argument {other:?}"));
                }
                target = Some(other.to_owned());
            }
        }
    }
    Ok(Args {
        target: target.ok_or("missing <tv>")?,
        bundle,
        key,
        adopt_existing,
        timeout,
        options,
    })
}
