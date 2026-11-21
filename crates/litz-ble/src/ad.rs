//! Advertising-data structures (Bluetooth Core, Vol 3, Part C, §11).
//!
//! One structure on the air is a length octet, an AD type octet and the data
//! the type defines. The length counts the type and the data together, never
//! itself. A zero length ends the list, which is how a controller pads a
//! payload shorter than 31 octets.

/// Flags: what the advertiser is and how it may be discovered.
pub const AD_FLAGS: u8 = 0x01;
/// Incomplete list of 16-bit service class UUIDs.
pub const AD_UUID16_INCOMPLETE: u8 = 0x02;
/// Complete list of 16-bit service class UUIDs.
pub const AD_UUID16_COMPLETE: u8 = 0x03;
/// Shortened local name.
pub const AD_NAME_SHORT: u8 = 0x08;
/// Complete local name.
pub const AD_NAME_COMPLETE: u8 = 0x09;

/// LE General Discoverable Mode with BR/EDR Not Supported: what a scannable
/// advertiser that has no classic-radio story sends.
pub const FLAGS_LE_GENERAL_DISCOVERABLE: u8 = 0x06;

/// One AD structure: the type octet and its data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdStructure {
    /// The AD type octet.
    pub kind: u8,
    /// The type's data, without the length or type octets.
    pub data: Vec<u8>,
}

impl AdStructure {
    pub fn new(kind: u8, data: impl Into<Vec<u8>>) -> Self {
        Self {
            kind,
            data: data.into(),
        }
    }

    /// The flags structure an advertising payload starts with.
    pub fn flags(flags: u8) -> Self {
        Self::new(AD_FLAGS, vec![flags])
    }

    /// A complete list of 16-bit service UUIDs, each little-endian on the air.
    pub fn services16(uuids: &[u16]) -> Self {
        let mut data = Vec::with_capacity(uuids.len() * 2);
        for uuid in uuids {
            data.extend_from_slice(&uuid.to_le_bytes());
        }
        Self::new(AD_UUID16_COMPLETE, data)
    }

    /// The complete local name.
    pub fn name(name: &str) -> Self {
        Self::new(AD_NAME_COMPLETE, name.as_bytes().to_vec())
    }
}

/// Encode a list of structures into one advertising payload.
///
/// The 31-octet payload limit belongs to the field that carries the bytes, not
/// to a structure, so this only refuses a structure whose own length octet
/// could not hold its data.
pub fn encode(structures: &[AdStructure]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for structure in structures {
        let length = u8::try_from(structure.data.len() + 1).map_err(|_| {
            format!(
                "an AD structure holds at most 254 octets of data, not {}",
                structure.data.len()
            )
        })?;
        out.push(length);
        out.push(structure.kind);
        out.extend_from_slice(&structure.data);
    }
    Ok(out)
}

/// Decode structures until a zero length or a truncated structure ends the
/// list. A controller pads a short payload with zeroes, so the terminator is
/// data rather than an error.
pub fn decode(data: &[u8]) -> Vec<AdStructure> {
    let mut structures = Vec::new();
    let mut rest = data;
    while let Some((&length, tail)) = rest.split_first() {
        let length = usize::from(length);
        if length == 0 || length > tail.len() {
            break;
        }
        // `length` covers the type and the data; the first octet of `tail` is
        // the type, and the remaining `length - 1` are the data.
        structures.push(AdStructure::new(tail[0], tail[1..length].to_vec()));
        rest = &tail[length..];
    }
    structures
}

/// The advertising payload for a beacon: flags, then the complete 16-bit
/// service list, then the complete local name.
///
/// The caller keeps the result within the 31-octet advertising limit; the board
/// rejects a name and service list that do not fit.
pub fn advertising_data(name: Option<&str>, services: &[u16]) -> Result<Vec<u8>, String> {
    let mut structures = vec![AdStructure::flags(FLAGS_LE_GENERAL_DISCOVERABLE)];
    if !services.is_empty() {
        structures.push(AdStructure::services16(services));
    }
    if let Some(name) = name {
        structures.push(AdStructure::name(name));
    }
    encode(&structures)
}

/// The complete (or shortened) local name in a payload.
pub fn name(data: &[u8]) -> Option<String> {
    decode(data)
        .into_iter()
        .find(|structure| matches!(structure.kind, AD_NAME_SHORT | AD_NAME_COMPLETE))
        .map(|structure| String::from_utf8_lossy(&structure.data).into_owned())
}

/// Every 16-bit service UUID in a payload, in list order.
pub fn services16(data: &[u8]) -> Vec<u16> {
    let mut uuids = Vec::new();
    for structure in decode(data) {
        if matches!(structure.kind, AD_UUID16_INCOMPLETE | AD_UUID16_COMPLETE) {
            for pair in structure.data.as_chunks::<2>().0 {
                uuids.push(u16::from_le_bytes(*pair));
            }
        }
    }
    uuids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_becomes_flags_then_a_complete_local_name() {
        let data = advertising_data(Some("Thermo"), &[]).unwrap();
        assert_eq!(data, b"\x02\x01\x06\x07\x09Thermo".to_vec());
        assert_eq!(name(&data).as_deref(), Some("Thermo"));
    }

    #[test]
    fn services_are_little_endian_in_the_order_given() {
        let data = advertising_data(None, &[0x180f, 0x180a]).unwrap();
        assert_eq!(data, vec![2, 1, 6, 5, 3, 0x0f, 0x18, 0x0a, 0x18]);
        assert_eq!(services16(&data), vec![0x180f, 0x180a]);
        assert_eq!(name(&data), None);
    }

    #[test]
    fn flags_services_and_name_round_trip_as_one_list() {
        let structures = [
            AdStructure::flags(FLAGS_LE_GENERAL_DISCOVERABLE),
            AdStructure::services16(&[0x180f]),
            AdStructure::name("litz"),
        ];
        let data = encode(&structures).unwrap();
        assert_eq!(decode(&data), structures.to_vec());
    }

    #[test]
    fn zero_padding_ends_a_payload_and_a_truncated_structure_is_ignored() {
        let mut data = advertising_data(Some("a"), &[]).unwrap();
        data.extend_from_slice(&[0, 0]);
        assert_eq!(decode(&data).len(), 2);
        assert_eq!(name(&data).as_deref(), Some("a"));

        let truncated = [5, AD_NAME_COMPLETE, b'x'];
        assert!(decode(&truncated).is_empty());
    }

    #[test]
    fn a_structure_too_large_for_its_length_octet_is_refused() {
        let structure = AdStructure::new(0x01, vec![0; 255]);
        assert!(encode(&[structure]).is_err());
    }
}
