//! Parts that schedule their own edges.
//!
//! The board only used to move when a chip drove a pin. A sensor that answers
//! later — an HC-SR04 raising ECHO after a measurement, a DHT22 shifting a
//! frame — needs the board's clock moved too, or its edges never reach the
//! chip. These tests are the schedule's contract.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level};
use litz_diagram::Circuit;

fn sonar_board() -> Board {
    let circuit = Circuit::from_json(
        r#"{
          "parts": [
            {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
            {"id": "sonar", "type": "litz-hc-sr04", "attrs": {"distance": 100.0}}
          ],
          "connections": [
            ["esp:GPIO4", "sonar:TRIG", "", []],
            ["esp:GPIO5", "sonar:ECHO", "", []],
            ["sonar:VCC", "esp:3V3", "", []],
            ["sonar:GND", "esp:GND.1", "", []]
          ]
        }"#,
    )
    .unwrap();
    Board::new(FlatBoard::build(&circuit, &Registry::builtin()).unwrap()).unwrap()
}

#[test]
fn an_hc_sr04_raises_echo_on_the_boards_clock() {
    let mut board = sonar_board();
    board.set_chip_output(4, Drive::strong(Level::High), 0);
    board.set_chip_output(4, Drive::strong(Level::Low), 20_000);
    assert_eq!(
        board.next_event(),
        Some(70_000),
        "echo follows the trigger by 50 us"
    );

    board.advance_to(70_000);
    let inputs = board.take_chip_inputs();
    assert!(
        inputs.contains(&(5, Level::High)),
        "echo went high: {inputs:?}"
    );

    board.advance_to(70_000 + 5_800_000);
    let inputs = board.take_chip_inputs();
    assert!(
        inputs.contains(&(5, Level::Low)),
        "echo went low: {inputs:?}"
    );
    assert_eq!(board.next_event(), None, "the measurement is finished");
}

#[test]
fn advancing_past_several_events_still_settles_at_the_last_level() {
    let mut board = sonar_board();
    board.set_chip_output(4, Drive::strong(Level::High), 0);
    board.set_chip_output(4, Drive::strong(Level::Low), 20_000);
    board.advance_to(10_000_000);
    let inputs = board.take_chip_inputs();
    let echo = inputs.iter().rfind(|(pin, _)| *pin == 5).copied();
    assert_eq!(echo, Some((5, Level::Low)));
}

#[test]
fn a_scheduled_edge_keeps_its_own_timestamp_in_the_waveform() {
    let mut board = sonar_board();
    board.take_chip_inputs();
    board.start_trace();
    board.set_chip_output(4, Drive::strong(Level::High), 0);
    board.set_chip_output(4, Drive::strong(Level::Low), 20_000);
    board.advance_to(70_000 + 5_800_000);
    let vcd = board.vcd();
    assert!(
        vcd.contains("#70000"),
        "echo rise is dated, not collapsed:\n{vcd}"
    );
    assert!(vcd.contains("#5870000"), "echo fall is dated:\n{vcd}");
}

#[test]
fn a_board_with_nothing_scheduled_does_not_move() {
    let circuit = Circuit::from_json(
        r#"{
          "parts": [
            {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
            {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
          ],
          "connections": [
            ["esp:GPIO8", "led1:A", "", []],
            ["led1:C", "esp:GND.1", "", []]
          ]
        }"#,
    )
    .unwrap();
    let mut board = Board::new(FlatBoard::build(&circuit, &Registry::builtin()).unwrap()).unwrap();
    assert_eq!(board.next_event(), None);
    board.take_chip_inputs();
    board.advance_to(1_000_000);
    assert!(board.take_chip_inputs().is_empty());
}
