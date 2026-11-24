use crate::bluetooth::aacp::{BatteryComponent, BatteryInfo, BatteryStatus};
use crate::devices::enums::{DeviceData, DeviceInformation, DeviceType};
use crate::devices::low_battery::notify;
use crate::ui::tray::MyTray;
use crate::utils::{ah, get_devices_path, get_preferences_path};
use aes::Aes128;
use aes::cipher::Array;
use aes::cipher::{BlockCipherDecrypt, KeyInit};
use bluer::monitor::{Monitor, MonitorEvent, Pattern};
use bluer::{Address, Session};
use futures::StreamExt;
use hex;
use log::{debug, info};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

fn decrypt(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(&Array::from(*key));
    let mut block = Array::from(*data);
    cipher.decrypt_block(&mut block);
    block.into()
}

fn verify_rpa(addr: &str, irk: &[u8; 16]) -> bool {
    let Ok(mut rpa) = addr
        .split(':')
        .map(|s| u8::from_str_radix(s, 16))
        .collect::<Result<Vec<u8>, _>>()
    else {
        return false;
    };
    rpa.reverse();
    if rpa.len() != 6 {
        return false;
    }
    let prand = [rpa[3], rpa[4], rpa[5]];
    let hash = [rpa[0], rpa[1], rpa[2]];
    let computed_hash = ah(irk, &prand);
    debug!(
        "Verifying RPA: addr={}, hash={:?}, computed_hash={:?}",
        addr, hash, computed_hash
    );
    hash == computed_hash
}

/// The case lid from a proximity pairing message (Apple manufacturer data
/// without the company ID): whether it is open, and its open counter. None
/// for other messages, and while this bud is out of the case: only a bud in
/// the case sees the lid (librepods' Linux client reads it the same way).
fn lid_state(apple_data: &[u8]) -> Option<(bool, u8)> {
    if apple_data.len() < 9 || apple_data[0] != 0x07 || apple_data[2] == 0x00 {
        return None;
    }
    if apple_data[5] & 0x40 == 0 {
        return None;
    }
    let lid = apple_data[8];
    Some((lid & 0x08 == 0, lid & 0x07))
}

/// One bud's view of the lid, to notice each opening once.
#[derive(Debug, Default)]
struct LidWatch {
    last: Option<(bool, u8)>,
}

impl LidWatch {
    /// True when this report opens the lid. A new open counter is a new
    /// opening too, since the closed state may never have been advertised.
    fn opened(&mut self, lid: Option<(bool, u8)>) -> bool {
        let Some((open, count)) = lid else {
            return false;
        };
        let new = open
            && self
                .last
                .is_none_or(|(was_open, was)| !was_open || was != count);
        self.last = lid;
        new
    }
}

// shortcut: a guessed window for both buds reporting one opening. A real
// reopening inside it is not shown. Upgrade: dedupe on the lid open counter
// once the live tests show both buds share it.
const LID_NOTICE_COOLDOWN: Duration = Duration::from_secs(10);

/// The last lid-open notification per AirPods, shared by their buds.
#[derive(Debug, Default)]
struct LidNotices {
    last: HashMap<String, Instant>,
}

impl LidNotices {
    fn due(&mut self, airpods_mac: &str, now: Instant) -> bool {
        if let Some(last) = self.last.get(airpods_mac)
            && now.duration_since(*last) < LID_NOTICE_COOLDOWN
        {
            return false;
        }
        self.last.insert(airpods_mac.to_string(), now);
        true
    }
}

/// Left, right and case battery from the decrypted advert payload. The
/// primary bud, status bit 5, comes first. 0xFF means not reported.
fn batteries(status: u8, decrypted: &[u8; 16]) -> [BatteryInfo; 3] {
    let primary_left = status & 0x20 != 0;
    let (left, right) = if primary_left { (1, 2) } else { (2, 1) };
    let info = |component, byte: u8| match byte {
        0xFF => BatteryInfo {
            component,
            level: 0,
            status: BatteryStatus::Disconnected,
        },
        _ => BatteryInfo {
            component,
            level: byte & 0x7F,
            status: if byte & 0x80 != 0 {
                BatteryStatus::Charging
            } else {
                BatteryStatus::NotCharging
            },
        },
    };
    [
        info(BatteryComponent::Left, decrypted[left]),
        info(BatteryComponent::Right, decrypted[right]),
        info(BatteryComponent::Case, decrypted[3]),
    ]
}

/// The lid-open notification text: each reported battery.
fn lid_open_body(batteries: &[BatteryInfo]) -> String {
    batteries
        .iter()
        .filter(|b| b.status != BatteryStatus::Disconnected)
        .map(|b| {
            let name = b.component.name();
            let charging = if b.status == BatteryStatus::Charging {
                " (charging)"
            } else {
                ""
            };
            format!("{name}: {}%{charging}", b.level)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Notify when this advert opens the case lid, once per opening across
/// both buds.
fn notice_lid_open(
    name: &str,
    airpods_mac: &str,
    apple_data: &[u8],
    decrypted: &[u8; 16],
    lid: &mut LidWatch,
    notices: &std::sync::Mutex<LidNotices>,
) {
    if !lid.opened(lid_state(apple_data)) {
        return;
    }
    let Ok(mut notices) = notices.lock() else {
        return;
    };
    if !notices.due(airpods_mac, Instant::now()) {
        return;
    }
    let body = lid_open_body(&batteries(apple_data[5], decrypted));
    info!("{} case opened: {}", name, body);
    let summary = name.to_string();
    tokio::task::spawn_blocking(move || notify(&summary, &body));
}

// None on a failed read or parse, e.g. mid-write, so the caller keeps its copy.
fn load_devices() -> Option<HashMap<String, DeviceData>> {
    let json = std::fs::read_to_string(get_devices_path()).ok()?;
    serde_json::from_str(&json).ok()
}

fn devices_modified() -> Option<std::time::SystemTime> {
    std::fs::metadata(get_devices_path())
        .and_then(|m| m.modified())
        .ok()
}

pub async fn start_le_monitor(tray_handle: Option<ksni::Handle<MyTray>>) -> bluer::Result<()> {
    let session = Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;

    let mut all_devices = load_devices().unwrap_or_default();
    let mut devices_mtime = devices_modified();

    let mut verified_macs: HashMap<Address, String> = HashMap::new();
    let mut failed_macs: HashSet<Address> = HashSet::new();
    let mut listening_macs: HashSet<Address> = HashSet::new();
    let connecting_macs = Arc::new(Mutex::new(HashSet::<Address>::new()));
    let lid_notices = Arc::new(std::sync::Mutex::new(LidNotices::default()));

    let pattern = Pattern {
        data_type: 0xFF, // Manufacturer specific data
        start_position: 0,
        content: vec![0x4C, 0x00], // Apple manufacturer ID (76) in LE
    };

    let mm = adapter.monitor().await?;
    let mut monitor_handle = mm
        .register(Monitor {
            monitor_type: bluer::monitor::Type::OrPatterns,
            rssi_low_threshold: None,
            rssi_high_threshold: None,
            rssi_low_timeout: None,
            rssi_high_timeout: None,
            rssi_sampling_period: None,
            patterns: Some(vec![pattern]),
            ..Default::default()
        })
        .await?;

    debug!("Started LE monitor");

    while let Some(mevt) = monitor_handle.next().await {
        if let MonitorEvent::DeviceFound(devid) = mevt {
            let adapter_monitor_clone = adapter.clone();
            let dev = adapter_monitor_clone.device(devid.device)?;
            let addr = dev.address();
            let addr_str = addr.to_string();

            let matched_airpods_mac: String;
            let mut matched_enc_key: Option<[u8; 16]> = None;

            if let Some(airpods_mac) = verified_macs.get(&addr) {
                matched_airpods_mac = airpods_mac.clone();
            } else {
                // LE keys are saved after the monitor starts, so reload devices.json when it changes.
                let mtime = devices_modified();
                if mtime != devices_mtime
                    && let Some(devices) = load_devices()
                {
                    devices_mtime = mtime;
                    all_devices = devices;
                    failed_macs.clear();
                }
                if failed_macs.contains(&addr) {
                    continue;
                }
                debug!("Checking RPA for device: {}", addr_str);
                let mut found_mac = None;
                for (airpods_mac, device_data) in &all_devices {
                    if device_data.type_ == DeviceType::AirPods
                        && let Some(DeviceInformation::AirPods(info)) = &device_data.information
                        && let Ok(irk_bytes) = hex::decode(&info.le_keys.irk)
                        && let Ok(irk) = <[u8; 16]>::try_from(irk_bytes.as_slice())
                    {
                        debug!("Verifying RPA {} for airpods MAC {}", addr_str, airpods_mac);
                        if verify_rpa(&addr_str, &irk) {
                            info!(
                                "Matched our device ({}) with the irk for {}",
                                addr, airpods_mac
                            );
                            verified_macs.insert(addr, airpods_mac.clone());
                            found_mac = Some(airpods_mac.clone());
                            break;
                        }
                    }
                }

                if let Some(mac) = found_mac {
                    matched_airpods_mac = mac;
                } else {
                    failed_macs.insert(addr);
                    debug!("Device {} did not match any of our irks", addr);
                    continue;
                }
            }

            if let Some(device_data) = all_devices.get(&matched_airpods_mac)
                && let Some(DeviceInformation::AirPods(info)) = &device_data.information
                && let Ok(enc_key_bytes) = hex::decode(&info.le_keys.enc_key)
                && let Ok(enc_key) = <[u8; 16]>::try_from(enc_key_bytes.as_slice())
            {
                matched_enc_key = Some(enc_key);
            }

            if listening_macs.contains(&addr) {
                continue;
            }
            listening_macs.insert(addr);
            let mut events = dev.events().await?;
            // The advert that announced this address raises no PropertyChanged.
            let first_data = dev.manufacturer_data().await.ok().flatten();
            let name = all_devices
                .get(&matched_airpods_mac)
                .map_or_else(|| "AirPods".to_string(), |d| d.name.clone());
            let tray_handle_clone = tray_handle.clone();
            let connecting_macs_clone = Arc::clone(&connecting_macs);
            let lid_notices = Arc::clone(&lid_notices);
            tokio::spawn(async move {
                let mut lid = LidWatch::default();
                if let Some(enc_key) = &matched_enc_key
                    && let Some(apple_data) = first_data.as_ref().and_then(|d| d.get(&76))
                    && apple_data.len() > 20
                    && let Some(last_16) = apple_data.last_chunk::<16>()
                {
                    notice_lid_open(
                        &name,
                        &matched_airpods_mac,
                        apple_data,
                        &decrypt(enc_key, last_16),
                        &mut lid,
                        &lid_notices,
                    );
                }
                while let Some(ev) = events.next().await {
                    match ev {
                        bluer::DeviceEvent::PropertyChanged(prop) => {
                            if let bluer::DeviceProperty::ManufacturerData(data) = prop
                                && let Some(enc_key) = &matched_enc_key
                                && let Some(apple_data) = data.get(&76)
                                && apple_data.len() > 20
                                && let Some(last_16) = apple_data.last_chunk::<16>()
                            {
                                let decrypted = decrypt(enc_key, last_16);
                                // Before auto-connect: its wait and its `continue` would
                                // delay or skip the notice.
                                notice_lid_open(
                                    &name,
                                    &matched_airpods_mac,
                                    apple_data,
                                    &decrypted,
                                    &mut lid,
                                    &lid_notices,
                                );

                                debug!(
                                    "Decrypted data from airpods_mac {}: {}",
                                    matched_airpods_mac,
                                    hex::encode(decrypted)
                                );

                                let connection_state = apple_data[10] as usize;
                                debug!("Connection state: {}", connection_state);
                                if connection_state == 0x00 {
                                    let pref_path = get_preferences_path();
                                    let preferences: HashMap<String, HashMap<String, bool>> =
                                        std::fs::read_to_string(&pref_path)
                                            .ok()
                                            .and_then(|s| serde_json::from_str(&s).ok())
                                            .unwrap_or_default();
                                    let auto_connect = preferences
                                        .get(&matched_airpods_mac)
                                        .and_then(|prefs| prefs.get("autoConnect"))
                                        .copied()
                                        .unwrap_or(true);
                                    debug!(
                                        "Auto-connect preference for {}: {}",
                                        matched_airpods_mac, auto_connect
                                    );
                                    if auto_connect {
                                        let real_address = addr;
                                        let mut cm = connecting_macs_clone.lock().await;
                                        if cm.contains(&real_address) {
                                            info!(
                                                "Already connecting to {}, skipping duplicate attempt.",
                                                matched_airpods_mac
                                            );
                                            continue;
                                        }
                                        cm.insert(real_address);
                                        // let adapter_clone = adapter_monitor_clone.clone();
                                        // let real_device = adapter_clone.device(real_address).unwrap();
                                        info!(
                                            "AirPods are disconnected, attempting to connect to {}",
                                            matched_airpods_mac
                                        );
                                        // if let Err(e) = real_device.connect().await {
                                        //     info!("Failed to connect to AirPods {}: {}", matched_airpods_mac, e);
                                        // } else {
                                        //     info!("Successfully connected to AirPods {}", matched_airpods_mac);
                                        // }
                                        // call bluetoothctl connect <mac> for now, I don't know why bluer connect isn't working
                                        let output = tokio::process::Command::new("bluetoothctl")
                                            .arg("connect")
                                            .arg(&matched_airpods_mac)
                                            .output()
                                            .await;
                                        let connected =
                                            matches!(&output, Ok(o) if o.status.success());
                                        match output {
                                            Ok(output) => {
                                                if output.status.success() {
                                                    info!(
                                                        "Successfully connected to AirPods {}",
                                                        matched_airpods_mac
                                                    );
                                                } else {
                                                    let stderr =
                                                        String::from_utf8_lossy(&output.stderr);
                                                    info!(
                                                        "Failed to connect to AirPods {}: {}",
                                                        matched_airpods_mac, stderr
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                info!(
                                                    "Failed to execute bluetoothctl to connect to AirPods {}: {}",
                                                    matched_airpods_mac, e
                                                );
                                            }
                                        }
                                        if connected {
                                            cm.remove(&real_address);
                                        } else {
                                            // Retry on a later advert, but not on
                                            // every one: they arrive constantly.
                                            drop(cm);
                                            let connecting = Arc::clone(&connecting_macs_clone);
                                            tokio::spawn(async move {
                                                tokio::time::sleep(std::time::Duration::from_secs(
                                                    30,
                                                ))
                                                .await;
                                                connecting.lock().await.remove(&real_address);
                                            });
                                        }
                                    } else {
                                        debug!(
                                            "Auto-connect is disabled for {}, not attempting to connect.",
                                            matched_airpods_mac
                                        );
                                    }
                                }

                                let status = apple_data[5] as usize;
                                let primary_left = (status >> 5) & 0x01 == 1;
                                let this_in_case = (status >> 6) & 0x01 == 1;
                                let xor_factor = primary_left ^ this_in_case;
                                let is_left_in_ear = if xor_factor {
                                    (status & 0x02) != 0
                                } else {
                                    (status & 0x08) != 0
                                };
                                let is_right_in_ear = if xor_factor {
                                    (status & 0x08) != 0
                                } else {
                                    (status & 0x02) != 0
                                };
                                let [left, right, case] = batteries(apple_data[5], &decrypted);

                                if let Some(handle) = &tray_handle_clone {
                                    let level = |b: &BatteryInfo| {
                                        (b.status != BatteryStatus::Disconnected).then_some(b.level)
                                    };
                                    let (l, r, c) = (left.clone(), right.clone(), case.clone());
                                    handle
                                        .update(move |tray: &mut MyTray| {
                                            tray.battery_l = level(&l);
                                            tray.battery_l_status = Some(l.status);
                                            tray.battery_r = level(&r);
                                            tray.battery_r_status = Some(r.status);
                                            tray.battery_c = level(&c);
                                            tray.battery_c_status = Some(c.status);
                                        })
                                        .await;
                                }

                                debug!(
                                    "Battery status: {:?}, InEar: L:{} R:{}",
                                    [left, right, case],
                                    is_left_in_ear,
                                    is_right_in_ear
                                );
                            }
                        }
                    }
                }
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The proximity pairing example from librepods' notes, with `status` and
    /// `lid` swapped in. Apple's company ID is already stripped.
    fn advert(status: u8, lid: u8) -> Vec<u8> {
        let mut data = vec![
            0x07, 0x12, 0x01, 0x0E, 0x20, status, 0xA7, 0xB3, lid, 0x02, 0x04,
        ];
        data.extend([0u8; 16]);
        data
    }

    const IN_CASE: u8 = 0x62;
    const OUT_OF_CASE: u8 = 0x22;

    #[test]
    fn lid_bit_three_clear_is_open() {
        assert_eq!(lid_state(&advert(IN_CASE, 0x01)), Some((true, 1)));
        assert_eq!(lid_state(&advert(IN_CASE, 0x09)), Some((false, 1)));
    }

    #[test]
    fn lid_unknown_unless_this_bud_is_in_the_case() {
        assert_eq!(lid_state(&advert(OUT_OF_CASE, 0x01)), None);
    }

    #[test]
    fn lid_unknown_for_other_messages() {
        let mut pairing = advert(IN_CASE, 0x01);
        pairing[2] = 0x00;
        assert_eq!(lid_state(&pairing), None);
        let mut other = advert(IN_CASE, 0x01);
        other[0] = 0x10;
        assert_eq!(lid_state(&other), None);
        assert_eq!(lid_state(&[0x07, 0x12, 0x01]), None);
    }

    #[test]
    fn notices_an_opening_once() {
        let mut w = LidWatch::default();
        assert!(w.opened(Some((true, 1))));
        assert!(!w.opened(Some((true, 1))));
        assert!(!w.opened(None));
        assert!(!w.opened(Some((false, 1))));
        assert!(w.opened(Some((true, 2))));
    }

    #[test]
    fn a_new_open_count_is_a_new_opening_even_if_the_close_was_missed() {
        let mut w = LidWatch::default();
        assert!(w.opened(Some((true, 3))));
        assert!(w.opened(Some((true, 4))));
    }

    #[test]
    fn both_buds_reporting_one_opening_notify_once() {
        let t = Instant::now();
        let mut n = LidNotices::default();
        assert!(n.due("AA", t));
        assert!(!n.due("AA", t + Duration::from_secs(1)));
        assert!(n.due("BB", t + Duration::from_secs(1)));
        assert!(n.due("AA", t + LID_NOTICE_COOLDOWN));
    }

    #[test]
    fn batteries_follow_the_primary_bud() {
        let mut dec = [0u8; 16];
        dec[1] = 0x55;
        dec[2] = 0xD0;
        dec[3] = 0xFF;
        let left_primary = batteries(IN_CASE, &dec);
        assert_eq!(
            left_primary,
            [
                BatteryInfo {
                    component: BatteryComponent::Left,
                    level: 85,
                    status: BatteryStatus::NotCharging
                },
                BatteryInfo {
                    component: BatteryComponent::Right,
                    level: 80,
                    status: BatteryStatus::Charging
                },
                BatteryInfo {
                    component: BatteryComponent::Case,
                    level: 0,
                    status: BatteryStatus::Disconnected
                },
            ]
        );
        let right_primary = batteries(IN_CASE & !0x20, &dec);
        assert_eq!(right_primary[0].level, 80);
        assert_eq!(right_primary[1].level, 85);
    }

    #[test]
    fn lid_open_body_lists_what_is_reported() {
        let mut dec = [0u8; 16];
        dec[1] = 0x55;
        dec[2] = 0xD0;
        dec[3] = 0x3C;
        assert_eq!(
            lid_open_body(&batteries(IN_CASE, &dec)),
            "Left: 85%, Right: 80% (charging), Case: 60%"
        );
        dec[3] = 0xFF;
        assert_eq!(
            lid_open_body(&batteries(IN_CASE, &dec)),
            "Left: 85%, Right: 80% (charging)"
        );
    }
}
