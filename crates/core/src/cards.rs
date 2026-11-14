//! Live state of a task's linked Fizzy cards: whether each is closed, its
//! column and its assignees, added on read to its card links (`state`
//! `closed` or `open`, `column`, `assignee`) and never saved, as a PR's
//! state and checks are (see `links`).
//!
//! A card is looked up with `fizzy card show NUMBER --json`, with
//! `FIZZY_PROFILE` set to the account in the card's URL (the Fizzy CLI's
//! profiles are named after their accounts), giving up after
//! `FIZZY_TIMEOUT`. Answers, failures included, are cached in
//! `$XDG_STATE_HOME/rex/switchyard-cards.json` for `CARD_TTL_SECS`, so a
//! missing, logged-out or slow fizzy isn't asked again on every refresh.
//! Only `reconcile_tasks` looks cards up.
//!
//! A cache entry is `{at, seen, card, error}`: `at` when Fizzy was last
//! asked, `seen` when it last answered with `card`, and `error` why the
//! last ask failed. A failure keeps the last answer and its `seen` but is
//! never presented as current: the card's state is unknown, so its badge
//! shows just its number, and the failure's reason goes to the
//! diagnostics log (see `rex::diagnose`). A task whose linked cards Fizzy
//! last answered are all closed counts as merged for archive-safety, as one
//! with a merged PR does; a card whose state is unknown blocks it.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

use crate::item_actions::run_bounded;
use crate::rex::{diagnose, home, now};

/// How long a card lookup stays fresh.
const CARD_TTL_SECS: u64 = 120;
/// How long one fizzy call may take before it is given up on.
const FIZZY_TIMEOUT: Duration = Duration::from_secs(10);
/// Cache entries no one has asked about in this long are dropped.
const KEEP_SECS: u64 = 86400;

/// A card as Fizzy reports it, reduced to what the inbox shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub closed: bool,
    /// The column's name: `In Progress`, or a pseudo column such as
    /// `Maybe?`, `Not Now` or `Done`.
    pub column: String,
    /// The assignees' first names, comma separated, or empty.
    pub assignee: String,
}

/// The card in `fizzy card show --json` output (the envelope or bare data).
#[must_use]
pub fn parse_card(json: &str) -> Option<Card> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let card = if v["data"].is_object() {
        &v["data"]
    } else {
        &v
    };
    card["number"].as_u64()?;
    let names: Vec<&str> = card["assignees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["name"].as_str()?.split_whitespace().next())
        .collect();
    let mut assignee = names.join(", ");
    if card["has_more_assignees"].as_bool() == Some(true) {
        assignee.push_str(", …");
    }
    Some(Card {
        closed: card["closed"].as_bool().unwrap_or(false),
        column: card["column"]["name"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string(),
        assignee,
    })
}

/// The account a card link's URL names, or empty when it has none.
fn account(link: &Value) -> String {
    let rest = link["url"]
        .as_str()
        .and_then(|u| u.strip_prefix("https://app.fizzy.do/"))
        .unwrap_or("");
    rest.split('/').next().unwrap_or("").to_string()
}

fn is_card(link: &Value) -> bool {
    link["kind"] == "card"
        && link["system"] == "fizzy"
        && link["ref"].as_str().is_some_and(|r| !r.is_empty())
}

/// The cache key for a card link: `ACCOUNT\tNUMBER`.
fn key(link: &Value) -> String {
    format!("{}\t{}", account(link), link["ref"].as_str().unwrap_or(""))
}

fn cache_path() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map_or_else(|_| home().join(".local/state"), PathBuf::from)
        .join("rex/switchyard-cards.json")
}

fn load_cache() -> Value {
    std::fs::read(cache_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn save_cache(cache: &Value) {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    if std::fs::write(&tmp, cache.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn card_to_json(card: &Card) -> Value {
    json!({"closed": card.closed, "column": card.column, "assignee": card.assignee})
}

fn card_from_json(v: &Value) -> Option<Card> {
    v.is_object().then(|| Card {
        closed: v["closed"].as_bool().unwrap_or(false),
        column: v["column"].as_str().unwrap_or("").to_string(),
        assignee: v["assignee"].as_str().unwrap_or("").to_string(),
    })
}

/// Ask Fizzy for a card; Err says why it couldn't: fizzy not on PATH, a
/// non-zero exit (with its message), a timeout, or output with no card.
fn show(account: &str, number: &str) -> Result<Card, String> {
    let mut cmd = Command::new("fizzy");
    cmd.args(["card", "show", number, "--json"]);
    if !account.is_empty() {
        cmd.env("FIZZY_PROFILE", account);
    }
    let out = run_bounded(&mut cmd, "fizzy", FIZZY_TIMEOUT)?;
    parse_card(&out).ok_or_else(|| "fizzy: no card in its output".to_string())
}

/// The card a cache entry holds as current: Fizzy's answer, unless the
/// latest ask failed. (An entry without `seen` predates failures being
/// told apart, so may hold a failure's leftover; it isn't trusted either.)
fn current(entry: &Value) -> Option<Card> {
    if entry["error"].is_string() || !entry["seen"].is_u64() {
        return None;
    }
    card_from_json(&entry["card"])
}

/// The live state of every card linked to `tasks`, keyed by account and
/// number; a card whose state is unknown (never looked up, or the latest
/// lookup failed) is absent. Fresh cache entries are used as they are; the
/// rest are asked of Fizzy at once, and failures logged.
pub fn find_cards<'a>(tasks: impl IntoIterator<Item = &'a Value>) -> HashMap<String, Card> {
    let stamp = now();
    let mut cache = load_cache();
    let (mut found, due) = from_cache(&cache, tasks, stamp);
    if due.is_empty() {
        return found;
    }
    let asks: Vec<_> = due
        .into_iter()
        .map(|(k, account, number)| std::thread::spawn(move || (k, show(&account, &number))))
        .collect();
    let answers: Vec<(String, Result<Card, String>)> =
        asks.into_iter().filter_map(|h| h.join().ok()).collect();
    for (k, answer) in &answers {
        if let Err(why) = answer {
            diagnose(&format!(
                "card {} lookup failed: {why}",
                k.replace('\t', "/")
            ));
        }
    }
    found.extend(record(&mut cache, answers, stamp));
    save_cache(&cache);
    found
}

/// The cards `tasks` link that the cache answers fresh (at `stamp`), and
/// the rest to ask Fizzy about: key, account and number, once each. A fresh
/// failure is neither: it waits out `CARD_TTL_SECS` unknown.
#[allow(clippy::type_complexity)]
fn from_cache<'a>(
    cache: &Value,
    tasks: impl IntoIterator<Item = &'a Value>,
    stamp: u64,
) -> (HashMap<String, Card>, Vec<(String, String, String)>) {
    let mut found = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut due = Vec::new();
    for link in tasks
        .into_iter()
        .filter_map(|t| t["links"].as_array())
        .flatten()
        .filter(|l| is_card(l))
    {
        let k = key(link);
        if !seen.insert(k.clone()) {
            continue;
        }
        let entry = &cache["cards"][&k];
        if entry["at"]
            .as_u64()
            .is_some_and(|at| stamp.saturating_sub(at) < CARD_TTL_SECS)
        {
            if let Some(card) = current(entry) {
                found.insert(k, card);
            }
        } else {
            due.push((
                k,
                account(link),
                link["ref"].as_str().unwrap_or("").to_string(),
            ));
        }
    }
    (found, due)
}

/// Record Fizzy's `answers` at `stamp` in the cache, dropping long-unasked
/// entries; the cards it answered. A failure is remembered, so fizzy isn't
/// asked again until it goes stale, keeping the card's last answer and when
/// that was (`seen`) but marked with the failure, so it isn't shown or
/// trusted as current.
fn record(
    cache: &mut Value,
    answers: Vec<(String, Result<Card, String>)>,
    stamp: u64,
) -> HashMap<String, Card> {
    if !cache["cards"].is_object() {
        cache["cards"] = json!({});
    }
    let mut found = HashMap::new();
    for (k, answer) in answers {
        cache["cards"][&k] = match answer {
            Ok(card) => {
                let entry = json!({"at": stamp, "seen": stamp, "card": card_to_json(&card)});
                found.insert(k.clone(), card);
                entry
            }
            Err(why) => {
                let previous = &cache["cards"][&k];
                json!({"at": stamp, "seen": previous["seen"], "card": previous["card"], "error": why})
            }
        };
    }
    if let Some(cards) = cache["cards"].as_object_mut() {
        cards.retain(|_, e| {
            e["at"]
                .as_u64()
                .is_some_and(|at| stamp.saturating_sub(at) < KEEP_SECS)
        });
    }
    found
}

/// Drop the cached lookup of one card, after an action changed it (closed
/// it), so the next refresh asks Fizzy again rather than showing its
/// column and state from before for up to `CARD_TTL_SECS`.
pub fn forget(account: &str, number: &str) {
    let mut cache = load_cache();
    if forget_in(&mut cache, account, number) {
        save_cache(&cache);
    }
}

/// `forget` on a cache; whether it dropped an entry.
fn forget_in(cache: &mut Value, account: &str, number: &str) -> bool {
    cache["cards"]
        .as_object_mut()
        .is_some_and(|cards| cards.remove(&format!("{account}\t{number}")).is_some())
}

/// Add each card link's live state from `cards` (not saved).
pub fn add_live<S: BuildHasher>(task: &mut Value, cards: &HashMap<String, Card, S>) {
    let Some(links) = task["links"].as_array_mut() else {
        return;
    };
    for link in links.iter_mut().filter(|l| is_card(l)) {
        let Some(card) = cards.get(&key(link)) else {
            continue;
        };
        link["state"] = if card.closed { "closed" } else { "open" }.into();
        link["column"] = card.column.clone().into();
        link["assignee"] = card.assignee.clone().into();
    }
}

/// Whether the task has cards and Fizzy's latest answer for every one of
/// them was closed (from the lookup cache, however old). One open card, or
/// one whose state is unknown (never looked up, or its latest lookup
/// failed), means work may remain.
#[must_use]
pub fn cards_closed(task: &Value) -> bool {
    all_closed(task, &load_cache())
}

fn all_closed(task: &Value, cache: &Value) -> bool {
    let cards: Vec<&Value> = task["links"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| is_card(l))
        .collect();
    !cards.is_empty()
        && cards
            .iter()
            .all(|l| current(&cache["cards"][&key(l)]).is_some_and(|c| c.closed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgetting_a_card_drops_only_its_entry() {
        let mut cache = json!({"cards": {"6284043\t1041": {"at": 1, "card": {"closed": false}},
            "6284043\t1042": {"at": 1, "card": {"closed": false}}}});
        assert!(forget_in(&mut cache, "6284043", "1041"));
        assert!(!forget_in(&mut cache, "6284043", "1041"), "already gone");
        assert!(cache["cards"].get("6284043\t1041").is_none());
        assert!(cache["cards"].get("6284043\t1042").is_some());
    }

    #[test]
    fn parse_card_reads_state_column_and_assignees() {
        let card = parse_card(r#"{"ok":true,"data":{"number":1149,"closed":false,
            "column":{"id":"x","name":"In Progress"},
            "assignees":[{"name":"John Allen"},{"name":"Ada Lovelace"}],"has_more_assignees":true}}"#)
            .unwrap();
        assert_eq!(
            card,
            Card {
                closed: false,
                column: "In Progress".into(),
                assignee: "John, Ada, …".into()
            }
        );
        let card =
            parse_card(r#"{"number":1155,"closed":true,"column":{"name":"Done","pseudo":true}}"#)
                .unwrap();
        assert_eq!(
            card,
            Card {
                closed: true,
                column: "Done".into(),
                assignee: String::new()
            }
        );
        assert!(parse_card("").is_none());
        assert!(parse_card(r#"{"ok":false,"error":"unauthorized"}"#).is_none());
    }

    #[test]
    fn archive_safety_needs_every_card_closed() {
        let card = |n: &str| json!({"kind":"card","system":"fizzy","ref":n,"url":format!("https://app.fizzy.do/9/cards/{n}")});
        let cache = json!({"cards": {
            "9\t1": {"at": 0, "seen": 0, "card": {"closed": true}},
            "9\t2": {"at": 0, "seen": 0, "card": {"closed": false}},
            "9\t3": {"at": 0, "card": null, "error": "fizzy: not found on PATH"},
            "9\t5": {"at": 9, "seen": 0, "card": {"closed": true}, "error": "fizzy took longer than 10s; gave up"},
            "9\t6": {"at": 0, "card": {"closed": true}},
        }});
        assert!(all_closed(&json!({"links": [card("1")]}), &cache));
        assert!(
            !all_closed(&json!({"links": [card("1"), card("2")]}), &cache),
            "one open card"
        );
        assert!(
            !all_closed(&json!({"links": [card("1"), card("3")]}), &cache),
            "one unknown card"
        );
        assert!(
            !all_closed(&json!({"links": [card("5")]}), &cache),
            "closed only before a failed lookup"
        );
        assert!(
            !all_closed(&json!({"links": [card("6")]}), &cache),
            "an entry from before failures were marked"
        );
        assert!(
            !all_closed(&json!({"links": [card("1"), card("4")]}), &cache),
            "one never looked up"
        );
        assert!(!all_closed(&json!({"links": []}), &cache));
        assert!(!all_closed(&json!({}), &cache));
    }

    #[test]
    fn live_state_goes_on_matching_card_links_only() {
        let mut task = json!({"links": [
            {"kind":"card","system":"fizzy","ref":"5","url":"https://app.fizzy.do/99/cards/5"},
            {"kind":"card","system":"fizzy","ref":"6","url":""},
            {"kind":"card","system":"fizzy","ref":"5","url":"https://app.fizzy.do/11/cards/5"},
            {"kind":"pr","system":"github","ref":"o/r#5","url":"p"},
        ]});
        let cards = HashMap::from([
            (
                "99\t5".to_string(),
                Card {
                    closed: true,
                    column: "Done".into(),
                    assignee: String::new(),
                },
            ),
            (
                "\t6".to_string(),
                Card {
                    closed: false,
                    column: "Maybe?".into(),
                    assignee: "John".into(),
                },
            ),
        ]);
        add_live(&mut task, &cards);
        let l = &task["links"];
        assert_eq!(
            (l[0]["state"].as_str(), l[0]["column"].as_str()),
            (Some("closed"), Some("Done"))
        );
        assert_eq!(
            (l[1]["state"].as_str(), l[1]["assignee"].as_str()),
            (Some("open"), Some("John"))
        );
        assert!(l[2]["state"].is_null(), "another account's card 5");
        assert!(l[3]["state"].is_null());
    }

    fn task(n: &str) -> Value {
        json!({"links": [{"kind":"card","system":"fizzy","ref":n,"url":format!("https://app.fizzy.do/9/cards/{n}")}]})
    }

    fn open_card() -> Card {
        Card {
            closed: false,
            column: "Maybe?".into(),
            assignee: String::new(),
        }
    }

    #[test]
    fn a_failed_lookup_is_not_shown_or_trusted_as_current() {
        let mut cache = json!({});
        let found = record(&mut cache, vec![("9\t1156".into(), Ok(open_card()))], 100);
        assert_eq!(found.get("9\t1156"), Some(&open_card()));

        // Fizzy can't run: the old answer stays, with when it was seen, but
        // nothing is found and the failure is kept with its reason.
        let found = record(
            &mut cache,
            vec![("9\t1156".into(), Err("fizzy: not found on PATH".into()))],
            300,
        );
        assert!(found.is_empty());
        let entry = &cache["cards"]["9\t1156"];
        assert_eq!(
            (entry["at"].as_u64(), entry["seen"].as_u64()),
            (Some(300), Some(100))
        );
        assert_eq!(entry["error"], "fizzy: not found on PATH");
        assert_eq!(entry["card"]["column"], "Maybe?");

        // While fresh, the failure isn't asked again, nor shown as open.
        let (found, due) = from_cache(&cache, [&task("1156")], 300 + CARD_TTL_SECS - 1);
        assert!(found.is_empty() && due.is_empty());
        let mut t = task("1156");
        add_live(&mut t, &found);
        assert!(t["links"][0]["state"].is_null(), "just the number");
        // Once stale it is asked again.
        let (_, due) = from_cache(&cache, [&task("1156")], 300 + CARD_TTL_SECS);
        assert_eq!(
            due,
            [("9\t1156".to_string(), "9".to_string(), "1156".to_string())]
        );

        // A later answer clears the failure.
        let closed = Card {
            closed: true,
            column: "Done".into(),
            assignee: String::new(),
        };
        record(
            &mut cache,
            vec![("9\t1156".into(), Ok(closed.clone()))],
            500,
        );
        assert!(cache["cards"]["9\t1156"]["error"].is_null());
        let (found, due) = from_cache(&cache, [&task("1156")], 501);
        assert_eq!((found.get("9\t1156"), due.len()), (Some(&closed), 0));
        assert!(all_closed(&task("1156"), &cache));
    }

    #[test]
    fn a_closed_card_whose_lookup_then_fails_blocks_archive() {
        let mut cache = json!({});
        let closed = Card {
            closed: true,
            column: "Done".into(),
            assignee: String::new(),
        };
        record(&mut cache, vec![("9\t7".into(), Ok(closed))], 0);
        assert!(all_closed(&task("7"), &cache));
        record(
            &mut cache,
            vec![("9\t7".into(), Err("fizzy: unauthorized".into()))],
            CARD_TTL_SECS,
        );
        assert!(!all_closed(&task("7"), &cache), "unknown blocks");
    }

    #[test]
    fn a_first_lookup_that_fails_leaves_the_card_unknown() {
        let mut cache = json!({});
        assert!(
            record(
                &mut cache,
                vec![(
                    "9\t8".into(),
                    Err("fizzy took longer than 10s; gave up".into())
                )],
                50
            )
            .is_empty()
        );
        let entry = &cache["cards"]["9\t8"];
        assert!(entry["seen"].is_null() && entry["card"].is_null());
        assert!(!all_closed(&task("8"), &cache));
        // Each card is asked about once however many tasks link it.
        let (_, due) = from_cache(&json!({}), [&task("8"), &task("8")], 50);
        assert_eq!(due.len(), 1);
    }
}
