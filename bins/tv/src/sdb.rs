//! `tv sdb`: the ordinary SDB commands, using the native `tv-sdb` client.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;
use tv_sdb::{Device, Options};

const USAGE: &str = "\
usage: tv sdb [-s <tv>[:port]] [--timeout <seconds>] [--verbose] <command>

  connect <tv>[:port]             check the TV answers and print its banner
  capability                      print sdbd's capability list
  shell <command...>              run an sdbd shell verb, e.g. `0 vd_applist`
  push <local-file> <remote-path>
  pull <remote-file> <local-file>
  install <package.tpk|package.wgt>
  uninstall <package-id>
  launch <app-id>";

fn strings(args: &[OsString]) -> io::Result<Vec<String>> {
    args.iter()
        .map(|arg| {
            arg.to_str().map(str::to_owned).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "argument is not valid UTF-8")
            })
        })
        .collect()
}

pub fn run(args: &[OsString]) -> io::Result<u8> {
    let args = strings(args)?;
    let mut target = None;
    let mut options = Options::default();
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-s" if rest.is_empty() => {
                index += 1;
                target = args.get(index).cloned();
                if target.is_none() {
                    return usage("--s needs a TV address");
                }
            }
            "--timeout" if rest.is_empty() => {
                index += 1;
                match args.get(index).and_then(|value| value.parse::<f64>().ok()) {
                    Some(seconds) if seconds > 0.0 => {
                        options.timeout = Duration::from_secs_f64(seconds)
                    }
                    _ => return usage("--timeout needs a positive number of seconds"),
                }
            }
            "-v" | "--verbose" if rest.is_empty() => options.verbose = true,
            "-h" | "--help" if rest.is_empty() => {
                println!("{USAGE}");
                return Ok(0);
            }
            _ => rest.push(args[index].clone()),
        }
        index += 1;
    }
    let Some((command, params)) = rest.split_first() else {
        return usage("missing command");
    };

    if command == "connect" {
        let [tv] = params else {
            return usage("connect needs <tv>[:port]");
        };
        return report(Device::connect(tv, &options).map(|device| {
            println!(
                "connected to {}: {}",
                tv_sdb::connection::with_default_port(tv),
                device.banner
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
    report(
        Device::connect(&target, &options)
            .and_then(|mut device| run_command(&mut device, command, params)),
    )
}

fn run_command(device: &mut Device, command: &str, params: &[String]) -> io::Result<()> {
    let stdout = &mut io::stdout();
    match command {
        "capability" => println!("{}", device.capabilities()?.trim_end()),
        "shell" => {
            device.shell(&params.join(" "), stdout)?;
        }
        "push" => {
            let remote = device.push(Path::new(&params[0]), &params[1])?;
            eprintln!("pushed {} -> {remote}", params[0]);
        }
        "pull" => {
            let bytes = device.pull(&params[0], Path::new(&params[1]))?;
            eprintln!("pulled {} -> {} ({bytes} bytes)", params[0], params[1]);
        }
        "install" => device.install(Path::new(&params[0]), stdout)?,
        "uninstall" => device.uninstall(&params[0], stdout)?,
        "launch" => device.launch(&params[0], stdout)?,
        _ => unreachable!(),
    }
    stdout.flush()
}

fn report(result: io::Result<()>) -> io::Result<u8> {
    match result {
        Ok(()) => Ok(0),
        // stdout closed early (`tv sdb shell ... | head`).
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(0),
        Err(error) => Err(error),
    }
}

fn usage(error: &str) -> io::Result<u8> {
    eprintln!("tv sdb: {error}\n\n{USAGE}");
    Ok(2)
}
