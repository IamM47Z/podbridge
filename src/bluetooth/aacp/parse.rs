//! Pure decoding of incoming AACP packets: no state, no I/O. The only side
//! effect is the per-field diagnostics (unknown battery component, unknown ear
//! status, key count) that receive_packet always logged mid-decode.

use super::{
    AudioSource, AudioSourceType, BatteryComponent, BatteryInfo, BatteryStatus, ConnectedDevice,
    ControlCommandIdentifiers, ControlCommandStatus, EarDetectionStatus, HEADER_BYTES,
    ProximityKey, StemPressBudType, StemPressType, opcodes,
};
use log::{debug, error, info};

/// One decoded AACP packet, one variant per opcode podbridge acts on.
#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    BatteryInfo(Vec<BatteryInfo>),
    ControlCommand(ControlCommandStatus),
    /// Primary bud first, then secondary.
    EarDetection(Vec<EarDetectionStatus>),
    ConversationAwareness(u8),
    /// The device-information strings, after dropping the first one, in
    /// `AirPodsInformation` field order.
    Information(Vec<String>),
    ProximityKeys(Vec<ProximityKey>),
    StemPress(StemPressType, StemPressBudType),
    AudioSource(AudioSource),
    ConnectedDevices(Vec<ConnectedDevice>),
    /// The response text, decoded lossily as UTF-8.
    SmartRoutingResponse(String),
    EqData,
    Unknown {
        opcode: u8,
    },
}

/// Why a packet was skipped. `log` reproduces the message receive_packet wrote.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Does not start with the 04 00 04 00 header.
    BadHeader,
    /// Header only, no opcode.
    TooShort,
    /// Shorter than the fixed fields of its opcode.
    Truncated {
        opcode: u8,
    },
    /// Shorter than the entry count it announces.
    LengthMismatch {
        opcode: u8,
    },
    /// Not the one length its opcode allows.
    UnexpectedLength {
        opcode: u8,
        len: usize,
    },
    /// A proximity key header runs past the end.
    KeysTruncated,
    /// Proximity key data runs past the end.
    KeyDataTruncated,
    UnknownControlCommand(u8),
    InvalidStemPress(Option<StemPressType>, Option<StemPressBudType>),
}

impl ParseError {
    /// Log why `packet` was skipped, at the level and with the text
    /// receive_packet always used.
    pub fn log(&self, packet: &[u8]) {
        let payload = packet.get(4..).unwrap_or_default();
        match self {
            Self::BadHeader => debug!(
                "Received packet does not start with expected header: {}",
                hex::encode(packet)
            ),
            Self::TooShort => debug!("Received packet too short: {}", hex::encode(packet)),
            // Ear detection has always logged the whole packet, the rest the payload.
            Self::Truncated {
                opcode: opcodes::EAR_DETECTION,
            } => error!("Ear Detection packet too short: {}", hex::encode(packet)),
            Self::Truncated { opcode } => error!(
                "{} packet too short: {}",
                opcode_name(*opcode),
                hex::encode(payload)
            ),
            Self::LengthMismatch { opcode } => error!(
                "{} packet length mismatch: {}",
                opcode_name(*opcode),
                hex::encode(payload)
            ),
            Self::UnexpectedLength { opcode, len } => info!(
                "Received {} packet with unexpected length: {}",
                opcode_name(*opcode),
                len
            ),
            Self::KeysTruncated => error!(
                "Proximity Keys Response packet too short while parsing keys ({} bytes)",
                payload.len()
            ),
            Self::KeyDataTruncated => error!(
                "Proximity Keys Response packet too short for key data ({} bytes)",
                payload.len()
            ),
            Self::UnknownControlCommand(identifier) => {
                debug!("Unknown Control Command identifier: {:#04x}", identifier)
            }
            Self::InvalidStemPress(press, bud) => error!(
                "Invalid Stem Press packet - type: {:?}, bud: {:?}",
                press, bud
            ),
        }
    }
}

fn opcode_name(opcode: u8) -> &'static str {
    match opcode {
        opcodes::BATTERY_INFO => "Battery Info",
        opcodes::CONTROL_COMMAND => "Control Command",
        opcodes::EAR_DETECTION => "Ear Detection",
        opcodes::CONVERSATION_AWARENESS => "Conversation Awareness",
        opcodes::INFORMATION => "Information",
        opcodes::PROXIMITY_KEYS_RSP => "Proximity Keys Response",
        opcodes::STEM_PRESS => "Stem Press",
        opcodes::AUDIO_SOURCE => "Audio Source",
        opcodes::CONNECTED_DEVICES => "Connected Devices",
        opcodes::SMART_ROUTING_RESP => "Smart Routing",
        _ => "Unknown",
    }
}

/// Decode one AACP packet, header included.
pub fn parse(packet: &[u8]) -> Result<Packet, ParseError> {
    if !packet.starts_with(&HEADER_BYTES) {
        return Err(ParseError::BadHeader);
    }
    let Some(&opcode) = packet.get(4) else {
        return Err(ParseError::TooShort);
    };
    // The payload starts at the opcode byte, so payload[2] is the first byte
    // after the opcode's 0x00 companion.
    let payload = &packet[4..];
    let truncated = Err(ParseError::Truncated { opcode });

    match opcode {
        opcodes::BATTERY_INFO => {
            if payload.len() < 3 {
                return truncated;
            }
            let count = payload[2] as usize;
            if payload.len() < 3 + count * 5 {
                return Err(ParseError::LengthMismatch { opcode });
            }
            let mut batteries = Vec::with_capacity(count);
            // [component] 01 [level] [status] 01
            for &[component, _, level, status, _] in payload[3..3 + count * 5].as_chunks::<5>().0 {
                let component = match component {
                    0x01 => BatteryComponent::Headphone,
                    0x02 => BatteryComponent::Right,
                    0x04 => BatteryComponent::Left,
                    0x08 => BatteryComponent::Case,
                    _ => {
                        error!("Unknown battery component: {:#04x}", component);
                        continue;
                    }
                };
                let status = match status {
                    0x01 => BatteryStatus::Charging,
                    0x02 => BatteryStatus::NotCharging,
                    0x04 => BatteryStatus::Disconnected,
                    _ => {
                        error!("Unknown battery status: {:#04x}", status);
                        continue;
                    }
                };
                batteries.push(BatteryInfo {
                    component,
                    level,
                    status,
                });
            }
            Ok(Packet::BatteryInfo(batteries))
        }
        opcodes::CONTROL_COMMAND => {
            if payload.len() < 7 {
                return truncated;
            }
            let identifier_byte = payload[2];
            let value_bytes = &payload[3..7];
            let value = match value_bytes.iter().rposition(|&b| b != 0) {
                Some(i) => value_bytes[..=i].to_vec(),
                None => vec![0],
            };
            let identifier = ControlCommandIdentifiers::from_u8(identifier_byte)
                .ok_or(ParseError::UnknownControlCommand(identifier_byte))?;
            Ok(Packet::ControlCommand(ControlCommandStatus {
                identifier,
                value,
            }))
        }
        opcodes::EAR_DETECTION => {
            if packet.len() < 8 {
                return truncated;
            }
            let status = |byte: u8| match byte {
                0x00 => EarDetectionStatus::InEar,
                0x01 => EarDetectionStatus::OutOfEar,
                0x02 => EarDetectionStatus::InCase,
                0x03 => EarDetectionStatus::Disconnected,
                _ => {
                    error!("Unknown ear detection status: {:#04x}", byte);
                    EarDetectionStatus::OutOfEar
                }
            };
            Ok(Packet::EarDetection(vec![
                status(packet[6]),
                status(packet[7]),
            ]))
        }
        opcodes::CONVERSATION_AWARENESS => {
            if packet.len() != 10 {
                return Err(ParseError::UnexpectedLength {
                    opcode,
                    len: packet.len(),
                });
            }
            Ok(Packet::ConversationAwareness(packet[9]))
        }
        opcodes::INFORMATION => {
            if payload.len() < 6 {
                return truncated;
            }
            Ok(Packet::Information(information_strings(&payload[4..])))
        }
        opcodes::PROXIMITY_KEYS_RSP => {
            if payload.len() < 4 {
                return truncated;
            }
            let key_count = payload[2] as usize;
            debug!("Proximity Keys Response contains {} keys.", key_count);
            let mut offset = 3;
            let mut keys = Vec::new();
            for _ in 0..key_count {
                // [type] 00 [length] 00 [data]
                if offset + 3 >= payload.len() {
                    return Err(ParseError::KeysTruncated);
                }
                let key_type = payload[offset];
                let key_length = payload[offset + 2] as usize;
                offset += 4;
                if offset + key_length > payload.len() {
                    return Err(ParseError::KeyDataTruncated);
                }
                keys.push(ProximityKey {
                    key_type,
                    data: payload[offset..offset + key_length].to_vec(),
                });
                offset += key_length;
            }
            Ok(Packet::ProximityKeys(keys))
        }
        opcodes::STEM_PRESS => {
            if payload.len() < 4 {
                return truncated;
            }
            match (
                StemPressType::from_u8(payload[2]),
                StemPressBudType::from_u8(payload[3]),
            ) {
                (Some(press), Some(bud)) => Ok(Packet::StemPress(press, bud)),
                (press, bud) => Err(ParseError::InvalidStemPress(press, bud)),
            }
        }
        opcodes::AUDIO_SOURCE => {
            if payload.len() < 9 {
                return truncated;
            }
            // The MAC is sent least significant byte first.
            let mac = format!(
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                payload[7], payload[6], payload[5], payload[4], payload[3], payload[2]
            );
            let r#type = AudioSourceType::from_u8(payload[8]).unwrap_or(AudioSourceType::None);
            Ok(Packet::AudioSource(AudioSource { mac, r#type }))
        }
        opcodes::CONNECTED_DEVICES => {
            if payload.len() < 5 {
                return truncated;
            }
            let count = payload[4] as usize;
            // Entries start at payload[5], right after the count.
            if payload.len() < 5 + count * 8 {
                return Err(ParseError::LengthMismatch { opcode });
            }
            let devices = payload[5..5 + count * 8]
                .as_chunks::<8>()
                .0
                .iter()
                .map(|&[m0, m1, m2, m3, m4, m5, info1, info2]| ConnectedDevice {
                    mac: format!(
                        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                        m0, m1, m2, m3, m4, m5
                    ),
                    info1,
                    info2,
                    r#type: None,
                })
                .collect();
            Ok(Packet::ConnectedDevices(devices))
        }
        opcodes::SMART_ROUTING_RESP => {
            if payload.len() < 2 {
                return truncated;
            }
            Ok(Packet::SmartRoutingResponse(
                String::from_utf8_lossy(&payload[2..]).into_owned(),
            ))
        }
        opcodes::EQ_DATA => Ok(Packet::EqData),
        _ => Ok(Packet::Unknown { opcode }),
    }
}

/// Split the information data into its NUL-separated strings: skip the
/// leading run up to the first NUL, drop invalid UTF-8, then drop the first
/// string.
fn information_strings(data: &[u8]) -> Vec<String> {
    let start = data.iter().position(|&b| b == 0x00).unwrap_or(data.len());
    let mut strings: Vec<String> = data[start..]
        .split(|&b| b == 0x00)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok())
        .map(str::to_string)
        .collect();
    if !strings.is_empty() {
        strings.remove(0);
    }
    strings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(body: &[u8]) -> Vec<u8> {
        [[0x04, 0x00, 0x04, 0x00].as_slice(), body].concat()
    }

    fn parse_body(body: &[u8]) -> Result<Packet, ParseError> {
        parse(&frame(body))
    }

    #[test]
    fn header_is_required() {
        assert_eq!(parse(&[]), Err(ParseError::BadHeader));
        assert_eq!(parse(&[0x04, 0x00, 0x04]), Err(ParseError::BadHeader));
        assert_eq!(
            parse(&[0x05, 0x00, 0x04, 0x00, 0x04, 0x00]),
            Err(ParseError::BadHeader)
        );
        assert_eq!(parse(&[0x04, 0x00, 0x04, 0x00]), Err(ParseError::TooShort));
    }

    #[test]
    fn battery_doc_example() {
        // AAP Definitions example. The doc's prose calls 0x02 "Left", but its
        // table and podbridge map 0x02 to Right.
        let p = parse_body(&[
            0x04, 0x00, 0x03, 0x02, 0x01, 0x64, 0x02, 0x01, 0x04, 0x01, 0x63, 0x01, 0x01, 0x08,
            0x01, 0x11, 0x02, 0x01,
        ]);
        assert_eq!(
            p,
            Ok(Packet::BatteryInfo(vec![
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
            ]))
        );
    }

    #[test]
    fn battery_skips_unknown_entries() {
        // Unknown component, unknown status (0x00 "Unknown" in the doc), then
        // a headphone entry that is kept.
        let p = parse_body(&[
            0x04, 0x00, 0x03, 0x10, 0x01, 0x50, 0x01, 0x01, 0x04, 0x01, 0x50, 0x00, 0x01, 0x01,
            0x01, 0x2A, 0x04, 0x01,
        ]);
        assert_eq!(
            p,
            Ok(Packet::BatteryInfo(vec![BatteryInfo {
                component: BatteryComponent::Headphone,
                level: 42,
                status: BatteryStatus::Disconnected,
            }]))
        );
        assert_eq!(
            parse_body(&[0x04, 0x00, 0x00]),
            Ok(Packet::BatteryInfo(vec![]))
        );
    }

    #[test]
    fn battery_rejects_short_packets() {
        let truncated = Err(ParseError::Truncated {
            opcode: opcodes::BATTERY_INFO,
        });
        assert_eq!(parse_body(&[0x04]), truncated);
        assert_eq!(parse_body(&[0x04, 0x00]), truncated);
        assert_eq!(
            parse_body(&[0x04, 0x00, 0x01, 0x02, 0x01, 0x64, 0x02]),
            Err(ParseError::LengthMismatch {
                opcode: opcodes::BATTERY_INFO
            })
        );
        // The trailing 0x01 of the last entry is not required.
        assert!(matches!(
            parse_body(&[0x04, 0x00, 0x01, 0x02, 0x01, 0x64, 0x02, 0x01]),
            Ok(Packet::BatteryInfo(b)) if b.len() == 1
        ));
    }

    fn control(identifier: ControlCommandIdentifiers, value: &[u8]) -> Result<Packet, ParseError> {
        Ok(Packet::ControlCommand(ControlCommandStatus {
            identifier,
            value: value.to_vec(),
        }))
    }

    #[test]
    fn control_command_trims_trailing_zeros() {
        // Noise control, Transparency.
        assert_eq!(
            parse_body(&[0x09, 0x00, 0x0D, 0x03, 0x00, 0x00, 0x00]),
            control(ControlCommandIdentifiers::ListeningMode, &[0x03])
        );
        // Conversational awareness state, the doc example.
        assert_eq!(
            parse_body(&[0x09, 0x00, 0x28, 0x01, 0x00, 0x00, 0x00]),
            control(ControlCommandIdentifiers::ConversationDetectConfig, &[0x01])
        );
        assert_eq!(
            parse_body(&[0x09, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00]),
            control(ControlCommandIdentifiers::OwnsConnection, &[0x00])
        );
        assert_eq!(
            parse_body(&[0x09, 0x00, 0x1A, 0x01, 0x00, 0x02, 0x00]),
            control(
                ControlCommandIdentifiers::ListeningModeConfigs,
                &[0x01, 0x00, 0x02]
            )
        );
        // Bytes past the 4-byte value are ignored.
        assert_eq!(
            parse_body(&[0x09, 0x00, 0x2E, 0x01, 0x02, 0x03, 0x04, 0x05]),
            control(
                ControlCommandIdentifiers::AutoAncStrength,
                &[0x01, 0x02, 0x03, 0x04]
            )
        );
    }

    #[test]
    fn control_command_rejects_bad_packets() {
        assert_eq!(
            parse_body(&[0x09, 0x00, 0x0D, 0x03, 0x00, 0x00]),
            Err(ParseError::Truncated {
                opcode: opcodes::CONTROL_COMMAND
            })
        );
        assert_eq!(
            parse_body(&[0x09, 0x00, 0x7F, 0x03, 0x00, 0x00, 0x00]),
            Err(ParseError::UnknownControlCommand(0x7F))
        );
    }

    #[test]
    fn ear_detection() {
        use EarDetectionStatus::*;
        assert_eq!(
            parse_body(&[0x06, 0x00, 0x00, 0x01]),
            Ok(Packet::EarDetection(vec![InEar, OutOfEar]))
        );
        assert_eq!(
            parse_body(&[0x06, 0x00, 0x02, 0x03]),
            Ok(Packet::EarDetection(vec![InCase, Disconnected]))
        );
        // An unknown status reads as out of ear.
        assert_eq!(
            parse_body(&[0x06, 0x00, 0x09, 0x00]),
            Ok(Packet::EarDetection(vec![OutOfEar, InEar]))
        );
        assert_eq!(
            parse_body(&[0x06, 0x00, 0x00]),
            Err(ParseError::Truncated {
                opcode: opcodes::EAR_DETECTION
            })
        );
    }

    #[test]
    fn conversation_awareness_needs_exact_length() {
        assert_eq!(
            parse_body(&[0x4B, 0x00, 0x02, 0x00, 0x01, 0x03]),
            Ok(Packet::ConversationAwareness(0x03))
        );
        for len in [5, 7] {
            let body = [0x4B, 0x00, 0x02, 0x00, 0x01, 0x03, 0x00][..len].to_vec();
            assert_eq!(
                parse_body(&body),
                Err(ParseError::UnexpectedLength {
                    opcode: opcodes::CONVERSATION_AWARENESS,
                    len: len + 4,
                })
            );
        }
    }

    #[test]
    fn information_doc_example() {
        // The Metadata example from AAP Definitions.
        let packet = hex::decode(concat!(
            "040004001d0002d5000400416972506f64732050726f004133303438004170706c6520496e632e00",
            "51584e524848595850360036312e313836383034303030323030303030302e323731330036312e31",
            "3836383034303030323030303030302e3237313300312e302e3000636f6d2e6170706c652e616363",
            "6573736f72792e757064617465722e6170702e3731004859394c5432454632364a59004833504c57",
            "48444a32364b3000363335373533360089312a6567a5400f84a3ca234947efd40b90d78436ae5946",
            "748d70273e66066a2589300035333935303630363400",
        ))
        .unwrap();
        let Ok(Packet::Information(strings)) = parse(&packet) else {
            panic!("{:?}", parse(&packet));
        };
        assert_eq!(
            strings[..11],
            [
                "AirPods Pro",
                "A3048",
                "Apple Inc.",
                "QXNRHHYXP6",
                "61.1868040002000000.2713",
                "61.1868040002000000.2713",
                "1.0.0",
                "com.apple.accessory.updater.app.71",
                "HY9LT2EF26JY",
                "H3PLWHDJ26K0",
                "6357536",
            ]
        );
    }

    #[test]
    fn information_string_splitting() {
        // payload[4..] starts with a run that is skipped up to the first NUL;
        // the first string after it is dropped; invalid UTF-8 is skipped.
        assert_eq!(
            parse_body(&[
                0x1D, 0x00, 0x00, 0x00, b'x', 0x00, 0x00, b'd', 0x00, b'a', 0x00, 0x00, 0xFF, 0x00,
                b'b',
            ]),
            Ok(Packet::Information(vec!["a".to_string(), "b".to_string()]))
        );
        assert_eq!(
            parse_body(&[0x1D, 0x00, 0x00, 0x00, 0x00, 0x00]),
            Ok(Packet::Information(vec![]))
        );
        assert_eq!(
            parse_body(&[0x1D, 0x00, 0x00, 0x00, 0x00]),
            Err(ParseError::Truncated {
                opcode: opcodes::INFORMATION
            })
        );
    }

    #[test]
    fn proximity_keys() {
        // count 2: IRK (type 1, 3 bytes), encryption key (type 4, 2 bytes).
        assert_eq!(
            parse_body(&[
                0x31, 0x00, 0x02, 0x01, 0x00, 0x03, 0x00, 0xA1, 0xA2, 0xA3, 0x04, 0x00, 0x02, 0x00,
                0xB1, 0xB2,
            ]),
            Ok(Packet::ProximityKeys(vec![
                ProximityKey {
                    key_type: 0x01,
                    data: vec![0xA1, 0xA2, 0xA3],
                },
                ProximityKey {
                    key_type: 0x04,
                    data: vec![0xB1, 0xB2],
                },
            ]))
        );
        assert_eq!(
            parse_body(&[0x31, 0x00, 0x00, 0x00]),
            Ok(Packet::ProximityKeys(vec![]))
        );
    }

    #[test]
    fn proximity_keys_reject_truncation() {
        assert_eq!(
            parse_body(&[0x31, 0x00, 0x01]),
            Err(ParseError::Truncated {
                opcode: opcodes::PROXIMITY_KEYS_RSP
            })
        );
        assert_eq!(
            parse_body(&[0x31, 0x00, 0x01, 0x01, 0x00, 0x03]),
            Err(ParseError::KeysTruncated)
        );
        assert_eq!(
            parse_body(&[0x31, 0x00, 0x01, 0x01, 0x00, 0x03, 0x00, 0xA1, 0xA2]),
            Err(ParseError::KeyDataTruncated)
        );
        // The second key's header is missing.
        assert_eq!(
            parse_body(&[0x31, 0x00, 0x02, 0x01, 0x00, 0x01, 0x00, 0xA1]),
            Err(ParseError::KeysTruncated)
        );
    }

    #[test]
    fn proximity_key_debug_hides_key_material() {
        let key = ProximityKey {
            key_type: 0x01,
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };
        let shown = format!("{:?}", Packet::ProximityKeys(vec![key]));
        assert!(!shown.contains("222") && !shown.contains("173"), "{shown}");
        assert!(shown.contains("4 bytes"), "{shown}");
    }

    #[test]
    fn stem_press() {
        assert_eq!(
            parse_body(&[0x19, 0x00, 0x05, 0x01]),
            Ok(Packet::StemPress(
                StemPressType::Single,
                StemPressBudType::Left
            ))
        );
        assert_eq!(
            parse_body(&[0x19, 0x00, 0x08, 0x02]),
            Ok(Packet::StemPress(
                StemPressType::Long,
                StemPressBudType::Right
            ))
        );
        assert_eq!(
            parse_body(&[0x19, 0x00, 0x07, 0x03]),
            Err(ParseError::InvalidStemPress(
                Some(StemPressType::Triple),
                None
            ))
        );
        assert_eq!(
            parse_body(&[0x19, 0x00, 0x04, 0x01]),
            Err(ParseError::InvalidStemPress(
                None,
                Some(StemPressBudType::Left)
            ))
        );
        assert_eq!(
            parse_body(&[0x19, 0x00, 0x05]),
            Err(ParseError::Truncated {
                opcode: opcodes::STEM_PRESS
            })
        );
    }

    #[test]
    fn audio_source() {
        assert_eq!(
            parse_body(&[0x0E, 0x00, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x01]),
            Ok(Packet::AudioSource(AudioSource {
                mac: "11:22:33:44:55:66".to_string(),
                r#type: AudioSourceType::Call,
            }))
        );
        // An unknown type reads as None.
        assert_eq!(
            parse_body(&[0x0E, 0x00, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x09]),
            Ok(Packet::AudioSource(AudioSource {
                mac: "FF:EE:DD:CC:BB:AA".to_string(),
                r#type: AudioSourceType::None,
            }))
        );
        assert_eq!(
            parse_body(&[0x0E, 0x00, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]),
            Err(ParseError::Truncated {
                opcode: opcodes::AUDIO_SOURCE
            })
        );
    }

    #[test]
    fn connected_devices() {
        // Packets captured from AirPods Pro, with made-up addresses; payload
        // after the 04000400 header.
        let dev = |mac: &str, info1, info2| ConnectedDevice {
            mac: mac.to_string(),
            info1,
            info2,
            r#type: None,
        };
        let one = hex::decode("2e000100010200000000010102").unwrap();
        assert_eq!(
            parse_body(&one),
            Ok(Packet::ConnectedDevices(vec![dev(
                "02:00:00:00:00:01",
                0x01,
                0x02
            )]))
        );
        let two = hex::decode("2e0001000202000000000201040200000000010002").unwrap();
        assert_eq!(
            parse_body(&two),
            Ok(Packet::ConnectedDevices(vec![
                dev("02:00:00:00:00:02", 0x01, 0x04),
                dev("02:00:00:00:00:01", 0x00, 0x02),
            ]))
        );
        let three =
            hex::decode("2e00010003020000000002000502000000000300010200000000010202").unwrap();
        assert_eq!(
            parse_body(&three),
            Ok(Packet::ConnectedDevices(vec![
                dev("02:00:00:00:00:02", 0x00, 0x05),
                dev("02:00:00:00:00:03", 0x00, 0x01),
                dev("02:00:00:00:00:01", 0x02, 0x02),
            ]))
        );
        assert_eq!(
            parse_body(&[0x2E, 0x00, 0x01, 0x00, 0x00]),
            Ok(Packet::ConnectedDevices(vec![]))
        );
        // The count byte itself is required.
        assert_eq!(
            parse_body(&[0x2E, 0x00, 0x01, 0x00]),
            Err(ParseError::Truncated {
                opcode: opcodes::CONNECTED_DEVICES
            })
        );
        // One entry needs 5 + 8 payload bytes; every shorter packet is an error.
        for len in 5..13 {
            let mut body = vec![0x2E, 0x00, 0x01, 0x00, 0x01];
            body.resize(len, 0xAA);
            assert_eq!(
                parse_body(&body),
                Err(ParseError::LengthMismatch {
                    opcode: opcodes::CONNECTED_DEVICES
                })
            );
        }
    }

    #[test]
    fn smart_routing_response() {
        let mut body = vec![0x11, 0x00];
        body.extend_from_slice(b"audioRoutingSetOwnershipToFalse");
        body.push(0xFF);
        assert_eq!(
            parse_body(&body),
            Ok(Packet::SmartRoutingResponse(
                "audioRoutingSetOwnershipToFalse\u{FFFD}".to_string()
            ))
        );
        assert_eq!(
            parse_body(&[0x11, 0x00]),
            Ok(Packet::SmartRoutingResponse(String::new()))
        );
        assert_eq!(
            parse_body(&[0x11]),
            Err(ParseError::Truncated {
                opcode: opcodes::SMART_ROUTING_RESP
            })
        );
    }

    #[test]
    fn eq_data_and_unknown_opcodes() {
        // Headphone accommodation example from AAP Definitions.
        assert_eq!(
            parse_body(&[0x53, 0x00, 0x84, 0x00, 0x02, 0x02]),
            Ok(Packet::EqData)
        );
        assert_eq!(parse_body(&[0x53]), Ok(Packet::EqData));
        for opcode in [0x77, opcodes::SET_FEATURE_FLAGS, 0x17, 0x58] {
            assert_eq!(parse_body(&[opcode, 0x00]), Ok(Packet::Unknown { opcode }));
        }
    }
}
