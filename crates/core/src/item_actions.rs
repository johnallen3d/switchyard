//! Actions on a task's linked work items, for the tab picker: comment on
//! a Fizzy card or a GitHub issue or PR, close a card or an issue, move a
//! card to Not Now, and mark
//! a draft PR ready for review, and approve or squash-merge a PR. (A PR
//! isn't closed from here.)
//!
//! A squash-merge asks GitHub first, at once, whether the PR may merge
//! (see `merge_blocker`: open, not a draft, approved, no failing or running
//! checks), and merges only the head it checked (`--match-head-commit`),
//! so a push in between doesn't merge unchecked commits.
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
    /// Approve a PR: the Work view's, not offered by `Item::actions`.
    Approve,
    /// Squash-merge a PR once `merge_blocker` finds nothing in its way:
    /// the Work view's, not offered by `Item::actions`. Its branch stays,
    /// for the task's archive to settle.
    Merge,
    /// Move a card to Not Now (`fizzy card postpone`).
    Postpone,
}

impl Action {
    /// What the action does, for confirmations: "close card 1041".
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Action::Comment => "comment on",
            Action::Close => "close",
            Action::Ready => "mark ready",
            Action::Approve => "approve",
            Action::Merge => "squash-merge",
            Action::Postpone => "postpone",
        }
    }
}

/// The work item an action is on, as the registry links it (see `links`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// A Fizzy card: its account (from its URL) and number.
    Card { account: String, number: String },
    /// A GitHub issue: `owner/repo` and its number.
    Issue { repo: String, number: String },
    /// A GitHub PR: `owner/repo`, its number, and whether it is a draft.
    Pr {
        repo: String,
        number: String,
        draft: bool,
    },
}

impl Item {
    /// The item a link names, when actions can be taken on it: a Fizzy card
    /// whose URL names its account, or a GitHub issue or PR.
    #[must_use]
    pub fn from_link(
        kind: &str,
        system: &str,
        reference: &str,
        url: &str,
        state: &str,
    ) -> Option<Item> {
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        match (kind, system) {
            ("card", "fizzy") => {
                let account = fizzy_account(url)?;
                digits(reference).then(|| Item::Card {
                    account: account.into(),
                    number: reference.into(),
                })
            }
            ("issue", "github") => {
                let (repo, number) = reference.split_once('#')?;
                (repo.contains('/') && digits(number)).then(|| Item::Issue {
                    repo: repo.into(),
                    number: number.into(),
                })
            }
            ("pr", "github") => {
                let (repo, number) = reference.split_once('#')?;
                (repo.contains('/') && digits(number)).then(|| Item::Pr {
                    repo: repo.into(),
                    number: number.into(),
                    draft: state == "draft",
                })
            }
            _ => None,
        }
    }

    /// The actions this item takes: comment on and close a card or an
    /// issue, and move a card to Not Now; comment on a PR, and mark a
    /// draft one ready.
    #[must_use]
    pub fn actions(&self) -> Vec<Action> {
        match self {
            Item::Card { .. } => vec![Action::Comment, Action::Close, Action::Postpone],
            Item::Issue { .. } => vec![Action::Comment, Action::Close],
            Item::Pr { draft: true, .. } => vec![Action::Comment, Action::Ready],
            Item::Pr { .. } => vec![Action::Comment],
        }
    }

    /// How a confirmation names it: `card 1041`, `issue owner/repo#3`,
    /// `PR owner/repo#57`.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Item::Card { number, .. } => format!("card {number}"),
            Item::Issue { repo, number } => format!("issue {repo}#{number}"),
            Item::Pr { repo, number, .. } => format!("PR {repo}#{number}"),
        }
    }
}

/// The account a Fizzy card URL names: `https://app.fizzy.do/ACCOUNT/cards/N`.
#[must_use]
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
    let s = |v: &[&str]| {
        v.iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    };
    let gh = |args: Vec<String>| -> Plan {
        ("gh", args, vec![("GH_PROMPT_DISABLED".into(), "1".into())])
    };
    if action == Action::Comment && body.trim().is_empty() {
        return Err("an empty comment: nothing to post".into());
    }
    if action == Action::Merge && body.is_empty() {
        return Err("no head commit checked: nothing to merge".into());
    }
    match (action, item) {
        (Action::Comment, Item::Card { account, number }) => {
            let args = s(&[
                "comment", "create", "--card", number, "--body", body, "--json",
            ]);
            Ok((
                "fizzy",
                args,
                vec![("FIZZY_PROFILE".into(), account.clone())],
            ))
        }
        (Action::Close, Item::Card { account, number }) => Ok((
            "fizzy",
            s(&["card", "close", number, "--json"]),
            vec![("FIZZY_PROFILE".into(), account.clone())],
        )),
        (Action::Postpone, Item::Card { account, number }) => Ok((
            "fizzy",
            s(&["card", "postpone", number, "--json"]),
            vec![("FIZZY_PROFILE".into(), account.clone())],
        )),
        (Action::Comment, Item::Issue { repo, number }) => Ok(gh(s(&[
            "issue", "comment", number, "-R", repo, "--body", body,
        ]))),
        (Action::Comment, Item::Pr { repo, number, .. }) => Ok(gh(s(&[
            "pr", "comment", number, "-R", repo, "--body", body,
        ]))),
        (Action::Close, Item::Issue { repo, number }) => {
            Ok(gh(s(&["issue", "close", number, "-R", repo])))
        }
        (Action::Ready, Item::Pr { repo, number, .. }) => {
            Ok(gh(s(&["pr", "ready", number, "-R", repo])))
        }
        (Action::Approve, Item::Pr { repo, number, .. }) => {
            Ok(gh(s(&["pr", "review", number, "-R", repo, "--approve"])))
        }
        // `body` is the head commit the merge was checked at.
        (Action::Merge, Item::Pr { repo, number, .. }) => Ok(gh(s(&[
            "pr",
            "merge",
            number,
            "-R",
            repo,
            "--squash",
            "--match-head-commit",
            body,
        ]))),
        (action, item) => Err(format!("can't {} {}", action.verb(), item.name())),
    }
}

/// Run `action` on `item` (a comment's text in `body`; gh under
/// `profile`); Ok is the outcome to show, Err the CLI's message.
///
/// # Errors
///
/// Fails when `action` doesn't apply to `item`, the work environment can't
/// be loaded, or the CLI fails or times out.
pub fn run(action: Action, item: &Item, body: &str, profile: &str) -> Result<String, String> {
    let head;
    let body = match (action, item) {
        (Action::Merge, Item::Pr { repo, number, .. }) => {
            head = mergeable_head(repo, number, profile)?;
            head.as_str()
        }
        _ => body,
    };
    let (program, args, env) = plan(action, item, body)?;
    let mut cmd = command(program, profile)?;
    cmd.args(&args).envs(env);
    run_bounded(&mut cmd, program, TIMEOUT)?;
    let name = item.name();
    Ok(match action {
        Action::Comment => format!("commented on {name}"),
        Action::Close => format!("closed {name}"),
        Action::Ready => format!("marked {name} ready for review"),
        Action::Approve => format!("approved {name}"),
        Action::Merge => format!("squash-merged {name}"),
        Action::Postpone => format!("moved {name} to Not Now"),
    })
}

/// `program`, run from home (not under a checkout, so no repo's
/// `.fizzy.yaml` steers it), gh with `profile`'s environment.
fn command(program: &str, profile: &str) -> Result<Command, String> {
    let mut cmd = Command::new(program);
    cmd.current_dir(home());
    if program == "gh" {
        cmd.env("GH_PROMPT_DISABLED", "1");
        if profile == "work" {
            cmd.envs(work_env()?);
        }
    }
    Ok(cmd)
}

/// Why a PR can't be squash-merged, from what GitHub reports of it: its
/// state (`OPEN`, ...), whether it's a draft, its review decision (see
/// items' `review_decision`) and its checks (see links' `checks_summary`;
/// none is no CI to wait for). None when it can merge.
#[must_use]
pub fn merge_blocker(state: &str, draft: bool, review: &str, checks: &str) -> Option<String> {
    Some(
        match (state, draft, review, checks) {
            (s, ..) if s != "OPEN" => "isn't open",
            (_, true, ..) => "is a draft",
            (_, _, "CHANGES_REQUESTED", _) => "has changes requested",
            (_, _, r, _) if r != "APPROVED" => "isn't approved yet",
            (.., "fail") => "has failing checks",
            (.., "pass" | "") => return None,
            _ => "has checks still running",
        }
        .into(),
    )
}

/// The head commit of PR `number` in `repo`, as GitHub reports it now,
/// when `merge_blocker` finds nothing in the way; else why not.
fn mergeable_head(repo: &str, number: &str, profile: &str) -> Result<String, String> {
    let fields = "state,isDraft,reviewDecision,latestReviews,statusCheckRollup,headRefOid";
    let mut cmd = command("gh", profile)?;
    cmd.args(["pr", "view", number, "-R", repo, "--json", fields]);
    let out = run_bounded(&mut cmd, "gh", TIMEOUT)?;
    let pr: Value = serde_json::from_str(out.trim()).map_err(|e| format!("gh pr view: {e}"))?;
    mergeable(&pr).map_err(|why| format!("PR {repo}#{number} {why}"))
}

/// The head of a PR from gh's `pr view --json`, when it can merge.
fn mergeable(pr: &Value) -> Result<String, String> {
    let blocker = merge_blocker(
        pr["state"].as_str().unwrap_or(""),
        pr["isDraft"].as_bool().unwrap_or(false),
        &crate::items::review_decision(pr),
        &crate::links::checks_summary(&pr["statusCheckRollup"]),
    );
    if let Some(why) = blocker {
        return Err(why);
    }
    pr["headRefOid"]
        .as_str()
        .filter(|h| !h.is_empty())
        .map(String::from)
        .ok_or_else(|| "has no head commit GitHub reports".into())
}

/// Run `cmd` with stdin closed, giving up after `timeout`. Its stdout, or
/// on failure the reason: a Fizzy JSON error's message, else the last line
/// of stderr (or stdout), or that the program isn't on PATH.
pub(crate) fn run_bounded(
    cmd: &mut Command,
    program: &str,
    timeout: Duration,
) -> Result<String, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let out = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} took longer than {}s; gave up",
                    timeout.as_secs()
                ));
            }
        }
    };
    let (out, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
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
    let last = |s: &str| {
        s.lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(String::from)
    };
    match last(err).or_else(|| last(out)) {
        Some(line) => format!("{program}: {line}"),
        None => format!("{program} failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Item {
        Item::Card {
            account: "6284043".into(),
            number: "1041".into(),
        }
    }

    #[test]
    fn links_become_items_with_their_actions() {
        let c = Item::from_link(
            "card",
            "fizzy",
            "1041",
            "https://app.fizzy.do/6284043/cards/1041",
            "",
        )
        .unwrap();
        assert_eq!(c, card());
        assert_eq!(
            c.actions(),
            [Action::Comment, Action::Close, Action::Postpone]
        );
        assert_eq!(c.name(), "card 1041");
        // A card without its account in a URL can't say whose profile.
        assert_eq!(Item::from_link("card", "fizzy", "1041", "", ""), None);
        let draft = Item::from_link(
            "pr",
            "github",
            "o/r#57",
            "https://github.com/o/r/pull/57",
            "draft",
        )
        .unwrap();
        assert_eq!(draft.name(), "PR o/r#57");
        assert_eq!(draft.actions(), [Action::Comment, Action::Ready]);
        let open = Item::from_link("pr", "github", "o/r#57", "u", "open").unwrap();
        assert_eq!(open.actions(), [Action::Comment]);
        let issue = Item::from_link("issue", "github", "o/r#3", "u", "").unwrap();
        assert_eq!(issue.actions(), [Action::Comment, Action::Close]);
        assert_eq!(issue.name(), "issue o/r#3");
        assert_eq!(Item::from_link("issue", "github", "3", "u", ""), None);
        assert_eq!(Item::from_link("commit", "github", "abc", "u", ""), None);
    }

    #[test]
    fn fizzy_account_reads_card_urls_only() {
        assert_eq!(
            fizzy_account("https://app.fizzy.do/6284043/cards/1159"),
            Some("6284043")
        );
        assert_eq!(fizzy_account("https://app.fizzy.do/6284043/boards/x"), None);
        assert_eq!(fizzy_account("https://github.com/o/r/pull/1"), None);
    }

    #[test]
    fn plans_run_the_clis_under_the_items_account() {
        let (program, args, env) = plan(Action::Close, &card(), "").unwrap();
        assert_eq!(program, "fizzy");
        assert_eq!(args, ["card", "close", "1041", "--json"]);
        assert_eq!(env, [("FIZZY_PROFILE".to_string(), "6284043".to_string())]);
        let (program, args, env) = plan(Action::Postpone, &card(), "").unwrap();
        assert_eq!(program, "fizzy");
        assert_eq!(args, ["card", "postpone", "1041", "--json"]);
        assert_eq!(env[0].1, "6284043");

        let (_, args, env) = plan(Action::Comment, &card(), "looks good; shipping").unwrap();
        assert_eq!(
            args,
            [
                "comment",
                "create",
                "--card",
                "1041",
                "--body",
                "looks good; shipping",
                "--json"
            ]
        );
        assert_eq!(env[0].1, "6284043");
        assert!(plan(Action::Comment, &card(), "  \n").is_err());

        let pr = Item::Pr {
            repo: "o/r".into(),
            number: "57".into(),
            draft: true,
        };
        let (program, args, _) = plan(Action::Ready, &pr, "").unwrap();
        assert_eq!(
            (program, args.as_slice()),
            (
                "gh",
                ["pr", "ready", "57", "-R", "o/r"]
                    .map(String::from)
                    .as_slice()
            )
        );

        let issue = Item::Issue {
            repo: "o/r".into(),
            number: "3".into(),
        };
        let (program, args, _) = plan(Action::Close, &issue, "").unwrap();
        assert_eq!(program, "gh");
        assert_eq!(args, ["issue", "close", "3", "-R", "o/r"]);
        let (_, args, _) = plan(Action::Comment, &issue, "on it").unwrap();
        assert_eq!(
            args,
            ["issue", "comment", "3", "-R", "o/r", "--body", "on it"]
        );
        let (program, args, _) = plan(Action::Approve, &pr, "").unwrap();
        assert_eq!(program, "gh");
        assert_eq!(args, ["pr", "review", "57", "-R", "o/r", "--approve"]);
        assert!(plan(Action::Approve, &issue, "").is_err());
        let (program, args, _) = plan(Action::Merge, &pr, "abc123").unwrap();
        assert_eq!(program, "gh");
        assert_eq!(
            args,
            [
                "pr",
                "merge",
                "57",
                "-R",
                "o/r",
                "--squash",
                "--match-head-commit",
                "abc123"
            ]
        );
        assert!(plan(Action::Merge, &pr, "").is_err());
        assert!(plan(Action::Merge, &issue, "abc").is_err());
        let (_, args, _) = plan(Action::Comment, &pr, "lgtm").unwrap();
        assert_eq!(args, ["pr", "comment", "57", "-R", "o/r", "--body", "lgtm"]);
        assert!(plan(Action::Comment, &issue, " ").is_err());

        // Actions only fit their items; a PR isn't closed from here.
        assert_eq!(
            plan(Action::Ready, &card(), "").unwrap_err(),
            "can't mark ready card 1041"
        );
        assert!(plan(Action::Close, &pr, "").is_err());
        assert!(plan(Action::Postpone, &issue, "").is_err());
    }

    #[test]
    fn merges_wait_for_an_approval_and_passing_checks() {
        let ok = |s, d, r, c| merge_blocker(s, d, r, c);
        assert_eq!(ok("OPEN", false, "APPROVED", "pass"), None);
        // No checks is no CI to wait for.
        assert_eq!(ok("OPEN", false, "APPROVED", ""), None);
        assert_eq!(
            ok("MERGED", false, "APPROVED", "pass").unwrap(),
            "isn't open"
        );
        assert_eq!(ok("OPEN", true, "APPROVED", "pass").unwrap(), "is a draft");
        assert_eq!(ok("OPEN", false, "", "pass").unwrap(), "isn't approved yet");
        assert_eq!(
            ok("OPEN", false, "REVIEW_REQUIRED", "pass").unwrap(),
            "isn't approved yet"
        );
        assert_eq!(
            ok("OPEN", false, "CHANGES_REQUESTED", "pass").unwrap(),
            "has changes requested"
        );
        assert_eq!(
            ok("OPEN", false, "APPROVED", "fail").unwrap(),
            "has failing checks"
        );
        assert_eq!(
            ok("OPEN", false, "APPROVED", "pending").unwrap(),
            "has checks still running"
        );

        let pr = serde_json::json!({
            "state": "OPEN", "isDraft": false, "reviewDecision": "",
            "latestReviews": [{"state": "APPROVED"}],
            "statusCheckRollup": [{"status": "COMPLETED", "conclusion": "SUCCESS"}],
            "headRefOid": "abc123",
        });
        assert_eq!(mergeable(&pr).unwrap(), "abc123");
        let mut running = pr.clone();
        running["statusCheckRollup"][0]["status"] = "IN_PROGRESS".into();
        assert_eq!(mergeable(&running).unwrap_err(), "has checks still running");
    }

    #[test]
    fn failures_say_why_in_a_line() {
        let fizzy = r#"{"ok": false, "error": "Resource not found: x", "code": "not_found"}"#;
        assert_eq!(failure("fizzy", fizzy, ""), "fizzy: Resource not found: x");
        assert_eq!(failure("gh", "", "GraphQL: oops\n\n"), "gh: GraphQL: oops");
        assert_eq!(failure("gh", "", ""), "gh failed");

        let mut sh = Command::new("sh");
        sh.args(["-c", "echo partial; echo 'no such PR' >&2; exit 1"]);
        assert_eq!(
            run_bounded(&mut sh, "gh", TIMEOUT).unwrap_err(),
            "gh: no such PR"
        );
        let mut ok = Command::new("sh");
        ok.args(["-c", "echo done"]);
        assert_eq!(run_bounded(&mut ok, "sh", TIMEOUT).unwrap(), "done\n");
        let mut slow = Command::new("sh");
        slow.args(["-c", "sleep 5"]);
        assert!(
            run_bounded(&mut slow, "fizzy", Duration::from_millis(200))
                .unwrap_err()
                .contains("gave up")
        );
        let mut missing = Command::new("switchyard-no-such-program");
        assert_eq!(
            run_bounded(&mut missing, "fizzy", TIMEOUT).unwrap_err(),
            "fizzy: not found on PATH"
        );
    }
}
