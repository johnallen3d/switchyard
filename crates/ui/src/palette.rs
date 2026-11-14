//! Colours come from the managed theme through the runtime config's
//! `[palette]` (`switchyard_core::config`), falling back to Rosé Pine. Every cell gets an
//! explicit background: a Rex float shows the pane beneath through cells left
//! on the terminal's default background.
//!
//! Roles: iris for structure and focus, gold for tabs waiting on input, foam
//! for working, pine for done, love for errors, subtle and muted for
//! secondary text, overlay and highlight-med for surfaces and selection.

use ratatui::style::Color;

#[derive(Clone, Copy)]
pub struct Palette {
    pub base: Color,
    pub surface: Color,
    pub overlay: Color,
    pub hl_med: Color,
    pub hl_high: Color,
    pub text: Color,
    pub subtle: Color,
    pub muted: Color,
    pub love: Color,
    pub gold: Color,
    pub rose: Color,
    pub pine: Color,
    pub foam: Color,
    pub iris: Color,
}

/// `#rrggbb` (the `#` optional) as a colour.
#[must_use]
pub fn color(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(s, 16).ok()?;
    let [_, red, green, blue] = value.to_be_bytes();
    Some(Color::Rgb(red, green, blue))
}

impl Palette {
    #[must_use]
    pub fn load() -> Self {
        let mut p = Palette {
            base: Color::Rgb(0x19, 0x17, 0x24),
            surface: Color::Rgb(0x1f, 0x1d, 0x2e),
            overlay: Color::Rgb(0x26, 0x23, 0x3a),
            hl_med: Color::Rgb(0x40, 0x3d, 0x52),
            hl_high: Color::Rgb(0x52, 0x4f, 0x67),
            text: Color::Rgb(0xe0, 0xde, 0xf4),
            subtle: Color::Rgb(0x90, 0x8c, 0xaa),
            muted: Color::Rgb(0x6e, 0x6a, 0x86),
            love: Color::Rgb(0xeb, 0x6f, 0x92),
            gold: Color::Rgb(0xf6, 0xc1, 0x77),
            rose: Color::Rgb(0xeb, 0xbc, 0xba),
            pine: Color::Rgb(0x31, 0x74, 0x8f),
            foam: Color::Rgb(0x9c, 0xcf, 0xd8),
            iris: Color::Rgb(0xc4, 0xa7, 0xe7),
        };
        let names = switchyard_core::config::load()
            .map(|c| &c.palette)
            .into_iter()
            .flatten();
        for (name, value) in names {
            let Some(c) = color(value) else { continue };
            match name.trim() {
                "base" => p.base = c,
                "surface" => p.surface = c,
                "overlay" => p.overlay = c,
                "hl_med" => p.hl_med = c,
                "hl_high" => p.hl_high = c,
                "text" => p.text = c,
                "subtle" => p.subtle = c,
                "muted" => p.muted = c,
                "love" => p.love = c,
                "gold" => p.gold = c,
                "rose" => p.rose = c,
                "pine" => p.pine = c,
                "foam" => p.foam = c,
                "iris" => p.iris = c,
                _ => {}
            }
        }
        p
    }
}
