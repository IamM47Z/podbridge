use super::{App, Message};
use crate::ui::style;
use crate::ui::widgets::toggle_row;
use crate::utils::StemAction;
use iced::Task;
use iced::widget::{
    Column, Container, Space, column, combo_box, container, pick_list, row, text, toggler,
};
use iced::{Center, Length, Padding};

impl App {
    /// Applies one edit to the window's settings and persists it.
    pub(super) fn change_setting(&mut self, apply: impl FnOnce(&mut Self)) -> Task<Message> {
        apply(self);
        self.save_settings();
        Task::none()
    }

    pub(super) fn settings_page(&self) -> Container<'_, Message> {
        container(column![
            self.appearance_section(),
            Space::new().height(Length::from(20)),
            setting_toggle(
                "Use text in tray",
                "Use text for battery status in tray instead of a progress bar.",
                self.tray_text_mode,
                Message::TrayTextModeChanged,
            ),
            Space::new().height(Length::from(20)),
            self.controls_section(),
            Space::new().height(Length::from(20)),
            setting_toggle(
                "Switch audio away when both buds are out",
                "After both buds have been out for 4 seconds, apps move to another output and microphone (speakers, headset, webcam). They move back to the AirPods when a bud goes in again. Media pauses right away either way.",
                self.audio_off_when_not_wearing,
                Message::AudioOffWhenNotWearingChanged,
            ),
            Space::new().height(Length::from(20)),
            setting_toggle(
                "Pause when one bud is removed",
                "Pause media when you take one bud out, like an iPhone, and resume when it goes back in. When off, playback continues in the other bud. With both buds out, media stops and does not resume.",
                self.pause_when_one_removed,
                Message::PauseWhenOneRemovedChanged,
            ),
            Space::new().height(Length::from(20)),
            setting_toggle(
                "Reset A2DP transport",
                "Briefly pauses and resumes the AirPods audio after the hi-res mic turns off. Without it, playback continues in only one earbud until the mic is used again or the AirPods reconnect.",
                self.a2dp_reset,
                Message::A2dpResetChanged,
            ),
            Space::new().height(Length::from(20)),
            setting_toggle(
                "Hi-res mic auto gain",
                "Automatically normalizes the hi-res microphone level. For most usecases this should remain on. Disable for a raw, unprocessed capture.",
                self.hires_mic_agc,
                Message::HiResMicAgcChanged,
            ),
            Space::new().height(Length::from(20)),
            setting_toggle(
                "Pause conversation awareness during capture",
                "Turns off conversation awareness while the hi-res microphone is capturing, then restores it afterwards.",
                self.hires_mic_pause_convo,
                Message::HiResMicPauseConvoChanged,
            ),
        ])
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill)
    }

    fn appearance_section(&self) -> Column<'_, Message> {
        column![
            heading("Appearance"),
            container(
                row![
                    text("Theme").size(16),
                    Space::new().width(Length::Fill),
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
                .align_y(Center)
            )
            .padding(Padding::from([5.0, 18.0]))
            .style(style::card)
        ]
        .spacing(12)
    }
}

impl App {
    fn controls_section(&self) -> Column<'_, Message> {
        let mut section = column![
            heading("Controls"),
            setting_toggle(
                "Stem press control",
                "podbridge handles double and triple presses with the actions below. Turning this on or off takes effect on the next connect. A changed action applies at once. Disable if your environment handles AirPods AVRCP commands natively.",
                self.stem_control,
                Message::StemControlChanged,
            )
        ]
        .spacing(12);
        if self.stem_control {
            section = section
                .push(stem_action_row(
                    "Double press",
                    self.stem_double_press,
                    Message::StemDoublePressChanged,
                ))
                .push(stem_action_row(
                    "Triple press",
                    self.stem_triple_press,
                    Message::StemTriplePressChanged,
                ));
        }
        section
    }
}

fn stem_action_row<'a>(
    label: &'a str,
    selected: StemAction,
    on_select: fn(StemAction) -> Message,
) -> Container<'a, Message> {
    container(
        row![
            text(label).size(16),
            Space::new().width(Length::Fill),
            pick_list(StemAction::ALL, Some(selected), on_select)
                .padding(Padding::from([5.0, 10.0]))
                .width(Length::from(200))
        ]
        .align_y(Center),
    )
    .padding(Padding::from([5.0, 18.0]))
    .style(style::card)
}

fn heading(title: &str) -> Container<'_, Message> {
    container(text(title).size(20).style(style::text_accent)).padding(Padding::from([0.0, 18.0]))
}

fn setting_toggle<'a>(
    title: &'a str,
    description: &'a str,
    value: bool,
    on_toggle: fn(bool) -> Message,
) -> Container<'a, Message> {
    container(toggle_row(
        title,
        description,
        toggler(value).on_toggle(on_toggle),
        12,
    ))
    .padding(Padding::from([5.0, 18.0]))
    .style(style::card)
    .align_y(Center)
}
