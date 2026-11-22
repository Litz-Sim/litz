//! The WIT contract between Litz and a microcontroller that is a WebAssembly
//! component.
//!
//! This crate is the contract's home rather than its implementation: the
//! `wit/` directory next to this file is what a chip module is compiled
//! against, and the host loads it from the same place. Keeping one copy means
//! a guest and a host cannot drift apart without one of the two failing to
//! build.
//!
//! ```
//! assert_eq!(litz_mcu_wit::ABI, "litz-mcu-1");
//! assert!(litz_mcu_wit::wit_file().is_file());
//! assert_eq!(litz_mcu_wit::ABI_V2, "litz-mcu-2");
//! assert!(litz_mcu_wit::wit_v2_file().is_file());
//! ```

use std::path::{Path, PathBuf};

/// The ABI name a `board.toml` writes in `[board.simulator] abi`.
///
/// It tracks the WIT package version: `litz-mcu-1` is `litz:mcu@0.1.0`. A
/// bump that changes the world's shape bumps this string; one that only adds a
/// record field does not, because the component model rejects an unknown field
/// on the caller's side rather than silently defaulting it.
pub const ABI: &str = "litz-mcu-1";

/// The current contract: pins and console, plus analog reads, I2C/SPI/UART
/// transactions, the simulated Wi-Fi services, and BLE advertising, scanning,
/// connection and GATT.
pub const ABI_V2: &str = "litz-mcu-2";

/// The WIT package this ABI names, as it appears in a component's metadata.
pub const PACKAGE: &str = "litz:mcu@0.1.0";

/// The package `ABI_V2` names.
pub const PACKAGE_V2: &str = "litz:mcu@0.2.0";

/// The world a chip module exports.
pub const WORLD: &str = "simulator";

/// Directory holding the WIT documents.
pub fn wit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("wit")
}

/// Convenience for tests and build scripts: the one WIT file.
pub fn wit_file() -> PathBuf {
    wit_dir().join("litz-mcu.wit")
}

/// Directory holding the 0.2 contract.
pub fn wit_v2_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("wit-v2")
}

/// The 0.2 WIT file, which a board template copies next to its simulator.
pub fn wit_v2_file() -> PathBuf {
    wit_v2_dir().join("litz-mcu.wit")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve() -> wit_parser::Resolve {
        let mut resolve = wit_parser::Resolve::default();
        resolve.push_path(wit_file()).expect("the WIT parses");
        resolve
    }

    fn resolve_v2() -> wit_parser::Resolve {
        let mut resolve = wit_parser::Resolve::default();
        resolve
            .push_path(wit_v2_file())
            .expect("the 0.2 WIT parses");
        resolve
    }

    fn names(
        resolve: &wit_parser::Resolve,
        items: &wit_parser::IndexMap<wit_parser::WorldKey, wit_parser::WorldItem>,
    ) -> Vec<String> {
        items
            .keys()
            .map(|key| match key {
                wit_parser::WorldKey::Name(name) => name.clone(),
                wit_parser::WorldKey::Interface(id) => resolve.interfaces[*id]
                    .name
                    .clone()
                    .unwrap_or_else(|| "unnamed".into()),
            })
            .collect()
    }

    #[test]
    fn the_second_contract_is_the_same_world_with_the_peripheral_calls() {
        let resolve = resolve_v2();
        let (package, _) = resolve
            .packages
            .iter()
            .find(|(_, p)| p.name.to_string() == PACKAGE_V2)
            .expect("the package the 0.2 ABI names");
        let world_id = resolve.packages[package]
            .worlds
            .get(WORLD)
            .copied()
            .unwrap_or_else(|| panic!("no world {WORLD}"));
        let world = &resolve.worlds[world_id];
        assert_eq!(names(&resolve, &world.imports), ["types", "host"]);
        assert_eq!(names(&resolve, &world.exports), ["chip"]);

        let host = resolve
            .interfaces
            .iter()
            .find(|(_, interface)| interface.name.as_deref() == Some("host"))
            .map(|(id, _)| id)
            .expect("a host interface");
        for name in [
            "analog-read",
            "ble-advertise",
            "ble-connect",
            "ble-discover",
            "ble-notifications",
            "ble-notify",
            "ble-read",
            "ble-scan",
            "ble-set-gatt",
            "ble-stop-advertising",
            "ble-subscribe",
            "ble-take-writes",
            "ble-write",
            "i2c-transfer",
            "radio-configure",
            "radio-receive",
            "radio-send",
            "spi-transfer",
            "tcp-connect",
            "uart-write",
            "wifi-join",
            "wifi-scan",
        ] {
            assert!(
                resolve.interfaces[host].functions.contains_key(name),
                "the 0.2 host has {name}"
            );
        }
    }

    #[test]
    fn the_package_and_world_are_the_ones_the_board_config_names() {
        let resolve = resolve();
        let (package, _) = resolve
            .packages
            .iter()
            .find(|(_, p)| p.name.to_string() == PACKAGE)
            .expect("the package the ABI names");
        let world_id = resolve.packages[package]
            .worlds
            .get(WORLD)
            .copied()
            .unwrap_or_else(|| panic!("no world {WORLD}"));
        let world = &resolve.worlds[world_id];
        // `types` is imported by the world too: a `use` inside an exported
        // interface pulls the type definitions in as an import.
        assert_eq!(names(&resolve, &world.imports), ["types", "host"]);
        assert_eq!(names(&resolve, &world.exports), ["chip"]);
    }

    /// The two invariants the whole design rests on: one call runs a budget,
    /// and the boundary carries only pins and console bytes.
    #[test]
    fn the_chip_interface_is_the_one_this_abi_names() {
        let resolve = resolve();
        let chip = resolve
            .interfaces
            .iter()
            .find(|(_, i)| i.name.as_deref() == Some("chip"))
            .map(|(id, _)| id)
            .expect("a chip interface");
        let mut found: Vec<&str> = resolve.interfaces[chip]
            .functions
            .keys()
            .map(String::as_str)
            .collect();
        found.sort_unstable();
        assert_eq!(
            found,
            [
                "boot",
                "info",
                "load-image",
                "read",
                "reset",
                "run",
                "set-inputs",
                "take-outputs",
                "write-console",
            ]
        );
    }

    /// A published crate can only carry files under its own directory, so the
    /// host and guest crates keep a copy of the contract beside their source.
    /// Every copy must be byte-identical to the one here. The check skips when
    /// the sibling crates are absent, which is the case inside a packaged
    /// tarball.
    #[test]
    fn every_packaged_copy_carries_this_contract_unchanged() {
        let copies = [
            ("../litz-mcu-wasm/wit/litz-mcu.wit", "wit/litz-mcu.wit"),
            (
                "../litz-mcu-wasm/wit-v2/litz-mcu.wit",
                "wit-v2/litz-mcu.wit",
            ),
            ("../litz-chip-rv32/wit/litz-mcu.wit", "wit/litz-mcu.wit"),
        ];
        for (copy, canonical) in copies {
            let Ok(expected) = std::fs::read_to_string(canonical) else {
                return;
            };
            let actual = std::fs::read_to_string(copy).expect("the copy is checked in");
            assert_eq!(actual, expected, "{copy} drifted from {canonical}");
        }
    }
}
