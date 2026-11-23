use aes::Aes128;
use aes::cipher::Array;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use iced::Theme;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// The folder name under the XDG data, config and state folders.
pub const APP_DIR: &str = "podbridge";

/// `$<xdg_var>/podbridge`, or `$HOME/<home_fallback>/podbridge` when unset.
/// Under `cfg(test)`, a per-process folder under the temp dir instead.
pub fn app_dir(xdg_var: &str, home_fallback: &str) -> PathBuf {
    #[cfg(test)]
    {
        // Tests never touch the real user folders: every path lives in a
        // per-process folder under the system temp dir.
        let _ = home_fallback;
        std::env::temp_dir()
            .join(format!("podbridge-test-{}", std::process::id()))
            .join(xdg_var)
            .join(APP_DIR)
    }
    #[cfg(not(test))]
    {
        let base = std::env::var(xdg_var).unwrap_or_else(|_| {
            format!(
                "{}/{}",
                std::env::var("HOME").unwrap_or_default(),
                home_fallback
            )
        });
        PathBuf::from(base).join(APP_DIR)
    }
}

pub fn get_devices_path() -> PathBuf {
    app_dir("XDG_DATA_HOME", ".local/share").join("devices.json")
}

/// Write a file atomically: write a temp file next to it, then rename over it.
/// A plain fs::write truncates first, so a concurrent reader (settings and the
/// devices file are read all the time) could see an empty or partial file, fall
/// back to defaults, and the next save would overwrite the user's data with them.
pub fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Unique per write: two threads saving the same file at once must not
    // share (and interleave in) one temp file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Serialises read-change-write of the devices file within the process.
static DEVICES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read the devices file, change it and write it back under one lock, so
/// writers never undo each other's changes. `change` returns whether to write.
/// A file that exists but cannot be read is left alone, so that the other
/// devices in it are not overwritten.
pub fn update_devices(
    change: impl FnOnce(&mut HashMap<String, crate::devices::enums::DeviceData>) -> bool,
) {
    update_devices_at(&get_devices_path(), change);
}

fn update_devices_at(
    path: &std::path::Path,
    change: impl FnOnce(&mut HashMap<String, crate::devices::enums::DeviceData>) -> bool,
) {
    let _guard = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut devices = match std::fs::read_to_string(path) {
        Ok(json) => match serde_json::from_str(&json) {
            Ok(devices) => devices,
            Err(e) => {
                log::error!("Devices file is unreadable, not changing it: {}", e);
                return;
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            log::error!("Failed to read devices file, not changing it: {}", e);
            return;
        }
    };
    if !change(&mut devices) {
        return;
    }
    match serde_json::to_string(&devices) {
        Ok(json) => {
            if let Err(e) = write_atomic(path, json.as_bytes()) {
                log::error!("Failed to write devices file: {}", e);
            }
        }
        Err(e) => log::error!("Failed to serialize devices file: {}", e),
    }
}

pub fn ensure_device_registered(mac: &str, name: &str, type_: crate::devices::enums::DeviceType) {
    update_devices(|devices| {
        if devices.contains_key(mac) {
            return false;
        }
        log::info!("Registering device {} ({}) as {:?}", name, mac, type_);
        devices.insert(
            mac.to_string(),
            crate::devices::enums::DeviceData {
                name: name.to_string(),
                type_,
                information: None,
            },
        );
        true
    });
}

pub fn get_preferences_path() -> PathBuf {
    app_dir("XDG_CONFIG_HOME", ".config").join("preferences.json")
}

pub fn get_app_settings_path() -> PathBuf {
    let new_path = app_dir("XDG_CONFIG_HOME", ".config").join("app_settings.json");

    // Callers write here with fs::write, which fails silently without the folder.
    if let Some(parent) = new_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    new_path
}

fn e(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let mut swapped_key = *key;
    swapped_key.reverse();
    let mut swapped_data = *data;
    swapped_data.reverse();
    let cipher = Aes128::new(&Array::from(swapped_key));
    let mut block = Array::from(swapped_data);
    cipher.encrypt_block(&mut block);
    let mut result: [u8; 16] = block.into();
    result.reverse();
    result
}

pub fn ah(k: &[u8; 16], r: &[u8; 3]) -> [u8; 3] {
    let mut r_padded = [0u8; 16];
    r_padded[..3].copy_from_slice(r);
    let encrypted = e(k, &r_padded);
    let mut hash = [0u8; 3];
    hash.copy_from_slice(&encrypted[..3]);
    hash
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MyTheme {
    Light,
    Dark,
    Dracula,
    Nord,
    SolarizedLight,
    SolarizedDark,
    GruvboxLight,
    GruvboxDark,
    CatppuccinLatte,
    CatppuccinFrappe,
    CatppuccinMacchiato,
    CatppuccinMocha,
    TokyoNight,
    TokyoNightStorm,
    TokyoNightLight,
    KanagawaWave,
    KanagawaDragon,
    KanagawaLotus,
    Moonfly,
    Nightfly,
    Oxocarbon,
    Ferra,
}

impl std::fmt::Display for MyTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::Dracula => "Dracula",
            Self::Nord => "Nord",
            Self::SolarizedLight => "Solarized Light",
            Self::SolarizedDark => "Solarized Dark",
            Self::GruvboxLight => "Gruvbox Light",
            Self::GruvboxDark => "Gruvbox Dark",
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::CatppuccinFrappe => "Catppuccin Frappé",
            Self::CatppuccinMacchiato => "Catppuccin Macchiato",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::TokyoNight => "Tokyo Night",
            Self::TokyoNightStorm => "Tokyo Night Storm",
            Self::TokyoNightLight => "Tokyo Night Light",
            Self::KanagawaWave => "Kanagawa Wave",
            Self::KanagawaDragon => "Kanagawa Dragon",
            Self::KanagawaLotus => "Kanagawa Lotus",
            Self::Moonfly => "Moonfly",
            Self::Nightfly => "Nightfly",
            Self::Oxocarbon => "Oxocarbon",
            Self::Ferra => "Ferra",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub theme: MyTheme,
    pub tray_text_mode: bool,
    pub stem_control: bool,
    pub hires_mic_enabled: bool,
    pub hires_mic_agc: bool,
    pub hires_mic_pause_convo: bool,
    pub a2dp_reset: bool,
    /// No bud worn for NOT_WORN_GRACE: switch the AirPods' audio off and hide the
    /// hi-res mic, so apps move to another output and microphone. Both come back
    /// when a bud goes in again.
    pub audio_off_when_not_wearing: bool,
    /// One bud out: pause media like an iPhone (otherwise keep playing).
    pub pause_when_one_removed: bool,
    /// Conversation awareness was switched off for a mic capture and couldn't
    /// be switched back on before the AirPods disconnected.
    pub convo_restore_pending: bool,
    /// AirPods volume (%) from before conversation awareness lowered it, while
    /// that is in effect; restored on the next activation if a disconnect
    /// interrupted the normal restore.
    pub conv_volume_restore: Option<u32>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: MyTheme::Dark,
            tray_text_mode: false,
            stem_control: false,
            hires_mic_enabled: true,
            hires_mic_agc: true,
            hires_mic_pause_convo: true,
            a2dp_reset: true,
            audio_off_when_not_wearing: true,
            pause_when_one_removed: true,
            convo_restore_pending: false,
            conv_volume_restore: None,
        }
    }
}

impl AppSettings {
    pub fn load() -> Self {
        let path = get_app_settings_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str(&text) {
            Ok(settings) => settings,
            Err(e) => {
                // Defaults are used, and the next save would replace the file
                // with them: keep a copy of what was there (once per run).
                static WARNED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    let backup = path.with_extension("json.unreadable");
                    let _ = std::fs::copy(&path, &backup);
                    log::warn!(
                        "Could not read {} ({}); using defaults. A copy was kept at {}",
                        path.display(),
                        e,
                        backup.display()
                    );
                }
                Self::default()
            }
        }
    }

    /// Change settings on disk: load, apply `change`, save, as one step. The
    /// window and several backend tasks all write this file; separate
    /// load()/save() calls from two of them would drop one side's change.
    pub fn update(change: impl FnOnce(&mut AppSettings)) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = Self::load();
        change(&mut settings);
        settings.save();
    }

    pub fn save(&self) {
        let path = get_app_settings_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = write_atomic(&path, json.as_bytes()) {
                    log::error!("Failed to write app settings: {}", e);
                }
            }
            Err(e) => log::error!("Failed to serialize app settings: {}", e),
        }
    }
}

impl From<MyTheme> for Theme {
    fn from(my_theme: MyTheme) -> Self {
        match my_theme {
            MyTheme::Light => Theme::Light,
            MyTheme::Dark => Theme::Dark,
            MyTheme::Dracula => Theme::Dracula,
            MyTheme::Nord => Theme::Nord,
            MyTheme::SolarizedLight => Theme::SolarizedLight,
            MyTheme::SolarizedDark => Theme::SolarizedDark,
            MyTheme::GruvboxLight => Theme::GruvboxLight,
            MyTheme::GruvboxDark => Theme::GruvboxDark,
            MyTheme::CatppuccinLatte => Theme::CatppuccinLatte,
            MyTheme::CatppuccinFrappe => Theme::CatppuccinFrappe,
            MyTheme::CatppuccinMacchiato => Theme::CatppuccinMacchiato,
            MyTheme::CatppuccinMocha => Theme::CatppuccinMocha,
            MyTheme::TokyoNight => Theme::TokyoNight,
            MyTheme::TokyoNightStorm => Theme::TokyoNightStorm,
            MyTheme::TokyoNightLight => Theme::TokyoNightLight,
            MyTheme::KanagawaWave => Theme::KanagawaWave,
            MyTheme::KanagawaDragon => Theme::KanagawaDragon,
            MyTheme::KanagawaLotus => Theme::KanagawaLotus,
            MyTheme::Moonfly => Theme::Moonfly,
            MyTheme::Nightfly => Theme::Nightfly,
            MyTheme::Oxocarbon => Theme::Oxocarbon,
            MyTheme::Ferra => Theme::Ferra,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::devices::enums::{DeviceData, DeviceType};

    fn device(name: &str) -> DeviceData {
        DeviceData {
            name: name.to_string(),
            type_: DeviceType::Nothing,
            information: None,
        }
    }

    fn temp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("podbridge-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn update_devices_keeps_what_others_wrote() {
        let path = temp_file("update-keeps.json");
        super::update_devices_at(&path, |d| {
            d.insert("a".into(), device("A"));
            true
        });
        super::update_devices_at(&path, |d| {
            d.insert("b".into(), device("B"));
            true
        });
        let saved: std::collections::HashMap<String, DeviceData> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved.contains_key("a") && saved.contains_key("b"));
    }

    #[test]
    fn update_devices_leaves_an_unreadable_file_alone() {
        let path = temp_file("update-corrupt.json");
        std::fs::write(&path, "{not json").unwrap();
        super::update_devices_at(&path, |d| {
            d.insert("a".into(), device("A"));
            true
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    }

    #[test]
    fn test_paths_stay_out_of_the_user_folders() {
        let root = std::env::temp_dir().join(format!("podbridge-test-{}", std::process::id()));
        for path in [
            super::get_devices_path(),
            super::get_preferences_path(),
            super::get_app_settings_path(),
        ] {
            assert!(
                path.starts_with(&root),
                "{} is outside the test root",
                path.display()
            );
        }
    }
}
