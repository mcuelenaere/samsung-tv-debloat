//! `tv`: ordinary host commands plus Git-style external subcommands.
//!
//! Built-ins have precedence. Anything else is looked up as `tv-<name>` next
//! to the current executable and then on `PATH`, so `tv root` runs `tv-root`
//! without the main CLI depending on the privileged rooting workspace.

mod dispatch;
mod sdb;
mod ssh_key;

use std::ffi::OsString;
use std::process::ExitCode;

const USAGE: &str = "\
usage: tv <command> [arguments...]

  sdb ...                         native SDB client commands
  ssh-key add <tv> <public-key>   push a key for on-TV approval
  package ...                     run the tv-pkg helper (host or on-TV build)
  root ...                        run the tv-root helper (privileged setup)
  setup ...                       run the tv-setup helper (one-shot setup)

Unrecognized commands run `tv-<command>` next to this executable, then from
PATH. Use `tv sdb --help` for the SDB client's own options.";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match dispatch::run(args) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("tv: {error}");
            ExitCode::from(1)
        }
    }
}
