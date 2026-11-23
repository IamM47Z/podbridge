//! Styles the UI repeats. Each takes the theme and returns the style that used to be inline.

use iced::border::Radius;
use iced::overlay::menu;
use iced::widget::{button, container, text, text_input};
use iced::{Background, Border, Color, Theme};

/// Settings card: faint primary fill, half-strength primary outline, rounded 16.
pub fn card(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.palette().primary.scale_alpha(0.1))),
        border: Border {
            color: theme.palette().primary.scale_alpha(0.5),
            ..Default::default()
        }
        .rounded(16),
        ..Default::default()
    }
}

/// Faint primary fill with a text-coloured outline and the given corner radius.
pub fn outlined_panel(theme: &Theme, radius: f32) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.palette().primary.scale_alpha(0.1))),
        border: Border {
            color: theme.palette().text,
            ..Default::default()
        }
        .rounded(radius),
        ..Default::default()
    }
}

/// Plain text in the theme's text colour.
pub fn text_normal(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.palette().text),
    }
}

/// Secondary text: the theme's text colour at 0.7 alpha.
pub fn text_muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.palette().text.scale_alpha(0.7)),
    }
}

/// Section headings in the theme's primary colour.
pub fn text_accent(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.palette().primary),
    }
}

/// Input of the noise-control and theme combo boxes.
pub fn combo_input(theme: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(theme.palette().primary.scale_alpha(0.2)),
        border: Border {
            width: 1.0,
            color: theme.palette().text.scale_alpha(0.3),
            radius: Radius::from(4.0),
        },
        icon: Default::default(),
        placeholder: theme.palette().text,
        value: theme.palette().text,
        selection: Default::default(),
    }
}

/// Dropdown menu of the noise-control and theme combo boxes.
pub fn combo_menu(theme: &Theme) -> menu::Style {
    menu::Style {
        background: Background::Color(theme.palette().background),
        border: Border {
            width: 1.0,
            color: theme.palette().text,
            radius: Radius::from(4.0),
        },
        text_color: theme.palette().text,
        selected_text_color: theme.palette().text,
        selected_background: Background::Color(theme.palette().primary.scale_alpha(0.3)),
        shadow: Default::default(),
    }
}

/// Sidebar tab button. The selected one keeps the default (black) text colour.
pub fn sidebar_tab(theme: &Theme, is_selected: bool) -> button::Style {
    if is_selected {
        button::Style {
            background: Some(Background::Color(theme.palette().primary)),
            border: Border {
                color: theme.palette().text,
                ..Default::default()
            }
            .rounded(12),
            ..Default::default()
        }
    } else {
        button::Style {
            background: Some(Background::Color(theme.palette().primary.scale_alpha(0.1))),
            border: Border {
                color: theme.palette().primary.scale_alpha(0.1),
                ..Default::default()
            }
            .rounded(8),
            text_color: theme.palette().text,
            ..Default::default()
        }
    }
}

/// Borderless transparent button that looks like plain text.
pub fn text_button(theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        text_color: theme.palette().text,
        background: Some(Background::Color(Color::TRANSPARENT)),
        ..Default::default()
    }
}
