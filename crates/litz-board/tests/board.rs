use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level, PartState};
use litz_diagram::Circuit;

fn board(parts: &[&str], connections: &[&str]) -> Board {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        r#"]}"#,
    ]
    .concat();
    let circuit = Circuit::from_json(&json).unwrap();
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap()
}

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;

#[test]
fn board_expands_with_prefixed_children_and_alias_pins() {
    let circuit = Circuit::from_json(
        &[
            r#"{"parts":["#,
            BOARD,
            r#"],"connections":[["esp:TX","$serialMonitor:RX","",[]]]}"#,
        ]
        .concat(),
    )
    .unwrap();
    let registry = Registry::builtin();
    let flat = FlatBoard::build(&circuit, &registry).unwrap();

    assert!(
        flat.part("esp/led_pwr").is_some(),
        "the onboard power LED is a child part"
    );
    assert!(flat.part("esp/r_pwr").is_some());
    assert!(flat.chip_pins.contains(&21), "TX is GPIO21 on this board");
}

#[test]
fn onboard_power_led_is_lit_from_the_rail() {
    let board = board(&[BOARD], &[]);
    assert_eq!(
        board.state("esp/led_pwr"),
        Some(PartState::Led { lit: true })
    );
}

#[test]
fn series_resistor_lights_the_led_from_a_pin() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{}}"#,
            r#"{"id":"led1","type":"litz-led","attrs":{}}"#,
        ],
        &[
            r#"["esp:GPIO8","r1:1","",[]]"#,
            r#"["r1:2","led1:A","",[]]"#,
            r#"["led1:C","esp:GND.1","",[]]"#,
        ],
    );

    assert_eq!(board.state("led1"), Some(PartState::Led { lit: false }));
    board.set_chip_output(8, Drive::strong(Level::High), 0);
    assert_eq!(board.state("led1"), Some(PartState::Led { lit: true }));
    board.set_chip_output(8, Drive::strong(Level::Low), 0);
    assert_eq!(board.state("led1"), Some(PartState::Led { lit: false }));
}

#[test]
fn resistor_from_a_rail_pulls_an_input_high() {
    let mut board = board(
        &[BOARD, r#"{"id":"r1","type":"litz-resistor","attrs":{}}"#],
        &[
            r#"["esp:3V3","r1:1","",[]]"#,
            r#"["r1:2","esp:GPIO9","",[]]"#,
        ],
    );
    let inputs = board.take_chip_inputs();
    assert!(inputs.contains(&(9, Level::High)), "inputs were {inputs:?}");
}

#[test]
fn gpio_against_ground_is_a_short() {
    let mut board = board(&[BOARD], &[r#"["esp:GPIO8","esp:GND.1","",[]]"#]);
    assert!(board.conflicts().is_empty(), "nothing is driving yet");
    board.set_chip_output(8, Drive::strong(Level::High), 0);
    let conflicts = board.conflicts();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert!(conflicts[0].detail.contains("GPIO8"), "{conflicts:?}");
}

/// A pull-up and a pull-down on one input is a divider, not a fault. Two
/// states cannot say "half way", so the pin reads floating — but the circuit
/// is legal and the run must go on.
#[test]
fn a_resistor_divider_reads_floating_without_a_conflict() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{}}"#,
            r#"{"id":"r2","type":"litz-resistor","attrs":{}}"#,
        ],
        &[
            r#"["esp:3V3","r1:1","",[]]"#,
            r#"["r1:2","esp:GPIO9","",[]]"#,
            r#"["esp:GPIO9","r2:1","",[]]"#,
            r#"["r2:2","esp:GND.1","",[]]"#,
        ],
    );
    assert!(board.conflicts().is_empty(), "{:?}", board.conflicts());
    let inputs = board.take_chip_inputs();
    assert!(
        inputs.contains(&(9, Level::HighZ)),
        "inputs were {inputs:?}"
    );
}

#[test]
fn a_named_part_resolves_to_its_chip_pin() {
    let board = board(
        &[
            BOARD,
            r#"{"id":"btn1","type":"litz-pushbutton","attrs":{}}"#,
        ],
        &[
            r#"["esp:GPIO0","btn1:1.l","",[]]"#,
            r#"["btn1:2.l","esp:GND.1","",[]]"#,
        ],
    );
    assert_eq!(board.named_pin("btn1"), Some(0));
}

/// Host interaction goes through the board, not around it: pressing the part
/// makes the net follow, so a switch wired to ground really does pull the pin
/// down and a switch in series with an LED really does light it.
#[test]
fn pressing_a_button_moves_its_net() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"btn1","type":"litz-pushbutton","attrs":{}}"#,
        ],
        &[
            r#"["esp:GPIO0","btn1:1.l","",[]]"#,
            r#"["btn1:2.l","esp:GND.1","",[]]"#,
        ],
    );
    assert!(
        board
            .take_chip_inputs()
            .iter()
            .all(|&(_, level)| level == Level::HighZ)
    );

    assert!(board.press("btn1", true, 0));
    assert_eq!(
        board.state("btn1"),
        Some(PartState::Button { pressed: true })
    );
    let inputs = board.take_chip_inputs();
    assert!(inputs.contains(&(0, Level::Low)), "inputs were {inputs:?}");

    assert!(board.press("btn1", false, 0));
    assert_eq!(
        board.state("btn1"),
        Some(PartState::Button { pressed: false })
    );
    let inputs = board.take_chip_inputs();
    assert!(
        inputs.contains(&(0, Level::HighZ)),
        "inputs were {inputs:?}"
    );
}

#[test]
fn a_part_without_a_press_input_refuses_one() {
    let mut board = board(
        &[BOARD, r#"{"id":"led1","type":"litz-led","attrs":{}}"#],
        &[],
    );
    assert!(!board.press("led1", true, 0));
    assert!(!board.press("nobody", true, 0));
}

/// A slide switch is latching, so a click throws it and it stays thrown: the
/// supply reaches the display and only the segment that is wired lights.
#[test]
fn throwing_a_switch_lights_one_segment() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"sw1","type":"litz-slide-switch","attrs":{}}"#,
            r#"{"id":"seg1","type":"litz-7segment","attrs":{"color":"red"}}"#,
        ],
        &[
            r#"["esp:3V3","sw1:3","",[]]"#,
            r#"["sw1:2","seg1:a","",[]]"#,
            r#"["seg1:com","esp:GND.1","",[]]"#,
        ],
    );
    assert_eq!(board.state("sw1"), Some(PartState::Switch { on: false }));
    assert_eq!(board.state("seg1"), Some(PartState::Segments { mask: 0 }));

    assert!(board.press("sw1", true, 0));
    assert_eq!(board.state("sw1"), Some(PartState::Switch { on: true }));
    assert_eq!(board.state("seg1"), Some(PartState::Segments { mask: 1 }));

    // Releasing the click must not undo the throw.
    assert!(board.press("sw1", false, 0));
    assert_eq!(board.state("sw1"), Some(PartState::Switch { on: true }));
}

/// A potentiometer across the supply is a voltage divider: the wiper sits at
/// a fraction of 3.3 V, which is what an ADC pin reads. Everything else on the
/// net still resolves digitally, so the wiper is a high-impedance observer.
#[test]
fn a_potentiometer_sets_the_wiper_voltage() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"pot1","type":"litz-potentiometer","attrs":{"value":"50"}}"#,
        ],
        &[
            r#"["esp:3V3","pot1:1","",[]]"#,
            r#"["pot1:2","esp:GPIO0","",[]]"#,
            r#"["pot1:3","esp:GND.1","",[]]"#,
        ],
    );
    assert_eq!(board.state("pot1"), Some(PartState::Knob { permille: 500 }));
    let volts = |board: &Board| {
        board
            .analog_inputs()
            .into_iter()
            .find(|&(pin, _)| pin == 0)
            .map(|(_, v)| v)
    };
    assert!(
        (volts(&board).unwrap() - 1.65).abs() < 0.01,
        "{:?}",
        volts(&board)
    );

    // The travel runs from pin 1 to pin 3, so turning it toward each end
    // parks the wiper on that end's rail.
    assert!(board.set_control("pot1", 0, 0));
    assert!((volts(&board).unwrap() - 3.3).abs() < 0.01);
    assert!(board.set_control("pot1", 1000, 0));
    assert_eq!(volts(&board), Some(0.0));

    assert!(!board.set_control("led1", 0, 0), "an LED has no control");
    assert!(!board.set_control("nobody", 0, 0));
}

#[test]
fn a_diagram_without_a_board_is_rejected() {
    let circuit = Circuit::from_json(
        r#"{"parts":[{"id":"led1","type":"litz-led","attrs":{}}],"connections":[]}"#,
    )
    .unwrap();
    let registry = Registry::builtin();
    let err = FlatBoard::build(&circuit, &registry)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no development board"), "{err}");
}

/// A trace is the same electrical model read back out: every transition the
/// fixed point made, with the tick it happened at, in a file PulseView opens.
#[test]
fn a_trace_records_net_transitions_as_vcd() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"220"}}"#,
            r#"{"id":"led1","type":"litz-led","attrs":{}}"#,
        ],
        &[
            r#"["esp:GPIO8","r1:1","",[]]"#,
            r#"["r1:2","led1:A","",[]]"#,
            r#"["led1:C","esp:GND.1","",[]]"#,
        ],
    );
    board.start_trace();
    board.set_chip_output(8, Drive::strong(Level::High), 1_000);
    board.set_chip_output(8, Drive::strong(Level::Low), 2_000);

    let vcd = board.vcd();
    assert!(vcd.starts_with("$timescale 1ns $end"), "{vcd}");
    assert!(vcd.contains("$var wire 1 n"), "{vcd}");
    assert!(
        vcd.contains("_gpio8"),
        "nets are named after their endpoint: {vcd}"
    );
    assert!(vcd.contains("#1000"), "{vcd}");
    assert!(vcd.contains("#2000"), "{vcd}");
    assert!(vcd.contains("0n0"), "the low level is in the dump: {vcd}");
    assert!(board.trace_len() >= 3, "{}", board.trace_len());
}
