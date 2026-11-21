//! Booting a hand-made ESP image with no mask ROM, no bootloader, no
//! ESP-IDF — the basis of every fixture the rest of the project runs on.
//!
//! If this test passes, `litz` can produce its own firmware, which means the
//! browser demo and the CLI tests need nothing downloaded to be useful.

#![cfg(feature = "esp32c3")]

/// An ESP-IDF application image: 24-byte header, then `load_addr`, `len` and
/// data per segment. The checksum and padding esptool writes are not read by
/// the loader, but a real image has them, so the builder keeps them.
fn image(entry: u32, segments: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = vec![0xE9, segments.len() as u8, 0x00, 0x00];
    out.extend(entry.to_le_bytes());
    out.extend([0u8; 16]);
    for (addr, data) in segments {
        out.extend(addr.to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(*data);
    }
    out.push(0);
    out
}

#[test]
fn an_app_image_without_a_rom_boots_and_runs() {
    // `jal x0, 0` — spin forever at the entry point. What is under test is
    // that the machine maps an IRAM segment and starts executing it.
    let code = 0x0000_006fu32.to_le_bytes();
    let mut mcu = litz_esp::backend::C3::new([0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56], 4 << 20);
    mcu.write_flash(0x1_0000, &image(0x4038_0000, &[(0x4038_0000, &code)]))
        .unwrap();
    let entry = mcu.boot_app(0x1_0000).expect("boot_app");
    assert_eq!(entry, 0x4038_0000);

    let before = mcu.seconds();
    let _ = mcu.run(1_000_000);
    assert!(mcu.seconds() > before, "the machine did not advance");
}

#[test]
fn the_same_image_boots_with_a_board_attached() {
    use litz_board::{Board as LitzBoard, FlatBoard};
    use litz_catalog::Registry;
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

    let code = 0x0000_006fu32.to_le_bytes();
    let mut mcu =
        litz_esp::backend::C3::new([0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56], 4 << 20).with_board(board);
    mcu.write_flash(0x1_0000, &image(0x4038_0000, &[(0x4038_0000, &code)]))
        .unwrap();
    mcu.boot_app(0x1_0000).expect("boot_app");
    let _ = mcu.run(1_000_000);
    assert!(mcu.conflicts().is_empty(), "{:?}", mcu.conflicts());
}
/// A breakpoint stops the run at the address it names, and the stop survives
/// the `Chip` mapping so a debugger can tell a breakpoint from a budget.
#[test]
fn a_breakpoint_stops_the_run_at_its_address() {
    use litz_board::chip::Chip;

    let code = 0x0000_006fu32.to_le_bytes();
    let mut mcu = litz_esp::backend::C3::new([0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56], 4 << 20);
    mcu.write_flash(0x1_0000, &image(0x4038_0000, &[(0x4038_0000, &code)]))
        .unwrap();
    mcu.boot_app(0x1_0000).expect("boot_app");
    mcu.break_at(0x4038_0000);

    let result = Chip::run(&mut mcu, 1_000_000);
    assert_eq!(result.stop, litz_board::Stop::Breakpoint(0x4038_0000));
    let registers = mcu.registers();
    assert!(
        !registers.is_empty(),
        "a stopped core has registers to read"
    );
}
