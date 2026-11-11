//! `tv-sdb`: talk to a Samsung TV's sdbd without Tizen Studio.
//!
//! Each invocation opens its own connection; `connect` only checks that the
//! TV answers, so setup scripts written for Samsung's `sdb` keep working
//! with `SDB=/path/to/tv-sdb`.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;
use tv_sdb::{Device, Options};

const USAGE: &str = "\
usage: tv-sdb [-s <tv>[:port]] [--timeout <seconds>] [--verbose] <command>

  connect <tv>[:port]             check the TV answers and print its banner
  capability                      print sdbd's capability list
  shell <command...>              run an sdbd shell verb, e.g. `0 vd_applist`
  push <local-file> <remote-path>
  pull <remote-file> <local-file>
  install <package.tpk|package.wgt>
  uninstall <package-id>
  launch <app-id>

The port defaults to 26101.";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut target = None;
    let mut options = Options::default();
    let mut rest = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-s" if rest.is_empty() => target = args.next(),
            "--timeout" if rest.is_empty() => match args.next().and_then(|t| t.parse::<f64>().ok())
            {
                Some(secs) if secs > 0.0 => options.timeout = Duration::from_secs_f64(secs),
                _ => return usage("--timeout needs a positive number of seconds"),
            },
            "-v" | "--verbose" if rest.is_empty() => options.verbose = true,
            "-h" | "--help" if rest.is_empty() => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => rest.push(arg),
        }
    }
    let Some((command, params)) = rest.split_first() else {
        return usage("missing command");
    };

    if command == "connect" {
        let [tv] = params else {
            return usage("connect needs <tv>[:port]");
        };
        return report(Device::connect(tv, &options).map(|d| {
            println!(
                "connected to {}: {}",
                tv_sdb::connection::with_default_port(tv),
                d.banner
            );
        }));
    }

    let arity = match command.as_str() {
        "capability" => params.is_empty(),
        "shell" => !params.is_empty(),
        "push" | "pull" => params.len() == 2,
        "install" | "uninstall" | "launch" => params.len() == 1,
        _ => return usage(&format!("unknown command {command:?}")),
    };
    if !arity {
        return usage(&format!("wrong arguments for {command}"));
    }
    let Some(target) = target else {
        return usage("no TV selected; pass -s <tv>[:port]");
    };
    report(Device::connect(&target, &options).and_then(|mut tv| run(&mut tv, command, params)))
}

fn run(tv: &mut Device, command: &str, params: &[String]) -> io::Result<()> {
    let stdout = &mut io::stdout();
    match command {
        "capability" => println!("{}", tv.capabilities()?.trim_end()),
        "shell" => {
            tv.shell(&params.join(" "), stdout)?;
        }
        "push" => {
            let remote = tv.push(Path::new(&params[0]), &params[1])?;
            eprintln!("pushed {} -> {remote}", params[0]);
        }
        "pull" => {
            let bytes = tv.pull(&params[0], Path::new(&params[1]))?;
            eprintln!("pulled {} -> {} ({bytes} bytes)", params[0], params[1]);
        }
        "install" => tv.install(Path::new(&params[0]), stdout)?,
        "uninstall" => tv.uninstall(&params[0], stdout)?,
        "launch" => tv.launch(&params[0], stdout)?,
        _ => unreachable!(),
    }
    stdout.flush()
}

fn report(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // stdout closed early (`tv-sdb shell ... | head`); channel errors use other kinds.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tv-sdb: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage(error: &str) -> ExitCode {
    eprintln!("tv-sdb: {error}\n\n{USAGE}");
    ExitCode::from(2)
}
