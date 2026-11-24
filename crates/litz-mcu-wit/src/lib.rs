//! The WIT contract between Litz and a microcontroller that is a WebAssembly
//! component.
//!
//! This crate is the contract's home rather than its implementation: the
//! `wit/` directory next to this file is what a chip module is compiled
//! against, and the host reads the same file when it generates its bindings.
//! There is no second copy in the workspace, so a guest and a host cannot
//! drift apart without one of the two failing to build.
//!
//! ```
//! assert_eq!(litz_mcu_wit::ABI, "litz-mcu-2");
//! assert!(litz_mcu_wit::wit_file().is_file());
//! ```

use std::path::{Path, PathBuf};

/// The ABI name a `board.toml` writes in `[board.simulator] abi`.
///
/// It tracks the WIT package version: `litz-mcu-2` is `litz:mcu@0.2.0`. The
/// 0.1 package is gone; a manifest that still names `litz-mcu-1` is refused
/// rather than loaded against a different world.
pub const ABI: &str = "litz-mcu-2";

/// The WIT package this ABI names, as it appears in a component's metadata.
pub const PACKAGE: &str = "litz:mcu@0.2.0";

/// The world a chip module exports.
pub const WORLD: &str = "simulator";

/// Directory holding the WIT document.
pub fn wit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("wit")
}

/// Convenience for tests and build scripts: the one WIT file.
pub fn wit_file() -> PathBuf {
    wit_dir().join("litz-mcu.wit")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve() -> wit_parser::Resolve {
        let mut resolve = wit_parser::Resolve::default();
        resolve.push_path(wit_file()).expect("the WIT parses");
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

    fn interface<'a>(resolve: &'a wit_parser::Resolve, name: &str) -> &'a wit_parser::Interface {
        resolve
            .interfaces
            .iter()
            .find(|(_, interface)| interface.name.as_deref() == Some(name))
            .map(|(_, interface)| interface)
            .unwrap_or_else(|| panic!("a {name} interface"))
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
        assert_eq!(names(&resolve, &world.exports), ["chip", "debug"]);
    }

    #[test]
    fn the_chip_interface_is_the_one_this_abi_names() {
        let resolve = resolve();
        let mut found: Vec<&str> = interface(&resolve, "chip")
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
                "reset",
                "run",
                "set-inputs",
                "take-outputs",
                "write-console",
            ]
        );
    }

    /// One debug interface, one call per host `Chip` debug method. `read`
    /// lives here rather than on `chip`, so there is no second copy to keep
    /// in step.
    #[test]
    fn the_debug_interface_mirrors_the_host_debug_surface() {
        let resolve = resolve();
        let mut found: Vec<&str> = interface(&resolve, "debug")
            .functions
            .keys()
            .map(String::as_str)
            .collect();
        found.sort_unstable();
        assert_eq!(
            found,
            [
                "available",
                "disassemble",
                "pc",
                "read",
                "registers",
                "set-breakpoints",
                "step",
            ]
        );
        assert!(
            !interface(&resolve, "chip").functions.contains_key("read"),
            "read belongs to debug only"
        );
    }

    #[test]
    fn a_run_can_stop_on_a_breakpoint() {
        let resolve = resolve();
        let stop = resolve
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("stop"))
            .map(|(id, _)| resolve.types[id].kind.clone())
            .expect("a stop type");
        let wit_parser::TypeDefKind::Variant(variant) = stop else {
            panic!("stop must be a variant so it can carry the breakpoint pc");
        };
        assert!(
            variant
                .cases
                .iter()
                .any(|case| case.name == "breakpoint" && case.ty.is_some()),
            "stop has a breakpoint(u32) case"
        );
    }

    #[test]
    fn the_host_still_has_the_peripheral_and_wireless_calls() {
        let resolve = resolve();
        let host = interface(&resolve, "host");
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
            assert!(host.functions.contains_key(name), "the host has {name}");
        }
    }
}
