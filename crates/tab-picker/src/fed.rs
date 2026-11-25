//! The cmd+k quick jump float away from the config host (`switchyard float
//! feed`, run on the config host by lua/switchyard/tab-picker.lua).
//!
//! The float runs on the focused session's host, but only the config host
//! (the Mac) knows every Rex server and has the task registry. So there the
//! float runs `--quick --fed-by LABEL`, fetches nothing, and this feeds it:
//! the config host's own data (`standalone::Feed`) goes into the float as
//! data pastes, through one `rex do` running `feed_float.lua`, which also
//! finds the float's host. Enter in the float reports its pick in the
//! terminal title (`PICK_TITLE`), which this follows with `rex events`; it
//! acts on the pick here, as the config host's own quick float would, and
//! pastes back `quit` to close the float, or `note` with the failure.
//! It ends when the float has gone.

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use switchyard_core::rex::rex_command;
use switchyard_ui::protocol;

use crate::standalone;

/// Runs on the config host's rex against the float (see the script).
const FEED_FLOAT: &str = include_str!("../../../lua/switchyard/feed_float.lua");

/// The title prefix of a fed float's pick: `switchyard-quick:PICK`, PICK as
/// the picker's `pick` gives it.
pub const PICK_TITLE: &str = "switchyard-quick";

/// How often to check that the float is still there between deliveries.
const PING: Duration = Duration::from_secs(2);

enum Msg {
    Paste(String),
    Pick(String),
    /// The events stream ended: the session or its server has gone.
    Gone,
}

/// Feed the quick jump float `block` in `session`, and act on its picks,
/// until it has gone. Returns at once when the float is on this host.
///
/// # Errors
///
/// Fails when rex can't run or the float can't be found.
pub fn feed(session: &str, block: &str) -> io::Result<()> {
    standalone::detach_env();
    let mut bridge = rex_command()
        .args(["do", "-e", FEED_FLOAT])
        .arg(format!("session={session}"))
        .arg(format!("block={block}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let result = run(&mut bridge, session, block);
    let _ = bridge.kill();
    let _ = bridge.wait();
    result
}

fn run(bridge: &mut Child, session: &str, block: &str) -> io::Result<()> {
    let mut stdin = bridge
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?;
    let stdout = bridge
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line)?;
    let host = match line.trim_end().split_once('\t') {
        Some(("host", host)) => host.to_string(),
        // On this host the float fetches for itself.
        _ if line.trim_end() == "local" => return Ok(()),
        _ => return Err(io::Error::other("can't find the float")),
    };

    let (tx, rx) = mpsc::channel();
    let mut events = follow_picks(&host, session, block, tx.clone())?;
    let feed = standalone::Feed::start(true);
    let paste_tx = tx;
    thread::spawn(move || {
        for text in &feed.rx {
            if paste_tx.send(Msg::Paste(text)).is_err() {
                return;
            }
        }
    });

    let result = loop {
        let sent = match rx.recv_timeout(PING) {
            Ok(Msg::Paste(text)) => match protocol::data(&text) {
                Some((kind, body)) => frame(&mut stdin, kind, body),
                None => Ok(()),
            },
            Ok(Msg::Pick(pick)) => {
                let (kind, body) = match act(&pick) {
                    Ok(()) => ("quit", String::new()),
                    Err(e) => ("note", e),
                };
                frame(&mut stdin, kind, &body)
            }
            Ok(Msg::Gone) | Err(RecvTimeoutError::Disconnected) => break Ok(()),
            Err(RecvTimeoutError::Timeout) => frame(&mut stdin, "ping", ""),
        };
        // The bridge ends once the float has gone, closing its stdin.
        if sent.is_err() {
            break Ok(());
        }
    };
    let _ = events.kill();
    let _ = events.wait();
    result
}

/// One frame for `feed_float.lua`: "KIND LENGTH", then the body.
fn frame(stdin: &mut ChildStdin, kind: &str, body: &str) -> io::Result<()> {
    write!(stdin, "{kind} {}\n{body}", body.len())?;
    stdin.flush()
}

/// What the config host's own quick float does on Enter (see `quick_key`).
fn act(pick: &str) -> Result<(), String> {
    let p = standalone::parse_pick(pick).ok_or_else(|| format!("can't read the pick {pick}"))?;
    if p.action == "resume" {
        standalone::resume(&p)
    } else {
        standalone::go(&p)
    }
}

/// Follow the float's title on its host, sending each pick it reports.
fn follow_picks(host: &str, session: &str, block: &str, tx: Sender<Msg>) -> io::Result<Child> {
    let mut child = rex_command()
        .args(["-S", host, "events", "-s", session, "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let block = block.to_string();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(pick) = pick_in(&line, &block)
                && tx.send(Msg::Pick(pick)).is_err()
            {
                return;
            }
        }
        let _ = tx.send(Msg::Gone);
    });
    Ok(child)
}

/// The pick a `rex events --json` line reports: `block`'s title changing to
/// `PICK_TITLE:PICK`.
fn pick_in(line: &str, block: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let event = &v["payload"]["event"];
    let e = &event["payload"];
    if event["type"] != "block_event" || e["name"] != "title_changed" || e["block_id"] != block {
        return None;
    }
    let pick = e["payload"]["title"]
        .as_str()?
        .strip_prefix(PICK_TITLE)?
        .strip_prefix(':')?;
    (!pick.is_empty()).then(|| pick.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_floats_pick_from_its_title_event() {
        let line = |block: &str, name: &str, title: &str| {
            serde_json::json!({"type": "session_event", "payload": {"session_id": "s", "event": {
                "type": "block_event", "payload": {"block_id": block,
                "creator_name": "com.superlogical.terminal", "name": name,
                "payload": {"title": title}}}}})
            .to_string()
        };
        assert_eq!(
            pick_in(
                &line("b", "title_changed", "switchyard-quick:go:-/s/w"),
                "b"
            ),
            Some("go:-/s/w".into())
        );
        // Another block, another event, another title, or an empty pick.
        assert_eq!(
            pick_in(
                &line("x", "title_changed", "switchyard-quick:go:-/s/w"),
                "b"
            ),
            None
        );
        assert_eq!(
            pick_in(&line("b", "pwd_changed", "switchyard-quick:go:-/s/w"), "b"),
            None
        );
        assert_eq!(pick_in(&line("b", "title_changed", "~"), "b"), None);
        assert_eq!(
            pick_in(&line("b", "title_changed", "switchyard-quick:"), "b"),
            None
        );
    }
}
