//! Names unnamed work at launch. `launch` asks for a name when the picker
//! left it blank: from the work item's title when there is one and its
//! tracker can say it, otherwise from the prompt. The name comes from
//! macOS's on-device Foundation Model through Apple's `fm` CLI (preinstalled
//! from macOS 27; its license must have been accepted once with
//! `sudo fm license`). It runs where `launch` does, the config host (the
//! Mac). No other model is tried: when `fm` is missing, fails, is slow or
//! answers with something unusable, there is no name and the work must be
//! named by hand.

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::links::{Tracker, bounded_output, gh_title, parse_ref};
use crate::naming::{name_char, name_problem};

/// How long one `fm` call may take; a warm call takes under a second.
const FM_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a work item's title lookup may take.
const TITLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Prompt characters the model sees; its context is a few thousand tokens.
const MAX_INPUT_CHARS: usize = 2000;
/// The longest slug kept, cut at a word.
const MAX_SLUG_CHARS: usize = 40;
/// The prefixes a name may have; `feature` work is named by its slug alone
/// (see `naming::branch_and_slug`).
const PREFIXES: [&str; 5] = ["feature", "fix", "chore", "docs", "refactor"];

const INSTRUCTIONS: &str = "Name a software task as a git branch. Pick the prefix by intent: \
fix for bugs and crashes, feature for new behaviour, docs for documentation, refactor for \
restructuring code, chore for maintenance. The slug names the specific thing being changed in \
2 to 5 lowercase kebab-case words, never a generic word alone. Examples: \"the login page \
rejects valid passwords\" -> fix / login-rejects-valid-passwords; \"let users export reports \
as CSV\" -> feature / csv-report-export.";

/// The structured output `fm respond --schema` must produce, as
/// `fm schema object --name WorkName --string prefix … --string slug …`
/// writes it.
const SCHEMA: &str = r#"{
  "title": "WorkName",
  "type": "object",
  "additionalProperties": false,
  "required": ["prefix", "slug"],
  "x-order": ["prefix", "slug"],
  "properties": {
    "prefix": {"type": "string", "description": "one of: feature, fix, chore, docs, refactor"},
    "slug": {"type": "string", "description": "2 to 5 lowercase kebab-case words naming the specific change"}
  }
}"#;

/// A name for work that has none: from `item`'s title when it can be
/// looked up, otherwise from `prompt`. None when neither gives one.
pub fn name_work(profile: &str, item: Option<&str>, tracker: &Tracker, prompt: &str) -> Option<String> {
    item.and_then(|i| item_title(profile, i, tracker))
        .and_then(|title| suggest(&title))
        .or_else(|| Some(prompt.trim()).filter(|p| !p.is_empty()).and_then(suggest))
}

/// A work item's title: a Fizzy card's from the Fizzy CLI, a GitHub issue
/// or PR's from gh under the project's profile. None for refs no tracker
/// places (a Linear key, say) or when the lookup fails.
fn item_title(profile: &str, item: &str, tracker: &Tracker) -> Option<String> {
    let link = parse_ref(item, tracker)?;
    let number = link["ref"].as_str()?;
    let title = match link["system"].as_str()? {
        "fizzy" => {
            let mut cmd = Command::new("fizzy");
            cmd.args(["card", "show", number, "--jq", ".data.title"]);
            // The Fizzy CLI's profiles are named after their accounts.
            if let Some(account) = card_account(&link).or(tracker.fizzy_account.clone()) {
                cmd.env("FIZZY_PROFILE", account);
            }
            bounded_output(&mut cmd, None, TITLE_TIMEOUT)?
        }
        "github" => {
            let (repo, n) = number.split_once('#')?;
            gh_title(profile, link["kind"].as_str()?, repo, n)?
        }
        _ => return None,
    };
    Some(title.trim().to_string()).filter(|t| !t.is_empty())
}

/// The account in a Fizzy card link's URL.
fn card_account(link: &Value) -> Option<String> {
    let rest = link["url"].as_str()?.strip_prefix("https://app.fizzy.do/")?;
    rest.split('/').next().filter(|a| !a.is_empty()).map(String::from)
}

/// Ask the on-device model to name work described by `text`.
fn suggest(text: &str) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let text: String = text.chars().take(MAX_INPUT_CHARS).collect();
    // `fm` reads the schema from a file; one per process keeps concurrent
    // launches from tripping over each other.
    let schema = std::env::temp_dir().join(format!("switchyard-name-schema-{}.json", std::process::id()));
    std::fs::write(&schema, SCHEMA).ok()?;
    let mut cmd = Command::new("fm");
    cmd.args(["respond", "--no-stream", "--greedy", "--instructions", INSTRUCTIONS, "--schema"]).arg(&schema);
    // The text goes in on stdin, so one starting with `-` stays text.
    let out = bounded_output(&mut cmd, Some(&text), FM_TIMEOUT);
    let _ = std::fs::remove_file(&schema);
    parse(&out?)
}

/// The work name in `fm`'s JSON answer, cleaned up: a known prefix
/// (`feature` otherwise) and a lowercase kebab-case slug, or None when
/// there is no usable slug.
fn parse(answer: &str) -> Option<String> {
    let v: Value = serde_json::from_str(answer.trim()).ok()?;
    let prefix = v["prefix"].as_str().map(|p| p.trim().to_ascii_lowercase()).unwrap_or_default();
    let prefix = PREFIXES.iter().find(|p| **p == prefix).copied().unwrap_or("feature");
    let slug = slug(v["slug"].as_str()?);
    if slug.is_empty() {
        return None;
    }
    let name = if prefix == "feature" { slug } else { format!("{prefix}/{slug}") };
    name_problem(&name).is_none().then_some(name)
}

/// `text` as a slug: lowercase ASCII words joined by single dashes, cut at
/// a word to `MAX_SLUG_CHARS`.
fn slug(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter_map(name_char)
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let mut slug = String::new();
    for word in cleaned.split('-').filter(|w| !w.is_empty()) {
        let extra = if slug.is_empty() { word.len() } else { word.len() + 1 };
        if slug.len() + extra > MAX_SLUG_CHARS {
            if slug.is_empty() {
                slug.push_str(&word[..MAX_SLUG_CHARS]);
            }
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(word);
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_become_names() {
        assert_eq!(parse(r#"{"prefix": "fix", "slug": "archive-during-check-crash"}"#).as_deref(), Some("fix/archive-during-check-crash"));
        // Key order varies; feature work is named by its slug alone.
        assert_eq!(parse(r#"{"slug": "dark-mode-toggle", "prefix": "feature"}"#).as_deref(), Some("dark-mode-toggle"));
        assert_eq!(parse(r#"{"prefix": "Docs", "slug": "README Linux install"}"#).as_deref(), Some("docs/readme-linux-install"));
        // An unknown prefix falls back to feature.
        assert_eq!(parse(r#"{"prefix": "bugfix", "slug": "x"}"#).as_deref(), Some("x"));
        assert_eq!(parse(r#"{"prefix": "fix", "slug": "a/b.c--d_e"}"#).as_deref(), Some("fix/a-b-c-d-e"));
    }

    #[test]
    fn unusable_answers_give_no_name() {
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"prefix": "fix"}"#), None);
        assert_eq!(parse(r#"{"prefix": "fix", "slug": "!!! ---"}"#), None);
    }

    #[test]
    fn long_slugs_are_cut_at_a_word() {
        let s = slug("one two three four five six seven eight nine ten");
        assert_eq!(s, "one-two-three-four-five-six-seven-eight");
        assert!(s.len() <= MAX_SLUG_CHARS);
        assert_eq!(slug(&"x".repeat(60)).len(), MAX_SLUG_CHARS);
    }

    /// Asks the real on-device model and Fizzy; run with `--ignored` on a
    /// Mac with `fm` set up.
    #[test]
    #[ignore]
    fn live_names_from_prompt_and_card() {
        let fizzy = Tracker::new("fizzy:6284043/board", "");
        for prompt in ["the tab picker crashes when you archive a task while its check is still running", "-n starts with a dash"] {
            let name = name_work("personal", None, &fizzy, prompt);
            eprintln!("{prompt} -> {name:?}");
            assert!(name.is_some());
        }
        let title = item_title("personal", "#1056", &fizzy);
        eprintln!("#1056 -> {title:?} -> {:?}", name_work("personal", Some("#1056"), &fizzy, ""));
        assert!(title.is_some());
    }

    #[test]
    fn card_links_carry_their_account() {
        let link = parse_ref("https://app.fizzy.do/6284043/cards/1056", &Tracker::default()).unwrap();
        assert_eq!(card_account(&link).as_deref(), Some("6284043"));
    }
}
