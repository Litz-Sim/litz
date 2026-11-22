//! The C3 clears the shared chip contract: boot, run, console, pin and a
//! second identical run.

#![cfg(feature = "esp32c3")]

use litz_board::Chip;
use litz_chip_conformance::{Fixture, check};

const MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x00, 0x01, 0x10];
const FLASH: usize = 4 << 20;

const DIAGRAM: &str = r#"{"version":1,"parts":[
  {"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}},
  {"id":"r1","type":"litz-resistor","attrs":{"value":"220"}},
  {"id":"led1","type":"litz-led","attrs":{"color":"green"}}],
  "connections":[
  ["esp:GPIO8","r1:1","green",[]],
  ["r1:2","led1:A","green",[]],
  ["led1:C","esp:GND.1","black",[]]]}"#;

#[test]
fn the_c3_passes_the_chip_contract() {
    let firmware = litz_fixture::blinky(16_000_000);
    let fixture = Fixture::new("esp32c3", DIAGRAM, &firmware)
        .console("litz: blinky")
        .gpio(8);

    let report = check(
        |board| {
            let chip = litz_esp::backend::C3::new(MAC, FLASH).with_shared_board(board, 0);
            Ok(Box::new(chip) as Box<dyn Chip>)
        },
        &fixture,
    );

    assert!(report.is_ok(), "{report}");
}
