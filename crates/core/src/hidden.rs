//! Hidden tasks: an agent in a detached Rex block, out of every tab, shown
//! in a tab of its own on demand and hidden again. Launches start hidden
//! by default (see `launch::Placement`).
//!
//! A hidden task keeps its block ID where a task keeps its window ID (the
//! registry's `window_id`, a pick's window): Rex window and block IDs have
//! distinct `window:` and `block:` prefixes, so the two never collide, and a
//! block ID survives the block moving between a tab and no tab. Its block's
//! label starts with [`LABEL`], so the host listings (`host_rows.lua`,
//! `WINDOWS_SCRIPT`) list it by its block, shown or hidden.

use crate::rex::{rex, rex_json};

/// The label prefix that marks a hidden task's block; the task's slug
/// follows it after a colon.
pub const LABEL: &str = "switchyard-hidden";

/// Whether `id`, a task's `window_id`, is a hidden task's block.
#[must_use]
pub fn is_block(id: &str) -> bool {
    id.starts_with("block:")
}

fn call(host: &str, method: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    rex_json(host, &["api", "call", method, &args.to_string()])
}

/// `block` on `host`, as `session.list_blocks` lists it.
fn find(host: &str, session: &str, block: &str) -> Result<serde_json::Value, String> {
    let list = call(
        host,
        "session.list_blocks",
        &serde_json::json!({ "session_id": session }),
    )?;
    list["blocks"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["block_id"].as_str() == Some(block)))
        .cloned()
        .ok_or_else(|| format!("{block} is gone"))
}

/// The window `block` is placed in on `host`, or None while it is detached.
///
/// # Errors
///
/// Fails when the session's blocks can't be listed or `block` isn't one.
pub fn window_of(host: &str, session: &str, block: &str) -> Result<Option<String>, String> {
    Ok(find(host, session, block)?["window_id"]
        .as_str()
        .map(str::to_string))
}

/// The tab name for a hidden task's block: its slug, from its label.
fn slug(found: &serde_json::Value) -> &str {
    found["label"]
        .as_str()
        .and_then(|l| l.strip_prefix(LABEL))
        .and_then(|l| l.strip_prefix(':'))
        .filter(|s| !s.is_empty())
        .unwrap_or("hidden")
}

/// Hide the agent just opened in `window` (its only block): label its block
/// and detach it, which closes the window. Returns the block's ID, the
/// task's key from now on.
///
/// # Errors
///
/// Fails when the window's block can't be found, labelled, or detached.
pub fn hide_new(host: &str, session: &str, window: &str, slug: &str) -> Result<String, String> {
    let view = call(
        host,
        "session.view",
        &serde_json::json!({ "session_id": session }),
    )?;
    let block = view["windows"]
        .as_array()
        .and_then(|ws| ws.iter().find(|w| w["window_id"].as_str() == Some(window)))
        .and_then(|w| w["focused_block_id"].as_str())
        .ok_or("the new tab has no block to hide")?
        .to_string();
    call(
        host,
        "session.set_block_label",
        &serde_json::json!({
            "session_id": session, "block_id": block, "label": format!("{LABEL}:{slug}"),
        }),
    )?;
    hide(host, session, &block)?;
    Ok(block)
}

/// Take `block` out of its tab; the tab closes with it. A detached block
/// stays detached.
///
/// # Errors
///
/// Fails when Rex refuses the detach.
pub fn hide(host: &str, session: &str, block: &str) -> Result<(), String> {
    call(
        host,
        "session.detach_block",
        &serde_json::json!({ "session_id": session, "block_id": block }),
    )
    .map(|_| ())
}

/// Put `block` in a tab of its own, unless it is in one already, and return
/// that tab's window. Rex places a detached block only beside another, so a
/// placeholder shell opens the tab and closes once the block is beside it.
///
/// # Errors
///
/// Fails when the block is gone or Rex refuses a step; a failed move closes
/// the placeholder tab again.
pub fn show(host: &str, session: &str, block: &str) -> Result<String, String> {
    let found = find(host, session, block)?;
    if let Some(window) = found["window_id"].as_str() {
        return Ok(window.to_string());
    }
    let created = rex_json(
        host,
        &[
            "window",
            "new",
            "--focus=false",
            "-s",
            session,
            slug(&found),
            "--json",
        ],
    )?;
    let window = created["window_id"]
        .as_str()
        .ok_or("rex window new returned no window_id")?
        .to_string();
    let placeholder = created["block_ids"][0]
        .as_str()
        .ok_or("rex window new returned no block")?
        .to_string();
    if let Err(e) = call(
        host,
        "session.move_block",
        &serde_json::json!({
            "session_id": session, "block_id": block, "anchor_block_id": placeholder,
        }),
    ) {
        let _ = rex(host, &["window", "close", "-s", session, &window]);
        return Err(e);
    }
    close(host, session, &placeholder)?;
    Ok(window)
}

/// Show `block` if it is hidden, or hide it if it is shown; Ok says which.
///
/// # Errors
///
/// Fails as `show` and `hide` do.
pub fn toggle(host: &str, session: &str, block: &str) -> Result<bool, String> {
    if window_of(host, session, block)?.is_some() {
        hide(host, session, block)?;
        Ok(false)
    } else {
        show(host, session, block)?;
        Ok(true)
    }
}

/// Close `block`, ending its agent, as closing its tab would.
///
/// # Errors
///
/// Fails when Rex refuses the close.
pub fn close(host: &str, session: &str, block: &str) -> Result<(), String> {
    call(
        host,
        "block.close",
        &serde_json::json!({ "session_id": session, "block_id": block }),
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_windows_are_told_apart() {
        assert!(is_block("block:01a1e5b5-5f4d-74b3-9b05-f23f3d7836b1"));
        assert!(!is_block("window:01a1e5b5-7bb1-7659-a387-7ed50775bafd"));
        assert!(!is_block(""));
    }

    #[test]
    fn a_shown_block_is_named_by_its_slug() {
        let named = serde_json::json!({ "label": "switchyard-hidden:debug-auth" });
        assert_eq!(slug(&named), "debug-auth");
        assert_eq!(slug(&serde_json::json!({ "label": "shell" })), "hidden");
        assert_eq!(slug(&serde_json::json!({})), "hidden");
    }
}
