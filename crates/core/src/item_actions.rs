//! Actions on a task's linked work items, for the tab picker: comment on
//! a Fizzy card, close one, and mark a draft GitHub PR ready for review.
//! (Opening an item in the browser needs only its URL; editing items stays
//! in the CLIs.)
//!
//! Each runs here, where the picker runs, under the item's own account:
//! Fizzy through `fizzy` with `FIZZY_PROFILE` set to the card's account
//! (the Fizzy CLI names its profiles by account), GitHub through `gh`
//! under the task's profile, as `links` asks GitHub (work gets amfaro's
//! mise env). Neither switches profiles nor touches credentials. A failure
//! is the CLI's own message, for the picker's footer.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::rex::home;
use crate::sessions::work_env;

/// How long one fizzy or gh call may take before it is given up on.
const TIMEOUT: Duration = Duration::from_secs(20);

/// A mutating action on a work item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Comment,
    Close,
    Ready,
}

impl Action {
    /// What the action does, for confirmations: "close card 1041".
    pub fn verb(self) -> &'static str {
        match self {
            Action::Comment => "comment on",
            Action::Close => "close",
            Action::Ready => "mark ready",
        }
    }
}

/// The work item an action is on, as the registry links it (see `links`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// A Fizzy card: its account (from its URL) and number.
    Card { account: String, number: String },
    /// A GitHub PR: `owner/repo`, its number, and whether it is a draft.
    Pr { repo: String, number: String, draft: bool },
}

impl Item {
    /// The item a link names, when actions can be taken on it: a Fizzy card
    /// whose URL names its account, or a GitHub PR.
    pub fn from_link(kind: &str, system: &str, reference: &str, url: &str, state: &str) -> Option<Item> {
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        match (kind, system) {
            ("card", "fizzy") => {
                let account = fizzy_account(url)?;
                digits(reference).then(|| Item::Card { account: account.into(), number: reference.into() })
            }
            ("pr", "github") => {
                let (repo, number) = reference.split_once('#')?;
                (repo.contains('/') && digits(number))
                    .then(|| Item::Pr { repo: repo.into(), number: number.into(), draft: state == "draft" })
            }
            _ => None,
        }
    }

    /// The actions this item takes: comment on and close a card; mark a
    /// draft PR ready. An open PR has none.
    pub fn actions(&self) -> Vec<Action> {
        match self {
            Item::Card { .. } => vec![Action::Comment, Action::Close],
            Item::Pr { draft: true, .. } => vec![Action::Ready],
            Item::Pr { .. } => vec![],
        }
    }

    /// How a confirmation names it: `card 1041`, `PR owner/repo#57`.
    pub fn name(&self) -> String {
        match self {
            Item::Card { number, .. } => format!("card {number}"),
            Item::Pr { repo, number, .. } => format!("PR {repo}#{number}"),
        }
    }
}

/// The account a Fizzy card URL names: `https://app.fizzy.do/ACCOUNT/cards/N`.
pub fn fizzy_account(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://app.fizzy.do/")?;
    let mut parts = rest.split('/');
    let account = parts.next().filter(|a| !a.is_empty())?;
    (parts.next() == Some("cards")).then_some(account)
}

/// A CLI call: program, arguments and extra environment.
type Plan = (&'static str, Vec<String>, Vec<(String, String)>);

/// The call an action runs. `body` is a comment's text.
fn plan(action: Action, item: &Item, body: &str) -> Result<Plan, String> {
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    match (action, item) {
        (Action::Comment, Item::Card { account, number }) => {
            if body.trim().is_empty() {
                return Err("an empty comment: nothing to post".into());
            }
            let args = s(&["comment", "create", "--card", number, "--body", body, "--json"]);
            Ok(("fizzy", args, vec![("FIZZY_PROFILE".into(), account.clone())]))
        }
        (Action::Close, Item::Card { account, number }) => {
            Ok(("fizzy", s(&["card", "close", number, "--json"]), vec![("FIZZY_PROFILE".into(), account.clone())]))
        }
        (Action::Ready, Item::Pr { repo, number, .. }) => {
            Ok(("gh", s(&["pr", "ready", number, "-R", repo]), vec![("GH_PROMPT_DISABLED".into(), "1".into())]))
        }
        (action, item) => Err(format!("can't {} {}", action.verb(), item.name())),
    }
}

/// Run `action` on `item` (a comment's text in `body`; gh under
/// `profile`); Ok is the outcome to show, Err the CLI's message.
pub fn run(action: Action, item: &Item, body: &str, profile: &str) -> Result<String, String> {
    let (program, args, env) = plan(action, item, body)?;
    let mut cmd = Command::new(program);
    cmd.args(&args).envs(env);
    // Not under a checkout, so no repo's `.fizzy.yaml` steers the CLI.
    cmd.current_dir(home());
    if program == "gh" && profile == "work" {
        cmd.envs(work_env()?);
    }
    run_bounded(&mut cmd, program, TIMEOUT)?;
    let name = item.name();
    Ok(match action {
        Action::Comment => format!("commented on {name}"),
        Action::Close => format!("closed {name}"),
        Action::Ready => format!("marked {name} ready for review"),
    })
}

/// Run `cmd` with stdin closed, giving up after `timeout`. Its stdout, or
/// on failure the reason: a Fizzy JSON error's message, else the last line
/// of stderr (or stdout), or that the program isn't on PATH.
pub(crate) fn run_bounded(cmd: &mut Command, program: &str, timeout: Duration) -> Result<String, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{program}: not found on PATH"),
        _ => format!("{program}: {e}"),
    })?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} took longer than {}s; gave up", timeout.as_secs()));
            }
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() {
        return Ok(out);
    }
    Err(failure(program, &out, &err))
}

/// Why a CLI call failed, in a line.
fn failure(program: &str, out: &str, err: &str) -> String {
    let json = serde_json::from_str::<Value>(out.trim()).ok();
    if let Some(msg) = json.as_ref().and_then(|v| v["error"].as_str()) {
        return format!("{program}: {msg}");
    }
    let last = |s: &str| s.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(String::from);
    match last(err).or_else(|| last(out)) {
        Some(line) => format!("{program}: {line}"),
        None => format!("{program} failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Item {
        Item::Card { account: "6284043".into(), number: "1041".into() }
    }

    #[test]
    fn links_become_items_with_their_actions() {
        let c = Item::from_link("card", "fizzy", "1041", "https://app.fizzy.do/6284043/cards/1041", "").unwrap();
        assert_eq!(c, card());
        assert_eq!(c.actions(), [Action::Comment, Action::Close]);
        assert_eq!(c.name(), "card 1041");
        // A card without its account in a URL can't say whose profile.
        assert_eq!(Item::from_link("card", "fizzy", "1041", "", ""), None);
        let draft = Item::from_link("pr", "github", "o/r#57", "https://github.com/o/r/pull/57", "draft").unwrap();
        assert_eq!(draft.actions(), [Action::Ready]);
        assert_eq!(draft.name(), "PR o/r#57");
        let open = Item::from_link("pr", "github", "o/r#57", "u", "open").unwrap();
        assert!(open.actions().is_empty());
        assert_eq!(Item::from_link("issue", "github", "o/r#3", "u", ""), None);
        assert_eq!(Item::from_link("commit", "github", "abc", "u", ""), None);
    }

    #[test]
    fn fizzy_account_reads_card_urls_only() {
        assert_eq!(fizzy_account("https://app.fizzy.do/6284043/cards/1159"), Some("6284043"));
        assert_eq!(fizzy_account("https://app.fizzy.do/6284043/boards/x"), None);
        assert_eq!(fizzy_account("https://github.com/o/r/pull/1"), None);
    }

    #[test]
    fn plans_run_the_clis_under_the_items_account() {
        let (program, args, env) = plan(Action::Close, &card(), "").unwrap();
        assert_eq!(program, "fizzy");
        assert_eq!(args, ["card", "close", "1041", "--json"]);
        assert_eq!(env, [("FIZZY_PROFILE".to_string(), "6284043".to_string())]);

        let (_, args, env) = plan(Action::Comment, &card(), "looks good; shipping").unwrap();
        assert_eq!(args, ["comment", "create", "--card", "1041", "--body", "looks good; shipping", "--json"]);
        assert_eq!(env[0].1, "6284043");
        assert!(plan(Action::Comment, &card(), "  \n").is_err());

        let pr = Item::Pr { repo: "o/r".into(), number: "57".into(), draft: true };
        let (program, args, _) = plan(Action::Ready, &pr, "").unwrap();
        assert_eq!((program, args.as_slice()), ("gh", ["pr", "ready", "57", "-R", "o/r"].map(String::from).as_slice()));

        // Actions only fit their items.
        assert_eq!(plan(Action::Ready, &card(), "").unwrap_err(), "can't mark ready card 1041");
        assert!(plan(Action::Close, &pr, "").is_err());
    }

    #[test]
    fn failures_say_why_in_a_line() {
        let fizzy = r#"{"ok": false, "error": "Resource not found: x", "code": "not_found"}"#;
        assert_eq!(failure("fizzy", fizzy, ""), "fizzy: Resource not found: x");
        assert_eq!(failure("gh", "", "GraphQL: oops\n\n"), "gh: GraphQL: oops");
        assert_eq!(failure("gh", "", ""), "gh failed");

        let mut sh = Command::new("sh");
        sh.args(["-c", "echo partial; echo 'no such PR' >&2; exit 1"]);
        assert_eq!(run_bounded(&mut sh, "gh", TIMEOUT).unwrap_err(), "gh: no such PR");
        let mut ok = Command::new("sh");
        ok.args(["-c", "echo done"]);
        assert_eq!(run_bounded(&mut ok, "sh", TIMEOUT).unwrap(), "done\n");
        let mut slow = Command::new("sh");
        slow.args(["-c", "sleep 5"]);
        assert!(run_bounded(&mut slow, "fizzy", Duration::from_millis(200)).unwrap_err().contains("gave up"));
        let mut missing = Command::new("switchyard-no-such-program");
        assert_eq!(run_bounded(&mut missing, "fizzy", TIMEOUT).unwrap_err(), "fizzy: not found on PATH");
    }
}
