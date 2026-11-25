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
/// How long the name box rests before its text is sent as the new name. Apple
/// commits on Return or on leaving the field; iced reports no blur, so this
/// stands in for it.
const NAME_DEBOUNCE: Duration = Duration::from_secs(1);

/// Whether `name` is worth sending: not empty, and not the name the device
/// already has (`current`).
fn name_needs_sending(name: &str, current: Option<&str>) -> bool {
    !name.trim().is_empty() && current != Some(name)
}

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

    /// Show the typed name at once; send it on Enter or once typing has been
    /// still for NAME_DEBOUNCE, whichever comes first.
    pub(super) fn name_edited(&mut self, mac: String, name: String) -> Task<Message> {
        if let Some(DeviceState::AirPods(state)) = self.device_states.get_mut(&mac) {
            state.device_name = name;
        }
        let generation = self.name_generations.entry(mac.clone()).or_default();
        *generation += 1;
        let generation = *generation;
        Task::perform(tokio::time::sleep(NAME_DEBOUNCE), move |_| {
            Message::NameSettled(mac.clone(), generation)
        })
    }

    pub(super) fn name_settled(&mut self, mac: String, generation: u64) -> Task<Message> {
        if self.name_generations.get(&mac) != Some(&generation) {
            return Task::none();
        }
        self.send_name(mac)
    }

    pub(super) fn name_submitted(&mut self, mac: String) -> Task<Message> {
        // Makes the pending timer stale, so the name is not sent twice.
        *self.name_generations.entry(mac.clone()).or_default() += 1;
        self.send_name(mac)
    }

    /// Rename the AirPods and their BlueZ alias, then the devices file so the
    /// device list shows the name without a reconnect.
    fn send_name(&mut self, mac: String) -> Task<Message> {
        let Some(DeviceState::AirPods(state)) = self.device_states.get(&mac) else {
            return Task::none();
        };
        let name = state.device_name.clone();
        let current = self.sent_names.get(&mac).cloned().or_else(|| {
            super::read_devices_list()
                .get(&mac)
                .map(|device| device.name.clone())
        });
        if !name_needs_sending(&name, current.as_deref()) {
            return Task::none();
        }
        let Some(aacp) = self
            .device_managers
            .blocking_read()
            .get(&mac)
            .and_then(|m| m.get_aacp())
        else {
            return Task::none();
        };
        self.sent_names.insert(mac.clone(), name.clone());
        Task::perform(
            async move {
                if let Err(e) = aacp.send_rename_packet(&name).await {
                    error!("Failed to send rename packet: {}", e);
                    return;
                }
                if let Err(e) = crate::bluetooth::discovery::sync_alias(&mac, &name).await {
                    error!("Failed to set the BlueZ alias: {}", e);
                }
                let saved = tokio::task::spawn_blocking(move || {
                    crate::utils::update_devices(|devices| match devices.get_mut(&mac) {
                        Some(device) if device.name != name => {
                            device.name = name;
                            true
                        }
                        _ => false,
                    })
                })
                .await;
                if let Err(e) = saved {
                    error!("Failed to save the new name: {}", e);
                }
            },
            |_| Message::Bluetooth(BluetoothUIMessage::NoOp),
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_sent_when_new_and_not_empty() {
        assert!(name_needs_sending("Pods", Some("Old")));
        assert!(name_needs_sending("Pods", None));
    }

    #[test]
    fn name_is_skipped_when_unchanged_or_empty() {
        assert!(!name_needs_sending("Pods", Some("Pods")));
        assert!(!name_needs_sending("", Some("Old")));
        assert!(!name_needs_sending("  ", None));
    }
}
