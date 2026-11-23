use super::{App, Message};
use crate::ui::style;
use crate::ui::widgets::toggle_row;
use iced::Task;
use iced::widget::{Column, Container, Space, column, combo_box, container, row, text, toggler};
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
            controls_section(self.stem_control),
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
                "Pause media when you take one bud out, like an iPhone, and resume when it goes back in. When off, playback continues in the other bud.",
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

fn controls_section<'a>(stem_control: bool) -> Column<'a, Message> {
    column![
        heading("Controls"),
        setting_toggle(
            "Stem press track control",
            "Double press = next track, triple press = previous track. Disable if your environment handles AirPods AVRCP commands natively.",
            stem_control,
            Message::StemControlChanged,
        )
    ]
    .spacing(12)
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
