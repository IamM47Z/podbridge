use super::{App, Message, Tab};
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::enums::{DeviceData, DeviceState, DeviceType};
use crate::ui::airpods::airpods_view;
use crate::ui::nothing::nothing_view;
use iced::widget::{container, text};
use iced::{Element, Length};
use log::{debug, error};
use std::collections::HashMap;

impl App {
    /// The page of the selected tab.
    pub(super) fn content<'a>(
        &'a self,
        devices_list: &HashMap<String, DeviceData>,
    ) -> Element<'a, Message> {
        let device_managers = self.device_managers.blocking_read();
        match &self.selected_tab {
            Tab::Device(id) => {
                if id == "none" {
                    centered_message("Select a device")
                } else {
                    self.device_page(id, devices_list, &device_managers)
                }
            }
            Tab::Settings => self.settings_page(),
            Tab::AddDevice => self.add_device_page(devices_list).into(),
        }
    }

    fn device_page<'a>(
        &'a self,
        id: &'a str,
        devices_list: &HashMap<String, DeviceData>,
        device_managers: &HashMap<String, DeviceManagers>,
    ) -> Element<'a, Message> {
        let device_type = devices_list.get(id).map(|d| d.type_.clone());
        let device_state = self.device_states.get(id);
        debug!(
            "Rendering device view for {}: type={:?}, state={:?}",
            id, device_type, device_state
        );
        match device_type {
            Some(DeviceType::AirPods) => self.airpods_page(id, devices_list, device_managers),
            Some(DeviceType::Nothing) => {
                nothing_page(id, devices_list, device_state, device_managers)
            }
            _ => centered_message("Unsupported device"),
        }
    }

    fn airpods_page<'a>(
        &'a self,
        id: &'a str,
        devices_list: &HashMap<String, DeviceData>,
        device_managers: &HashMap<String, DeviceManagers>,
    ) -> Element<'a, Message> {
        let state = match self.device_states.get(id) {
            Some(DeviceState::AirPods(state)) => Some(state),
            _ => None,
        };
        state
            .and_then(|state| {
                device_managers.get(id).and_then(|managers| {
                    managers.get_aacp().map(|aacp_manager| {
                        airpods_view(
                            id,
                            devices_list,
                            state,
                            aacp_manager.clone(),
                            self.hires_mic_pause_convo,
                            &self.expanded_sections,
                        )
                    })
                })
            })
            .unwrap_or_else(|| {
                centered_message("Required managers or state not available for this AirPods device")
            })
    }
}

fn nothing_page<'a>(
    id: &'a str,
    devices_list: &HashMap<String, DeviceData>,
    device_state: Option<&'a DeviceState>,
    device_managers: &HashMap<String, DeviceManagers>,
) -> Element<'a, Message> {
    let Some(DeviceState::Nothing(state)) = device_state else {
        return centered_message("No state available for this Nothing device");
    };
    let Some(device_managers) = device_managers.get(id) else {
        error!("No manager found for Nothing device {}", id);
        return centered_message("No manager found for this Nothing device");
    };
    let Some(att_manager) = device_managers.get_att() else {
        error!("No ATT manager found for Nothing device {}", id);
        return centered_message("No valid ATT manager found for this Nothing device");
    };
    nothing_view(id, devices_list, state, att_manager.clone())
}

fn centered_message<'a>(message: &str) -> Element<'a, Message> {
    container(text(message.to_string()).size(16).center())
        .padding(20)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}
