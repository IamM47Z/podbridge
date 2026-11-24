//! Debug builds only: `PODBRIDGE_UI_PREVIEW=1` opens the window on a sample
//! AirPods and a sample Nothing device, to check the layout without them.
//! Nothing touches Bluetooth, the tray or the running instance's lock.

use super::bluetooth::noise_control_modes;
use super::{App, BluetoothUIMessage, Message, Tab, main_window_settings, run_app};
use crate::bluetooth::aacp::{
    AACPManager, AirPodsLEKeys, BatteryComponent, BatteryInfo, BatteryStatus,
};
use crate::bluetooth::att::ATTManager;
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::airpods::AirPodsInformation;
use crate::devices::enums::{
    AirPodsNoiseControlMode, AirPodsState, DeviceData, DeviceInformation, DeviceState, DeviceType,
    NothingAncMode, NothingState,
};
use crate::devices::nothing::NothingInformation;
use crate::devices::{controls, press_and_hold};
use crate::ui::widgets::Section;
use iced::widget::combo_box;
use iced::{Size, Task, window};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// The environment variable that starts the preview.
pub const ENV: &str = "PODBRIDGE_UI_PREVIEW";

const AIRPODS_MAC: &str = "02:00:00:00:00:01";
const NOTHING_MAC: &str = "02:00:00:00:00:02";

/// Opens the preview window. It writes sample devices and settings, so it
/// points the XDG config and data folders at a fresh temp dir first.
pub fn run() -> iced::Result {
    let dir = std::env::temp_dir().join(format!("podbridge-preview-{}", std::process::id()));
    // A folder left by an earlier run with the same PID would not be fresh.
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("{ENV}: could not create {}: {e}", dir.display());
        std::process::exit(2);
    }
    eprintln!("{ENV}: settings and devices go to {}", dir.display());
    // SAFETY: no other thread runs yet; main calls this before starting any.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("XDG_DATA_HOME", &dir);
    }
    // The AACP manager keeps a handle to the runtime it was made in.
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = runtime.enter();

    crate::utils::update_devices(|devices| {
        devices.insert(AIRPODS_MAC.to_string(), airpods_data());
        devices.insert(NOTHING_MAC.to_string(), nothing_data());
        true
    });
    let mut managers = HashMap::new();
    managers.insert(
        AIRPODS_MAC.to_string(),
        DeviceManagers::with_aacp(AACPManager::new()),
    );
    managers.insert(
        NOTHING_MAC.to_string(),
        DeviceManagers::with_att(ATTManager::new()),
    );
    let device_managers = Arc::new(RwLock::new(managers));
    // Kept open for the window's lifetime: a closed channel spins the UI.
    let (ui_tx, ui_rx) = tokio::sync::mpsc::unbounded_channel::<BluetoothUIMessage>();
    let ui_rx = Arc::new(Mutex::new(ui_rx));

    run_app(move || {
        let _keep_open = &ui_tx;
        // Opened here instead of by App::new, at the size asked for: a tiling
        // compositor may ignore a later resize.
        let (mut app, task) = App::new(Arc::clone(&ui_rx), true, Arc::clone(&device_managers));
        let (id, open) = window::open(window::Settings {
            size: window_size(),
            ..main_window_settings()
        });
        app.window = Some(id);
        app.window_opened_at = Some(std::time::Instant::now());
        app.device_states.insert(
            AIRPODS_MAC.to_string(),
            DeviceState::AirPods(airpods_state()),
        );
        app.device_states.insert(
            NOTHING_MAC.to_string(),
            DeviceState::Nothing(NothingState {
                anc_mode: NothingAncMode::Transparency,
                anc_mode_state: combo_box::State::new(vec![
                    NothingAncMode::Off,
                    NothingAncMode::Transparency,
                    NothingAncMode::HighNoiseCancellation,
                ]),
            }),
        );
        app.bluetooth_state.connected_devices =
            vec![AIRPODS_MAC.to_string(), NOTHING_MAC.to_string()];
        app.selected_tab = Tab::Device(AIRPODS_MAC.to_string());
        let tour = match std::env::var_os(SHOTS_ENV) {
            Some(dir) => screenshot_tour(id, dir.into()),
            None => Task::none(),
        };
        (
            app,
            Task::batch([task, open.map(Message::WindowOpened), tour]),
        )
    })
}

/// Set to a folder, the preview walks through its pages, saves a screenshot
/// of each there and exits.
pub const SHOTS_ENV: &str = "PODBRIDGE_UI_PREVIEW_SHOTS";

/// The window's size as `WIDTHxHEIGHT`, by default the app's own.
pub const SIZE_ENV: &str = "PODBRIDGE_UI_PREVIEW_SIZE";

fn window_size() -> Size {
    std::env::var(SIZE_ENV)
        .ok()
        .and_then(|size| {
            let (width, height) = size.split_once('x')?;
            Some(Size::new(width.parse().ok()?, height.parse().ok()?))
        })
        .unwrap_or(main_window_settings().size)
}

fn screenshot_tour(id: window::Id, dir: PathBuf) -> Task<Message> {
    let size = window_size();
    let steps: Vec<(&str, Vec<Message>)> = vec![
        ("airpods", vec![]),
        ("sidebar", vec![Message::SidebarToggled]),
        ("settings", vec![Message::SelectTab(Tab::Settings)]),
        (
            "nothing",
            vec![Message::SelectTab(Tab::Device(NOTHING_MAC.to_string()))],
        ),
        (
            "airpods-open",
            Section::ALL
                .iter()
                .map(|&s| Message::SectionToggled(s))
                .chain([Message::SelectTab(Tab::Device(AIRPODS_MAC.to_string()))])
                .collect(),
        ),
        (
            "settings-open",
            vec![
                Message::StemControlChanged(true),
                Message::SelectTab(Tab::Settings),
            ],
        ),
    ];
    // Lets the window open first.
    let mut tour = pause();
    for (name, messages) in steps {
        let path = dir.join(format!("{name}-{}x{}.png", size.width, size.height));
        tour = tour
            .chain(Task::batch(messages.into_iter().map(Task::done)))
            .chain(pause())
            .chain(window::screenshot(id).then(move |shot| {
                let saved = image::RgbaImage::from_raw(
                    shot.size.width,
                    shot.size.height,
                    shot.rgba.to_vec(),
                )
                .map(|img| img.save(&path));
                eprintln!("{}: {:?}", path.display(), saved.map(|r| r.is_ok()));
                Task::none()
            }));
    }
    tour.chain(iced::exit())
}

/// Lets the window open or redraw.
fn pause() -> Task<Message> {
    Task::future(async {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        Message::MicLevelTick
    })
}

fn airpods_state() -> AirPodsState {
    let battery = |component, level, status| BatteryInfo {
        component,
        level,
        status,
    };
    AirPodsState {
        device_name: "Sample AirPods Pro".to_string(),
        noise_control_mode: AirPodsNoiseControlMode::Adaptive,
        noise_control_state: combo_box::State::new(noise_control_modes(true)),
        conversation_awareness_enabled: true,
        personalized_volume_enabled: false,
        allow_off_mode: true,
        adaptive_strength: Some(50),
        press_and_hold_modes: Some(press_and_hold::DEFAULT | press_and_hold::ADAPTIVE),
        one_bud_anc: Some(controls::OFF),
        chime_volume: Some(70),
        volume_swipe: Some(controls::ON),
        volume_swipe_interval: Some(controls::VOLUME_SWIPE_INTERVALS[0].value),
        press_speed: Some(controls::PRESS_SPEEDS[0].value),
        press_and_hold_duration: Some(controls::PRESS_AND_HOLD_DURATIONS[0].value),
        ear_detection: Some(controls::ON),
        mic_mode: Some(controls::MIC_MODES[0].value),
        sleep_detection: Some(controls::ON),
        hires_mic_enabled: true,
        battery: vec![
            battery(BatteryComponent::Left, 80, BatteryStatus::NotCharging),
            battery(BatteryComponent::Right, 75, BatteryStatus::NotCharging),
            battery(BatteryComponent::Case, 40, BatteryStatus::Charging),
        ],
    }
}

fn airpods_data() -> DeviceData {
    let sample = |s: &str| s.to_string();
    DeviceData {
        name: sample("Sample AirPods Pro"),
        type_: DeviceType::AirPods,
        information: Some(DeviceInformation::AirPods(Box::new(AirPodsInformation {
            name: sample("Sample AirPods Pro"),
            model_number: sample("A0000"),
            manufacturer: sample("Sample"),
            serial_number: sample("SAMPLE000001"),
            version1: sample("0A000"),
            version2: sample("0A000"),
            hardware_revision: sample("1.0.0"),
            updater_identifier: sample("0.0.0"),
            left_serial_number: sample("SAMPLE000002"),
            right_serial_number: sample("SAMPLE000003"),
            version3: sample("0.0.0"),
            le_keys: AirPodsLEKeys {
                irk: String::new(),
                enc_key: String::new(),
            },
        }))),
    }
}

fn nothing_data() -> DeviceData {
    DeviceData {
        name: "Sample Nothing Ear".to_string(),
        type_: DeviceType::Nothing,
        information: Some(DeviceInformation::Nothing(NothingInformation {
            serial_number: "SAMPLE000004".to_string(),
            firmware_version: "0.0.0".to_string(),
        })),
    }
}
