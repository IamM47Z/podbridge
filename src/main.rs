mod audio;
mod bluetooth;
mod devices;
mod hand_back;
mod media_controller;
mod ui;
mod utils;

use crate::bluetooth::aacp::AACPManager;
use crate::bluetooth::discovery::{find_connected_airpods, find_other_managed_devices};
use crate::bluetooth::le::start_le_monitor;
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::enums::DeviceData;
use crate::ui::messages::BluetoothUIMessage;
use crate::ui::tray::MyTray;
use crate::utils::{ensure_device_registered, get_devices_path};
use bluer::{Address, InternalErrorKind};
use clap::Parser;
use dbus::arg::{RefArg, Variant};
use dbus::blocking::Connection;
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::message::MatchRule;
use devices::airpods::AirPodsDevice;
use ksni::TrayMethods;
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

#[derive(Parser)]
struct Args {
    #[arg(long, short = 'd', help = "Enable debug logging")]
    debug: bool,
    #[arg(
        long,
        help = "Disable system tray, useful if your environment doesn't support AppIndicator or StatusNotifier"
    )]
    no_tray: bool,
    #[arg(long, help = "Start the application minimized to tray")]
    start_minimized: bool,
    #[arg(
        long,
        help = "Enable Bluetooth LE debug logging. Only use when absolutely necessary; this produces a lot of logs."
    )]
    le_debug: bool,
    #[arg(long, short = 'v', help = "Show application version and exit")]
    version: bool,
}

fn main() -> iced::Result {
    let args = Args::parse();

    if args.version {
        println!(
            "You are running podbridge version {}",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }

    let log_level = if args.debug { "debug" } else { "info" };
    // let wayland_display = env::var("WAYLAND_DISPLAY").is_ok();
    // if wayland_display && env::var("WGPU_BACKEND").is_err() {
    //     unsafe { env::set_var("WGPU_BACKEND", "gl") };
    // }
    if env::var("RUST_LOG").is_err() {
        unsafe {
            env::set_var(
                "RUST_LOG",
                log_level.to_owned()
                    + &format!(
                        ",zbus=warn,winit=warn,tracing=warn,iced_wgpu=warn,wgpu_hal=warn,wgpu_core=warn,cosmic_text=warn,naga=warn,iced_winit=warn,podbridge::bluetooth::le={}",
                        if args.le_debug { "debug" } else { "info" }
                    ),
            )
        };
    }
    env_logger::init();

    // Single instance: two copies fight over the AirPodsHiRes mic module (each unloads the
    // other's as stale), so a second launch asks the running one to open its window and exits.
    let Ok(_instance_lock) = acquire_instance_lock(args.start_minimized) else {
        return Ok(());
    };

    let (ui_tx, ui_rx) = unbounded_channel::<BluetoothUIMessage>();

    let device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Load stem_control initial value from settings JSON, then apply CLI override.
    if args.no_tray {
        // Run headless without UI
        info!("Running in headless mode (no GUI)");
        run_backend(ui_tx, device_managers);
        Ok(())
    } else {
        // Run with UI
        let device_managers_clone = device_managers.clone();
        std::thread::spawn(|| run_backend(ui_tx, device_managers_clone));

        ui::window::start_ui(ui_rx, args.start_minimized, device_managers)
    }
}

/// Run the Bluetooth backend on its own runtime. Exits the process cleanly (no
/// panic, no core dump / crash dialog) if the backend can't run, e.g. no
/// Bluetooth adapter appeared in time.
fn run_backend(
    ui_tx: tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>,
    device_managers: DeviceMap,
) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            log::error!("Could not start the async runtime: {}", e);
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(async_main(ui_tx, device_managers)) {
        log::error!("Bluetooth backend stopped: {}", e);
        std::process::exit(1);
    }
}

async fn async_main(
    ui_tx: tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>,
    device_managers: DeviceMap,
) -> bluer::Result<()> {
    let args = Args::parse();

    let (devices_list, managed_devices_mac) = load_managed_devices();

    let (shutdown_tx, shutdown_rx) = unbounded_channel::<()>();
    spawn_shutdown_handler(device_managers.clone(), shutdown_rx);
    spawn_open_window_handler(ui_tx.clone());

    let tray_handle = if args.no_tray {
        None
    } else {
        start_tray(&ui_tx, &shutdown_tx).await
    };

    let session = bluer::Session::new().await?;
    let adapter = wait_for_adapter(&session).await?;
    adapter.set_powered(true).await?;
    let local_mac = adapter.address().await?.to_string();

    spawn_le_monitor(tray_handle.clone());

    info!("Listening for new connections.");

    let backend = Backend {
        ui_tx,
        tray_handle,
        device_managers,
        local_mac,
        connecting: Arc::new(std::sync::Mutex::new(HashSet::new())),
    };

    // Subscribe to connection changes *before* scanning for already connected
    // devices: AirPods connecting in between (e.g. right after the adapter
    // appears) would otherwise not be set up until they reconnect. Signals queue
    // until the process() loop below runs.
    let conn = Connection::new_system()?;
    let rule = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged");
    let watcher = ConnectionWatcher {
        backend: backend.clone(),
    };
    conn.add_match(rule, move |_: (), conn, msg| {
        watcher.on_properties_changed(conn, msg);
        true
    })?;

    info!("Checking for connected devices...");
    setup_connected_airpods(&adapter, &backend).await;
    setup_connected_managed_devices(&adapter, &backend, &devices_list, managed_devices_mac).await;

    info!("Listening for Bluetooth connections via D-Bus...");
    loop {
        conn.process(std::time::Duration::from_millis(1000))?;
    }
}

type DeviceMap = Arc<RwLock<HashMap<String, DeviceManagers>>>;

/// The known devices, and the MACs of the non-AirPods among them (AirPods are
/// found by their service UUID instead).
fn load_managed_devices() -> (HashMap<String, DeviceData>, Vec<String>) {
    let devices_path = get_devices_path();
    let devices_json = std::fs::read_to_string(&devices_path).unwrap_or_else(|e| {
        // Runs on every connect: a fresh install has no file yet.
        if e.kind() != std::io::ErrorKind::NotFound {
            log::error!("Failed to read devices file: {}", e);
        }
        "{}".to_string()
    });
    let devices_list: HashMap<String, DeviceData> = serde_json::from_str(&devices_json)
        .unwrap_or_else(|e| {
            log::error!("Deserialization failed: {}", e);
            HashMap::new()
        });
    let managed_devices_mac = devices_list
        .iter()
        .filter(|(_, device_data)| device_data.type_ == devices::enums::DeviceType::Nothing)
        .map(|(mac, _)| mac.clone())
        .collect();
    (devices_list, managed_devices_mac)
}

async fn start_tray(
    ui_tx: &tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>,
    shutdown_tx: &tokio::sync::mpsc::UnboundedSender<()>,
) -> Option<ksni::Handle<MyTray>> {
    let tray = MyTray {
        conversation_detect_enabled: None,
        battery_headphone: None,
        battery_headphone_status: None,
        battery_l: None,
        battery_l_status: None,
        battery_r: None,
        battery_r_status: None,
        battery_c: None,
        battery_c_status: None,
        connected: false,
        listening_mode: None,
        allow_off_option: None,
        command_tx: None,
        ui_tx: Some(ui_tx.clone()),
        shutdown_tx: Some(shutdown_tx.clone()),
    };
    // podbridge can be started by the session manager before the desktop's
    // StatusNotifierWatcher is ready. Keep the tray service alive in that
    // case so it can register when the watcher appears instead of aborting
    // the entire process during login.
    match tray.assume_sni_available(true).spawn().await {
        Ok(handle) => Some(handle),
        Err(e) => {
            log::warn!("Failed to start system tray ({e}); continuing without tray.");
            None
        }
    }
}

fn spawn_le_monitor(tray_handle: Option<ksni::Handle<MyTray>>) {
    tokio::spawn(async move {
        info!("Starting LE monitor...");
        if let Err(e) = start_le_monitor(tray_handle).await {
            // Not needed for connected AirPods; it only shows battery levels
            // from advertisements while they aren't connected.
            warn!(
                "Bluetooth LE monitor not available ({}); battery levels are only shown while the AirPods are connected",
                e
            );
        }
    });
}

/// What setting up a device needs.
#[derive(Clone)]
struct Backend {
    ui_tx: tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>,
    tray_handle: Option<ksni::Handle<MyTray>>,
    device_managers: DeviceMap,
    local_mac: String,
    connecting: Connecting,
}

impl Backend {
    fn send_ui(&self, message: BluetoothUIMessage) {
        if let Err(e) = self.ui_tx.send(message) {
            warn!("Failed to send UI message: {:?}", e);
        }
    }

    /// Store the AirPods' managers, end the setup claim and show them unless
    /// their control channel is down. Returns their AACP manager.
    async fn finish_airpods_setup(&self, mac: &str, airpods_device: AirPodsDevice) -> AACPManager {
        let aacp = airpods_device.aacp_manager.clone();
        let channel_up = aacp.is_connected();
        let mut managers = self.device_managers.write().await;
        // let dev_managers = DeviceManagers::with_both(airpods_device.aacp_manager.clone(), airpods_device.att_manager.clone());
        let dev_managers = DeviceManagers::with_aacp(airpods_device.aacp_manager.clone());
        managers
            .entry(mac.to_string())
            .or_insert(dev_managers)
            .set_aacp(airpods_device.aacp_manager);
        drop(managers);
        end_setup(&self.connecting, mac);
        if !channel_up {
            // Don't show a device whose controls can't work; the next
            // connect signal from BlueZ sets it up again.
            warn!("{} has no AACP channel; not showing it as connected", mac);
        } else {
            self.send_ui(BluetoothUIMessage::DeviceConnected(mac.to_string()));
        }
        aacp
    }

    /// Connect a Nothing device and show it. Runs in its own task.
    fn spawn_nothing_setup(&self, addr: Address, addr_str: String) {
        let backend = self.clone();
        tokio::spawn(async move {
            // Connect before taking the lock: the UI reads this map.
            let dev = match devices::nothing::NothingDevice::new(addr).await {
                Ok(dev) => dev,
                Err(e) => {
                    warn!("Could not set up Nothing device {}: {}", addr_str, e);
                    return;
                }
            };
            let mut managers = backend.device_managers.write().await;
            let dev_managers = DeviceManagers::with_att(dev.att_manager.clone());
            managers
                .entry(addr_str.clone())
                .or_insert(dev_managers)
                .set_att(dev.att_manager);
            drop(managers);
            backend.send_ui(BluetoothUIMessage::DeviceConnected(addr_str));
        });
    }
}

/// The Device1 property change a signal reports, as far as setup cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceChange {
    Connected,
    Disconnected,
    /// The service UUIDs changed without a connection change. Right after
    /// pairing they resolve only after Connected turned true.
    UuidsChanged,
}

fn device_change(connected: Option<bool>, uuids_changed: bool) -> Option<DeviceChange> {
    match connected {
        Some(true) => Some(DeviceChange::Connected),
        Some(false) => Some(DeviceChange::Disconnected),
        None if uuids_changed => Some(DeviceChange::UuidsChanged),
        None => None,
    }
}

/// Reacts to BlueZ device connection changes, and to UUIDs resolved late.
struct ConnectionWatcher {
    backend: Backend,
}

impl ConnectionWatcher {
    fn on_properties_changed(&self, conn: &Connection, msg: &dbus::Message) {
        let Some(path) = msg.path() else {
            return;
        };
        if !path.contains("/org/bluez/hci") || !path.contains("/dev_") {
            return;
        }
        // debug!("PropertiesChanged signal for path: {}", path);
        let Ok((iface, changed, _)) =
            msg.read3::<String, HashMap<String, Variant<Box<dyn RefArg>>>, Vec<String>>()
        else {
            return;
        };
        if iface != "org.bluez.Device1" {
            return;
        }
        let change = device_change(
            changed
                .get("Connected")
                .and_then(|v| v.0.as_ref().as_u64())
                .map(|c| c != 0),
            changed.contains_key("UUIDs"),
        );
        let Some(change) = change else {
            return;
        };
        let proxy = conn.with_proxy("org.bluez", &path, std::time::Duration::from_millis(5000));
        if change == DeviceChange::UuidsChanged
            && !proxy
                .get::<bool>("org.bluez.Device1", "Connected")
                .unwrap_or(false)
        {
            return;
        }
        let Ok(uuids) = proxy.get::<Vec<String>>("org.bluez.Device1", "UUIDs") else {
            debug!("{}: could not read its UUIDs; ignoring the change", path);
            return;
        };
        let target_uuid = "74ec2172-0bad-4d01-8f77-997b2be0722a";

        let Ok(addr_str) = proxy.get::<String>("org.bluez.Device1", "Address") else {
            debug!("{}: could not read its address; ignoring the change", path);
            return;
        };
        let Ok(addr) = addr_str.parse::<Address>() else {
            return;
        };
        if change == DeviceChange::Disconnected {
            if let Err(e) = self
                .backend
                .ui_tx
                .send(BluetoothUIMessage::DeviceDisconnected(addr_str.clone()))
            {
                warn!("Failed to send UI message: {:?}", e);
            }
            return;
        }
        // Read the file on each connect: devices added in the UI since startup count.
        let (devices_list, managed_devices_mac) = load_managed_devices();
        if managed_devices_mac.contains(&addr_str) {
            if change != DeviceChange::Connected {
                return;
            }
            info!("Managed device connected: {}, initializing", addr_str);
            if devices_list
                .get(&addr_str)
                .is_some_and(|d| d.type_ == devices::enums::DeviceType::Nothing)
            {
                self.backend.spawn_nothing_setup(addr, addr_str);
            }
            return;
        }

        if !uuids.iter().any(|u| u.to_lowercase() == target_uuid) {
            if change == DeviceChange::Connected {
                // Right after pairing the UUIDs may not be resolved yet; their
                // change comes as its own signal.
                debug!(
                    "{} connected without the AACP UUID (yet); not AirPods so far",
                    addr_str
                );
            }
            return;
        }
        let name = proxy
            .get::<String>("org.bluez.Device1", "Name")
            .unwrap_or_else(|_| "Unknown".to_string());
        ensure_device_registered(&addr_str, &name, devices::enums::DeviceType::AirPods);
        let backend = self.backend.clone();
        tokio::spawn(async move {
            if !begin_setup(&backend.connecting, &addr_str) {
                info!(
                    "Already setting up {}; ignoring duplicate connect",
                    addr_str
                );
                return;
            }
            // A UUIDs change on AirPods that are already set up changes nothing.
            if change == DeviceChange::UuidsChanged
                && aacp_link_up(&backend.device_managers, &addr_str).await
            {
                end_setup(&backend.connecting, &addr_str);
                debug!("{} UUIDs changed; already set up", addr_str);
                return;
            }
            info!("AirPods connected: {}, initializing", name);
            close_stale_link(&backend.device_managers, &addr_str).await;
            let airpods_device = AirPodsDevice::new(
                addr,
                backend.tray_handle.clone(),
                backend.ui_tx.clone(),
                backend.local_mac.clone(),
            )
            .await;
            backend
                .finish_airpods_setup(&addr_str, airpods_device)
                .await;
        });
    }
}

/// AirPods that were already connected when we started.
async fn setup_connected_airpods(adapter: &bluer::Adapter, backend: &Backend) {
    let device = match find_connected_airpods(adapter).await {
        Ok(device) => device,
        Err(_) => {
            info!("No connected AirPods found.");
            return;
        }
    };
    // A failed name lookup must not stop the whole backend.
    let name = device
        .name()
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "Unknown".to_string());
    info!("Found connected AirPods: {}, initializing.", name);
    ensure_device_registered(
        &device.address().to_string(),
        &name,
        devices::enums::DeviceType::AirPods,
    );
    let mac_str = device.address().to_string();
    begin_setup(&backend.connecting, &mac_str);
    let airpods_device = AirPodsDevice::new(
        device.address(),
        backend.tray_handle.clone(),
        backend.ui_tx.clone(),
        backend.local_mac.clone(),
    )
    .await;

    let hires_aacp = backend.finish_airpods_setup(&mac_str, airpods_device).await;
    // PODBRIDGE_HIRES_MIC=1: enable the hi-res mic feature headlessly.
    if env::var("PODBRIDGE_HIRES_MIC").is_ok() {
        tokio::spawn(async move {
            hires_aacp.set_hires_mic_enabled(true).await;
        });
    }
}

/// Non-AirPods devices that were already connected when we started.
async fn setup_connected_managed_devices(
    adapter: &bluer::Adapter,
    backend: &Backend,
    devices_list: &HashMap<String, DeviceData>,
    managed_devices_mac: Vec<String>,
) {
    match find_other_managed_devices(adapter, managed_devices_mac).await {
        Ok(devices) => {
            for device in devices {
                let addr_str = device.address().to_string();
                info!(
                    "Found connected managed device: {}, initializing.",
                    addr_str
                );
                if devices_list
                    .get(&addr_str)
                    .is_some_and(|d| d.type_ == devices::enums::DeviceType::Nothing)
                {
                    backend.spawn_nothing_setup(device.address(), addr_str);
                }
            }
        }
        Err(e) => {
            log::debug!("type of error: {:?}", e.kind);
            if e.kind
                != bluer::ErrorKind::Internal(InternalErrorKind::Io(std::io::ErrorKind::NotFound))
            {
                log::error!("Error finding other managed devices: {}", e);
            } else {
                info!("No other managed devices found.");
            }
        }
    }
}

/// MACs whose AirPodsDevice is being set up right now.
type Connecting = Arc<std::sync::Mutex<HashSet<String>>>;

/// Claim the setup of a device; false if it's already being set up (duplicate
/// connect signal, or the startup scan racing the signal).
fn begin_setup(connecting: &Connecting, mac: &str) -> bool {
    connecting
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(mac.to_string())
}

fn end_setup(connecting: &Connecting, mac: &str) {
    connecting
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(mac);
}

/// True if these AirPods have a live AACP link.
async fn aacp_link_up(device_managers: &DeviceMap, mac: &str) -> bool {
    device_managers
        .read()
        .await
        .get(mac)
        .and_then(|m| m.get_aacp())
        .is_some_and(|aacp| aacp.is_connected())
}

/// BlueZ says the AirPods (re)connected, but our previous AACP link for them may
/// not have noticed yet that it died: close it so only one device stack runs.
async fn close_stale_link(
    device_managers: &Arc<RwLock<HashMap<String, DeviceManagers>>>,
    mac: &str,
) {
    let old = device_managers
        .read()
        .await
        .get(mac)
        .and_then(|m| m.get_aacp());
    if let Some(old) = old
        && old.is_connected()
    {
        info!(
            "{} connected again while the old AACP link looked alive; closing it",
            mac
        );
        old.close().await;
    }
}

/// The adapter can appear a moment after we start (USB dongle plugged in, BlueZ
/// still registering it). Wait up to ~20 s instead of failing right away.
async fn wait_for_adapter(session: &bluer::Session) -> bluer::Result<bluer::Adapter> {
    let mut attempt = 0;
    loop {
        match session.default_adapter().await {
            Ok(adapter) => return Ok(adapter),
            Err(e) if attempt < 20 => {
                if attempt == 0 {
                    info!("Waiting for a Bluetooth adapter ({})", e);
                }
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Takes `$XDG_RUNTIME_DIR/podbridge.lock` for the life of the process. `Ok(None)` means the
/// lock file could not be opened, so we run without the check. `Err(())` means another
/// instance holds it: unless we were started minimized (autostart), it is sent SIGUSR1 to
/// open its window, and the caller should exit.
fn acquire_instance_lock(start_minimized: bool) -> Result<Option<std::fs::File>, ()> {
    use std::io::{Read, Write};

    let dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&dir).join("podbridge.lock");
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            warn!(
                "Could not open {}: {}; skipping single-instance check",
                path.display(),
                e
            );
            return Ok(None);
        }
    };

    match file.try_lock() {
        Ok(()) => {
            let _ = file.set_len(0);
            let _ = write!(file, "{}", std::process::id());
            Ok(Some(file))
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut pid = String::new();
            let _ = file.read_to_string(&mut pid);
            let pid = pid.trim();
            if start_minimized {
                info!("podbridge is already running (pid {}); exiting", pid);
            } else if !pid.is_empty() {
                info!(
                    "podbridge is already running (pid {}); asking it to open its window",
                    pid
                );
                let _ = std::process::Command::new("kill")
                    .args(["-USR1", pid])
                    .status();
            }
            Err(())
        }
        Err(std::fs::TryLockError::Error(e)) => {
            warn!(
                "Could not lock {}: {}; skipping single-instance check",
                path.display(),
                e
            );
            Ok(Some(file))
        }
    }
}

// SIGUSR1 (sent by a second launch, see acquire_instance_lock) opens the main window.
fn spawn_open_window_handler(ui_tx: tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut sigusr1) = signal(SignalKind::user_defined1()) else {
            return;
        };
        while sigusr1.recv().await.is_some() {
            let _ = ui_tx.send(BluetoothUIMessage::OpenWindow);
        }
    });
}

fn spawn_shutdown_handler(
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    mut shutdown_rx: UnboundedReceiver<()>,
) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).ok();
        let mut sigint = signal(SignalKind::interrupt()).ok();
        let sigterm = async {
            match sigterm.as_mut() {
                Some(s) => s.recv().await.map(|_| ()),
                None => std::future::pending().await,
            }
        };
        let sigint = async {
            match sigint.as_mut() {
                Some(s) => s.recv().await.map(|_| ()),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = shutdown_rx.recv() => {}
            _ = sigterm => {}
            _ = sigint => {}
        }

        info!("Shutting down: tearing down hi-res mic streams...");
        let cleanup = async {
            let managers = device_managers.read().await;
            for dm in managers.values() {
                if let Some(aacp) = dm.get_aacp() {
                    aacp.disarm_hires_mic().await;
                }
            }
            // The virtual mic stays loaded for the next run; send apps elsewhere.
            let _ = tokio::task::spawn_blocking(crate::audio::hires_mic::park_on_exit).await;
        };
        if tokio::time::timeout(std::time::Duration::from_secs(6), cleanup)
            .await
            .is_err()
        {
            warn!("Shutdown cleanup timed out; exiting anyway");
        }
        std::process::exit(0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_change_wins_over_a_uuids_change() {
        assert_eq!(
            device_change(Some(true), true),
            Some(DeviceChange::Connected)
        );
        assert_eq!(
            device_change(Some(false), true),
            Some(DeviceChange::Disconnected)
        );
    }

    #[test]
    fn uuids_resolved_after_pairing_count_as_a_change() {
        assert_eq!(device_change(None, true), Some(DeviceChange::UuidsChanged));
        assert_eq!(device_change(None, false), None);
    }
}
