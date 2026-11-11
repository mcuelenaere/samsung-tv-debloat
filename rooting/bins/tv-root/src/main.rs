//! `tv-root`: one-shot FDET root acquisition; reached as `tv root`.
//!
//! There is no interactive shell, agent, discovery or host SSH. Each command
//! stages the managed payload, executes one root command and cleans up.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use tv_root::compatibility::QN90B;
use tv_root::fdet::{self, PayloadSet, RootEvidence, Runtime};
use tv_root::inject;
use tv_root::transport::{SdbTransport, Transport};
use tv_sdb::Options;

const USAGE: &str = "\
usage: tv-root [options] <tv> --command <command>
       tv-root [options] <tv> --check

  --command <command>       one command to run as UID 0 (required to execute)
  --check                   print the compatibility fingerprint only
  --payload-dir <dir>       modern .NET 6 payload directory
  --legacy-payload-dir <d>  optional .NET Core 3.1 payload directory
  --timeout <seconds>       overall command deadline (default 60)
  --verbose                 log SDB frame headers to stderr

The payload directory must contain FdetProbe.dll and
FdetProbe.runtimeconfig.json. TV_FDET_PAYLOAD supplies the default path.";

struct Args {
    target: String,
    command: Option<String>,
    check: bool,
    payload: PathBuf,
    legacy: Option<PathBuf>,
    timeout: Duration,
    options: Options,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("tv-root: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<u8, String> {
    let args = parse()?;
    let mut transport = SdbTransport::new(&args.target, args.options.clone());
    transport
        .require_shell_injection()
        .map_err(|error| error.to_string())?;

    let capture = transport
        .capture(&QN90B.probe_command(), Duration::from_secs(30))
        .map_err(|error| error.to_string())?;
    let (body, _) = inject::validate_exit(&capture, &capture.marker_token)
        .map_err(|error| error.to_string())?;
    if body.trim().is_empty() {
        return Err("preflight callback connected but returned no output".into());
    }
    let assessment = QN90B.assess(&body);
    assessment
        .require_compatible()
        .map_err(|error| error.to_string())?;
    if args.check {
        println!("{}", assessment.render());
        return Ok(0);
    }

    let runtime = select_runtime(&mut transport)?;
    let payloads = PayloadSet::new(&args.payload, args.legacy.clone());
    let (dll, config) = payloads.files(runtime).map_err(|error| error.to_string())?;
    let remote = format!(
        "/home/owner/share/tmp/sdk_tools/tv-root-{}",
        inject::random_hex(8).map_err(|error| error.to_string())?
    );
    transport
        .inject(&format!("/bin/mkdir -p {}", inject::shell_quote(&remote)))
        .map_err(|error| error.to_string())?;
    let result = (|| -> Result<String, String> {
        transport
            .push(&dll, &format!("{remote}/FdetProbe.dll"))
            .map_err(|error| error.to_string())?;
        transport
            .push(&config, &format!("{remote}/FdetProbe.runtimeconfig.json"))
            .map_err(|error| error.to_string())?;
        let invocation = fdet::invocation(
            &format!("{remote}/FdetProbe.dll"),
            args.command.as_deref().unwrap_or("id"),
        )
        .map_err(|error| error.to_string())?;
        let capture = transport
            .capture(&invocation, args.timeout)
            .map_err(|error| error.to_string())?;
        let (body, status) = inject::validate_exit(&capture, &capture.marker_token)
            .map_err(|error| error.to_string())?;
        if status != 0 {
            let tail: String = body
                .chars()
                .rev()
                .take(2000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            return Err(format!("remote command exited {status}: {tail}"));
        }
        RootEvidence::validate(&body).map_err(|error| error.to_string())?;
        Ok(body)
    })();
    let _ = transport.inject(&format!("/bin/rm -rf -- {}", inject::shell_quote(&remote)));
    let body = result?;
    println!("{body}");
    Ok(0)
}

fn select_runtime(transport: &mut SdbTransport) -> Result<Runtime, String> {
    let capture = transport
        .capture(fdet::ACTIVE_RUNTIME_PROBE, Duration::from_secs(15))
        .map_err(|error| error.to_string())?;
    let (body, _) = inject::validate_exit(&capture, &capture.marker_token)
        .map_err(|error| error.to_string())?;
    Ok(fdet::runtime_from_output(&body))
}

fn parse() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut target = None;
    let mut command = None;
    let mut check = false;
    let mut payload = std::env::var_os("TV_FDET_PAYLOAD")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rooting/payloads/fdet/out/modern"));
    let mut legacy = std::env::var_os("TV_FDET_LEGACY_PAYLOAD")
        .map(PathBuf::from)
        .or_else(|| {
            let path = PathBuf::from("rooting/payloads/fdet/out/legacy");
            path.is_dir().then_some(path)
        });
    let mut timeout = Duration::from_secs(60);
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--command" => {
                command = Some(args.next().ok_or("--command needs a value")?);
            }
            "--check" => check = true,
            "--payload-dir" => {
                payload = PathBuf::from(args.next().ok_or("--payload-dir needs a value")?);
            }
            "--legacy-payload-dir" => {
                legacy = Some(PathBuf::from(
                    args.next().ok_or("--legacy-payload-dir needs a value")?,
                ));
            }
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
    let target = target.ok_or("missing <tv>")?;
    if !check && command.is_none() {
        return Err("give --command <command> or --check".into());
    }
    Ok(Args {
        target,
        command,
        check,
        payload,
        legacy,
        timeout,
        options,
    })
}
