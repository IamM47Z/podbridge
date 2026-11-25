//! Bluetooth card profile control and stream routing, shared by the media
//! controller (ear detection, ownership) and the hi-res mic (A2DP reset).
//!
//! Profile changes come from several tasks (ear detection, the playback
//! listener, ownership changes, the hi-res mic). Every change goes through
//! PROFILE_LOCK so they can't interleave and leave the card in the wrong state.

use libpulse_binding::callbacks::ListResult;
use log::{debug, info, warn};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::audio::output::{connect, wait_op};

/// Process-wide: it serialises changes to the sound server's card profiles,
/// one system-wide resource that every task and connection shares.
static PROFILE_LOCK: Mutex<()> = Mutex::new(());

/// How long no bud may be worn before the AirPods' audio switches to another
/// output and the hi-res mic is hidden. Shared by the media controller and the
/// hi-res monitor so both switch at the same moment. Short removals (adjusting
/// a bud) don't switch anything; media still pauses right away.
pub const NOT_WORN_GRACE: Duration = Duration::from_secs(4);

/// When an A2DP profile was last switched on from off. Switching on creates a
/// fresh transport, which makes a pending A2DP reset unnecessary. Process-wide:
/// it describes the one card, written by the media controller and read by the
/// hi-res mic's reset task, which share no owner.
static LAST_ACTIVATION: Mutex<Option<Instant>> = Mutex::new(None);

pub fn note_activated() {
    *LAST_ACTIVATION.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

/// True if the card was switched on from off at or after `t`.
pub fn activated_since(t: Instant) -> bool {
    LAST_ACTIVATION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|a| a >= t)
}

fn lock() -> MutexGuard<'static, ()> {
    PROFILE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Copy, Debug)]
pub enum CardRef<'a> {
    Index(u32),
    Name(&'a str),
}

/// Active profile name of a card ("off", "a2dp-sink", ...), or None if the card
/// doesn't exist (yet).
pub fn active_profile(card: CardRef) -> Option<String> {
    let (mut mainloop, context) = connect()?;
    let introspect = context.introspect();
    let profile: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let cb = {
        let profile = profile.clone();
        move |result: ListResult<&libpulse_binding::context::introspect::CardInfo>| {
            if let ListResult::Item(item) = result {
                *profile.borrow_mut() = item
                    .active_profile
                    .as_ref()
                    .and_then(|p| p.name.as_ref())
                    .map(|n| n.to_string());
            }
        }
    };
    let op = match card {
        CardRef::Index(i) => introspect.get_card_info_by_index(i, cb),
        CardRef::Name(n) => introspect.get_card_info_by_name(n, cb),
    };
    wait_op(&mut mainloop, &op);
    drop(op);
    profile.borrow().clone()
}

fn set_profile_unlocked(card: CardRef, profile: &str) -> bool {
    let Some((mut mainloop, context)) = connect() else {
        return false;
    };
    let mut introspect = context.introspect();
    let ok = Rc::new(Cell::new(false));
    let cb: Box<dyn FnMut(bool) + 'static> = {
        let ok = ok.clone();
        Box::new(move |success| ok.set(success))
    };
    let op = match card {
        CardRef::Index(i) => introspect.set_card_profile_by_index(i, profile, Some(cb)),
        CardRef::Name(n) => introspect.set_card_profile_by_name(n, profile, Some(cb)),
    };
    wait_op(&mut mainloop, &op);
    let success = ok.get();
    if !success {
        warn!("[pw] setting profile {} on {:?} failed", profile, card);
    }
    success
}

/// The card's highest-priority available headset profile (one carrying a
/// microphone), e.g. "headset-head-unit" (LC3/mSBC) ahead of "-cvsd".
pub fn best_headset_profile(card: &str) -> Option<String> {
    let (mut mainloop, context) = connect()?;
    let best: Rc<RefCell<Option<(u32, String)>>> = Rc::new(RefCell::new(None));
    let op = context.introspect().get_card_info_by_name(card, {
        let best = best.clone();
        move |result| {
            if let ListResult::Item(item) = result {
                for p in item.profiles.iter() {
                    let Some(name) = p.name.as_ref() else {
                        continue;
                    };
                    if !p.available || p.n_sources == 0 || !name.starts_with("headset-head-unit") {
                        continue;
                    }
                    let mut b = best.borrow_mut();
                    if b.as_ref().is_none_or(|(prio, _)| p.priority > *prio) {
                        *b = Some((p.priority, name.to_string()));
                    }
                }
            }
        }
    });
    wait_op(&mut mainloop, &op);
    drop(op);
    best.borrow().clone().map(|(_, n)| n)
}

/// Set a card profile, serialized with every other profile change. Returns the
/// real result reported by the server.
pub fn set_profile(card: CardRef, profile: &str) -> bool {
    let _guard = lock();
    set_profile_unlocked(card, profile)
}

/// Wait up to `wait` for a card to be on an A2DP profile (it may still be being
/// activated, e.g. a bud just went back in). Doesn't hold the profile lock, so
/// the activation can finish meanwhile.
pub fn wait_for_a2dp(card: &str, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        let current = active_profile(CardRef::Name(card));
        if current.as_deref().is_some_and(|p| p.starts_with("a2dp")) {
            return true;
        }
        if Instant::now() >= deadline {
            info!(
                "[pw] {} not on an A2DP profile ({:?}); skipping A2DP reset",
                card, current
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Cycle an A2DP card off and back on. Fallback A2DP reset for when
/// suspend_resume() fails: it works, but the sink disappears for a moment, so
/// the default output changes and back and apps re-link. Call wait_for_a2dp
/// first. Returns true if a cycle happened.
pub fn cycle_a2dp(card: &str) -> bool {
    let _guard = lock();
    // Re-read under the lock: a deactivation may have won the race meanwhile.
    let Some(profile) = active_profile(CardRef::Name(card)).filter(|p| p.starts_with("a2dp"))
    else {
        debug!("[pw] {} left A2DP before the reset; skipping", card);
        return false;
    };
    info!("[pw] reset A2DP transport: {} off -> {}", card, profile);
    let off = set_profile_unlocked(CardRef::Name(card), "off");
    let on = set_profile_unlocked(CardRef::Name(card), &profile);
    off && on
}

fn card_index_by_name(card: &str) -> Option<u32> {
    let (mut mainloop, context) = connect()?;
    let introspect = context.introspect();
    let index: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let op = introspect.get_card_info_by_name(card, {
        let index = index.clone();
        move |result| {
            if let ListResult::Item(item) = result {
                index.set(Some(item.index));
            }
        }
    });
    wait_op(&mut mainloop, &op);
    drop(op);
    index.get()
}

/// A2DP transport reset: suspend the card's sink and resume it. The Bluetooth
/// transport is released and re-acquired, but the sink node stays, so the
/// default output doesn't change and streams stay linked. This fixes the
/// one-earbud playback the AirPods fall into after a hi-res mic STOP (tested
/// with AirPods Pro, model A3063). Returns false if it couldn't be done.
pub fn suspend_resume(card: &str) -> bool {
    let _guard = lock();
    let Some(card_index) = card_index_by_name(card) else {
        return false;
    };
    let Some(&sink) = sinks_of_card(card_index).first() else {
        return false;
    };
    let Some((mut mainloop, context)) = connect() else {
        return false;
    };
    let mut introspect = context.introspect();
    let mut run = |suspend: bool| -> bool {
        let ok = Rc::new(Cell::new(false));
        let cb: Box<dyn FnMut(bool) + 'static> = {
            let ok = ok.clone();
            Box::new(move |success| ok.set(success))
        };
        let op = introspect.suspend_sink_by_index(sink, suspend, Some(cb));
        wait_op(&mut mainloop, &op);
        ok.get()
    };
    info!("[pw] reset A2DP transport (suspend/resume): {}", card);
    let suspended = run(true);
    std::thread::sleep(Duration::from_millis(400));
    // Always try to resume, even if the suspend reported failure.
    let resumed = run(false);
    if !(suspended && resumed) {
        warn!(
            "[pw] suspend/resume of {} failed (suspend={}, resume={})",
            card, suspended, resumed
        );
    }
    suspended && resumed
}

/// Sink indices that belong to a card.
fn sinks_of_card(card_index: u32) -> Vec<u32> {
    let Some((mut mainloop, context)) = connect() else {
        return Vec::new();
    };
    let introspect = context.introspect();
    let sinks: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let op = introspect.get_sink_info_list({
        let sinks = sinks.clone();
        move |result| {
            if let ListResult::Item(item) = result
                && item.card == Some(card_index)
            {
                sinks.borrow_mut().push(item.index);
            }
        }
    });
    wait_op(&mut mainloop, &op);
    drop(op);
    sinks.borrow().clone()
}

/// A playback stream remembered while the AirPods' card is off. PipeWire reuses
/// object ids, so the app name is kept too and checked before moving it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedStream {
    pub index: u32,
    pub app: Option<String>,
}

/// True if any playback stream is playing (not paused) on any output.
pub fn any_playing() -> bool {
    let Some((mut mainloop, context)) = connect() else {
        // Can't tell: assume so, the cautious answer for the callers.
        return true;
    };
    let playing = Rc::new(Cell::new(false));
    let op = context.introspect().get_sink_input_info_list({
        let playing = playing.clone();
        move |result| {
            if let ListResult::Item(item) = result
                && !item.corked
            {
                playing.set(true);
            }
        }
    });
    let done = wait_op(&mut mainloop, &op);
    drop(op);
    !done || playing.get()
}

/// All playback streams (sink-inputs) with their sink and app name.
fn list_streams() -> Vec<(SavedStream, u32)> {
    let Some((mut mainloop, context)) = connect() else {
        return Vec::new();
    };
    let introspect = context.introspect();
    let streams: Rc<RefCell<Vec<(SavedStream, u32)>>> = Rc::new(RefCell::new(Vec::new()));
    let op = introspect.get_sink_input_info_list({
        let streams = streams.clone();
        move |result| {
            if let ListResult::Item(item) = result {
                let app = item
                    .proplist
                    .get_str("application.name")
                    .or_else(|| item.name.as_ref().map(|n| n.to_string()));
                streams.borrow_mut().push((
                    SavedStream {
                        index: item.index,
                        app,
                    },
                    item.sink,
                ));
            }
        }
    });
    wait_op(&mut mainloop, &op);
    drop(op);
    streams.borrow().clone()
}

/// Playback streams currently going to a card's sinks.
pub fn streams_on_card(card_index: u32) -> Vec<SavedStream> {
    let sinks = sinks_of_card(card_index);
    if sinks.is_empty() {
        return Vec::new();
    }
    list_streams()
        .into_iter()
        .filter(|(_, sink)| sinks.contains(sink))
        .map(|(stream, _)| stream)
        .collect()
}

/// A playing (uncorked) playback stream and where its sound goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayingStream {
    pub stream: SavedStream,
    /// The sink it plays to now.
    pub sink: Option<String>,
    /// Pinned to a sink (`target.object` in PipeWire's metadata, e.g. after
    /// the user moved the stream) instead of following the default output.
    pub pinned: bool,
}

/// Where playback goes right now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackRouting {
    /// The output the user picked (PipeWire's configured default, kept while
    /// that sink is gone), else the current default.
    pub default_sink: Option<String>,
    /// `default_sink` is the configured default. Without it, a default that
    /// is gone (the AirPods with their card off) can't be seen.
    pub default_configured: bool,
    pub playing: Vec<PlayingStream>,
}

/// Snapshot of the playing streams and the default output. None if the sound
/// server can't be asked.
pub fn playback_routing() -> Option<PlaybackRouting> {
    // The pulse protocol shows neither the configured default nor which
    // streams are pinned: PipeWire keeps both in its "default" metadata.
    // Without pw-metadata (e.g. in the Flatpak) every stream counts as
    // following the current default, and the media controller remembers
    // whether the AirPods were the default when it switched their card off.
    let metadata = std::process::Command::new("pw-metadata")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_pw_metadata(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    let (mut mainloop, context) = connect()?;
    let introspect = context.introspect();
    let default_sink: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let op = introspect.get_server_info({
        let default_sink = default_sink.clone();
        move |info| {
            *default_sink.borrow_mut() = info.default_sink_name.as_ref().map(|n| n.to_string());
        }
    });
    let mut done = wait_op(&mut mainloop, &op);
    let sinks: Rc<RefCell<Vec<(u32, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let op = introspect.get_sink_info_list({
        let sinks = sinks.clone();
        move |result| {
            if let ListResult::Item(item) = result
                && let Some(name) = item.name.as_ref()
            {
                sinks.borrow_mut().push((item.index, name.to_string()));
            }
        }
    });
    done &= wait_op(&mut mainloop, &op);
    let playing: Rc<RefCell<Vec<PlayingStream>>> = Rc::new(RefCell::new(Vec::new()));
    let op = introspect.get_sink_input_info_list({
        let playing = playing.clone();
        let sinks = sinks.clone();
        move |result| {
            if let ListResult::Item(item) = result
                && !item.corked
            {
                let app = item
                    .proplist
                    .get_str("application.name")
                    .or_else(|| item.name.as_ref().map(|n| n.to_string()));
                // Metadata is keyed by PipeWire node id, not the pulse index.
                let node = item
                    .proplist
                    .get_str("object.id")
                    .and_then(|id| id.parse::<u32>().ok());
                playing.borrow_mut().push(PlayingStream {
                    stream: SavedStream {
                        index: item.index,
                        app,
                    },
                    sink: sinks
                        .borrow()
                        .iter()
                        .find(|(i, _)| *i == item.sink)
                        .map(|(_, n)| n.clone()),
                    pinned: node.is_some_and(|n| metadata.pinned.contains(&n)),
                });
            }
        }
    });
    done &= wait_op(&mut mainloop, &op);
    drop(op);
    if !done {
        return None;
    }
    let playing = playing.borrow().clone();
    Some(PlaybackRouting {
        default_configured: metadata.configured_sink.is_some(),
        default_sink: metadata
            .configured_sink
            .or_else(|| default_sink.borrow().clone()),
        playing,
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PwMetadata {
    configured_sink: Option<String>,
    /// Node ids of streams with a target.
    pinned: Vec<u32>,
}

/// Parse `pw-metadata` output, lines such as
/// `update: id:152 key:'target.object' value:'7179' type:'Spa:Id'`.
fn parse_pw_metadata(text: &str) -> PwMetadata {
    let mut out = PwMetadata::default();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("update: id:") else {
            continue;
        };
        let Some((id, rest)) = rest.split_once(" key:'") else {
            continue;
        };
        let Some((key, rest)) = rest.split_once("' value:'") else {
            continue;
        };
        let Some((value, _)) = rest.rsplit_once("' type:'") else {
            continue;
        };
        let Ok(id) = id.parse::<u32>() else {
            continue;
        };
        match key {
            "default.configured.audio.sink" if id == 0 => {
                out.configured_sink = serde_json::from_str::<serde_json::Value>(value)
                    .ok()
                    .and_then(|v| v.get("name")?.as_str().map(str::to_string));
            }
            "target.object" | "target.node" if value != "-1" => out.pinned.push(id),
            _ => {}
        }
    }
    out
}

/// Move saved playback streams that still exist (same index *and* app) to the
/// card's sink. Waits up to `wait` for the sink to appear after the profile was set.
pub fn move_streams_to_card(streams: &[SavedStream], card_index: u32, wait: Duration) {
    if streams.is_empty() {
        return;
    }
    let deadline = Instant::now() + wait;
    let sink = loop {
        if let Some(&s) = sinks_of_card(card_index).first() {
            break s;
        }
        if Instant::now() >= deadline {
            warn!(
                "[pw] no sink for card {} yet; not moving streams back",
                card_index
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let current = list_streams();
    let Some((mut mainloop, context)) = connect() else {
        return;
    };
    let mut introspect = context.introspect();
    let mut moved = 0;
    // Streams that follow the default output are usually back already.
    let already_there = streams
        .iter()
        .filter(|saved| current.iter().any(|(s, on)| s == *saved && *on == sink))
        .count();
    for saved in streams {
        let still_there = current
            .iter()
            .any(|(s, on_sink)| s == saved && *on_sink != sink);
        if !still_there {
            continue;
        }
        let ok = Rc::new(Cell::new(false));
        let cb: Box<dyn FnMut(bool) + 'static> = {
            let ok = ok.clone();
            Box::new(move |success| ok.set(success))
        };
        let op = introspect.move_sink_input_by_index(saved.index, sink, Some(cb));
        wait_op(&mut mainloop, &op);
        if ok.get() {
            moved += 1;
        }
    }
    info!(
        "[pw] {} saved stream(s): {} moved back to the AirPods, {} already there, {} gone",
        streams.len(),
        moved,
        already_there,
        streams.len() - moved - already_there
    );
}

#[derive(Clone, Copy, Debug)]
pub enum DefaultKind {
    Sink,
    Source,
}

/// Make desktop volume controls pick up the current default again.
///
/// When the default device reappears and becomes default in the same moment
/// (Bluetooth reconnect, card switched back on, virtual mic recreated), the
/// volume applet and volume-key handler of KDE Plasma (plasma-pa) can miss the
/// change: they keep showing the previous device as default (two devices look
/// selected) and volume keys act on the wrong device. Setting the same default
/// again emits no event, so briefly switch to another device and back. Only
/// done if `name` is the current default, so a different choice is never
/// overridden.
pub fn refresh_default(kind: DefaultKind, name: &str) {
    let Some((mut mainloop, mut context)) = connect() else {
        return;
    };
    let current: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let op = context.introspect().get_server_info({
        let current = current.clone();
        move |info| {
            let n = match kind {
                DefaultKind::Sink => info.default_sink_name.as_ref(),
                DefaultKind::Source => info.default_source_name.as_ref(),
            };
            *current.borrow_mut() = n.map(|n| n.to_string());
        }
    });
    wait_op(&mut mainloop, &op);
    drop(op);
    if current.borrow().as_deref() != Some(name) {
        return;
    }

    // Any other real device of the same kind (not a monitor source).
    let other: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let wanted = name.to_string();
    match kind {
        DefaultKind::Sink => {
            let op = context.introspect().get_sink_info_list({
                let other = other.clone();
                move |result| {
                    if let ListResult::Item(item) = result
                        && let Some(n) = item.name.as_ref()
                        && *n != *wanted
                        && other.borrow().is_none()
                    {
                        *other.borrow_mut() = Some(n.to_string());
                    }
                }
            });
            wait_op(&mut mainloop, &op);
        }
        DefaultKind::Source => {
            let op = context.introspect().get_source_info_list({
                let other = other.clone();
                move |result| {
                    if let ListResult::Item(item) = result
                        && item.monitor_of_sink.is_none()
                        && let Some(n) = item.name.as_ref()
                        && *n != *wanted
                        && other.borrow().is_none()
                    {
                        *other.borrow_mut() = Some(n.to_string());
                    }
                }
            });
            wait_op(&mut mainloop, &op);
        }
    }
    let Some(other) = other.borrow().clone() else {
        return;
    };

    let mut set = |target: &str| {
        let op = match kind {
            DefaultKind::Sink => context.set_default_sink(target, |_| {}),
            DefaultKind::Source => context.set_default_source(target, |_| {}),
        };
        wait_op(&mut mainloop, &op);
    };
    debug!(
        "[pw] refreshing default {:?} {} (via {})",
        kind, name, other
    );
    set(&other);
    std::thread::sleep(Duration::from_millis(150));
    set(name);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pw_metadata_gives_configured_sink_and_pinned_streams() {
        let text = r#"Found "default" metadata 40
update: id:0 key:'default.configured.audio.sink' value:'{ "name": "bluez_output.24_C4_06_94_4B_E1.1" }' type:'Spa:String:JSON'
update: id:0 key:'default.audio.sink' value:'{"name":"alsa_output.usb"}' type:'Spa:String:JSON'
update: id:152 key:'target.object' value:'7179' type:'Spa:Id'
update: id:153 key:'target.object' value:'-1' type:'Spa:Id'
"#;
        assert_eq!(
            parse_pw_metadata(text),
            PwMetadata {
                configured_sink: Some("bluez_output.24_C4_06_94_4B_E1.1".to_string()),
                pinned: vec![152],
            }
        );
    }

    #[test]
    fn pw_metadata_without_entries_is_empty() {
        assert_eq!(parse_pw_metadata(""), PwMetadata::default());
    }
}
