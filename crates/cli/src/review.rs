//! `switchyard review ...`: what the tab picker's review float runs (see
//! core's `review`). `task` reviews the changes of the agent task in a tab
//! in tuicr and, once John says so, sends the review to the agent as if
//! he typed it; `pr` reviews a pull request in tuicr, which submits it to
//! GitHub itself. The float closes when this exits, so a failure waits
//! for Enter before it does.

use std::io::{self, BufRead, Write};
use std::process::{Command, ExitCode};

use switchyard_core::actions;
use switchyard_core::changes::task_dir;
use switchyard_core::review::{self, Outcome};
use switchyard_core::rex::home;
use switchyard_core::sessions::work_env;

pub const USAGE: &str = "usage: switchyard review task TARGET SESSION_ID WINDOW_ID
       switchyard review pr PROFILE PR

task reviews the changes of the agent task in the tab (TARGET is - for
this host, else its hex-encoded Rex label) in tuicr, in its checkout on
its host: the branch's commits since its base and the working tree. y in
tuicr ends the review; Enter then sends it to the agent as if typed in its
tab, and clears the comments from tuicr's session. pr reviews the pull
request PR (owner/repo#N or its URL) in tuicr, with gh under PROFILE's
account (work loads amfaro's env); tuicr's :submit sends the review.";

pub fn main(args: &[&str]) -> ExitCode {
    let result = match args {
        ["task", target, session, window] => task(target, session, window),
        ["pr", profile, reference] => pr(profile, reference),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\n{e}");
            wait_for_enter();
            ExitCode::FAILURE
        }
    }
}

fn task(target: &str, session: &str, window: &str) -> Result<(), String> {
    let host = review::target_host(target);
    println!("Opening the review on {host}…");
    let checkout = task_dir(&host, session, window)?;
    let folder = review::prepare(&host)?;
    let ran = review::run(&host, &folder, &checkout);
    let outcome = review::collect(&host, &folder)?;
    let status = ran?;
    match outcome.status {
        Some(0) => {}
        Some(_) => return Err(failure("tuicr failed", &outcome)),
        None => return Err(format!("the review didn't run on {host} ({status})")),
    }
    if outcome.review.is_empty() {
        // Quit without a review: nothing to send.
        return Ok(());
    }
    send(&host, session, window, &outcome)
}

/// Ask, then send the review to the agent and clear tuicr's comments; a
/// review that can't be sent is kept in a file.
fn send(host: &str, session: &str, window: &str, outcome: &Outcome) -> Result<(), String> {
    clear_screen();
    println!("{}\n", outcome.review);
    if let Some(summary) = outcome.summary() {
        println!("tuicr: {summary}");
    }
    print!("Enter sends this review to the agent; n and Enter keeps it in tuicr: ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    let _ = io::stdin().lock().read_line(&mut answer);
    if answer.trim().eq_ignore_ascii_case("n") {
        return Ok(());
    }
    match actions::message(host, session, window, &outcome.review) {
        Ok(sent) => {
            if let Some(slug) = outcome.session()
                && let Err(e) = review::clear(host, slug)
            {
                return Err(format!("{sent}, but {e}"));
            }
            Ok(())
        }
        Err(e) => Err(match keep(&outcome.review) {
            Ok(path) => format!("{e}\nThe review is in {path}"),
            Err(keep_err) => format!("{e}\n{keep_err}"),
        }),
    }
}

/// Write an unsent review to Switchyard's state folder, and give its path.
fn keep(review: &str) -> Result<String, String> {
    let dir = std::env::var_os("XDG_STATE_HOME")
        .map_or_else(|| home().join(".local/state"), std::path::PathBuf::from)
        .join("switchyard/reviews");
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't keep the review: {e}"))?;
    let path = dir.join(format!("{}.md", switchyard_core::rex::now()));
    std::fs::write(&path, review).map_err(|e| format!("can't keep the review: {e}"))?;
    Ok(path.display().to_string())
}

fn pr(profile: &str, reference: &str) -> Result<(), String> {
    let env = if profile == "work" {
        work_env()?
    } else {
        Vec::new()
    };
    let status = Command::new("tuicr")
        .args(["pr", reference, "--no-update-check"])
        .envs(env)
        .current_dir(home())
        .status()
        .map_err(|e| format!("can't run tuicr: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tuicr failed ({status})"))
    }
}

fn failure(what: &str, outcome: &Outcome) -> String {
    let why: Vec<&str> = outcome
        .stderr
        .lines()
        .filter(|l| !l.starts_with("tuicr-session:") && !l.starts_with("tuicr-summary:"))
        .collect();
    if why.is_empty() {
        what.to_string()
    } else {
        format!("{what}: {}", why.join("\n"))
    }
}

fn clear_screen() {
    print!("\x1b[2J\x1b[H");
}

fn wait_for_enter() {
    eprint!("Press Enter to close.");
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
}
