//! Work names. Naming follows the old herdr short-name convention: `api-work`
//! becomes branch `feature/api-work` in `.worktrees/api-work` with tab
//! `api-work`; a name with a slash is used as the branch as is, its slug
//! replacing `/` with `-`.

/// The agent harnesses work can run in; the first is the default.
pub const HARNESSES: [&str; 2] = ["claude", "pi"];

/// The models work can pick for `harness`; the first, empty, is the
/// harness's own default. Claude's are its aliases for the latest of each
/// family; pi's name their provider so the pattern is unambiguous.
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
