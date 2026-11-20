//! Names unnamed work at launch. `launch` asks for a name when the picker
//! left it blank: from the work item's title when there is one and its
//! tracker can say it, otherwise from the prompt. The name comes from
//! macOS's on-device Foundation Model through Apple's `fm` CLI (preinstalled
//! from macOS 27; its license must have been accepted once with
//! `sudo fm license`). It runs where `launch` does, the config host (the
//! Mac). No other model is tried: when `fm` is missing, fails, is slow or
//! answers with something unusable, a work item's title, or else the
//! prompt's first words, becomes the name as it is (slugged). The picker
//! has already closed by then, so a launch never fails for want of a name
//! unless the text has nothing to name it from.
//!
//! The model makes up a name when the text gives it nothing to go on (a
//! bare link and a slash command become `fix/crash-fix`, echoing its
//! instructions), so:
//!
//! - A prompt names work only when it is `nameable`: enough words of its
//!   own once links, paths, slash commands and code are dropped (see
//!   `prose`). The picker asks for a name otherwise, and `name_work` gives
//!   none.
//! - An answer counts only when it is `grounded`: some specific word of its
//!   slug (not a prefix or a generic word like `fix` or `issue`) appears in
//!   the text, near enough (see `same_word`).
//!
//! The picker also asks for a name as the prompt is typed (`suggest_name`),
//! so Enter rarely waits on the model at launch.

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::links::{Tracker, bounded_output, gh_title, parse_ref};
use crate::naming::{name_char, name_problem};

/// How long one `fm` call may take; a warm call can take 4 seconds, and a
/// cold one longer.
const FM_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a work item's title lookup may take.
const TITLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Prompt characters the model sees; its context is a few thousand tokens.
const MAX_INPUT_CHARS: usize = 2000;
/// The longest slug kept, cut at a word.
const MAX_SLUG_CHARS: usize = 40;
/// The prefixes a name may have; `feature` work is named by its slug alone
/// (see `naming::branch_and_slug`).
const PREFIXES: [&str; 5] = ["feature", "fix", "chore", "docs", "refactor"];
/// How many words of its own (see `content_words`) a prompt needs to name
/// work from.
const MIN_PROSE_WORDS: usize = 2;
/// Words that say nothing about which work is meant: they don't make a
/// prompt nameable, nor ground a name (with `GENERIC`).
const FILLER: &[&str] = &[
    "about", "after", "again", "all", "also", "and", "any", "are", "before", "but", "can", "check",
    "could", "did", "does", "doing", "done", "for", "from", "get", "give", "got", "had", "has",
    "have", "help", "here", "hey", "hello", "how", "into", "its", "just", "let", "lets", "like",
    "look", "make", "more", "need", "not", "now", "okay", "one", "our", "out", "please", "read",
    "really", "see", "should", "some", "take", "thank", "thanks", "that", "the", "then", "there",
    "these", "this", "those", "try", "very", "want", "was", "were", "what", "when", "where",
    "which", "who", "why", "will", "with", "would", "yeah", "yes", "you", "your",
];
/// Slug words too generic to show the model read the text: the model
/// reaches for them (and its instructions' examples) when it has nothing.
const GENERIC: &[&str] = &[
    "add", "bug", "change", "chore", "crash", "docs", "error", "feature", "fix", "improve",
    "issue", "problem", "refactor", "support", "task", "update", "work",
];
/// Letters two words must share at the start to count as one (`archive`
/// and `archiving`).
const STEM_CHARS: usize = 5;

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
/// looked up (the model's name, or the title itself slugged), otherwise
/// from `prompt` (likewise) when it is `nameable`. None when neither gives
/// one.
#[must_use]
pub fn name_work(
    profile: &str,
    item: Option<&str>,
    tracker: &Tracker,
    prompt: &str,
) -> Option<String> {
    item.and_then(|i| item_title(profile, i, tracker))
        .and_then(|title| suggest(&title).or_else(|| title_name(&title)))
        .or_else(|| {
            Some(prompt.trim())
                .filter(|p| nameable(p))
                .and_then(|p| suggest(p).or_else(|| prompt_name(p)))
        })
}

/// The model's name for work described by `prompt`, for the picker to
/// offer as it is typed; None when the prompt isn't `nameable`, off the
/// Mac, or when the model gives nothing `grounded`.
#[must_use]
pub fn suggest_name(prompt: &str) -> Option<String> {
    let prompt = prompt.trim();
    if nameable(prompt) {
        suggest(prompt)
    } else {
        None
    }
}

/// Have the on-device model loaded, so the first `suggest_name` answers
/// sooner: one throwaway question, its answer ignored. Does nothing off
/// the Mac.
pub fn warm_up() {
    if cfg!(target_os = "macos") {
        let _ = bounded_output(
            Command::new("fm").args(["respond", "--no-stream", "--greedy"]),
            Some("ok"),
            FM_TIMEOUT,
        );
    }
}

/// Whether `prompt` says enough to name work from: at least
/// `MIN_PROSE_WORDS` words of its own (see `content_words`).
#[must_use]
pub fn nameable(prompt: &str) -> bool {
    content_words(&prose(prompt)).len() >= MIN_PROSE_WORDS
}

/// `text`'s prose: its words with fenced code blocks, links, paths (any
/// word with a slash), slash commands and `@file` mentions dropped, and
/// backticks and leading list marks stripped.
fn prose(text: &str) -> String {
    let mut words = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        for word in line.split_whitespace() {
            let word = word.trim_matches('`');
            if word.contains('/') || word.contains('\\') || word.starts_with('@') {
                continue;
            }
            if word.starts_with("www.") || word.contains("://") {
                continue;
            }
            if word.chars().any(char::is_alphanumeric) {
                words.push(word);
            }
        }
    }
    words.join(" ")
}

/// `text`'s words, lowercased and split at anything but letters and
/// digits.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// The words in `text` that could say which work is meant: three letters
/// or more, with a letter, and not `FILLER`.
fn content_words(text: &str) -> Vec<String> {
    words(text)
        .filter(|w| w.chars().count() >= 3 && w.chars().any(char::is_alphabetic))
        .filter(|w| !FILLER.contains(&w.as_str()))
        .collect()
}

/// Whether two lowercase words are the same word: equal, or sharing their
/// first `STEM_CHARS` letters (or all of the shorter, when that has three
/// or more), so `hangs` and `hang` or `archive` and `archiving` match.
fn same_word(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let n = a.chars().count().min(b.chars().count()).min(STEM_CHARS);
    n >= 3 && a.chars().take(n).eq(b.chars().take(n))
}

/// Whether `name`'s slug shows the model read `text`: one of its words,
/// other than prefixes, `FILLER` and `GENERIC` words, is a word of `text`.
fn grounded(name: &str, text: &str) -> bool {
    let slug = name.rsplit('/').next().unwrap_or(name);
    let said: Vec<String> = words(text).collect();
    content_words(slug)
        .iter()
        .filter(|w| !GENERIC.contains(&w.as_str()) && !PREFIXES.contains(&w.as_str()))
        .any(|w| said.iter().any(|s| same_word(w, s)))
}

/// A work item's title: a Fizzy card's from the Fizzy CLI, a GitHub issue
/// or PR's from gh under the project's profile. None for refs no tracker
/// places (a Linear key, say) or when the lookup fails.
fn item_title(profile: &str, item: &str, tracker: &Tracker) -> Option<String> {
    link_title(profile, &parse_ref(item, tracker)?, tracker)
}

/// A link's title (see `item_title`), for `titles`' cache.
pub(crate) fn link_title(profile: &str, link: &Value, tracker: &Tracker) -> Option<String> {
    let number = link["ref"].as_str()?;
    let title = match link["system"].as_str()? {
        "fizzy" => {
            let mut cmd = Command::new("fizzy");
            cmd.args(["card", "show", number, "--jq", ".data.title"]);
            // The Fizzy CLI's profiles are named after their accounts.
            if let Some(account) = card_account(link).or(tracker.fizzy_account.clone()) {
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
    let rest = link["url"]
        .as_str()?
        .strip_prefix("https://app.fizzy.do/")?;
    rest.split('/')
        .next()
        .filter(|a| !a.is_empty())
        .map(String::from)
}

/// Ask the on-device model to name work described by `text`.
fn suggest(text: &str) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let text: String = text.chars().take(MAX_INPUT_CHARS).collect();
    // `fm` reads the schema from a file; one per process keeps concurrent
    // launches from tripping over each other.
    let schema = std::env::temp_dir().join(format!(
        "switchyard-name-schema-{}.json",
        std::process::id()
    ));
    std::fs::write(&schema, SCHEMA).ok()?;
    let mut cmd = Command::new("fm");
    cmd.args([
        "respond",
        "--no-stream",
        "--greedy",
        "--instructions",
        INSTRUCTIONS,
        "--schema",
    ])
    .arg(&schema);
    // The text goes in on stdin, so one starting with `-` stays text.
    let out = bounded_output(&mut cmd, Some(&text), FM_TIMEOUT);
    let _ = std::fs::remove_file(&schema);
    parse(&out?).filter(|name| grounded(name, &text))
}

/// A work item's title as a feature name, for when the model can't name it.
fn title_name(title: &str) -> Option<String> {
    Some(slug(title)).filter(|s| !s.is_empty() && name_problem(s).is_none())
}

/// A prompt's first words of prose (see `prose`) as a feature name, for
/// when the model can't name it. Line breaks separate words, rather than
/// joining them.
fn prompt_name(prompt: &str) -> Option<String> {
    title_name(&prose(prompt))
}

/// The work name in `fm`'s JSON answer, cleaned up: a known prefix
/// (`feature` otherwise) and a lowercase kebab-case slug, or None when
/// there is no usable slug.
fn parse(answer: &str) -> Option<String> {
    let v: Value = serde_json::from_str(answer.trim()).ok()?;
    let prefix = v["prefix"]
        .as_str()
        .map(|p| p.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let prefix = PREFIXES
        .iter()
        .find(|p| **p == prefix)
        .copied()
        .unwrap_or("feature");
    let slug = slug(v["slug"].as_str()?);
    if slug.is_empty() {
        return None;
    }
    let name = if prefix == "feature" {
        slug
    } else {
        format!("{prefix}/{slug}")
    };
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
        let extra = if slug.is_empty() {
            word.len()
        } else {
            word.len() + 1
        };
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
        assert_eq!(
            parse(r#"{"prefix": "fix", "slug": "archive-during-check-crash"}"#).as_deref(),
            Some("fix/archive-during-check-crash")
        );
        // Key order varies; feature work is named by its slug alone.
        assert_eq!(
            parse(r#"{"slug": "dark-mode-toggle", "prefix": "feature"}"#).as_deref(),
            Some("dark-mode-toggle")
        );
        assert_eq!(
            parse(r#"{"prefix": "Docs", "slug": "README Linux install"}"#).as_deref(),
            Some("docs/readme-linux-install")
        );
        // An unknown prefix falls back to feature.
        assert_eq!(
            parse(r#"{"prefix": "bugfix", "slug": "x"}"#).as_deref(),
            Some("x")
        );
        assert_eq!(
            parse(r#"{"prefix": "fix", "slug": "a/b.c--d_e"}"#).as_deref(),
            Some("fix/a-b-c-d-e")
        );
    }

    #[test]
    fn unusable_answers_give_no_name() {
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"prefix": "fix"}"#), None);
        assert_eq!(parse(r#"{"prefix": "fix", "slug": "!!! ---"}"#), None);
    }

    #[test]
    fn titles_name_work_the_model_cannot() {
        assert_eq!(
            title_name("Archive: crashes while a check runs!").as_deref(),
            Some("archive-crashes-while-a-check-runs")
        );
        assert_eq!(title_name("!!!"), None);
    }

    #[test]
    fn prompts_name_work_the_model_cannot() {
        assert_eq!(
            prompt_name("when i archive a session the item stays in the list\nwhile it works")
                .as_deref(),
            Some("when-i-archive-a-session-the-item-stays")
        );
        assert_eq!(prompt_name("fix\nthis").as_deref(), Some("fix-this"));
        assert_eq!(prompt_name("日本語"), None);
    }

    #[test]
    fn prompts_need_words_of_their_own_to_name_work() {
        let slack = "- read this: https://example.slack.com/archives/C08/p17?thread_ts=1.5&cid=C08\n\
                     - /agent-kit:issue-create";
        assert!(!nameable(slack));
        assert!(!nameable("hello"));
        assert!(!nameable(
            "```\nfn main() { panic!(\"tab picker\") }\n```\nlook"
        ));
        assert!(!nameable("@crates/core/src/lib.rs ~/notes/x.md"));
        assert!(nameable("the tab picker hangs"));
        assert!(nameable("fix login"));
        assert!(nameable(
            "```\n/home/x/session.jsonl\n```\n\non drummer -- what is this session's title?"
        ));
        assert_eq!(
            name_work("personal", None, &Tracker::default(), slack),
            None
        );
    }

    #[test]
    fn prompt_names_skip_links_paths_and_code() {
        assert_eq!(
            prompt_name("see https://x.y/z and `@a/b.rs`\n```\ncode here\n```\nthe picker hangs")
                .as_deref(),
            Some("see-and-the-picker-hangs")
        );
    }

    #[test]
    fn names_must_echo_the_text_not_the_instructions() {
        let slack =
            "- read this: https://example.slack.com/archives/C08\n- /agent-kit:issue-create";
        assert!(!grounded("fix/crash-fix", slack));
        assert!(!grounded("fix/login-timeout-issue", "hello"));
        assert!(!grounded("fix/csv-report-export", "the tab picker hangs"));
        assert!(grounded("fix/tab-picker-hangs", "the tab picker hangs"));
        assert!(grounded("fix/picker-freeze", "the tab picker hangs"));
        assert!(grounded(
            "fix/archive-during-check",
            "archiving while a check runs"
        ));
        assert!(grounded("fix/hang-on-archive", "it hangs"));
        // A crash the text names is the text's own word.
        assert!(grounded("fix/archive-crash", "a crash on archive"));
        assert!(!grounded("fix/crash", "a crash on archive"));
    }

    #[test]
    fn same_word_matches_stems() {
        assert!(same_word("hangs", "hang"));
        assert!(same_word("archive", "archiving"));
        assert!(!same_word("crash", "create"));
        assert!(!same_word("ab", "abc"));
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
    #[ignore = "asks the real on-device model and Fizzy"]
    fn live_names_from_prompt_and_card() {
        let fizzy = Tracker::new("fizzy:6284043/board", "");
        for prompt in [
            "the tab picker crashes when you archive a task while its check is still running",
            "-n starts with a dash",
        ] {
            let name = name_work("personal", None, &fizzy, prompt);
            eprintln!("{prompt} -> {name:?}");
            assert!(name.is_some());
        }
        // Nothing of its own to name it from: no name rather than a made-up one.
        let slack = "- read this: https://example.slack.com/archives/C08/p17?thread_ts=1.5\n\
                     - /agent-kit:issue-create";
        assert_eq!(suggest_name(slack), None);
        let named = suggest_name("the tab picker hangs when archiving a task");
        eprintln!("suggested {named:?}");
        assert!(named.is_some());
        let title = item_title("personal", "#1056", &fizzy);
        eprintln!(
            "#1056 -> {title:?} -> {:?}",
            name_work("personal", Some("#1056"), &fizzy, "")
        );
        assert!(title.is_some());
    }

    #[test]
    fn card_links_carry_their_account() {
        let link = parse_ref(
            "https://app.fizzy.do/6284043/cards/1056",
            &Tracker::default(),
        )
        .unwrap();
        assert_eq!(card_account(&link).as_deref(), Some("6284043"));
    }
}
