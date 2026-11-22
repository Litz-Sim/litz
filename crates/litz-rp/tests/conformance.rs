//! The RP2040 clears the shared chip contract: boot, run, console, pin and a
//! second identical run.

use litz_board::Chip;
use litz_chip_conformance::{Fixture, check};

const DIAGRAM: &str = r#"{"version":1,"parts":[
  {"id":"pico","type":"board-raspberry-pi-pico","attrs":{}},
  {"id":"r1","type":"litz-resistor","attrs":{"value":"330"}},
  {"id":"led1","type":"litz-led","attrs":{"color":"green"}}],
  "connections":[
  ["pico:GP15","r1:1","green",[]],
  ["r1:2","led1:A","green",[]],
  ["led1:C","pico:GND.1","black",[]]]}"#;

#[test]
fn the_rp2040_passes_the_chip_contract() {
    let firmware = litz_fixture::pico_blink(15, 1);
    let fixture = Fixture::new("rp2040", DIAGRAM, &firmware)
        .console("litz: pico")
        .gpio(15);

    let report = check(
        |board| {
            let chip = litz_rp::Rp2040::new().with_shared_board(board, 0);
            Ok(Box::new(chip) as Box<dyn Chip>)
        },
        &fixture,
    );

    assert!(report.is_ok(), "{report}");
}
