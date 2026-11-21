//! The 0.2 contract, end to end: a component built from the board template
//! calls the buses, the ADC and the radio, and the host answers.
//!
//! The guest is checked in (see `fixtures/board-v2/README.md`) so the test
//! does not need a wasm toolchain. Its program is a byte script the template's
//! register machine understands: emit text, a host call, repeat, halt.

#![cfg(not(target_arch = "wasm32"))]

use litz_board::chip::{Chip, ImageKind};
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;
use litz_mcu_wasm::WasmChip;

const MODULE: &[u8] = include_bytes!("fixtures/board-v2/board-v2.wasm");

const DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "mcu", "type": "board-v2-fixture", "attrs": {}},
    {"id": "bmp1", "type": "litz-bmp280", "attrs": {}},
    {"id": "ap", "type": "litz-wifi-router", "attrs": {"ssid": "lab", "echo": "7"}},
    {"id": "beacon", "type": "litz-ble-beacon", "attrs": {}},
    {"id": "gps", "type": "litz-gps-neo6m", "attrs": {}}
  ],
  "connections": [
    ["mcu:GPIO0", "bmp1:SDA", "", []],
    ["mcu:GPIO1", "bmp1:SCL", "", []],
    ["mcu:3V3", "bmp1:VCC", "", []],
    ["mcu:GND.1", "bmp1:GND", "", []],
    ["mcu:GPIO2", "gps:TX", "", []],
    ["mcu:GPIO3", "gps:RX", "", []],
    ["mcu:3V3", "gps:VCC", "", []],
    ["mcu:GND.1", "gps:GND", "", []]
  ]
}"#;

fn registry() -> Registry {
    let mut registry = Registry::builtin();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    registry.load_dir(std::path::Path::new(dir)).unwrap();
    registry
}

/// A program for the template machine: print `hi`, then exercise every 0.2
/// host family, then halt.
fn program() -> Vec<u8> {
    let mut bytes = vec![1, b'h', 1, b'i', 1, b'\n'];
    // wifi-scan
    bytes.extend([4, 0]);
    // i2c read: controller 0, address 0x76, register 0xd0
    bytes.extend([4, 1, 0, 0x76, 0xd0]);
    // ble-scan
    bytes.extend([4, 2]);
    // analog read on GPIO0
    bytes.extend([4, 3, 0]);
    // uart write: port 0, two bytes
    bytes.extend([4, 4, 0, 2, b'O', b'K']);
    // join the access point called "lab"
    bytes.extend([4, 5, 3, b'l', b'a', b'b']);
    // tcp echo on port 7 with "pin"
    bytes.extend([4, 6, 0x00, 0x07, 3, b'p', b'i', b'n']);
    bytes.push(0);
    bytes
}

#[test]
fn a_v2_guest_reaches_the_buses_the_adc_and_the_radio() {
    let circuit = Circuit::from_json(DIAGRAM).unwrap();
    let flat = FlatBoard::build(&circuit, &registry()).unwrap();
    let board = Board::new(flat).unwrap();

    let mut chip = WasmChip::from_bytes_with_abi(MODULE, "board-v2.wasm", "litz-mcu-2")
        .expect("the component is a 0.2 guest")
        .with_board(board);
    chip.load_image(ImageKind::Firmware, 0, &program())
        .expect("the image loads");
    chip.boot().expect("the core boots");

    let mut serial = String::new();
    let mut stop = litz_board::chip::Stop::Budget;
    for _ in 0..64 {
        let result = chip.run(1_000_000);
        serial.push_str(&String::from_utf8_lossy(&chip.take_console()));
        stop = result.stop;
        if matches!(stop, litz_board::chip::Stop::Halted) {
            break;
        }
    }
    assert!(chip.fault().is_none(), "{:?}", chip.fault());
    assert!(
        matches!(stop, litz_board::chip::Stop::Halted),
        "the program ends at its halt instruction; stopped {stop:?}"
    );

    assert!(serial.starts_with("hi\n"), "{serial:?}");
    assert!(serial.contains("wifi lab ch6 -40dBm"), "{serial:?}");
    assert!(serial.contains("i2c 76 58"), "{serial:?}");
    assert!(
        serial.contains("ble ") && serial.contains("litz"),
        "{serial:?}"
    );
    assert!(serial.contains("adc 0 none"), "{serial:?}");
    assert!(serial.contains("uart 0 ok"), "{serial:?}");
    assert!(serial.contains("joined lab at 10.0.2.15"), "{serial:?}");
    assert!(serial.contains("tcp 7 pin"), "{serial:?}");
}

#[test]
fn a_module_that_names_an_unknown_abi_is_refused_before_it_runs() {
    let error = WasmChip::from_bytes_with_abi(MODULE, "board-v2.wasm", "litz-mcu-9")
        .err()
        .expect("an unknown contract is an error");
    assert!(error.contains("litz-mcu-9"), "{error}");
    assert!(error.contains("litz-mcu-2"), "{error}");
}

#[test]
fn a_v1_guest_still_loads_through_the_same_door() {
    let module = include_bytes!("../../litz-catalog/builtin/boards/litz-rv32.wasm");
    let chip = WasmChip::from_bytes_with_abi(module, "litz-rv32.wasm", "litz-mcu-1")
        .expect("the reference chip is a 0.1 guest");
    assert_eq!(chip.name(), "litz-rv32");
}

/// One chip's advertisement is heard by another station on the same board,
/// and not by the advertiser itself. This is the runtime half of BLE: the
/// circuit's beacon parts are static, these come and go from a guest.
#[test]
fn one_custom_chip_advertises_and_another_hears_it() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let diagram = r#"{
      "version": 1,
      "parts": [
        {"id": "mcu", "type": "board-v2-fixture", "attrs": {}}
      ],
      "connections": []
    }"#;
    let flat = FlatBoard::build(&Circuit::from_json(diagram).unwrap(), &registry()).unwrap();
    let board = Rc::new(RefCell::new(Board::new(flat).unwrap()));

    let boot = |index: u16, program: &[u8]| {
        let mut chip = WasmChip::from_bytes_with_abi(MODULE, "board-v2.wasm", "litz-mcu-2")
            .unwrap()
            .with_shared_board(Rc::clone(&board), index);
        chip.load_image(ImageKind::Firmware, 0, program).unwrap();
        chip.boot().unwrap();
        let mut serial = String::new();
        for _ in 0..16 {
            let result = chip.run(1_000_000);
            serial.push_str(&String::from_utf8_lossy(&chip.take_console()));
            if matches!(result.stop, litz_board::chip::Stop::Halted) {
                break;
            }
        }
        (chip, serial)
    };

    // Advertising payload: flags, then the complete local name "node".
    let adv = [2u8, 1, 6, 5, 9, b'n', b'o', b'd', b'e'];
    let mut advertise = vec![4, 7, adv.len() as u8];
    advertise.extend(adv);
    advertise.extend([0, 0]); // no scan response, then halt
    let (advertiser, serial) = boot(0, &advertise);
    assert!(
        serial.contains("ble adv "),
        "serial {serial:?}, fault {:?}",
        advertiser.fault()
    );

    let address = litz_ble::derive_address(&adv);
    let text = address
        .iter()
        .rev()
        .map(|octet| format!("{octet:02x}"))
        .collect::<Vec<_>>()
        .join(":");
    assert!(serial.contains(&text), "{serial:?}");

    let (_scanner, serial) = boot(1, &[4, 2, 0]);
    assert!(
        serial.contains(&format!("ble {text} node -40")),
        "{serial:?}"
    );
}

/// A custom chip serves a GATT table and another custom chip — a central —
/// discovers it, reads a characteristic, writes another, subscribes, and is
/// notified. Both directions cross the same shared board, so the connection is
/// a property of the circuit, not of either component.
#[test]
fn one_custom_chip_serves_gatt_and_another_uses_it() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let diagram = r#"{
      "version": 1,
      "parts": [
        {"id": "mcu", "type": "board-v2-fixture", "attrs": {}}
      ],
      "connections": []
    }"#;
    let flat = FlatBoard::build(&Circuit::from_json(diagram).unwrap(), &registry()).unwrap();
    let board = Rc::new(RefCell::new(Board::new(flat).unwrap()));

    let boot = |index: u16, program: &[u8]| {
        let mut chip = WasmChip::from_bytes_with_abi(MODULE, "board-v2.wasm", "litz-mcu-2")
            .unwrap()
            .with_shared_board(Rc::clone(&board), index);
        chip.load_image(ImageKind::Firmware, 0, program).unwrap();
        chip.boot().unwrap();
        let mut serial = String::new();
        for _ in 0..16 {
            let result = chip.run(1_000_000);
            serial.push_str(&String::from_utf8_lossy(&chip.take_console()));
            if matches!(result.stop, litz_board::chip::Stop::Halted) {
                break;
            }
        }
        (chip, serial)
    };

    // Peripheral: one 0x180f service, a readable+notifiable 0x2a19 holding
    // 0x64 and a writable 0x2a1a, then advertise "node".
    let mut peripheral = vec![4, 9, 1, 0x18, 0x0f, 2];
    peripheral.extend([0x2a, 0x19, 0x05, 1, 0x64]);
    peripheral.extend([0x2a, 0x1a, 0x02, 0]);
    peripheral.extend([4, 7, 9, 2, 1, 6, 5, 9, b'n', b'o', b'd', b'e', 0]);
    peripheral.push(0);
    let (chip, serial) = boot(0, &peripheral);
    assert!(
        serial.contains("gatt 1"),
        "serial {serial:?}, fault {:?}",
        chip.fault()
    );
    assert!(serial.contains("ble adv "), "{serial:?}");

    // Central: connect to the first advertiser, discover, read, write,
    // subscribe.
    let central = vec![
        4, 10, 0, // first advertiser
        4, 11, 1, // discover on handle 1
        4, 12, 1, 0x18, 0x0f, 0x2a, 0x19, // read the battery level
        4, 13, 1, 0x18, 0x0f, 0x2a, 0x1a, 1, 0xaa, // write it
        4, 14, 1, 0x18, 0x0f, 0x2a, 0x19, 1, // subscribe
        0,
    ];
    let (chip, serial) = boot(1, &central);
    assert!(
        serial.contains("link 1"),
        "serial {serial:?}, fault {:?}",
        chip.fault()
    );
    assert!(serial.contains("svc 180f 2a19,2a1a"), "{serial:?}");
    assert!(serial.contains("read 180f 2a19 64"), "{serial:?}");
    assert!(serial.contains("write 180f 2a1a"), "{serial:?}");
    assert!(serial.contains("sub 180f 2a19"), "{serial:?}");

    // The peripheral reads the write and notifies the subscriber.
    let notify = vec![
        4, 16, // take writes
        4, 17, 0x18, 0x0f, 0x2a, 0x19, 1, 0x63, // notify 0x63
        0,
    ];
    let (chip, serial) = boot(0, &notify);
    assert!(
        serial.contains("wrote 180f 2a1a aa"),
        "serial {serial:?}, fault {:?}",
        chip.fault()
    );
    assert!(serial.contains("notified 180f 2a19"), "{serial:?}");

    // The central drains the notification and disconnects.
    let drain = vec![4, 15, 1, 4, 18, 1, 0];
    let (chip, serial) = boot(1, &drain);
    assert!(
        serial.contains("notify 180f 2a19 63"),
        "serial {serial:?}, fault {:?}",
        chip.fault()
    );
    assert!(serial.contains("closed 1"), "{serial:?}");
}

/// Two custom chips share the raw packet medium: one configures a network,
/// channel and address and sends a frame, the other hears it after its
/// airtime. This is the same medium the SPI radio parts use, reached without
/// a radio part in between.
#[test]
fn one_custom_chip_puts_a_raw_frame_on_the_air_and_another_hears_it() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let diagram = r#"{
      "version": 1,
      "parts": [
        {"id": "mcu", "type": "board-v2-fixture", "attrs": {}}
      ],
      "connections": []
    }"#;
    let flat = FlatBoard::build(&Circuit::from_json(diagram).unwrap(), &registry()).unwrap();
    let board = Rc::new(RefCell::new(Board::new(flat).unwrap()));

    let boot = |index: u16, program: &[u8]| {
        let mut chip = WasmChip::from_bytes_with_abi(MODULE, "board-v2.wasm", "litz-mcu-2")
            .unwrap()
            .with_shared_board(Rc::clone(&board), index);
        chip.load_image(ImageKind::Firmware, 0, program).unwrap();
        chip.boot().unwrap();
        let mut serial = String::new();
        for _ in 0..16 {
            let result = chip.run(1_000_000);
            serial.push_str(&String::from_utf8_lossy(&chip.take_console()));
            if matches!(result.stop, litz_board::chip::Stop::Halted) {
                break;
            }
        }
        (chip, serial)
    };

    // Station 0: network "lab", channel 15, address 01, then broadcast
    // "radiogram" with 1000 ns of airtime.
    let mut sender = vec![4, 19, 3, b'l', b'a', b'b', 0, 15, 1, 0x01];
    sender.extend([4, 20, 9]);
    sender.extend(b"radiogram");
    sender.extend([0, 0, 0, 0x03, 0xe8]);
    sender.push(0);
    let (chip, serial) = boot(0, &sender);
    assert!(
        serial.contains("radio lab ch15"),
        "serial {serial:?}, fault {:?}",
        chip.fault()
    );
    assert!(serial.contains("radio tx 9"), "{serial:?}");

    // Station 1: same network and channel, address 02, then listen.
    let listener = vec![4, 19, 3, b'l', b'a', b'b', 0, 15, 1, 0x02, 4, 21, 0];
    let (chip, serial) = boot(1, &listener);
    assert!(
        serial.contains("radio rx 01 726164696f6772616d"),
        "serial {serial:?}, fault {:?}",
        chip.fault()
    );
}
