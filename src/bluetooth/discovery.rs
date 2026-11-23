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
