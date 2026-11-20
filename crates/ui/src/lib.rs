//! View state and rendering (REQUIREMENTS §5.3). Depends on `protocol` and
//! the UI toolkit (GPUI, DR 0003) only: views render the engine's model and
//! never compute architecture themselves (§2.6).

use gpui::WindowAppearance;

pub mod actions;
pub mod code_view;
pub mod findings;
pub mod layout;
pub mod map;
pub mod outline;
pub mod review_list;
pub mod workspace;

/// The colours of one theme (§5.5: light and dark, following the system).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Window background.
    pub background: u32,
    /// Primary text.
    pub text: u32,
    /// Secondary text.
    pub muted: u32,
}

impl Theme {
    /// The light theme.
    pub const LIGHT: Theme = Theme {
        background: 0x00ff_ffff,
        text: 0x001f_2328,
        muted: 0x0059_636e,
    };
    /// The dark theme.
    pub const DARK: Theme = Theme {
        background: 0x000d_1117,
        text: 0x00e6_edf3,
        muted: 0x009d_a7b3,
    };

    /// The theme for the system's appearance.
    #[must_use]
    pub fn for_appearance(appearance: WindowAppearance) -> Theme {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::DARK,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::LIGHT,
        }
    }
}

/// The WCAG 2 contrast ratio of two `0xRRGGBB` colours, from 1 to 21
/// (§5.5: WCAG AA asks 4.5 for text, 3 for graphics such as edges).
#[must_use]
pub fn contrast(a: u32, b: u32) -> f64 {
    let luminance = |c: u32| {
        let channel = |shift: u32| {
            let v = f64::from((c >> shift) & 0xff) / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    };
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_matches_the_wcag_formula() {
        assert!((contrast(0x00ff_ffff, 0) - 21.0).abs() < 1e-9);
        assert!((contrast(0x0077_7777, 0x0077_7777) - 1.0).abs() < 1e-9);
        // WCAG's own example: #767676 on white is just over 4.5.
        assert!(contrast(0x0076_7676, 0x00ff_ffff) > 4.5);
    }

    #[test]
    fn themes_follow_the_system_appearance() {
        assert_eq!(Theme::for_appearance(WindowAppearance::Dark), Theme::DARK);
        assert_eq!(Theme::for_appearance(WindowAppearance::Light), Theme::LIGHT);
        assert_ne!(Theme::LIGHT.background, Theme::DARK.background);
    }
}
