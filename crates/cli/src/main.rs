//! `switchyard`: the one binary for every Switchyard tool.
//!
//! Bare, from a terminal, it opens the tabs and agents picker (see
//! switchyard-tabs). The other user-facing subcommands are `new` (the
//! new-session picker, switchyard-spawn), `link` and `unlink` (link.rs),
//! `usage` and `server` (switchyard-server).
//!
//! `float new`, `float feed`, `task` (task.rs), `review` (review.rs, the
//! tab picker's review float), `transcript --local` and `diff --local` are
//! plumbing for
//! the Rex config's Lua, core, and other hosts, so they stay out of the
//! top-level help.

mod link;
mod review;
mod task;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use switchyard_spawn::Kind;
use switchyard_tabs::View;

/// The build's version: the source commit's date and hash for Nix builds
/// (see flake.nix), else "dev" for a local Cargo build.
const VERSION: &str = match option_env!("SWITCHYARD_BUILD_VERSION") {
    Some(v) => v,
    None => "dev",
};

const USAGE: &str =
    "usage: switchyard [--view inbox|work|usage|archive] [--current SESSION/WINDOW] [--quick]
       switchyard new [--session]
       switchyard link [REF...]
       switchyard unlink REF...
       switchyard usage [--json|--local]
       switchyard server [serve|snapshot|subscribe]
       switchyard --version

Bare, from a terminal, switchyard opens the picker for Rex tabs, agent
tasks and the archive, which fetches its own data and acts on Enter,
ctrl-b (work item actions: open, read, comment on or close a card
or issue, mark a draft PR ready, each mutation confirmed first), ctrl-r
(pin), ctrl-t (review in tuicr) and ctrl-s (snooze), staying open until its tab is closed. cmd+1-4 on its tab pick the views: Inbox, Work (one
project's open cards or issues, or with ctrl-f its open pull requests,
to start a session from), Usage, Archive.
--current highlights that tab (cmd+i and cmd+k pass the tab they were
pressed on). --quick is cmd+k's float: Inbox's list alone, where
Enter jumps to the highlighted tab and quits, and Escape quits. With
--fed-by LABEL on a host other than LABEL, the quick float fetches nothing
and LABEL feeds it and acts on its picks (see switchyard float --help). new opens the
new-session picker: agent work by default, or a plain session with
--session. link and unlink record the work items the current branch is
for (see switchyard link --help). usage opens the picker's Usage view;
--json prints sanitized accounting across configured hosts, --local this
host only. Configure usage.sessions, usage.quotas and usage.rates in the
Home Manager settings for account labels and prices. server runs the data
server (see switchyard server --help). --version prints the build's
source commit date and hash.";

const FLOAT_USAGE: &str = "usage: switchyard float new [--kind agent|session] --home LABEL FOLDERS
       switchyard float feed SESSION BLOCK

new is the new-session picker in the Rex config's float. It gets the config
host's label and zoxide folders, one per line, and reports the highlighted
row in the terminal title for the config's mode to act on.

feed runs on the config host for a cmd+k quick jump float on another host
(`switchyard --quick --fed-by LABEL` there, where LABEL is the config
host's): it pastes the config host's tabs and tasks into the float and acts
on the pick it reports, until the float closes.";

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
/// Bare: `[--view VIEW] [--current SESSION/WINDOW] [--quick [--fed-by
/// LABEL]]`, in any order.
fn picker(mut args: &[&str]) -> ExitCode {
    let (mut view, mut current, mut quick, mut fed_by) = (View::Inbox, None, false, None);
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
            ["--fed-by", host, rest @ ..] => {
                fed_by = Some(*host);
                rest
            }
            _ => return usage_error(USAGE),
        };
    }
    if quick {
        return interactive(|| switchyard_tabs::quick(current, fed_by));
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
        ["--home", label, folders] => done(switchyard_spawn::float(
            kind,
            label.to_string(),
            folders.to_string(),
        )),
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
        ["-V" | "--version" | "version"] => {
            println!("switchyard {VERSION}");
            ExitCode::SUCCESS
        }
        [] | ["--view" | "--current" | "--quick" | "--fed-by", ..] => picker(&a),
        ["new"] | ["new", "--agent"] => interactive(|| switchyard_spawn::standalone(Kind::Agent)),
        ["new", "--session"] => interactive(|| switchyard_spawn::standalone(Kind::Session)),
        ["link" | "unlink", ..] => link::main(&args),
        ["usage"] => interactive(|| switchyard_tabs::standalone(View::Usage, None)),
        ["usage", "--json"] => done(switchyard_tabs::usage_json(false)),
        ["usage", "--local"] => done(switchyard_tabs::usage_json(true)),
        ["server", rest @ ..] => {
            switchyard_server::main(
                &rest
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>(),
            );
            ExitCode::SUCCESS
        }
        // Plumbing: the new-session float, and the task work behind it.
        ["float", "-h" | "--help"] => {
            println!("{FLOAT_USAGE}");
            ExitCode::SUCCESS
        }
        ["float", "new", rest @ ..] => float_new(rest),
        ["float", "feed", session, block] => done(switchyard_tabs::feed_float(session, block)),
        ["float", ..] => usage_error(FLOAT_USAGE),
        ["task", rest @ ..] => task::main(rest),
        ["review", "-h" | "--help"] => {
            println!("{}", review::USAGE);
            ExitCode::SUCCESS
        }
        ["review", rest @ ..] => review::main(rest),
        // The highlighted agent's transcript for the picker's preview, read
        // on the agent's host by what the config host found (see core's
        // transcript).
        [
            "transcript",
            "--local",
            profile,
            harness,
            dir,
            agent_session,
            pid,
        ] => {
            let pid = pid.parse().unwrap_or(0);
            println!(
                "{}",
                switchyard_core::transcript::local(profile, harness, dir, agent_session, pid)
            );
            ExitCode::SUCCESS
        }
        // What the highlighted task has changed, for the picker's preview,
        // read on the agent's host (see core's changes).
        ["diff", "--local", dir] => {
            println!("{}", switchyard_core::changes::local(dir));
            ExitCode::SUCCESS
        }
        _ => usage_error(USAGE),
    }
}
