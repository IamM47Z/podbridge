use crate::bluetooth::managers::DeviceManagers;
use crate::devices::enums::{DeviceData, DeviceState, DeviceType};
use crate::ui::messages::BluetoothUIMessage;
use crate::utils::{AppSettings, MyTheme};
use bluer::Address;
use iced::widget::{combo_box, container, pane_grid};
use iced::{Element, Font, Length, Settings, Size, Subscription, Task, Theme, daemon, window};
use log::error;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{Mutex, RwLock};

mod actions;
mod add_device;
mod bluetooth;
mod content;
mod settings;
mod sidebar;

pub fn start_ui(
    ui_rx: UnboundedReceiver<BluetoothUIMessage>,
    start_minimized: bool,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    // stem_control: Arc<AtomicBool>,
) -> iced::Result {
    let ui_rx = Arc::new(Mutex::new(ui_rx));

    // not sure if this is a good idea
    daemon(
        move || {
            App::new(
                Arc::clone(&ui_rx),
                start_minimized,
                Arc::clone(&device_managers),
                // Arc::clone(&stem_control),
            )
        },
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .theme(App::theme)
    .title(App::title)
    .settings(Settings {
        id: Some(APP_ID.to_string()),
        fonts: vec![
            include_bytes!("../../assets/font/Inter-Regular.ttf")
                .as_slice()
                .into(),
        ],
        default_font: Font::with_name("Inter"),
        ..Settings::default()
    })
    .run()
}

/// The basename of the desktop file; Wayland compositors match windows by it.
pub(crate) const APP_ID: &str = "io.github.iamm47z.podbridge";

pub struct App {
    window: Option<window::Id>,
    // When `window` was last opened, so a tray double-click doesn't replace it twice.
    window_opened_at: Option<Instant>,
    panes: pane_grid::State<Pane>,
    selected_tab: Tab,
    theme_state: combo_box::State<MyTheme>,
    selected_theme: MyTheme,
    ui_rx: Arc<Mutex<UnboundedReceiver<BluetoothUIMessage>>>,
    bluetooth_state: BluetoothState,
    paired_devices: HashMap<String, Address>,
    device_states: HashMap<String, DeviceState>,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    pending_add_device: Option<(String, Address)>,
    device_type_state: combo_box::State<DeviceType>,
    selected_device_type: Option<DeviceType>,
    tray_text_mode: bool,
    stem_control: bool,
    hires_mic_enabled: bool,
    hires_mic_agc: bool,
    hires_mic_pause_convo: bool,
    a2dp_reset: bool,
    audio_off_when_not_wearing: bool,
    pause_when_one_removed: bool,
}

pub struct BluetoothState {
    connected_devices: Vec<String>,
}

impl BluetoothState {
    pub fn new() -> Self {
        Self {
            connected_devices: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    Resized(pane_grid::ResizeEvent),
    SelectTab(Tab),
    ThemeSelected(MyTheme),
    CopyToClipboard(String),
    Bluetooth(BluetoothUIMessage),
    ShowNewDialogTab,
    GotPairedDevices(HashMap<String, Address>),
    StartAddDevice(String, Address),
    SelectDeviceType(DeviceType),
    ConfirmAddDevice,
    CancelAddDevice,
    StateChanged(String, DeviceState),
    /// The adaptive strength slider was let go: send its level (mac, level).
    AdaptiveStrengthReleased(String, u8),
    TrayTextModeChanged(bool), // yes, I know I should add all settings to a struct, but I'm lazy
    StemControlChanged(bool),
    A2dpResetChanged(bool),
    AudioOffWhenNotWearingChanged(bool),
    PauseWhenOneRemovedChanged(bool),
    HiResMicAgcChanged(bool),
    HiResMicPauseConvoChanged(bool),
    MicLevelTick,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Device(String),
    Settings,
    AddDevice,
}

#[derive(Clone, Copy)]
pub enum Pane {
    Sidebar,
    Content,
}

impl App {
    pub fn new(
        ui_rx: Arc<Mutex<UnboundedReceiver<BluetoothUIMessage>>>,
        start_minimized: bool,
        device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
        // stem_control: Arc<AtomicBool>,
    ) -> (Self, Task<Message>) {
        let (mut panes, first_pane) = pane_grid::State::new(Pane::Sidebar);
        let split = panes.split(pane_grid::Axis::Vertical, first_pane, Pane::Content);
        panes.resize(split.unwrap().1, 0.2);

        let wait_task = Task::perform(wait_for_message(Arc::clone(&ui_rx)), |msg| msg);

        let (window, open_task) = if start_minimized {
            (None, Task::none())
        } else {
            let (id, open) = window::open(main_window_settings());
            (Some(id), open.map(Message::WindowOpened))
        };
        let window_opened_at = window.map(|_| Instant::now());

        let app_settings = AppSettings::load();
        let selected_theme = app_settings.theme;
        let tray_text_mode = app_settings.tray_text_mode;
        let stem_control = app_settings.stem_control;
        let hires_mic_enabled = app_settings.hires_mic_enabled;
        let hires_mic_agc = app_settings.hires_mic_agc;
        let hires_mic_pause_convo = app_settings.hires_mic_pause_convo;
        let a2dp_reset = app_settings.a2dp_reset;
        let audio_off_when_not_wearing = app_settings.audio_off_when_not_wearing;
        let pause_when_one_removed = app_settings.pause_when_one_removed;

        let bluetooth_state = BluetoothState::new();

        // let dummy_device_state = DeviceState::AirPods(AirPodsState {
        //     conversation_awareness_enabled: false,
        // });
        // let device_states = HashMap::from([
        //     ("02:00:00:00:00:05".to_string(), dummy_device_state),
        // ]);

        let device_states = HashMap::new();
        (
            Self {
                window,
                window_opened_at,
                panes,
                selected_tab: Tab::Device("none".to_string()),
                theme_state: combo_box::State::new(vec![
                    MyTheme::Light,
                    MyTheme::Dark,
                    MyTheme::Dracula,
                    MyTheme::Nord,
                    MyTheme::SolarizedLight,
                    MyTheme::SolarizedDark,
                    MyTheme::GruvboxLight,
                    MyTheme::GruvboxDark,
                    MyTheme::CatppuccinLatte,
                    MyTheme::CatppuccinFrappe,
                    MyTheme::CatppuccinMacchiato,
                    MyTheme::CatppuccinMocha,
                    MyTheme::TokyoNight,
                    MyTheme::TokyoNightStorm,
                    MyTheme::TokyoNightLight,
                    MyTheme::KanagawaWave,
                    MyTheme::KanagawaDragon,
                    MyTheme::KanagawaLotus,
                    MyTheme::Moonfly,
                    MyTheme::Nightfly,
                    MyTheme::Oxocarbon,
                    MyTheme::Ferra,
                ]),
                selected_theme,
                ui_rx,
                bluetooth_state,
                paired_devices: HashMap::new(),
                device_states,
                pending_add_device: None,
                device_type_state: combo_box::State::new(vec![DeviceType::Nothing]),
                selected_device_type: None,
                device_managers,
                tray_text_mode,
                stem_control,
                hires_mic_enabled,
                hires_mic_agc,
                hires_mic_pause_convo,
                a2dp_reset,
                audio_off_when_not_wearing,
                pause_when_one_removed,
            },
            Task::batch(vec![open_task, wait_task]),
        )
    }

    fn save_settings(&self) {
        // Only the fields the window edits; the rest is written by the backend.
        AppSettings::update(|s| {
            s.theme = self.selected_theme;
            s.tray_text_mode = self.tray_text_mode;
            s.stem_control = self.stem_control;
            s.hires_mic_enabled = self.hires_mic_enabled;
            s.hires_mic_agc = self.hires_mic_agc;
            s.hires_mic_pause_convo = self.hires_mic_pause_convo;
            s.a2dp_reset = self.a2dp_reset;
            s.audio_off_when_not_wearing = self.audio_off_when_not_wearing;
            s.pause_when_one_removed = self.pause_when_one_removed;
        });
    }

    /// Waits for the next message from the Bluetooth side.
    fn wait_task(&self) -> Task<Message> {
        Task::perform(wait_for_message(Arc::clone(&self.ui_rx)), |msg| msg)
    }

    fn title(&self, _id: window::Id) -> String {
        "podbridge".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowOpened(id) => {
                self.window = Some(id);
                Task::none()
            }
            Message::WindowClosed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                }
                Task::none()
            }
            Message::Resized(event) => {
                self.panes.resize(event.split, event.ratio);
                Task::none()
            }
            Message::SelectTab(tab) => {
                self.selected_tab = tab;
                Task::none()
            }
            Message::CopyToClipboard(data) => iced::clipboard::write(data),
            Message::MicLevelTick => Task::none(),
            Message::Bluetooth(ui_message) => self.handle_bluetooth(ui_message),
            Message::ShowNewDialogTab => {
                self.selected_tab = Tab::AddDevice;
                Task::perform(load_paired_devices(), Message::GotPairedDevices)
            }
            Message::GotPairedDevices(map) => self.got_paired_devices(map),
            Message::StartAddDevice(name, addr) => self.start_add_device(name, addr),
            Message::SelectDeviceType(device_type) => {
                self.selected_device_type = Some(device_type);
                Task::none()
            }
            Message::ConfirmAddDevice => self.confirm_add_device(),
            Message::CancelAddDevice => self.cancel_add_device(),
            Message::StateChanged(mac, state) => self.state_changed(mac, state),
            Message::AdaptiveStrengthReleased(mac, level) => {
                self.send_adaptive_strength(&mac, level)
            }
            Message::ThemeSelected(theme) => self.change_setting(|app| app.selected_theme = theme),
            Message::TrayTextModeChanged(on) => self.change_setting(|app| app.tray_text_mode = on),
            Message::StemControlChanged(on) => self.change_setting(|app| app.stem_control = on),
            Message::A2dpResetChanged(on) => self.change_setting(|app| app.a2dp_reset = on),
            Message::AudioOffWhenNotWearingChanged(on) => {
                self.change_setting(|app| app.audio_off_when_not_wearing = on)
            }
            Message::PauseWhenOneRemovedChanged(on) => {
                self.change_setting(|app| app.pause_when_one_removed = on)
            }
            Message::HiResMicAgcChanged(on) => self.change_setting(|app| app.hires_mic_agc = on),
            Message::HiResMicPauseConvoChanged(on) => {
                self.change_setting(|app| app.hires_mic_pause_convo = on)
            }
        }
    }

    fn view(&self, _id: window::Id) -> Element<'_, Message> {
        let devices_list = read_devices_list();
        let pane_grid =
            pane_grid::PaneGrid::new(&self.panes, |_pane_id, pane, _is_maximized| match pane {
                Pane::Sidebar => self.sidebar_pane(&devices_list),
                Pane::Content => self.content_pane(&devices_list),
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .on_resize(20, Message::Resized);

        container(pane_grid).into()
    }

    fn theme(&self, _id: window::Id) -> Theme {
        self.selected_theme.into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let close = window::close_events().map(Message::WindowClosed);

        // Only tick while a hi-res mic is capturing.
        let mic_active = self
            .device_states
            .values()
            .any(|s| matches!(s, DeviceState::AirPods(a) if a.hires_mic_enabled));

        if mic_active {
            let tick = iced::time::every(std::time::Duration::from_millis(50))
                .map(|_| Message::MicLevelTick);
            Subscription::batch([close, tick])
        } else {
            close
        }
    }
}

fn main_window_settings() -> window::Settings {
    window::Settings {
        min_size: Some(Size::new(400.0, 300.0)),
        icon: window::icon::from_file_data(include_bytes!("../../assets/icon.png"), None).ok(),
        // iced takes the Linux app_id from here; Settings.id only reaches the BSDs.
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The saved devices, empty when the file is missing or unreadable.
fn read_devices_list() -> HashMap<String, DeviceData> {
    let devices_json =
        std::fs::read_to_string(crate::utils::get_devices_path()).unwrap_or_else(|e| {
            error!("Failed to read devices file: {}", e);
            "{}".to_string()
        });
    serde_json::from_str(&devices_json).unwrap_or_else(|e| {
        error!("Deserialization failed: {}", e);
        HashMap::new()
    })
}

async fn wait_for_message(ui_rx: Arc<Mutex<UnboundedReceiver<BluetoothUIMessage>>>) -> Message {
    let mut rx = ui_rx.lock().await;
    match rx.recv().await {
        Some(msg) => Message::Bluetooth(msg),
        None => {
            error!("UI message channel closed");
            Message::Bluetooth(BluetoothUIMessage::NoOp)
        }
    }
}

/// Paired Bluetooth devices by name. A D-Bus failure is logged and gives an
/// empty or partial list.
async fn load_paired_devices() -> HashMap<String, Address> {
    let mut devices = HashMap::new();
    let adapter = match bluer::Session::new().await {
        Ok(session) => session.default_adapter().await,
        Err(e) => Err(e),
    };
    let adapter = match adapter {
        Ok(adapter) => adapter,
        Err(e) => {
            error!("Failed to open the Bluetooth adapter: {}", e);
            return devices;
        }
    };
    let addresses = match adapter.device_addresses().await {
        Ok(addresses) => addresses,
        Err(e) => {
            error!("Failed to list Bluetooth devices: {}", e);
            return devices;
        }
    };
    for addr in addresses {
        let device = match adapter.device(addr) {
            Ok(device) => device,
            Err(e) => {
                error!("Failed to open Bluetooth device {}: {}", addr, e);
                continue;
            }
        };
        if !device.is_paired().await.unwrap_or(false) {
            continue;
        }
        let name = device
            .name()
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| addr.to_string());
        devices.insert(name, addr);
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_window_has_the_desktop_file_app_id() {
        assert_eq!(
            main_window_settings().platform_specific.application_id,
            APP_ID
        );
    }
}
