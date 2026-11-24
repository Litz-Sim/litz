//! The contract is written once. The crates that compile it carry a copy,
//! because wasmtime's `bindgen!` and `wit_bindgen::generate!` take a path and a
//! published crate carries only its own files — a path into a sibling builds
//! in this workspace and fails for anyone installing from the registry. This
//! test is what keeps the copies honest.

use std::path::PathBuf;

/// Where a copy of the contract lives, relative to this crate.
const MIRRORS: [&str; 3] = [
    "../litz-mcu-wasm/wit/litz-mcu.wit",
    "../litz-chip-rv32/wit/litz-mcu.wit",
    "../litz-cli/templates/board/wit/litz-mcu.wit",
];

#[test]
fn every_copy_of_the_contract_is_the_one_file() {
    let canonical = std::fs::read_to_string(litz_mcu_wit::wit_file()).expect("the contract");
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for mirror in MIRRORS {
        let path = here.join(mirror);
        let Ok(copy) = std::fs::read_to_string(&path) else {
            // `cargo test` on the packaged crate has no siblings; the copies
            // only matter in the workspace, where they are compiled.
            continue;
        };
        assert_eq!(canonical, copy, "{} drifted from the contract", path.display());
    }
}
