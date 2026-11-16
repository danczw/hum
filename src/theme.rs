use ratatui::style::{Color, Modifier, Style};

/// Bars from this share of use up take `load_mid`.
pub const LOAD_MID: f64 = 0.60;
/// Bars from this share of use up take `load_high`.
pub const LOAD_HIGH: f64 = 0.85;

/// The styles widgets draw with, one per role. Widget code names roles,
/// never colors.
#[derive(Clone, Copy)]
pub struct Theme {
    /// Titles, rules, units, the unused part of bars.
    pub dim: Style,
    /// Agent root rows.
    pub accent: Style,
    /// Orphan rows and a non-zero orphan count.
    pub warn: Style,
    pub load_low: Style,
    pub load_mid: Style,
    pub load_high: Style,
    #[cfg_attr(not(test), expect(dead_code, reason = "no row selection yet"))]
    pub selected: Style,
}

impl Theme {
    /// Modifiers only when `NO_COLOR` is set and non-empty (no-color.org),
    /// else [`Theme::ayu_mirage`] when `COLORTERM` announces truecolor, else
    /// [`Theme::ansi`].
    pub fn from_env() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit");
        Self::pick(no_color, truecolor)
    }

    fn pick(no_color: bool, truecolor: bool) -> Self {
        match (no_color, truecolor) {
            (true, _) => Self::no_color(),
            (false, true) => Self::ayu_mirage(),
            (false, false) => Self::ansi(),
        }
    }

    /// Truecolor, the older ayu-mirage palette (bg `#212733`, fg `#d9d7ce`).
    /// Dark backgrounds only.
    pub fn ayu_mirage() -> Self {
        Self::colors(
            Color::from_u32(0x5c6773),
            Color::from_u32(0x5ccfe6),
            Color::from_u32(0xffd57f),
            Color::from_u32(0xff3333),
        )
    }

    /// ANSI 16 for terminals without truecolor; follows the terminal theme.
    pub fn ansi() -> Self {
        Self::colors(Color::DarkGray, Color::Cyan, Color::Yellow, Color::Red)
    }

    /// `warn` doubles as `load_mid`; `load_low` keeps the default fg.
    fn colors(dim: Color, accent: Color, warn: Color, high: Color) -> Self {
        Self {
            dim: Style::new().fg(dim),
            accent: Style::new().fg(accent),
            warn: Style::new().fg(warn),
            load_low: Style::new(),
            load_mid: Style::new().fg(warn),
            load_high: Style::new().fg(high),
            selected: Style::new().add_modifier(Modifier::REVERSED),
        }
    }

    pub fn no_color() -> Self {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        Self {
            dim: Style::new().add_modifier(Modifier::DIM),
            accent: bold,
            warn: bold,
            load_low: Style::new(),
            load_mid: Style::new(),
            load_high: bold,
            selected: Style::new().add_modifier(Modifier::REVERSED),
        }
    }

    /// The bar style for a share of use from 0.0 to 1.0.
    pub fn load(&self, ratio: f64) -> Style {
        if ratio >= LOAD_HIGH {
            self.load_high
        } else if ratio >= LOAD_MID {
            self.load_mid
        } else {
            self.load_low
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_steps_at_the_thresholds() {
        let theme = Theme::ayu_mirage();
        assert_eq!(theme.load(0.0), theme.load_low);
        assert_eq!(theme.load(0.59), theme.load_low);
        assert_eq!(theme.load(0.60), theme.load_mid);
        assert_eq!(theme.load(0.84), theme.load_mid);
        assert_eq!(theme.load(0.85), theme.load_high);
        assert_eq!(theme.load(1.0), theme.load_high);
    }

    #[test]
    fn no_color_sets_no_colors_and_keeps_selected_visible() {
        let theme = Theme::no_color();
        for style in [
            theme.dim,
            theme.accent,
            theme.warn,
            theme.load_low,
            theme.load_mid,
            theme.load_high,
            theme.selected,
        ] {
            assert_eq!((style.fg, style.bg), (None, None), "{style:?}");
        }
        assert!(theme.selected.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn truecolor_picks_ayu_mirage_and_no_color_wins() {
        let accent = |theme: Theme| theme.accent.fg;
        assert_eq!(
            accent(Theme::pick(false, true)),
            accent(Theme::ayu_mirage())
        );
        assert_eq!(accent(Theme::pick(false, false)), accent(Theme::ansi()));
        assert_eq!(accent(Theme::pick(true, true)), None);
    }
}
