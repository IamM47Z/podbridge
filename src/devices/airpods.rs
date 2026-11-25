use crate::bluetooth::aacp::ControlCommandIdentifiers;
use crate::bluetooth::aacp::{AACPEvent, AACPManager, AirPodsLEKeys, ProximityKeyType};
use crate::devices::controls;
use crate::devices::low_battery::{alert_body, check_for, notify};
use crate::media_controller::MediaController;
use crate::ui::messages::BluetoothUIMessage;
use crate::ui::tray::MyTray;
use crate::utils::{StemAction, get_app_settings_path};
use bluer::Address;
use ksni::Handle;
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

pub struct AirPodsDevice {
    pub aacp_manager: AACPManager,
    // pub att_manager: ATTManager,
    // pub command_tx: Option<tokio::sync::mpsc::UnboundedSender<(ControlCommandIdentifiers, Vec<u8>)>>,
}

type CommandSender = tokio::sync::mpsc::UnboundedSender<(ControlCommandIdentifiers, Vec<u8>)>;

impl AirPodsDevice {
    pub async fn new(
        mac_address: Address,
        tray_handle: Option<Handle<MyTray>>,
        ui_tx: tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>,
        local_mac: String,
    ) -> Self {
        info!("Creating new AirPodsDevice for {}", mac_address);
        let mut aacp_manager = AACPManager::new();
        if !aacp_manager.connect(mac_address).await {
            // No control channel: nothing below can work (every send would
            // fail), and the tray must not claim the AirPods are connected.
            return AirPodsDevice { aacp_manager };
        }

        // let mut att_manager = ATTManager::new();
        // att_manager.connect(mac_address).await.expect("Failed to connect ATT");

        if let Some(handle) = &tray_handle {
            handle
                .update(|tray: &mut MyTray| {
                    tray.connected = true;
                    clear_tray_batteries(tray);
                })
                .await;
        }

        send_handshake_and_requests(&aacp_manager).await;
        restore_pending_conversation_detection(&aacp_manager).await;
        let stem_control = stem_control_enabled();
        if stem_control {
            enable_stem_press_detection(&aacp_manager).await;
        }
        restore_press_and_hold_modes(&aacp_manager, &mac_address.to_string()).await;
        restore_stored_controls(&aacp_manager, &mac_address.to_string()).await;

        let media_controller = Arc::new(Mutex::new(MediaController::new(
            mac_address.to_string(),
            local_mac.clone(),
        )));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();

        aacp_manager.set_event_channel(tx).await;
        if let Some(handle) = &tray_handle {
            handle
                .update(|tray: &mut MyTray| tray.command_tx = Some(command_tx.clone()))
                .await;
        }
        if let Some(handle) = tray_handle.clone() {
            spawn_tray_reset_on_disconnect(&aacp_manager, handle);
        }
        spawn_command_sender(&aacp_manager, command_rx);

        media_controller
            .lock()
            .await
            .start_playback_listener(aacp_manager.clone(), command_tx.clone())
            .await;

        spawn_tray_routing(&aacp_manager, &tray_handle).await;
        spawn_ownership_watch(&aacp_manager, media_controller.clone()).await;
        spawn_ear_detection_watch(&aacp_manager, media_controller.clone()).await;

        let events = EventLoop {
            aacp: aacp_manager.clone(),
            mac_address,
            local_mac,
            ui_tx,
            command_tx,
            media_controller,
            tray_handle,
            stem_control,
        };
        spawn_for_connection(&aacp_manager, events.run(rx));

        AirPodsDevice {
            aacp_manager,
            // att_manager,
            // command_tx: Some(command_tx.clone()),
        }
    }
}

fn clear_tray_batteries(tray: &mut MyTray) {
    tray.battery_headphone = None;
    tray.battery_headphone_status = None;
    tray.battery_l = None;
    tray.battery_l_status = None;
    tray.battery_r = None;
    tray.battery_r_status = None;
    tray.battery_c = None;
    tray.battery_c_status = None;
}

/// The packets that open an AACP session, and the requests that follow them.
async fn send_handshake_and_requests(aacp_manager: &AACPManager) {
    info!("Sending handshake");
    if let Err(e) = aacp_manager.send_handshake().await {
        error!("Failed to send handshake to AirPods device: {}", e);
    }

    sleep(Duration::from_millis(300)).await;

    info!("Setting feature flags");
    if let Err(e) = aacp_manager.send_set_feature_flags_packet().await {
        error!("Failed to set feature flags: {}", e);
    }

    sleep(Duration::from_millis(300)).await;

    info!("Requesting notifications");
    if let Err(e) = aacp_manager.send_notification_request().await {
        error!("Failed to request notifications: {}", e);
    }

    info!("sending some packet");
    if let Err(e) = aacp_manager.send_some_packet().await {
        error!("Failed to send some packet: {}", e);
    }

    info!("Requesting Proximity Keys: IRK and ENC_KEY");
    if let Err(e) = aacp_manager
        .send_proximity_keys_request(vec![ProximityKeyType::Irk, ProximityKeyType::EncKey])
        .await
    {
        error!("Failed to request proximity keys: {}", e);
    }
}

async fn restore_pending_conversation_detection(aacp_manager: &AACPManager) {
    if crate::utils::AppSettings::load().convo_restore_pending {
        info!("Restoring conversation awareness left off by an interrupted mic capture");
        if aacp_manager.set_conversation_detection(true).await {
            crate::utils::AppSettings::update(|s| s.convo_restore_pending = false);
        }
    }
}

fn stem_control_enabled() -> bool {
    let app_settings_path = get_app_settings_path();
    let settings = std::fs::read_to_string(&app_settings_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    settings
        .and_then(|v| v.get("stem_control").cloned())
        .and_then(|s| serde_json::from_value(s).ok())
        .unwrap_or(false)
}

async fn enable_stem_press_detection(aacp_manager: &AACPManager) {
    // Enable stem press detection (double and triple tap)
    // StemConfig bitmask for the control command: single=0x01, double=0x02, triple=0x04, long=0x08
    // We want double and triple: 0x02 | 0x04 = 0x06
    // Note: these bitmask values differ from the StemPressType event enum values (0x05–0x08)
    info!("Enabling stem press detection for double and triple tap");
    if let Err(e) = aacp_manager
        .send_control_command(ControlCommandIdentifiers::StemConfig, &[0x06])
        .await
    {
        error!("Failed to enable stem press detection: {}", e);
    }
}

/// The AirPods forget the press-and-hold modes when they join an Apple device
/// that is not iCloud-synced; send the ones chosen here again.
async fn restore_press_and_hold_modes(aacp_manager: &AACPManager, mac: &str) {
    let Some(&mask) = crate::utils::AppSettings::load()
        .press_and_hold_modes
        .get(mac)
    else {
        return;
    };
    info!("Restoring press-and-hold modes {:#04x}", mask);
    match aacp_manager
        .send_control_command(ControlCommandIdentifiers::ListeningModeConfigs, &[mask])
        .await
    {
        // Whether the AirPods report 0x1A back after a write is unverified;
        // record it so the UI shows what was sent either way.
        Ok(()) => {
            aacp_manager
                .record_control_status(ControlCommandIdentifiers::ListeningModeConfigs, mask)
                .await
        }
        Err(e) => error!("Failed to restore press-and-hold modes: {}", e),
    }
}

/// Send the stored choice of each setting in controls::STORED again. Record
/// it so the backend and the page follow it.
async fn restore_stored_controls(aacp_manager: &AACPManager, mac: &str) {
    let Some(stored) = crate::utils::AppSettings::load()
        .stored_controls
        .remove(mac)
    else {
        return;
    };
    for (identifier, _) in controls::STORED {
        let Some(&value) = stored.get(&(identifier as u8)) else {
            continue;
        };
        info!("Restoring {:?} {:#04x}", identifier, value);
        match aacp_manager
            .send_control_command(identifier, &[value])
            .await
        {
            Ok(()) => aacp_manager.record_control_status(identifier, value).await,
            Err(e) => error!("Failed to restore {:?}: {}", identifier, e),
        }
    }
}

/// When the connection ends, stop showing it: the tray would keep the last
/// battery levels and a listening-mode menu that sends nowhere.
fn spawn_tray_reset_on_disconnect(aacp_manager: &AACPManager, handle: Handle<MyTray>) {
    let aacp = aacp_manager.clone();
    tokio::spawn(async move {
        aacp.disconnected().await;
        handle
            .update(|tray: &mut MyTray| {
                tray.connected = false;
                clear_tray_batteries(tray);
                tray.listening_mode = None;
                tray.allow_off_option = None;
                tray.conversation_detect_enabled = None;
                tray.command_tx = None;
            })
            .await;
    });
}

/// Sends the control commands the tray and the media controller queue up.
fn spawn_command_sender(
    aacp_manager: &AACPManager,
    mut command_rx: tokio::sync::mpsc::UnboundedReceiver<(ControlCommandIdentifiers, Vec<u8>)>,
) {
    let aacp_manager_clone = aacp_manager.clone();
    spawn_for_connection(aacp_manager, async move {
        while let Some((id, value)) = command_rx.recv().await {
            if let Err(e) = aacp_manager_clone.send_control_command(id, &value).await {
                log::error!("Failed to send control command: {}", e);
            }
        }
    });
}

/// Mirror the control commands the tray menu shows into the tray.
async fn spawn_tray_routing(aacp_manager: &AACPManager, tray_handle: &Option<Handle<MyTray>>) {
    mirror_into_tray(
        aacp_manager,
        ControlCommandIdentifiers::ListeningMode,
        tray_handle,
        |tray, value| tray.listening_mode = Some(value[0]),
    )
    .await;
    mirror_into_tray(
        aacp_manager,
        ControlCommandIdentifiers::AllowOffOption,
        tray_handle,
        |tray, value| tray.allow_off_option = Some(value[0]),
    )
    .await;
    mirror_into_tray(
        aacp_manager,
        ControlCommandIdentifiers::ConversationDetectConfig,
        tray_handle,
        |tray, value| tray.conversation_detect_enabled = Some(value[0] == 0x01),
    )
    .await;
}

/// Subscribe to one control command and apply each value to the tray. Values
/// are never empty: parsing yields at least one byte.
async fn mirror_into_tray(
    aacp_manager: &AACPManager,
    identifier: ControlCommandIdentifiers,
    tray_handle: &Option<Handle<MyTray>>,
    apply: fn(&mut MyTray, &[u8]),
) {
    let (value_tx, mut value_rx) = tokio::sync::mpsc::unbounded_channel();
    aacp_manager
        .subscribe_to_control_command(identifier, value_tx)
        .await;
    let tray_handle_clone = tray_handle.clone();
    spawn_for_connection(aacp_manager, async move {
        while let Some(value) = value_rx.recv().await {
            if let Some(handle) = &tray_handle_clone {
                handle.update(|tray: &mut MyTray| apply(tray, &value)).await;
            }
        }
    });
}

async fn spawn_ownership_watch(
    aacp_manager: &AACPManager,
    media_controller: Arc<Mutex<MediaController>>,
) {
    let (owns_connection_tx, mut owns_connection_rx) = tokio::sync::mpsc::unbounded_channel();
    aacp_manager
        .subscribe_to_control_command(
            ControlCommandIdentifiers::OwnsConnection,
            owns_connection_tx,
        )
        .await;
    spawn_for_connection(aacp_manager, async move {
        while let Some(value) = owns_connection_rx.recv().await {
            let owns = value.first().copied().unwrap_or(0) != 0;
            if !owns {
                info!("Lost ownership, pausing media and disconnecting audio");
                let controller = media_controller.lock().await;
                controller.forget_claim().await;
                controller.pause_all_media().await;
                controller.deactivate_a2dp_profile().await;
            }
        }
    });
}

/// Follow the AirPods' Automatic Ear Detection setting (0x0A), reported or
/// set from the device page, in podbridge's own ear-based pause and resume.
async fn spawn_ear_detection_watch(
    aacp_manager: &AACPManager,
    media_controller: Arc<Mutex<MediaController>>,
) {
    let (value_tx, mut value_rx) = tokio::sync::mpsc::unbounded_channel();
    aacp_manager
        .subscribe_to_control_command(ControlCommandIdentifiers::EarDetectionConfig, value_tx)
        .await;
    spawn_for_connection(aacp_manager, async move {
        while let Some(value) = value_rx.recv().await {
            media_controller
                .lock()
                .await
                .set_ear_detection_enabled(controls::is_on(&value))
                .await;
        }
    });
}

/// Handles the AACP events of one connection.
struct EventLoop {
    aacp: AACPManager,
    mac_address: Address,
    local_mac: String,
    ui_tx: tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>,
    command_tx: CommandSender,
    media_controller: Arc<Mutex<MediaController>>,
    tray_handle: Option<Handle<MyTray>>,
    stem_control: bool,
}

impl EventLoop {
    async fn run(self, mut rx: tokio::sync::mpsc::UnboundedReceiver<AACPEvent>) {
        let mut connect_routing = ConnectRouting::default();
        while let Some(event) = rx.recv().await {
            if let AACPEvent::BatteryInfo(battery) = &event {
                let crossed = check_for(&self.mac_address.to_string(), battery);
                if !crossed.is_empty() {
                    let body = alert_body(&crossed);
                    info!("Low battery: {}", body);
                    tokio::task::spawn_blocking(move || notify("AirPods battery low", &body));
                }
            }
            let connect_check = match &event {
                AACPEvent::EarDetection(old, _) if old.is_empty() => {
                    connect_routing.note_first_ear()
                }
                AACPEvent::AudioSource(_) => connect_routing.note_source(),
                _ => false,
            };
            if connect_check {
                self.route_audio_on_connect().await;
            }
            self.handle(event).await;
        }
    }

    async fn route_audio_on_connect(&self) {
        if self.aacp.is_worn().await && !self.aacp.other_device_using_audio(&self.local_mac).await {
            info!("Connected while worn and no other device uses the AirPods; routing audio here");
            let mc = self.media_controller.clone();
            spawn_for_connection(&self.aacp, async move {
                mc.lock().await.activate_a2dp_profile_on_connect().await;
            });
        }
    }

    async fn handle(&self, event: AACPEvent) {
        match &event {
            AACPEvent::EarDetection(old_status, new_status) => {
                debug!(
                    "Received EarDetection event: old_status={:?}, new_status={:?}",
                    old_status, new_status
                );
                let other_device_active = self.aacp.other_device_using_audio(&self.local_mac).await;
                let controller = self.media_controller.lock().await;
                controller
                    .handle_ear_detection(
                        old_status.clone(),
                        new_status.clone(),
                        other_device_active,
                    )
                    .await;
            }
            AACPEvent::BatteryInfo(battery_info) => {
                debug!("Received BatteryInfo event: {:?}", battery_info);
                self.show_battery_in_tray(battery_info).await;
                debug!("Updated tray with new battery info");
                self.send_to_ui(event.clone());
                debug!("Sent BatteryInfo event to UI");
            }
            AACPEvent::ControlCommand(status) => {
                debug!("Received ControlCommand event: {:?}", status);
                self.send_to_ui(event.clone());
                debug!("Sent ControlCommand event to UI");
            }
            AACPEvent::ConversationalAwareness(status) => {
                debug!("Received ConversationalAwareness event: {}", status);
                let controller = self.media_controller.lock().await;
                controller.handle_conversational_awareness(*status).await;
            }
            AACPEvent::ConnectedDevices(old_devices, new_devices) => {
                self.greet_new_devices(old_devices, new_devices);
            }
            AACPEvent::OwnershipToFalseRequest => {
                info!(
                    "Received ownership to false request. Setting ownership to false and pausing media."
                );
                let _ = self
                    .command_tx
                    .send((ControlCommandIdentifiers::OwnsConnection, vec![0x00]));
                let controller = self.media_controller.lock().await;
                controller.forget_claim().await;
                controller.pause_all_media().await;
                controller.deactivate_a2dp_profile().await;
            }
            AACPEvent::StemPress(press_type, bud_type) => {
                info!("Received Stem Press: {:?} on {:?}", press_type, bud_type);
                self.handle_stem_press(press_type).await;
            }
            _ => {
                debug!("Received unhandled AACP event: {:?}", event);
                self.send_to_ui(event.clone());
                debug!("Sent unhandled AACP event to UI");
            }
        }
    }

    fn send_to_ui(&self, event: AACPEvent) {
        let _ = self.ui_tx.send(BluetoothUIMessage::AACPUIEvent(
            self.mac_address.to_string(),
            event,
        ));
    }

    async fn show_battery_in_tray(&self, battery_info: &[crate::bluetooth::aacp::BatteryInfo]) {
        let Some(handle) = &self.tray_handle else {
            return;
        };
        handle
            .update(|tray: &mut MyTray| {
                for b in battery_info {
                    match b.component as u8 {
                        0x01 => {
                            tray.battery_headphone = Some(b.level);
                            tray.battery_headphone_status = Some(b.status);
                        }
                        0x02 => {
                            tray.battery_r = Some(b.level);
                            tray.battery_r_status = Some(b.status);
                        }
                        0x04 => {
                            tray.battery_l = Some(b.level);
                            tray.battery_l_status = Some(b.status);
                        }
                        0x08 => {
                            tray.battery_c = Some(b.level);
                            tray.battery_c_status = Some(b.status);
                        }
                        _ => {}
                    }
                }
            })
            .await;
    }

    /// Tell the AirPods about devices that just joined them (not us).
    fn greet_new_devices(
        &self,
        old_devices: &[crate::bluetooth::aacp::ConnectedDevice],
        new_devices: &[crate::bluetooth::aacp::ConnectedDevice],
    ) {
        let local_mac = &self.local_mac;
        let new_devices_filtered = new_devices.iter().filter(|new_device| {
            let not_in_old = old_devices
                .iter()
                .all(|old_device| old_device.mac != new_device.mac);
            let not_local = new_device.mac != *local_mac;
            not_in_old && not_local
        });

        for device in new_devices_filtered {
            info!(
                "New connected device: {}, info1: {}, info2: {}",
                device.mac, device.info1, device.info2
            );
            info!(
                "Sending new Tipi packet for device {}, and sending media info to the device",
                device.mac
            );
            let aacp_manager_clone = self.aacp.clone();
            let local_mac_clone = local_mac.clone();
            let device_mac_clone = device.mac.clone();
            tokio::spawn(async move {
                if let Err(e) = aacp_manager_clone
                    .send_media_information_new_device(&local_mac_clone, &device_mac_clone)
                    .await
                {
                    error!("Failed to send media info new device: {}", e);
                }
                if let Err(e) = aacp_manager_clone
                    .send_add_tipi_device(&local_mac_clone, &device_mac_clone)
                    .await
                {
                    error!("Failed to send add tipi device: {}", e);
                }
            });
        }
    }

    async fn handle_stem_press(&self, press_type: &crate::bluetooth::aacp::StemPressType) {
        if !self.stem_control {
            debug!("Stem control disabled, ignoring stem press event");
            return;
        }
        // Read on each press, so a change in Settings applies at once.
        let Some(action) = stem_action(press_type, &crate::utils::AppSettings::load()) else {
            debug!("Unhandled stem press type: {:?}", press_type);
            return;
        };
        info!("{:?} press: {}", press_type, action);
        let controller = self.media_controller.lock().await;
        match action {
            StemAction::PlayPause => controller.play_pause().await,
            StemAction::NextTrack => controller.next_track().await,
            StemAction::PreviousTrack => controller.previous_track().await,
        }
    }
}

/// The action the settings give a stem press. Single and long presses stay
/// with the AirPods: podbridge only asks for double and triple presses.
fn stem_action(
    press: &crate::bluetooth::aacp::StemPressType,
    settings: &crate::utils::AppSettings,
) -> Option<StemAction> {
    use crate::bluetooth::aacp::StemPressType;
    match press {
        StemPressType::Double => Some(settings.stem_double_press),
        StemPressType::Triple => Some(settings.stem_triple_press),
        StemPressType::Single | StemPressType::Long => None,
    }
}

/// Routing audio here at connect needs both the first ear reading (is a bud
/// in?) and the first AudioSource report (is another device using the
/// AirPods?). They arrive in either order; this says when both are in, once.
#[derive(Default)]
struct ConnectRouting {
    ear_seen: bool,
    source_seen: bool,
    done: bool,
}

impl ConnectRouting {
    fn note_first_ear(&mut self) -> bool {
        self.ear_seen = true;
        self.ready()
    }

    fn note_source(&mut self) -> bool {
        self.source_seen = true;
        self.ready()
    }

    fn ready(&mut self) -> bool {
        if self.done || !self.ear_seen || !self.source_seen {
            return false;
        }
        self.done = true;
        true
    }
}

/// Run a per-connection task until it finishes or the AACP connection drops,
/// whichever comes first. These loops hold AACPManager clones (and channels the
/// manager's state holds the other end of), so without this they never end and
/// pile up with every reconnect.
fn spawn_for_connection<F>(aacp: &AACPManager, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let aacp = aacp.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = aacp.disconnected() => {}
            _ = fut => {}
        }
    });
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirPodsInformation {
    pub name: String,
    pub model_number: String,
    pub manufacturer: String,
    pub serial_number: String,
    pub version1: String,
    pub version2: String,
    pub hardware_revision: String,
    pub updater_identifier: String,
    pub left_serial_number: String,
    pub right_serial_number: String,
    pub version3: String,
    pub le_keys: AirPodsLEKeys,
}

#[cfg(test)]
mod tests {
    use super::{ConnectRouting, stem_action};
    use crate::bluetooth::aacp::StemPressType;
    use crate::utils::{AppSettings, StemAction};

    #[test]
    fn stem_presses_default_to_next_and_previous_track() {
        let s = AppSettings::default();
        assert_eq!(
            stem_action(&StemPressType::Double, &s),
            Some(StemAction::NextTrack)
        );
        assert_eq!(
            stem_action(&StemPressType::Triple, &s),
            Some(StemAction::PreviousTrack)
        );
    }

    #[test]
    fn stem_presses_follow_the_settings() {
        let s = AppSettings {
            stem_double_press: StemAction::PlayPause,
            stem_triple_press: StemAction::NextTrack,
            ..AppSettings::default()
        };
        assert_eq!(
            stem_action(&StemPressType::Double, &s),
            Some(StemAction::PlayPause)
        );
        assert_eq!(
            stem_action(&StemPressType::Triple, &s),
            Some(StemAction::NextTrack)
        );
    }

    #[test]
    fn single_and_long_presses_are_left_to_the_airpods() {
        let s = AppSettings::default();
        assert_eq!(stem_action(&StemPressType::Single, &s), None);
        assert_eq!(stem_action(&StemPressType::Long, &s), None);
    }

    #[test]
    fn connect_routing_fires_once_after_both_first_reports() {
        let mut g = ConnectRouting::default();
        assert!(!g.note_source());
        assert!(g.note_first_ear());
        assert!(!g.note_source());
        assert!(!g.note_first_ear());

        let mut g = ConnectRouting::default();
        assert!(!g.note_first_ear());
        assert!(g.note_source());
        assert!(!g.note_source());
    }
}
