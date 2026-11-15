//! Popups over a float: one look for every box that opens over the
//! content (rounded iris border, a padded title, a column of padding each
//! side), one place for list popups to open, and a backdrop that dims
//! what they cover.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding};

use crate::Palette;

/// The width every list popup takes, less where the pane is narrower.
pub const LIST_WIDTH: u16 = 80;

/// A popup's block: rounded iris border on the base colour, `title` set
/// one rule in from the corner, and a column of padding inside each side.
#[must_use]
pub fn block(p: &Palette, title: &str) -> Block<'static> {
    let border = Style::new().fg(p.iris).bg(p.base);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .style(Style::new().bg(p.base))
        .padding(Padding::horizontal(1))
        .title_top(Line::from(vec![
            Span::styled("─", border),
            Span::styled(
                format!(" {title} "),
                Style::new().fg(p.text).add_modifier(Modifier::BOLD),
            ),
        ]))
}

/// Draw `block` over `r`, clearing what was there, and return the area
/// inside its border and padding.
pub fn open(f: &mut Frame, r: Rect, block: Block<'static>) -> Rect {
    let inner = block.inner(r);
    f.render_widget(Clear, r);
    f.render_widget(block, r);
    inner
}

/// Where a list popup `height` rows tall opens in `area`: [`LIST_WIDTH`]
/// wide, centred across, its top a quarter of the way down (higher only
/// when it wouldn't fit), so its first row lands in the same place
/// whatever it lists.
#[must_use]
pub fn list_rect(area: Rect, height: u16) -> Rect {
    let width = LIST_WIDTH.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height).min(area.height / 4),
        width,
        height,
    }
}

/// Pick out `row` (a line of a popup's padded `inner`) as selected: the
/// highlight runs across the padding to the border on both sides, so no
/// bar sits beside the border.
pub fn select(f: &mut Frame, p: &Palette, inner: Rect, row: Rect) {
    let band = Rect {
        x: inner.x.saturating_sub(1),
        width: inner.width.saturating_add(2),
        ..row
    }
    .intersection(f.area());
    f.buffer_mut().set_style(band, Style::new().bg(p.overlay));
}

/// Dim everything in `area`, so a popup drawn over it reads as the focus:
/// each cell's colours are drawn three fifths of the way to the base
/// colour. Kitty image
/// placeholders keep theirs, as their colour names the image.
pub fn backdrop(f: &mut Frame, p: &Palette, area: Rect) {
    dim(f.buffer_mut(), p, area);
}

fn dim(buf: &mut Buffer, p: &Palette, area: Rect) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if cell.symbol().starts_with('\u{10EEEE}') {
                continue;
            }
            cell.fg = toward(cell.fg, p.base, p.muted);
            cell.bg = toward(cell.bg, p.base, p.base);
        }
    }
}

/// `c` three fifths of the way to `base`; `fallback` for a colour that
/// isn't RGB.
fn toward(c: Color, base: Color, fallback: Color) -> Color {
    match (c, base) {
        (Color::Rgb(r, g, b), Color::Rgb(br, bg, bb)) => {
            let mix =
                |a: u8, z: u8| u8::try_from((u16::from(a) * 2 + u16::from(z) * 3) / 5).unwrap_or(z);
            Color::Rgb(mix(r, br), mix(g, bg), mix(b, bb))
        }
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_popups_share_an_anchor_and_width() {
        let area = Rect::new(10, 2, 120, 40);
        let small = list_rect(area, 4);
        let big = list_rect(area, 20);
        assert_eq!((small.x, small.y, small.width), (30, 12, 80));
        assert_eq!((big.x, big.y), (small.x, small.y));
        // Too tall for a quarter down: it moves up, never off the pane.
        let tall = list_rect(area, 38);
        assert_eq!((tall.y, tall.bottom()), (4, 42));
        // A narrow pane gets the popup at its width.
        assert_eq!(list_rect(Rect::new(0, 0, 50, 10), 30).width, 50);
    }

    #[test]
    fn backdrop_draws_colours_toward_base_and_spares_images() {
        let mut p = Palette::load();
        p.base = Color::Rgb(0, 0, 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        buf[(0, 0)].set_fg(Color::Rgb(200, 100, 50));
        buf[(1, 0)]
            .set_symbol("\u{10EEEE}\u{0305}\u{0305}")
            .set_fg(Color::Rgb(0, 0, 7));
        let all = buf.area;
        dim(&mut buf, &p, all);
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(80, 40, 20));
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(0, 0, 0));
        assert_eq!(buf[(1, 0)].fg, Color::Rgb(0, 0, 7));
    }
}
