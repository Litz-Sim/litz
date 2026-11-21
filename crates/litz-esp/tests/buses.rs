//! The esp32sim `BoardModel` routes chip peripheral traffic to the Litz
//! board's bus devices.
//!
//! These call the same callbacks the esp32sim controllers call — `i2c_devices`,
//! `spi_transfer_pins`, `uart_tx`, `uart_rx` — rather than programming a
//! peripheral register, so the routing is the thing under test.

#![cfg(feature = "esp32c3")]

use esp_soc::board::SpiPins;
use esp_soc::uart::UartRoute;
use litz_board::{Board as LitzBoard, FlatBoard, SpiDevice, UartPeer};
use litz_catalog::Registry;
use litz_core::Tick;
use litz_diagram::Circuit;
use litz_esp::backend::C3;

const MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x00, 0x01, 0x10];
const FLASH: usize = 4 << 20;

fn board(json: &str) -> LitzBoard {
    let registry = Registry::builtin();
    let circuit = Circuit::from_json(json).unwrap();
    LitzBoard::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap()
}

const BMP_DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "bmp1", "type": "litz-bmp280", "attrs": {}}
  ],
  "connections": [
    ["esp:GPIO8", "bmp1:SDA", "", []],
    ["esp:GPIO9", "bmp1:SCL", "", []],
    ["esp:3V3", "bmp1:VCC", "", []],
    ["esp:GND.1", "bmp1:GND", "", []]
  ]
}"#;

const EMPTY_DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}}
  ],
  "connections": []
}"#;

#[test]
fn esp32sim_receives_the_boards_i2c_device() {
    let mut mcu = C3::new(MAC, FLASH).with_board(board(BMP_DIAGRAM));
    let devices = mcu.board_model().i2c_devices();
    let (bus, address, mut device) = devices
        .into_iter()
        .find(|(_, address, _)| *address == 0x76)
        .expect("the BMP280 is attached at 0x76");
    assert_eq!((bus, address), (0, 0x76));

    assert!(
        device.start_address(0x76, 0x76, false),
        "write phase acknowledged"
    );
    assert!(device.write(0xd0));
    assert!(
        device.start_address(0x76, 0x76, true),
        "repeated start for the read"
    );
    assert_eq!(device.read(), 0x58, "the chip id");
    device.stop();
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
fn an_asserted_chip_select_reaches_its_spi_device() {
    let mut board = board(EMPTY_DIAGRAM);
    board
        .buses_mut()
        .spi_mut(0, 0)
        .attach(7, Box::new(Echo))
        .unwrap();
    let mut mcu = C3::new(MAC, FLASH).with_board(board);

    let selected = SpiPins {
        cs: 1 << 7,
        ..Default::default()
    };
    assert_eq!(
        mcu.board_model()
            .spi_transfer_pins(2, selected, &[0xde, 0xad], 2),
        vec![0xde, 0xad]
    );
    assert_eq!(
        mcu.board_model().spi_transfer(2, &[0x01], 1),
        vec![0x01],
        "the only device answers a controller-level transfer"
    );

    let unselected = SpiPins {
        cs: 1 << 6,
        ..Default::default()
    };
    assert_eq!(
        mcu.board_model()
            .spi_transfer_pins(2, unselected, &[0x00], 1),
        vec![0xff],
        "a floating MISO line"
    );
}

struct Reply {
    seen: Vec<u8>,
}

impl UartPeer for Reply {
    fn receive(&mut self, _at: Tick, bytes: &[u8]) {
        self.seen.extend_from_slice(bytes);
    }

    fn poll(&mut self, _at: Tick) -> Vec<u8> {
        if self.seen == b"A" {
            b"OK\n".to_vec()
        } else {
            Vec::new()
        }
    }
}

#[test]
fn uart_traffic_crosses_to_the_peer_and_back() {
    let mut board = board(EMPTY_DIAGRAM);
    board
        .buses_mut()
        .uart_mut(0, 0)
        .attach(0, Box::new(Reply { seen: Vec::new() }))
        .unwrap();
    let mut mcu = C3::new(MAC, FLASH).with_board(board);

    let route = UartRoute::new(0, 1 << 21, Some(20), Some(115_200));
    assert!(
        mcu.board_model().uart_rx(10).is_empty(),
        "the peer has nothing to say before it hears the chip"
    );
    mcu.board_model().uart_tx(10, route, b'A');

    let inputs = mcu.board_model().uart_rx(20);
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].pin, 20, "the chip's receive pin, from the route");
    assert_eq!(inputs[0].baud, 115_200);
    assert_eq!(inputs[0].data, b"OK\n");
}

const OLED_DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "oled", "type": "litz-ssd1306", "attrs": {}}
  ],
  "connections": [
    ["esp:GPIO8", "oled:SDA", "", []],
    ["esp:GPIO9", "oled:SCL", "", []],
    ["esp:3V3", "oled:VCC", "", []],
    ["esp:GND.1", "oled:GND", "", []]
  ]
}"#;

#[test]
fn a_display_on_the_board_becomes_a_capture_the_png_writer_can_read() {
    let mut mcu = C3::new(MAC, FLASH).with_board(board(OLED_DIAGRAM));
    let shared = mcu.board().unwrap().clone();
    // One GDDRAM byte: page 0, column 0, 0xaa. The SSD1306's control byte
    // 0x40 says the rest of the write is data, after the command that set the
    // page and the two column halves.
    {
        let mut board = shared.borrow_mut();
        let i2c = board.buses_mut().i2c_mut(0, 0);
        assert!(i2c.write(0x3c, &[0x00, 0xb0, 0x00, 0x10]));
        assert!(i2c.write(0x3c, &[0x40, 0xaa]));
    }

    let (width, height, pixels, version) = mcu
        .board_model()
        .display()
        .expect("the SSD1306 is the board's display");
    assert_eq!((width, height), (128, 64));
    assert_eq!(pixels[0], 0, "0xaa has bit 0 clear: the top pixel is off");
    assert_eq!(pixels[128], 0xffff, "bit 1 sets the second row of page 0");
    assert_eq!(pixels[256], 0, "bit 2 is clear again");
    assert_eq!(pixels[1], 0, "column 1 was never written");
    assert!(version > 0, "the panel counted a change");

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("oled-{}.png", std::process::id()));
    mcu.save_display(&path).expect("the PNG is written");
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"\x89PNG"), "a real PNG");
    assert!(bytes.len() > 100, "the frame is in it");
    let _ = std::fs::remove_file(path);
}
