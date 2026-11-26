//! The squeeze mute of the AirPods microphone. While an app still records, the
//! mute must not end and the recording must not move to another microphone: the
//! user believes they are muted. A marker file keeps it across restarts.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use log::{info, warn};

use crate::audio::output::{self, SOURCE_NAME};

/// Apps close and reopen their stream, and reconnect after a sound server
/// restart: only silence this long ends the mute.
const IDLE_BEFORE_UNMUTE: Duration = Duration::from_secs(10);

/// One change of the mute at a time: a press can meet the end of a recording.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Present while podbridge has muted the microphone and not unmuted it. It
/// lives as long as the virtual mic can: until logout.
fn marker() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("podbridge-mic-muted")
}

fn set_marked(marked: bool) {
    let result = if marked {
        std::fs::write(marker(), b"")
    } else {
        match std::fs::remove_file(marker()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    };
    if let Err(e) = result {
        warn!("[hires] could not record the microphone mute: {e}");
    }
}

/// Flip the microphone's mute, wherever it was last changed. Returns whether it
/// is muted now, or None if it could not be changed. Blocking.
pub fn toggle() -> Option<bool> {
    let _lock = lock();
    let muted = !output::source_muted(SOURCE_NAME)?;
    // Marked before muting: a crash in between must not leave an unknown mute.
    if muted {
        set_marked(true);
    }
    let done = output::set_source_mute(SOURCE_NAME, muted);
    // The mark stays only if the microphone is muted now.
    let muted_now = if done { muted } else { !muted };
    if !muted_now {
        set_marked(false);
    }
    done.then_some(muted)
}

fn holds(marked: bool, muted: Option<bool>, recorders: bool) -> bool {
    // Unknown counts as muted.
    marked && muted != Some(false) && recorders
}

/// An app records from a microphone we muted: leave both as they are. Blocking.
pub fn holds_a_recording() -> bool {
    let _lock = lock();
    holds(
        marker().exists(),
        output::source_muted(SOURCE_NAME),
        !output::recorders_on(SOURCE_NAME).is_empty(),
    )
}

/// How long nothing has recorded.
#[derive(Default)]
pub struct Idle(Option<Instant>);

impl Idle {
    fn long_enough(&mut self, in_use: bool, now: Instant) -> bool {
        if in_use {
            self.0 = None;
            return false;
        }
        let since = *self.0.get_or_insert(now);
        now.duration_since(since) >= IDLE_BEFORE_UNMUTE
    }
}

/// Unmute once nothing has recorded for a while. `recording` is the hi-res
/// monitor's view. Blocking.
pub fn end_when_idle(idle: &mut Idle, recording: bool) {
    let _lock = lock();
    if !marker().exists() {
        *idle = Idle::default();
        return;
    }
    let in_use = recording || !output::recorders_on(SOURCE_NAME).is_empty();
    if idle.long_enough(in_use, Instant::now()) && output::set_source_mute(SOURCE_NAME, false) {
        info!("[hires] recording over; unmuted '{SOURCE_NAME}'");
        set_marked(false);
    }
}

/// A virtual mic created anew starts unmuted: mute it again if we had. Blocking.
pub fn restore() {
    let _lock = lock();
    if marker().exists() && !output::set_source_mute(SOURCE_NAME, true) {
        warn!("[hires] could not mute the new '{SOURCE_NAME}'");
    }
}

/// The virtual mic was removed, and its mute with it.
pub fn forget() {
    let _lock = lock();
    set_marked(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_muted_recording_is_held() {
        assert!(holds(true, Some(true), true));
        // Cannot tell whether it is muted: assume it is.
        assert!(holds(true, None, true));
        // Unmuted from the desktop, not by us, or nobody records.
        assert!(!holds(true, Some(false), true));
        assert!(!holds(false, Some(true), true));
        assert!(!holds(true, Some(true), false));
    }

    #[test]
    fn only_a_long_enough_silence_ends_the_mute() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let mut idle = Idle::default();
        assert!(!idle.long_enough(false, at(0)));
        assert!(!idle.long_enough(false, at(9)));
        // An app records again: the count starts over.
        assert!(!idle.long_enough(true, at(9)));
        assert!(!idle.long_enough(false, at(12)));
        assert!(!idle.long_enough(false, at(21)));
        assert!(idle.long_enough(false, at(22)));
    }
}
