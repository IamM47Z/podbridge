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

/// Flip one mode. The cycle needs at least two modes, so turning one off
/// that would leave fewer is refused and the mask comes back unchanged.
pub fn toggle(mask: u8, mode: u8) -> u8 {
    let toggled = mask ^ mode;
    if (toggled & 0x0F).count_ones() < 2 {
        mask
    } else {
        toggled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_turn_on_and_off() {
        let mask = NOISE_CANCELLATION | TRANSPARENCY;
        assert_eq!(toggle(mask, ADAPTIVE), mask | ADAPTIVE);
        assert_eq!(toggle(mask | ADAPTIVE, ADAPTIVE), mask);
    }

    #[test]
    fn fewer_than_two_modes_is_refused() {
        let mask = NOISE_CANCELLATION | TRANSPARENCY;
        assert_eq!(toggle(mask, TRANSPARENCY), mask);
    }

    #[test]
    fn values_match_the_protocol_notes() {
        // "Turns on Adaptive from O and ANC": 0x03 becomes 0x0B.
        assert_eq!(toggle(OFF | NOISE_CANCELLATION, ADAPTIVE), 0x0B);
        // "Turns off Transparency from O and Adaptive": 0x0D becomes 0x09.
        assert_eq!(toggle(0x0D, TRANSPARENCY), 0x09);
    }
}
