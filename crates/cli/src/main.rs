//! `switchyard`: the one binary for every Switchyard tool.
//!
//! Bare, from a terminal, it opens the tabs and agents picker (see
//! switchyard-tabs). The other user-facing subcommands are `new` (the
//! new-session picker, switchyard-spawn), `link` and `unlink` (link.rs),
//! `usage` and `server` (switchyard-server).
//!
//! `float` and `task` are plumbing for the Rex config's Lua, core, and other
//! hosts. They map onto the tools' original flags, so they stay out of the
//! top-level help.
//!
//! Run as `rex-tab-picker` or `rex-spawn` (the package's symlinks), it takes
//! those tools' original arguments unchanged. Hosts on an older build still
//! call those names, and so do floats and remote commands; card #1082
//! removes the old names once every host runs this binary.

mod link;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

const USAGE: &str = "usage: switchyard [--view all|archive|usage]
       switchyard new [--session]
       switchyard link [REF...]
       switchyard unlink REF...
       switchyard usage [--json|--local]
       switchyard server [serve|snapshot|subscribe|pastes [--checked]]

Bare, from a terminal, switchyard opens the picker for Rex tabs, agent
tasks and the archive, which fetches its own data and acts on Enter.
new opens the new-session picker: agent work by default, or a plain
session with --session. link and unlink record the work items the
current branch is for (see switchyard link --help). usage opens the
picker's Usage view; --json prints accounting across configured hosts,
--local this host only. server runs the data server (see switchyard
server --help).";

fn usage_error() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn done(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("switchyard: {e}");
            ExitCode::FAILURE
        }
    }
}

fn tabs(args: &[&str]) -> ExitCode {
    done(switchyard_tabs::main(args.iter().map(|s| s.to_string()).collect()))
}

fn spawn(args: &[&str]) -> ExitCode {
    done(switchyard_spawn::main(args.iter().map(|s| s.to_string()).collect()))
}

fn server(args: &[&str]) -> ExitCode {
    switchyard_server::main(args.iter().map(|s| s.to_string()).collect());
    ExitCode::SUCCESS
}

/// The interactive pickers need a terminal. Without one, the tabs picker
/// would wait on stdin for rows.
fn interactive(f: impl FnOnce() -> ExitCode) -> ExitCode {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        f()
    } else {
        eprintln!("switchyard: the picker needs a terminal\n");
        usage_error()
    }
}

/// `switchyard task ...`: what rex-spawn's flags do, named.
fn task(args: &[&str]) -> ExitCode {
    let flag = match args.first() {
        Some(&"launch") => "--launch",
        Some(&"open") => "--open",
        Some(&"run") => "--run",
        Some(&"act") => "--act",
        Some(&"list") => "--tasks",
        Some(&"session-of") => "--session-of",
        _ => return usage_error(),
    };
    let mut argv = vec![flag];
    argv.extend_from_slice(&args[1..]);
    spawn(&argv)
}

fn main() -> ExitCode {
    let argv0 = std::env::args().next().unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    // The old names, from hosts and floats that still call them.
    match argv0.rsplit('/').next().unwrap_or("") {
        "rex-tab-picker" => return tabs(&a),
        "rex-spawn" => return spawn(&a),
        _ => {}
    }
    match a.as_slice() {
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        [] => interactive(|| tabs(&[])),
        ["--view", view @ ("all" | "archive" | "usage")] => interactive(|| tabs(&["--view", view])),
        ["new"] => interactive(|| spawn(&[])),
        ["new", "--session"] => interactive(|| spawn(&["--kind", "session"])),
        ["new", "--agent"] => interactive(|| spawn(&["--kind", "agent"])),
        ["link" | "unlink", ..] => link::main(args),
        ["usage"] => interactive(|| tabs(&["--view", "usage"])),
        ["usage", "--json"] => tabs(&["--usage-json"]),
        ["usage", "--local"] => tabs(&["--usage-local"]),
        ["server", rest @ ..] => server(rest),
        // Plumbing: the floats' modes, and the task work behind them.
        ["float", "tabs", rest @ ..] => tabs(rest),
        ["float", "new", rest @ ..] => spawn(rest),
        ["task", rest @ ..] => task(rest),
        _ => usage_error(),
    }
}
