//! Work item titles for a task's links: an issue's, PR's or card's title
//! from its tracker (see autoname's `link_title`: the Fizzy CLI for cards,
//! gh under the project's profile for GitHub). Titles are live fields, as a
//! PR's state is (see `links`): added to each link as `title` on read and
//! never saved to the registry.
//!
//! Lookups happen only in `reconcile_tasks` (`refresh`), never in
//! `live_tasks` or on a redraw; both add what the cache holds (`apply`).
//! The cache is `$XDG_STATE_HOME/rex/switchyard-titles.json`. A title stays
//! fresh for `TITLE_TTL_SECS`, since titles rarely change, and an old one is
//! still shown until it is looked up again. A lookup that fails (no CLI, no
//! auth, a timeout, a ref no tracker places) keeps the title it had (a
//! title, unlike a card's state, says nothing about whether work remains),
//! is logged (see `rex::diagnose`), and is not retried for `MISS_TTL_SECS`,
//! so a slow tracker doesn't hold up every check.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::autoname::link_title;
use crate::links::Tracker;
use crate::rex::{diagnose, home, now};

/// How long a title stays fresh.
const TITLE_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// How long a failed lookup waits before it is tried again.
const MISS_TTL_SECS: u64 = 10 * 60;
/// Entries no check has refreshed in this long are dropped.
const KEEP_SECS: u64 = 30 * 24 * 60 * 60;

/// A link to title: the task's profile (for gh) and tracker (for a card's
/// Fizzy account).
#[derive(Clone)]
pub struct Ask {
    pub profile: String,
    pub link: Value,
    pub tracker: Tracker,
}

/// The cache key for a link, or None for links without a title (commits)
/// or without a ref.
fn key(link: &Value) -> Option<String> {
    let system = link["system"].as_str().filter(|s| !s.is_empty())?;
    if link["kind"] == "commit" {
        return None;
    }
    let url = link["url"].as_str().unwrap_or("");
    let id = if url.is_empty() {
        link["ref"].as_str().filter(|r| !r.is_empty())?
    } else {
        url
    };
    Some(format!("{system}\t{id}"))
}

fn cache_path() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map_or_else(|_| home().join(".local/state"), PathBuf::from)
        .join("rex/switchyard-titles.json")
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

/// Whether a cache entry needs looking up again at `stamp`.
fn stale(entry: &Value, stamp: u64) -> bool {
    let Some(at) = entry["at"].as_u64() else {
        return true;
    };
    let miss = entry["miss"] == true || entry["title"].as_str().is_none_or(str::is_empty);
    stamp.saturating_sub(at) >= if miss { MISS_TTL_SECS } else { TITLE_TTL_SECS }
}

/// Look up the titles `asks` need that the cache lacks or holds stale, all
/// at once, and save them. Failures are cached as blank.
pub fn refresh(asks: Vec<Ask>) {
    let mut cache = load_cache();
    let lookup = |a: &Ask| {
        let title = link_title(&a.profile, &a.link, &a.tracker);
        if title.is_none() {
            let id = key(&a.link).unwrap_or_default().replace('\t', " ");
            diagnose(&format!("title lookup failed for {id}"));
        }
        title
    };
    if refresh_cache(&mut cache, asks, now(), lookup) {
        save_cache(&cache);
    }
}

/// `refresh` on a cache in memory, with `lookup` asking the tracker. True
/// when the cache changed.
fn refresh_cache<F>(cache: &mut Value, asks: Vec<Ask>, stamp: u64, lookup: F) -> bool
where
    F: Fn(&Ask) -> Option<String> + Sync,
{
    let mut wanted: Vec<(String, Ask)> = Vec::new();
    for ask in asks {
        let Some(k) = key(&ask.link) else { continue };
        if wanted.iter().any(|(w, _)| *w == k) || !stale(&cache["titles"][&k], stamp) {
            continue;
        }
        wanted.push((k, ask));
    }
    if wanted.is_empty() {
        return false;
    }
    let found: Vec<(String, Option<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = wanted
            .iter()
            .map(|(k, ask)| (k.clone(), s.spawn(|| lookup(ask))))
            .collect();
        handles
            .into_iter()
            .map(|(k, h)| (k, h.join().ok().flatten()))
            .collect()
    });
    if !cache["titles"].is_object() {
        cache["titles"] = json!({});
    }
    for (k, title) in found {
        let title = title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        cache["titles"][&k] = match title {
            Some(title) => json!({"at": stamp, "title": title}),
            // A failed lookup keeps the title it had, and is retried as a
            // miss.
            None => {
                json!({"at": stamp, "title": cache["titles"][&k]["title"].as_str().unwrap_or(""), "miss": true})
            }
        };
    }
    if let Some(titles) = cache["titles"].as_object_mut() {
        titles.retain(|_, e| {
            e["at"]
                .as_u64()
                .is_some_and(|at| stamp.saturating_sub(at) < KEEP_SECS)
        });
    }
    true
}

/// Add the cached titles to the tasks' links (not saved).
pub fn apply(tasks: &mut [Value]) {
    let cache = load_cache();
    for task in tasks {
        apply_from(&cache, task);
    }
}

fn apply_from(cache: &Value, task: &mut Value) {
    let Some(links) = task.get_mut("links").and_then(Value::as_array_mut) else {
        return;
    };
    for l in links {
        let Some(k) = key(l) else { continue };
        if let Some(title) = cache["titles"][&k]["title"]
            .as_str()
            .filter(|t| !t.is_empty())
        {
            l["title"] = title.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ask(link: Value) -> Ask {
        Ask {
            profile: "personal".into(),
            link,
            tracker: Tracker::default(),
        }
    }

    fn card(n: &str) -> Value {
        json!({"kind": "card", "system": "fizzy", "ref": n, "url": format!("https://app.fizzy.do/1/cards/{n}")})
    }

    #[test]
    fn keys_skip_commits_and_prefer_urls() {
        assert_eq!(
            key(&card("5")).as_deref(),
            Some("fizzy\thttps://app.fizzy.do/1/cards/5")
        );
        assert_eq!(
            key(&json!({"kind": "issue", "system": "github", "ref": "o/r#4", "url": ""}))
                .as_deref(),
            Some("github\to/r#4")
        );
        assert_eq!(
            key(&json!({"kind": "commit", "system": "github", "ref": "abc"})),
            None
        );
        assert_eq!(
            key(&json!({"kind": "card", "system": "fizzy", "ref": "", "url": ""})),
            None
        );
    }

    #[test]
    fn titles_are_looked_up_once_and_added_on_read() {
        let calls = AtomicUsize::new(0);
        let lookup = |a: &Ask| {
            calls.fetch_add(1, Ordering::SeqCst);
            (a.link["ref"] == "5").then(|| " Work item titles \n".to_string())
        };
        let mut cache = json!({});
        // The same card twice is asked once; the commit not at all.
        let asks = vec![
            ask(card("5")),
            ask(card("5")),
            ask(card("6")),
            ask(json!({"kind": "commit", "system": "github", "ref": "abc"})),
        ];
        assert!(refresh_cache(&mut cache, asks.clone(), 1000, lookup));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Fresh: neither the title nor the miss is asked again.
        assert!(!refresh_cache(&mut cache, asks.clone(), 1000 + 60, lookup));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let mut task = json!({"links": [card("5"), card("6"), {"kind": "commit", "system": "github", "ref": "abc"}]});
        apply_from(&cache, &mut task);
        assert_eq!(task["links"][0]["title"], "Work item titles");
        assert!(task["links"][1]["title"].is_null());
        assert!(task["links"][2]["title"].is_null());
        // The miss is retried after MISS_TTL_SECS, the title only later.
        assert!(refresh_cache(
            &mut cache,
            asks.clone(),
            1000 + MISS_TTL_SECS,
            lookup
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(refresh_cache(
            &mut cache,
            asks,
            1000 + TITLE_TTL_SECS,
            lookup
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn a_failed_lookup_keeps_the_old_title() {
        let mut cache = json!({});
        refresh_cache(&mut cache, vec![ask(card("5"))], 0, |_| Some("Old".into()));
        assert!(refresh_cache(
            &mut cache,
            vec![ask(card("5"))],
            TITLE_TTL_SECS,
            |_| None
        ));
        // Retried as a miss, not a week later.
        assert!(stale(
            &cache["titles"]["fizzy\thttps://app.fizzy.do/1/cards/5"],
            TITLE_TTL_SECS + MISS_TTL_SECS
        ));
        let mut task = json!({"links": [card("5")]});
        apply_from(&cache, &mut task);
        assert_eq!(task["links"][0]["title"], "Old");
    }

    #[test]
    fn tasks_without_links_are_left_alone() {
        let mut task = json!({"name": "x"});
        apply_from(&json!({"titles": {}}), &mut task);
        assert_eq!(task, json!({"name": "x"}));
    }
}
