//! The WIT contract between Litz and a schematic part that is a WebAssembly
//! component.
//!
//! This crate is the contract's home rather than its implementation: the
//! `wit/` directory next to this file is what a part module is compiled
//! against, and the host loads it from the same place. Keeping one copy means
//! a guest and a host cannot drift apart without one of the two failing to
//! build.
//!
//! `wit-legacy/` holds the 0.1.0 package unchanged. The host still loads a
//! module built against it, so both files are part of the contract surface.
//!
//! ```
//! assert_eq!(litz_part_wit::ABI, "litz-part-2");
//! assert_eq!(litz_part_wit::LEGACY_ABI, "litz-part-1");
//! assert!(litz_part_wit::wit_file().is_file());
//! assert!(litz_part_wit::legacy_wit_file().is_file());
//! ```
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// The ABI name a `part.toml` writes in `[simulation] abi` for the current
/// contract, and the default when it writes none.
///
/// It tracks the WIT package version: `litz-part-2` is `litz:part@0.2.0`. A
/// bump that changes the world's shape bumps this string; one that only adds a
/// record field does not, because the component model rejects an unknown field
/// on the caller's side rather than silently defaulting it.
pub const ABI: &str = "litz-part-2";

/// The previous ABI, still accepted so a module built before 0.2 loads.
pub const LEGACY_ABI: &str = "litz-part-1";

/// The WIT package this ABI names, as it appears in a component's metadata.
pub const PACKAGE: &str = "litz:part@0.2.0";

/// The package [`LEGACY_ABI`] names.
pub const LEGACY_PACKAGE: &str = "litz:part@0.1.0";

/// The world a part module exports.
pub const WORLD: &str = "component";

/// Directory holding the current WIT documents.
pub fn wit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("wit")
}

/// Convenience for tests and build scripts: the one current WIT file.
pub fn wit_file() -> PathBuf {
    wit_dir().join("litz-part.wit")
}

/// Directory holding the 0.1.0 WIT document.
pub fn legacy_wit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("wit-legacy")
}

/// The 0.1.0 WIT file, kept for the guests and for the compatibility test.
pub fn legacy_wit_file() -> PathBuf {
    legacy_wit_dir().join("litz-part.wit")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_file(path: PathBuf) -> wit_parser::Resolve {
        let mut resolve = wit_parser::Resolve::default();
        resolve.push_path(path).expect("the WIT parses");
        resolve
    }

    fn resolve() -> wit_parser::Resolve {
        resolve_file(wit_file())
    }

    fn names(
        resolve: &wit_parser::Resolve,
        items: &wit_parser::IndexMap<wit_parser::WorldKey, wit_parser::WorldItem>,
    ) -> Vec<String> {
        let mut names: Vec<String> = items
            .keys()
            .map(|key| match key {
                wit_parser::WorldKey::Name(name) => name.clone(),
                wit_parser::WorldKey::Interface(id) => resolve.interfaces[*id]
                    .name
                    .clone()
                    .unwrap_or_else(|| "unnamed".into()),
            })
            .collect();
        names.sort();
        names
    }

    fn world_of<'a>(
        resolve: &'a wit_parser::Resolve,
        package_name: &str,
    ) -> (wit_parser::PackageId, &'a wit_parser::World) {
        let (id, _) = resolve
            .packages
            .iter()
            .find(|(_, p)| p.name.to_string() == package_name)
            .unwrap_or_else(|| panic!("the package {package_name} the ABI names"));
        let world_id = resolve.packages[id]
            .worlds
            .get(WORLD)
            .copied()
            .unwrap_or_else(|| panic!("no world {WORLD}"));
        (id, &resolve.worlds[world_id])
    }

    fn part_functions(resolve: &wit_parser::Resolve) -> Vec<String> {
        let part = resolve
            .interfaces
            .iter()
            .find(|(_, i)| i.name.as_deref() == Some("part"))
            .map(|(id, _)| id)
            .expect("a part interface");
        let mut found: Vec<String> = resolve.interfaces[part].functions.keys().cloned().collect();
        found.sort();
        found
    }

    #[test]
    fn the_package_and_world_are_the_ones_the_part_manifest_names() {
        let resolve = resolve();
        let (_, world) = world_of(&resolve, PACKAGE);
        // `types` appears as an import because the exported `part` interface
        // uses it; `host` is the two-call host surface. Both are declared
        // imports, so a guest that does not use them still links.
        assert_eq!(names(&resolve, &world.imports), ["host", "types"]);
        assert_eq!(names(&resolve, &world.exports), ["part"]);
    }

    /// The whole `litz_core::Part` surface plus the capability groups 0.2
    /// adds: bus transactions, attributes and a framebuffer. Nothing else.
    #[test]
    fn the_part_interface_is_the_one_this_abi_names() {
        let resolve = resolve();
        assert_eq!(
            part_functions(&resolve),
            [
                "advance",
                "analog",
                "attributes",
                "damage",
                "draw",
                "drive",
                "framebuffer",
                "i2c-connect",
                "i2c-read",
                "i2c-stop",
                "i2c-write",
                "info",
                "next-event",
                "press",
                "set-attribute",
                "set-control",
                "set-pin",
                "spi-select",
                "spi-transfer",
                "state",
                "uart-poll",
                "uart-receive",
            ]
        );
    }

    /// The legacy package still parses and still has the surface the shipped
    /// 74HC595 component was built against.
    #[test]
    fn the_legacy_package_keeps_its_original_shape() {
        let resolve = resolve_file(legacy_wit_file());
        let (_, world) = world_of(&resolve, LEGACY_PACKAGE);
        assert!(names(&resolve, &world.imports).contains(&"types".to_string()));
        assert_eq!(names(&resolve, &world.exports), ["part"]);
        assert_eq!(
            part_functions(&resolve),
            [
                "advance",
                "drive",
                "info",
                "next-event",
                "press",
                "set-control",
                "set-pin",
                "state",
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
            ("../litz-part-wasm/wit/litz-part.wit", "wit/litz-part.wit"),
            (
                "../litz-part-wasm/wit-legacy/litz-part.wit",
                "wit-legacy/litz-part.wit",
            ),
            ("../litz-part-sdk/wit/litz-part.wit", "wit/litz-part.wit"),
            (
                "../litz-part-74hc595/wit-legacy/litz-part.wit",
                "wit-legacy/litz-part.wit",
            ),
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
