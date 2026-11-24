//! Handlers for messages that come from the Bluetooth side.

use super::{App, Message, Tab, main_window_settings, read_devices_list};
use crate::bluetooth::aacp::{AACPEvent, ControlCommandIdentifiers, ControlCommandStatus};
use crate::devices::enums::{
    AirPodsNoiseControlMode, AirPodsState, DeviceState, DeviceType, NothingAncMode, NothingState,
};
use crate::devices::press_and_hold;
use crate::ui::messages::BluetoothUIMessage;
use iced::widget::combo_box;
use iced::{Task, window};
use log::{debug, error};
use std::time::{Duration, Instant};

impl App {
    pub(super) fn handle_bluetooth(&mut self, message: BluetoothUIMessage) -> Task<Message> {
        match message {
            BluetoothUIMessage::NoOp => self.wait_task(),
            BluetoothUIMessage::OpenWindow => self.open_window(),
            BluetoothUIMessage::DeviceConnected(mac) => {
                let wait_task = self.wait_task();
                self.device_connected(mac);
                Task::batch(vec![wait_task])
            }
            BluetoothUIMessage::DeviceDisconnected(mac) => {
                let wait_task = self.wait_task();
                self.device_disconnected(mac);
                Task::batch(vec![wait_task])
            }
            BluetoothUIMessage::AACPUIEvent(mac, event) => {
                let wait_task = self.wait_task();
                debug!("AACP UI Event for {}: {:?}", mac, event);
                self.aacp_event(&mac, event);
                Task::batch(vec![wait_task])
            }
            BluetoothUIMessage::ATTNotification(mac, handle, value) => {
                debug!(
                    "ATT Notification for {}: handle=0x{:04X}, value={:?}",
                    mac, handle, value
                );

                // TODO: Handle Nothing's ANC Mode changes here

                let wait_task = self.wait_task();
                Task::batch(vec![wait_task])
            }
        }
    }

    fn open_window(&mut self) -> Task<Message> {
        let wait_task = self.wait_task();
        debug!("Opening main window...");
        let just_opened = self
            .window_opened_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1));
        match self.window {
            Some(window_id) if just_opened => {
                Task::batch(vec![window::gain_focus(window_id), wait_task])
            }
            // On Wayland an app can't un-minimize or raise its own window
            // (gain_focus is ignored without an activation token), so replace
            // an existing window with a new one, which the compositor shows.
            old_window => {
                let (new_window, open_task) = window::open(main_window_settings());
                self.window = Some(new_window);
                self.window_opened_at = Some(Instant::now());
                let close_task = old_window.map_or(Task::none(), window::close);
                Task::batch(vec![
                    close_task,
                    open_task.map(Message::WindowOpened),
                    wait_task,
                ])
            }
        }
    }

    fn device_connected(&mut self, mac: String) {
        debug!(
            "Device connected: {}. Adding to connected devices list",
            mac
        );
        if !self.bluetooth_state.connected_devices.contains(&mac) {
            self.bluetooth_state.connected_devices.push(mac.clone());
        }

        // self.device_states.insert(mac.clone(), DeviceState::AirPods(AirPodsState {
        //     conversation_awareness_enabled: false,
        // }));

        // on the first connection the device is not in the devices file yet,
        // so derive the type from the registered managers first
        let type_ = self
            .registered_device_type(&mac)
            .or_else(|| device_type_from_file(&mac));
        match type_ {
            Some(DeviceType::AirPods) => {
                let state = self.connected_airpods_state(&mac);
                self.device_states
                    .insert(mac.clone(), DeviceState::AirPods(state));
            }
            Some(DeviceType::Nothing) => {
                self.device_states.insert(
                    mac.clone(),
                    DeviceState::Nothing(NothingState {
                        anc_mode: NothingAncMode::Off,
                        anc_mode_state: combo_box::State::new(vec![
                            NothingAncMode::Off,
                            NothingAncMode::Transparency,
                            NothingAncMode::AdaptiveNoiseCancellation,
                            NothingAncMode::LowNoiseCancellation,
                            NothingAncMode::MidNoiseCancellation,
                            NothingAncMode::HighNoiseCancellation,
                        ]),
                    }),
                );
            }
            _ => {}
        }
    }

    fn registered_device_type(&self, mac: &str) -> Option<DeviceType> {
        let device_managers = self.device_managers.blocking_read();
        device_managers.get(mac).and_then(|managers| {
            if managers.get_aacp().is_some() {
                Some(DeviceType::AirPods)
            } else if managers.get_att().is_some() {
                Some(DeviceType::Nothing)
            } else {
                None
            }
        })
    }

    /// Initial state of a freshly connected AirPods device, read from its AACP manager.
    fn connected_airpods_state(&self, mac: &str) -> AirPodsState {
        let device_managers = self.device_managers.blocking_read();
        let device_manager = device_managers.get(mac).unwrap();
        let aacp_manager = device_manager.get_aacp().unwrap();
        let aacp_manager_state = aacp_manager.state.clone();
        let state = aacp_manager_state.blocking_lock();
        debug!("AACP manager found for AirPods device {}", mac);
        let device_name = read_devices_list()
            .get(mac)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "Unknown Device".to_string());
        let statuses = &state.control_command_status_list;
        let allow_off_mode = control_enabled(statuses, ControlCommandIdentifiers::AllowOffOption);
        let mut airpods = AirPodsState {
            device_name,
            battery: state.battery_info.clone(),
            noise_control_mode: statuses
                .iter()
                .find_map(|status| {
                    if status.identifier == ControlCommandIdentifiers::ListeningMode {
                        status.value.first().map(AirPodsNoiseControlMode::from_byte)
                    } else {
                        None
                    }
                })
                .unwrap_or(AirPodsNoiseControlMode::Transparency),
            noise_control_state: combo_box::State::new(noise_control_modes(allow_off_mode)),
            conversation_awareness_enabled: control_enabled(
                statuses,
                ControlCommandIdentifiers::ConversationDetectConfig,
            ),
            personalized_volume_enabled: control_enabled(
                statuses,
                ControlCommandIdentifiers::AdaptiveVolumeConfig,
            ),
            allow_off_mode,
            adaptive_strength: None,
            press_and_hold_modes: statuses
                .iter()
                .find(|status| status.identifier == ControlCommandIdentifiers::ListeningModeConfigs)
                .and_then(|status| status.value.first().copied()),
            one_bud_anc: None,
            chime_volume: None,
            volume_swipe: None,
            volume_swipe_interval: None,
            press_speed: None,
            press_and_hold_duration: None,
            ear_detection: None,
            mic_mode: None,
            sleep_detection: None,
            hires_mic_enabled: self.hires_mic_enabled,
        };
        for status in statuses {
            airpods.record_setting(status.identifier, &status.value);
        }
        let settings = crate::utils::AppSettings::load();
        let stored = settings
            .stored_controls
            .get(mac)
            .cloned()
            .unwrap_or_default();
        airpods.fill_unreported(statuses, &stored);
        airpods.press_and_hold_modes = Some(press_and_hold::shown_mask(
            airpods.press_and_hold_modes,
            settings.press_and_hold_modes.get(mac).copied(),
        ));
        airpods
    }

    fn device_disconnected(&mut self, mac: String) {
        debug!("Device disconnected: {}", mac);

        self.bluetooth_state
            .connected_devices
            .retain(|device| device != &mac);

        self.device_states.remove(&mac);

        if matches!(&self.selected_tab, Tab::Device(selected_mac) if selected_mac == &mac) {
            self.selected_tab = Tab::Device("none".to_string());
        }
    }

    fn airpods_state_mut(&mut self, mac: &str) -> Option<&mut AirPodsState> {
        match self.device_states.get_mut(mac) {
            Some(DeviceState::AirPods(state)) => Some(state),
            _ => None,
        }
    }

    fn aacp_event(&mut self, mac: &str, event: AACPEvent) {
        match event {
            AACPEvent::ControlCommand(status) => match status.identifier {
                ControlCommandIdentifiers::ListeningMode => {
                    let mode = status
                        .value
                        .first()
                        .map(AirPodsNoiseControlMode::from_byte)
                        .unwrap_or(AirPodsNoiseControlMode::Transparency);
                    if let Some(state) = self.airpods_state_mut(mac) {
                        state.noise_control_mode = mode;
                    }
                }
                ControlCommandIdentifiers::ConversationDetectConfig => {
                    let is_enabled = parse_toggle(&status.value, "Conversation Detect Config");
                    if let Some(state) = self.airpods_state_mut(mac) {
                        state.conversation_awareness_enabled = is_enabled;
                    }
                }
                ControlCommandIdentifiers::AdaptiveVolumeConfig => {
                    let is_enabled = parse_toggle(&status.value, "Adaptive Volume Config");
                    if let Some(state) = self.airpods_state_mut(mac) {
                        state.personalized_volume_enabled = is_enabled;
                    }
                }
                ControlCommandIdentifiers::ListeningModeConfigs => {
                    if let Some(state) = self.airpods_state_mut(mac) {
                        state.press_and_hold_modes = status.value.first().copied();
                    }
                }
                ControlCommandIdentifiers::AllowOffOption => {
                    let is_enabled = parse_toggle(&status.value, "Allow Off Option");
                    if let Some(state) = self.airpods_state_mut(mac) {
                        state.allow_off_mode = is_enabled;
                        state.noise_control_state =
                            combo_box::State::new(noise_control_modes(is_enabled));
                    }
                }
                identifier => {
                    let recorded = self
                        .airpods_state_mut(mac)
                        .is_some_and(|state| state.record_setting(identifier, &status.value));
                    if !recorded {
                        debug!("Unhandled Control Command Status: {:?}", status);
                    }
                }
            },
            AACPEvent::BatteryInfo(battery_info) => {
                if let Some(state) = self.airpods_state_mut(mac) {
                    state.battery = battery_info;
                    debug!("Updated battery info for {}: {:?}", mac, state.battery);
                }
            }
            _ => {}
        }
    }
}

/// The listening modes a device offers; "Off" only when the device allows it.
pub(super) fn noise_control_modes(allow_off: bool) -> Vec<AirPodsNoiseControlMode> {
    let mut modes = vec![
        AirPodsNoiseControlMode::Transparency,
        AirPodsNoiseControlMode::NoiseCancellation,
        AirPodsNoiseControlMode::Adaptive,
    ];
    if allow_off {
        modes.insert(0, AirPodsNoiseControlMode::Off);
    }
    modes
}

pub(super) fn device_type_from_file(mac: &str) -> Option<DeviceType> {
    read_devices_list().get(mac).map(|d| d.type_.clone())
}

fn control_enabled(statuses: &[ControlCommandStatus], id: ControlCommandIdentifiers) -> bool {
    statuses
        .iter()
        .any(|status| status.identifier == id && matches!(status.value.as_slice(), [0x01]))
}

/// Decodes a 0x01 (on) / 0x02 (off) control value; anything else logs and counts as off.
fn parse_toggle(value: &[u8], name: &str) -> bool {
    match value {
        [0x01] => true,
        [0x02] => false,
        _ => {
            error!("Unknown {} value: {:?}", name, value);
            false
        }
    }
}
