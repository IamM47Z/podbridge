use bluer::Adapter;
use log::debug;
use std::io::Error;

pub(crate) async fn find_connected_airpods(adapter: &Adapter) -> bluer::Result<bluer::Device> {
    let target_uuid = uuid::uuid!("74ec2172-0bad-4d01-8f77-997b2be0722a");

    let addrs = adapter.device_addresses().await?;
    for addr in addrs {
        let device = adapter.device(addr)?;
        if device.is_connected().await.unwrap_or(false)
            && let Ok(uuids) = device.uuids().await
            && let Some(uuids) = uuids
            && uuids.iter().any(|u| *u == target_uuid)
        {
            return Ok(device);
        }
    }
    Err(bluer::Error::from(Error::new(
        std::io::ErrorKind::NotFound,
        "No connected AirPods found",
    )))
}

pub async fn find_other_managed_devices(
    adapter: &Adapter,
    managed_macs: Vec<String>,
) -> bluer::Result<Vec<bluer::Device>> {
    let addrs = adapter.device_addresses().await?;
    let mut devices = Vec::new();
    for addr in addrs {
        let device = adapter.device(addr)?;
        let device_mac = device.address().to_string();
        let connected = device.is_connected().await.unwrap_or(false);
        debug!("Checking device: {}, connected: {}", device_mac, connected);
        if connected && managed_macs.contains(&device_mac) {
            debug!("Found managed device: {}", device_mac);
            devices.push(device);
        }
    }
    if !devices.is_empty() {
        return Ok(devices);
    }
    debug!("No other managed devices found");
    Err(bluer::Error::from(Error::new(
        std::io::ErrorKind::NotFound,
        "No other managed devices found",
    )))
}

/// Asks BlueZ to connect the AirPods' A2DP sink profile. After some connects
/// BlueZ links only the headset profile, so the card has no A2DP profile until
/// this is requested; restarting WirePlumber used to paper over that.
pub(crate) async fn connect_a2dp_profile(mac: &str) -> bluer::Result<()> {
    const A2DP_SINK: uuid::Uuid = uuid::Uuid::from_u128(0x0000110b_0000_1000_8000_00805f9b34fb);
    let addr: bluer::Address = mac
        .parse()
        .map_err(|_| bluer::Error::from(Error::other(format!("bad address {mac}"))))?;
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    // Bounded: the caller holds the media controller locks while it waits.
    let device = adapter.device(addr)?;
    let request = device.connect_profile(&A2DP_SINK);
    match tokio::time::timeout(std::time::Duration::from_secs(5), request).await {
        Ok(result) => result,
        Err(_) => Err(bluer::Error::from(Error::new(
            std::io::ErrorKind::TimedOut,
            "A2DP profile request timed out",
        ))),
    }
}

/// The alias to set so BlueZ shows the name the AirPods report, or `None`
/// when it already matches or the reported name is empty.
fn alias_to_set<'a>(current: &str, reported: &'a str) -> Option<&'a str> {
    (!reported.is_empty() && current != reported).then_some(reported)
}

/// Makes the BlueZ alias of `mac` match the name the AirPods report, so
/// desktop Bluetooth settings and PipeWire show the AirPods' own name.
pub(crate) async fn sync_alias(mac: &str, reported: &str) -> bluer::Result<()> {
    let addr: bluer::Address = mac
        .parse()
        .map_err(|_| bluer::Error::from(Error::other(format!("bad address {mac}"))))?;
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let device = adapter.device(addr)?;
    let current = device.alias().await?;
    if let Some(alias) = alias_to_set(&current, reported) {
        device.set_alias(alias.to_string()).await?;
        log::info!("Set the BlueZ alias of {mac} from {current:?} to {alias:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::alias_to_set;

    #[test]
    fn alias_to_set_returns_the_reported_name_when_it_differs() {
        assert_eq!(
            alias_to_set("Mateus’s AirPods Pro - Find My", "Mateus’s AirPods Pro"),
            Some("Mateus’s AirPods Pro")
        );
    }

    #[test]
    fn alias_to_set_is_none_when_equal() {
        assert_eq!(alias_to_set("Pods", "Pods"), None);
    }

    #[test]
    fn alias_to_set_is_none_when_reported_name_is_empty() {
        assert_eq!(alias_to_set("Pods", ""), None);
    }
}
