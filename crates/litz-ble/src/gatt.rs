//! A peripheral's GATT table: the services and characteristics a central can
//! discover, read, write and subscribe to.
//!
//! The table belongs to the circuit or to the guest that advertises it, not to
//! the central: a connection is a pair (central station, table owner), and every
//! answer is the table's current bytes. Nothing here reads a clock; a value
//! changes only because a write or a notification changed it.

/// The characteristic is readable.
pub const FLAG_READ: u8 = 0x01;
/// The characteristic accepts writes.
pub const FLAG_WRITE: u8 = 0x02;
/// The characteristic can notify a subscribed central.
pub const FLAG_NOTIFY: u8 = 0x04;

/// The most services one table carries, and the most characteristics one
/// service does. Bounded so a guest cannot grow a peripheral without limit.
pub const MAX_SERVICES: usize = 16;
pub const MAX_CHARACTERISTICS: usize = 16;

/// The longest characteristic value a table holds.
pub const MAX_VALUE: usize = 512;

/// One characteristic: a 16-bit UUID, its access flags and its current bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GattCharacteristic {
    pub uuid: u16,
    pub flags: u8,
    pub value: Vec<u8>,
}

impl GattCharacteristic {
    pub fn new(uuid: u16, flags: u8, value: impl Into<Vec<u8>>) -> Self {
        Self {
            uuid,
            flags,
            value: value.into(),
        }
    }

    pub fn readable(&self) -> bool {
        self.flags & FLAG_READ != 0
    }

    pub fn writable(&self) -> bool {
        self.flags & FLAG_WRITE != 0
    }

    pub fn notifiable(&self) -> bool {
        self.flags & FLAG_NOTIFY != 0
    }
}

/// One service: a 16-bit UUID and the characteristics it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GattService {
    pub uuid: u16,
    pub characteristics: Vec<GattCharacteristic>,
}

impl GattService {
    pub fn new(uuid: u16, characteristics: Vec<GattCharacteristic>) -> Self {
        Self {
            uuid,
            characteristics,
        }
    }

    pub fn characteristic(&self, uuid: u16) -> Option<&GattCharacteristic> {
        self.characteristics
            .iter()
            .find(|characteristic| characteristic.uuid == uuid)
    }
}

/// A write a central made, as the peripheral reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GattWrite {
    pub service: u16,
    pub characteristic: u16,
    pub value: Vec<u8>,
}

/// A notification a subscribed central reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GattNotification {
    pub service: u16,
    pub characteristic: u16,
    pub value: Vec<u8>,
}

/// Find one characteristic in a table.
pub fn characteristic(
    table: &[GattService],
    service: u16,
    characteristic: u16,
) -> Option<&GattCharacteristic> {
    table
        .iter()
        .find(|entry| entry.uuid == service)?
        .characteristic(characteristic)
}

/// The same table with every value blanked, which is what service discovery
/// reports: a central learns the UUIDs, not the bytes.
pub fn discovery(table: &[GattService]) -> Vec<GattService> {
    table
        .iter()
        .map(|service| GattService {
            uuid: service.uuid,
            characteristics: service
                .characteristics
                .iter()
                .map(|characteristic| GattCharacteristic {
                    value: Vec::new(),
                    ..characteristic.clone()
                })
                .collect(),
        })
        .collect()
}

/// Set one characteristic's value in a table, when the table has it.
pub fn set_value(
    table: &mut [GattService],
    service: u16,
    characteristic_uuid: u16,
    value: Vec<u8>,
) -> bool {
    let Some(service) = table.iter_mut().find(|entry| entry.uuid == service) else {
        return false;
    };
    let Some(characteristic) = service
        .characteristics
        .iter_mut()
        .find(|entry| entry.uuid == characteristic_uuid)
    else {
        return false;
    };
    characteristic.value = value;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Vec<GattService> {
        vec![GattService::new(
            0x180f,
            vec![
                GattCharacteristic::new(0x2a19, FLAG_READ | FLAG_NOTIFY, vec![64]),
                GattCharacteristic::new(0x2a1a, FLAG_WRITE, Vec::new()),
            ],
        )]
    }

    #[test]
    fn a_table_finds_its_characteristics_and_their_access() {
        let table = table();
        let level = characteristic(&table, 0x180f, 0x2a19).expect("battery level");
        assert!(level.readable() && level.notifiable() && !level.writable());
        assert_eq!(level.value, vec![64]);
        assert!(characteristic(&table, 0x180f, 0x2a1a).unwrap().writable());
        assert!(characteristic(&table, 0x180f, 0x9999).is_none());
        assert!(characteristic(&table, 0x9999, 0x2a19).is_none());
    }

    #[test]
    fn discovery_reports_uuids_without_values() {
        let discovered = discovery(&table());
        assert_eq!(discovered[0].characteristics[0].uuid, 0x2a19);
        assert!(discovered[0].characteristics[0].value.is_empty());
        assert!(discovered[0].characteristics[0].readable());
    }

    #[test]
    fn a_value_changes_only_when_the_table_has_it() {
        let mut table = table();
        assert!(set_value(&mut table, 0x180f, 0x2a1a, vec![7]));
        assert_eq!(
            characteristic(&table, 0x180f, 0x2a1a).unwrap().value,
            vec![7]
        );
        assert!(!set_value(&mut table, 0x180f, 0x2a1b, vec![7]));
        assert!(!set_value(&mut table, 0x1800, 0x2a19, vec![7]));
    }
}
