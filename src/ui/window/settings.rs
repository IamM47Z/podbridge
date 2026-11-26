use super::{App, Message};
use crate::ui::style;
use crate::ui::widgets::{Section, collapsible, divided, page, toggle_row};
use crate::utils::StemAction;
use iced::Task;
use iced::widget::{
    Column, Container, column, combo_box, container, pick_list, row, text, toggler,
};
use iced::{Center, Element, Length, Padding};

impl App {
    /// Applies one edit to the window's settings and persists it.
    pub(super) fn change_setting(&mut self, apply: impl FnOnce(&mut Self)) -> Task<Message> {
        apply(self);
        self.save_settings();
        Task::none()
    }

    /// The everyday settings first; the stem and hi-res microphone ones in
    /// sections that stay collapsed until opened.
    pub(super) fn settings_page(&self) -> Element<'_, Message> {
        page(column![
            heading("Appearance"),
            group(vec![
                self.theme_row(),
                plain_toggle(
                    "Use text in tray",
                    "Use text for battery status in tray instead of a progress bar.",
                    self.tray_text_mode,
                    Message::TrayTextModeChanged,
                ),
            ]),
            heading("Playback"),
            group(vec![
                plain_toggle(
                    "Switch audio away when both buds are out",
                    "After both buds have been out for 4 seconds, apps move to another output and microphone (speakers, headset, webcam). They move back to the AirPods when a bud goes in again. Media pauses right away either way.",
                    self.audio_off_when_not_wearing,
                    Message::AudioOffWhenNotWearingChanged,
                ),
                plain_toggle(
                    "Pause when one bud is removed",
                    "Pause media when you take one bud out, like an iPhone, and resume when it goes back in. When off, playback continues in the other bud. With both buds out, media stops and does not resume.",
                    self.pause_when_one_removed,
                    Message::PauseWhenOneRemovedChanged,
                ),
                plain_toggle(
                    "Hand AirPods back when done",
                    "After this computer took the AirPods from your iPhone or another device, give them back once nothing here has played or used the mic for 30 seconds. Never during a call.",
                    self.hand_back_when_idle,
                    Message::HandBackWhenIdleChanged,
                ),
            ]),
            collapsible(Section::StemControls, &self.expanded_sections, || {
                self.stem_controls().into()
            }),
            collapsible(Section::HiResMic, &self.expanded_sections, || {
                divided(vec![
                    plain_toggle(
                        "Hi-res mic auto gain",
                        "Automatically normalizes the hi-res microphone level. For most usecases this should remain on. Disable for a raw, unprocessed capture.",
                        self.hires_mic_agc,
                        Message::HiResMicAgcChanged,
                    ),
                    plain_toggle(
                        "Pause conversation awareness during capture",
                        "Turns off conversation awareness while the hi-res microphone is capturing, then restores it afterwards.",
                        self.hires_mic_pause_convo,
                        Message::HiResMicPauseConvoChanged,
                    ),
                    plain_toggle(
                        "Reset A2DP transport",
                        "Briefly pauses and resumes the AirPods audio after the hi-res mic turns off. Without it, playback continues in only one earbud until the mic is used again or the AirPods reconnect.",
                        self.a2dp_reset,
                        Message::A2dpResetChanged,
                    ),
                ])
                .into()
            }),
        ])
    }

    fn theme_row(&self) -> Element<'_, Message> {
        row![
            text("Theme").size(16).width(Length::Fill),
            combo_box(
                &self.theme_state,
                "Select theme",
                Some(&self.selected_theme),
                Message::ThemeSelected
            )
            .input_style(style::combo_input)
            .menu_style(style::combo_menu)
            .padding(Padding::from([5.0, 10.0]))
            .width(Length::from(200))
        ]
        .spacing(12)
        .align_y(Center)
        .padding(Padding::from([4.0, 0.0]))
        .into()
    }
}

impl App {
    fn stem_controls(&self) -> Column<'_, Message> {
        let mut rows = vec![plain_toggle(
            "Stem press control",
            "podbridge always plays and pauses on a single press, or mutes the AirPods microphone while an app records from it. Turn this on to let it handle double and triple presses too, with the actions below. Turning it on or off takes effect on the next connect. A changed action applies at once. Off, the AirPods send their own next and previous track commands.",
            self.stem_control,
            Message::StemControlChanged,
        )];
        if self.stem_control {
            rows.push(stem_action_row(
                "Double press",
                self.stem_double_press,
                Message::StemDoublePressChanged,
            ));
            rows.push(stem_action_row(
                "Triple press",
                self.stem_triple_press,
                Message::StemTriplePressChanged,
            ));
        }
        divided(rows)
    }
}

fn stem_action_row<'a>(
    label: &'a str,
    selected: StemAction,
    on_select: fn(StemAction) -> Message,
) -> Element<'a, Message> {
    row![
        text(label).size(16).width(Length::Fill),
        pick_list(StemAction::ALL, Some(selected), on_select)
            .padding(Padding::from([5.0, 10.0]))
            .width(Length::from(200))
    ]
    .spacing(8)
    .align_y(Center)
    .padding(Padding::from([4.0, 0.0]))
    .into()
}

/// A toggle row for inside a card.
fn plain_toggle<'a>(
    title: &'a str,
    description: &'a str,
    value: bool,
    on_toggle: fn(bool) -> Message,
) -> Element<'a, Message> {
    toggle_row(title, description, toggler(value).on_toggle(on_toggle), 12).into()
}

fn heading(title: &str) -> Container<'_, Message> {
    container(text(title).size(20).style(style::text_accent)).padding(Padding::from([0.0, 18.0]))
}

/// Rows in one card, split by dividers.
fn group(rows: Vec<Element<'_, Message>>) -> Container<'_, Message> {
    container(divided(rows))
        .padding(Padding::from([10.0, 18.0]))
        .width(Length::Fill)
        .style(style::card)
}
