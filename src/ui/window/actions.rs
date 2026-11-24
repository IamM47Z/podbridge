//! Handlers for device actions: adding a device and device state changes.

use super::bluetooth::{device_type_from_file, noise_control_modes};
use super::{App, Message, Tab};
use crate::bluetooth::aacp::ControlCommandIdentifiers;
use crate::devices::enums::{DeviceData, DeviceState, DeviceType};
use crate::devices::{controls, press_and_hold};
use crate::ui::messages::BluetoothUIMessage;
use bluer::Address;
use iced::Task;
use iced::widget::combo_box;
use log::error;
use std::collections::HashMap;
use std::time::Duration;

const SLIDER_DEBOUNCE: Duration = Duration::from_millis(300);

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

    /// Show and send a setting's new value at once.
    pub(super) fn control_changed(
        &mut self,
        mac: String,
        identifier: ControlCommandIdentifiers,
        value: u8,
    ) -> Task<Message> {
        if let Some(DeviceState::AirPods(state)) = self.device_states.get_mut(&mac) {
            state.record_setting(identifier, &[value]);
        }
        if controls::stored_default(identifier).is_some() {
            crate::utils::AppSettings::update(|s| {
                s.stored_controls
                    .entry(mac.clone())
                    .or_default()
                    .insert(identifier as u8, value);
            });
        }
        self.send_control_command(&mac, identifier, value)
    }

    /// Show a slider's new value at once; send it once the slider has been
    /// still for SLIDER_DEBOUNCE, like the Android client. Covers drags, taps,
    /// arrow keys and scrolling alike.
    pub(super) fn slider_moved(
        &mut self,
        mac: String,
        identifier: ControlCommandIdentifiers,
        value: u8,
    ) -> Task<Message> {
        if let Some(DeviceState::AirPods(state)) = self.device_states.get_mut(&mac) {
            state.record_setting(identifier, &[value]);
        }
        let generation = self.slider_generations.entry(identifier).or_default();
        *generation += 1;
        let generation = *generation;
        Task::perform(tokio::time::sleep(SLIDER_DEBOUNCE), move |_| {
            Message::SliderSettled(mac.clone(), identifier, value, generation)
        })
    }

    pub(super) fn slider_settled(
        &self,
        mac: &str,
        identifier: ControlCommandIdentifiers,
        value: u8,
        generation: u64,
    ) -> Task<Message> {
        if self.slider_generations.get(&identifier) != Some(&generation) {
            return Task::none();
        }
        self.send_control_command(mac, identifier, value)
    }

    /// Flip one press-and-hold mode, unless that would leave fewer than two.
    /// The choice is stored so the next connect sends it again: the AirPods
    /// reset it when they join an Apple device that is not iCloud-synced.
    pub(super) fn press_and_hold_toggled(&mut self, mac: String, mode: u8) -> Task<Message> {
        let Some(DeviceState::AirPods(state)) = self.device_states.get_mut(&mac) else {
            return Task::none();
        };
        let Some(mask) = state.press_and_hold_modes else {
            return Task::none();
        };
        let new_mask = press_and_hold::toggle(mask, mode, state.allow_off_mode);
        if new_mask == mask {
            return Task::none();
        }
        state.press_and_hold_modes = Some(new_mask);
        crate::utils::AppSettings::update(|s| {
            s.press_and_hold_modes.insert(mac.clone(), new_mask);
        });
        self.send_control_command(
            &mac,
            ControlCommandIdentifiers::ListeningModeConfigs,
            new_mask,
        )
    }

    fn send_control_command(
        &self,
        mac: &str,
        identifier: ControlCommandIdentifiers,
        value: u8,
    ) -> Task<Message> {
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
                let payload = controls::payload(identifier, value);
                match aacp.send_control_command(identifier, &payload).await {
                    // Whether the AirPods echo each setting is unverified;
                    // record it so the backend acts on it either way.
                    Ok(()) => aacp.record_control_status(identifier, value).await,
                    Err(e) => error!("Failed to send {:?}: {}", identifier, e),
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
