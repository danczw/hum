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
    /// else [`Theme::ayu_mirage`].
    pub fn from_env() -> Self {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            Self::no_color()
        } else {
            Self::ayu_mirage()
        }
    }

    /// Truecolor, the older ayu-mirage palette (bg `#212733`, fg `#d9d7ce`).
    /// Dark backgrounds only.
    pub fn ayu_mirage() -> Self {
        let warn = Color::from_u32(0xffd57f);
        Self {
            dim: Style::new().fg(Color::from_u32(0x5c6773)),
            accent: Style::new().fg(Color::from_u32(0x5ccfe6)),
            warn: Style::new().fg(warn),
            load_low: Style::new(),
            load_mid: Style::new().fg(warn),
            load_high: Style::new().fg(Color::from_u32(0xff3333)),
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
}
