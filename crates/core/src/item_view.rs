//! A linked work item's description and comments, for the tab picker to
//! show in place rather than in the browser: a Fizzy card through `fizzy`
//! (`card show` and `comment list`, with `FIZZY_PROFILE` set to the card's
//! account, as `item_actions` does), a GitHub issue or PR through `gh`
//! (`view --json`) under the task's profile.
//!
//! Read-only. Each call is bounded; a failure is the CLI's own message,
//! for the picker's footer.

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::item_actions::{fizzy_account, run_bounded};
use crate::rex::home;
use crate::sessions::work_env;

/// How long one fizzy or gh call may take before it is given up on.
const TIMEOUT: Duration = Duration::from_secs(20);

/// A comment: who wrote it, when (ISO 8601), and its text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    pub at: String,
    pub body: String,
}

/// An item's title, state, opening post and comments, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Thread {
    pub title: String,
    /// Its state as the tracker says it: a card's column, `OPEN`, `MERGED`…
    pub state: String,
    /// The description or issue body, as the opening post.
    pub post: Comment,
    pub comments: Vec<Comment>,
    /// Whether the bodies are Markdown (GitHub's), rather than the plain
    /// text Fizzy gives for its rich text.
    pub markdown: bool,
}

/// Fetch the thread of the item a link names (see `links`): a Fizzy card
/// whose URL names its account, or a GitHub issue or PR (`owner/repo#N`),
/// asking gh under `profile`.
///
/// # Errors
///
/// Fails when the link names no item this can read, the work environment
/// can't be loaded, or a CLI fails, times out, or gives output that can't
/// be parsed.
pub fn fetch(
    kind: &str,
    system: &str,
    reference: &str,
    url: &str,
    profile: &str,
) -> Result<Thread, String> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match (kind, system) {
        ("card", "fizzy") => {
            let account = fizzy_account(url)
                .ok_or_else(|| format!("card {reference}: its URL names no Fizzy account"))?;
            if !digits(reference) {
                return Err(format!("card {reference}: not a card number"));
            }
            let fizzy = |args: &[&str]| {
                let mut cmd = Command::new("fizzy");
                cmd.args(args).env("FIZZY_PROFILE", account);
                // Not under a checkout, so no repo's `.fizzy.yaml` steers it.
                cmd.current_dir(home());
                run_bounded(&mut cmd, "fizzy", TIMEOUT)
            };
            let card = fizzy(&["card", "show", reference, "--json"])?;
            let comments = fizzy(&["comment", "list", "--card", reference, "--all", "--json"])?;
            parse_card(&card, &comments)
        }
        ("issue" | "pr", "github") => {
            let (repo, number) = reference
                .split_once('#')
                .filter(|(r, n)| r.contains('/') && digits(n))
                .ok_or_else(|| format!("{reference}: not an owner/repo#N reference"))?;
            let mut cmd = Command::new("gh");
            cmd.args([
                kind,
                "view",
                number,
                "-R",
                repo,
                "--json",
                "title,body,state,author,createdAt,comments",
            ])
            .env("GH_PROMPT_DISABLED", "1");
            cmd.current_dir(home());
            if profile == "work" {
                cmd.envs(work_env()?);
            }
            parse_github(&run_bounded(&mut cmd, "gh", TIMEOUT)?)
        }
        _ => Err(format!(
            "{kind} {reference}: only Fizzy cards and GitHub issues and PRs can be read here"
        )),
    }
}

/// The JSON `s` holds, or why not.
fn json(s: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(s.trim()).map_err(|e| format!("{what}: {e}"))
}

/// A string field, trimmed, or empty.
fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").trim().to_string()
}

/// A thread from `fizzy card show --json` and `fizzy comment list --json`.
fn parse_card(card: &str, comments: &str) -> Result<Thread, String> {
    let card = json(card, "fizzy card show")?;
    let card = &card["data"];
    if !card.is_object() {
        return Err("fizzy card show: no card in its output".into());
    }
    let state = if card["closed"].as_bool() == Some(true) && card["column"].is_null() {
        "Closed".to_string()
    } else {
        text(&card["column"]["name"])
    };
    let list = json(comments, "fizzy comment list")?;
    let mut comments: Vec<Comment> = list["data"]
        .as_array()
        .ok_or("fizzy comment list: not a list")?
        .iter()
        .map(|c| Comment {
            author: text(&c["creator"]["name"]),
            at: text(&c["created_at"]),
            body: text(&c["body"]["plain_text"]),
        })
        .collect();
    comments.sort_by(|a, b| a.at.cmp(&b.at));
    Ok(Thread {
        title: text(&card["title"]),
        state,
        post: Comment {
            author: text(&card["creator"]["name"]),
            at: text(&card["created_at"]),
            body: text(&card["description"]),
        },
        comments,
        markdown: false,
    })
}

/// A thread from `gh issue view --json` or `gh pr view --json`.
fn parse_github(out: &str) -> Result<Thread, String> {
    let v = json(out, "gh view")?;
    if !v.is_object() {
        return Err("gh view: not an object".into());
    }
    let mut comments: Vec<Comment> = v["comments"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .map(|c| Comment {
                    author: text(&c["author"]["login"]),
                    at: text(&c["createdAt"]),
                    body: text(&c["body"]),
                })
                .collect()
        })
        .unwrap_or_default();
    comments.sort_by(|a, b| a.at.cmp(&b.at));
    Ok(Thread {
        title: text(&v["title"]),
        state: text(&v["state"]),
        post: Comment {
            author: text(&v["author"]["login"]),
            at: text(&v["createdAt"]),
            body: text(&v["body"]),
        },
        comments,
        markdown: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_read_their_description_and_comments_oldest_first() {
        let card = r#"{"ok": true, "data": {"number": 1176, "title": " View in place ",
            "description": "Show it inline.", "created_at": "2026-11-14T18:00:00Z",
            "creator": {"name": "John Allen"}, "closed": false,
            "column": {"name": "Doing"}}}"#;
        let comments = r#"{"ok": true, "data": [
            {"creator": {"name": "B"}, "created_at": "2026-11-14T19:00:02Z",
             "body": {"plain_text": "second", "html": "<p>second</p>"}},
            {"creator": {"name": "A"}, "created_at": "2026-11-14T19:00:01Z",
             "body": {"plain_text": "first\n\n• a point", "html": ""}}
        ]}"#;
        let t = parse_card(card, comments).unwrap();
        assert_eq!(t.title, "View in place");
        assert_eq!(t.state, "Doing");
        assert_eq!(t.post.author, "John Allen");
        assert_eq!(t.post.body, "Show it inline.");
        let bodies: Vec<&str> = t.comments.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(bodies, ["first\n\n• a point", "second"]);
        assert_eq!(t.comments[0].author, "A");
        assert!(!t.markdown);

        let closed = r#"{"data": {"title": "t", "closed": true, "column": null}}"#;
        let t = parse_card(closed, r#"{"data": []}"#).unwrap();
        assert_eq!(t.state, "Closed");
        assert_eq!(t.comments, [] as [Comment; 0]);

        assert!(parse_card(r#"{"ok": false}"#, r#"{"data": []}"#).is_err());
        assert!(parse_card(card, "nope").is_err());
    }

    #[test]
    fn github_threads_read_body_and_comments() {
        let out = r#"{"title": "Fix it", "state": "OPEN", "body": "It breaks.\n",
            "author": {"login": "octo"}, "createdAt": "2026-11-01T10:00:00Z",
            "comments": [
                {"author": {"login": "z"}, "createdAt": "2026-11-02T10:00:00Z", "body": "later"},
                {"author": {"login": "y"}, "createdAt": "2026-11-01T11:00:00Z", "body": "sooner"}
            ]}"#;
        let t = parse_github(out).unwrap();
        assert_eq!(t.title, "Fix it");
        assert_eq!(t.state, "OPEN");
        assert_eq!(t.post.author, "octo");
        assert_eq!(t.post.body, "It breaks.");
        assert_eq!(t.comments[0].body, "sooner");
        assert_eq!(t.comments[1].author, "z");
        assert!(t.markdown);
        // No comments field: none.
        let t = parse_github(r#"{"title": "x"}"#).unwrap();
        assert_eq!(t.comments, [] as [Comment; 0]);
        assert!(parse_github("[]").is_err());
    }

    #[test]
    fn fetch_refuses_links_it_cant_read() {
        assert!(
            fetch("card", "fizzy", "1041", "", "personal")
                .unwrap_err()
                .contains("names no Fizzy account")
        );
        assert!(
            fetch("issue", "github", "nope", "u", "personal")
                .unwrap_err()
                .contains("owner/repo#N")
        );
        assert!(
            fetch("commit", "github", "abc", "u", "personal")
                .unwrap_err()
                .contains("only Fizzy cards")
        );
    }
}
