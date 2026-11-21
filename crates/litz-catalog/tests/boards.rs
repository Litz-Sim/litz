//! Each board manifest we ship parses through the catalog's text entry point.
//!
//! These boards are intentionally not in [`Registry::builtin`]; integration
//! registers them centrally. The test therefore reads the file from disk and
//! inserts it into a fresh registry, which also proves the manifest stands on
//! its own rather than relying on builtin registration.

use std::collections::HashSet;
use std::path::Path;

use litz_catalog::{BoardPinTarget, Registry};

/// `(board id, chip backend, highest GPIO the chip adapter accepts)`.
const BOARDS: &[(&str, &str, u32)] = &[
    ("board-arduino-nano", "atmega328p", 21),
    ("board-arduino-pro-mini", "atmega328p", 21),
    ("board-esp32-s3-devkitc-1-n16r8", "esp32s3", 48),
    ("board-esp32-c6-devkitm-1", "esp32c6", 23),
];

fn load(id: &str) -> (Registry, String) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("builtin")
        .join("boards")
        .join(format!("{id}.toml"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));

    let mut catalog = Registry::new();
    let loaded = catalog
        .insert_text(&text)
        .unwrap_or_else(|error| panic!("{id}: {error}"));
    (catalog, loaded)
}

#[test]
fn new_board_manifests_load_and_map_their_pins() {
    for &(id, chip, max_gpio) in BOARDS {
        let (catalog, loaded) = load(id);
        assert_eq!(loaded, id, "{id}: the manifest id must match its file");

        let manifest = catalog
            .get(id)
            .unwrap_or_else(|| panic!("{id} did not load"));
        let info = manifest
            .board
            .as_ref()
            .unwrap_or_else(|| panic!("{id} is not a board"));
        assert_eq!(info.chip.as_deref(), Some(chip), "{id}: chip backend");

        let mut seen = HashSet::new();
        for pin in &info.header {
            let BoardPinTarget::Gpio(gpio) = pin.target else {
                continue;
            };
            assert!(
                gpio <= max_gpio,
                "{id}: pin {:?} is gpio {gpio}, above the adapter's {max_gpio}",
                pin.name
            );
            assert!(
                seen.insert(gpio),
                "{id}: gpio {gpio} is claimed by more than one header pin",
            );
        }
        assert!(!seen.is_empty(), "{id} maps no GPIO pins");
    }
}
