//! Boots an unmodified ESP-IDF `hello_world` for ESP32-C3 from the real mask
//! ROM through the `esp32sim` backend.
//!
//! Needs two asset directories, so it is `#[ignore]`d by default:
//!
//! ```sh
//! LITZ_ROM_DIR=build/roms \
//! LITZ_C3_ASSETS=build/esp32sim/web/wasm/fw/public \
//! cargo test -p litz-esp --features esp32c3 --test c3_boot -- --ignored
//! ```
//!
//! `build/roms` comes from `esp32sim`'s `tools/fetch-rom-elfs.sh` (Apache-2.0,
//! `espressif/esp-rom-elfs`); the assets are the `c3-hello-*` trio.

#![cfg(feature = "esp32c3")]

use std::path::{Path, PathBuf};

fn asset_dir(var: &str) -> PathBuf {
    let p = PathBuf::from(std::env::var(var).unwrap_or_else(|_| panic!("{var} is not set")));
    if p.is_absolute() {
        p
    } else {
        // Cargo runs tests with the crate directory as the working directory;
        // relative asset paths in the command line are meant from the repo root.
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(p)
    }
}

fn boot(rom_dir: &Path, assets: &Path, steps_per_run: u64, runs: u32) -> String {
    let rom = std::fs::read(rom_dir.join("esp32c3_rev3_rom.elf")).expect("rom elf");
    let bootloader = std::fs::read(assets.join("c3-hello-bootloader.bin")).expect("bootloader");
    let ptable = std::fs::read(assets.join("c3-hello-ptable.bin")).expect("partition table");
    let app = std::fs::read(assets.join("c3-hello_world.bin")).expect("app");

    let mut mcu = litz_esp::backend::C3::new([0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56], 4 << 20);
    mcu.load_rom(&rom).expect("load rom");
    mcu.write_flash(0x0, &bootloader).expect("flash bootloader");
    mcu.write_flash(0x8000, &ptable)
        .expect("flash partition table");
    mcu.write_flash(0x1_0000, &app).expect("flash app");

    for _ in 0..runs {
        let _ = mcu.run(steps_per_run);
        if String::from_utf8_lossy(mcu.uart0()).contains("Hello world!") {
            break;
        }
    }
    String::from_utf8_lossy(mcu.uart0()).into_owned()
}

#[test]
#[ignore = "needs LITZ_ROM_DIR and LITZ_C3_ASSETS"]
fn c3_boots_unmodified_idf_hello_world() {
    let out = boot(
        &asset_dir("LITZ_ROM_DIR"),
        &asset_dir("LITZ_C3_ASSETS"),
        1_000_000,
        200,
    );
    assert!(out.contains("Hello world!"), "serial was:\n{out}");
    assert!(out.contains("esp32c3 chip"), "serial was:\n{out}");
}

/// The same firmware, two fresh machines, must produce byte-identical serial
/// output. This is the property a CI gate rests on; if it ever fails, the
/// backend picked up host state (env, clock, randomness).
#[test]
#[ignore = "needs LITZ_ROM_DIR and LITZ_C3_ASSETS"]
fn c3_run_is_deterministic() {
    let rom_dir = asset_dir("LITZ_ROM_DIR");
    let assets = asset_dir("LITZ_C3_ASSETS");
    let first = boot(&rom_dir, &assets, 1_000_000, 40);
    let second = boot(&rom_dir, &assets, 1_000_000, 40);
    assert_eq!(first, second);
}

/// The whole chain: a diagram is flattened into a board, the board is attached
/// to the C3, and an unmodified firmware still boots. The LED is off because
/// `hello_world` never touches GPIO8 — what this proves is that attaching a
/// board does not disturb the chip.
#[test]
#[ignore = "needs LITZ_ROM_DIR and LITZ_C3_ASSETS"]
fn c3_boots_with_a_board_attached() {
    use litz_board::{Board as LitzBoard, FlatBoard};
    use litz_catalog::Registry;
    use litz_core::PartState;
    use litz_diagram::Circuit;

    let diagram = r#"{
      "version": 1,
      "parts": [
        {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
        {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
        {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
      ],
      "connections": [
        ["esp:GPIO8", "r1:1", "", []],
        ["r1:2", "led1:A", "", []],
        ["led1:C", "esp:GND.1", "", []]
      ]
    }"#;
    let registry = Registry::builtin();
    let circuit = Circuit::from_json(diagram).unwrap();
    let board = LitzBoard::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap();

    let rom_dir = asset_dir("LITZ_ROM_DIR");
    let assets = asset_dir("LITZ_C3_ASSETS");
    let rom = std::fs::read(rom_dir.join("esp32c3_rev3_rom.elf")).expect("rom elf");
    let bootloader = std::fs::read(assets.join("c3-hello-bootloader.bin")).expect("bootloader");
    let ptable = std::fs::read(assets.join("c3-hello-ptable.bin")).expect("partition table");
    let app = std::fs::read(assets.join("c3-hello_world.bin")).expect("app");

    let mut mcu =
        litz_esp::backend::C3::new([0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56], 4 << 20).with_board(board);
    mcu.load_rom(&rom).expect("load rom");
    mcu.write_flash(0x0, &bootloader).expect("flash bootloader");
    mcu.write_flash(0x8000, &ptable)
        .expect("flash partition table");
    mcu.write_flash(0x1_0000, &app).expect("flash app");

    for _ in 0..200 {
        let _ = mcu.run(1_000_000);
        if String::from_utf8_lossy(mcu.uart0()).contains("Hello world!") {
            break;
        }
    }

    assert!(String::from_utf8_lossy(mcu.uart0()).contains("Hello world!"));
    assert!(mcu.conflicts().is_empty(), "{:?}", mcu.conflicts());
    assert_eq!(mcu.state("led1"), Some(PartState::Led { lit: false }));
    assert_eq!(mcu.state("esp/led_pwr"), Some(PartState::Led { lit: true }));
}
