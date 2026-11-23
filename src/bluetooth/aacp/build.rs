//! Pure builders for the outgoing AACP packets that carry arguments. Each
//! returns the data after the 04 00 04 00 header, which send_data_packet adds.

use super::{ControlCommandIdentifiers, ProximityKeyType, opcodes};

/// A MAC address string that is not colon-separated hex.
#[derive(Debug, PartialEq, Eq)]
pub struct InvalidMac(pub String);

impl std::fmt::Display for InvalidMac {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid MAC address: {:?}", self.0)
    }
}

/// The MAC's bytes, last first, as the smart-routing packets carry them.
fn reversed_mac(mac: &str) -> Result<Vec<u8>, InvalidMac> {
    let address: bluer::Address = mac.parse().map_err(|_| InvalidMac(mac.to_string()))?;
    let mut bytes = address.0.to_vec();
    bytes.reverse();
    Ok(bytes)
}

fn smart_routing(buffer: &[u8]) -> Vec<u8> {
    [[opcodes::SMART_ROUTING, 0x00].as_slice(), buffer].concat()
}

pub fn build_proximity_keys_request(key_types: &[ProximityKeyType]) -> Vec<u8> {
    vec![
        opcodes::PROXIMITY_KEYS_REQ,
        0x00,
        key_types.iter().fold(0u8, |acc, kt| acc | (*kt as u8)),
        0x00,
    ]
}

/// A name longer than the one-byte length allows is cut at a char boundary.
pub fn build_rename(name: &str) -> Vec<u8> {
    let mut end = name.len().min(u8::MAX as usize);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let name_bytes = &name.as_bytes()[..end];
    let size = name_bytes.len();
    let mut packet = Vec::with_capacity(6 + size);
    packet.push(opcodes::RENAME);
    packet.push(0x00);
    packet.push(0x01);
    packet.push(size as u8);
    packet.push(0x00);
    packet.extend_from_slice(name_bytes);
    packet
}

/// The value is zero-padded or cut to the four bytes the packet carries.
pub fn build_control_command(identifier: ControlCommandIdentifiers, value: &[u8]) -> Vec<u8> {
    let mut packet = vec![opcodes::CONTROL_COMMAND, 0x00, identifier as u8];
    for i in 0..4 {
        packet.push(value.get(i).copied().unwrap_or(0));
    }
    packet
}

pub fn build_media_information_new_device(
    self_mac_address: &str,
    target_mac_address: &str,
) -> Result<Vec<u8>, InvalidMac> {
    let mut buffer = Vec::with_capacity(112);
    buffer.extend_from_slice(&reversed_mac(target_mac_address)?);
    buffer.extend_from_slice(&[0x68, 0x00]);
    buffer.extend_from_slice(&[0x01, 0xE5, 0x4A]);
    buffer.extend_from_slice(b"playingApp");
    buffer.push(0x42);
    buffer.extend_from_slice(b"NA");
    buffer.push(0x52);
    buffer.extend_from_slice(b"hostStreamingState");
    buffer.push(0x42);
    buffer.extend_from_slice(b"NO");
    buffer.push(0x49);
    buffer.extend_from_slice(b"btAddress");
    buffer.push(0x51);
    buffer.extend_from_slice(self_mac_address.as_bytes());
    buffer.push(0x46);
    buffer.extend_from_slice(b"btName");
    buffer.push(0x43);
    buffer.extend_from_slice(b"Mac");
    buffer.push(0x58);
    buffer.extend_from_slice(b"otherDevice");
    buffer.extend_from_slice(b"AudioCategory");
    buffer.extend_from_slice(&[0x30, 0x64]);
    Ok(smart_routing(&buffer))
}

pub fn build_hijack_request(target_mac_address: &str) -> Result<Vec<u8>, InvalidMac> {
    let mut buffer = Vec::with_capacity(106);
    buffer.extend_from_slice(&reversed_mac(target_mac_address)?);
    buffer.extend_from_slice(&[0x62, 0x00]);
    buffer.extend_from_slice(&[0x01, 0xE5]);
    buffer.push(0x4A);
    buffer.extend_from_slice(b"localscore");
    buffer.extend_from_slice(&[0x30, 0x64]);
    buffer.push(0x46);
    buffer.extend_from_slice(b"reason");
    buffer.push(0x48);
    buffer.extend_from_slice(b"Hijackv2");
    buffer.push(0x51);
    buffer.extend_from_slice(b"audioRoutingScore");
    buffer.extend_from_slice(&[0x31, 0x2D, 0x01, 0x5F]);
    buffer.extend_from_slice(b"audioRoutingSetOwnershipToFalse");
    buffer.push(0x01);
    buffer.push(0x4B);
    buffer.extend_from_slice(b"remotescore");
    buffer.push(0xA5);
    buffer.resize(buffer.len().max(106), 0x00);
    Ok(smart_routing(&buffer))
}

pub fn build_media_information(
    self_mac_address: &str,
    target_mac_address: &str,
    streaming_state: bool,
) -> Result<Vec<u8>, InvalidMac> {
    let mut buffer = Vec::with_capacity(138);
    buffer.extend_from_slice(&reversed_mac(target_mac_address)?);
    buffer.extend_from_slice(&[0x82, 0x00]);
    buffer.extend_from_slice(&[0x01, 0xE5, 0x4A]);
    buffer.extend_from_slice(b"PlayingApp");
    buffer.push(0x56);
    buffer.extend_from_slice(b"com.google.ios.youtube");
    buffer.push(0x52);
    buffer.extend_from_slice(b"HostStreamingState");
    buffer.push(0x42);
    buffer.extend_from_slice(if streaming_state { b"YES" } else { b"NO" });
    buffer.push(0x49);
    buffer.extend_from_slice(b"btAddress");
    buffer.push(0x51);
    buffer.extend_from_slice(self_mac_address.as_bytes());
    buffer.extend_from_slice(b"btName");
    buffer.push(0x43);
    buffer.extend_from_slice(b"Mac");
    buffer.push(0x58);
    buffer.extend_from_slice(b"otherDevice");
    buffer.extend_from_slice(b"AudioCategory");
    buffer.extend_from_slice(&[0x31, 0x2D, 0x01]);
    buffer.resize(buffer.len().max(138), 0x00);
    Ok(smart_routing(&buffer))
}

pub fn build_smart_routing_show_ui(target_mac_address: &str) -> Result<Vec<u8>, InvalidMac> {
    let mut buffer = Vec::with_capacity(134);
    buffer.extend_from_slice(&reversed_mac(target_mac_address)?);
    buffer.extend_from_slice(&[0x7E, 0x00]);
    buffer.extend_from_slice(&[0x01, 0xE6, 0x5B]);
    buffer.extend_from_slice(b"SmartRoutingKeyShowNearbyUI");
    buffer.push(0x01);
    buffer.push(0x4A);
    buffer.extend_from_slice(b"localscore");
    buffer.extend_from_slice(&[0x31, 0x2D]);
    buffer.push(0x01);
    buffer.push(0x46);
    buffer.extend_from_slice(b"reasonHhijackv2");
    buffer.push(0x51);
    buffer.extend_from_slice(b"audioRoutingScore");
    buffer.push(0xA2);
    buffer.push(0x5F);
    buffer.extend_from_slice(b"audioRoutingSetOwnershipToFalse");
    buffer.push(0x01);
    buffer.push(0x4B);
    buffer.extend_from_slice(b"remotescore");
    buffer.push(0xA2);
    buffer.resize(buffer.len().max(134), 0x00);
    Ok(smart_routing(&buffer))
}

pub fn build_hijack_reversed(target_mac_address: &str) -> Result<Vec<u8>, InvalidMac> {
    let mut buffer = Vec::with_capacity(97);
    buffer.extend_from_slice(&reversed_mac(target_mac_address)?);
    buffer.extend_from_slice(&[0x59, 0x00]);
    buffer.extend_from_slice(&[0x01, 0xE3]);
    buffer.push(0x5F);
    buffer.extend_from_slice(b"audioRoutingSetOwnershipToFalse");
    buffer.push(0x01);
    buffer.push(0x59);
    buffer.extend_from_slice(b"audioRoutingShowReverseUI");
    buffer.push(0x01);
    buffer.push(0x46);
    buffer.extend_from_slice(b"reason");
    buffer.push(0x53);
    buffer.extend_from_slice(b"ReverseBannerTapped");
    buffer.resize(buffer.len().max(97), 0x00);
    Ok(smart_routing(&buffer))
}

pub fn build_add_tipi_device(
    self_mac_address: &str,
    target_mac_address: &str,
) -> Result<Vec<u8>, InvalidMac> {
    let mut buffer = Vec::with_capacity(86);
    buffer.extend_from_slice(&reversed_mac(target_mac_address)?);
    buffer.extend_from_slice(&[0x4E, 0x00]);
    buffer.extend_from_slice(&[0x01, 0xE5]);
    buffer.extend_from_slice(&[0x48, 0x69]);
    buffer.extend_from_slice(b"idleTime");
    buffer.extend_from_slice(&[0x08, 0x47]);
    buffer.extend_from_slice(b"newTipi");
    buffer.extend_from_slice(&[0x01, 0x49]);
    buffer.extend_from_slice(b"btAddress");
    buffer.push(0x51);
    buffer.extend_from_slice(self_mac_address.as_bytes());
    buffer.push(0x46);
    buffer.extend_from_slice(b"btName");
    buffer.push(0x43);
    buffer.extend_from_slice(b"Mac");
    buffer.push(0x50);
    buffer.extend_from_slice(b"nearbyAudioScore");
    buffer.push(0x0E);
    Ok(smart_routing(&buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bytes pinned from the send_* methods before the builders were split out
    // of them.
    const SELF_MAC: &str = "AA:BB:CC:DD:EE:FF";
    const TARGET_MAC: &str = "11:22:33:44:55:66";

    fn hex(s: &str) -> Vec<u8> {
        hex::decode(s).unwrap()
    }

    #[test]
    fn proximity_keys_request() {
        assert_eq!(
            build_proximity_keys_request(&[ProximityKeyType::Irk, ProximityKeyType::EncKey]),
            hex("30000500")
        );
    }

    #[test]
    fn rename() {
        assert_eq!(build_rename("Pods"), hex("1a00010400506f6473"));
    }

    #[test]
    fn rename_cuts_long_names_at_a_char_boundary() {
        // 128 two-byte chars: 256 bytes, one past what the length byte holds.
        let packet = build_rename(&"é".repeat(128));
        assert_eq!(packet[3], 254);
        assert_eq!(packet.len(), 5 + 254);
        assert!(std::str::from_utf8(&packet[5..]).is_ok());
    }

    #[test]
    fn reversed_mac_needs_six_octets() {
        assert_eq!(
            reversed_mac(TARGET_MAC).unwrap(),
            [0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
        for bad in [
            "11:22:33:44:55",
            "11:22:33:44:55:66:77",
            "11:22:33:44:55:zz",
        ] {
            assert!(reversed_mac(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn control_command_pads_and_truncates_to_four_bytes() {
        assert_eq!(
            build_control_command(ControlCommandIdentifiers::ListeningMode, &[0x02]),
            hex("09000d02000000")
        );
        assert_eq!(
            build_control_command(
                ControlCommandIdentifiers::AutoAncStrength,
                &[0x01, 0x02, 0x03, 0x04, 0x05]
            ),
            hex("09002e01020304")
        );
    }

    #[test]
    fn media_information_new_device() {
        assert_eq!(
            build_media_information_new_device(SELF_MAC, TARGET_MAC),
            Ok(hex(concat!(
                "1000665544332211680001e54a706c6179696e67417070424e4152686f737453747265616d696e67",
                "5374617465424e4f496274416464726573735141413a42423a43433a44443a45453a46464662744e",
                "616d65434d6163586f74686572446576696365417564696f43617465676f72793064",
            )))
        );
    }

    #[test]
    fn hijack_request() {
        assert_eq!(
            build_hijack_request(TARGET_MAC),
            Ok(hex(concat!(
                "1000665544332211620001e54a6c6f63616c73636f7265306446726561736f6e4848696a61636b76",
                "3251617564696f526f7574696e6753636f7265312d015f617564696f526f7574696e675365744f77",
                "6e657273686970546f46616c7365014b72656d6f746573636f7265a5",
            )))
        );
    }

    #[test]
    fn media_information() {
        assert_eq!(
            build_media_information(SELF_MAC, TARGET_MAC, true),
            Ok(hex(concat!(
                "1000665544332211820001e54a506c6179696e6741707056636f6d2e676f6f676c652e696f732e79",
                "6f757475626552486f737453747265616d696e675374617465425945534962744164647265737351",
                "41413a42423a43433a44443a45453a464662744e616d65434d6163586f7468657244657669636541",
                "7564696f43617465676f7279312d010000000000",
            )))
        );
        assert_eq!(
            build_media_information(SELF_MAC, TARGET_MAC, false),
            Ok(hex(concat!(
                "1000665544332211820001e54a506c6179696e6741707056636f6d2e676f6f676c652e696f732e79",
                "6f757475626552486f737453747265616d696e675374617465424e4f496274416464726573735141",
                "413a42423a43433a44443a45453a464662744e616d65434d6163586f746865724465766963654175",
                "64696f43617465676f7279312d01000000000000",
            )))
        );
    }

    #[test]
    fn smart_routing_show_ui() {
        assert_eq!(
            build_smart_routing_show_ui(TARGET_MAC),
            Ok(hex(concat!(
                "10006655443322117e0001e65b536d617274526f7574696e674b657953686f774e65617262795549",
                "014a6c6f63616c73636f7265312d0146726561736f6e4868696a61636b763251617564696f526f75",
                "74696e6753636f7265a25f617564696f526f7574696e675365744f776e657273686970546f46616c",
                "7365014b72656d6f746573636f7265a2",
            )))
        );
    }

    #[test]
    fn hijack_reversed() {
        assert_eq!(
            build_hijack_reversed(TARGET_MAC),
            Ok(hex(concat!(
                "1000665544332211590001e35f617564696f526f7574696e675365744f776e657273686970546f46",
                "616c73650159617564696f526f7574696e6753686f775265766572736555490146726561736f6e53",
                "5265766572736542616e6e6572546170706564",
            )))
        );
    }

    #[test]
    fn add_tipi_device() {
        assert_eq!(
            build_add_tipi_device(SELF_MAC, TARGET_MAC),
            Ok(hex(concat!(
                "10006655443322114e0001e5486969646c6554696d6508476e657754697069014962744164647265",
                "73735141413a42423a43433a44443a45453a46464662744e616d65434d6163506e65617262794175",
                "64696f53636f72650e",
            )))
        );
    }

    #[test]
    fn malformed_target_mac_is_an_error() {
        for mac in [
            "",
            "11:22:33:44:55:GG",
            "11-22-33-44-55-66",
            "111:22:33:44:55:66",
        ] {
            let err = Err(InvalidMac(mac.to_string()));
            assert_eq!(build_hijack_request(mac), err);
            assert_eq!(build_hijack_reversed(mac), err);
            assert_eq!(build_smart_routing_show_ui(mac), err);
            assert_eq!(build_media_information(SELF_MAC, mac, true), err);
            assert_eq!(build_media_information_new_device(SELF_MAC, mac), err);
            assert_eq!(build_add_tipi_device(SELF_MAC, mac), err);
        }
    }
}
