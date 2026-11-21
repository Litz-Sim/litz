//! The Bluetooth beacon part: the advertisers a circuit puts around a radio.
//!
//! A radio backend owns the LE link a chip hears. What a scan reports about the
//! devices around it — their addresses, names, services and signal — belongs to
//! the parts the circuit places, not to one chip adapter. That keeps a scan a
//! property of the circuit: the same diagram is heard by an ESP32-C3 or any
//! later backend, and the addresses and bytes a test asserts on do not depend
//! on which backend carried them.
//!
//! The part has no behaviour of its own. Its pins only keep it a real, powered
//! part; the backend reads its configuration through
//! [`Board::bluetooth`](crate::Board::bluetooth).

use crate::BuildError;
use crate::flatten::{Engine, FlatPart};
use litz_ble::{Beacon, BleConfig, MAX_ADVERTISING_DATA, MAX_NAME};
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};

/// The engine name the beacon manifest names.
pub const ENGINE: &str = "ble-beacon";

fn attr(part: &FlatPart, name: &str) -> Option<String> {
    part.attrs
        .get(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let mut digits = text.bytes();
    while let (Some(high), Some(low)) = (digits.next(), digits.next()) {
        let high = (high as char).to_digit(16)?;
        let low = (low as char).to_digit(16)?;
        bytes.push((high << 4 | low) as u8);
    }
    Some(bytes)
}

/// An advertising payload or scan response from six hex digits per octet.
fn data_hex(part: &FlatPart, name: &str) -> Result<Option<Vec<u8>>, BuildError> {
    let Some(text) = attr(part, name) else {
        return Ok(None);
    };
    let bytes = parse_hex(&text).ok_or_else(|| {
        BuildError::Message(format!(
            "part {:?}: {name} = {text:?} is not an even number of hex digits",
            part.id
        ))
    })?;
    if bytes.len() > MAX_ADVERTISING_DATA {
        return Err(BuildError::Message(format!(
            "part {:?}: {name} is {} bytes; an advertising payload holds at most {MAX_ADVERTISING_DATA}",
            part.id,
            bytes.len()
        )));
    }
    Ok(Some(bytes))
}

/// A comma-separated list of 16-bit service UUIDs, each four hex digits. The
/// value is the UUID as printed (`180f`); the payload carries it little-endian.
fn services(part: &FlatPart) -> Result<Vec<u16>, BuildError> {
    let Some(text) = attr(part, "services") else {
        return Ok(Vec::new());
    };
    text.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let digits = entry.strip_prefix("0x").unwrap_or(entry);
            if digits.len() != 4 {
                return Err(BuildError::Message(format!(
                    "part {:?}: services entry {entry:?} is not a four-digit hex UUID",
                    part.id
                )));
            }
            u16::from_str_radix(digits, 16).map_err(|_| {
                BuildError::Message(format!(
                    "part {:?}: services entry {entry:?} is not a four-digit hex UUID",
                    part.id
                ))
            })
        })
        .collect()
}

/// A GATT table from `service.characteristic[+flags][=hex]` entries.
///
/// `180f.2a19=64` is a readable battery level, `180f.2a1a+w` a writable
/// characteristic with no initial value, `1809.2a37+n` a notifiable one.
/// Flags default to `r`; services are collected in first-seen order.
fn gatt_table(part: &FlatPart) -> Result<Vec<litz_ble::GattService>, BuildError> {
    use litz_ble::{FLAG_NOTIFY, FLAG_READ, FLAG_WRITE, GattCharacteristic, GattService};

    let Some(text) = attr(part, "gatt") else {
        return Ok(Vec::new());
    };
    let mut services: Vec<GattService> = Vec::new();
    for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (body, value) = match entry.split_once('=') {
            Some((body, hex)) => {
                let value = parse_hex(hex.trim()).ok_or_else(|| {
                    BuildError::Message(format!(
                        "part {:?}: gatt value {hex:?} is not an even number of hex digits",
                        part.id
                    ))
                })?;
                (body.trim(), value)
            }
            None => (entry, Vec::new()),
        };
        let (body, flags) = match body.split_once('+') {
            Some((body, flags)) => (body.trim(), flags.trim()),
            None => (body, "r"),
        };
        let Some((service, characteristic)) = body.split_once('.') else {
            return Err(BuildError::Message(format!(
                "part {:?}: gatt entry {entry:?} is not service.characteristic",
                part.id
            )));
        };
        let uuid = |text: &str| -> Result<u16, BuildError> {
            let digits = text.trim().strip_prefix("0x").unwrap_or(text.trim());
            if digits.len() != 4 {
                return Err(BuildError::Message(format!(
                    "part {:?}: gatt UUID {text:?} is not four hex digits",
                    part.id
                )));
            }
            u16::from_str_radix(digits, 16).map_err(|_| {
                BuildError::Message(format!(
                    "part {:?}: gatt UUID {text:?} is not four hex digits",
                    part.id
                ))
            })
        };
        let service_uuid = uuid(service)?;
        let characteristic_uuid = uuid(characteristic)?;
        let mut bit = 0u8;
        for flag in flags.chars() {
            bit |= match flag {
                'r' => FLAG_READ,
                'w' => FLAG_WRITE,
                'n' => FLAG_NOTIFY,
                _ => {
                    return Err(BuildError::Message(format!(
                        "part {:?}: gatt flag {flag:?} is not r, w or n",
                        part.id
                    )));
                }
            };
        }
        if bit == 0 {
            return Err(BuildError::Message(format!(
                "part {:?}: gatt entry {entry:?} has no access flags",
                part.id
            )));
        }
        if value.len() > litz_ble::MAX_VALUE {
            return Err(BuildError::Message(format!(
                "part {:?}: gatt value is {} bytes; the limit is {}",
                part.id,
                value.len(),
                litz_ble::MAX_VALUE
            )));
        }
        let characteristic = GattCharacteristic::new(characteristic_uuid, bit, value);
        match services.iter_mut().find(|s| s.uuid == service_uuid) {
            Some(service) => {
                if service.characteristics.len() >= litz_ble::MAX_CHARACTERISTICS {
                    return Err(BuildError::Message(format!(
                        "part {:?}: service {service_uuid:04x} carries more than {} characteristics",
                        part.id,
                        litz_ble::MAX_CHARACTERISTICS
                    )));
                }
                service.characteristics.push(characteristic);
            }
            None => {
                if services.len() >= litz_ble::MAX_SERVICES {
                    return Err(BuildError::Message(format!(
                        "part {:?}: more than {} gatt services",
                        part.id,
                        litz_ble::MAX_SERVICES
                    )));
                }
                services.push(GattService::new(service_uuid, vec![characteristic]));
            }
        }
    }
    Ok(services)
}

/// The signal strength an LE Advertising Report carries: -127 through 20 dBm,
/// or 127 for "not available".
fn rssi(part: &FlatPart) -> Result<i8, BuildError> {
    let Some(text) = attr(part, "rssi") else {
        return Ok(-60);
    };
    let value = text.parse::<i8>().map_err(|_| {
        BuildError::Message(format!(
            "part {:?}: rssi = {text:?} is not a number",
            part.id
        ))
    })?;
    if !((-127..=20).contains(&value) || value == 127) {
        return Err(BuildError::Message(format!(
            "part {:?}: rssi {value} is outside -127 through 20 dBm, or 127 (not available)",
            part.id
        )));
    }
    Ok(value)
}

/// Turn one beacon part's attributes into the advertiser a backend reports.
///
/// The rules mirror esp32sim's nearby-device parser: a name is 1 to 26 bytes,
/// `adv` replaces the name-built data and is at most 31 bytes, name and `adv`
/// are exclusive, the signal is a reportable dBm value, and an address is six
/// hex octets. An unnamed address is derived from the advertising data, so it
/// is stable without a random source.
fn beacon(part: &FlatPart) -> Result<Beacon, BuildError> {
    let name = attr(part, "name");
    if let Some(name) = &name
        && name.len() > MAX_NAME
    {
        return Err(BuildError::Message(format!(
            "part {:?}: name is {} bytes; 1 to {MAX_NAME} fit the advertising data",
            part.id,
            name.len()
        )));
    }

    let explicit = data_hex(part, "adv")?;
    if name.is_some() && explicit.is_some() {
        return Err(BuildError::Message(format!(
            "part {:?}: a beacon takes name or adv, not both",
            part.id
        )));
    }

    // An explicit adv is the whole payload: the name and services a scan reads
    // are the ones inside it, and the attributes that build a payload are
    // ignored.
    let (name, services, adv) = match explicit {
        Some(adv) => {
            let name = litz_ble::ad::name(&adv);
            let services = litz_ble::ad::services16(&adv);
            (name, services, adv)
        }
        None => {
            let services = services(part)?;
            if name.is_none() && services.is_empty() {
                return Err(BuildError::Message(format!(
                    "part {:?}: a beacon needs a name, services or advertising data (adv=)",
                    part.id
                )));
            }
            let adv = litz_ble::advertising_data(name.as_deref(), &services)
                .map_err(|why| BuildError::Message(format!("part {:?}: {why}", part.id)))?;
            (name, services, adv)
        }
    };
    if adv.len() > MAX_ADVERTISING_DATA {
        return Err(BuildError::Message(format!(
            "part {:?}: the advertising data is {} bytes; a payload holds at most {MAX_ADVERTISING_DATA}",
            part.id,
            adv.len()
        )));
    }

    let address = match attr(part, "address") {
        Some(text) => litz_ble::parse_address(&text).ok_or_else(|| {
            BuildError::Message(format!(
                "part {:?}: address = {text:?} is not six hex octets (aa:bb:cc:dd:ee:ff)",
                part.id
            ))
        })?,
        None => litz_ble::derive_address(&adv),
    };

    Ok(Beacon {
        name,
        services,
        address,
        rssi: rssi(part)?,
        adv,
        scan_response: data_hex(part, "rsp")?.unwrap_or_default(),
        gatt: gatt_table(part)?,
    })
}

/// Every beacon the board places, in board order.
pub fn config(parts: &[FlatPart]) -> Result<BleConfig, BuildError> {
    let mut config = BleConfig::new();
    for part in parts {
        if !matches!(&part.engine, Engine::Builtin(engine) if engine == ENGINE) {
            continue;
        }
        config.beacons.push(beacon(part)?);
    }
    Ok(config)
}

/// The part itself: no pins are driven, and it has nothing to advance.
pub struct BeaconPart;

impl Part for BeaconPart {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

/// Build the part. Its attributes become the beacon a backend reports, so an
/// invalid one fails here rather than in the middle of a scan.
pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
    beacon(part)?;
    Ok(Box::new(BeaconPart))
}
