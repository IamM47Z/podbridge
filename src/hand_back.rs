//! Automatic hand-back: once nothing here has used the AirPods for a while,
//! give up this computer's claim on them so another device (an iPhone) can
//! have them without a tug of war. A device they were taken from gets them
//! back. Otherwise, as after a manual connect, the claim is only dropped, the
//! way an idle Mac behaves: the next device to play takes them.
//!
//! Apple documents only one rule for automatic switching: sound should not
//! switch during a conversation (a call or video conference). An active mic
//! or any unpaused stream here blocks the hand-back. So does media paused by
//! taking one bud out, which putting it back resumes. Apple does not document
//! the timing, nor whether the iPhone resumes on its own; the delay below is a
//! guess the live tests set.

use std::time::{Duration, Instant};

// shortcut: a guessed idle time; the live tests with an iPhone set the real one.
pub const HAND_BACK_AFTER: Duration = Duration::from_secs(30);

/// What to give up once this computer has been idle long enough.
#[derive(Debug, PartialEq, Eq)]
pub enum Release {
    /// Hand the AirPods back to the device they were taken from.
    HandBackTo(String),
    /// Nobody waits for them: only drop this computer's claim.
    Anyone,
}

#[derive(Debug, Default)]
pub struct HandBack {
    /// This computer claimed the AirPods and has not given them up since.
    holds: bool,
    /// The device the AirPods were taken from, while this computer holds them.
    from: Option<String>,
    idle_since: Option<Instant>,
}

impl HandBack {
    /// This computer took the AirPods from `from`.
    pub fn took_over_from(&mut self, from: String) {
        self.from = Some(from);
        self.claimed();
    }

    /// This computer claimed the AirPods. A device still waiting for them
    /// keeps waiting.
    pub fn claimed(&mut self) {
        self.holds = true;
        self.idle_since = None;
    }

    /// Whether this computer holds a claim that an idle period would release.
    pub fn holds(&self) -> bool {
        self.holds
    }

    /// The device waiting to get the AirPods back, if any.
    pub fn pending(&self) -> Option<&str> {
        self.from.as_deref()
    }

    /// Another device has the AirPods now: there is nothing to hand back.
    pub fn released(&mut self) {
        *self = Self::default();
    }

    /// Feed one observation. Returns what to give up when it is time, once
    /// per claim.
    pub fn tick(
        &mut self,
        now: Instant,
        in_use_here: bool,
        from_still_connected: bool,
    ) -> Option<Release> {
        if !self.holds {
            return None;
        }
        if !from_still_connected {
            // Gone: nobody to hand back to, but the claim still blocks others.
            self.from = None;
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
        self.released();
        Some(from.map_or(Release::Anyone, Release::HandBackTo))
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
        assert_eq!(
            h.tick(t + HAND_BACK_AFTER, false, true),
            Some(Release::HandBackTo(PHONE.into()))
        );
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
            Some(Release::HandBackTo(PHONE.into()))
        );
    }

    #[test]
    fn nothing_to_release_without_a_claim() {
        let t = Instant::now();
        let mut h = HandBack::default();
        assert_eq!(h.tick(t, false, false), None);
        assert_eq!(h.tick(t + HAND_BACK_AFTER * 2, false, false), None);
    }

    #[test]
    fn a_claim_from_nobody_is_released_once_when_idle() {
        let t = Instant::now();
        let mut h = HandBack::default();
        h.claimed();
        assert!(h.holds());
        assert_eq!(h.tick(t, false, false), None);
        assert_eq!(
            h.tick(t + HAND_BACK_AFTER, false, false),
            Some(Release::Anyone)
        );
        assert!(!h.holds());
        assert_eq!(h.tick(t + HAND_BACK_AFTER * 3, false, false), None);
    }

    #[test]
    fn claiming_again_after_a_release_rearms_it() {
        let t = Instant::now();
        let mut h = HandBack::default();
        h.claimed();
        h.tick(t, false, false);
        assert_eq!(
            h.tick(t + HAND_BACK_AFTER, false, false),
            Some(Release::Anyone)
        );
        let later = t + HAND_BACK_AFTER * 2;
        h.claimed();
        assert_eq!(h.tick(later, false, false), None);
        assert_eq!(
            h.tick(later + HAND_BACK_AFTER, false, false),
            Some(Release::Anyone)
        );
    }

    #[test]
    fn a_new_claim_keeps_the_device_waiting() {
        let t = Instant::now();
        let mut h = HandBack::default();
        h.took_over_from(PHONE.into());
        h.claimed();
        assert_eq!(h.pending(), Some(PHONE));
        h.tick(t, false, true);
        assert_eq!(
            h.tick(t + HAND_BACK_AFTER, false, true),
            Some(Release::HandBackTo(PHONE.into()))
        );
    }

    #[test]
    fn a_device_that_left_is_forgotten_but_the_claim_is_released() {
        let t = Instant::now();
        let mut h = HandBack::default();
        h.took_over_from(PHONE.into());
        assert_eq!(h.tick(t, false, false), None);
        assert_eq!(h.pending(), None);
        assert_eq!(
            h.tick(t + HAND_BACK_AFTER, false, false),
            Some(Release::Anyone)
        );
    }

    #[test]
    fn another_device_taking_them_drops_the_claim() {
        let t = Instant::now();
        let mut h = HandBack::default();
        h.took_over_from(PHONE.into());
        h.released();
        assert_eq!(h.tick(t + HAND_BACK_AFTER * 3, false, true), None);

        h.claimed();
        h.tick(t, false, false);
        h.released();
        assert_eq!(h.tick(t + HAND_BACK_AFTER * 3, false, false), None);
    }
}
