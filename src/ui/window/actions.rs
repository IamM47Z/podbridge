//! Handlers for device actions: adding a device and device state changes.

use super::bluetooth::{device_type_from_file, noise_control_modes};
use super::{App, Message, Tab};
use crate::devices::enums::{DeviceData, DeviceState, DeviceType};
use bluer::Address;
use iced::Task;
use iced::widget::combo_box;
use std::collections::HashMap;

impl App {
    pub(super) fn got_paired_devices(&mut self, map: HashMap<String, Address>) -> Task<Message> {
        self.paired_devices = map;
        Task::none()
    }

    pub(super) fn start_add_device(&mut self, name: String, addr: Address) -> Task<Message> {
        self.pending_add_device = Some((name, addr));
        self.selected_device_type = None;
        Task::none()
    }

    pub(super) fn cancel_add_device(&mut self) -> Task<Message> {
        self.pending_add_device = None;
        self.selected_device_type = None;
        Task::none()
    }

    pub(super) fn confirm_add_device(&mut self) -> Task<Message> {
        if let Some((name, addr)) = self.pending_add_device.take()
            && let Some(type_) = self.selected_device_type.take()
        {
            crate::utils::update_devices(|devices| {
                devices.insert(
                    addr.to_string(),
                    DeviceData {
                        name,
                        type_,
                        information: None,
                    },
                );
                true
            });
            self.selected_tab = Tab::Device(addr.to_string());
        }
        Task::none()
    }

    pub(super) fn state_changed(&mut self, mac: String, state: DeviceState) -> Task<Message> {
        if let DeviceState::AirPods(a) = &state
            && a.hires_mic_enabled != self.hires_mic_enabled
        {
            self.hires_mic_enabled = a.hires_mic_enabled;
            self.save_settings();
        }
        self.device_states.insert(mac.clone(), state);
        // if airpods, update the noise control state combo box based on allow off mode
        let type_ = device_type_from_file(&mac);
        if let Some(DeviceType::AirPods) = type_
            && let Some(DeviceState::AirPods(state)) = self.device_states.get_mut(&mac)
        {
            state.noise_control_state =
                combo_box::State::new(noise_control_modes(state.allow_off_mode));
        }
        Task::none()
    }
}
