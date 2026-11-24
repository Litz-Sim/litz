//! Two development boards on one diagram.
//!
//! Every test places two ESP32-C3 boards and wires them — or a part on the
//! second board — through their header pins. The point is that the boards are
//! one electrical circuit while each keeps its own chip pins and its own input
//! queue: a level board 0 drives is a level board 1's chip sees, and board 1's
//! GPIO 4 is not board 0's GPIO 4.

use litz_board::flatten::{Endpoint, Rail};
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level, PartState};
use litz_diagram::Circuit;

fn board_part(id: &str) -> String {
    format!(r#"{{"id":"{id}","type":"board-esp32-c3-devkitm-1","attrs":{{}}}}"#)
}

/// A board whose own power switch starts off.
fn board_part_off(id: &str) -> String {
    format!(r#"{{"id":"{id}","type":"board-esp32-c3-devkitm-1","attrs":{{"power":false}}}}"#)
}

fn part(id: &str, type_key: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"{type_key}","attrs":{attrs}}}"#)
}

fn wire(from: &str, to: &str) -> String {
    format!(r#"["{from}","{to}","",[]]"#)
}

fn flat_board(parts: &[String], connections: &[String]) -> FlatBoard {
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
    FlatBoard::build(&circuit, &registry).expect("the diagram flattens")
}

fn built_board(parts: &[String], connections: &[String]) -> Board {
    Board::new(flat_board(parts, connections)).expect("the boards build")
}

fn part_index(flat: &FlatBoard, id: &str) -> u16 {
    flat.parts
        .iter()
        .position(|part| part.id == id)
        .expect("the part is flat") as u16
}

/// GPIO4 of one board wired to GPIO4 of the other is one net. Either chip can
/// drive it and the other sees the level, which is the foundation of MCU-to-
/// MCU signalling; the two pins are different pads with the same number.
#[test]
fn boards_wired_gpio_to_gpio_see_each_others_outputs() {
    let mut board = built_board(
        &[board_part("a"), board_part("b")],
        &[wire("a:GPIO4", "b:GPIO4")],
    );
    assert_eq!(board.chips(), 2, "both boards are present");

    // Board 0 drives high; board 1's input queue reports that level.
    board.set_chip_output_on(0, 4, Drive::strong(Level::High), 0);
    assert_eq!(board.chip_level_on(1, 4), Some(Level::High));
    assert!(
        board
            .take_chip_inputs_on(1)
            .iter()
            .any(|&(pin, level)| pin == 4 && level == Level::High),
        "board 1 sees board 0's high on its own GPIO4"
    );

    // Release board 0 first so the next drive is the only strong source, then
    // board 1 drives low and board 0 sees it.
    board.set_chip_output_on(0, 4, Drive::HIGH_Z, 10);
    board.set_chip_output_on(1, 4, Drive::strong(Level::Low), 20);
    assert_eq!(board.chip_level_on(0, 4), Some(Level::Low));
    assert!(
        board
            .take_chip_inputs_on(0)
            .iter()
            .any(|&(pin, level)| pin == 4 && level == Level::Low),
        "board 0 sees board 1's low on its own GPIO4"
    );
    assert!(board.conflicts().is_empty(), "the two drives never met");
}

/// A part placed at top level and wired to the second board's header is on
/// that board's chip, not the first board's: the same GPIO number on board 0
/// stays untouched.
#[test]
fn a_part_on_the_second_board_is_reached_through_its_header() {
    let mut board = built_board(
        &[
            board_part("a"),
            board_part("b"),
            part("r1", "litz-resistor", r#"{"value":"220"}"#),
            part("led1", "litz-led", r#"{"color":"green"}"#),
        ],
        &[
            wire("b:GPIO8", "r1:1"),
            wire("r1:2", "led1:A"),
            wire("led1:C", "b:GND.1"),
        ],
    );

    board.set_chip_output_on(1, 8, Drive::strong(Level::High), 0);
    assert_eq!(
        board.state("led1"),
        Some(PartState::Led { lit: true }),
        "the LED is driven by board 1's GPIO8"
    );
    assert_eq!(board.part_pin_level("led1", "C"), Some(Level::Low));
    assert_eq!(board.named_pin_on("r1"), Some((1, 8)));
    assert_eq!(board.chip_level_on(1, 8), Some(Level::High));
    // Board 0 never sees GPIO8 high: its own pad is a different node.
    assert_ne!(board.chip_level_on(0, 8), Some(Level::High));
}

/// Each board's fixed supply pins name its own regulator, so wiring two boards'
/// `3V3` pins together joins those supplies instead of one global net.
#[test]
fn a_wire_between_boards_joins_their_rails() {
    let mut board = built_board(
        &[
            board_part("a"),
            board_part("b"),
            part("r1", "litz-resistor", r#"{"value":"220"}"#),
            part("led1", "litz-led", r#"{"color":"green"}"#),
        ],
        &[
            // The two boards' ground and 3.3 V pins are explicitly tied.
            wire("a:GND.1", "b:GND.1"),
            wire("a:3V3", "b:3V3"),
            // Power comes from board 0, ground returns through board 1.
            wire("a:GPIO8", "r1:1"),
            wire("r1:2", "led1:A"),
            wire("led1:C", "b:GND.1"),
        ],
    );

    let flat = board.flat();
    assert!(
        flat.endpoint_net.contains_key(&Endpoint::Rail(Rail::Gnd)),
        "both boards' GND pins resolve to one global ground"
    );
    assert_eq!(
        flat.endpoint_net.get(&Endpoint::BoardRail {
            board: 0,
            rail: Rail::V33
        }),
        flat.endpoint_net.get(&Endpoint::BoardRail {
            board: 1,
            rail: Rail::V33
        }),
        "the wire joins the two boards' own 3.3 V rails into one net"
    );

    board.set_chip_output_on(0, 8, Drive::strong(Level::High), 0);
    assert_eq!(
        board.state("led1"),
        Some(PartState::Led { lit: true }),
        "current flows from board 0's GPIO through the LED to board 1's ground"
    );
}

/// Board 0 keeps the old spelling: a one-board diagram has exactly the
/// `Endpoint::Chip` endpoints it had before boards could be repeated, and
/// never a `ChipOn`.
#[test]
fn a_single_board_still_uses_endpoint_chip() {
    let flat = flat_board(
        &[
            board_part("esp"),
            part("r1", "litz-resistor", r#"{"value":"220"}"#),
        ],
        &[wire("esp:GPIO4", "r1:1"), wire("r1:2", "esp:GND.1")],
    );

    assert_eq!(flat.boards.len(), 1);
    assert!(flat.endpoint_net.contains_key(&Endpoint::Chip(4)));
    assert!(
        !flat
            .endpoint_net
            .keys()
            .any(|endpoint| matches!(endpoint, Endpoint::ChipOn { .. })),
        "board 0 never produces a ChipOn endpoint"
    );
    assert!(flat.chip_pins.contains(&4));
    assert!(
        flat.chip_pins_of
            .get(&0)
            .is_some_and(|pins| pins.contains(&4)),
        "the per-board pin set names board 0"
    );

    let board = Board::new(flat).expect("the board builds");
    assert_eq!(board.chips(), 1);
}

/// A UART part on the second board is reachable at the pin level: its TX pin
/// and board 1's GPIO20 are one flattened net, so a level driven there reaches
/// the part's pin. This is deliberately not the bus fabric — `FlatBuses` is
/// not keyed by board yet — so the test reads the wire only.
#[test]
fn a_uart_part_on_the_second_board_is_wired_at_the_pin_level() {
    let parts = [
        board_part("a"),
        board_part("b"),
        part("gps", "litz-gps-neo6m", "{}"),
    ];
    let connections = [
        wire("b:GPIO20", "gps:TX"),
        wire("b:GPIO21", "gps:RX"),
        wire("b:3V3", "gps:VCC"),
        wire("b:GND.1", "gps:GND"),
    ];
    let flat = flat_board(&parts, &connections);

    // The GPS's TX pin and board 1's GPIO20 are one net, and it is board 1's
    // pad rather than board 0's.
    let gps = part_index(&flat, "gps");
    let tx = flat.parts[gps as usize]
        .pins
        .iter()
        .position(|pin| pin == "TX")
        .expect("the GPS has a TX pin") as u16;
    let net = flat
        .endpoint_net
        .get(&Endpoint::Part { part: gps, pin: tx })
        .copied()
        .expect("the GPS TX pin is wired");
    assert!(
        flat.nets[net as usize]
            .iter()
            .any(|endpoint| matches!(endpoint, Endpoint::ChipOn { board: 1, pin: 20 })),
        "GPS TX shares a net with board 1's GPIO20"
    );
    assert!(
        !flat.nets[net as usize]
            .iter()
            .any(|endpoint| matches!(endpoint, Endpoint::Chip(20))),
        "the wire does not reach board 0's GPIO20"
    );

    let mut board = Board::new(flat).expect("the board builds");
    board.set_chip_output_on(1, 20, Drive::strong(Level::High), 0);
    assert_eq!(
        board.part_pin_level("gps", "TX"),
        Some(Level::High),
        "a level on board 1's GPIO20 reaches the GPS TX pin"
    );
    assert_eq!(board.chip_level_on(1, 20), Some(Level::High));
}

/// Two boards wired TX to RX are a UART link with no part on the bus. The
/// sender's route names its transmit pads, the netlist names the far end, and
/// the receiver's route says it is a receive pad, so the bytes cross between
/// the two chips' `(board, port)` queues.
#[test]
fn uart_bytes_cross_a_wire_between_two_boards() {
    let mut board = built_board(
        &[board_part("a"), board_part("b")],
        &[wire("a:GPIO6", "b:GPIO5"), wire("b:GPIO4", "a:GPIO7")],
    );
    board.set_uart_pins(0, 1, 1 << 6, Some(7));
    board.set_uart_pins(1, 1, 1 << 4, Some(5));

    assert!(
        board.uart_receive(0, 1, 10, b"hello"),
        "the wire takes the bytes even though no part listens"
    );
    assert_eq!(board.uart_poll(1, 1, 20), b"hello");

    assert!(board.uart_receive(1, 1, 30, b"pico"));
    assert_eq!(board.uart_poll(0, 1, 30), b"pico");

    // A byte stamped later than the poll waits for the next one.
    assert!(board.uart_receive(0, 1, 40, b"later"));
    assert!(board.uart_poll(1, 1, 35).is_empty());
    assert_eq!(board.uart_poll(1, 1, 40), b"later");
}

/// A UART is crossed, not straight: a transmitter wired to another
/// transmitter reaches no receiver and carries nothing.
#[test]
fn a_wire_between_two_transmitters_carries_nothing() {
    let mut board = built_board(
        &[board_part("a"), board_part("b")],
        &[wire("a:GPIO6", "b:GPIO4")],
    );
    board.set_uart_pins(0, 1, 1 << 6, Some(7));
    board.set_uart_pins(1, 1, 1 << 4, Some(5));

    assert!(!board.uart_receive(0, 1, 10, b"lost"));
    assert!(board.uart_poll(1, 1, 20).is_empty());
}

/// A board starts powered, and its switch takes the whole board with it: the
/// regulator's 3.3 V rail goes quiet and the onboard power LED goes dark.
#[test]
fn a_board_can_be_switched_off_and_back_on() {
    let mut board = built_board(&[board_part("a")], &[]);
    assert!(board.board_powered(0), "a board starts powered");
    assert_eq!(
        board.state("a/led_pwr"),
        Some(PartState::Led { lit: true }),
        "the onboard power LED runs from the board's own 3.3 V"
    );

    assert!(board.set_board_power(0, false, 100));
    assert!(!board.board_powered(0));
    assert_eq!(
        board.state("a/led_pwr"),
        Some(PartState::Led { lit: false }),
        "no regulator, no 3.3 V, no light"
    );

    assert!(board.set_board_power(0, true, 200));
    assert!(board.board_powered(0));
    assert_eq!(board.state("a/led_pwr"), Some(PartState::Led { lit: true }));
}

/// A board can start with its switch off and be powered from another board's
/// 5 V header. Turning the supplying board off takes both down, which is the
/// one-board-manages-another case a diagram needs.
#[test]
fn one_board_supply_switches_another_on() {
    let mut board = built_board(
        &[board_part("a"), board_part_off("b")],
        &[wire("a:5V", "b:5V")],
    );
    assert!(board.board_power_switch(0));
    assert!(!board.board_power_switch(1), "board b starts switched off");
    assert!(board.board_powered(1), "board a's 5 V powers board b");
    assert_eq!(
        board.state("b/led_pwr"),
        Some(PartState::Led { lit: true }),
        "board b's own regulator runs from the supplied 5 V"
    );

    board.set_board_power(0, false, 100);
    assert!(!board.board_powered(1), "the supply is gone with board a");
    assert_eq!(
        board.state("b/led_pwr"),
        Some(PartState::Led { lit: false }),
        "board b's power LED goes dark with its supply"
    );

    board.set_board_power(1, true, 200);
    assert!(board.board_powered(1), "board b can also power itself");
}

/// A scope can watch any header pin, including a supply pin, and any pad that
/// is not wired at all reads as the high impedance it is.
#[test]
fn any_header_pin_is_a_scope_point() {
    let mut board = built_board(&[board_part("a")], &[]);
    board.add_probe("v33", "a:3V3").expect("a supply pin");
    board.add_probe("gnd", "a:GND").expect("a ground pin");
    board.add_probe("gpio0", "a:GPIO0").expect("a GPIO by name");
    board.add_probe("pad0", "a:0").expect("a GPIO by number");
    board
        .add_probe("unwired", "a:GPIO10")
        .expect("an unwired pad is still a point");
    assert_eq!(board.probes().len(), 5);
    assert!(board.add_probe("typo", "a:NOPE").is_err());
}
