use crate::bluetooth::aacp::{BatteryInfo, ControlCommandIdentifiers};
use crate::devices::airpods::AirPodsInformation;
use crate::devices::nothing::NothingInformation;
use iced::widget::combo_box;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeviceType {
    AirPods,
    Nothing,
}

impl Display for DeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceType::AirPods => write!(f, "AirPods"),
            DeviceType::Nothing => write!(f, "Nothing"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum DeviceInformation {
    AirPods(Box<AirPodsInformation>),
    Nothing(NothingInformation),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceData {
    pub name: String,
    pub type_: DeviceType,
    pub information: Option<DeviceInformation>,
}

#[derive(Clone, Debug)]
pub enum DeviceState {
    AirPods(AirPodsState),
    Nothing(NothingState),
}

impl Display for DeviceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceState::AirPods(_) => write!(f, "AirPods State"),
            DeviceState::Nothing(_) => write!(f, "Nothing State"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AirPodsState {
    pub device_name: String,
    pub noise_control_mode: AirPodsNoiseControlMode,
    pub noise_control_state: combo_box::State<AirPodsNoiseControlMode>,
    pub conversation_awareness_enabled: bool,
    pub personalized_volume_enabled: bool,
    pub allow_off_mode: bool,
    /// Adaptive mode's noise filtering, 0 to 100 (100 filters the most), as
    /// the AirPods report it; None on models without it.
    pub adaptive_strength: Option<u8>,
    /// The press-and-hold listening modes bitmask (control command 0x1A), as
    /// the AirPods report it; None until they do.
    pub press_and_hold_modes: Option<u8>,
    /// Accessibility settings, as the AirPods report them; each None until
    /// they do. Toggles hold controls::ON or OFF, pickers a controls::Choice
    /// value, chime volume 0 to 100.
    pub one_bud_anc: Option<u8>,
    pub chime_volume: Option<u8>,
    pub volume_swipe: Option<u8>,
    pub volume_swipe_interval: Option<u8>,
    pub press_speed: Option<u8>,
    pub press_and_hold_duration: Option<u8>,
    /// Local capture state of the proprietary hi-res microphone (UI-driven).
    pub hires_mic_enabled: bool,
    pub battery: Vec<BatteryInfo>,
}

impl AirPodsState {
    /// Record a setting the AirPods reported or the user chose. Returns false
    /// for a setting this state does not keep this way.
    pub fn record_setting(&mut self, identifier: ControlCommandIdentifiers, value: &[u8]) -> bool {
        let slot = match identifier {
            ControlCommandIdentifiers::AutoAncStrength => &mut self.adaptive_strength,
            ControlCommandIdentifiers::OneBudAncMode => &mut self.one_bud_anc,
            ControlCommandIdentifiers::ChimeVolume => &mut self.chime_volume,
            ControlCommandIdentifiers::VolumeSwipeMode => &mut self.volume_swipe,
            ControlCommandIdentifiers::VolumeSwipeInterval => &mut self.volume_swipe_interval,
            ControlCommandIdentifiers::DoubleClickInterval => &mut self.press_speed,
            ControlCommandIdentifiers::ClickHoldInterval => &mut self.press_and_hold_duration,
            _ => return false,
        };
        *slot = value.first().copied();
        true
    }
}

#[derive(Clone, Debug)]
pub enum AirPodsNoiseControlMode {
    Off,
    NoiseCancellation,
    Transparency,
    Adaptive,
}

impl Display for AirPodsNoiseControlMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AirPodsNoiseControlMode::Off => write!(f, "Off"),
            AirPodsNoiseControlMode::NoiseCancellation => write!(f, "Noise Cancellation"),
            AirPodsNoiseControlMode::Transparency => write!(f, "Transparency"),
            AirPodsNoiseControlMode::Adaptive => write!(f, "Adaptive"),
        }
    }
}

impl AirPodsNoiseControlMode {
    pub fn from_byte(value: &u8) -> Self {
        match value {
            0x01 => AirPodsNoiseControlMode::Off,
            0x02 => AirPodsNoiseControlMode::NoiseCancellation,
            0x03 => AirPodsNoiseControlMode::Transparency,
            0x04 => AirPodsNoiseControlMode::Adaptive,
            _ => AirPodsNoiseControlMode::Off,
        }
    }
    pub fn to_byte(&self) -> u8 {
        match self {
            AirPodsNoiseControlMode::Off => 0x01,
            AirPodsNoiseControlMode::NoiseCancellation => 0x02,
            AirPodsNoiseControlMode::Transparency => 0x03,
            AirPodsNoiseControlMode::Adaptive => 0x04,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NothingState {
    pub anc_mode: NothingAncMode,
    pub anc_mode_state: combo_box::State<NothingAncMode>,
}

#[derive(Clone, Debug)]
pub enum NothingAncMode {
    Off,
    LowNoiseCancellation,
    MidNoiseCancellation,
    HighNoiseCancellation,
    AdaptiveNoiseCancellation,
    Transparency,
}

impl Display for NothingAncMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NothingAncMode::Off => write!(f, "Off"),
            NothingAncMode::LowNoiseCancellation => write!(f, "Low Noise Cancellation"),
            NothingAncMode::MidNoiseCancellation => write!(f, "Mid Noise Cancellation"),
            NothingAncMode::HighNoiseCancellation => write!(f, "High Noise Cancellation"),
            NothingAncMode::AdaptiveNoiseCancellation => write!(f, "Adaptive Noise Cancellation"),
            NothingAncMode::Transparency => write!(f, "Transparency"),
        }
    }
}
impl NothingAncMode {
    pub fn to_byte(&self) -> u8 {
        match self {
            NothingAncMode::LowNoiseCancellation => 0x03,
            NothingAncMode::MidNoiseCancellation => 0x02,
            NothingAncMode::HighNoiseCancellation => 0x01,
            NothingAncMode::AdaptiveNoiseCancellation => 0x04,
            NothingAncMode::Transparency => 0x07,
            NothingAncMode::Off => 0x05,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn airpods_state() -> AirPodsState {
        AirPodsState {
            device_name: String::new(),
            noise_control_mode: AirPodsNoiseControlMode::Off,
            noise_control_state: combo_box::State::new(Vec::new()),
            conversation_awareness_enabled: false,
            personalized_volume_enabled: false,
            allow_off_mode: false,
            adaptive_strength: None,
            press_and_hold_modes: None,
            one_bud_anc: None,
            chime_volume: None,
            volume_swipe: None,
            volume_swipe_interval: None,
            press_speed: None,
            press_and_hold_duration: None,
            hires_mic_enabled: false,
            battery: Vec::new(),
        }
    }

    #[test]
    fn reported_settings_land_in_their_field() {
        let mut state = airpods_state();
        for (id, value) in [
            (ControlCommandIdentifiers::OneBudAncMode, 0x01),
            (ControlCommandIdentifiers::ChimeVolume, 75),
            (ControlCommandIdentifiers::VolumeSwipeMode, 0x02),
            (ControlCommandIdentifiers::VolumeSwipeInterval, 0x03),
            (ControlCommandIdentifiers::DoubleClickInterval, 0x01),
            (ControlCommandIdentifiers::ClickHoldInterval, 0x02),
            (ControlCommandIdentifiers::AutoAncStrength, 40),
        ] {
            assert!(state.record_setting(id, &[value, 0x50]));
        }
        assert_eq!(state.one_bud_anc, Some(0x01));
        assert_eq!(state.chime_volume, Some(75));
        assert_eq!(state.volume_swipe, Some(0x02));
        assert_eq!(state.volume_swipe_interval, Some(0x03));
        assert_eq!(state.press_speed, Some(0x01));
        assert_eq!(state.press_and_hold_duration, Some(0x02));
        assert_eq!(state.adaptive_strength, Some(40));
    }

    #[test]
    fn other_settings_are_not_recorded() {
        let mut state = airpods_state();
        assert!(!state.record_setting(ControlCommandIdentifiers::ListeningMode, &[0x02]));
    }
}
