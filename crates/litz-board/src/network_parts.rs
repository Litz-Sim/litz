//! The Wi-Fi router part: an access point and the network behind it.
//!
//! A radio backend owns the 802.11 link a chip hears. Everything past the
//! gateway — the names the network answers and the TCP/UDP services it runs —
//! lives in the part typed `litz-wifi-router`, which any board can place.
//! That keeps the network a property of the circuit rather than of one chip
//! adapter: the same diagram runs on an ESP32, a C6 or a wasm MCU, and the
//! bytes a test asserts on do not depend on which backend carried them.
//!
//! The part has no behaviour of its own. Its pins only keep it a real,
//! powered part; the backend reads its configuration through
//! [`Board::network`](crate::Board::network).

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};
use litz_net::{AccessPoint, NearbyNetwork, NetworkConfig, Service};

/// The engine name the router manifest names.
pub const ENGINE: &str = "router";

fn attr(part: &FlatPart, name: &str) -> Option<String> {
    part.attrs
        .get(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// A comma-separated list of ports (`"7, 9000"`), refusing a value that is not
/// a port so a typo fails when the board is built rather than silently serving
/// nothing.
fn ports(part: &FlatPart, name: &str) -> Result<Vec<u16>, BuildError> {
    let Some(text) = attr(part, name) else {
        return Ok(Vec::new());
    };
    text.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry.parse::<u16>().map_err(|_| {
                BuildError::Message(format!(
                    "part {:?}: {name} = {entry:?} is not a port number",
                    part.id
                ))
            })
        })
        .collect()
}

/// Turn a router part's attributes into the network a backend puts on the air.
pub fn config(part: &FlatPart) -> Result<NetworkConfig, BuildError> {
    let ssid = attr(part, "ssid").unwrap_or_else(|| "litz".to_string());
    let channel = attr(part, "channel")
        .map(|text| {
            text.parse::<u8>().map_err(|_| {
                BuildError::Message(format!(
                    "part {:?}: channel = {text:?} is not a number",
                    part.id
                ))
            })
        })
        .transpose()?
        .unwrap_or(6);
    if !(1..=14).contains(&channel) {
        return Err(BuildError::Message(format!(
            "part {:?}: channel {channel} is outside 1 through 14",
            part.id
        )));
    }

    let mut network = NetworkConfig {
        access_points: vec![AccessPoint {
            ssid,
            psk: attr(part, "psk"),
            channel,
            bssid: None,
            epoch_s: None,
        }],
        ..NetworkConfig::default()
    };

    if let Some(body) = attr(part, "http") {
        network = network.with_service(Service::http(80, body));
    }
    // The secure twin of `http`: the same body, behind the simulated CA.
    if let Some(body) = attr(part, "https") {
        network = network.with_service(Service::https(443, body));
    }
    for port in ports(part, "ws")? {
        network = network.with_service(Service::websocket(port));
    }
    for port in ports(part, "wss")? {
        network = network.with_service(Service::wss(port));
    }
    for port in ports(part, "echo")? {
        network = network.with_service(Service::TcpEcho { port });
    }
    for port in ports(part, "udp_echo")? {
        network = network.with_service(Service::UdpEcho { port });
    }
    // Port 0 turns the broker off, which is how a manifest can carry the
    // attribute without every circuit running one.
    if let Some(port) = ports(part, "mqtt")?.first().filter(|port| **port != 0) {
        network = network.with_service(Service::mqtt(*port));
    }
    for name in attr(part, "nearby")
        .map(|text| {
            text.split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
    {
        network.nearby.push(NearbyNetwork::new(name));
    }
    Ok(network)
}

/// The part itself: no pins are driven, and it has nothing to advance.
pub struct RouterPart;

impl Part for RouterPart {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
    config(part)?;
    Ok(Box::new(RouterPart))
}

/// The board's one router, if it has one.
pub fn board_network(parts: &[FlatPart]) -> Result<Option<NetworkConfig>, BuildError> {
    let mut found: Option<(&FlatPart, NetworkConfig)> = None;
    for part in parts {
        if !matches!(&part.engine, crate::flatten::Engine::Builtin(engine) if engine == ENGINE) {
            continue;
        }
        if let Some((first, _)) = found {
            return Err(BuildError::Message(format!(
                "the board has two routers ({:?} and {:?}); a circuit joins one network",
                first.id, part.id
            )));
        }
        found = Some((part, config(part)?));
    }
    Ok(found.map(|(_, network)| network))
}
