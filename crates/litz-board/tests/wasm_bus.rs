//! A wasm part on a bus: the catalog puts it on I2C, the board's flatten pass
//! resolves the wires, and `devices::build` loads the component as the device.
//!
//! The EEPROM is the reference guest under `crates/litz-part-eeprom`; the
//! panel proves a second component type is dispatched the same way. Both are
//! built against `litz:part@0.2.0`.

use litz_board::{Board, Device, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;
const EEPROM: &str = r#"{"id":"rom1","type":"litz-24c02","attrs":{}}"#;
const PANEL: &str = r#"{"id":"lcd1","type":"litz-rgb-panel","attrs":{}}"#;

fn board(parts: &[&str], connections: &[&str]) -> Board {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        r#"]}"#,
    ]
    .concat();
    let circuit = Circuit::from_json(&json).expect("the diagram parses");
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).expect("the diagram flattens"))
        .expect("the board builds")
}

fn i2c_wires(part: &str, sda: u8, scl: u8) -> Vec<String> {
    vec![
        format!(r#"["esp:GPIO{sda}","{part}:SDA","",[]]"#),
        format!(r#"["esp:GPIO{scl}","{part}:SCL","",[]]"#),
        format!(r#"["esp:3V3","{part}:VCC","",[]]"#),
        format!(r#"["esp:GND.1","{part}:GND","",[]]"#),
    ]
}

fn as_refs(wires: &[String]) -> Vec<&str> {
    wires.iter().map(String::as_str).collect()
}

#[test]
fn a_wasm_eeprom_is_attached_at_0x50() {
    let wires = i2c_wires("rom1", 8, 9);
    let circuit = Circuit::from_json(
        &[
            r#"{"version":1,"parts":["#,
            BOARD,
            ",",
            EEPROM,
            r#"],"connections":["#,
            &as_refs(&wires).join(","),
            r#"]}"#,
        ]
        .concat(),
    )
    .unwrap();
    let flat = FlatBoard::build(&circuit, &Registry::builtin()).unwrap();
    assert_eq!(
        flat.part("rom1").and_then(|part| part.device),
        Some(Device::I2c {
            board: 0,
            controller: 0,
            address: 0x50
        })
    );

    let board = Board::new(flat).unwrap();
    let i2c = board.buses().i2c.get(&(0, 0)).expect("i2c0 was built");
    assert!(i2c.addresses().any(|address| address == 0x50));
}

#[test]
fn a_wasm_eeprom_answers_write_and_read_through_the_board() {
    let wires = i2c_wires("rom1", 8, 9);
    let mut board = board(&[BOARD, EEPROM], &as_refs(&wires));

    // Fresh memory reads zero at the start of the array.
    assert_eq!(
        board.i2c_write_read(0, 0, 0x50, &[0x10], 1),
        Some(vec![0x00])
    );

    // Pointer 0x10, then two data bytes; a zero-length read closes the write.
    assert_eq!(
        board.i2c_write_read(0, 0, 0x50, &[0x10, 0xde, 0xad], 0),
        Some(Vec::new())
    );
    assert_eq!(
        board.i2c_write_read(0, 0, 0x50, &[0x10], 2),
        Some(vec![0xde, 0xad])
    );
    assert!(board.conflicts().is_empty(), "{:?}", board.conflicts());
}

#[test]
fn a_wasm_framebuffer_display_is_dispatched_too() {
    let wires = i2c_wires("lcd1", 18, 19);
    let board = board(&[BOARD, PANEL], &as_refs(&wires));
    let i2c = board.buses().i2c.get(&(0, 0)).expect("i2c0 was built");
    assert!(i2c.addresses().any(|address| address == 0x3c));
}

/// The 0.2 contract gives every guest the same three bus callback groups, so
/// the same component can be boxed as any of the board's device traits. This
/// checks the SPI and UART impls directly; the I2C path is covered through
/// `Board` above.
#[test]
fn one_component_can_answer_spi_and_uart_too() {
    use litz_board::{SpiDevice, UartPeer};

    let bytes = include_bytes!("../../litz-catalog/builtin/parts/24c02.wasm");
    let mut part = litz_part_wasm::WasmPart::from_bytes(bytes, "24c02.wasm").unwrap();

    part.select(true);
    assert_eq!(part.transfer(&[0xaa, 0x55], 3), vec![0xaa, 0x55, 0xff]);
    part.select(false);

    part.receive(10, b"AT\n");
    assert_eq!(part.poll(20), b"AT\n".to_vec());
}
