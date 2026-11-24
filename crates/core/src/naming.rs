//! Work names. Naming follows the old herdr short-name convention: `api-work`
//! becomes branch `feature/api-work` in `.worktrees/api-work` with tab
//! `api-work`; a name with a slash is used as the branch as is, its slug
//! replacing `/` with `-`.

/// The agent harnesses work can run in; the first is the default where the
/// config's `[harnesses]` names none (see `default_harness`).
pub const HARNESSES: [&str; 3] = ["claude", "pi", "agy"];

/// The models work can pick for `harness`; the first, empty, is the
/// harness's own default. Claude's are its aliases for the latest of each
/// family; pi's name their provider so the pattern is unambiguous;
/// Antigravity's (`agy`) are IDs from `agy models`.
#[must_use]
pub fn models(harness: &str) -> &'static [&'static str] {
    match harness {
        "claude" => &["", "fable", "opus", "sonnet", "haiku"],
        "pi" => &[
            "",
            "subscription-codex/gpt-6.1-sol",
            "subscription-codex/gpt-6-astra",
            "subscription-codex/gpt-6-luna",
            "anthropic/claude-opus-5-5",
            "anthropic/claude-sonnet-5-5",
        ],
        "agy" => &[
            "",
            "gemini-4-neon",
            "gemini-4-neon-xhigh",
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-low",
            "claude-opus-4-6-thinking",
            "claude-sonnet-4-6",
        ],
        _ => &[""],
    }
}

/// A launch's harness argument, `HARNESS` or `HARNESS:MODEL`, as the
/// harness and its model (None for the harness default). Only the first
/// colon splits: pi models may carry their own (`sonnet:high`).
#[must_use]
pub fn split_harness(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once(':') {
        Some((harness, model)) if !model.is_empty() => (harness, Some(model)),
        Some((harness, _)) => (harness, None),
        None => (spec, None),
    }
}

/// The harness argument for `harness` running `model` (see `split_harness`).
#[must_use]
pub fn harness_spec(harness: &str, model: Option<&str>) -> String {
    match model.filter(|m| !m.is_empty()) {
        Some(model) => format!("{harness}:{model}"),
        None => harness.to_string(),
    }
}

/// The default `HARNESS[:MODEL]` for new work under `profile`: the
/// config's `[harnesses]` entry for it, or the first harness on its own
/// default model when it has none, the config can't be read, or the entry
/// names an unknown harness. An unknown model falls back to the harness's
/// own default.
#[must_use]
pub fn default_harness(profile: &str) -> String {
    match crate::config::load() {
        Ok(config) => default_harness_in(&config.harnesses, profile),
        Err(_) => HARNESSES[0].to_string(),
    }
}

/// `default_harness` under `harnesses`, a config's `[harnesses]` table.
#[must_use]
pub fn default_harness_in<S: std::hash::BuildHasher>(
    harnesses: &std::collections::HashMap<String, String, S>,
    profile: &str,
) -> String {
    known_harness(harnesses.get(profile).map_or("", String::as_str))
}

/// `spec` if it names a known harness, else the first; with its model only
/// if the harness lists it.
fn known_harness(spec: &str) -> String {
    let (harness, model) = split_harness(spec.trim());
    match HARNESSES.iter().find(|h| **h == harness) {
        Some(h) => harness_spec(h, model.filter(|m| models(h).contains(m))),
        None => HARNESSES[0].to_string(),
    }
}

/// Normalise typed input: lowercase, spaces become dashes, and only
/// characters that are safe in a branch, a path and a tab label survive.
#[must_use]
pub fn name_char(c: char) -> Option<char> {
    match c {
        ' ' | '_' => Some('-'),
        'A'..='Z' => Some(c.to_ascii_lowercase()),
        'a'..='z' | '0'..='9' | '-' | '.' | '/' => Some(c),
        _ => None,
    }
}

/// Why a name can't be used yet, or None when it can.
#[must_use]
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("name the work");
    }
    if name.starts_with(['/', '-', '.'])
        || name.ends_with(['/', '.'])
        || name.contains("//")
        || name.contains("..")
    {
        return Some("not a valid branch name");
    }
    None
}

/// (branch, slug) for a work name.
#[must_use]
pub fn branch_and_slug(name: &str) -> (String, String) {
    if name.contains('/') {
        (name.to_string(), name.replace('/', "-"))
    } else {
        (format!("feature/{name}"), name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_defaults_keep_only_known_harnesses_and_models() {
        assert_eq!(known_harness("agy:gemini-4-neon"), "agy:gemini-4-neon");
        assert_eq!(known_harness("pi"), "pi");
        assert_eq!(known_harness("agy:no-such-model"), "agy");
        assert_eq!(known_harness("nope:opus"), HARNESSES[0]);
        assert_eq!(known_harness(""), HARNESSES[0]);
    }

    #[test]
    fn harness_specs_split_on_their_first_colon() {
        assert_eq!(split_harness("claude"), ("claude", None));
        assert_eq!(split_harness("claude:opus"), ("claude", Some("opus")));
        assert_eq!(split_harness("pi:sonnet:high"), ("pi", Some("sonnet:high")));
        assert_eq!(split_harness("pi:"), ("pi", None));
        assert_eq!(harness_spec("pi", Some("")), "pi");
        assert_eq!(harness_spec("claude", Some("opus")), "claude:opus");
    }

    #[test]
    fn every_harness_lists_its_default_first() {
        for harness in HARNESSES {
            assert_eq!(models(harness)[0], "", "{harness}");
        }
    }
}
