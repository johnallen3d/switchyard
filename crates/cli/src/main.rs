//! `switchyard`: the one binary for every Switchyard tool.
//!
//! Bare, from a terminal, it opens the tabs and agents picker (see
//! switchyard-tabs). The other user-facing subcommands are `new` (the
//! new-session picker, switchyard-spawn), `link` and `unlink` (link.rs),
//! `usage` and `server` (switchyard-server).
//!
//! `float new`, `task` (task.rs) and `transcript --local` are plumbing for
//! the Rex config's Lua, core, and other hosts, so they stay out of the
//! top-level help.

mod link;
mod task;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use switchyard_spawn::Kind;
use switchyard_tabs::View;

const USAGE: &str = "usage: switchyard [--view inbox|usage|archive] [--current SESSION/WINDOW] [--quick]
       switchyard new [--session]
       switchyard link [REF...]
       switchyard unlink REF...
       switchyard usage [--json|--local]
       switchyard server [serve|snapshot|subscribe]

Bare, from a terminal, switchyard opens the picker for Rex tabs, agent
tasks and the archive, which fetches its own data and acts on Enter,
ctrl-b (open the task's work item), ctrl-t (pin) and
ctrl-s (snooze), staying open until its tab is closed. ctrl-h and ctrl-l
cycle the views: Inbox, Usage, Archive. --current highlights that
tab (cmd+i and cmd+k pass the tab they were pressed on). --quick is
cmd+k's float: Inbox's list alone, where Enter jumps to the highlighted
tab and quits, and Escape quits. new opens the
new-session picker: agent work by default, or a plain session with
--session. link and unlink record the work items the current branch is
for (see switchyard link --help). usage opens the picker's Usage view;
--json prints sanitized accounting across configured hosts, --local this
host only. Configure usage.sessions, usage.quotas and usage.rates in the
Home Manager settings for account labels and prices. server runs the data
server (see switchyard server --help).";

const FLOAT_USAGE: &str = "usage: switchyard float new [--kind agent|session] --home LABEL FOLDERS

The new-session picker in the Rex config's float. It gets the config
host's label and zoxide folders, one per line, and reports the highlighted
row in the terminal title for the config's mode to act on.";

fn usage_error(usage: &str) -> ExitCode {
    eprintln!("{usage}");
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

/// The interactive pickers need a terminal.
/// Bare: `[--view VIEW] [--current SESSION/WINDOW] [--quick]`, in any
/// order.
fn picker(mut args: &[&str]) -> ExitCode {
    let (mut view, mut current, mut quick) = (View::Inbox, None, false);
    loop {
        args = match args {
            [] => break,
            ["--view", v, rest @ ..] => match View::parse(v) {
                Some(v) => {
                    view = v;
                    rest
                }
                None => return usage_error(USAGE),
            },
            ["--current", c, rest @ ..] => {
                current = Some(*c);
                rest
            }
            ["--quick", rest @ ..] => {
                quick = true;
                rest
            }
            _ => return usage_error(USAGE),
        };
    }
    if quick {
        return interactive(|| switchyard_tabs::quick(current));
    }
    interactive(|| switchyard_tabs::standalone(view, current))
}

fn interactive(f: impl FnOnce() -> io::Result<()>) -> ExitCode {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        done(f())
    } else {
        eprintln!("switchyard: the picker needs a terminal\n");
        usage_error(USAGE)
    }
}

/// `float new [--kind KIND] --home LABEL FOLDERS` (FOLDERS may be empty).
fn float_new(args: &[&str]) -> ExitCode {
    let (kind, rest) = match args {
        ["--kind", kind, rest @ ..] => match Kind::parse(kind) {
            Some(kind) => (kind, rest),
            None => return usage_error(FLOAT_USAGE),
        },
        _ => (Kind::Agent, args),
    };
    match rest {
        ["--home", label, folders] => done(switchyard_spawn::float(kind, label.to_string(), folders.to_string())),
        _ => usage_error(FLOAT_USAGE),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        [] | ["--view" | "--current" | "--quick", ..] => picker(&a),
        ["new"] | ["new", "--agent"] => interactive(|| switchyard_spawn::standalone(Kind::Agent)),
        ["new", "--session"] => interactive(|| switchyard_spawn::standalone(Kind::Session)),
        ["link" | "unlink", ..] => link::main(args),
        ["usage"] => interactive(|| switchyard_tabs::standalone(View::Usage, None)),
        ["usage", "--json"] => done(switchyard_tabs::usage_json(false)),
        ["usage", "--local"] => done(switchyard_tabs::usage_json(true)),
        ["server", rest @ ..] => {
            switchyard_server::main(rest.iter().map(|s| s.to_string()).collect());
            ExitCode::SUCCESS
        }
        // Plumbing: the new-session float, and the task work behind it.
        ["float", "-h" | "--help"] => {
            println!("{FLOAT_USAGE}");
            ExitCode::SUCCESS
        }
        ["float", "new", rest @ ..] => float_new(rest),
        ["float", ..] => usage_error(FLOAT_USAGE),
        ["task", rest @ ..] => task::main(rest),
        // The highlighted agent's transcript for the picker's preview, read
        // on the agent's host by what the config host found (see core's
        // transcript).
        ["transcript", "--local", profile, harness, dir, agent_session, pid] => {
            let pid = pid.parse().unwrap_or(0);
            println!("{}", switchyard_core::transcript::local(profile, harness, dir, agent_session, pid));
            ExitCode::SUCCESS
        }
        _ => usage_error(USAGE),
    }
}
