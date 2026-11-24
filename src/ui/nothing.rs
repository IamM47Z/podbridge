use crate::bluetooth::att::{ATTHandles, ATTManager};
use crate::devices::enums::{DeviceData, DeviceInformation, DeviceState, NothingState};
use crate::ui::style;
use crate::ui::widgets::{info_row, page};
use crate::ui::window::Message;
use iced::widget::combo_box;
use iced::widget::{Column, Container, Space, column, container, row, text};
use iced::{Element, Length, Theme};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use tokio::runtime::Runtime;

pub fn nothing_view<'a>(
    mac: &'a str,
    devices_list: &HashMap<String, DeviceData>,
    state: &'a NothingState,
    att_manager: Arc<ATTManager>,
) -> Element<'a, Message> {
    page(column![
        noise_control_section(mac, state, att_manager),
        container(information_section(mac, devices_list))
            .style(|theme: &Theme| style::outlined_panel(theme, 20.0))
            .padding(20)
            .width(Length::Fill)
    ])
}

fn information_section<'a>(
    mac: &str,
    devices_list: &HashMap<String, DeviceData>,
) -> Column<'a, Message> {
    let mut information_col = column![];
    if let Some(device) = devices_list.get(mac)
        && let Some(DeviceInformation::Nothing(ref nothing_info)) = device.information
    {
        information_col = information_col
            .push(
                text("Device Information")
                    .size(18)
                    .style(style::text_accent),
            )
            .push(Space::new().height(Length::from(10)))
            .push(info_row(
                "Serial Number",
                text(nothing_info.serial_number.clone()).size(16),
            ))
            .push(info_row(
                "Firmware Version",
                text(nothing_info.firmware_version.clone()).size(16),
            ));
    }
    information_col
}

fn noise_control_section<'a>(
    mac: &str,
    state: &'a NothingState,
    att_manager: Arc<ATTManager>,
) -> Container<'a, Message> {
    container(
        row![
            text("Noise Control Mode")
                .size(16)
                .style(style::text_normal)
                .width(Length::Fill),
            {
                let state_clone = state.clone();
                let mac = mac.to_string();
                let att_manager_clone = att_manager.clone();
                combo_box(
                    &state.anc_mode_state,
                    "Select Noise Control Mode",
                    Some(&state.anc_mode.clone()),
                    {
                        move |selected_mode| {
                            let att_manager = att_manager_clone.clone();
                            let selected_mode_c = selected_mode.clone();
                            let mac_s = mac.clone();
                            run_async_in_thread(async move {
                                if let Err(e) = att_manager
                                    .write(
                                        ATTHandles::NothingEverything,
                                        &[
                                            0x55,
                                            0x60,
                                            0x01,
                                            0x0F,
                                            0xF0,
                                            0x03,
                                            0x00,
                                            0x00,
                                            0x01,
                                            selected_mode_c.to_byte(),
                                            0x00,
                                            0x00,
                                            0x00,
                                        ],
                                    )
                                    .await
                                {
                                    log::error!(
                                        "Failed to set noise cancellation mode for device {}: {}",
                                        mac_s,
                                        e
                                    );
                                }
                            });
                            let mut state = state_clone.clone();
                            state.anc_mode = selected_mode.clone();
                            Message::StateChanged(mac.to_string(), DeviceState::Nothing(state))
                        }
                    },
                )
                .width(Length::from(200))
                .input_style(style::combo_input)
                .padding(iced::Padding::from([5.0, 10.0]))
                .menu_style(style::combo_menu)
            }
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
    )
    .padding(iced::Padding::from([5.0, 18.0]))
    .style(style::card)
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
