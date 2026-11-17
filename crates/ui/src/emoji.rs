//! Emoji completion for the prompt boxes, as in Slack: typing `:` and two
//! or more letters of a shortcode opens a menu of the emoji whose GitHub
//! shortcodes match, and Tab puts the highlighted one in. Typing a whole
//! `:shortcode:` turns it into its emoji straight away.

use std::sync::LazyLock;

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::Palette;
use crate::draw::cycle;
use crate::slash::{Row, Word, draw_rows};

/// Letters of a shortcode typed after the `:` before the menu opens, so a
/// colon in ordinary text (`note:`, `12:30`) doesn't open it.
pub const MIN_QUERY: usize = 2;

/// The most matches the menu offers; more letters narrow it.
const MAX_HITS: usize = 50;

/// Every shortcode and its emoji, in the emoji's standard order.
static SHORTCODES: LazyLock<Vec<(&'static str, &'static str)>> = LazyLock::new(|| {
    emojis::iter()
        .flat_map(|e| e.shortcodes().map(move |code| (code, e.as_str())))
        .collect()
});

fn is_code_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+')
}

/// The `:` word at `cursor` (row, column in characters) in `lines`, if one
/// is being typed: it starts a line or follows a space or an opening
/// bracket, and has at least `MIN_QUERY` shortcode characters between the
/// `:` and the cursor.
#[must_use]
pub fn word_at(lines: &[String], cursor: (usize, usize)) -> Option<Word> {
    let (row, col) = cursor;
    let chars: Vec<char> = lines.get(row)?.chars().collect();
    let col = col.min(chars.len());
    let start = chars[..col].iter().rposition(|&c| c == ':')?;
    let query: String = chars[start + 1..col].iter().collect();
    if query.chars().count() < MIN_QUERY || !query.chars().all(is_code_char) {
        return None;
    }
    if start > 0 && !(chars[start - 1].is_whitespace() || "([{\"'".contains(chars[start - 1])) {
        return None;
    }
    // The rest of the word under the cursor goes too, with a closing `:`.
    let rest = chars[col..]
        .iter()
        .position(|&c| !is_code_char(c))
        .map_or(chars.len(), |i| col + i);
    let end = if chars.get(rest) == Some(&':') {
        rest + 1
    } else {
        rest
    };
    Some(Word {
        row,
        start,
        end,
        query,
    })
}

/// A shortcode matching the query: its index in `SHORTCODES` and the
/// characters of it that matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub chars: Vec<usize>,
}

/// The shortcodes containing `query`, ignoring case: those starting with it
/// first, then those with a `_`-separated part starting with it, then the
/// rest; shorter ones first within each, then in the emoji's order.
#[must_use]
pub fn matches(query: &str) -> Vec<Hit> {
    let query = query.to_ascii_lowercase();
    let len = query.chars().count();
    let mut found: Vec<(u8, usize, Hit)> = SHORTCODES
        .iter()
        .enumerate()
        .filter_map(|(index, (code, _))| {
            let at = code.find(&query)?;
            let rank = if at == 0 {
                0
            } else if let Some(part) = code
                .match_indices('_')
                .map(|(i, _)| i + 1)
                .find(|&i| code[i..].starts_with(&query))
            {
                return Some((1, code.len(), hit(index, part, len)));
            } else {
                2
            };
            Some((rank, code.len(), hit(index, at, len)))
        })
        .collect();
    found.sort_by_key(|(rank, len, h)| (*rank, *len, h.index));
    found.truncate(MAX_HITS);
    found.into_iter().map(|(_, _, h)| h).collect()
}

fn hit(index: usize, at: usize, len: usize) -> Hit {
    // Shortcodes are ASCII, so byte offsets are character offsets.
    Hit {
        index,
        chars: (at..at + len).collect(),
    }
}

/// The emoji for a whole shortcode, if there is one.
#[must_use]
pub fn lookup(code: &str) -> Option<&'static str> {
    emojis::get_by_shortcode(code).map(emojis::Emoji::as_str)
}

/// The emoji of match `index`.
#[must_use]
pub fn emoji(index: usize) -> &'static str {
    SHORTCODES[index].1
}

/// A `:shortcode:` just closed at `cursor`: the columns (in characters) it
/// starts and ends at in its row, and its emoji.
#[must_use]
pub fn closed_at(lines: &[String], cursor: (usize, usize)) -> Option<(usize, usize, &'static str)> {
    let (row, col) = cursor;
    let chars: Vec<char> = lines.get(row)?.chars().collect();
    if col == 0 || chars.get(col - 1) != Some(&':') {
        return None;
    }
    let word = word_at(lines, (row, col - 1))?;
    lookup(&word.query).map(|e| (word.start, col, e))
}

/// The emoji menu's highlight, and whether Escape closed it.
#[derive(Default)]
pub struct Menu {
    /// The highlighted match.
    pub selected: usize,
    seen: Option<Word>,
    dismissed: bool,
}

impl Menu {
    /// The word at the cursor and its matches while the menu is open,
    /// keeping the highlight while the query stays the same.
    pub fn open(&mut self, lines: &[String], cursor: (usize, usize)) -> Option<(Word, Vec<Hit>)> {
        let word = word_at(lines, cursor);
        if word != self.seen {
            self.selected = 0;
            self.dismissed = false;
            self.seen.clone_from(&word);
        }
        let word = word.filter(|_| !self.dismissed)?;
        let found = matches(&word.query);
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
            self.selected = cycle(self.selected, by, count);
        }
    }

    /// Draw the menu for `found` just above `card` in `bounds`, or below it
    /// when there is no room above.
    pub fn draw(&self, f: &mut Frame, p: &Palette, found: &[Hit], card: Rect, bounds: Rect) {
        let rows: Vec<Row> = found
            .iter()
            .map(|hit| {
                let (code, emoji) = SHORTCODES[hit.index];
                Row {
                    lead: format!("{emoji} :"),
                    name: code,
                    chars: &hit.chars,
                    tail: ":",
                    hint: "",
                    description: "",
                }
            })
            .collect();
        draw_rows(f, p, &rows, self.selected, card, bounds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.split('\n').map(String::from).collect()
    }

    fn codes(found: &[Hit]) -> Vec<&'static str> {
        found.iter().map(|h| SHORTCODES[h.index].0).collect()
    }

    #[test]
    fn a_colon_word_opens_after_two_letters() {
        assert!(word_at(&lines(":s"), (0, 2)).is_none());
        let w = word_at(&lines("nice :smi"), (0, 9)).unwrap();
        assert_eq!((w.start, w.end, w.query.as_str()), (5, 9, "smi"));
        assert!(word_at(&lines("(:+1"), (0, 4)).is_some());
        // The cursor mid-word: the rest, and a closing colon, go too.
        let w = word_at(&lines(":smile: ok"), (0, 3)).unwrap();
        assert_eq!((w.end, w.query.as_str()), (7, "sm"));
    }

    #[test]
    fn colons_in_ordinary_text_are_not_shortcodes() {
        assert!(word_at(&lines("note:ab"), (0, 7)).is_none());
        assert!(word_at(&lines("at 12:30"), (0, 8)).is_none());
        assert!(word_at(&lines("see: this"), (0, 9)).is_none());
        assert!(word_at(&lines("http://x"), (0, 8)).is_none());
    }

    #[test]
    fn prefix_matches_come_first() {
        let found = codes(&matches("tada"));
        assert_eq!(found[0], "tada");
        let found = codes(&matches("Thumbs"));
        assert!(found.starts_with(&["thumbsup", "thumbsdown"]), "{found:?}");
        assert_eq!(matches("zzzzqq"), Vec::<Hit>::new());
        assert_eq!(matches("heart")[0].chars, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_closed_shortcode_is_found() {
        assert_eq!(
            closed_at(&lines("go :rocket:"), (0, 11)),
            Some((3, 11, "🚀"))
        );
        assert!(closed_at(&lines("go :nope_nope:"), (0, 14)).is_none());
        assert!(closed_at(&lines("go :rocket"), (0, 10)).is_none());
    }
}
