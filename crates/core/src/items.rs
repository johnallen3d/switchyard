//! A project's open work items, for the new-session picker to start work
//! from: the open cards on its Fizzy board (`fizzy card list`) or the open
//! issues in its GitHub repo (`gh issue list` under the project's profile),
//! whichever its tracker says a bare `#42` is (see links' `Tracker` and
//! `tracker_or_fizzy`).
//!
//! Listing runs where the picker does, so its checkout's `.fizzy.yaml` is
//! read there. Each CLI call is bounded; a missing, unauthenticated or slow
//! CLI gives an error the picker shows as nothing to choose.

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::hosts::Project;
use crate::item_actions::run_bounded;
use crate::links::{FIZZY_BOARD_SH, Tracker, bounded_output, gh, tracker_or_fizzy};
use crate::rex::{home, sh_quote};
use crate::tasks::rfc3339_secs;

/// How long one listing call may take before it is given up on.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// The most items listed.
pub const LIST_LIMIT: usize = 100;

/// An open card or issue.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenItem {
    pub number: u64,
    pub title: String,
    /// The item's URL, the work item a launch is given: unambiguous on any
    /// host, whatever tracker its checkout there has.
    pub url: String,
    /// When it was created, in seconds since the Unix epoch.
    pub created: Option<u64>,
    /// Its tags (Fizzy) or labels (GitHub).
    pub labels: Vec<String>,
    /// Who it's assigned to: names (Fizzy) or logins (GitHub).
    pub assignees: Vec<String>,
}

/// A project's open items and what they are (`cards` or `issues`).
#[derive(Clone, Debug, PartialEq)]
pub struct OpenItems {
    pub noun: &'static str,
    pub items: Vec<OpenItem>,
}

/// The open items of `project`'s tracker, as this host's CLIs see them.
///
/// # Errors
///
/// Fails when the tracker names no board or repository, or its CLI fails
/// or gives output that can't be parsed.
pub fn list_open(project: &Project) -> Result<OpenItems, String> {
    let tracker = Tracker::new(
        &tracker_or_fizzy(&project.tracker, &local_board(&project.path)),
        &project.clone_url,
    );
    if tracker.cards {
        let board = tracker
            .fizzy_board
            .as_deref()
            .ok_or("the tracker names no Fizzy board")?;
        let mut cmd = Command::new("fizzy");
        cmd.args([
            "card",
            "list",
            "--board",
            board,
            "--all",
            "--jq",
            "[.data[]|{number,title,url,created_at,tags,assignees:[.assignees[]?.name]}]",
        ]);
        // Away from any checkout's `.fizzy.yaml`; the board is explicit.
        cmd.current_dir(home());
        // The Fizzy CLI's profiles are named after their accounts.
        if let Some(account) = &tracker.fizzy_account {
            cmd.env("FIZZY_PROFILE", account);
        }
        // Why it failed (not on PATH, its error, a timeout) goes to the
        // picker's log.
        let out = run_bounded(&mut cmd, "fizzy", LIST_TIMEOUT)?;
        return Ok(OpenItems {
            noun: "cards",
            items: parse(&out)?,
        });
    }
    let repo = tracker
        .github
        .as_deref()
        .ok_or("no GitHub repo or Fizzy board to list")?;
    let limit = LIST_LIMIT.to_string();
    let out = gh(
        &project.profile,
        &[
            "issue",
            "list",
            "-R",
            repo,
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,title,url,createdAt,labels,assignees",
        ],
    )
    .ok_or("gh issue list failed")?;
    Ok(OpenItems {
        noun: "issues",
        items: parse(&out)?,
    })
}

/// The board the checkout at `path` (here) names in its `.fizzy.yaml`, as
/// `ACCOUNT/BOARD`, or empty.
fn local_board(path: &str) -> String {
    let dir = match path.strip_prefix("~/") {
        Some(rest) => home().join(rest).to_string_lossy().into_owned(),
        None => path.to_string(),
    };
    let mut cmd = Command::new("bash");
    cmd.args([
        "-c",
        &format!("{FIZZY_BOARD_SH}fizzy_board {}\n", sh_quote(&dir)),
    ]);
    bounded_output(&mut cmd, None, LIST_TIMEOUT).unwrap_or_default()
}

/// The names in a JSON list of strings or of objects naming themselves
/// (`login`, as GitHub's assignees, before `name`, which may be empty).
fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            v.as_str()
                .or_else(|| v["login"].as_str())
                .or_else(|| v["name"].as_str())
        })
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(String::from)
        .collect()
}

/// Items from a JSON list of `{number, title, url}`, as both CLIs give
/// them, with what else they give (creation time, labels or tags,
/// assignees), capped at `LIST_LIMIT`.
fn parse(json: &str) -> Result<Vec<OpenItem>, String> {
    let list: Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("work item list: {e}"))?;
    let items = list.as_array().ok_or("work item list: not a list")?;
    Ok(items
        .iter()
        .filter_map(|i| {
            Some(OpenItem {
                number: i["number"].as_u64()?,
                title: i["title"].as_str().unwrap_or("").trim().to_string(),
                url: i["url"].as_str().filter(|u| !u.is_empty())?.to_string(),
                created: i["created_at"]
                    .as_str()
                    .or_else(|| i["createdAt"].as_str())
                    .and_then(rfc3339_secs),
                labels: names(if i["tags"].is_null() {
                    &i["labels"]
                } else {
                    &i["tags"]
                }),
                assignees: names(&i["assignees"]),
            })
        })
        .take(LIST_LIMIT)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cards_and_issues_and_skips_unusable_entries() {
        let json = r#"[
            {"number": 1158, "title": " Spawn from a card ", "url": "https://app.fizzy.do/1/cards/1158"},
            {"number": 7, "title": "no url"},
            {"title": "no number", "url": "https://github.com/o/r/issues/3"},
            {"number": 42, "title": "Fix it", "url": "https://github.com/o/r/issues/42",
             "createdAt": "2026-11-07T15:40:18Z",
             "labels": [{"name": "bug", "color": "d73a4a"}],
             "assignees": [{"login": "octo", "name": "Octo Cat"}]}
        ]"#;
        let items = parse(json).unwrap();
        assert_eq!(
            items,
            vec![
                OpenItem {
                    number: 1158,
                    title: "Spawn from a card".into(),
                    url: "https://app.fizzy.do/1/cards/1158".into(),
                    ..OpenItem::default()
                },
                OpenItem {
                    number: 42,
                    title: "Fix it".into(),
                    url: "https://github.com/o/r/issues/42".into(),
                    created: rfc3339_secs("2026-11-07T15:40:18Z"),
                    labels: vec!["bug".into()],
                    assignees: vec!["octo".into()],
                },
            ]
        );
        // Fizzy's shape, as the listing's jq gives it.
        let card = parse(
            r#"[{"number": 9, "title": "t", "url": "u", "created_at": "2026-11-15T03:05:47.144Z",
                 "tags": ["feature"], "assignees": ["John Allen"]}]"#,
        )
        .unwrap();
        assert_eq!(card[0].labels, ["feature"]);
        assert_eq!(card[0].assignees, ["John Allen"]);
        assert!(card[0].created.is_some());
        assert_eq!(parse("[]").unwrap(), [] as [OpenItem; 0]);
        assert!(parse("{\"ok\": false}").is_err());
        assert!(parse("not json").is_err());
    }
}
