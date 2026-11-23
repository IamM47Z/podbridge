use crate::bluetooth::att::{ATTHandles, ATTManager};
use crate::devices::enums::{DeviceData, DeviceInformation, DeviceType};
use crate::utils::update_devices;
use bluer::Address;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NothingInformation {
    pub serial_number: String,
    pub firmware_version: String,
}

pub struct NothingDevice {
    pub att_manager: ATTManager,
}

impl NothingDevice {
    pub async fn new(mac_address: Address) -> bluer::Result<Self> {
        let mut att_manager = ATTManager::new();
        att_manager.connect(mac_address).await?;

        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

        att_manager
            .register_listener(ATTHandles::NothingEverythingRead, tx)
            .await;

        // Request version information
        att_manager
            .write(
                ATTHandles::NothingEverything,
                &[
                    0x55, 0x20, 0x01, 0x42, 0xC0, 0x00, 0x00, 0x00, 0x00,
                    0x00, // something, idk
                ],
            )
            .await
            .unwrap_or_else(|e| log::error!("Failed to write to Nothing device: {}", e));

        sleep(Duration::from_millis(100)).await;

        // Request serial number
        att_manager
            .write(
                ATTHandles::NothingEverything,
                &[0x55, 0x20, 0x01, 0x06, 0xC0, 0x00, 0x00, 0x13, 0x00, 0x00],
            )
            .await
            .unwrap_or_else(|e| log::error!("Failed to write to Nothing device: {}", e));

        let device_key = mac_address.to_string();
        tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                debug!(
                    "Received data from (Nothing) device {}, data: {:?}",
                    mac_address, data
                );
                let Some(reply) = parse_reply(&data) else {
                    continue;
                };
                info!("Nothing device {}: {:?}", mac_address, reply);
                let key = device_key.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    update_devices(|devices| {
                        apply_reply(devices, &key, reply);
                        true
                    })
                })
                .await;
            }
        });

        Ok(NothingDevice { att_manager })
    }
}

const FIRMWARE_REPLY: [u8; 5] = [0x55, 0x20, 0x01, 0x42, 0x40];
const SERIAL_REPLY: [u8; 5] = [0x55, 0x20, 0x01, 0x06, 0x40];
/// Both replies carry their payload after an 8-byte header.
const REPLY_HEADER: usize = 8;

#[derive(Debug)]
enum Reply {
    Firmware(String),
    Serial(String),
}

fn parse_reply(data: &[u8]) -> Option<Reply> {
    if data.starts_with(&FIRMWARE_REPLY) {
        let version = data.get(REPLY_HEADER..)?;
        return Some(Reply::Firmware(
            String::from_utf8_lossy(version).into_owned(),
        ));
    }
    if data.starts_with(&SERIAL_REPLY) {
        // The serial starts with "SH" and ends at a newline.
        let start = data.iter().position(|&b| b == b'S').unwrap_or(REPLY_HEADER);
        if data.get(start + 1) != Some(&b'H') {
            debug!(
                "Serial number format unexpected from Nothing device: {:?}",
                data
            );
            return None;
        }
        let end = data[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(data.len(), |pos| pos + start);
        return Some(Reply::Serial(
            String::from_utf8_lossy(&data[start..end]).into_owned(),
        ));
    }
    None
}

/// Store a reply in this device's entry, keeping every other field.
fn apply_reply(devices: &mut HashMap<String, DeviceData>, key: &str, reply: Reply) {
    let device = devices
        .entry(key.to_string())
        .or_insert_with(|| DeviceData {
            name: "Nothing Device".to_string(),
            type_: DeviceType::Nothing,
            information: None,
        });
    let mut info = match device.information.take() {
        Some(DeviceInformation::Nothing(info)) => info,
        _ => NothingInformation {
            serial_number: String::new(),
            firmware_version: String::new(),
        },
    };
    match reply {
        Reply::Firmware(v) => info.firmware_version = v,
        Reply::Serial(s) => info.serial_number = s,
    }
    device.information = Some(DeviceInformation::Nothing(info));
}

#[cfg(test)]
mod tests {
    use super::*;

    const FW: [u8; 5] = [0x55, 0x20, 0x01, 0x42, 0x40];
    const SN: [u8; 5] = [0x55, 0x20, 0x01, 0x06, 0x40];

    #[test]
    fn short_firmware_replies_are_ignored() {
        for len in 5..8 {
            let mut data = FW.to_vec();
            data.resize(len, 0);
            assert!(parse_reply(&data).is_none(), "len {len}");
        }
    }

    #[test]
    fn firmware_reply_is_read_after_its_header() {
        let mut data = FW.to_vec();
        data.extend([0, 0, 0]);
        data.extend(b"1.0.2");
        assert!(matches!(parse_reply(&data), Some(Reply::Firmware(v)) if v == "1.0.2"));
    }

    #[test]
    fn serial_reply_runs_to_newline() {
        let mut data = SN.to_vec();
        data.extend([0, 0, 0]);
        data.extend(b"SH123\nrest");
        assert!(matches!(parse_reply(&data), Some(Reply::Serial(s)) if s == "SH123"));
        let mut bad = SN.to_vec();
        bad.extend(b"XY");
        assert!(parse_reply(&bad).is_none());
    }

    #[test]
    fn replies_update_only_their_own_field_of_the_current_file() {
        let mut devices = HashMap::new();
        devices.insert(
            "other".to_string(),
            DeviceData {
                name: "Pods".into(),
                type_: DeviceType::AirPods,
                information: None,
            },
        );
        apply_reply(&mut devices, "aa", Reply::Firmware("1.0".into()));
        // Someone renamed the device between the two replies.
        devices.get_mut("aa").unwrap().name = "Ear (2)".into();
        apply_reply(&mut devices, "aa", Reply::Serial("SH1".into()));

        let d = &devices["aa"];
        assert_eq!(d.name, "Ear (2)");
        let Some(DeviceInformation::Nothing(info)) = &d.information else {
            panic!("no Nothing information");
        };
        assert_eq!(
            (info.firmware_version.as_str(), info.serial_number.as_str()),
            ("1.0", "SH1")
        );
        assert!(devices.contains_key("other"));
    }
}
