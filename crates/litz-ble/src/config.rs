//! What a board puts on the air: every beacon it places.

use crate::Beacon;

/// The advertisers a board places.
///
/// The order is the board's part order, which flatten fixes, so a backend that
/// reports a scan in list order produces the same report on every run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BleConfig {
    pub beacons: Vec<Beacon>,
}

impl BleConfig {
    /// A circuit with nothing in range.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the circuit places no advertiser at all.
    pub fn is_empty(&self) -> bool {
        self.beacons.is_empty()
    }

    /// Add one beacon.
    pub fn with_beacon(mut self, beacon: Beacon) -> Self {
        self.beacons.push(beacon);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_starts_empty_and_collects_beacons_in_order() {
        let config = BleConfig::new();
        assert!(config.is_empty());

        let beacon = |name: &str, last: u8| Beacon {
            name: Some(name.to_string()),
            services: Vec::new(),
            address: [0, 0, 0, 0, 0, last],
            rssi: -60,
            adv: Vec::new(),
            scan_response: Vec::new(),
            gatt: Vec::new(),
        };
        let config = config
            .with_beacon(beacon("one", 1))
            .with_beacon(beacon("two", 2));
        assert!(!config.is_empty());
        let names: Vec<_> = config
            .beacons
            .iter()
            .filter_map(|beacon| beacon.name.as_deref())
            .collect();
        assert_eq!(names, ["one", "two"]);
    }
}
