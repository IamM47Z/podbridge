//! Small widget builders shared by the device pages.

use crate::ui::style;
use crate::ui::window::Message;
use iced::widget::{Row, Space, button, column, row, text, toggler::Toggler};
use iced::{Center, Element, Length};

/// Title and muted description on the left, a toggler on the right.
pub fn toggle_row<'a>(
    title: &'a str,
    description: &'a str,
    toggle: Toggler<'a, Message>,
    spacing: u32,
) -> Row<'a, Message> {
    row![
        column![
            text(title).size(16),
            text(description)
                .size(12)
                .style(style::text_muted)
                .width(Length::Fill)
        ]
        .width(Length::Fill),
        toggle.spacing(0).size(20)
    ]
    .align_y(Center)
    .spacing(spacing)
}

/// "Label ... value" line of a device information card.
pub fn info_row<'a>(label: &'a str, value: impl Into<Element<'a, Message>>) -> Row<'a, Message> {
    row![
        text(label).size(16).style(style::text_normal),
        Space::new().width(Length::Fill),
        value.into()
    ]
}

/// A value that copies itself to the clipboard when pressed.
pub fn copyable<'a>(value: &str) -> Element<'a, Message> {
    button(text(value.to_string()).size(16))
        .style(style::text_button)
        .padding(0)
        .on_press(Message::CopyToClipboard(value.to_string()))
        .into()
}
