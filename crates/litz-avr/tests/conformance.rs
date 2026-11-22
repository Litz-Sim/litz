//! The ATmega328P clears the shared chip contract on the Uno's own board.

use litz_board::Chip;
use litz_chip_conformance::{Fixture, check};

const DIAGRAM: &str = r#"{"version":1,"parts":[{"id":"uno","type":"board-arduino-uno","attrs":{}}],"connections":[]}"#;

/// Six bytes of hand-assembled AVR that light the onboard D13 LED.
const FIRMWARE: &[u8] = include_bytes!("../../../examples/uno-blink/firmware/uno-blink.hex");

#[test]
fn the_atmega328p_passes_the_chip_contract() {
    let fixture = Fixture::new("atmega328p", DIAGRAM, FIRMWARE)
        .gpio(13)
        .budget(10_000);

    let report = check(
        |board| {
            let chip = litz_avr::Atmega328p::new().with_shared_board(board, 0);
            Ok(Box::new(chip) as Box<dyn Chip>)
        },
        &fixture,
    );

    assert!(report.is_ok(), "{report}");
}
