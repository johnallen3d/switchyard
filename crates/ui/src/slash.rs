//! Slash-command completion for the prompt boxes: typing `/` opens a menu of
//! the agent's commands (see core's `commands`), more letters narrow it
//! fuzzily, and Tab puts the highlighted one in. Claude takes a command
//! anywhere in a message, so the menu opens on any word starting with `/`;
//! Pi only at the start, so there it opens only on the message's first word.

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Clear;
use switchyard_core::commands::SlashCommand;

use crate::Palette;
use crate::draw::str_width;

/// The most commands the menu shows at once; more scroll.
pub const MENU_ROWS: usize = 8;

/// The `/` word the cursor is in: its row, the columns (in characters) it
/// starts at (the `/`) and ends at, and what is typed after the `/` up to
/// the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub query: String,
}

/// The `/` word at `cursor` (row, column in characters) in `lines`, if it is
/// where a command can go: anywhere after a line start or a space when
/// `anywhere`, else only first in the message. A word with a second `/`
/// (a path) isn't one.
pub fn word_at(lines: &[String], cursor: (usize, usize), anywhere: bool) -> Option<Word> {
    let (row, col) = cursor;
    let chars: Vec<char> = lines.get(row)?.chars().collect();
    let col = col.min(chars.len());
    let start = chars[..col]
        .iter()
        .rposition(|c| c.is_whitespace())
        .map_or(0, |i| i + 1);
    if chars.get(start) != Some(&'/') || start >= col {
        return None;
    }
    let query: String = chars[start + 1..col].iter().collect();
    if query.contains('/') {
        return None;
    }
    if !anywhere {
        let blank_before = lines[..row].iter().all(|l| l.trim().is_empty())
            && chars[..start].iter().all(|c| c.is_whitespace());
        if !blank_before {
            return None;
        }
    }
    let end = chars[col..]
        .iter()
        .position(|c| c.is_whitespace())
        .map_or(chars.len(), |i| col + i);
    Some(Word {
        row,
        start,
        end,
        query,
    })
}

/// A command matching the query: its index in the list and the characters
/// of its name that matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub chars: Vec<usize>,
}

/// The commands whose name fuzzily matches `query`, ignoring case, best
/// first and in their listed order among equals; all of them, in order,
/// for an empty query. Pi's `skill:` prefix needs no typing: `fiz` finds
/// `skill:fizzy`.
pub fn matches(commands: &[SlashCommand], query: &str) -> Vec<Hit> {
    if query.is_empty() {
        return (0..commands.len())
            .map(|index| Hit {
                index,
                chars: Vec::new(),
            })
            .collect();
    }
    // Taken literally: the query is one word, and `^`, `!` or `'` are just
    // characters of a command name here.
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, Hit)> = commands
        .iter()
        .enumerate()
        .filter_map(|(index, c)| {
            let hay = Utf32Str::new(&c.name, &mut buf);
            let mut idx = Vec::new();
            let score = pattern.indices(hay, &mut matcher, &mut idx)?;
            idx.sort_unstable();
            idx.dedup();
            let chars = idx.into_iter().map(|i| i as usize).collect();
            Some((score, Hit { index, chars }))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, h)| h).collect()
}

/// A prompt box's commands and its menu over them.
#[derive(Default)]
pub struct Menu {
    commands: Vec<SlashCommand>,
    anywhere: bool,
    /// The highlighted match.
    pub selected: usize,
    /// The word and query the menu was last worked out for, and whether
    /// Escape closed it there; typing on opens it again.
    seen: Option<Word>,
    dismissed: bool,
}

impl Menu {
    pub fn set(&mut self, commands: Vec<SlashCommand>, anywhere: bool) {
        self.commands = commands;
        self.anywhere = anywhere;
        self.selected = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// The word at the cursor and its matches while the menu is open,
    /// keeping the highlight while the query stays the same.
    pub fn open(&mut self, lines: &[String], cursor: (usize, usize)) -> Option<(Word, Vec<Hit>)> {
        if self.commands.is_empty() {
            return None;
        }
        let word = word_at(lines, cursor, self.anywhere);
        if word != self.seen {
            self.selected = 0;
            self.dismissed = false;
            self.seen = word.clone();
        }
        let word = word.filter(|_| !self.dismissed)?;
        let found = matches(&self.commands, &word.query);
        if found.is_empty() {
            return None;
        }
        self.selected = self.selected.min(found.len() - 1);
        Some((word, found))
    }

    /// Close the menu until the word or query changes.
    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    pub fn step(&mut self, by: isize, count: usize) {
        if count > 0 {
            self.selected = (self.selected as isize + by).rem_euclid(count as isize) as usize;
        }
    }

    pub fn command(&self, i: usize) -> &SlashCommand {
        &self.commands[i]
    }

    /// Draw the menu for `found` (the highlight on `selected`) just above
    /// `card` in `bounds`, or below it when there is no room above.
    pub fn draw(&self, f: &mut Frame, p: &Palette, found: &[Hit], card: Rect, bounds: Rect) {
        let rows = found.len().min(MENU_ROWS) as u16;
        let above = card.y.saturating_sub(bounds.y);
        let below = (bounds.y + bounds.height).saturating_sub(card.y + card.height);
        let (y, rows) = if above >= rows || above >= below {
            (card.y - rows.min(above), rows.min(above))
        } else {
            (card.y + card.height, rows.min(below))
        };
        if rows == 0 || card.width < 8 {
            return;
        }
        let area = Rect {
            x: card.x,
            y,
            width: card.width,
            height: rows,
        };
        f.render_widget(Clear, area);
        let first = self.selected.saturating_sub(rows as usize - 1);
        let w = area.width as usize;
        for (n, hit) in found.iter().enumerate().skip(first).take(rows as usize) {
            let c = &self.commands[hit.index];
            let bg = if n == self.selected {
                p.hl_med
            } else {
                p.surface
            };
            // The name, with the characters the query matched picked out.
            let plain = Style::new().fg(p.text).bg(bg);
            let picked = Style::new().fg(p.foam).bg(bg).add_modifier(Modifier::BOLD);
            let mut spans = vec![
                Span::styled("  ", Style::new().bg(bg)),
                Span::styled("/", plain),
            ];
            let mut run = String::new();
            let mut run_hit = false;
            for (k, ch) in c.name.chars().enumerate() {
                let is_hit = hit.chars.binary_search(&k).is_ok();
                if is_hit != run_hit && !run.is_empty() {
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        if run_hit { picked } else { plain },
                    ));
                }
                run_hit = is_hit;
                run.push(ch);
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, if run_hit { picked } else { plain }));
            }
            if !c.hint.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", c.hint),
                    Style::new().fg(p.subtle).bg(bg),
                ));
            }
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            if !c.description.is_empty() && used + 4 < w {
                let first_line = c.description.lines().next().unwrap_or("");
                spans.push(Span::styled(
                    format!("  {}", clip(first_line, w - used - 3)),
                    Style::new().fg(p.muted).bg(bg),
                ));
            }
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            spans.push(Span::styled(
                " ".repeat(w.saturating_sub(used)),
                Style::new().bg(bg),
            ));
            let row = Rect {
                y: area.y + (n - first) as u16,
                height: 1,
                ..area
            };
            f.render_widget(Line::from(spans), row);
        }
    }
}

/// `s` cut to `room` columns, with `…` when cut.
fn clip(s: &str, room: usize) -> String {
    if str_width(s) <= room {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if str_width(&out) + 2 > room {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.split('\n').map(String::from).collect()
    }

    fn cmd(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: String::new(),
            hint: String::new(),
        }
    }

    #[test]
    fn a_slash_word_is_found_anywhere_for_claude() {
        let w = word_at(&lines("please /iss now"), (0, 11), true).unwrap();
        assert_eq!((w.start, w.end, w.query.as_str()), (7, 11, "iss"));
        assert_eq!(word_at(&lines("/"), (0, 1), true).unwrap().query, "");
        // The cursor mid-word: the whole word is replaced, the query stops at the cursor.
        let w = word_at(&lines("/issue-create"), (0, 4), true).unwrap();
        assert_eq!((w.end, w.query.as_str()), (13, "iss"));
        assert_eq!(word_at(&lines("a\n  /w"), (1, 4), true).unwrap().row, 1);
    }

    #[test]
    fn paths_and_mid_word_slashes_are_not_commands() {
        assert!(word_at(&lines("/usr/bin"), (0, 8), true).is_none());
        assert!(word_at(&lines("and/or"), (0, 6), true).is_none());
        assert!(word_at(&lines("x /w y"), (0, 1), true).is_none());
        // Just before the slash, nothing is typed into it yet.
        assert!(word_at(&lines("/w"), (0, 0), true).is_none());
    }

    #[test]
    fn pi_takes_a_command_only_first_in_the_message() {
        assert!(word_at(&lines("  /iss"), (0, 6), false).is_some());
        assert!(word_at(&lines("\n/iss"), (1, 4), false).is_some());
        assert!(word_at(&lines("do /iss"), (0, 7), false).is_none());
        assert!(word_at(&lines("x\n/iss"), (1, 4), false).is_none());
    }

    fn indices(found: Vec<Hit>) -> Vec<usize> {
        found.into_iter().map(|h| h.index).collect()
    }

    #[test]
    fn matching_is_fuzzy_with_skill_optional() {
        let cmds = [
            cmd("issue-create"),
            cmd("skill:fizzy"),
            cmd("wrap"),
            cmd("Issue-plan"),
        ];
        assert_eq!(indices(matches(&cmds, "iss")), [0, 3]);
        assert_eq!(indices(matches(&cmds, "fiz")), [1]);
        assert_eq!(indices(matches(&cmds, "skill:f")), [1]);
        assert_eq!(indices(matches(&cmds, "rap")), [2]);
        assert_eq!(indices(matches(&cmds, "icr")), [0]);
        assert_eq!(indices(matches(&cmds, "ipl")), [3]);
        assert_eq!(indices(matches(&cmds, "zzz")), Vec::<usize>::new());
        assert_eq!(indices(matches(&cmds, "")), [0, 1, 2, 3]);
        assert_eq!(matches(&cmds, "icr")[0].chars, [0, 6, 7]);
    }

    #[test]
    fn closer_matches_come_first() {
        let cmds = [cmd("review-pr"), cmd("pr")];
        assert_eq!(indices(matches(&cmds, "pr")), [1, 0]);
    }

    #[test]
    fn escape_closes_the_menu_until_the_query_changes() {
        let mut m = Menu::default();
        m.set(vec![cmd("wrap"), cmd("wait")], true);
        assert_eq!(indices(m.open(&lines("/w"), (0, 2)).unwrap().1), [0, 1]);
        m.step(1, 2);
        assert_eq!(m.selected, 1);
        assert_eq!(m.open(&lines("/w"), (0, 2)).map(|_| m.selected), Some(1));
        m.dismiss();
        assert!(m.open(&lines("/w"), (0, 2)).is_none());
        assert!(m.open(&lines("/wr"), (0, 3)).is_some());
        assert_eq!(m.selected, 0);
    }
}
