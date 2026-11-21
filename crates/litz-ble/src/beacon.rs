//! A beacon: one advertiser the circuit places, with a deterministic address.

use crate::gatt::GattService;

/// The most octets an advertising payload or a scan response carries
/// (Bluetooth Core, Vol 3, Part C, §11).
pub const MAX_ADVERTISING_DATA: usize = 31;

/// The longest name that still leaves room for the flags structure, the rule
/// an advertising payload imposes on a complete local name.
pub const MAX_NAME: usize = 26;

/// One Bluetooth advertiser a board puts in range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Beacon {
    /// The complete local name, when the payload advertises one.
    pub name: Option<String>,
    /// 16-bit service UUIDs in the payload, in advertising order.
    pub services: Vec<u16>,
    /// The controller address in HCI order: least significant octet first.
    pub address: [u8; 6],
    /// Signal strength in dBm as an LE Advertising Report carries it: -127
    /// through 20, or 127 for "not available".
    pub rssi: i8,
    /// The advertising data.
    pub adv: Vec<u8>,
    /// The scan response, which an active scan reads.
    pub scan_response: Vec<u8>,
    /// The GATT table a central discovers after connecting. Empty means the
    /// advertiser accepts no connection (a pure beacon).
    pub gatt: Vec<GattService>,
}

impl Beacon {
    /// The address as printed, most significant octet first.
    pub fn address_text(&self) -> String {
        address_text(&self.address)
    }

    /// 0 for a public address, 1 for a random static one.
    ///
    /// A derived address is random static: its two most significant bits are
    /// 11. An explicit address that does not carry those bits is reported as
    /// public, so a report never names a random address with the reserved 10
    /// pattern (Vol 6, Part B, §1.3.2).
    pub fn address_type(&self) -> u8 {
        u8::from(self.address[5] >> 6 == 0b11)
    }
}

/// Derive a stable random-static address from advertising data.
///
/// FNV-1a over the bytes, taken little-endian, with the top two bits of the
/// most significant octet forced to 11. There is no clock and no random
/// source, so the same advertiser has the same address on every run and in
/// every process; the 11 bits select the random-static type, never the
/// reserved 10 pattern.
pub fn derive_address(adv: &[u8]) -> [u8; 6] {
    let hash = adv.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
    });
    let bytes = hash.to_le_bytes();
    [
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5] | 0xc0,
    ]
}

/// Parse `aa:bb:cc:dd:ee:ff` into the HCI order a controller carries, which is
/// the reverse: the printed address is most significant octet first.
pub fn parse_address(text: &str) -> Option<[u8; 6]> {
    let mut address = [0u8; 6];
    let mut parts = text.split(':');
    for slot in address.iter_mut().rev() {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(address)
}

/// An address in printed order, most significant octet first.
pub fn address_text(address: &[u8; 6]) -> String {
    address
        .iter()
        .rev()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beacon(name: &str, address: [u8; 6]) -> Beacon {
        Beacon {
            name: Some(name.to_string()),
            services: Vec::new(),
            address,
            rssi: -60,
            adv: Vec::new(),
            scan_response: Vec::new(),
            gatt: Vec::new(),
        }
    }

    #[test]
    fn an_address_round_trips_through_printed_order() {
        let address = parse_address("aa:bb:cc:dd:ee:ff").unwrap();
        assert_eq!(address, [0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa]);
        assert_eq!(address_text(&address), "aa:bb:cc:dd:ee:ff");
        for bad in [
            "aa:bb:cc:dd:ee",
            "aa:bb:cc:dd:ee:ff:00",
            "aa:bb:cc:dd:ee:g",
            "a:b:c:d:e:f",
            "",
        ] {
            assert!(parse_address(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_derived_address_is_stable_and_random_static() {
        let adv = b"\x02\x01\x06\x07\x09Thermo";
        let first = derive_address(adv);
        assert_eq!(
            first,
            derive_address(adv),
            "the same bytes give the same address"
        );
        assert_eq!(
            first[5] >> 6,
            0b11,
            "random static, never reserved 10 or private"
        );
        assert_ne!(first, derive_address(b"\x02\x01\x06\x07\x09Scale"));
        assert_eq!(beacon("Thermo", first).address_type(), 1);
    }

    #[test]
    fn an_explicit_address_is_random_only_with_the_static_bits() {
        let public = parse_address("aa:bb:cc:dd:ee:ff").unwrap();
        assert_eq!(beacon("public", public).address_type(), 0);
        let random = parse_address("c4:aa:01:02:03:04").unwrap();
        assert_eq!(beacon("random", random).address_type(), 1);
    }
}
