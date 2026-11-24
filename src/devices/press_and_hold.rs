//! The listening modes a stem press and hold cycles through (control command
//! 0x1A). The AirPods reset this when they join an Apple device that is not
//! iCloud-synced, so podbridge stores it and sends it again on each connect.

pub const OFF: u8 = 0x01;
pub const NOISE_CANCELLATION: u8 = 0x02;
pub const TRANSPARENCY: u8 = 0x04;
pub const ADAPTIVE: u8 = 0x08;

/// The modes in the order the UI lists them, with their names.
pub const MODES: [(u8, &str); 4] = [
    (OFF, "Off"),
    (NOISE_CANCELLATION, "Noise Cancellation"),
    (TRANSPARENCY, "Transparency"),
    (ADAPTIVE, "Adaptive"),
];

/// What the page shows before any report or stored choice: Noise
/// Cancellation and Transparency. Apple's support page 108918 does not name a
/// default cycle. These two are the modes every AirPods with noise control
/// offers.
pub const DEFAULT: u8 = NOISE_CANCELLATION | TRANSPARENCY;

/// The mask to show: the AirPods' report, else the choice stored for this
/// device, else DEFAULT. Some AirPods (Pro 2, A3064) never report 0x1A.
pub fn shown_mask(reported: Option<u8>, stored: Option<u8>) -> u8 {
    reported.or(stored).unwrap_or(DEFAULT)
}

/// Flip one mode. The cycle needs at least two modes, so turning one off
/// that would leave fewer is refused and the mask comes back unchanged.
/// Off counts only while it is allowed: hidden, it cannot be one of the two.
pub fn toggle(mask: u8, mode: u8, allow_off: bool) -> u8 {
    let toggled = mask ^ mode;
    let shown = if allow_off { 0x0F } else { 0x0F & !OFF };
    if (toggled & shown).count_ones() < 2 {
        mask
    } else {
        toggled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_beats_the_stored_choice() {
        assert_eq!(shown_mask(Some(0x0B), Some(0x06)), 0x0B);
    }

    #[test]
    fn unreported_modes_show_the_stored_choice_else_the_default() {
        assert_eq!(shown_mask(None, Some(0x0D)), 0x0D);
        assert_eq!(shown_mask(None, None), 0x06);
    }

    #[test]
    fn modes_turn_on_and_off() {
        let mask = NOISE_CANCELLATION | TRANSPARENCY;
        assert_eq!(toggle(mask, ADAPTIVE, true), mask | ADAPTIVE);
        assert_eq!(toggle(mask | ADAPTIVE, ADAPTIVE, true), mask);
    }

    #[test]
    fn fewer_than_two_modes_is_refused() {
        let mask = NOISE_CANCELLATION | TRANSPARENCY;
        assert_eq!(toggle(mask, TRANSPARENCY, true), mask);
    }

    #[test]
    fn hidden_off_does_not_count_toward_two() {
        // Off + ANC + Adaptive with Off hidden: only ANC and Adaptive show,
        // so unchecking ANC would leave one visible mode.
        let mask = OFF | NOISE_CANCELLATION | ADAPTIVE;
        assert_eq!(toggle(mask, NOISE_CANCELLATION, false), mask);
        assert_eq!(toggle(mask, NOISE_CANCELLATION, true), OFF | ADAPTIVE);
    }

    #[test]
    fn values_match_the_protocol_notes() {
        // "Turns on Adaptive from O and ANC": 0x03 becomes 0x0B.
        assert_eq!(toggle(OFF | NOISE_CANCELLATION, ADAPTIVE, true), 0x0B);
        // "Turns off Transparency from O and Adaptive": 0x0D becomes 0x09.
        assert_eq!(toggle(0x0D, TRANSPARENCY, true), 0x09);
    }
}
