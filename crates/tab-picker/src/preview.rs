//! The preview in the Inbox view: what the highlighted agent has been
//! saying, from its session log (see core's `transcript`).
//!
//! A thread here fetches it for the tab the picker asks for (see `follow`)
//! and sends it as a `preview` paste: `TARGET/SESSION/WINDOW`, a newline,
//! then the transcript JSON, and the picker shows it only while that tab's row is
//! highlighted. Fetches wait for the highlight to settle and repeat every
//! REFRESH while it stays, one at a time. The picker wraps the entries to
//! the preview's width by word and scrolls them.

use std::ops::Range;
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

/// A tab, as a pick names it: TARGET (`-` for the config host, else the
/// hex-encoded host label), session and window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub target: String,
    pub session: String,
    pub window: String,
}

/// Where an agent runs: its host, profile, harness and folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Agent {
    pub host: String,
    pub profile: String,
    pub harness: String,
    pub dir: String,
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
    /// Where the agent runs, when the transcript says (for the reply box's
    /// slash commands).
    pub agent: Option<Agent>,
    entries: Result<Vec<(String, String)>, String>,
    drawn: Option<(u16, Drawn)>,
}

/// Drawn lines, and for each the entry and the bytes of its text it was
/// drawn from (none for the blank lines between turns and blocks).
#[derive(Default)]
struct Drawn {
    lines: Vec<Line<'static>>,
    from: Vec<Option<(usize, Range<usize>)>>,
}

impl Drawn {
    fn push(&mut self, line: Line<'static>, from: Option<(usize, Range<usize>)>) {
        self.lines.push(line);
        self.from.push(from);
    }
}

impl Preview {
    /// The entries wrapped to `width`, or why there are none.
    pub fn lines(&mut self, width: u16, p: &Palette) -> Result<&[Line<'static>], &str> {
        let entries = self.entries.as_ref().map_err(String::as_str)?;
        if self.drawn.as_ref().is_none_or(|(w, _)| *w != width) {
            self.drawn = Some((width, render(entries, width as usize, p)));
        }
        Ok(self.drawn.as_ref().map(|(_, d)| d.lines.as_slice()).unwrap_or_default())
    }

    /// The markdown lines `a..=b` (as last drawn) came from: for each entry
    /// they touch, its text from the first block they touch to the last,
    /// entries a blank line apart. A block is a paragraph, heading, table,
    /// code block, quote or top-level list item, so part of one copies all
    /// of it.
    pub fn source(&self, a: usize, b: usize) -> Option<String> {
        let entries = self.entries.as_ref().ok()?;
        let (_, drawn) = self.drawn.as_ref()?;
        let mut spans: Vec<(usize, Range<usize>)> = Vec::new();
        for (e, r) in drawn.from.get(a..=b.min(drawn.from.len().checked_sub(1)?))?.iter().flatten() {
            match spans.last_mut() {
                Some((last, span)) if last == e => *span = span.start.min(r.start)..span.end.max(r.end),
                _ => spans.push((*e, r.clone())),
            }
        }
        let parts: Vec<&str> = spans.iter().map(|(e, r)| entries[*e].1[r.clone()].trim_end()).collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }
}

/// A `preview` paste body.
pub fn parse(body: &str) -> Option<Preview> {
    let (header, json) = body.split_once('\n')?;
    let mut f = header.splitn(3, '/');
    let pane = Pane { target: f.next()?.into(), session: f.next()?.into(), window: f.next()?.trim_end().into() };
    let v = serde_json::from_str::<serde_json::Value>(json);
    let agent = v.as_ref().ok().and_then(|v| {
        let a = &v["agent"];
        let f = |k: &str| a[k].as_str().map(String::from);
        Some(Agent { host: f("host")?, profile: f("profile")?, harness: f("harness")?, dir: f("dir")? })
    });
    let entries = match v {
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
    Some(Preview { pane, agent, entries, drawn: None })
}

/// The entries as lines `width` wide: John's messages after `›` on a
/// full-width band, the agent's after `●`, both drawn from markdown, its
/// tool calls dim, one line each, and questions for John in gold. A blank line goes between
/// turns, not between tool calls; blank lines within a turn keep the
/// gutter, so scrollback's `{`/`}` stop only between turns.
///
/// Every line has a two-column gutter and fits the width, so scrollback
/// can count one line per screen row.
fn render(entries: &[(String, String)], width: usize, p: &Palette) -> Drawn {
    let mut out = Drawn::default();
    let mut last = "";
    let room = width.saturating_sub(2).max(1);
    for (e, (kind, text)) in entries.iter().enumerate() {
        let (mark, mark_style, style) = match kind.as_str() {
            "user" => ("› ", Style::new().fg(p.iris).add_modifier(Modifier::BOLD), Style::new().fg(p.iris)),
            "ask" => ("? ", Style::new().fg(p.gold).add_modifier(Modifier::BOLD), Style::new().fg(p.gold)),
            "tool" => ("· ", Style::new().fg(p.muted), Style::new().fg(p.subtle)),
            _ => ("● ", Style::new().fg(p.pine), Style::new().fg(p.text)),
        };
        if !out.lines.is_empty() && !(kind == "tool" && last == "tool") {
            out.push(Line::default(), None);
        }
        last = kind;
        if kind == "tool" {
            let line = Line::from(vec![Span::styled(mark, mark_style), Span::styled(clip_width(text, room), style)]);
            out.push(line, Some((e, 0..text.len())));
            continue;
        }
        let mut first = true;
        for (row, from) in markdown(text, room, p) {
            let lead = if first { Span::styled(mark, mark_style) } else { Span::raw("  ") };
            first = false;
            let row_style = style.patch(row.style);
            let body = row.spans.into_iter().map(|s| Span::styled(s.content, row_style.patch(s.style)));
            let mut line = Line::from(std::iter::once(lead).chain(body).collect::<Vec<_>>());
            if kind == "user" {
                // A full-width band; on the line, so the scrollback cursor
                // and selection, which replace the line's style, still show.
                let pad = width.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.style(Style::new().bg(p.overlay));
            }
            out.push(line, from.map(|r| (e, r)));
        }
    }
    out
}

/// Markdown drawn as lines `width` wide in the palette's colours, each
/// with the bytes of `text` it was drawn from. Each block (see `blocks`)
/// is drawn on its own so its lines know where they came from; a blank
/// line goes between blocks, except between the items of a tight list.
/// Rules are drawn across the width.
fn markdown(text: &str, width: usize, p: &Palette) -> Vec<(Line<'static>, Option<Range<usize>>)> {
    let options = tui_markdown::Options::new(Sheet(*p)).table_width(width.min(u16::MAX as usize) as u16);
    let mut out = Vec::new();
    let mut prev: Option<Range<usize>> = None;
    for (range, rule) in blocks(text) {
        if let Some(prev) = &prev {
            let gap_from = prev.start + text[prev.clone()].trim_end().len();
            let gap = text.get(gap_from..range.start).unwrap_or("");
            let tight = gap.matches('\n').count() < 2 && is_item(&text[prev.clone()]) && is_item(&text[range.clone()]);
            if !tight {
                out.push((Line::default(), None));
            }
        }
        prev = Some(range.clone());
        if rule {
            out.push((Line::styled("─".repeat(width), Style::new().fg(p.muted)), Some(range)));
            continue;
        }
        let mut lines = tui_markdown::from_str_with_options(&text[range.clone()], &options).lines;
        while lines.last().is_some_and(|l| l.width() == 0) {
            lines.pop();
        }
        let lead = lines.iter().take_while(|l| l.width() == 0).count();
        for l in lines.into_iter().skip(lead) {
            let base = l.style;
            let line = Line::from(
                l.spans.into_iter().map(|s| Span::styled(s.content.into_owned(), base.patch(s.style))).collect::<Vec<_>>(),
            );
            out.extend(wrap_line(line, width).into_iter().map(|row| (row, Some(range.clone()))));
        }
    }
    if out.is_empty() {
        out.push((Line::default(), Some(0..text.len())));
    }
    out
}

/// Whether a block is a list item, by its marker.
fn is_item(block: &str) -> bool {
    let t = block.trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    t.starts_with(['-', '*', '+']) || (digits > 0 && t[digits..].starts_with(['.', ')']))
}

/// The top-level blocks of `text` as byte ranges, with whether each is a
/// rule: paragraphs, headings, tables, code blocks, quotes and so on, and
/// each item of a top-level list on its own.
fn blocks(text: &str) -> Vec<(Range<usize>, bool)> {
    use pulldown_cmark::{Event, Options as Opts, Parser, Tag};
    // As tui-markdown parses.
    let opts = Opts::ENABLE_STRIKETHROUGH
        | Opts::ENABLE_TASKLISTS
        | Opts::ENABLE_HEADING_ATTRIBUTES
        | Opts::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Opts::ENABLE_SUPERSCRIPT
        | Opts::ENABLE_SUBSCRIPT
        | Opts::ENABLE_MATH
        | Opts::ENABLE_FOOTNOTES
        | Opts::ENABLE_DEFINITION_LIST
        | Opts::ENABLE_GFM
        | Opts::ENABLE_TABLES;
    let mut out = Vec::new();
    let mut depth = 0;
    let mut in_list = false;
    for (event, range) in Parser::new_ext(text, opts).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                match (depth, &tag) {
                    (0, Tag::List(_)) => in_list = true,
                    (0, _) => out.push((range, false)),
                    (1, Tag::Item) if in_list => out.push((range, false)),
                    _ => {}
                }
                depth += 1;
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    in_list = false;
                }
            }
            Event::Rule if depth == 0 => out.push((range, true)),
            _ if depth == 0 => out.push((range, false)),
            _ => {}
        }
    }
    out
}

/// Markdown styles from the palette. Code keeps the text's background, so
/// it reads as a colour rather than a block.
#[derive(Clone)]
struct Sheet(Palette);

impl tui_markdown::StyleSheet for Sheet {
    fn heading(&self, level: u8) -> Style {
        let s = Style::new().fg(self.0.iris).add_modifier(Modifier::BOLD);
        if level == 1 { s.add_modifier(Modifier::UNDERLINED) } else { s }
    }
    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }
    fn code(&self) -> Style {
        Style::new().fg(self.0.foam)
    }
    fn code_block_fence(&self) -> &str {
        ""
    }
    fn link(&self) -> Style {
        Style::new().fg(self.0.foam).add_modifier(Modifier::UNDERLINED)
    }
    fn blockquote(&self) -> Style {
        Style::new().fg(self.0.subtle).add_modifier(Modifier::ITALIC)
    }
    fn heading_meta(&self) -> Style {
        Style::new().fg(self.0.muted)
    }
    fn metadata_block(&self) -> Style {
        Style::new().fg(self.0.subtle)
    }
    fn html(&self) -> Style {
        Style::new().fg(self.0.muted)
    }
    fn math_inline(&self) -> Style {
        Style::new().fg(self.0.rose).add_modifier(Modifier::ITALIC)
    }
    fn math_display(&self) -> Style {
        Style::new().fg(self.0.rose)
    }
    fn footnote_ref(&self) -> Style {
        Style::new().fg(self.0.muted)
    }
    fn footnote_def(&self) -> Style {
        Style::new().fg(self.0.subtle)
    }
    fn alert(&self, kind: tui_markdown::AlertKind) -> Style {
        use tui_markdown::AlertKind::*;
        Style::new().fg(match kind {
            Note => self.0.foam,
            Tip => self.0.pine,
            Important => self.0.iris,
            Warning => self.0.gold,
            Caution => self.0.love,
        })
    }
    fn table_header(&self) -> Style {
        Style::new().fg(self.0.foam).add_modifier(Modifier::BOLD)
    }
    fn table_border(&self) -> Style {
        Style::new().fg(self.0.muted)
    }
    fn list_marker(&self) -> Style {
        Style::new().fg(self.0.subtle)
    }
    fn image_alt(&self) -> Style {
        Style::new().fg(self.0.muted).add_modifier(Modifier::ITALIC)
    }
}

fn char_width(c: char) -> usize {
    str_width(c.encode_utf8(&mut [0; 4]))
}

/// How far a line's wrapped rows indent: past its leading spaces and any
/// list or quote marker (`- `, `* `, `+ `, `1. `, `> `), so a list item's
/// rows line up under its text; just the spaces if that is over half the
/// width, and nothing if they are too.
fn hang(text: &str, width: usize) -> usize {
    let spaces = text.chars().take_while(|c| *c == ' ').count();
    let rest = &text[spaces..];
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let marker = if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") || rest.starts_with("> ") {
        2
    } else if digits > 0 && (rest[digits..].starts_with(". ") || rest[digits..].starts_with(") ")) {
        digits + 2
    } else {
        0
    };
    [spaces + marker, spaces].into_iter().find(|n| n * 2 <= width).unwrap_or(0)
}

/// `line` wrapped by word to `width` columns, keeping its spans' styles.
/// A line that fits comes back as it is (tables keep their spacing); a
/// longer one breaks at spaces, which are dropped at the break, its rows
/// indented past its leading spaces and list marker (see `hang`). A word
/// wider than a row is broken.
pub fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    if line.width() <= width {
        return vec![line];
    }
    let base = line.style;
    let chars: Vec<(char, Style)> =
        line.spans.iter().flat_map(|s| s.content.chars().map(move |c| (c, base.patch(s.style)))).collect();
    let text: String = chars.iter().map(|(c, _)| *c).collect();
    let indent = hang(&text, width);

    let mut rows: Vec<Vec<(char, Style)>> = Vec::new();
    let mut row: Vec<(char, Style)> = Vec::new();
    let mut used = 0;
    // Whether the row holds more than its indent, and the spaces waiting
    // to go before the next word.
    let mut filled = false;
    let mut gap: Vec<(char, Style)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].0 == ' ' {
            gap.push(chars[i]);
            i += 1;
            continue;
        }
        let end = (i..chars.len()).find(|&j| chars[j].0 == ' ').unwrap_or(chars.len());
        let word = &chars[i..end];
        let word_w: usize = word.iter().map(|(c, _)| char_width(*c)).sum();
        let gap_w = gap.len();
        if filled && used + gap_w + word_w > width {
            rows.push(std::mem::take(&mut row));
            row.extend(std::iter::repeat_n((' ', Style::new()), indent));
            used = indent;
            gap.clear();
        } else {
            used += gap_w;
            row.append(&mut gap);
        }
        for &(c, s) in word {
            // A word longer than a row: break it.
            if used + char_width(c) > width && used > indent {
                rows.push(std::mem::take(&mut row));
                row.extend(std::iter::repeat_n((' ', Style::new()), indent));
                used = indent;
            }
            row.push((c, s));
            used += char_width(c);
        }
        filled = true;
        i = end;
    }
    rows.push(row);
    rows.into_iter().map(spans).collect()
}

/// Styled chars as a line, a span per run of one style.
fn spans(chars: Vec<(char, Style)>) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for (c, s) in chars {
        match out.last_mut() {
            Some(last) if last.style == s => last.content.to_mut().push(c),
            _ => out.push(Span::styled(c.to_string(), s)),
        }
    }
    Line::from(out)
}

/// `text` wrapped by word to `width` columns, keeping its own line breaks
/// and indentation; a word wider than the width is broken.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    text.lines()
        .flat_map(|l| wrap_line(Line::from(l.to_string()), width))
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
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
        assert_eq!(lines, ["› Fix it      ", "", "· Bash: Run t…", "· Read: /a/ve…", "", "● Done and", "  tested"]);
    }

    #[test]
    fn a_transcript_says_where_its_agent_runs() {
        let json = serde_json::json!({ "error": "no log yet", "agent": { "host": "miller", "profile": "work", "harness": "pi", "dir": "/w" } });
        let pv = parse(&format!("-/s/w\n{json}")).unwrap();
        let want = Agent {
            host: "miller".into(),
            profile: "work".into(),
            harness: "pi".into(),
            dir: "/w".into(),
        };
        assert_eq!(pv.agent, Some(want));
        assert_eq!(parse("-/s/w\n{\"entries\": []}").unwrap().agent, None);
    }

    fn agent(md: &str, width: u16) -> Vec<Line<'static>> {
        let json = serde_json::json!({ "entries": [{ "kind": "assistant", "text": md }] });
        let mut pv = parse(&format!("-/s/w\n{json}")).unwrap();
        pv.lines(width, &Palette::load()).unwrap().to_vec()
    }

    #[test]
    fn agent_markdown_draws_headings_lists_code_and_emphasis() {
        let p = Palette::load();
        let md = "# Plan\n\nSome **bold**, *slanted* and `code`.\n\n- one\n- two\n\n1. first\n\n```rust\nfn main() {}\n```";
        let lines = agent(md, 40);
        let rows: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            rows,
            ["● Plan", "  ", "  Some bold, slanted and code.", "  ", "  - one", "  - two", "  ", "  1. first", "  ", "  fn main() {}"]
        );
        let style_of = |row: usize, word: &str| lines[row].spans.iter().find(|s| s.content.contains(word)).unwrap().style;
        assert_eq!(style_of(0, "Plan").fg, Some(p.iris));
        assert!(style_of(0, "Plan").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(2, "bold").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(2, "slanted").add_modifier.contains(Modifier::ITALIC));
        assert_eq!(style_of(2, "code").fg, Some(p.foam));
        assert_eq!(style_of(2, "Some").fg, Some(p.text));
        assert_eq!(style_of(9, "fn main").fg, Some(p.foam));
    }

    #[test]
    fn agent_tables_draw_with_borders_and_fit_the_pane() {
        let md = "| Name | Count |\n|:--|--:|\n| apples and pears from the market | 3 |";
        let lines = agent(md, 24);
        let rows: Vec<String> = lines.iter().map(text).collect();
        assert!(rows[0].starts_with("● ┌"), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("│ Name")), "{rows:?}");
        assert!(rows.iter().all(|r| str_width(r) <= 24), "{rows:?}");
        // A cell too wide for the pane wraps inside the table.
        assert!(rows.iter().filter(|r| r.contains('│')).count() > 2, "{rows:?}");
        assert_eq!(lines[0].spans[1].style.fg, Some(Palette::load().muted));
    }

    #[test]
    fn johns_messages_are_markdown_too() {
        let json = serde_json::json!({ "entries": [{ "kind": "user", "text": "one\n*two*" }] });
        let mut pv = parse(&format!("-/s/w\n{json}")).unwrap();
        let p = Palette::load();
        let lines = pv.lines(20, &p).unwrap();
        assert_eq!(lines.iter().map(text).collect::<Vec<_>>(), ["› one two           "]);
        assert!(lines[0].spans.iter().any(|s| s.content == "two" && s.style.add_modifier.contains(Modifier::ITALIC)));
        // On a full-width band, in their own colour.
        assert_eq!(lines[0].style.bg, Some(p.overlay));
        assert!(lines[0].spans.iter().any(|s| s.content == "one" && s.style.fg == Some(p.iris)));
    }

    #[test]
    fn rules_span_the_pane_and_lists_keep_their_spacing_and_numbers() {
        let rows = |md: &str| agent(md, 12).iter().map(text).collect::<Vec<_>>();
        assert_eq!(rows("a\n\n---\n\nb"), ["● a", "  ", "  ──────────", "  ", "  b"]);
        assert_eq!(rows("- one\n- two"), ["● - one", "  - two"]);
        assert_eq!(rows("- one\n\n- two"), ["● - one", "  ", "  - two"]);
        assert_eq!(rows("1. a\n2. b"), ["● 1. a", "  2. b"]);
    }

    #[test]
    fn copying_gives_the_markdown_the_lines_came_from() {
        let md = "# Plan\n\nSome **bold** text.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- x\n- `y`";
        let json = serde_json::json!({ "entries": [
            { "kind": "assistant", "text": md },
            { "kind": "user", "text": "do `it`" },
        ]});
        let mut pv = parse(&format!("-/s/w\n{json}")).unwrap();
        let rows: Vec<String> = pv.lines(30, &Palette::load()).unwrap().iter().map(text).collect();
        let at = |s: &str| rows.iter().position(|r| r.contains(s)).unwrap();
        // One line of a table copies the table.
        assert_eq!(pv.source(at("│ 1"), at("│ 1")).unwrap(), "| a | b |\n|---|---|\n| 1 | 2 |");
        assert_eq!(pv.source(at("bold"), at("bold")).unwrap(), "Some **bold** text.");
        assert_eq!(pv.source(at("- y"), at("- y")).unwrap(), "- `y`");
        // Across turns: each entry's part, a blank line apart.
        assert_eq!(pv.source(at("- x"), rows.len() - 1).unwrap(), "- x\n- `y`\n\ndo `it`");
        // The blank line between turns copies nothing.
        assert_eq!(pv.source(at("- y") + 1, at("- y") + 1), None);
    }

    #[test]
    fn styled_wrapping_keeps_styles_and_hangs_list_items() {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let line = Line::from(vec![Span::raw("- a "), Span::styled("bold word", bold), Span::raw(" then more")]);
        let rows = wrap_line(line, 10);
        assert_eq!(rows.iter().map(text).collect::<Vec<_>>(), ["- a bold", "  word", "  then", "  more"]);
        assert!(rows.iter().all(|r| r.width() <= 10));
        assert_eq!(rows[0].spans.last().unwrap(), &Span::styled("bold", bold));
        assert_eq!(rows[1].spans[1], Span::styled("word", bold));
        // A line that fits keeps its spacing, as table rows need.
        let row = Line::from("│ a   │ b │");
        assert_eq!(wrap_line(row.clone(), 20), [row]);
        // Wide characters count two columns.
        assert_eq!(wrap_line(Line::from("主机主机 x"), 4).iter().map(text).collect::<Vec<_>>(), ["主机", "主机", "x"]);
    }

    #[test]
    fn a_failed_fetch_says_why() {
        let mut pv = parse("6869/s/w\n{\"error\":\"no agent in this tab\"}").unwrap();
        assert_eq!(pv.lines(20, &Palette::load()).unwrap_err(), "no agent in this tab");
    }
}
