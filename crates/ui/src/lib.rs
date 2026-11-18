//! View state and rendering (REQUIREMENTS §5.3). Depends on `protocol` and
//! the UI toolkit (GPUI, DR 0003) only: views render the engine's model and
//! never compute architecture themselves (§2.6).

use gpui::{
    Context, IntoElement, ParentElement, Render, Styled, Window, WindowAppearance, div, rgb,
};

pub mod layout;

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

/// The application shell: the window's root view. M3 fills it in with the
/// review list, map and code diff (#90, #92, #93).
#[derive(Debug, Default)]
pub struct Shell;

impl Render for Shell {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_6()
            .bg(rgb(theme.background))
            .child(
                div()
                    .text_xl()
                    .text_color(rgb(theme.text))
                    .child("Blueprint"),
            )
            .child(
                div()
                    .text_color(rgb(theme.muted))
                    .child("Open a repository to review a change."),
            )
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
