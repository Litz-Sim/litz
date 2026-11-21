//! Bus devices on a board: a manifest puts a part on a bus, the diagram's
//! wires say where, and the runtime hands transactions to the part.

use litz_board::{Board, Device, FlatBoard, SpiDevice, UartPeer};
use litz_catalog::Registry;
use litz_core::Tick;
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;
const BMP280: &str = r#"{"id":"bmp1","type":"litz-bmp280","attrs":{}}"#;

fn circuit(parts: &[&str], connections: &[&str]) -> Circuit {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        r#"]}"#,
    ]
    .concat();
    Circuit::from_json(&json).unwrap()
}

fn flat_with(
    registry: &Registry,
    parts: &[&str],
    connections: &[&str],
) -> Result<FlatBoard, String> {
    FlatBoard::build(&circuit(parts, connections), registry).map_err(|e| e.to_string())
}

fn flat(parts: &[&str], connections: &[&str]) -> Result<FlatBoard, String> {
    flat_with(&Registry::builtin(), parts, connections)
}

fn board(parts: &[&str], connections: &[&str]) -> Board {
    Board::new(flat(parts, connections).expect("the diagram flattens")).expect("the board builds")
}

const BMP_WIRES: &[&str] = &[
    r#"["esp:GPIO8","bmp1:SDA","",[]]"#,
    r#"["esp:GPIO9","bmp1:SCL","",[]]"#,
    r#"["esp:3V3","bmp1:VCC","",[]]"#,
    r#"["esp:GND.1","bmp1:GND","",[]]"#,
];

const BOARD_B: &str = r#"{"id":"esp2","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;

#[test]
fn two_boards_each_have_their_own_i2c_bus() {
    // Each board carries one device on its own GPIO8/GPIO9 at an address the
    // other board does not have. Before the buses were keyed by board, the two
    // declarations met in one claim space and one controller map.
    let parts = [
        BOARD,
        BOARD_B,
        BMP280,
        r#"{"id":"oled","type":"litz-ssd1306","attrs":{}}"#,
    ];
    let wires = [
        r#"["esp:GPIO8","bmp1:SDA","",[]]"#,
        r#"["esp:GPIO9","bmp1:SCL","",[]]"#,
        r#"["esp:3V3","bmp1:VCC","",[]]"#,
        r#"["esp:GND.1","bmp1:GND","",[]]"#,
        r#"["esp2:GPIO8","oled:SDA","",[]]"#,
        r#"["esp2:GPIO9","oled:SCL","",[]]"#,
        r#"["esp2:3V3","oled:VCC","",[]]"#,
        r#"["esp2:GND.1","oled:GND","",[]]"#,
    ];
    let flat = flat(&parts, &wires).expect("two boards each have an i2c0");
    assert_eq!(
        flat.part("bmp1").and_then(|part| part.device),
        Some(Device::I2c {
            board: 0,
            controller: 0,
            address: 0x76
        })
    );
    assert_eq!(
        flat.part("oled").and_then(|part| part.device),
        Some(Device::I2c {
            board: 1,
            controller: 0,
            address: 0x3c
        })
    );
    let first = flat.buses.i2c.get(&(0, 0)).expect("board a has i2c0");
    let second = flat.buses.i2c.get(&(1, 0)).expect("board b has i2c0");
    assert_eq!((first.sda, first.scl), (8, 9));
    assert_eq!((second.sda, second.scl), (8, 9));

    let mut board = Board::new(flat).expect("the board builds");
    assert_eq!(
        board.i2c_write_read(0, 0, 0x76, &[0xd0], 1),
        Some(vec![0x58])
    );
    assert_eq!(
        board.i2c_write_read(1, 0, 0x76, &[0xd0], 1),
        None,
        "the bmp280 is on board a, not board b"
    );
    assert!(
        board.i2c_write_read(1, 0, 0x3c, &[0x00], 1).is_some(),
        "the display answers on board b's own controller"
    );
}

#[test]
fn a_bus_that_spans_two_boards_is_refused() {
    let parts = [BOARD, BOARD_B, BMP280];
    let wires = [
        r#"["esp:GPIO8","bmp1:SDA","",[]]"#,
        r#"["esp2:GPIO9","bmp1:SCL","",[]]"#,
        r#"["esp:3V3","bmp1:VCC","",[]]"#,
        r#"["esp:GND.1","bmp1:GND","",[]]"#,
    ];
    let error = flat(&parts, &wires).expect_err("a bus is one board's");
    assert!(error.contains("a bus belongs to one board"), "{error}");
}

#[test]
fn a_bmp280_register_read_returns_the_chip_id() {
    let mut board = board(&[BOARD, BMP280], BMP_WIRES);
    assert_eq!(
        board.i2c_write_read(0, 0, 0x76, &[0xd0], 1),
        Some(vec![0x58])
    );
}

#[test]
fn the_flattened_board_records_the_bus_it_derived() {
    let flat = flat(&[BOARD, BMP280], BMP_WIRES).expect("the diagram flattens");
    let i2c = flat.buses.i2c.get(&(0, 0)).expect("i2c0 is on the board");
    assert_eq!((i2c.sda, i2c.scl), (8, 9), "SDA then SCL, from the wires");
    assert_eq!(
        flat.part("bmp1").and_then(|part| part.device),
        Some(Device::I2c {
            board: 0,
            controller: 0,
            address: 0x76
        })
    );
}

struct Echo;

impl SpiDevice for Echo {
    fn select(&mut self, _selected: bool) {}

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        let mut out = tx.to_vec();
        out.resize(rx_len, 0);
        out
    }
}

#[test]
fn a_spi_transfer_reaches_the_device_on_its_chip_select() {
    let mut board = board(&[BOARD], &[]);
    board
        .buses_mut()
        .spi_mut(0, 0)
        .attach(7, Box::new(Echo))
        .unwrap();

    assert_eq!(
        board.spi_transfer(0, 0, 7, &[0xaa, 0x55], 2),
        vec![0xaa, 0x55]
    );
    assert_eq!(
        board.spi_transfer(0, 0, 6, &[0xaa], 1),
        vec![0xff],
        "no device on that select"
    );
}

struct Line {
    received: Vec<u8>,
    reply: Vec<u8>,
}

impl UartPeer for Line {
    fn receive(&mut self, _at: Tick, bytes: &[u8]) {
        self.received.extend_from_slice(bytes);
    }

    fn poll(&mut self, _at: Tick) -> Vec<u8> {
        std::mem::take(&mut self.reply)
    }
}

#[test]
fn a_uart_peer_receives_and_replies_through_the_board() {
    let mut board = board(&[BOARD], &[]);
    board
        .buses_mut()
        .uart_mut(0, 0)
        .attach(
            0,
            Box::new(Line {
                received: Vec::new(),
                reply: b"OK\n".to_vec(),
            }),
        )
        .unwrap();

    assert!(board.uart_receive(0, 0, 10, b"AT\n"));
    assert_eq!(board.uart_poll(0, 0, 20), b"OK\n".to_vec());
    assert!(!board.uart_receive(0, 1, 30, b"AT\n"), "no peer on UART1");
}

const SECOND_BMP: &str = r#"{"id":"bmp2","type":"litz-bmp280","attrs":{}}"#;

fn wires_with(part: &str, sda: u8, scl: u8) -> Vec<String> {
    let mut wires: Vec<String> = BMP_WIRES.iter().map(|wire| wire.to_string()).collect();
    wires.extend([
        format!(r#"["esp:GPIO{sda}","{part}:SDA","",[]]"#),
        format!(r#"["esp:GPIO{scl}","{part}:SCL","",[]]"#),
        format!(r#"["esp:3V3","{part}:VCC","",[]]"#),
        format!(r#"["esp:GND.1","{part}:GND","",[]]"#),
    ]);
    wires
}

fn as_refs(wires: &[String]) -> Vec<&str> {
    wires.iter().map(String::as_str).collect()
}

#[test]
fn two_i2c_devices_at_one_address_are_rejected() {
    let wires = wires_with("bmp2", 8, 9);
    let error = flat(&[BOARD, BMP280, SECOND_BMP], &as_refs(&wires)).unwrap_err();
    assert!(error.contains("0x76"), "{error}");
    assert!(error.contains("bmp1") && error.contains("bmp2"), "{error}");
}

#[test]
fn two_i2c_devices_wired_to_different_pins_are_rejected() {
    // 0x77 avoids the duplicate-address check, leaving the conflicting bus
    // pins as the reason i2c0 cannot be built.
    let mut registry = Registry::builtin();
    registry
        .insert_text(
            r#"
[part]
id = "bmp-0x77"
name = "BMP280 at 0x77"
version = "1.0.0"
[pins.VCC]
kind = "power"
[pins.GND]
kind = "ground"
[pins.SCL]
kind = "i2c-scl"
[pins.SDA]
kind = "i2c-sda"
[buses.i2c0]
address = 0x77
[simulation]
backend = "builtin"
engine = "bmp280"
"#,
        )
        .unwrap();
    let second = r#"{"id":"bmp2","type":"bmp-0x77","attrs":{}}"#;
    let wires = wires_with("bmp2", 18, 19);
    let error = flat_with(&registry, &[BOARD, BMP280, second], &as_refs(&wires)).unwrap_err();
    assert!(error.contains("i2c0 is wired two ways"), "{error}");
}

#[test]
fn a_bus_pin_not_wired_to_the_chip_is_rejected() {
    let error = flat(
        &[BOARD, BMP280],
        &[
            r#"["esp:GPIO8","bmp1:SDA","",[]]"#,
            r#"["esp:3V3","bmp1:VCC","",[]]"#,
            r#"["esp:GND.1","bmp1:GND","",[]]"#,
        ],
    )
    .unwrap_err();
    assert!(error.contains("i2c-scl"), "{error}");
}
