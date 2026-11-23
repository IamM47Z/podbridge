//! Handlers for device actions: adding a device and device state changes.

use super::bluetooth::{device_type_from_file, noise_control_modes};
use super::{App, Message, Tab};
use crate::bluetooth::aacp::ControlCommandIdentifiers;
use crate::devices::enums::{DeviceData, DeviceState, DeviceType};
use crate::ui::messages::BluetoothUIMessage;
use bluer::Address;
use iced::Task;
use iced::widget::combo_box;
use log::error;
use std::collections::HashMap;
use std::time::Duration;

const ADAPTIVE_STRENGTH_DEBOUNCE: Duration = Duration::from_millis(300);

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
            // shortcut: the backend starts a device when it connects, so one that is
            // connected while it is added is managed from its next connection. To
            // manage it at once, send the backend a message to set it up.
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

    /// Show the new level at once; send it once the slider has been still for
    /// ADAPTIVE_STRENGTH_DEBOUNCE, like the Android client. Covers drags, taps,
    /// arrow keys and scrolling alike.
    pub(super) fn adaptive_strength_changed(&mut self, mac: String, level: u8) -> Task<Message> {
        if let Some(DeviceState::AirPods(state)) = self.device_states.get_mut(&mac) {
            state.adaptive_strength = Some(level);
        }
        self.adaptive_strength_generation += 1;
        let generation = self.adaptive_strength_generation;
        Task::perform(tokio::time::sleep(ADAPTIVE_STRENGTH_DEBOUNCE), move |_| {
            Message::AdaptiveStrengthSettled(mac.clone(), generation)
        })
    }

    pub(super) fn adaptive_strength_settled(&self, mac: &str, generation: u64) -> Task<Message> {
        if generation != self.adaptive_strength_generation {
            return Task::none();
        }
        let Some(DeviceState::AirPods(state)) = self.device_states.get(mac) else {
            return Task::none();
        };
        let Some(level) = state.adaptive_strength else {
            return Task::none();
        };
        let Some(aacp) = self
            .device_managers
            .blocking_read()
            .get(mac)
            .and_then(|m| m.get_aacp())
        else {
            return Task::none();
        };
        Task::perform(
            async move {
                if let Err(e) = aacp
                    .send_control_command(ControlCommandIdentifiers::AutoAncStrength, &[level])
                    .await
                {
                    error!("Failed to send adaptive strength: {}", e);
                }
            },
            |_| Message::Bluetooth(BluetoothUIMessage::NoOp),
        )
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
