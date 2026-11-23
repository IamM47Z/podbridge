//! Automatic hand-back: after this computer took the AirPods from another
//! device (an iPhone), give them up once nothing here has used them for a
//! while, so the other device can have them back without a tug of war.
//!
//! Apple documents only one rule for automatic switching: sound should not
//! switch during a conversation (a call or video conference). An active mic
//! here counts as one. Apple does not document the timing, nor whether the
//! iPhone resumes on its own; the delay below is a guess the live tests set.

use std::time::{Duration, Instant};

// shortcut: a guessed idle time; the live tests with an iPhone set the real one.
pub const HAND_BACK_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct HandBack {
    /// The device the AirPods were taken from, while this computer holds them.
    from: Option<String>,
    idle_since: Option<Instant>,
}

impl HandBack {
    /// This computer took the AirPods from `from`.
    pub fn took_over_from(&mut self, from: String) {
        self.from = Some(from);
        self.idle_since = None;
    }

    /// The device waiting to get the AirPods back, if any.
    pub fn pending(&self) -> Option<&str> {
        self.from.as_deref()
    }

    /// Another device has the AirPods now: there is nothing to hand back.
    pub fn released(&mut self) {
        *self = Self::default();
    }

    /// Feed one observation. Returns the device to hand the AirPods back to
    /// when it is time, once.
    pub fn tick(
        &mut self,
        now: Instant,
        in_use_here: bool,
        from_still_connected: bool,
    ) -> Option<String> {
        self.from.as_ref()?;
        if !from_still_connected {
            self.released();
            return None;
        }
        if in_use_here {
            self.idle_since = None;
            return None;
        }
        let since = *self.idle_since.get_or_insert(now);
        if now.duration_since(since) < HAND_BACK_AFTER {
            return None;
        }
        let from = self.from.take();
        self.idle_since = None;
        from
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: &str = "11:22:33:44:55:66";

    #[test]
    fn hands_back_once_after_the_idle_time() {
        let t = Instant::now();
        let mut h = HandBack::default();
        h.took_over_from(PHONE.into());
        assert_eq!(h.tick(t, false, true), None);
        assert_eq!(h.tick(t + HAND_BACK_AFTER, false, true), Some(PHONE.into()));
        assert_eq!(h.tick(t + HAND_BACK_AFTER * 2, false, true), None);
    }

    #[test]
    fn use_here_restarts_the_wait() {
        let t = Instant::now();
        let mut h = HandBack::default();
        h.took_over_from(PHONE.into());
        h.tick(t, false, true);
        // A call or playback here (a conversation never switches, per Apple).
        assert_eq!(h.tick(t + HAND_BACK_AFTER, true, true), None);
        let later = t + HAND_BACK_AFTER + Duration::from_secs(1);
        assert_eq!(h.tick(later, false, true), None);
        assert_eq!(
            h.tick(later + HAND_BACK_AFTER, false, true),
            Some(PHONE.into())
        );
    }

    #[test]
    fn nothing_to_hand_back_without_a_takeover() {
        let t = Instant::now();
        let mut h = HandBack::default();
        assert_eq!(h.tick(t + HAND_BACK_AFTER * 2, false, true), None);
    }

    #[test]
    fn a_device_that_left_or_took_them_back_is_forgotten() {
        let t = Instant::now();
        let mut h = HandBack::default();
        h.took_over_from(PHONE.into());
        assert_eq!(h.tick(t, false, false), None);
        assert_eq!(h.tick(t + HAND_BACK_AFTER, false, true), None);

        h.took_over_from(PHONE.into());
        h.released();
        assert_eq!(h.tick(t + HAND_BACK_AFTER * 3, false, true), None);
    }
}
