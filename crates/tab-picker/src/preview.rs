//! The preview in the All view: what the highlighted agent has been
//! saying, from its session log (see core's `transcript`).
//!
//! In the float, the fill script runs `switchyard transcript` for the tab
//! the title names and pastes the result as `preview`; standalone, a thread
//! here fetches it for the tab the picker asks for (see `follow`). Either
//! way the paste body is `TARGET/SESSION/WINDOW`, a newline, then the
//! transcript JSON, and the picker shows it only while that tab's row is
//! highlighted. Fetches wait for the highlight to settle and repeat every
//! REFRESH while it stays, one at a time. The picker wraps the entries to
//! the preview's width by word and scrolls them.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use switchyard_ui::Palette;
use switchyard_ui::draw::str_width;
use switchyard_ui::protocol::DATA;

/// How often a highlighted agent's transcript is fetched again.
pub const REFRESH: Duration = Duration::from_secs(2);

/// How long the highlight must rest before a fetch.
const SETTLE: Duration = Duration::from_millis(150);

/// A tab, as the title names it: TARGET (`-` for the config host, else the
/// hex-encoded host label), session and window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub target: String,
    pub session: String,
    pub window: String,
}

impl Pane {
    fn header(&self) -> String {
        format!("{}/{}/{}", self.target, self.session, self.window)
    }
}

/// A transcript: its entries as (kind, text), or why there are none, and
/// its lines as last drawn, by width.
pub struct Preview {
    pub pane: Pane,
    entries: Result<Vec<(String, String)>, String>,
    drawn: Option<(u16, Vec<Line<'static>>)>,
}

impl Preview {
    /// The entries wrapped to `width`, or why there are none.
    pub fn lines(&mut self, width: u16, p: &Palette) -> Result<&[Line<'static>], &str> {
        let entries = self.entries.as_ref().map_err(String::as_str)?;
        if self.drawn.as_ref().is_none_or(|(w, _)| *w != width) {
            self.drawn = Some((width, render(entries, width as usize, p)));
        }
        Ok(self.drawn.as_ref().map(|(_, lines)| lines.as_slice()).unwrap_or_default())
    }
}

/// A `preview` paste body.
pub fn parse(body: &str) -> Option<Preview> {
    let (header, json) = body.split_once('\n')?;
    let mut f = header.splitn(3, '/');
    let pane = Pane { target: f.next()?.into(), session: f.next()?.into(), window: f.next()?.trim_end().into() };
    let entries = match serde_json::from_str::<serde_json::Value>(json) {
        Ok(v) => match v["error"].as_str() {
            Some(e) => Err(e.to_string()),
            None => Ok(v["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| (e["kind"].as_str().unwrap_or("").to_string(), e["text"].as_str().unwrap_or("").to_string()))
                .collect()),
        },
        Err(_) => Err("unreadable transcript".to_string()),
    };
    Some(Preview { pane, entries, drawn: None })
}

/// The entries as lines `width` wide: John's messages after `›`, the
/// agent's after `●`, its tool calls dim, one line each, and questions for
/// John in gold. A blank line goes between turns, not between tool calls.
fn render(entries: &[(String, String)], width: usize, p: &Palette) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut last = "";
    for (kind, text) in entries {
        let (mark, mark_style, style) = match kind.as_str() {
            "user" => ("› ", Style::new().fg(p.iris).add_modifier(Modifier::BOLD), Style::new().fg(p.text)),
            "ask" => ("? ", Style::new().fg(p.gold).add_modifier(Modifier::BOLD), Style::new().fg(p.gold)),
            "tool" => ("· ", Style::new().fg(p.muted), Style::new().fg(p.subtle)),
            _ => ("● ", Style::new().fg(p.pine), Style::new().fg(p.text)),
        };
        if !lines.is_empty() && !(kind == "tool" && last == "tool") {
            lines.push(Line::default());
        }
        last = kind;
        let room = width.saturating_sub(2).max(1);
        if kind == "tool" {
            lines.push(Line::from(vec![Span::styled(mark, mark_style), Span::styled(clip_width(text, room), style)]));
            continue;
        }
        for (i, row) in wrap(text, room).into_iter().enumerate() {
            let lead = if i == 0 { Span::styled(mark, mark_style) } else { Span::raw("  ") };
            lines.push(Line::from(vec![lead, Span::styled(row, style)]));
        }
    }
    lines
}

fn char_width(c: char) -> usize {
    str_width(c.encode_utf8(&mut [0; 4]))
}

/// `text` wrapped by word to `width` columns, keeping its own line breaks
/// and indentation; a word wider than the width is broken.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut indent: String = para.chars().take_while(|c| *c == ' ').collect();
        if indent.len() * 2 > width {
            indent.clear();
        }
        let mut line = String::new();
        let mut used = 0;
        for word in para.split_whitespace() {
            if used > 0 && used + 1 + str_width(word) > width {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            if used == 0 {
                line.push_str(&indent);
                used = indent.len();
            } else {
                line.push(' ');
                used += 1;
            }
            for c in word.chars() {
                // A word longer than a line: break it.
                if used + char_width(c) > width && used > indent.len() {
                    out.push(std::mem::take(&mut line));
                    line.push_str(&indent);
                    used = indent.len();
                }
                line.push(c);
                used += char_width(c);
            }
        }
        out.push(line);
    }
    out
}

fn clip_width(s: &str, room: usize) -> String {
    if str_width(s) <= room {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        if used + char_width(c) + 1 > room {
            break;
        }
        out.push(c);
        used += char_width(c);
    }
    out.push('…');
    out
}

/// Fetch the transcript the standalone picker wants (sent on `wants` as
/// it changes), once the highlight settles and every REFRESH after,
/// sending each as a paste on `tx`; returns once the picker has gone.
pub fn follow(wants: Receiver<Option<Pane>>, tx: Sender<String>) {
    let mut want: Option<Pane> = None;
    let mut last: Option<Instant> = None;
    loop {
        let wait = match (&want, last) {
            (Some(_), Some(at)) => REFRESH.saturating_sub(at.elapsed()),
            _ => Duration::from_secs(3600),
        };
        match wants.recv_timeout(wait) {
            Ok(w) => {
                want = w;
                // Let the highlight settle.
                loop {
                    match wants.recv_timeout(SETTLE) {
                        Ok(w) => want = w,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some(pane) = &want else {
            last = None;
            continue;
        };
        let json = switchyard_core::transcript::fetch(&pane.target, &pane.session, &pane.window);
        if tx.send(format!("{DATA}preview\n{}\n{json}", pane.header())).is_err() {
            return;
        }
        last = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wrapping_goes_by_word_and_keeps_breaks_and_indents() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("a\n  - b c d", 6), ["a", "  - b", "  c d"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn a_transcript_draws_turns_apart_and_tool_calls_together() {
        let json = serde_json::json!({ "harness": "claude", "entries": [
            { "kind": "user", "text": "Fix it" },
            { "kind": "tool", "text": "Bash: Run the tests" },
            { "kind": "tool", "text": "Read: /a/very/long/path/that/goes/on" },
            { "kind": "assistant", "text": "Done and tested" },
        ]});
        let mut pv = parse(&format!("-/s/w\n{json}")).unwrap();
        let p = Palette::load();
        let lines: Vec<String> = pv.lines(14, &p).unwrap().iter().map(text).collect();
        assert_eq!(lines, ["› Fix it", "", "· Bash: Run t…", "· Read: /a/ve…", "", "● Done and", "  tested"]);
    }

    #[test]
    fn a_failed_fetch_says_why() {
        let mut pv = parse("6869/s/w\n{\"error\":\"no agent in this tab\"}").unwrap();
        assert_eq!(pv.lines(20, &Palette::load()).unwrap_err(), "no agent in this tab");
    }
}
