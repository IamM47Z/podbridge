//! Values of the AirPods settings podbridge shows as plain toggles, pickers
//! and sliders. Every value comes from the librepods Android client:
//! AACPManager.kt and its settings screens.

use crate::bluetooth::aacp::ControlCommandIdentifiers;
use std::fmt::Display;

/// The values a control command toggle uses for on and off.
pub const ON: u8 = 0x01;
pub const OFF: u8 = 0x02;

pub fn toggle_value(on: bool) -> u8 {
    if on { ON } else { OFF }
}

/// Whether a reported toggle value is on. The Android client reads the first
/// byte and counts only ON as on.
pub fn is_on(value: &[u8]) -> bool {
    value.first() == Some(&ON)
}

/// One entry of a settings picker: the byte the AirPods use and its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub value: u8,
    pub name: &'static str,
}

impl Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

const fn choice(value: u8, name: &'static str) -> Choice {
    Choice { value, name }
}

/// Press speed (0x17): how fast two or three presses must come.
pub const PRESS_SPEEDS: [Choice; 3] = [
    choice(0x00, "Default"),
    choice(0x01, "Slower"),
    choice(0x02, "Slowest"),
];

/// Press and hold duration (0x18). The names are the Android client's.
pub const PRESS_AND_HOLD_DURATIONS: [Choice; 3] = [
    choice(0x00, "Default"),
    choice(0x01, "Slower"),
    choice(0x02, "Slowest"),
];

/// Volume swipe interval (0x23): the wait between two volume swipes.
pub const VOLUME_SWIPE_INTERVALS: [Choice; 3] = [
    choice(0x01, "Default"),
    choice(0x02, "Longer"),
    choice(0x03, "Longest"),
];

/// Microphone (0x01): which AirPod's microphone is used.
pub const MIC_MODES: [Choice; 3] = [
    choice(0x00, "Automatic"),
    choice(0x02, "Always Left"),
    choice(0x01, "Always Right"),
];

/// The choice the AirPods reported; None for a value no client documents.
pub fn find_choice(choices: &[Choice], value: u8) -> Option<Choice> {
    choices.iter().find(|c| c.value == value).copied()
}

/// The highest chime volume the Android client's slider sends.
pub const MAX_CHIME_VOLUME: u8 = 100;

/// The bytes a control command carries for a setting's value. Chime volume
/// (0x1F) is sent as [volume, 0x50], as the Android client does; what 0x50
/// means is undocumented. Every other setting is one byte.
pub fn payload(identifier: ControlCommandIdentifiers, value: u8) -> Vec<u8> {
    match identifier {
        ControlCommandIdentifiers::ChimeVolume => vec![value.min(MAX_CHIME_VOLUME), 0x50],
        _ => vec![value],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_use_one_for_on_and_two_for_off() {
        assert_eq!(toggle_value(true), 0x01);
        assert_eq!(toggle_value(false), 0x02);
    }

    #[test]
    fn only_a_first_byte_of_one_is_on() {
        assert!(is_on(&[0x01]));
        assert!(is_on(&[0x01, 0x00]));
        assert!(!is_on(&[0x02]));
        assert!(!is_on(&[0x00]));
        assert!(!is_on(&[]));
    }

    #[test]
    fn microphone_values_follow_the_android_client() {
        assert_eq!(find_choice(&MIC_MODES, 0x00).unwrap().name, "Automatic");
        assert_eq!(find_choice(&MIC_MODES, 0x01).unwrap().name, "Always Right");
        assert_eq!(find_choice(&MIC_MODES, 0x02).unwrap().name, "Always Left");
        assert_eq!(find_choice(&MIC_MODES, 0x03), None);
    }

    #[test]
    fn reported_values_map_to_their_choice() {
        assert_eq!(find_choice(&PRESS_SPEEDS, 0x00).unwrap().name, "Default");
        assert_eq!(find_choice(&PRESS_SPEEDS, 0x02).unwrap().name, "Slowest");
        assert_eq!(
            find_choice(&PRESS_AND_HOLD_DURATIONS, 0x01).unwrap().name,
            "Slower"
        );
        assert_eq!(
            find_choice(&VOLUME_SWIPE_INTERVALS, 0x01).unwrap().name,
            "Default"
        );
        assert_eq!(
            find_choice(&VOLUME_SWIPE_INTERVALS, 0x03).unwrap().name,
            "Longest"
        );
    }

    #[test]
    fn undocumented_values_have_no_choice() {
        assert_eq!(find_choice(&PRESS_SPEEDS, 0x03), None);
        // The swipe interval starts at 1: 0 is not one of its values.
        assert_eq!(find_choice(&VOLUME_SWIPE_INTERVALS, 0x00), None);
    }

    #[test]
    fn chime_volume_carries_the_android_second_byte() {
        assert_eq!(
            payload(ControlCommandIdentifiers::ChimeVolume, 75),
            vec![75, 0x50]
        );
    }

    #[test]
    fn chime_volume_is_capped_at_its_maximum() {
        assert_eq!(
            payload(ControlCommandIdentifiers::ChimeVolume, 200),
            vec![MAX_CHIME_VOLUME, 0x50]
        );
    }

    #[test]
    fn other_settings_are_one_byte() {
        assert_eq!(
            payload(ControlCommandIdentifiers::OneBudAncMode, ON),
            vec![ON]
        );
        assert_eq!(
            payload(ControlCommandIdentifiers::AutoAncStrength, 37),
            vec![37]
        );
    }
}
