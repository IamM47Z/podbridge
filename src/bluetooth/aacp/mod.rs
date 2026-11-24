use crate::devices::airpods::AirPodsInformation;
use crate::devices::enums::{DeviceData, DeviceInformation, DeviceType};
use bluer::{
    Address, AddressType, Error, Result,
    l2cap::{SeqPacket, Socket, SocketAddr},
};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep};

mod build;
mod parse;
mod protocol;

use build::*;
use parse::{Packet, parse};
pub use protocol::*;

const PSM: u16 = 0x1001;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// L2CAP recv buffer. 0x58 hi-res audio SDUs can exceed 1 KB; SOCK_SEQPACKET
/// silently truncates an undersized buffer, so this must comfortably exceed the
/// largest SDU
const RECV_BUF_LEN: usize = 4096;

/// Bound for the audio SDU forwarding channel. Realtime: if the decoder falls
/// behind we drop SDUs (a brief glitch) rather than back-pressure the L2CAP
/// recv loop, which would stall control traffic too.
const AUDIO_CHANNEL_CAP: usize = 256;

#[derive(Debug, Clone)]
pub enum AACPEvent {
    BatteryInfo(Vec<BatteryInfo>),
    ControlCommand(ControlCommandStatus),
    EarDetection(Vec<EarDetectionStatus>, Vec<EarDetectionStatus>),
    ConversationalAwareness(u8),
    #[expect(
        dead_code,
        reason = "proximity-key exchange: wired by the Apple-parity plan"
    )]
    ProximityKeys(Vec<(u8, Vec<u8>)>),
    AudioSource(
        #[expect(
            dead_code,
            reason = "audio-source owner: wired by the Apple-parity plan"
        )]
        AudioSource,
    ),
    ConnectedDevices(Vec<ConnectedDevice>, Vec<ConnectedDevice>),
    OwnershipToFalseRequest,
    StemPress(StemPressType, StemPressBudType),
}

pub struct AACPManagerState {
    pub sender: Option<mpsc::Sender<Vec<u8>>>,
    pub control_command_status_list: Vec<ControlCommandStatus>,
    pub control_command_subscribers:
        HashMap<ControlCommandIdentifiers, Vec<mpsc::UnboundedSender<Vec<u8>>>>,
    pub owns: bool,
    pub old_connected_devices: Vec<ConnectedDevice>,
    pub connected_devices: Vec<ConnectedDevice>,
    pub audio_source: Option<AudioSource>,
    pub battery_info: Vec<BatteryInfo>,
    pub conversational_awareness_status: u8,
    pub old_ear_detection_status: Vec<EarDetectionStatus>,
    pub ear_detection_status: Vec<EarDetectionStatus>,
    event_tx: Option<mpsc::UnboundedSender<AACPEvent>>,
    pub airpods_mac: Option<Address>,
    /// When set, recv_thread forwards raw 0x58 audio SDUs here (hi-res mic).
    pub audio_tx: Option<mpsc::Sender<Vec<u8>>>,
}

impl AACPManagerState {
    fn new() -> Self {
        AACPManagerState {
            sender: None,
            control_command_status_list: Vec::new(),
            control_command_subscribers: HashMap::new(),
            owns: false,
            old_connected_devices: Vec::new(),
            connected_devices: Vec::new(),
            audio_source: None,
            battery_info: Vec::new(),
            conversational_awareness_status: 0,
            old_ear_detection_status: Vec::new(),
            ear_detection_status: Vec::new(),
            event_tx: None,
            airpods_mac: None,
            audio_tx: None,
        }
    }
}

#[derive(Clone)]
pub struct AACPManager {
    pub state: Arc<Mutex<AACPManagerState>>,
    tasks: Arc<Mutex<JoinSet<()>>>,
    hires_enabled: Arc<AtomicBool>,
    hires_mic: Arc<Mutex<Option<crate::audio::hires_mic::HiResMic>>>,
    /// Wakes the hi-res monitor so it re-polls promptly when the feature is
    /// toggled, instead of waiting out the poll interval.
    hires_wake: Arc<Notify>,
    mic_status: crate::audio::hires_mic::MicStatus,
    /// Handle to the long-lived backend runtime, so the hi-res monitor task
    /// survives even when armed from a throwaway runtime (the UI toggle thread).
    runtime: tokio::runtime::Handle,
    /// true while the L2CAP control channel is up. Per-connection tasks (playback
    /// listener, command/subscriber loops) watch this and exit when it drops, so
    /// they don't pile up across reconnects.
    connected: Arc<watch::Sender<bool>>,
    /// The user changed conversation awareness while a capture had it
    /// overridden; the capture then must not restore its saved value.
    convo_user_changed: Arc<AtomicBool>,
}

impl AACPManager {
    pub fn new() -> Self {
        AACPManager {
            state: Arc::new(Mutex::new(AACPManagerState::new())),
            tasks: Arc::new(Mutex::new(JoinSet::new())),
            hires_enabled: Arc::new(AtomicBool::new(
                crate::utils::AppSettings::load().hires_mic_enabled,
            )),
            hires_mic: Arc::new(Mutex::new(None)),
            hires_wake: Arc::new(Notify::new()),
            mic_status: crate::audio::hires_mic::MicStatus::new(),
            runtime: tokio::runtime::Handle::current(),
            connected: Arc::new(watch::channel(false).0),
            convo_user_changed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_connected(&self) -> bool {
        *self.connected.borrow()
    }

    /// Tear this connection down from outside: used when BlueZ reports the
    /// AirPods connected again while our old L2CAP link hasn't noticed yet that it
    /// died (otherwise two device stacks would run side by side until the old
    /// socket errors out).
    pub async fn close(&self) {
        self.tasks.lock().await.abort_all();
        {
            let mut state = self.state.lock().await;
            state.sender = None;
            state.owns = false;
            state.connected_devices.clear();
            state.ear_detection_status.clear();
            state.old_ear_detection_status.clear();
            state.audio_tx = None;
        }
        self.connected.send_replace(false);
        self.disarm_hires_mic().await;
    }

    /// Conversation awareness changed from the UI.
    pub async fn set_conversation_detection_by_user(&self, enabled: bool) -> bool {
        self.convo_user_changed.store(true, Ordering::Relaxed);
        self.set_conversation_detection(enabled).await
    }

    /// Whether the user changed conversation awareness since the last call (and
    /// clear the flag).
    pub fn take_convo_user_changed(&self) -> bool {
        self.convo_user_changed.swap(false, Ordering::Relaxed)
    }

    /// True if a device other than this computer is currently playing media or
    /// in a call on the AirPods (per their audio-source report).
    pub async fn other_device_using_audio(&self, local_mac: &str) -> bool {
        let state = self.state.lock().await;
        state.audio_source.as_ref().is_some_and(|source| {
            source.r#type != AudioSourceType::None
                && source.mac != "00:00:00:00:00:00"
                && !source.mac.eq_ignore_ascii_case(local_mac)
        })
    }

    /// Resolves once this connection has ended (immediately if it never came up).
    pub async fn disconnected(&self) {
        let mut rx = self.connected.subscribe();
        let _ = rx.wait_for(|up| !*up).await;
    }

    /// True if at least one bud is in an ear. An empty status (not reported yet)
    /// counts as wearing so it never blocks. So does Automatic Ear Detection
    /// off: then all audio plays to the AirPods whether or not they are worn
    /// (Apple Support 108764).
    pub async fn is_worn(&self) -> bool {
        let state = self.state.lock().await;
        let ear_detection_off = state
            .control_command_status_list
            .iter()
            .find(|s| s.identifier == ControlCommandIdentifiers::EarDetectionConfig)
            .is_some_and(|s| !crate::devices::controls::is_on(&s.value));
        ear_detection_off
            || state.ear_detection_status.is_empty()
            || state
                .ear_detection_status
                .contains(&EarDetectionStatus::InEar)
    }

    /// True once the AirPods have reported an ear-detection state on this
    /// connection (is_worn() is only a guess before that).
    pub async fn ear_status_reported(&self) -> bool {
        !self.state.lock().await.ear_detection_status.is_empty()
    }

    pub async fn conversation_detection_enabled(&self) -> bool {
        self.state
            .lock()
            .await
            .control_command_status_list
            .iter()
            .find(|s| s.identifier == ControlCommandIdentifiers::ConversationDetectConfig)
            .is_some_and(|s| s.value.first() == Some(&0x01))
    }

    /// Returns false if the command couldn't be sent (e.g. not connected).
    pub async fn set_conversation_detection(&self, enabled: bool) -> bool {
        let value = if enabled { 0x01 } else { 0x02 };
        info!("[aacp] setting conversation detection to {}", enabled);
        if let Err(e) = self
            .send_control_command(
                ControlCommandIdentifiers::ConversationDetectConfig,
                &[value],
            )
            .await
        {
            warn!("[hires] failed to set conversation detection: {}", e);
            return false;
        }
        // AirPods don't echo this back, so record it ourselves: the status
        // list is the source of truth for conversation_detection_enabled.
        self.record_control_status(ControlCommandIdentifiers::ConversationDetectConfig, value)
            .await;
        true
    }

    /// Record a control value the AirPods do not echo back: store it in the
    /// status list and pass it to the subscribers and the event channel, as a
    /// report would.
    pub async fn record_control_status(&self, identifier: ControlCommandIdentifiers, value: u8) {
        let mut state = self.state.lock().await;
        if let Some(existing) = state
            .control_command_status_list
            .iter_mut()
            .find(|s| s.identifier == identifier)
        {
            existing.value = vec![value];
        } else {
            state
                .control_command_status_list
                .push(ControlCommandStatus {
                    identifier,
                    value: vec![value],
                });
        }
        if let Some(subscribers) = state.control_command_subscribers.get(&identifier) {
            for sub in subscribers {
                let _ = sub.send(vec![value]);
            }
        }
        if let Some(ref tx) = state.event_tx {
            let _ = tx.send(AACPEvent::ControlCommand(ControlCommandStatus {
                identifier,
                value: vec![value],
            }));
        }
    }

    pub fn mic_level(&self) -> f32 {
        self.mic_status.level()
    }

    pub fn mic_active(&self) -> bool {
        self.mic_status.active()
    }

    pub fn mic_app(&self) -> Option<String> {
        self.mic_status.app()
    }

    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }

    pub fn hires_mic_enabled(&self) -> bool {
        self.hires_enabled.load(Ordering::Relaxed)
    }

    pub fn hires_wake(&self) -> Arc<Notify> {
        self.hires_wake.clone()
    }

    pub async fn set_hires_mic_enabled(&self, enabled: bool) {
        self.hires_enabled.store(enabled, Ordering::Relaxed);
        self.hires_wake.notify_one();
        if enabled {
            self.arm_hires_mic().await;
        }
    }

    // Create the virtual device + monitor if the feature is enabled, AirPods are
    // connected, and it is not already armed. Called on enable and on connect.
    pub async fn arm_hires_mic(&self) {
        if !self.hires_enabled.load(Ordering::Relaxed) {
            return;
        }
        let mut guard = self.hires_mic.lock().await;
        if guard.as_ref().is_some_and(|m| m.is_running()) {
            return;
        }
        let addr = self.state.lock().await.airpods_mac.map(|a| a.to_string());
        let Some(addr) = addr else {
            return;
        };
        match crate::audio::hires_mic::HiResMic::start(self, addr, self.mic_status.clone()).await {
            Some(mic) => *guard = Some(mic),
            None => error!("Failed to start hi-res microphone"),
        }
    }

    // Tear down the virtual device + monitor without clearing the enabled flag,
    // so a later reconnect re-arms. Called on disable and on disconnect.
    pub async fn disarm_hires_mic(&self) {
        let mic = self.hires_mic.lock().await.take();
        if let Some(mic) = mic {
            mic.stop().await;
        }
    }

    /// Open the AACP control channel, retrying a few times: right after the
    /// AirPods connect the L2CAP channel can be refused or time out, and giving up
    /// after one attempt would leave the device half-initialised until the next
    /// reconnect.
    /// Returns false if the channel could not be opened.
    pub async fn connect(&mut self, addr: Address) -> bool {
        const BACKOFF_SECS: [u64; 4] = [1, 2, 4, 8];
        for attempt in 0..=BACKOFF_SECS.len() {
            if self.try_connect(addr).await {
                return true;
            }
            if let Some(backoff) = BACKOFF_SECS.get(attempt) {
                warn!(
                    "AACP connect attempt {} failed; retrying in {}s",
                    attempt + 1,
                    backoff
                );
                sleep(Duration::from_secs(*backoff)).await;
            }
        }
        error!("Could not open the AACP channel to {}", addr);
        false
    }

    async fn try_connect(&mut self, addr: Address) -> bool {
        info!("AACPManager connecting to {} on PSM {:#06X}...", addr, PSM);
        let target_sa = SocketAddr::new(addr, AddressType::BrEdr, PSM);

        {
            let mut state = self.state.lock().await;
            state.airpods_mac = Some(addr);
        }

        let socket = match Socket::new_seq_packet() {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to create L2CAP socket: {}", e);
                return false;
            }
        };

        let seq_packet =
            match tokio::time::timeout(CONNECT_TIMEOUT, socket.connect(target_sa)).await {
                Ok(Ok(s)) => Arc::new(s),
                Ok(Err(e)) => {
                    error!("L2CAP connect failed: {}", e);
                    return false;
                }
                Err(_) => {
                    error!("L2CAP connect timed out");
                    return false;
                }
            };

        // Wait for connection to be fully established
        let start = Instant::now();
        loop {
            match seq_packet.peer_addr() {
                Ok(peer) if peer.cid != 0 => break,
                Ok(_) => { /* still waiting */ }
                Err(e) => {
                    if e.raw_os_error() == Some(107) {
                        // ENOTCONN
                        error!("Peer has disconnected during connection setup.");
                        return false;
                    }
                    error!("Error getting peer address: {}", e);
                }
            }
            if start.elapsed() >= CONNECT_TIMEOUT {
                error!("Timed out waiting for L2CAP connection to be fully established.");
                return false;
            }
            sleep(POLL_INTERVAL).await;
        }

        info!("L2CAP connection established with {}", addr);

        let (tx, rx) = mpsc::channel(128);

        let manager_clone = self.clone();
        {
            let mut state = self.state.lock().await;
            state.sender = Some(tx);
        }

        self.connected.send_replace(true);
        let mut tasks = self.tasks.lock().await;
        tasks.spawn(recv_thread(manager_clone, seq_packet.clone()));
        tasks.spawn(send_thread(rx, seq_packet));
        true
    }

    async fn send_packet(&self, data: &[u8]) -> Result<()> {
        // The sender is cloned and the lock released before awaiting. Holding
        // the state mutex across a full channel blocks every other task,
        // including the receive loop, and the manager stops responding
        // entirely. The timeout turns a stuck mutex into a logged error
        // instead of a hang with no diagnosis.
        let sender = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.state.lock(),
        )
        .await
        {
            Ok(state) => state.sender.clone(),
            Err(_) => {
                error!("send_packet: state mutex held for over 2s, giving up");
                return Err(Error::from(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "state mutex busy",
                )));
            }
        };
        if let Some(sender) = sender {
            sender.send(data.to_vec()).await.map_err(|e| {
                error!("Failed to send packet to channel: {}", e);
                Error::from(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "L2CAP send channel closed",
                ))
            })
        } else {
            error!("Cannot send packet, sender is not available.");
            Err(Error::from(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "L2CAP stream not connected",
            )))
        }
    }

    async fn send_data_packet(&self, data: &[u8]) -> Result<()> {
        let packet = [HEADER_BYTES.as_slice(), data].concat();
        self.send_packet(&packet).await
    }

    pub async fn set_event_channel(&self, tx: mpsc::UnboundedSender<AACPEvent>) {
        let mut state = self.state.lock().await;
        state.event_tx = Some(tx);
    }

    pub async fn subscribe_to_control_command(
        &self,
        identifier: ControlCommandIdentifiers,
        tx: mpsc::UnboundedSender<Vec<u8>>,
    ) {
        let mut state = self.state.lock().await;
        state
            .control_command_subscribers
            .entry(identifier)
            .or_default()
            .push(tx);
        // send initial value if available
        if let Some(status) = state
            .control_command_status_list
            .iter()
            .find(|s| s.identifier == identifier)
        {
            // Both unwraps hold: `tx` was pushed under this same lock just above.
            let _ = state
                .control_command_subscribers
                .get(&identifier)
                .unwrap()
                .last()
                .unwrap()
                .send(status.value.clone());
        }
    }

    pub async fn receive_packet(&self, packet: &[u8]) {
        match parse(packet) {
            Ok(p) => self.handle(p).await,
            Err(e) => e.log(packet),
        }
    }

    /// Act on one decoded packet: state updates, events, saves and wakeups.
    async fn handle(&self, packet: Packet) {
        match packet {
            Packet::BatteryInfo(batteries) => {
                let mut state = self.state.lock().await;
                state.battery_info = batteries.clone();
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::BatteryInfo(batteries));
                }
                info!("Received Battery Info: {:?}", state.battery_info);
            }
            Packet::ControlCommand(status) => {
                let identifier = status.identifier;
                let value = status.value.clone();
                let mut state = self.state.lock().await;
                if let Some(existing) = state
                    .control_command_status_list
                    .iter_mut()
                    .find(|s| s.identifier == identifier)
                {
                    existing.value = value.clone();
                } else {
                    state.control_command_status_list.push(status.clone());
                }
                if identifier == ControlCommandIdentifiers::OwnsConnection {
                    // The value keeps the first byte unless all four are zero.
                    state.owns = value.first().is_some_and(|&b| b != 0);
                }
                if let Some(subscribers) = state.control_command_subscribers.get(&identifier) {
                    for sub in subscribers {
                        let _ = sub.send(value.clone());
                    }
                }
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::ControlCommand(status));
                }
                info!(
                    "Received Control Command: {:?}, value: {}",
                    identifier,
                    hex::encode(&value)
                );
            }
            Packet::EarDetection(statuses) => {
                let mut state = self.state.lock().await;
                state.old_ear_detection_status = state.ear_detection_status.clone();
                state.ear_detection_status = statuses.clone();

                if let Some(ref tx) = state.event_tx {
                    debug!(
                        "Sending Ear Detection event: old: {:?}, new: {:?}",
                        state.old_ear_detection_status, statuses
                    );
                    let _ = tx.send(AACPEvent::EarDetection(
                        state.old_ear_detection_status.clone(),
                        statuses,
                    ));
                }
                info!(
                    "Received Ear Detection Status: {:?}",
                    state.ear_detection_status
                );
                drop(state);
                // The hi-res monitor ties capture to "a bud is in an ear"; re-check now.
                self.hires_wake.notify_one();
            }
            Packet::ConversationAwareness(status) => {
                let mut state = self.state.lock().await;
                state.conversational_awareness_status = status;
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::ConversationalAwareness(status));
                }
                info!("Received Conversation Awareness: {}", status);
            }
            Packet::Information(strings) => {
                let field = |i: usize| strings.get(i).cloned().unwrap_or_default();
                let mut info = AirPodsInformation {
                    name: field(0),
                    model_number: field(1),
                    manufacturer: field(2),
                    serial_number: field(3),
                    version1: field(4),
                    version2: field(5),
                    hardware_revision: field(6),
                    updater_identifier: field(7),
                    left_serial_number: field(8),
                    right_serial_number: field(9),
                    version3: field(10),
                    le_keys: AirPodsLEKeys {
                        irk: "".to_string(),
                        enc_key: "".to_string(),
                    },
                };
                // Don't hold the AACP state lock across disk I/O: every incoming
                // packet needs it.
                let mac = self.state.lock().await.airpods_mac;
                info!("Received Information: {:?}", info);
                if let Some(mac) = mac {
                    update_devices_off_runtime(move |devices| {
                        let Some(device_data) = devices.get_mut(&mac.to_string()) else {
                            return false;
                        };
                        // The Information packet carries no LE keys; keep the ones the
                        // Proximity Keys response stored, or the LE monitor can't match.
                        if let Some(DeviceInformation::AirPods(old)) = &device_data.information {
                            info.le_keys = old.le_keys.clone();
                        }
                        device_data.name = info.name.clone();
                        device_data.information = Some(DeviceInformation::AirPods(Box::new(info)));
                        true
                    })
                    .await;
                }
            }
            Packet::ProximityKeys(keys) => {
                // Key material (IRK, encryption key) is secret: it identifies the
                // device and decrypts its BLE broadcasts, so only types and sizes
                // are logged.
                info!(
                    "Received Proximity Keys Response: {:?}",
                    keys.iter()
                        .map(|k| format!("type {}: {} bytes", k.key_type, k.data.len()))
                        .collect::<Vec<_>>()
                );
                let Some(mac) = self.state.lock().await.airpods_mac else {
                    return;
                };
                update_devices_off_runtime(move |devices| {
                    store_proximity_keys(devices, &mac.to_string(), &keys);
                    true
                })
                .await;
            }
            Packet::StemPress(press, bud) => {
                info!("Received Stem Press: {:?} on {:?}", press, bud);
                let state = self.state.lock().await;
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::StemPress(press, bud));
                }
            }
            Packet::AudioSource(audio_source) => {
                let mut state = self.state.lock().await;
                state.audio_source = Some(audio_source.clone());
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::AudioSource(audio_source));
                }
                info!("Received Audio Source: {:?}", state.audio_source);
            }
            Packet::ConnectedDevices(devices) => {
                let mut state = self.state.lock().await;
                state.old_connected_devices = state.connected_devices.clone();
                state.connected_devices = devices.clone();
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::ConnectedDevices(
                        state.old_connected_devices.clone(),
                        devices,
                    ));
                }
                info!("Received Connected Devices: {:?}", state.connected_devices);
            }
            Packet::SmartRoutingResponse(packet_string) => {
                info!("Received Smart Routing Response: {}", packet_string);
                if packet_string.contains("SetOwnershipToFalse") {
                    info!("Received OwnershipToFalse request");
                    if let Some(ref tx) = self.state.lock().await.event_tx {
                        let _ = tx.send(AACPEvent::OwnershipToFalseRequest);
                    }
                }
            }
            Packet::EqData => debug!("Received EQ Data"),
            Packet::Unknown { opcode } => {
                debug!("Received unknown packet with opcode {:#04x}", opcode)
            }
        }
    }

    /// Create the channel that recv_thread forwards 0x58 audio SDUs to, and
    /// return the receiving half for the hi-res decode task. Replaces any
    /// previously installed channel.
    pub async fn take_audio_channel(&self) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel(AUDIO_CHANNEL_CAP);
        let mut state = self.state.lock().await;
        state.audio_tx = Some(tx);
        rx
    }

    /// Stop forwarding audio SDUs
    pub async fn clear_audio_channel(&self) {
        let mut state = self.state.lock().await;
        state.audio_tx = None;
    }

    /// Start the proprietary hi-res microphone stream (0x58 START).
    pub async fn send_start_audio(&self) -> Result<()> {
        self.send_packet(&AACP_START_AUDIO).await
    }

    /// Stop the proprietary hi-res microphone stream (0x58 STOP).
    pub async fn send_stop_audio(&self) -> Result<()> {
        self.send_packet(&AACP_STOP_AUDIO).await
    }

    pub async fn send_notification_request(&self) -> Result<()> {
        let opcode = [opcodes::REQUEST_NOTIFICATIONS, 0x00];
        let data = [0xFF, 0xFF, 0xFF, 0xFF];
        let packet = [opcode.as_slice(), data.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_set_feature_flags_packet(&self) -> Result<()> {
        let opcode = [opcodes::SET_FEATURE_FLAGS, 0x00];
        // let data = [0xD7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let data = [0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]; // adaptive volume is actually useful, seeing if it works
        let packet = [opcode.as_slice(), data.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_handshake(&self) -> Result<()> {
        let packet = [
            0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        self.send_packet(&packet).await
    }

    pub async fn send_proximity_keys_request(
        &self,
        key_types: Vec<ProximityKeyType>,
    ) -> Result<()> {
        self.send_data_packet(&build_proximity_keys_request(&key_types))
            .await
    }

    pub async fn send_rename_packet(&self, name: &str) -> Result<()> {
        self.send_data_packet(&build_rename(name)).await
    }

    pub async fn send_control_command(
        &self,
        identifier: ControlCommandIdentifiers,
        value: &[u8],
    ) -> Result<()> {
        self.send_data_packet(&build_control_command(identifier, value))
            .await
    }

    pub async fn send_media_information_new_device(
        &self,
        self_mac_address: &str,
        target_mac_address: &str,
    ) -> Result<()> {
        let packet = build_media_information_new_device(self_mac_address, target_mac_address)
            .map_err(invalid_mac)?;
        self.send_data_packet(&packet).await
    }

    pub async fn send_hijack_request(&self, target_mac_address: &str) -> Result<()> {
        let packet = build_hijack_request(target_mac_address).map_err(invalid_mac)?;
        self.send_data_packet(&packet).await
    }

    pub async fn send_media_information(
        &self,
        self_mac_address: &str,
        target_mac_address: &str,
        streaming_state: bool,
    ) -> Result<()> {
        let packet = build_media_information(self_mac_address, target_mac_address, streaming_state)
            .map_err(invalid_mac)?;
        self.send_data_packet(&packet).await
    }

    pub async fn send_smart_routing_show_ui(&self, target_mac_address: &str) -> Result<()> {
        let packet = build_smart_routing_show_ui(target_mac_address).map_err(invalid_mac)?;
        self.send_data_packet(&packet).await
    }

    #[expect(
        dead_code,
        reason = "hand audio back to the other device: wired by the Apple-parity plan"
    )]
    pub async fn send_hijack_reversed(&self, target_mac_address: &str) -> Result<()> {
        let packet = build_hijack_reversed(target_mac_address).map_err(invalid_mac)?;
        self.send_data_packet(&packet).await
    }

    pub async fn send_add_tipi_device(
        &self,
        self_mac_address: &str,
        target_mac_address: &str,
    ) -> Result<()> {
        let packet =
            build_add_tipi_device(self_mac_address, target_mac_address).map_err(invalid_mac)?;
        self.send_data_packet(&packet).await
    }

    pub async fn send_some_packet(&self) -> Result<()> {
        self.send_data_packet(&[0x29, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
            .await
    }
}

/// A malformed MAC used to panic the builder; now the send fails and is logged.
fn invalid_mac(e: InvalidMac) -> Error {
    error!("Cannot build smart routing packet: {}", e);
    Error::from(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        e.to_string(),
    ))
}

/// Persist the devices file (atomically, off the async runtime).
/// update_devices on a blocking thread: it does disk I/O.
async fn update_devices_off_runtime(
    change: impl FnOnce(&mut HashMap<String, DeviceData>) -> bool + Send + 'static,
) {
    if let Err(e) = tokio::task::spawn_blocking(move || crate::utils::update_devices(change)).await
    {
        error!("Failed to save devices: {}", e);
    }
}

/// Store the IRK and encryption key in the device's entry.
fn store_proximity_keys(
    devices: &mut HashMap<String, DeviceData>,
    mac_str: &str,
    keys: &[ProximityKey],
) {
    for key in keys {
        let Some(kt) = ProximityKeyType::from_u8(key.key_type) else {
            continue;
        };
        let device_data = devices.entry(mac_str.to_string()).or_insert(DeviceData {
            name: mac_str.to_string(),
            type_: DeviceType::AirPods,
            information: None,
        });
        // The proximity key response is sent before the INFORMATION packet, so
        // there is often no information entry yet. Create a stub instead of
        // dropping the keys: without the IRK, RPA matching in the LE monitor can
        // never succeed and auto-connect never fires.
        if !matches!(device_data.information, Some(DeviceInformation::AirPods(_))) {
            device_data.information =
                Some(DeviceInformation::AirPods(Box::new(AirPodsInformation {
                    name: device_data.name.clone(),
                    model_number: String::new(),
                    manufacturer: String::new(),
                    serial_number: String::new(),
                    version1: String::new(),
                    version2: String::new(),
                    hardware_revision: String::new(),
                    updater_identifier: String::new(),
                    left_serial_number: String::new(),
                    right_serial_number: String::new(),
                    version3: String::new(),
                    le_keys: AirPodsLEKeys {
                        irk: String::new(),
                        enc_key: String::new(),
                    },
                })));
        }
        if let Some(DeviceInformation::AirPods(info)) = device_data.information.as_mut() {
            match kt {
                ProximityKeyType::Irk => info.le_keys.irk = hex::encode(&key.data),
                ProximityKeyType::EncKey => info.le_keys.enc_key = hex::encode(&key.data),
            }
        }
    }
}

async fn recv_thread(manager: AACPManager, sp: Arc<SeqPacket>) {
    let mut buf = vec![0u8; RECV_BUF_LEN];
    manager.arm_hires_mic().await;
    loop {
        match sp.recv(&mut buf).await {
            Ok(0) => {
                info!("Remote closed the connection.");
                break;
            }
            Ok(n) => {
                let data = &buf[..n];

                // Forward audio SDUs to the audio thread, leaving control SDUs for the control thread.
                if crate::bluetooth::aacp_audio::is_audio(data) {
                    // Device-level liveness for the hi-res stall watchdog.
                    manager.mic_status.mark_sdu();
                    let audio_tx = {
                        let state = manager.state.lock().await;
                        state.audio_tx.clone()
                    };
                    if let Some(tx) = audio_tx {
                        let _ = tx.try_send(data.to_vec());
                    }
                    continue;
                }

                // The proximity keys response carries the IRK and encryption key.
                if data.get(4) == Some(&opcodes::PROXIMITY_KEYS_RSP) {
                    debug!("Received {} bytes: <proximity keys redacted>", n);
                } else {
                    debug!("Received {} bytes: {}", n, hex::encode(data));
                }
                manager.receive_packet(data).await;
            }
            Err(e) => {
                debug!("Read error: {}", e);
                info!(
                    "We have probably disconnected, clearing state variables (owns=false, connected_devices=empty, control_command_status_list=empty)."
                );
                let mut state = manager.state.lock().await;
                state.owns = false;
                state.connected_devices.clear();
                state.control_command_status_list.clear();
                break;
            }
        }
    }
    {
        let mut state = manager.state.lock().await;
        state.sender = None;
        // Stale "InEar" would otherwise drive takeover / mic decisions later.
        state.ear_detection_status.clear();
        state.old_ear_detection_status.clear();
    }
    manager.connected.send_replace(false);
    manager.disarm_hires_mic().await;
}

async fn send_thread(mut rx: mpsc::Receiver<Vec<u8>>, sp: Arc<SeqPacket>) {
    while let Some(data) = rx.recv().await {
        let mut attempts = 0;
        loop {
            match sp.send(&data).await {
                Ok(_) => {
                    debug!("Sent {} bytes: {}", data.len(), hex::encode(&data));
                    break;
                }
                Err(e) if e.raw_os_error() == Some(107) && attempts < 10 => {
                    attempts += 1;
                    sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    error!("Failed to send data: {}", e);
                    return;
                }
            }
        }
    }
    info!("Send thread finished.");
}

#[cfg(test)]
mod tests {
    use super::{AACPManager, AirPodsLEKeys, HEADER_BYTES, opcodes};

    /// A connected-devices packet that announces more entries than it carries
    /// must be rejected, not read past its end.
    #[tokio::test]
    async fn short_connected_devices_packet_is_rejected() {
        let manager = AACPManager::new();
        for extra in 0..2 {
            // opcode, 0x00, 0x01, 0x00, count = 1, then 6 + `extra` bytes: two or
            // one short of the 8 the entry needs (it starts at offset 5).
            let mut packet = HEADER_BYTES.to_vec();
            packet.extend_from_slice(&[opcodes::CONNECTED_DEVICES, 0x00, 0x01, 0x00, 0x01]);
            packet.extend(std::iter::repeat_n(0xAA, 6 + extra));
            manager.receive_packet(&packet).await;
            assert!(manager.state.lock().await.connected_devices.is_empty());
        }
        // A complete one is still accepted.
        let mut packet = HEADER_BYTES.to_vec();
        packet.extend_from_slice(&[opcodes::CONNECTED_DEVICES, 0x00, 0x01, 0x00, 0x01]);
        packet.extend(std::iter::repeat_n(0xAA, 8));
        manager.receive_packet(&packet).await;
        assert_eq!(manager.state.lock().await.connected_devices.len(), 1);
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        [HEADER_BYTES.as_slice(), body].concat()
    }

    async fn manager_with_events() -> (
        AACPManager,
        tokio::sync::mpsc::UnboundedReceiver<super::AACPEvent>,
    ) {
        let manager = AACPManager::new();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_channel(tx).await;
        (manager, rx)
    }

    #[tokio::test]
    async fn recorded_status_reaches_subscribers_like_a_report() {
        use super::*;
        let (manager, mut rx) = manager_with_events().await;
        let (sub_tx, mut sub_rx) = tokio::sync::mpsc::unbounded_channel();
        manager
            .subscribe_to_control_command(ControlCommandIdentifiers::EarDetectionConfig, sub_tx)
            .await;
        manager
            .record_control_status(ControlCommandIdentifiers::EarDetectionConfig, 0x02)
            .await;
        assert_eq!(sub_rx.try_recv().unwrap(), vec![0x02]);
        assert!(matches!(
            rx.try_recv(),
            Ok(AACPEvent::ControlCommand(ControlCommandStatus {
                identifier: ControlCommandIdentifiers::EarDetectionConfig,
                value,
            })) if value == vec![0x02]
        ));
    }

    /// Apple Support 108764: with Automatic Ear Detection off, all audio plays
    /// to the AirPods whether or not they are worn.
    #[tokio::test]
    async fn ear_detection_off_counts_as_worn() {
        use super::*;
        let manager = AACPManager::new();
        // Both buds out of ear.
        manager
            .receive_packet(&frame(&[0x06, 0x00, 0x01, 0x01]))
            .await;
        assert!(!manager.is_worn().await);
        manager
            .record_control_status(ControlCommandIdentifiers::EarDetectionConfig, 0x02)
            .await;
        assert!(manager.is_worn().await);
        manager
            .record_control_status(ControlCommandIdentifiers::EarDetectionConfig, 0x01)
            .await;
        assert!(!manager.is_worn().await);
    }

    /// Characterization: the state and event side effects of each handled
    /// opcode, pinned before the parser was split out of receive_packet.
    #[tokio::test]
    async fn receive_packet_side_effects() {
        use super::*;
        let (manager, mut rx) = manager_with_events().await;

        // Battery, the AAP Definitions example.
        manager
            .receive_packet(&frame(&[
                0x04, 0x00, 0x03, 0x02, 0x01, 0x64, 0x02, 0x01, 0x04, 0x01, 0x63, 0x01, 0x01, 0x08,
                0x01, 0x11, 0x02, 0x01,
            ]))
            .await;
        let expected = vec![
            BatteryInfo {
                component: BatteryComponent::Right,
                level: 100,
                status: BatteryStatus::NotCharging,
            },
            BatteryInfo {
                component: BatteryComponent::Left,
                level: 99,
                status: BatteryStatus::Charging,
            },
            BatteryInfo {
                component: BatteryComponent::Case,
                level: 17,
                status: BatteryStatus::NotCharging,
            },
        ];
        assert_eq!(manager.state.lock().await.battery_info, expected);
        assert!(matches!(rx.try_recv(), Ok(AACPEvent::BatteryInfo(b)) if b == expected));

        // Control command: status list, subscriber, event, `owns`.
        let (sub_tx, mut sub_rx) = tokio::sync::mpsc::unbounded_channel();
        manager
            .subscribe_to_control_command(ControlCommandIdentifiers::ListeningMode, sub_tx)
            .await;
        manager
            .receive_packet(&frame(&[0x09, 0x00, 0x0D, 0x02, 0x00, 0x00, 0x00]))
            .await;
        assert_eq!(sub_rx.try_recv().unwrap(), vec![0x02]);
        assert!(matches!(
            rx.try_recv(),
            Ok(AACPEvent::ControlCommand(ControlCommandStatus {
                identifier: ControlCommandIdentifiers::ListeningMode,
                value,
            })) if value == vec![0x02]
        ));
        manager
            .receive_packet(&frame(&[0x09, 0x00, 0x06, 0x01, 0x00, 0x00, 0x00]))
            .await;
        assert!(manager.state.lock().await.owns);
        assert!(rx.try_recv().is_ok());
        manager
            .receive_packet(&frame(&[0x09, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00]))
            .await;
        assert!(!manager.state.lock().await.owns);
        assert!(rx.try_recv().is_ok());
        assert_eq!(
            manager.state.lock().await.control_command_status_list.len(),
            2
        );

        // Ear detection: old/new pair.
        manager
            .receive_packet(&frame(&[0x06, 0x00, 0x00, 0x01]))
            .await;
        manager
            .receive_packet(&frame(&[0x06, 0x00, 0x02, 0x03]))
            .await;
        let _ = rx.try_recv();
        match rx.try_recv() {
            Ok(AACPEvent::EarDetection(old, new)) => {
                assert_eq!(
                    old,
                    vec![EarDetectionStatus::InEar, EarDetectionStatus::OutOfEar]
                );
                assert_eq!(
                    new,
                    vec![EarDetectionStatus::InCase, EarDetectionStatus::Disconnected]
                );
            }
            other => panic!("{other:?}"),
        }

        // Conversational awareness.
        manager
            .receive_packet(&frame(&[0x4B, 0x00, 0x02, 0x00, 0x01, 0x05]))
            .await;
        assert_eq!(
            manager.state.lock().await.conversational_awareness_status,
            5
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(AACPEvent::ConversationalAwareness(5))
        ));

        // Audio source: MAC is sent little-endian.
        manager
            .receive_packet(&frame(&[
                0x0E, 0x00, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x02,
            ]))
            .await;
        assert_eq!(
            manager.state.lock().await.audio_source,
            Some(AudioSource {
                mac: "11:22:33:44:55:66".to_string(),
                r#type: AudioSourceType::Media,
            })
        );
        assert!(rx.try_recv().is_ok());

        // Connected devices.
        manager
            .receive_packet(&frame(&[
                0x2E, 0x00, 0x01, 0x00, 0x01, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x07, 0x08,
            ]))
            .await;
        assert_eq!(
            manager.state.lock().await.connected_devices,
            vec![ConnectedDevice {
                mac: "11:22:33:44:55:66".to_string(),
                info1: 0x07,
                info2: 0x08,
                r#type: None,
            }]
        );
        assert!(
            matches!(rx.try_recv(), Ok(AACPEvent::ConnectedDevices(old, new)) if old.is_empty() && new.len() == 1)
        );

        // Stem press.
        manager
            .receive_packet(&frame(&[0x19, 0x00, 0x06, 0x02]))
            .await;
        assert!(matches!(
            rx.try_recv(),
            Ok(AACPEvent::StemPress(
                StemPressType::Double,
                StemPressBudType::Right
            ))
        ));

        // Smart routing response asking us to give up ownership.
        let mut body = vec![0x11, 0x00];
        body.extend_from_slice(b"..audioRoutingSetOwnershipToFalse..");
        manager.receive_packet(&frame(&body)).await;
        assert!(matches!(
            rx.try_recv(),
            Ok(AACPEvent::OwnershipToFalseRequest)
        ));

        // Nothing below produces an event or touches the state.
        for bad in [
            vec![0x04, 0x00],
            vec![0x05, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01],
            HEADER_BYTES.to_vec(),
            frame(&[0x04, 0x00]),
            frame(&[0x04, 0x00, 0x01, 0x02, 0x01, 0x64, 0x02]),
            frame(&[0x09, 0x00, 0x0D, 0x02, 0x00, 0x00]),
            frame(&[0x09, 0x00, 0x7F, 0x02, 0x00, 0x00, 0x00]),
            frame(&[0x06, 0x00, 0x00]),
            frame(&[0x4B, 0x00, 0x02, 0x00, 0x01]),
            frame(&[0x0E, 0x00, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]),
            frame(&[0x19, 0x00, 0x06]),
            frame(&[0x19, 0x00, 0x09, 0x02]),
            frame(&[0x11]),
            frame(&[0x53, 0x00]),
            frame(&[0x77, 0x00]),
        ] {
            manager.receive_packet(&bad).await;
            assert!(rx.try_recv().is_err(), "{}", hex::encode(&bad));
        }
        let state = manager.state.lock().await;
        assert_eq!(state.battery_info.len(), 3);
        assert_eq!(state.connected_devices.len(), 1);
        assert_eq!(state.conversational_awareness_status, 5);
    }

    /// Each send method puts the header plus its builder's bytes on the wire,
    /// and a malformed MAC fails the send instead of panicking.
    #[tokio::test]
    async fn send_methods_frame_their_builders() {
        use super::build::*;
        use super::*;
        let manager = AACPManager::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        manager.state.lock().await.sender = Some(tx);
        let (me, them) = ("AA:BB:CC:DD:EE:FF", "11:22:33:44:55:66");

        manager
            .send_proximity_keys_request(vec![ProximityKeyType::Irk])
            .await
            .unwrap();
        manager.send_rename_packet("Pods").await.unwrap();
        manager
            .send_control_command(ControlCommandIdentifiers::ListeningMode, &[0x02])
            .await
            .unwrap();
        manager
            .send_media_information_new_device(me, them)
            .await
            .unwrap();
        manager.send_hijack_request(them).await.unwrap();
        manager
            .send_media_information(me, them, true)
            .await
            .unwrap();
        manager.send_smart_routing_show_ui(them).await.unwrap();
        manager.send_add_tipi_device(me, them).await.unwrap();
        for expected in [
            build_proximity_keys_request(&[ProximityKeyType::Irk]),
            build_rename("Pods"),
            build_control_command(ControlCommandIdentifiers::ListeningMode, &[0x02]),
            build_media_information_new_device(me, them).unwrap(),
            build_hijack_request(them).unwrap(),
            build_media_information(me, them, true).unwrap(),
            build_smart_routing_show_ui(them).unwrap(),
            build_add_tipi_device(me, them).unwrap(),
        ] {
            assert_eq!(rx.try_recv().unwrap(), frame(&expected));
        }

        assert!(manager.send_hijack_request("not a mac").await.is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn le_keys_debug_hides_key_material() {
        let keys = AirPodsLEKeys {
            irk: "00112233445566778899aabbccddeeff".to_string(),
            enc_key: "ffeeddccbbaa99887766554433221100".to_string(),
        };
        let shown = format!("{:?}", keys);
        assert!(
            !shown.contains("00112233") && !shown.contains("ffeeddcc"),
            "{shown}"
        );
    }
}
