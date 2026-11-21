//! The GP-SPI clock the firmware programmed reaches the board's wires.
//!
//! A transfer callback carries bytes, not a clock, so the adapter watches the
//! controller's clock register and hands the rate to the board just before the
//! transaction. These tests write the register the way a driver does and then
//! make one transfer through the same `BoardModel` entry point the chip uses.

#![cfg(any(feature = "esp32", feature = "esp32s3"))]

use esp_soc::board::SpiPins;
use litz_board::{Board as LitzBoard, FlatBoard, SpiDevice};
use litz_catalog::Registry;
use litz_diagram::Circuit;

const MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x00, 0x01, 0x10];
const FLASH: usize = 4 << 20;

const DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}}
  ],
  "connections": []
}"#;

struct Echo;

impl SpiDevice for Echo {
    fn select(&mut self, _selected: bool) {}

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        let mut out = tx.to_vec();
        out.resize(rx_len, 0);
        out
    }
}

/// A board with one SPI device on controller 0, chip select 7.
fn board() -> LitzBoard {
    let registry = Registry::builtin();
    let circuit = Circuit::from_json(DIAGRAM).unwrap();
    let mut board = LitzBoard::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap();
    board
        .buses_mut()
        .spi_mut(0, 0)
        .attach(7, Box::new(Echo))
        .unwrap();
    board
}

/// Program `pre=1, n=16`, i.e. APB (80 MHz) / 16 = 5 MHz, then make one
/// transfer and read back what the board will use for the waveform.
fn transfer_at_registers<S: esp_soc::Soc>(
    mut chip: litz_esp::backend::EspChip<S>,
    clock: u32,
) -> u32 {
    chip.load_script(&format!("0 poke {clock:#x} {:#x}", 15 << 12))
        .unwrap();
    chip.run(1);
    let pins = SpiPins {
        cs: 1 << 7,
        ..Default::default()
    };
    chip.board_model().spi_transfer_pins(2, pins, &[0x01], 0);
    let board = chip.board().unwrap().borrow();
    board.spi_hz(0, 0)
}

#[cfg(feature = "esp32")]
#[test]
fn the_esp32_gp_spi_programmed_divider_reaches_the_board() {
    let chip = litz_esp::backend::Esp32::new(MAC, FLASH).with_board(board());
    assert_eq!(transfer_at_registers(chip, 0x3ff6_4018), 5_000_000);
}

#[cfg(feature = "esp32s3")]
#[test]
fn the_s3_gp_spi_programmed_divider_reaches_the_board() {
    let chip = litz_esp::backend::S3::new(MAC, FLASH).with_board(board());
    assert_eq!(transfer_at_registers(chip, 0x6002_400c), 5_000_000);
}
