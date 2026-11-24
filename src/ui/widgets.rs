//! Small widget builders shared by the device pages.

use crate::ui::style;
use crate::ui::window::Message;
use iced::border::Radius;
use iced::widget::rule::FillMode;
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::{
    Column, Row, Space, button, column, container, row, rule, scrollable, text, toggler::Toggler,
};
use iced::{Center, Element, Length, Padding, Theme};
use std::collections::BTreeSet;

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

/// A collapsible section of a page. Each one's open state is saved in
/// AppSettings::expanded_sections under its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Audio,
    PressAndHold,
    Accessibility,
    Connection,
    Microphone,
    DeviceInfo,
    StemControls,
    HiResMic,
}

impl Section {
    #[cfg(any(test, debug_assertions))]
    pub const ALL: [Section; 8] = [
        Section::Audio,
        Section::PressAndHold,
        Section::Accessibility,
        Section::Connection,
        Section::Microphone,
        Section::DeviceInfo,
        Section::StemControls,
        Section::HiResMic,
    ];

    /// The name saved in the settings file. Never change one: a section the
    /// user opened would silently close.
    pub fn key(self) -> &'static str {
        match self {
            Section::Audio => "airpods.audio",
            Section::PressAndHold => "airpods.press_and_hold",
            Section::Accessibility => "airpods.accessibility",
            Section::Connection => "airpods.connection",
            Section::Microphone => "airpods.microphone",
            Section::DeviceInfo => "airpods.device_info",
            Section::StemControls => "settings.stem_controls",
            Section::HiResMic => "settings.hires_mic",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Section::Audio => "Audio",
            Section::PressAndHold => "Press and Hold",
            Section::Accessibility => "Accessibility",
            Section::Connection => "Connection",
            Section::Microphone => "Microphone",
            Section::DeviceInfo => "Device Information",
            Section::StemControls => "Stem Controls",
            Section::HiResMic => "Hi-Res Microphone",
        }
    }
}

/// A card whose title row opens and closes it. The body is built only while
/// the section is open.
pub fn collapsible<'a>(
    section: Section,
    expanded: &BTreeSet<String>,
    body: impl FnOnce() -> Element<'a, Message>,
) -> Element<'a, Message> {
    let open = expanded.contains(section.key());
    let header = button(
        row![
            text(section.title()).size(17).style(style::text_accent),
            Space::new().width(Length::Fill),
            text(if open { "\u{25BE}" } else { "\u{25B8}" })
                .size(17)
                .style(style::text_muted),
        ]
        .align_y(Center),
    )
    .style(style::text_button)
    .padding(Padding::from([8.0, 8.0]))
    .width(Length::Fill)
    .on_press(Message::SectionToggled(section));
    let mut content = column![header];
    if open {
        content = content.push(container(body()).padding(Padding {
            bottom: 8.0,
            ..Padding::from([0.0, 8.0])
        }));
    }
    container(content)
        .padding(Padding::from([2.0, 10.0]))
        .width(Length::Fill)
        .style(style::card)
        .into()
}

/// Rows split by thin dividers.
pub fn divided<'a>(rows: Vec<Element<'a, Message>>) -> Column<'a, Message> {
    let mut list = column![].spacing(4);
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            list = list.push(divider());
        }
        list = list.push(row);
    }
    list
}

pub fn divider<'a>() -> Element<'a, Message> {
    rule::horizontal(1)
        .style(|theme: &Theme| rule::Style {
            color: theme.palette().text.scale_alpha(0.2),
            radius: Radius::from(12),
            fill_mode: FillMode::Full,
            snap: false,
        })
        .into()
}

/// The widest a page's content grows; a wider window centres it.
const PAGE_MAX_WIDTH: f32 = 720.0;

/// A page that scrolls vertically, so a short window cuts nothing off.
pub fn page<'a>(content: Column<'a, Message>) -> Element<'a, Message> {
    scrollable(
        container(content.spacing(16).max_width(PAGE_MAX_WIDTH))
            .padding(20)
            .center_x(Length::Fill),
    )
    .direction(Direction::Vertical(
        Scrollbar::new().width(6).scroller_width(6).spacing(2),
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_keys_are_unique() {
        let keys: BTreeSet<&str> = Section::ALL.iter().map(|s| s.key()).collect();
        assert_eq!(keys.len(), Section::ALL.len());
    }

    #[test]
    fn settings_without_open_sections_load_with_all_collapsed() {
        let settings: crate::utils::AppSettings =
            serde_json::from_str(r#"{"tray_text_mode": true}"#).unwrap();
        assert!(settings.tray_text_mode);
        assert!(settings.expanded_sections.is_empty());
    }
}
