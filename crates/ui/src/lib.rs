//! View state and rendering (REQUIREMENTS §5.3). Depends on `protocol` and
//! the UI toolkit (GPUI, DR 0003) only: views render the engine's model and
//! never compute architecture themselves (§2.6).

use gpui::WindowAppearance;

pub mod actions;
pub mod code_view;
pub mod layout;
pub mod map;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_follow_the_system_appearance() {
        assert_eq!(Theme::for_appearance(WindowAppearance::Dark), Theme::DARK);
        assert_eq!(Theme::for_appearance(WindowAppearance::Light), Theme::LIGHT);
        assert_ne!(Theme::LIGHT.background, Theme::DARK.background);
    }
}
