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
//! missing, logged-out or slow fizzy costs the inbox's refreshes nothing
//! more and says nothing. Only `reconcile_tasks` looks cards up. A task
//! whose linked cards are all closed counts as merged for archive-safety,
//! as one with a merged PR does.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

use crate::links::bounded_output;
use crate::rex::{home, now};

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
pub fn parse_card(json: &str) -> Option<Card> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let card = if v["data"].is_object() { &v["data"] } else { &v };
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
        column: card["column"]["name"].as_str().unwrap_or("").trim().to_string(),
        assignee,
    })
}

/// The account a card link's URL names, or empty when it has none.
fn account(link: &Value) -> String {
    let rest = link["url"].as_str().and_then(|u| u.strip_prefix("https://app.fizzy.do/")).unwrap_or("");
    rest.split('/').next().unwrap_or("").to_string()
}

fn is_card(link: &Value) -> bool {
    link["kind"] == "card" && link["system"] == "fizzy" && link["ref"].as_str().is_some_and(|r| !r.is_empty())
}

/// The cache key for a card link: `ACCOUNT\tNUMBER`.
fn key(link: &Value) -> String {
    format!("{}\t{}", account(link), link["ref"].as_str().unwrap_or(""))
}

fn cache_path() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local/state"))
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

/// Ask Fizzy for a card; None when fizzy is missing, fails or is slow.
fn show(account: &str, number: &str) -> Option<Card> {
    let mut cmd = Command::new("fizzy");
    cmd.args(["card", "show", number, "--json"]);
    if !account.is_empty() {
        cmd.env("FIZZY_PROFILE", account);
    }
    parse_card(&bounded_output(&mut cmd, None, FIZZY_TIMEOUT)?)
}

/// The live state of every card linked to `tasks`, keyed by account and
/// number; a card Fizzy couldn't tell about is absent. Fresh cache entries
/// are used as they are; the rest are asked of Fizzy at once.
pub fn find_cards<'a>(tasks: impl IntoIterator<Item = &'a Value>) -> HashMap<String, Card> {
    let stamp = now();
    let mut cache = load_cache();
    let mut found = HashMap::new();
    let mut stale: Vec<(String, String, String)> = Vec::new();
    for link in tasks.into_iter().filter_map(|t| t["links"].as_array()).flatten().filter(|l| is_card(l)) {
        let k = key(link);
        if found.contains_key(&k) || stale.iter().any(|(s, _, _)| *s == k) {
            continue;
        }
        let entry = &cache["cards"][&k];
        if entry["at"].as_u64().is_some_and(|at| stamp.saturating_sub(at) < CARD_TTL_SECS) {
            if let Some(card) = card_from_json(&entry["card"]) {
                found.insert(k, card);
            }
        } else {
            stale.push((k, account(link), link["ref"].as_str().unwrap_or("").to_string()));
        }
    }
    if stale.is_empty() {
        return found;
    }
    let asks: Vec<_> = stale
        .into_iter()
        .map(|(k, account, number)| std::thread::spawn(move || (k, show(&account, &number))))
        .collect();
    if !cache["cards"].is_object() {
        cache["cards"] = json!({});
    }
    for handle in asks {
        let Ok((k, card)) = handle.join() else { continue };
        // A failure is remembered too, so fizzy isn't asked again until
        // the entry goes stale, keeping the card's last answer (shown, and
        // used for archive-safety) when it had one.
        let previous = cache["cards"][&k]["card"].clone();
        let saved = card.as_ref().map(card_to_json).unwrap_or(previous);
        if let Some(card) = card_from_json(&saved) {
            found.insert(k.clone(), card);
        }
        cache["cards"][&k] = json!({"at": stamp, "card": saved});
    }
    if let Some(cards) = cache["cards"].as_object_mut() {
        cards.retain(|_, e| e["at"].as_u64().is_some_and(|at| stamp.saturating_sub(at) < KEEP_SECS));
    }
    save_cache(&cache);
    found
}

/// Add each card link's live state from `cards` (not saved).
pub fn add_live(task: &mut Value, cards: &HashMap<String, Card>) {
    let Some(links) = task["links"].as_array_mut() else { return };
    for link in links.iter_mut().filter(|l| is_card(l)) {
        let Some(card) = cards.get(&key(link)) else { continue };
        link["state"] = if card.closed { "closed" } else { "open" }.into();
        link["column"] = card.column.clone().into();
        link["assignee"] = card.assignee.clone().into();
    }
}

/// Whether the task has cards and Fizzy last reported every one of them
/// closed (from the lookup cache, however old). One open or unknown card
/// means work remains.
pub fn cards_closed(task: &Value) -> bool {
    all_closed(task, &load_cache())
}

fn all_closed(task: &Value, cache: &Value) -> bool {
    let cards: Vec<&Value> = task["links"].as_array().into_iter().flatten().filter(|l| is_card(l)).collect();
    !cards.is_empty() && cards.iter().all(|l| cache["cards"][&key(l)]["card"]["closed"].as_bool() == Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_card_reads_state_column_and_assignees() {
        let card = parse_card(r#"{"ok":true,"data":{"number":1149,"closed":false,
            "column":{"id":"x","name":"In Progress"},
            "assignees":[{"name":"John Allen"},{"name":"Ada Lovelace"}],"has_more_assignees":true}}"#)
            .unwrap();
        assert_eq!(card, Card { closed: false, column: "In Progress".into(), assignee: "John, Ada, …".into() });
        let card = parse_card(r#"{"number":1155,"closed":true,"column":{"name":"Done","pseudo":true}}"#).unwrap();
        assert_eq!(card, Card { closed: true, column: "Done".into(), assignee: String::new() });
        assert!(parse_card("").is_none());
        assert!(parse_card(r#"{"ok":false,"error":"unauthorized"}"#).is_none());
    }

    #[test]
    fn archive_safety_needs_every_card_closed() {
        let card = |n: &str| json!({"kind":"card","system":"fizzy","ref":n,"url":format!("https://app.fizzy.do/9/cards/{n}")});
        let cache = json!({"cards": {
            "9\t1": {"at": 0, "card": {"closed": true}},
            "9\t2": {"at": 0, "card": {"closed": false}},
            "9\t3": {"at": 0, "card": null},
        }});
        assert!(all_closed(&json!({"links": [card("1")]}), &cache));
        assert!(!all_closed(&json!({"links": [card("1"), card("2")]}), &cache), "one open card");
        assert!(!all_closed(&json!({"links": [card("1"), card("3")]}), &cache), "one unknown card");
        assert!(!all_closed(&json!({"links": [card("1"), card("4")]}), &cache), "one never looked up");
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
            ("99\t5".to_string(), Card { closed: true, column: "Done".into(), assignee: String::new() }),
            ("\t6".to_string(), Card { closed: false, column: "Maybe?".into(), assignee: "John".into() }),
        ]);
        add_live(&mut task, &cards);
        let l = &task["links"];
        assert_eq!((l[0]["state"].as_str(), l[0]["column"].as_str()), (Some("closed"), Some("Done")));
        assert_eq!((l[1]["state"].as_str(), l[1]["assignee"].as_str()), (Some("open"), Some("John")));
        assert!(l[2]["state"].is_null(), "another account's card 5");
        assert!(l[3]["state"].is_null());
    }
}
