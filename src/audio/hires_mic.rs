//! Hi-res microphone lifecycle: a persistent virtual input device plus a monitor
//! that runs the AACP 0x58 capture only while an app is recording from it.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::task::JoinHandle as TokioHandle;

use crate::audio::card::NOT_WORN_GRACE;
use crate::audio::eld::{ELD_CHANNELS, ELD_FRAME_SAMPLES, ELD_SAMPLE_RATE, EldDecoder};
use crate::audio::mic_mute;
use crate::audio::output::{self, Output, VirtualMic};
use crate::bluetooth::aacp::AACPManager;
use crate::bluetooth::aacp_audio;

/// Delay after sending 0x58 STOP before resetting the A2DP transport
const A2DP_RESET_DELAY: Duration = Duration::from_millis(800);
/// How often the monitor polls the virtual source for activity
const POLL_INTERVAL: Duration = Duration::from_millis(400);
/// If no audio SDUs arrive for this long while a capture is active, the stream
/// is considered stalled and the capture is torn down and restarted.
const STALL_TIMEOUT: Duration = Duration::from_millis(2000);
/// How long the AirPods mic stream keeps running after the last app stopped
/// recording. Every STOP needs an A2DP reset (a short audible dip), and apps
/// close and reopen the mic often: push-to-talk dictation does it for every
/// sentence, voice chat apps when rejoining a call or changing settings.
/// Lingering turns a burst of open/close into one START and one STOP.
const RECORDER_GONE_GRACE: Duration = Duration::from_secs(60);
/// Upper bound for the delay between stall restarts that keep failing.
const MAX_RESTART_BACKOFF: Duration = Duration::from_secs(30);
/// After this many restarts in a row without audio the headset profile takes
/// over the same virtual mic (see Fallback), so apps keep their device.
const MAX_FAILED_RESTARTS: u32 = 2;
/// The headset profile costs playback quality, so it's left this soon after the
/// last recorder; the next recording tries the AirPods stream first again.
const FALLBACK_IDLE: Duration = Duration::from_secs(3);
/// How many polls between checks that WirePlumber's AirPods mic is still hidden
/// (a WirePlumber restart resets its runtime setting).
const WIREPLUMBER_RECHECK_POLLS: u32 = 25;
const LEVEL_RELEASE: f32 = 0.85;
/// After connecting, how long a parked mic waits for the first ear-detection
/// report before it is unparked anyway (ear detection may be switched off).
const EAR_REPORT_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, PartialEq, Eq)]
enum CaptureAction {
    /// An app started recording while a bud is in an ear: send 0x58 START.
    Start,
    /// Capturing and the device delivers audio: nothing to do.
    Keep,
    /// Capturing but no audio SDUs for STALL_TIMEOUT while worn: STOP + START.
    Restart,
    /// Capture must end (STOP, then an A2DP reset).
    Stop,
    /// Not capturing and nothing to start.
    Idle,
}

/// Decide what the hi-res monitor does this poll. The AirPods only deliver mic
/// audio while a bud is in an ear, so capture is tied to `worn` (which includes
/// the not-worn grace period); otherwise the stall watchdog would keep
/// restarting the stream while the buds are out.
fn capture_action(
    enabled: bool,
    recording: bool,
    worn: bool,
    capturing: bool,
    stalled: bool,
) -> CaptureAction {
    let want = enabled && recording && worn;
    match (want, capturing) {
        (true, false) => CaptureAction::Start,
        (true, true) if stalled => CaptureAction::Restart,
        (true, true) => CaptureAction::Keep,
        (false, true) => CaptureAction::Stop,
        (false, false) => CaptureAction::Idle,
    }
}

/// Process-wide "an app is recording from the hi-res mic right now" (there is
/// one AirPods connection at a time). Used to avoid disruptive actions such as a
/// WirePlumber restart in the middle of a call.
static ANY_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn any_capture_active() -> bool {
    ANY_CAPTURE_ACTIVE.load(Ordering::Relaxed)
}

/// The card is on a headset profile because the AirPods stream failed and the
/// headset mic feeds the virtual mic. The media controller must not switch it
/// back to A2DP meanwhile.
static HEADSET_FALLBACK_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn headset_fallback_active() -> bool {
    HEADSET_FALLBACK_ACTIVE.load(Ordering::Relaxed)
}

#[derive(Clone, Default)]
pub struct MicStatus {
    level: Arc<AtomicU32>,
    active: Arc<AtomicBool>,
    app: Arc<Mutex<Option<String>>>,
    last_sdu: Arc<Mutex<Option<Instant>>>,
    /// An audio SDU arrived since the capture last (re)started. The stall clock
    /// is refreshed at every start without audio, so only this may clear the
    /// failed-restart count.
    got_audio: Arc<AtomicBool>,
    /// An app recorded from the virtual mic at the last poll. Unlike `active`,
    /// it drops as soon as the app stops and holds through a capture restart.
    app_recording: Arc<AtomicBool>,
}

impl MicStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn recording(&self) -> bool {
        self.app_recording.load(Ordering::Relaxed)
    }

    fn set_recording(&self, recording: bool) {
        self.app_recording.store(recording, Ordering::Relaxed);
    }

    pub fn set_level(&self, level: f32) {
        self.level.store(level.to_bits(), Ordering::Relaxed);
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn app(&self) -> Option<String> {
        lock(&self.app).clone()
    }

    fn set_capture(&self, app: Option<String>) {
        self.active.store(app.is_some(), Ordering::Relaxed);
        ANY_CAPTURE_ACTIVE.store(app.is_some(), Ordering::Relaxed);
        *lock(&self.app) = app;
    }

    fn reset(&self) {
        self.active.store(false, Ordering::Relaxed);
        ANY_CAPTURE_ACTIVE.store(false, Ordering::Relaxed);
        *lock(&self.app) = None;
        *lock(&self.last_sdu) = None;
        self.set_level(0.0);
    }

    /// Record that an audio SDU just arrived from the device.
    pub fn mark_sdu(&self) {
        *lock(&self.last_sdu) = Some(Instant::now());
        self.got_audio.store(true, Ordering::Relaxed);
    }

    /// A capture (re)started: start the stall clock with no audio seen yet.
    fn mark_start(&self) {
        *lock(&self.last_sdu) = Some(Instant::now());
        self.got_audio.store(false, Ordering::Relaxed);
    }

    fn got_audio(&self) -> bool {
        self.got_audio.load(Ordering::Relaxed)
    }

    /// Time since the last audio SDU, or None if none has arrived yet.
    fn since_last_sdu(&self) -> Option<Duration> {
        lock(&self.last_sdu).map(|t| t.elapsed())
    }
}

/// A poisoned lock only means another thread panicked mid-update; these
/// values stay usable, so carry on with them.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct HiResMic {
    stop: Arc<Notify>,
    monitor: Option<TokioHandle<()>>,
}

impl HiResMic {
    // Make the shared virtual input available (see SharedMic) and spawn the
    // activity monitor. When the monitor exits the input is kept for now (see
    // release_mic), or unloaded if the hi-res mic was turned off.
    pub async fn start(aacp: &AACPManager, addr: String, status: MicStatus) -> Option<HiResMic> {
        // Name it after the AirPods exactly, like their output, so the desktop
        // shows one AirPods microphone.
        let device_name = {
            let addr = addr.clone();
            blocking(move || {
                let json = std::fs::read_to_string(crate::utils::get_devices_path()).ok()?;
                let devices: std::collections::HashMap<String, crate::devices::enums::DeviceData> =
                    serde_json::from_str(&json).ok()?;
                devices
                    .get(&addr)
                    .map(|d| d.name.trim().to_string())
                    .filter(|n| !n.is_empty() && n != &addr)
            })
            .await
            .flatten()
        };
        let mic_name = match device_name {
            Some(name) => name,
            None => "AirPods".to_string(),
        };
        let created = {
            let name = mic_name.clone();
            blocking(move || acquire_mic(&name)).await.flatten()?
        };
        if created {
            refresh_desktop_default_source(aacp.runtime());
        }
        blocking(output::hide_wireplumber_mic).await;
        let stop = Arc::new(Notify::new());
        let wake = aacp.hires_wake();
        output::watch_recorders(wake.clone());
        // Spawn on the backend runtime, not the caller's (the UI toggle uses a
        // throwaway runtime that would abort this task immediately).
        let monitor = aacp.runtime().spawn(monitor_loop(
            aacp.clone(),
            addr,
            status,
            stop.clone(),
            wake,
            mic_name,
        ));
        Some(HiResMic {
            stop,
            monitor: Some(monitor),
        })
    }

    // Stop the monitor (tearing down any active capture) and unload the device.
    pub async fn stop(mut self) {
        self.stop.notify_one();
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.await;
        }
    }

    pub fn is_running(&self) -> bool {
        self.monitor.as_ref().is_some_and(|h| !h.is_finished())
    }
}

// A live capture session: the playback stream feeding the sink plus its decode
// thread, started when an app opens the mic.
struct Capture {
    decode_thread: Option<JoinHandle<()>>,
}

// The AirPods stream failed: the card is on its headset profile and parec
// copies that profile's mic into the virtual mic, so apps keep their device.
struct Fallback {
    /// The profile to go back to (the A2DP one that was active).
    previous_profile: Option<String>,
    feed: Option<std::process::Child>,
    idle_since: Option<Instant>,
}

fn card_name(addr: &str) -> String {
    format!("bluez_card.{}", addr.replace(':', "_"))
}

/// Blocking.
fn enter_fallback(addr: &str) -> Fallback {
    let card = card_name(addr);
    let previous_profile =
        crate::audio::card::active_profile(crate::audio::card::CardRef::Name(&card));
    HEADSET_FALLBACK_ACTIVE.store(true, Ordering::Relaxed);
    match crate::audio::card::best_headset_profile(&card) {
        Some(profile) => {
            if crate::audio::card::set_profile(crate::audio::card::CardRef::Name(&card), &profile) {
                info!(
                    "[hires] AirPods stream unavailable; card on {} meanwhile",
                    profile
                );
            }
        }
        None => error!("[hires] no headset profile to fall back to on {}", card),
    }
    Fallback {
        previous_profile,
        feed: None,
        idle_since: None,
    }
}

/// Start copying the headset mic into the virtual mic. Blocking.
fn start_feed(addr: &str) -> Option<std::process::Child> {
    let source = output::headset_source(addr)?;
    let mut output = Output::open(ELD_SAMPLE_RATE, ELD_CHANNELS as u8)?;
    let mut child = std::process::Command::new("parec")
        .args([
            "-d",
            &source,
            "--raw",
            "--format=s16le",
            "--channels=1",
            &format!("--rate={ELD_SAMPLE_RATE}"),
            "--latency-msec=20",
            "--client-name=podbridge-headset-feed",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    std::thread::Builder::new()
        .name("headset-feed".into())
        .spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            let mut pcm: Vec<i16> = Vec::with_capacity(buf.len() / 2);
            while let Ok(n) = stdout.read(&mut buf) {
                if n < 2 {
                    break;
                }
                pcm.clear();
                pcm.extend(
                    buf[..n - n % 2]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| i16::from_le_bytes([b[0], b[1]])),
                );
                if output.write(&pcm).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    info!("[hires] feeding the AirPods mic from {}", source);
    Some(child)
}

/// Leave the headset profile. Blocking.
fn leave_fallback(mut fallback: Fallback, addr: &str, restore_profile: bool) {
    if let Some(mut feed) = fallback.feed.take() {
        let _ = feed.kill();
        let _ = feed.wait();
    }
    if restore_profile {
        let card = card_name(addr);
        let profile = fallback
            .previous_profile
            .filter(|p| p.starts_with("a2dp"))
            .unwrap_or_else(|| "a2dp-sink".to_string());
        crate::audio::card::set_profile(crate::audio::card::CardRef::Name(&card), &profile);
        info!(
            "[hires] back on {}; the next recording tries the AirPods stream first",
            profile
        );
    }
    HEADSET_FALLBACK_ACTIVE.store(false, Ordering::Relaxed);
}

async fn monitor_loop(
    aacp: AACPManager,
    addr: String,
    status: MicStatus,
    stop: Arc<Notify>,
    wake: Arc<Notify>,
    mic_name: String,
) {
    let mut monitor = Monitor::new(aacp, addr, status, mic_name).await;
    info!(
        "[hires] activity monitor started, watching '{}'",
        output::SOURCE_NAME
    );

    loop {
        tokio::select! {
            _ = stop.notified() => break,
            _ = wake.notified() => {}
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        if !monitor.poll().await {
            break;
        }
    }

    monitor.finish().await;
}

/// Whether a bud is in an ear, as one poll of the monitor sees it.
struct Wearing {
    /// A bud is in an ear right now.
    now: bool,
    /// The AirPods reported their ear state (or we stopped waiting for it).
    known: bool,
    /// `now`, or no bud for less than the not-worn grace.
    worn: bool,
}

/// State of the hi-res activity monitor, kept across polls.
struct Monitor {
    aacp: AACPManager,
    addr: String,
    status: MicStatus,
    mic_name: String,
    /// False while parked: with no bud worn (past the grace period) apps are
    /// sent to another mic, like the output is. A mic that was parked before
    /// this connection (disconnect, earlier run) stays parked until a bud is
    /// worn.
    shown: bool,
    started: Instant,
    capture: Option<Capture>,
    /// Conversation detection value saved while we override it off for capture.
    old_convo_state: Option<bool>,
    restart_backoff: Duration,
    next_restart_at: Instant,
    /// Earliest next START after one failed (don't retry on every poll).
    next_start_at: Instant,
    failed_restarts: u32,
    fallback: Option<Fallback>,
    polls: u32,
    /// When the recorder disappeared while capturing (stop grace period).
    recorder_gone_since: Option<Instant>,
    source_missing_polls: u32,
    /// When the last bud left an ear (None while worn).
    out_since: Option<Instant>,
    /// A bud was reported in an ear on this connection. The not-worn grace
    /// only applies after that: AirPods that connect from the open case were
    /// never worn, so there is nothing to ride out.
    seen_worn: bool,
    /// A STOP happened with the buds out and the card stayed on: the transport
    /// is in the post-STOP state, so reset it once a bud is back (unless a
    /// START does).
    reset_when_worn: bool,
    /// How long nothing has recorded, to end a squeeze mute.
    mute_idle: mic_mute::Idle,
}

impl Monitor {
    async fn new(aacp: AACPManager, addr: String, status: MicStatus, mic_name: String) -> Self {
        let shown = !blocking(mic_is_parked).await.unwrap_or(false);
        Monitor {
            aacp,
            addr,
            status,
            mic_name,
            shown,
            started: Instant::now(),
            capture: None,
            old_convo_state: None,
            restart_backoff: STALL_TIMEOUT,
            next_restart_at: Instant::now(),
            next_start_at: Instant::now(),
            failed_restarts: 0,
            fallback: None,
            polls: 0,
            recorder_gone_since: None,
            source_missing_polls: 0,
            out_since: None,
            seen_worn: false,
            reset_when_worn: false,
            mute_idle: mic_mute::Idle::default(),
        }
    }

    /// One poll. Returns false when the monitor should stop.
    async fn poll(&mut self) -> bool {
        self.polls = self.polls.wrapping_add(1);
        if self.polls.is_multiple_of(WIREPLUMBER_RECHECK_POLLS) {
            blocking(output::hide_wireplumber_mic).await;
        }

        let wearing = self.wearing().await;

        // Bring apps back to the virtual mic as soon as a bud is back.
        if wearing.now && !self.shown && wearing.known {
            info!("[hires] bud back in; unparking '{}'", output::SOURCE_NAME);
            blocking(unpark_mic).await;
            self.shown = true;
        }

        let app = self.recording_app().await;
        let enabled = self.aacp.hires_mic_enabled();
        let recording = self.recording(app.is_some());
        self.status.set_recording(app.is_some());
        // The next recording must not start silent.
        let mut idle = std::mem::take(&mut self.mute_idle);
        self.mute_idle = blocking(move || {
            mic_mute::end_when_idle(&mut idle);
            idle
        })
        .await
        .unwrap_or_default();

        if self.fallback.is_some() {
            self.poll_fallback(enabled && app.is_some() && wearing.worn)
                .await;
            return true;
        }

        // A stall only counts while a bud is really in: during the not-worn
        // grace the AirPods simply don't send mic audio.
        let stalled = wearing.now
            && self.capture.is_some()
            && self
                .status
                .since_last_sdu()
                .is_some_and(|d| d > STALL_TIMEOUT);

        let action = capture_action(
            enabled,
            recording,
            wearing.worn,
            self.capture.is_some(),
            stalled,
        );
        if wearing.now && self.reset_when_worn && action != CaptureAction::Start {
            info!("[hires] bud back after a STOP with buds out; resetting A2DP");
            schedule_a2dp_reset(&self.addr);
        }
        if wearing.now || action == CaptureAction::Start {
            self.reset_when_worn = false;
        }

        match action {
            CaptureAction::Start if !wearing.known || Instant::now() < self.next_start_at => {}
            CaptureAction::Start => self.start(app).await,
            CaptureAction::Keep => {
                if app.is_some() {
                    self.status.set_capture(app);
                }
                // A restart refreshes the stall clock without any audio, so
                // only audio clears the count.
                if wearing.now && self.status.got_audio() {
                    self.restart_backoff = STALL_TIMEOUT;
                    self.failed_restarts = 0;
                }
            }
            CaptureAction::Restart => {
                if app.is_some() {
                    self.status.set_capture(app);
                }
                if self.restart_or_fall_back().await {
                    // Now on the headset profile: the fallback takes over from
                    // the next poll.
                    return true;
                }
            }
            // Disabled, recorder gone, or no bud worn (past the grace): end the
            // 0x58 stream.
            CaptureAction::Stop => {
                info!(
                    "[hires] stopping capture (enabled={}, recording={}, worn={})",
                    enabled, recording, wearing.worn
                );
                self.stop(wearing.now).await;
            }
            CaptureAction::Idle => {
                self.recorder_gone_since = None;
            }
        }

        if !wearing.worn {
            self.park_if_unused().await;
        }

        // Once disabled and nothing is recording from the source, unload it.
        let mute_pending = blocking(mic_mute::pending).await.unwrap_or(true);
        monitor_stays(enabled, recording, mute_pending)
    }

    /// Apps close and reopen their mic stream often (push-to-talk, rejoining
    /// a call, changing audio settings). Keep capturing through the gap; see
    /// RECORDER_GONE_GRACE. True while an app records or the gap lasts.
    fn recording(&mut self, app_present: bool) -> bool {
        if app_present {
            self.recorder_gone_since = None;
        } else if self.capture.is_some() && self.recorder_gone_since.is_none() {
            self.recorder_gone_since = Some(Instant::now());
        }
        let in_grace = self.capture.is_some()
            && self
                .recorder_gone_since
                .is_some_and(|t| t.elapsed() < RECORDER_GONE_GRACE);
        app_present || in_grace
    }

    /// No bud worn past the grace period: park the virtual mic so apps (and
    /// the default source) switch to another mic, matching the output that
    /// the media controller switches away at the same moment.
    async fn park_if_unused(&mut self) {
        if self.capture.is_none()
            && self.shown
            && crate::utils::AppSettings::load().audio_off_when_not_wearing
            && blocking(park_mic).await.unwrap_or(false)
        {
            info!(
                "[hires] no bud in ear; parked '{}' so apps use another mic",
                output::SOURCE_NAME
            );
            self.shown = false;
        }
    }

    /// Wearing, with a grace period: taking both buds out briefly (adjusting
    /// them) must not stop the mic, hide it or switch outputs.
    async fn wearing(&mut self) -> Wearing {
        let now = self.aacp.is_worn().await;
        // Right after connecting is_worn() is only a guess until the AirPods
        // report their ear state (they connect when the case opens, not yet
        // worn): wait for the report, or briefly in case ear detection is off.
        // The report also marks the end of the AACP handshake; a mic START sent
        // before that is ignored by the AirPods (the capture then stalls and has
        // to be restarted) and lands in the middle of A2DP setup.
        let known =
            self.aacp.ear_status_reported().await || self.started.elapsed() > EAR_REPORT_WAIT;
        if now {
            self.out_since = None;
            self.seen_worn |= known;
        } else if self.out_since.is_none() {
            self.out_since = Some(Instant::now());
        }
        let worn =
            now || (self.seen_worn && self.out_since.is_some_and(|t| t.elapsed() < NOT_WORN_GRACE));
        Wearing { now, known, worn }
    }

    /// The app recording from the virtual mic, if it may start the AirPods mic.
    /// Recreates the virtual mic if the sound server lost it.
    async fn recording_app(&mut self) -> Option<String> {
        let usage = blocking(|| output::source_consumer(output::SOURCE_NAME))
            .await
            .unwrap_or(output::SourceUse::Present(None));
        match usage {
            // Parked: whoever still records from it gets silence; don't start
            // the AirPods mic for them.
            output::SourceUse::Present(_) if !self.shown => {
                self.source_missing_polls = 0;
                None
            }
            output::SourceUse::Present(app) => {
                self.source_missing_polls = 0;
                app
            }
            output::SourceUse::Missing => {
                // pipewire-pulse restarted (or the module was unloaded): the
                // virtual mic is gone and would never come back until the AirPods
                // reconnect. Recreate it after two polls in a row.
                self.source_missing_polls += 1;
                if self.source_missing_polls >= 2 {
                    self.recreate_source().await;
                }
                None
            }
        }
    }

    async fn recreate_source(&mut self) {
        warn!(
            "[hires] '{}' disappeared; recreating it",
            output::SOURCE_NAME
        );
        if let Some(c) = self.capture.take() {
            // It writes into the old, now orphaned FIFO.
            stop_capture(c, &self.aacp).await;
            schedule_a2dp_reset(&self.addr);
            self.restore_convo().await;
            self.status.reset();
        }
        let name = self.mic_name.clone();
        if blocking(move || recreate_mic(&name)).await.unwrap_or(false) {
            blocking(mic_mute::restore).await;
            if !self.shown {
                // No-op if the parked state was carried over.
                blocking(park_mic).await;
            }
            refresh_desktop_default_source(&tokio::runtime::Handle::current());
            self.source_missing_polls = 0;
        }
    }

    /// One poll on the headset profile: keep the feed running while wanted,
    /// leave the profile once it has been idle for FALLBACK_IDLE.
    async fn poll_fallback(&mut self, wanted: bool) {
        let Some(mut fb) = self.fallback.take() else {
            return;
        };
        if wanted {
            fb.idle_since = None;
            if fb
                .feed
                .as_mut()
                .is_none_or(|c| c.try_wait().ok().flatten().is_some())
            {
                let feed_addr = self.addr.clone();
                fb.feed = blocking(move || start_feed(&feed_addr)).await.flatten();
            }
        } else if fb.idle_since.is_none() {
            fb.idle_since = Some(Instant::now());
        } else if fb.idle_since.is_some_and(|t| t.elapsed() >= FALLBACK_IDLE) {
            let leave_addr = self.addr.clone();
            blocking(move || leave_fallback(fb, &leave_addr, true)).await;
            self.restore_convo().await;
            self.status.reset();
            self.restart_backoff = STALL_TIMEOUT;
            self.failed_restarts = 0;
            return;
        }
        self.fallback = Some(fb);
    }

    async fn start(&mut self, app: Option<String>) {
        info!("[hires] recorder detected ({:?}), starting capture", app);
        let new_capture = start_capture(&self.aacp, &self.addr, &self.status).await;
        if new_capture.is_none() {
            warn!("[hires] could not start the capture; retrying shortly");
            self.next_start_at = Instant::now() + STALL_TIMEOUT;
        }
        if let Some(c) = new_capture {
            self.status.set_capture(app);
            self.capture = Some(c);

            // Only disable (and remember to restore) if it was on.
            self.aacp.take_convo_user_changed();
            if crate::utils::AppSettings::load().hires_mic_pause_convo
                && self.aacp.conversation_detection_enabled().await
            {
                self.old_convo_state = Some(true);
                self.aacp.set_conversation_detection(false).await;
            }
        }
    }

    /// The capture stalled. Returns true if it fell back to the headset profile.
    async fn restart_or_fall_back(&mut self) -> bool {
        if self.failed_restarts >= MAX_FAILED_RESTARTS {
            warn!(
                "[hires] no mic audio after {} restarts; falling back to the headset profile",
                self.failed_restarts
            );
            // No A2DP reset: the card leaves A2DP right away.
            if let Some(c) = self.capture.take() {
                stop_capture(c, &self.aacp).await;
            }
            let fb_addr = self.addr.clone();
            self.fallback = blocking(move || enter_fallback(&fb_addr)).await;
            return true;
        }
        if Instant::now() >= self.next_restart_at {
            self.failed_restarts += 1;
            warn!(
                "[hires] no audio from device for {}ms, restarting capture",
                STALL_TIMEOUT.as_millis()
            );
            if let Some(c) = self.capture.take() {
                stop_capture(c, &self.aacp).await;
            }
            self.capture = start_capture(&self.aacp, &self.addr, &self.status).await;
            if self.capture.is_none() {
                warn!("[hires] capture restart failed; retrying shortly");
                self.status.reset();
                // The STOP went out without a START after it.
                schedule_a2dp_reset(&self.addr);
                self.next_start_at = Instant::now() + STALL_TIMEOUT;
            }
            // Back off if the device keeps not delivering (e.g. another
            // device holds the AirPods' mic) instead of restarting every
            // couple of seconds.
            self.next_restart_at = Instant::now() + self.restart_backoff;
            self.restart_backoff = (self.restart_backoff * 2).min(MAX_RESTART_BACKOFF);
        }
        false
    }

    async fn stop(&mut self, worn_now: bool) {
        if let Some(c) = self.capture.take() {
            stop_capture(c, &self.aacp).await;
        }
        if worn_now {
            // After a STOP the AirPods keep playing from one earbud only
            // until the A2DP transport is reset (e.g. after leaving a call).
            schedule_a2dp_reset(&self.addr);
        } else if !crate::utils::AppSettings::load().audio_off_when_not_wearing {
            // Buds out and the card stays on: reset once a bud is back.
            // When the card is being switched off instead, there's nothing
            // to do: switching it back on creates a fresh transport.
            self.reset_when_worn = true;
        }
        self.restore_convo().await;
        self.status.reset();
        self.restart_backoff = STALL_TIMEOUT;
        self.failed_restarts = 0;
        self.recorder_gone_since = None;
    }

    async fn restore_convo(&mut self) {
        if let Some(prev) = self.old_convo_state.take() {
            restore_conversation_detection(&self.aacp, prev).await;
        }
    }

    /// The monitor stopped: end any capture or fallback and release the mic.
    async fn finish(mut self) {
        // A squeeze mute stays: an app may go on recording from the mic.
        self.status.set_recording(false);
        if let Some(c) = self.capture.take() {
            stop_capture(c, &self.aacp).await;
            self.restore_convo().await;
            // The AirPods are still connected (podbridge is exiting, or the monitor
            // is being replaced): the STOP needs its A2DP reset like any other, or
            // they keep playing from one earbud. Done here rather than scheduled,
            // since the process may exit before a scheduled one runs.
            if self.aacp.is_connected() {
                tokio::time::sleep(A2DP_RESET_DELAY).await;
                let reset_addr = self.addr.clone();
                blocking(move || output::reset_a2dp(&reset_addr)).await;
            }
        }
        if let Some(fb) = self.fallback.take() {
            let connected = self.aacp.is_connected();
            let leave_addr = self.addr.clone();
            blocking(move || leave_fallback(fb, &leave_addr, connected)).await;
            self.restore_convo().await;
        }
        self.status.reset();
        blocking(output::restore_wireplumber_mic).await;

        if self.aacp.hires_mic_enabled()
            // A muted mic stays even with the feature off.
            || blocking(mic_mute::holds_the_mic).await.unwrap_or(true)
        {
            // Disconnected (or podbridge exiting): keep the virtual mic for now.
            release_mic(&tokio::runtime::Handle::current());
        } else {
            blocking(remove_mic).await;
        }
    }
}

/// The virtual mic was just created: make desktop volume controls pick up the
/// default source again (see card::refresh_default). That briefly switches the
/// default to another mic and back, which moves every app recording from the
/// default along with it, so it's skipped while anything records.
fn refresh_desktop_default_source(runtime: &tokio::runtime::Handle) {
    runtime.spawn(async {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = tokio::task::spawn_blocking(|| {
            if output::any_recorder() {
                debug!("[hires] an app is recording; not refreshing the default source");
                return;
            }
            crate::audio::card::refresh_default(
                crate::audio::card::DefaultKind::Source,
                output::SOURCE_NAME,
            );
        })
        .await;
    });
}

// --- The virtual mic, shared by every AirPods connection --------------------
//
// Some apps crash when a microphone is added or removed while they record:
// Discord's voice engine aborts (in libpulse, from its mic volume control) on
// any source appearing or disappearing, even one it doesn't use. So the
// virtual mic is kept: across short disconnects, with no bud worn, and across
// podbridge restarts (a new run adopts the loaded module). It is unloaded when
// the hi-res mic is turned off, and when the AirPods stayed gone past
// DISCONNECT_PARK_DELAY while no app records from any mic, or when podbridge
// starts without AirPods and finds it left over: a mic without AirPods should
// not stay listed. The next connection creates it again, which
// an app recording from another mic at that moment can still trip over.
//
// While the AirPods can't deliver mic audio the mic is "parked": another mic
// becomes the default and apps pinned to ours are moved to it, then both are
// undone when the AirPods are back. Apps see an ordinary device switch.

/// After a disconnect, how long the mic stays as it is before it is parked. A
/// short drop (the AirPods reconnect right away) then changes nothing at all.
const DISCONNECT_PARK_DELAY: Duration = Duration::from_secs(15);
/// After switching the default mic, time for the session manager to move the
/// streams that follow the default before we look for the ones left behind.
const DEFAULT_SWITCH_SETTLE: Duration = Duration::from_millis(400);

struct SharedMic {
    vmic: VirtualMic,
    /// Some while parked.
    parked: Option<Parked>,
    /// AirPods connection monitors using it (normally 0 or 1; briefly 2 while a
    /// reconnect replaces a stale connection).
    users: u32,
    /// Bumped on every acquire, so a delayed park only acts on the release it
    /// was scheduled for.
    generation: u64,
}

/// What parking changed, to undo it when the AirPods are back.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Parked {
    /// The mic made default while parked (only if ours was the default).
    default_set: Option<String>,
    /// Recorders still on our mic after the default switch (apps pinned to it)
    /// that were moved to the other mic.
    moved: Vec<output::Recorder>,
}

impl Parked {
    fn serialize(&self) -> String {
        let mut s = String::new();
        if let Some(d) = &self.default_set {
            s.push_str(&format!("default\t{d}\n"));
        }
        for r in &self.moved {
            s.push_str(&format!(
                "moved\t{}\t{}\n",
                r.index,
                r.app.as_deref().unwrap_or("")
            ));
        }
        s
    }

    fn parse(text: &str) -> Parked {
        let mut p = Parked::default();
        for line in text.lines() {
            let mut fields = line.splitn(3, '\t');
            match (fields.next(), fields.next(), fields.next()) {
                (Some("default"), Some(name), None) if !name.is_empty() => {
                    p.default_set = Some(name.to_string());
                }
                (Some("moved"), Some(index), app) => {
                    if let Ok(index) = index.parse() {
                        p.moved.push(output::Recorder {
                            index,
                            app: app.filter(|a| !a.is_empty()).map(str::to_string),
                        });
                    }
                }
                _ => {}
            }
        }
        p
    }
}

static SHARED_MIC: Mutex<Option<SharedMic>> = Mutex::new(None);

/// Parked state, kept on disk so it can still be undone by a later run. It is
/// in the state directory rather than next to the FIFO because it must outlive
/// the session: parking at shutdown makes another mic the default, and the
/// session manager remembers that default across a reboot. Without this file
/// the AirPods mic would then never become the default again by itself.
fn parked_path() -> std::path::PathBuf {
    crate::utils::app_dir("XDG_STATE_HOME", ".local/state").join("hires-mic.parked")
}

/// Parked state saved by an earlier run or before the sound server restarted,
/// for a virtual mic that was created anew since. The default-mic switch can
/// still be undone. The moved recorders can't: their stream indices belong to
/// the old sound server and may now name other apps' streams.
fn carried_over(parked: Option<Parked>) -> Option<Parked> {
    let default_set = parked?.default_set?;
    Some(Parked {
        default_set: Some(default_set),
        moved: Vec::new(),
    })
}

fn load_parked() -> Option<Parked> {
    std::fs::read_to_string(parked_path())
        .ok()
        .map(|t| Parked::parse(&t))
}

fn save_parked(parked: Option<&Parked>) {
    match parked {
        Some(p) => {
            if let Err(e) = crate::utils::write_atomic(&parked_path(), p.serialize().as_bytes()) {
                warn!("[hires] could not save the parked mic state: {}", e);
            }
        }
        None => {
            let _ = std::fs::remove_file(parked_path());
        }
    }
}

fn shared_mic() -> std::sync::MutexGuard<'static, Option<SharedMic>> {
    SHARED_MIC.lock().unwrap_or_else(|e| e.into_inner())
}

/// Send apps to another mic (see the section comment). Blocking.
fn park() -> Parked {
    let mut parked = Parked::default();
    let Some(fallback) = output::fallback_source(output::SOURCE_NAME) else {
        info!(
            "[hires] no other microphone to switch apps to; leaving them on '{}'",
            output::SOURCE_NAME
        );
        return parked;
    };
    if output::default_source().as_deref() == Some(output::SOURCE_NAME) {
        if output::set_default_source(&fallback) {
            info!(
                "[hires] AirPods mic unavailable; default microphone is now {}",
                fallback
            );
            parked.default_set = Some(fallback.clone());
            std::thread::sleep(DEFAULT_SWITCH_SETTLE);
        } else {
            warn!("[hires] could not make {} the default microphone", fallback);
        }
    }
    let left = output::recorders_on(output::SOURCE_NAME);
    parked.moved = output::move_recorders(&left, &fallback);
    parked
}

/// Undo park(): restore our mic as the default (unless the user picked another
/// one meanwhile) and move the apps that were moved back. Blocking.
fn unpark(parked: &Parked) {
    if let Some(fallback) = &parked.default_set {
        if output::default_source().as_deref() == Some(fallback.as_str()) {
            if output::set_default_source(output::SOURCE_NAME) {
                info!("[hires] AirPods mic back; it's the default microphone again");
            }
        } else {
            info!("[hires] default microphone was changed meanwhile; leaving it");
        }
    }
    if !parked.moved.is_empty() {
        output::move_recorders(&parked.moved, output::SOURCE_NAME);
    }
}

/// Make the virtual mic available to an AirPods connection: reuse the existing
/// one or adopt/create it. Returns Some(true) if a new device was
/// created, Some(false) if an existing one is used. Blocking.
fn acquire_mic(description: &str) -> Option<bool> {
    let mut guard = shared_mic();
    let (mic, created) = match guard.as_mut() {
        // If it's parked it stays parked: the monitor unparks it once a bud
        // is worn (see monitor_loop).
        Some(mic) => (mic, false),
        None => {
            let (vmic, created) = match VirtualMic::adopt(description) {
                Some(vmic) => (vmic, false),
                None => (
                    VirtualMic::open(ELD_SAMPLE_RATE, ELD_CHANNELS as u8, description)?,
                    true,
                ),
            };
            // An earlier run may have parked the mic; keep that state so the
            // monitor can undo it once a bud is worn. For a freshly created
            // mic (after a reboot or logout) only the default-mic switch is
            // left to undo.
            let parked = if created {
                let parked = carried_over(load_parked());
                save_parked(parked.as_ref());
                parked
            } else {
                load_parked()
            };
            let mic = guard.insert(SharedMic {
                vmic,
                parked,
                users: 0,
                generation: 0,
            });
            (mic, created)
        }
    };
    mic.users += 1;
    mic.generation += 1;
    Some(created)
}

/// True if the mic exists and is parked. Blocking.
fn mic_is_parked() -> bool {
    shared_mic().as_ref().is_some_and(|m| m.parked.is_some())
}

/// Park the mic now (no bud worn, or podbridge exiting). False if it must stay
/// shown: moving a muted call would put it on a live microphone. Blocking.
fn park_mic() -> bool {
    if mic_mute::holds_the_mic() {
        return false;
    }
    let mut guard = shared_mic();
    if let Some(mic) = guard.as_mut()
        && mic.parked.is_none()
    {
        let parked = park();
        save_parked(Some(&parked));
        mic.parked = Some(parked);
    }
    true
}

/// Unpark the mic (a bud is back in). Blocking.
fn unpark_mic() {
    let mut guard = shared_mic();
    if let Some(mic) = guard.as_mut()
        && let Some(parked) = mic.parked.take()
    {
        unpark(&parked);
        save_parked(None);
    }
}

/// The AirPods connection ended: park the mic after DISCONNECT_PARK_DELAY
/// unless a new connection took it meanwhile, then remove it if nothing records.
fn release_mic(runtime: &tokio::runtime::Handle) {
    let generation = {
        let mut guard = shared_mic();
        let Some(mic) = guard.as_mut() else { return };
        mic.users = mic.users.saturating_sub(1);
        mic.generation
    };
    runtime.spawn(async move {
        tokio::time::sleep(DISCONNECT_PARK_DELAY).await;
        // A squeeze mute outlasts the connection: with no monitor left, end it
        // here once the recording does, before parking and removing.
        let superseded = move || {
            shared_mic()
                .as_ref()
                .is_none_or(|m| m.users > 0 || m.generation != generation)
        };
        let mut idle = mic_mute::Idle::default();
        let mut missing = 0;
        while blocking(move || !superseded() && mic_mute::pending())
            .await
            .unwrap_or(false)
        {
            // Twice, like the monitor: a restarted sound server lost the mic.
            match blocking(|| output::source_consumer(output::SOURCE_NAME)).await {
                Some(output::SourceUse::Missing) => missing += 1,
                _ => missing = 0,
            }
            if missing >= 2 {
                blocking(move || drop_lost_mic(generation)).await;
                return;
            }
            idle = blocking(move || {
                mic_mute::end_when_idle(&mut idle);
                idle
            })
            .await
            .unwrap_or_default();
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let _ = tokio::task::spawn_blocking(move || {
            let mut guard = shared_mic();
            let Some(mic) = guard.as_mut() else { return };
            if mic.users > 0 || mic.generation != generation {
                return;
            }
            // Often parked already: no bud was worn before the disconnect.
            // A muted mic stays as it is (see park_mic).
            if mic.parked.is_none() && !mic_mute::holds_the_mic() {
                info!("[hires] AirPods still gone; switching apps to another microphone");
                let parked = park();
                save_parked(Some(&parked));
                mic.parked = Some(parked);
            }
            // shortcut: checked once. If an app still records now, the mic
            // stays listed until the AirPods connect and leave again. Upgrade:
            // retry when the recorder watcher sees the last recorder leave.
            if !output::any_recorder() {
                info!(
                    "[hires] nothing records; removing '{}' until the AirPods are back",
                    output::SOURCE_NAME
                );
                // The parked state stays on disk: the next connection creates
                // the mic again and undoes the default-mic switch.
                drop(guard.take());
                mic_mute::forget();
            }
        })
        .await;
    });
}

/// The sound server that held the virtual mic is gone, and the squeeze mute
/// with it. The module index belongs to that server, so it is forgotten rather
/// than unloaded (see recreate_mic). Blocking.
fn drop_lost_mic(generation: u64) {
    let mut guard = shared_mic();
    if guard
        .as_ref()
        .is_some_and(|m| m.users == 0 && m.generation == generation)
    {
        if let Some(mic) = guard.take() {
            std::mem::forget(mic.vmic);
        }
        mic_mute::forget();
    }
}

/// Whether the monitor keeps running: while the feature is on, while an app
/// records, and until a squeeze mute has been ended.
fn monitor_stays(enabled: bool, recording: bool, mute_pending: bool) -> bool {
    enabled || recording || mute_pending
}

/// Whether a leftover mic is removed: only when nothing records, since a mic
/// disappearing under a recording app can abort it (see the section comment).
fn remove_leftover(leftover: bool, recording: bool) -> bool {
    leftover && !recording
}

/// podbridge started without AirPods and an earlier run left the mic loaded:
/// remove it like release_mic does after a disconnect. Blocking.
pub fn remove_leftover_mic() {
    if !remove_leftover(output::leftover_module(), output::any_recorder()) {
        return;
    }
    // An earlier run may have parked it already; keep that state so the next
    // connection restores the default mic.
    if load_parked().is_none() {
        save_parked(Some(&park()));
    }
    // park() takes a moment; an app may have started recording meanwhile.
    if output::any_recorder() {
        return;
    }
    info!(
        "[hires] no AirPods and nothing records; removing leftover '{}'",
        output::SOURCE_NAME
    );
    output::unload_leftover();
    mic_mute::forget();
}

/// The hi-res mic was turned off: unload the virtual mic. Blocking.
fn remove_mic() {
    let mic = shared_mic().take();
    if let Some(mic) = mic {
        info!(
            "[hires] hi-res mic turned off; removing '{}'",
            output::SOURCE_NAME
        );
        drop(mic.vmic);
        mic_mute::forget();
    }
    save_parked(None);
}

/// pipewire-pulse restarted and dropped the virtual mic: create a new one. The
/// old module index belongs to the dead server (and may now name someone else's
/// module), so it's forgotten rather than unloaded. Blocking.
fn recreate_mic(description: &str) -> bool {
    let mut guard = shared_mic();
    let Some(fresh) = VirtualMic::open(ELD_SAMPLE_RATE, ELD_CHANNELS as u8, description) else {
        return false;
    };
    // If it was parked it still is: the default mic is the one parking chose,
    // and that is to be undone once a bud is worn.
    match guard.as_mut() {
        Some(mic) => {
            std::mem::forget(std::mem::replace(&mut mic.vmic, fresh));
            mic.parked = carried_over(mic.parked.take());
            save_parked(mic.parked.as_ref());
        }
        None => {
            let parked = carried_over(load_parked());
            save_parked(parked.as_ref());
            *guard = Some(SharedMic {
                vmic: fresh,
                parked,
                users: 1,
                generation: 1,
            });
        }
    }
    true
}

/// podbridge is exiting: keep the virtual mic loaded (the next run adopts it)
/// and send apps to another mic, since nothing will feed this one, unless our
/// mute still holds it (see park_mic).
pub fn park_on_exit() {
    park_mic();
    output::restore_wireplumber_mic();
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

/// Turn conversation awareness back on after a capture. If the AirPods are
/// already gone (disconnected mid-capture), remember it so the next connection
/// restores it; the setting lives on the AirPods and would otherwise stay off.
async fn restore_conversation_detection(aacp: &AACPManager, enabled: bool) {
    if aacp.take_convo_user_changed() {
        info!("[hires] conversation awareness was changed by the user; not restoring it");
        return;
    }
    if !aacp.set_conversation_detection(enabled).await && enabled {
        info!("[hires] AirPods gone; will restore conversation awareness on next connect");
        crate::utils::AppSettings::update(|s| s.convo_restore_pending = true);
    }
}

async fn start_capture(aacp: &AACPManager, addr: &str, status: &MicStatus) -> Option<Capture> {
    let decoder = EldDecoder::new()?;
    let output = Output::open(ELD_SAMPLE_RATE, ELD_CHANNELS as u8)?;

    let rx = aacp.take_audio_channel().await;
    // Start the stall grace period now; the device should deliver SDUs (which
    // refresh this) well within STALL_TIMEOUT.
    status.mark_start();
    let Some(decode_thread) = spawn_decode_thread(rx, decoder, output, status.clone()) else {
        aacp.clear_audio_channel().await;
        return None;
    };

    if let Err(e) = aacp.send_start_audio().await {
        error!("failed to send 0x58 START: {}", e);
        aacp.clear_audio_channel().await;
        return None;
    }
    info!("[aacp] microphone stream started");

    // No A2DP reset after START: playback keeps working in both buds when the
    // mic stream starts (tested with AirPods Pro, model A3063). Only STOP needs
    // one.
    let _ = addr;

    Some(Capture {
        decode_thread: Some(decode_thread),
    })
}

// Reset the A2DP transport shortly after a 0x58 STOP: afterwards the AirPods
// play from one earbud only until the transport is reset (see
// output::reset_a2dp).
//
// Coalesced: requests close together run one reset, after the last of them.
fn schedule_a2dp_reset(addr: &str) {
    let now = Instant::now();
    {
        let mut pending = RESET_AT.lock().unwrap_or_else(|e| e.into_inner());
        let already_pending = pending.is_some();
        // (deadline, time of the latest START/STOP that asked for it)
        *pending = Some((now + A2DP_RESET_DELAY, now));
        if already_pending {
            debug!("[hires] A2DP reset already pending; pushed back");
            return;
        }
    }
    let reset_addr = addr.to_string();
    tokio::spawn(async move {
        let requested_at = loop {
            let pending = *RESET_AT.lock().unwrap_or_else(|e| e.into_inner());
            let Some((deadline, requested_at)) = pending else {
                return;
            };
            let now = Instant::now();
            if now >= deadline {
                *RESET_AT.lock().unwrap_or_else(|e| e.into_inner()) = None;
                break requested_at;
            }
            tokio::time::sleep(deadline - now).await;
        };
        // The card was switched back on after the request (a bud went back in):
        // that already created a fresh transport, so no reset is needed.
        if crate::audio::card::activated_since(requested_at) {
            info!("[hires] A2DP was just re-activated; no reset needed");
            return;
        }
        let _ = tokio::task::spawn_blocking(move || output::reset_a2dp(&reset_addr)).await;
    });
}

/// Pending A2DP reset: (when to run it, when it was last requested).
static RESET_AT: Mutex<Option<(Instant, Instant)>> = Mutex::new(None);

async fn stop_capture(mut capture: Capture, aacp: &AACPManager) {
    if let Err(e) = aacp.send_stop_audio().await {
        warn!("failed to send 0x58 STOP: {}", e);
    }
    aacp.clear_audio_channel().await;

    if let Some(handle) = capture.decode_thread.take() {
        let _ = tokio::task::spawn_blocking(move || handle.join()).await;
    }
    // The A2DP reset after a STOP is scheduled by the caller (monitor_loop): a
    // stall restart (STOP immediately followed by START) doesn't need one.
}

fn spawn_decode_thread(
    mut rx: mpsc::Receiver<Vec<u8>>,
    mut decoder: EldDecoder,
    mut output: Output,
    status: MicStatus,
) -> Option<JoinHandle<()>> {
    let spawned = std::thread::Builder::new()
        .name("hires-decode".into())
        .spawn(move || {
            let mut frames: u64 = 0;
            let mut errors: u64 = 0;
            let mut env: f32 = 0.0;
            let mut pcm: Vec<i16> = Vec::with_capacity(4096);

            while let Some(sdu) = rx.blocking_recv() {
                pcm.clear();
                aacp_audio::demux_type58(&sdu, |au| match decoder.decode(au, &mut pcm) {
                    Some(_) => frames += 1,
                    None => {
                        // insert a silent frame
                        errors += 1;
                        pcm.resize(pcm.len() + ELD_FRAME_SAMPLES * ELD_CHANNELS as usize, 0);
                    }
                });

                let peak = if pcm.is_empty() {
                    0.0
                } else {
                    match output.write(&pcm) {
                        Ok(peak) => peak,
                        Err(()) => {
                            warn!("hi-res output broke; stopping decode loop");
                            break;
                        }
                    }
                };

                env = if peak >= env {
                    peak
                } else {
                    env * LEVEL_RELEASE
                };
                status.set_level(env);

                if frames > 0 && frames.is_multiple_of(400) {
                    let secs = frames as f64 * ELD_FRAME_SAMPLES as f64 / ELD_SAMPLE_RATE as f64;
                    info!(
                        "[audio] {} frames ({:.0}s), {} errors, level {:.2}",
                        frames, secs, errors, env
                    );
                }
            }
            status.set_level(0.0);
            info!(
                "[audio] hi-res decode loop ended ({} frames, {} errors)",
                frames, errors
            );
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(e) => {
            error!("failed to spawn hires-decode thread: {}", e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CaptureAction, Parked, capture_action, carried_over, monitor_stays, remove_leftover,
    };
    use crate::audio::output::Recorder;

    #[test]
    fn the_monitor_stays_while_it_has_something_to_do() {
        assert!(monitor_stays(true, false, false));
        assert!(monitor_stays(false, true, false));
        // Switched off, between two streams of a muted recording.
        assert!(monitor_stays(false, false, true));
        assert!(!monitor_stays(false, false, false));
    }

    #[test]
    fn leftover_mic_is_removed_only_when_nothing_records() {
        assert!(remove_leftover(true, false));
        assert!(!remove_leftover(true, true));
        assert!(!remove_leftover(false, false));
    }

    #[test]
    fn parked_state_round_trips() {
        let parked = Parked {
            default_set: Some("alsa_input.usb-mic.mono-fallback".into()),
            moved: vec![
                Recorder {
                    index: 117144,
                    app: Some("WEBRTC VoiceEngine".into()),
                },
                Recorder {
                    index: 7,
                    app: None,
                },
            ],
        };
        assert_eq!(Parked::parse(&parked.serialize()), parked);
        assert_eq!(Parked::parse(""), Parked::default());
        assert_eq!(Parked::parse("garbage\nmoved\tx\tapp\n"), Parked::default());
    }

    #[test]
    fn only_the_default_switch_survives_a_new_mic() {
        let parked = Parked {
            default_set: Some("alsa_input.usb-mic.mono-fallback".into()),
            moved: vec![Recorder {
                index: 7,
                app: Some("app".into()),
            }],
        };
        let kept = carried_over(Some(parked)).expect("default switch is kept");
        assert_eq!(
            kept.default_set.as_deref(),
            Some("alsa_input.usb-mic.mono-fallback")
        );
        assert!(kept.moved.is_empty());
        // Nothing to undo: not parked any more.
        let moved_only = Parked {
            default_set: None,
            moved: vec![Recorder {
                index: 7,
                app: None,
            }],
        };
        assert_eq!(carried_over(Some(moved_only)), None);
        assert_eq!(carried_over(None), None);
    }

    #[test]
    fn starts_only_when_enabled_recording_and_worn() {
        assert_eq!(
            capture_action(true, true, true, false, false),
            CaptureAction::Start
        );
        assert_eq!(
            capture_action(false, true, true, false, false),
            CaptureAction::Idle
        );
        assert_eq!(
            capture_action(true, false, true, false, false),
            CaptureAction::Idle
        );
        // Both buds out: an app holding the mic must not start the 0x58 stream.
        assert_eq!(
            capture_action(true, true, false, false, false),
            CaptureAction::Idle
        );
    }

    #[test]
    fn buds_out_stops_and_never_restarts() {
        assert_eq!(
            capture_action(true, true, false, true, true),
            CaptureAction::Stop
        );
        assert_eq!(
            capture_action(true, true, false, true, false),
            CaptureAction::Stop
        );
    }

    #[test]
    fn app_closing_mic_or_disabling_stops() {
        // Leaving a call: recorder gone, buds still in.
        assert_eq!(
            capture_action(true, false, true, true, false),
            CaptureAction::Stop
        );
        // Feature disabled mid-capture.
        assert_eq!(
            capture_action(false, true, true, true, false),
            CaptureAction::Stop
        );
    }

    #[test]
    fn stall_restarts_only_while_capturing_and_worn() {
        assert_eq!(
            capture_action(true, true, true, true, true),
            CaptureAction::Restart
        );
        assert_eq!(
            capture_action(true, true, true, true, false),
            CaptureAction::Keep
        );
    }
}
