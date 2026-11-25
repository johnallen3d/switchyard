//! `switchyard task ...`: the task work behind the floats, for the Rex
//! config's Lua on the config host and for core on other hosts. See
//! switchyard-spawn's crate docs for what each does.

use std::path::Path;
use std::process::ExitCode;

use switchyard_core::actions;
use switchyard_core::atuin;
use switchyard_core::folders;
use switchyard_core::launch::launch;
use switchyard_core::rex::log;
use switchyard_core::sessions::newest_session_here;
use switchyard_core::tasks::{live_tasks, reconcile_tasks};

pub const USAGE: &str = "usage: switchyard task open HOST DIR
       switchyard task launch HOST PATH NAME HARNESS [PROMPT_REF [WORK_ITEM]]
       switchyard task run PROFILE PATH NAME HARNESS CLONE_URL [AGENT_SESSION new|resume [PROMPT]]
       switchyard task list [--live]
       switchyard task act archive|force-archive|resume|link|pin HOST SESSION_ID WINDOW_ID
       switchyard task act snooze HOST SESSION_ID WINDOW_ID UNTIL
       switchyard task act message HOST SESSION_ID WINDOW_ID PROMPT_REF
       switchyard task act switch HOST SESSION_ID WINDOW_ID HARNESS
       switchyard task session-of PROFILE HARNESS WORKTREE
       switchyard task convert PROFILE SOURCE TARGET WORKTREE SESSION_ID

open finds or creates the session for DIR on HOST (auto: the catalog
project's least busy host) and prints HOST<TAB>SESSION_ID.
launch collects the prompt (an empty PROMPT_REF is none), leads it with
WORK_ITEM (a work project's /agent-kit:issue-implement, otherwise the
item's URL), opens the work's tab on HOST (auto: the catalog project's
least busy host; Git folders use their listed host and primary
checkout), records the task, and prints HOST<TAB>SESSION_ID<TAB>WINDOW_ID.
HARNESS is claude or pi, or HARNESS:MODEL to start it on MODEL (launch
takes the models the new-agent picker offers). run sets up the worktree
and runs the harness in the current tab,
starting AGENT_SESSION with PROMPT as its first message or resuming it.
list checks recorded tasks against their hosts and prints them as JSON
with live agent status; with --live it only adds the status, without
checking worktrees or ending tasks. act archives the task in a
tab (force-archive also throws away uncommitted changes), resumes an
ended task's agent session in a new tab and prints it as launch does,
switches a live or ended task to another harness in the same worktree via
Atuin Threads and prints HOST<TAB>SESSION_ID<TAB>WINDOW_ID (switch),
prints the URL of the task's main work item (link), pins or unpins an
open task in the inbox (pin), snoozes it until UNTIL, Unix seconds,
with 0 waking it (snooze), or sends the agent in the tab the text of the
prompt file PROMPT_REF as if typed there (message; refused while the
agent waits on a prompt). session-of prints the newest agent session
for a worktree on this host. convert converts a captured session on this
host into another harness via Atuin Threads and prints the new session ID.";

fn usage_error() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn out(result: Result<String, String>) -> ExitCode {
    match result {
        Ok(s) => {
            print!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("switchyard: {e}");
            ExitCode::FAILURE
        }
    }
}

pub fn main(args: &[&str]) -> ExitCode {
    match args {
        ["-h" | "--help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["open", host, dir] => out(folders::open_on(host, dir).map(|(h, s)| format!("{h}\t{s}"))),
        // An empty PROMPT_REF stands for none when a WORK_ITEM follows it.
        ["launch", host, path, name, harness, rest @ ..] if rest.len() <= 2 => out(launch(
            host,
            path,
            name,
            harness,
            rest.first().copied().filter(|r| !r.is_empty()),
            rest.get(1).copied(),
            None,
        )
        .map(|(host, session, window)| format!("{host}\t{session}\t{window}"))),
        ["run", profile, path, name, harness, clone_url] => {
            switchyard_spawn::run(profile, path, name, harness, clone_url, None, "")
        }
        [
            "run",
            profile,
            path,
            name,
            harness,
            clone_url,
            session,
            mode @ ("new" | "resume"),
            prompt @ ..,
        ] if prompt.len() <= 1 => switchyard_spawn::run(
            profile,
            path,
            name,
            harness,
            clone_url,
            Some((session, mode)),
            prompt.first().copied().unwrap_or(""),
        ),
        ["list"] => out(reconcile_tasks().map(|t| format!("{}\n", serde_json::Value::from(t)))),
        ["list", "--live"] => {
            out(live_tasks().map(|t| format!("{}\n", serde_json::Value::from(t))))
        }
        ["act", action, host, session, window, rest @ ..] if rest.len() <= 1 => {
            let result = match (*action, rest) {
                ("snooze", [until]) => match until.parse() {
                    Ok(until) => actions::snooze(window, until),
                    Err(_) => Err(format!("snooze: UNTIL must be Unix seconds, not {until}")),
                },
                ("message", [prompt_ref]) => {
                    actions::message_ref(host, session, window, prompt_ref)
                }
                ("message", []) => Err("message takes a PROMPT_REF".into()),
                ("switch", [harness]) => actions::switch_harness(host, session, window, harness),
                ("switch", []) => Err("switch takes a HARNESS[:MODEL]".into()),
                (_, []) => actions::act(action, host, session, window),
                _ => Err(format!("{action} takes no extra argument")),
            };
            match &result {
                Ok(msg) => log(msg),
                Err(e) => log(&format!("{action} failed: {e}")),
            }
            out(result.map(|msg| format!("{msg}\n")))
        }
        ["session-of", profile, harness, worktree] => {
            if let Some(id) = newest_session_here(profile, harness, worktree) {
                println!("{id}");
            }
            ExitCode::SUCCESS
        }
        ["convert", profile, source, target, worktree, session_id] => {
            out(
                atuin::convert_here(profile, source, target, Path::new(worktree), session_id)
                    .map(|id| format!("{id}\n")),
            )
        }
        _ => usage_error(),
    }
}
