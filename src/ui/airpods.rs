use crate::bluetooth::aacp::{AACPManager, ControlCommandIdentifiers};
use crate::ui::style;
use crate::ui::widgets::{copyable, info_row, toggle_row};
use iced::Alignment::End;
use iced::border::Radius;
use iced::widget::rule::FillMode;
use iced::widget::{
    Column, Container, Row, Space, column, combo_box, container, row, rule, slider, text,
    text_input, toggler,
};
use iced::{Background, Border, Center, Color, Length, Padding, Theme};
use log::error;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use tokio::runtime::Runtime;
// use crate::bluetooth::att::ATTManager;
use crate::devices::enums::{
    AirPodsNoiseControlMode, AirPodsState, DeviceData, DeviceInformation, DeviceState,
};
use crate::ui::window::Message;

pub fn airpods_view<'a>(
    mac: &'a str,
    devices_list: &HashMap<String, DeviceData>,
    state: &'a AirPodsState,
    aacp_manager: Arc<AACPManager>,
    pause_convo: bool,
    // att_manager: Arc<ATTManager>
) -> Container<'a, Message> {
    // order: name, noise control, press and hold config, call controls (not sure if why it might be needed, adding it just in case), audio (personalized volume, conversational awareness, adaptive audio slider), connection settings, microphone, head gestures (not adding this), off listening mode, device information

    container(column![
        name_section(mac, state, &aacp_manager),
        Space::new().height(Length::from(20)),
        listening_mode_section(mac, state, &aacp_manager),
        Space::new().height(Length::from(20)),
        audio_section(mac, state, &aacp_manager, pause_convo),
        Space::new().height(Length::from(20)),
        off_listening_mode_section(mac, state, &aacp_manager),
        Space::new().height(Length::from(20)),
        hires_mic_section(mac, state, &aacp_manager),
        Space::new().height(Length::from(20)),
        information_section(mac, devices_list)
    ])
    .padding(20)
    .center_x(Length::Fill)
    .height(Length::Fill)
}

fn name_section<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp_manager: &Arc<AACPManager>,
) -> Container<'a, Message> {
    let aacp_manager_for_rename = aacp_manager.clone();
    let mac = mac.to_string();
    container(
        row![
            Space::new().width(10),
            text("Name").size(16).style(style::text_normal),
            Space::new().width(Length::Fill),
            text_input("", &state.device_name)
                .padding(Padding::from([5.0, 10.0]))
                .style(|theme: &Theme, _status| {
                    text_input::Style {
                        background: Background::Color(Color::TRANSPARENT),
                        border: Default::default(),
                        icon: Default::default(),
                        placeholder: theme.palette().text.scale_alpha(0.7),
                        value: theme.palette().text,
                        selection: Default::default(),
                    }
                })
                .align_x(End)
                .on_input({
                    let state = state.clone();
                    move |new_name| {
                        let aacp_manager = aacp_manager_for_rename.clone();
                        run_async_in_thread({
                            let new_name = new_name.clone();
                            async move {
                                aacp_manager
                                    .send_rename_packet(&new_name)
                                    .await
                                    .unwrap_or_else(|e| {
                                        log::error!("Failed to send rename packet: {}", e)
                                    });
                            }
                        });
                        let mut state = state.clone();
                        state.device_name = new_name.clone();
                        Message::StateChanged(mac.to_string(), DeviceState::AirPods(state))
                    }
                })
        ]
        .align_y(Center),
    )
    .padding(Padding::from([5.0, 10.0]))
    .style(style::card)
}

fn listening_mode_section<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp_manager: &Arc<AACPManager>,
) -> Container<'a, Message> {
    container(
        column![
            row![
                text("Listening Mode").size(16).style(style::text_normal),
                Space::new().width(Length::Fill),
                {
                    let state_clone = state.clone();
                    let mac = mac.to_string();
                    // this combo_box doesn't go really well with the design, but I am not writing my own dropdown menu for this
                    combo_box(
                        &state.noise_control_state,
                        "Select Listening Mode",
                        Some(&state.noise_control_mode.clone()),
                        {
                            let aacp_manager = aacp_manager.clone();
                            move |selected_mode| {
                                let aacp_manager = aacp_manager.clone();
                                let selected_mode_c = selected_mode.clone();
                                run_async_in_thread(async move {
                                    aacp_manager
                                        .send_control_command(
                                            ControlCommandIdentifiers::ListeningMode,
                                            &[selected_mode_c.to_byte()],
                                        )
                                        .await
                                        .unwrap_or_else(|e| {
                                            log::error!(
                                                "Failed to send Noise Control Mode command: {}",
                                                e
                                            )
                                        });
                                });
                                let mut state = state_clone.clone();
                                state.noise_control_mode = selected_mode.clone();
                                Message::StateChanged(mac.to_string(), DeviceState::AirPods(state))
                            }
                        },
                    )
                    .width(Length::from(200))
                    .input_style(style::combo_input)
                    .padding(Padding::from([5.0, 10.0]))
                    .menu_style(style::combo_menu)
                }
            ]
            .align_y(Center)
        ]
        .extend(adaptive_strength_row(mac, state)),
    )
    .padding(Padding::from([5.0, 18.0]))
    .style(style::card)
}

/// Adaptive mode's strength slider, like Apple's "Customize Adaptive Audio".
/// Shown only in Adaptive mode on AirPods that report the setting. The App
/// sends the level once the slider has been still for a moment.
fn adaptive_strength_row<'a>(
    mac: &str,
    state: &'a AirPodsState,
) -> Option<iced::Element<'a, Message>> {
    if !matches!(state.noise_control_mode, AirPodsNoiseControlMode::Adaptive) {
        return None;
    }
    let level = state.adaptive_strength?;
    let mac = mac.to_string();
    Some(
        row![
            text("Less noise").size(14).style(style::text_muted),
            slider(0.0..=100.0, slider_from_level(level), move |value: f32| {
                Message::AdaptiveStrengthChanged(mac.clone(), level_from_slider(value))
            })
            .step(1.0_f32),
            text("More noise").size(14).style(style::text_muted),
        ]
        .spacing(12)
        .align_y(Center)
        .padding(Padding::from([8.0, 0.0]))
        .into(),
    )
}

/// The slider runs from "Less noise" (0) to "More noise" (100); the AirPods'
/// level runs the other way.
fn slider_from_level(level: u8) -> f32 {
    100.0 - f32::from(level.min(100))
}

fn level_from_slider(value: f32) -> u8 {
    (100.0 - value.clamp(0.0, 100.0)).round() as u8
}

fn audio_section<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp_manager: &Arc<AACPManager>,
    pause_convo: bool,
) -> Column<'a, Message> {
    column![
        container(text("Audio Settings").size(18).style(style::text_accent))
            .padding(Padding::from([5.0, 18.0])),
        container(
            column![
                personalized_volume_row(mac, state, aacp_manager),
                rule::horizontal(1).style(|theme: &Theme| {
                    rule::Style {
                        color: theme.palette().text.scale_alpha(0.2),
                        radius: Radius::from(12),
                        fill_mode: FillMode::Full,
                        snap: false,
                    }
                }),
                conversation_awareness_row(mac, state, aacp_manager, pause_convo)
            ]
            .spacing(4)
            .padding(8)
        )
        .padding(Padding::from([5.0, 10.0]))
        .style(style::card)
    ]
}

fn personalized_volume_row<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp_manager: &Arc<AACPManager>,
) -> Row<'a, Message> {
    let aacp_manager_pv = aacp_manager.clone();
    toggle_row(
        "Personalized Volume",
        "Adjusts the volume in response to your environment.",
        toggler(state.personalized_volume_enabled).on_toggle({
            let mac = mac.to_string();
            let state = state.clone();
            move |is_enabled| {
                let aacp_manager = aacp_manager_pv.clone();
                let mac = mac.clone();
                run_async_in_thread(async move {
                    aacp_manager
                        .send_control_command(
                            ControlCommandIdentifiers::AdaptiveVolumeConfig,
                            if is_enabled { &[0x01] } else { &[0x02] },
                        )
                        .await
                        .unwrap_or_else(|e| {
                            log::error!("Failed to send Personalized Volume command: {}", e)
                        });
                });
                let mut state = state.clone();
                state.personalized_volume_enabled = is_enabled;
                Message::StateChanged(mac, DeviceState::AirPods(state))
            }
        }),
        8,
    )
}

fn conversation_awareness_row<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp_manager: &Arc<AACPManager>,
    pause_convo: bool,
) -> Row<'a, Message> {
    let aacp_manager_conv_detect = aacp_manager.clone();
    let mac = mac.to_string();
    // Locked while a capture manages it, so the user can't fight the override.
    let convo_toggler = toggler(state.conversation_awareness_enabled);
    let convo_toggler = if pause_convo && aacp_manager.mic_active() {
        convo_toggler
    } else {
        convo_toggler.on_toggle(move |is_enabled| {
            let aacp_manager = aacp_manager_conv_detect.clone();
            run_async_in_thread(async move {
                aacp_manager
                    .set_conversation_detection_by_user(is_enabled)
                    .await;
            });
            let mut state = state.clone();
            state.conversation_awareness_enabled = is_enabled;
            Message::StateChanged(mac.to_string(), DeviceState::AirPods(state))
        })
    };
    toggle_row(
        "Conversation Awareness",
        "Lowers the volume of your audio when it detects that you are speaking.",
        convo_toggler,
        8,
    )
}

fn off_listening_mode_section<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp_manager: &Arc<AACPManager>,
) -> Container<'a, Message> {
    let aacp_manager_olm = aacp_manager.clone();
    let mac = mac.to_string();
    container(toggle_row(
        "Off Listening Mode",
        "When this is on, AirPods listening modes will include an Off option. Loud sound levels are not reduced when listening mode is set to Off.",
        toggler(state.allow_off_mode).on_toggle(move |is_enabled| {
            let aacp_manager = aacp_manager_olm.clone();
            run_async_in_thread(async move {
                aacp_manager
                    .send_control_command(
                        ControlCommandIdentifiers::AllowOffOption,
                        if is_enabled { &[0x01] } else { &[0x02] },
                    )
                    .await
                    .unwrap_or_else(|e| {
                        log::error!("Failed to send Off Listening Mode command: {}", e)
                    });
            });
            let mut state = state.clone();
            state.allow_off_mode = is_enabled;
            Message::StateChanged(mac.to_string(), DeviceState::AirPods(state))
        }),
        8,
    ))
    .padding(Padding::from([5.0, 18.0]))
    .style(style::card)
}

fn hires_mic_section<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp_manager: &Arc<AACPManager>,
) -> Container<'a, Message> {
    let aacp_manager_mic = aacp_manager.clone();
    let mac = mac.to_string();
    let state = state.clone();
    let mic_active = aacp_manager.mic_active();
    let mic_app = aacp_manager.mic_app();
    let level = aacp_manager.mic_level().clamp(0.0, 1.0);

    let header = toggle_row(
        "Hi-Res Microphone",
        "Captures the AirPods' high-quality AAC-ELD microphone stream and exposes it as an 'AirPodsHiRes' input.",
        toggler(state.hires_mic_enabled).on_toggle(move |is_enabled| {
            let aacp_manager = aacp_manager_mic.clone();
            run_async_in_thread(async move {
                aacp_manager.set_hires_mic_enabled(is_enabled).await;
            });
            let mut state = state.clone();
            state.hires_mic_enabled = is_enabled;
            Message::StateChanged(mac.to_string(), DeviceState::AirPods(state))
        }),
        8,
    );

    let mut content = column![header].spacing(10);
    if mic_active {
        content = content.push(level_meter(level, mic_app));
    }

    container(content)
        .padding(Padding::from([5.0, 18.0]))
        .style(style::card)
}

fn information_section<'a>(
    mac: &str,
    devices_list: &HashMap<String, DeviceData>,
) -> Column<'a, Message> {
    let mut information_col = column![];
    if let Some(device) = devices_list.get(mac) {
        if let Some(DeviceInformation::AirPods(ref airpods_info)) = device.information {
            let info_rows = column![
                info_row(
                    "Model Number",
                    text(airpods_info.model_number.clone()).size(16)
                ),
                info_row(
                    "Manufacturer",
                    text(airpods_info.manufacturer.clone()).size(16)
                ),
                info_row("Serial Number", copyable(&airpods_info.serial_number)),
                info_row(
                    "Left Serial Number",
                    copyable(&airpods_info.left_serial_number)
                ),
                info_row(
                    "Right Serial Number",
                    copyable(&airpods_info.right_serial_number)
                ),
                info_row("Version 1", text(airpods_info.version1.clone()).size(16)),
                info_row("Version 2", text(airpods_info.version2.clone()).size(16)),
                info_row("Version 3", text(airpods_info.version3.clone()).size(16))
            ]
            .spacing(4)
            .padding(8);

            information_col = column![
                container(
                    text("Device Information")
                        .size(18)
                        .style(style::text_accent)
                )
                .padding(Padding::from([5.0, 18.0])),
                container(info_rows)
                    .padding(Padding::from([5.0, 10.0]))
                    .style(style::card)
            ];
        } else {
            error!(
                "Expected AirPodsInformation for device {}, got something else",
                mac
            );
        }
    }
    information_col
}

fn level_meter<'a>(level: f32, app: Option<String>) -> Container<'a, Message> {
    let filled = (level * 1000.0).round() as u16;
    let rest = 1000u16.saturating_sub(filled);
    let hot = level >= 0.9;
    let label = match app {
        Some(app) => format!("In use by {}", app),
        None => "Input level".to_string(),
    };

    let bar = container(
        row![
            container(Space::new())
                .width(Length::FillPortion(filled))
                .height(Length::Fill)
                .style(move |theme: &Theme| {
                    let color = if hot {
                        theme.palette().warning
                    } else {
                        theme.palette().success
                    };
                    container::Style {
                        background: Some(Background::Color(color)),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    }
                }),
            container(Space::new()).width(Length::FillPortion(rest)),
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::from(10))
    .style(|theme: &Theme| container::Style {
        background: Some(Background::Color(theme.palette().text.scale_alpha(0.12))),
        border: Border::default().rounded(4),
        ..Default::default()
    });

    container(column![text(label).size(12).style(style::text_muted), bar].spacing(4))
        .width(Length::Fill)
        .padding(Padding {
            bottom: 6.0,
            ..Padding::ZERO
        })
}

fn run_async_in_thread<F>(fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(fut);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_runs_from_less_noise_to_more_noise() {
        // Level 100 filters the most noise: the "Less noise" end, slider 0.
        assert_eq!(slider_from_level(100), 0.0);
        assert_eq!(slider_from_level(0), 100.0);
        assert_eq!(level_from_slider(0.0), 100);
        assert_eq!(level_from_slider(100.0), 0);
        assert_eq!(level_from_slider(slider_from_level(37)), 37);
    }

    #[test]
    fn out_of_range_levels_are_clamped() {
        assert_eq!(slider_from_level(200), 0.0);
        assert_eq!(level_from_slider(-5.0), 100);
        assert_eq!(level_from_slider(150.0), 0);
    }
}
