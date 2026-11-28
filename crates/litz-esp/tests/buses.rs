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

const SUPPLY_DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "ps", "type": "litz-power-supply", "attrs": {"voltage": 9}},
    {"id": "dac", "type": "litz-mcp4725", "attrs": {}},
    {"id": "reg", "type": "litz-voltage-regulator", "attrs": {"variant": "LM317"}},
    {"id": "adc", "type": "litz-ads1115", "attrs": {}},
    {"id": "r1", "type": "litz-resistor", "attrs": {"value": "10k"}},
    {"id": "r2", "type": "litz-resistor", "attrs": {"value": "10k"}},
    {"id": "load", "type": "litz-resistor", "attrs": {"value": "220"}}
  ],
  "connections": [
    ["ps:+", "reg:VI", "", []],
    ["ps:-", "esp:GND.1", "", []],
    ["reg:GND", "esp:GND.1", "", []],
    ["dac:VOUT", "reg:ADJ", "", []],
    ["dac:VCC", "esp:3V3", "", []],
    ["dac:GND", "esp:GND.1", "", []],
    ["dac:SDA", "esp:GPIO8", "", []],
    ["dac:SCL", "esp:GPIO9", "", []],
    ["adc:VDD", "esp:3V3", "", []],
    ["adc:GND", "esp:GND.1", "", []],
    ["adc:SDA", "esp:GPIO8", "", []],
    ["adc:SCL", "esp:GPIO9", "", []],
    ["reg:VO", "r1:1", "", []],
    ["r1:2", "adc:A0", "", []],
    ["adc:A0", "r2:1", "", []],
    ["r2:2", "esp:GND.1", "", []],
    ["reg:VO", "load:1", "", []],
    ["load:2", "esp:GND.1", "", []]
  ]
}"#;

/// Traffic the chip drives itself — the DAC's registers and the ADC's
/// conversion — keeps the analog view in step: a code the firmware latches
/// moves the regulator, and the ADC's channel pin is read from the solve.
#[test]
fn a_firmware_dac_write_moves_the_regulator_and_the_adc_reads_it() {
    let mut board = board(SUPPLY_DIAGRAM);
    board.set_analog(true);
    let mut mcu = C3::new(MAC, FLASH).with_board(board);
    let shared = mcu.board().unwrap().clone();
    let devices = mcu.board_model().i2c_devices();
    let mut dac = devices
        .into_iter()
        .find(|(_, address, _)| *address == 0x60)
        .map(|(_, _, device)| device)
        .expect("the MCP4725 is attached at 0x60");

    // Fast write of 0x0fc0 = 4032, which is 3.249 V on the 3.3 V rail.
    assert!(dac.start_address(0x60, 0x60, false));
    assert!(dac.write(0x0f));
    assert!(dac.write(0xc0));
    dac.stop();

    let adj = shared
        .borrow()
        .node_voltage("reg", "ADJ")
        .expect("the ADJ pin is solved");
    assert!(
        (adj - 3.249).abs() < 0.01,
        "the regulator's ADJ pin sees {adj} V"
    );
    let vo = shared
        .borrow()
        .node_voltage("reg", "VO")
        .expect("the output is solved");
    assert!(
        (vo - 4.499).abs() < 0.02,
        "the output follows ADJ + 1.25 V, but sits at {vo} V"
    );

    // AIN0 single-ended, ±4.096 V, single shot: the divider halves the output.
    let devices = mcu.board_model().i2c_devices();
    let mut adc = devices
        .into_iter()
        .find(|(_, address, _)| *address == 0x48)
        .map(|(_, _, device)| device)
        .expect("the ADS1115 is attached at 0x48");
    assert!(adc.start_address(0x48, 0x48, false));
    assert!(adc.write(0x01));
    assert!(adc.write(0xc3));
    assert!(adc.write(0x83));
    adc.stop();
    assert!(adc.start_address(0x48, 0x48, false));
    assert!(adc.write(0x00));
    assert!(adc.start_address(0x48, 0x48, true));
    let high = adc.read();
    let low = adc.read();
    adc.stop();

    let code = i16::from_be_bytes([high, low]);
    let expected = (32767.0_f64 * (vo / 2.0) / 4.096).round() as i16;
    assert!(
        (code - expected).abs() <= 2,
        "the ADC converted {code}, expected {expected} from {vo} V through the divider"
    );
}
