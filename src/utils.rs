use aes::Aes128;
use aes::cipher::Array;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use iced::Theme;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The folder name under the XDG data, config and state folders.
pub const APP_DIR: &str = "podbridge";
/// The folder name an earlier LibrePods install used.
const OLD_APP_DIR: &str = "librepods";

/// Copies what an earlier LibrePods install left in the XDG data, config and state
/// folders into the podbridge folders, once. It copies rather than moves, so going
/// back to LibrePods still works. Must run before anything creates a podbridge folder.
pub fn migrate_from_librepods() {
    let home = std::env::var("HOME").unwrap_or_default();
    let base = |var: &str, default: &str| {
        PathBuf::from(std::env::var(var).unwrap_or_else(|_| format!("{home}/{default}")))
    };
    for dir in [
        base("XDG_DATA_HOME", ".local/share"),
        base("XDG_CONFIG_HOME", ".config"),
        base("XDG_STATE_HOME", ".local/state"),
    ] {
        let (old, new) = (dir.join(OLD_APP_DIR), dir.join(APP_DIR));
        match copy_old_app_dir(&old, &new) {
            Ok(true) => log::info!("Copied {} to {}", old.display(), new.display()),
            Ok(false) => {}
            Err(e) => log::warn!(
                "Could not copy {} to {}: {}",
                old.display(),
                new.display(),
                e
            ),
        }
    }
}

/// Copies `old` to `new` when `old` exists and `new` does not. A temp folder renamed
/// at the end keeps an interrupted copy from passing for a finished one. Entries that
/// fail to copy are logged and skipped: other code creates the `new` folders right
/// after, so a copy aborted here would never be retried.
fn copy_old_app_dir(old: &Path, new: &Path) -> std::io::Result<bool> {
    if !old.is_dir() || new.exists() {
        return Ok(false);
    }
    let tmp = new.with_extension("migrating");
    let _ = std::fs::remove_dir_all(&tmp);
    copy_dir(old, &tmp)?;
    std::fs::rename(&tmp, new)?;
    Ok(true)
}

/// Copies a folder, keeping its mode (the devices file holds pairing keys) and
/// copying links as links. Leftover `write_atomic` temp files are skipped.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    std::fs::set_permissions(to, std::fs::metadata(from)?.permissions())?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().contains(".tmp.") {
            continue;
        }
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let copied = entry.file_type().and_then(|kind| {
            if kind.is_symlink() {
                std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)
            } else if kind.is_dir() {
                copy_dir(&source, &target)
            } else {
                std::fs::copy(&source, &target).map(|_| ())
            }
        });
        if let Err(e) = copied {
            log::warn!("Skipped {} while copying: {}", source.display(), e);
        }
    }
    Ok(())
}

pub fn get_devices_path() -> PathBuf {
    let data_dir = std::env::var("XDG_DATA_HOME")
        .unwrap_or_else(|_| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    PathBuf::from(data_dir).join(APP_DIR).join("devices.json")
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

pub fn ensure_device_registered(mac: &str, name: &str, type_: crate::devices::enums::DeviceType) {
    use crate::devices::enums::DeviceData;
    use std::collections::HashMap;

    let path = get_devices_path();
    let json = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".to_string());
    let mut devices: HashMap<String, DeviceData> = serde_json::from_str(&json).unwrap_or_default();
    if devices.contains_key(mac) {
        return;
    }
    log::info!("Registering device {} ({}) as {:?}", name, mac, type_);
    devices.insert(
        mac.to_string(),
        DeviceData {
            name: name.to_string(),
            type_,
            information: None,
        },
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string(&devices) {
        Ok(updated) => {
            if let Err(e) = write_atomic(&path, updated.as_bytes()) {
                log::error!("Failed to write devices file: {}", e);
            }
        }
        Err(e) => log::error!("Failed to serialize devices file: {}", e),
    }
}

pub fn get_preferences_path() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .unwrap_or_else(|_| format!("{}/.config", std::env::var("HOME").unwrap_or_default()));
    PathBuf::from(config_dir)
        .join(APP_DIR)
        .join("preferences.json")
}

pub fn get_app_settings_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();

    let config_dir =
        std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home));

    let data_dir =
        std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", home));

    let new_path = PathBuf::from(&config_dir)
        .join(APP_DIR)
        .join("app_settings.json");

    let old_path = PathBuf::from(&data_dir).join("app_settings.json");

    // Callers write here with fs::write, which fails silently without the folder.
    if let Some(parent) = new_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // migrate if needed
    if old_path.exists() && !new_path.exists() && std::fs::copy(&old_path, &new_path).is_ok() {
        let _ = std::fs::remove_file(&old_path);
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
            pause_when_one_removed: false,
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
    use super::copy_old_app_dir;
    use std::fs;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("podbridge-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn old_folder_is_copied_once_and_kept() {
        let base = scratch("copy");
        let (old, new) = (base.join("librepods"), base.join("podbridge"));
        fs::create_dir_all(old.join("sub")).unwrap();
        fs::write(old.join("devices.json"), "{\"a\":1}").unwrap();
        fs::write(old.join("sub/x"), "x").unwrap();

        assert!(copy_old_app_dir(&old, &new).unwrap());
        assert_eq!(
            fs::read_to_string(new.join("devices.json")).unwrap(),
            "{\"a\":1}"
        );
        assert_eq!(fs::read_to_string(new.join("sub/x")).unwrap(), "x");
        assert!(old.join("devices.json").exists());

        // A later run never overwrites podbridge data with the old copy.
        fs::write(old.join("devices.json"), "{}").unwrap();
        assert!(!copy_old_app_dir(&old, &new).unwrap());
        assert_eq!(
            fs::read_to_string(new.join("devices.json")).unwrap(),
            "{\"a\":1}"
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn links_are_copied_as_links_and_temp_files_skipped() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let base = scratch("links");
        let (old, new) = (base.join("librepods"), base.join("podbridge"));
        fs::create_dir_all(old.join("dir")).unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o700)).unwrap();
        symlink("dir", old.join("to-dir")).unwrap();
        symlink("missing", old.join("dangling")).unwrap();
        fs::write(old.join("devices.json.tmp.1.0"), "partial").unwrap();

        assert!(copy_old_app_dir(&old, &new).unwrap());
        assert_eq!(
            fs::read_link(new.join("to-dir")).unwrap().to_str(),
            Some("dir")
        );
        assert_eq!(
            fs::read_link(new.join("dangling")).unwrap().to_str(),
            Some("missing")
        );
        assert!(!new.join("devices.json.tmp.1.0").exists());
        assert_eq!(
            fs::metadata(&new).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn nothing_happens_without_an_old_folder() {
        let base = scratch("none");
        let new = base.join("podbridge");
        assert!(!copy_old_app_dir(&base.join("librepods"), &new).unwrap());
        assert!(!new.exists());
        fs::remove_dir_all(base).unwrap();
    }
}
