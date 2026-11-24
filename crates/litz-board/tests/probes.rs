//! Probes: a point named in the circuit, and the record a scope reads back.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level};
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;
const WIRES: &[&str] = &[
    r#"["esp:GPIO8","r1:1","",[]]"#,
    r#"["r1:2","led1:A","",[]]"#,
    r#"["led1:C","esp:GND.1","",[]]"#,
];

fn board() -> Board {
    let json = format!(
        r#"{{"version":1,"parts":[{BOARD},
            {{"id":"r1","type":"litz-resistor","attrs":{{"value":"220"}}}},
            {{"id":"led1","type":"litz-led","attrs":{{"color":"green"}}}}],
            "connections":[{}]}}"#,
        WIRES.join(",")
    );
    let circuit = Circuit::from_json(&json).unwrap();
    Board::new(FlatBoard::build(&circuit, &Registry::builtin()).unwrap()).unwrap()
}

#[test]
fn a_probe_names_a_chip_pin_or_a_part_pin() {
    let mut board = board();
    board.add_probe("gpio", "esp:GPIO8").unwrap();
    board.add_probe("anode", "led1:A").unwrap();
    let probes = board.probes();
    assert_eq!(probes.len(), 2);
    assert_eq!(probes[0].point, "esp:GPIO8");
    assert_ne!(
        probes[0].net, probes[1].net,
        "the resistor separates the two points"
    );
}

#[test]
fn a_probe_point_that_is_not_there_is_refused() {
    let mut board = board();
    let unknown_part = board.add_probe("p", "nope:A").unwrap_err();
    assert!(unknown_part.contains("neither a board"), "{unknown_part}");
    let unknown_pin = board.add_probe("p", "led1:Q").unwrap_err();
    assert!(unknown_pin.contains("has no pin"), "{unknown_pin}");
    let no_colon = board.add_probe("p", "led1").unwrap_err();
    assert!(no_colon.contains("<board>:<GPIO>"), "{no_colon}");
    assert!(board.probes().is_empty(), "none of those were recorded");
}

/// An unwired pad is still a point: a scope can watch anything on the board,
/// including a pin nothing drives, which reads high impedance.
#[test]
fn a_probe_on_an_unwired_pin_reads_high_impedance() {
    let mut board = board();
    board.add_probe("unused", "esp:GPIO9").unwrap();
    board.add_probe("supply", "esp:3V3").unwrap();
    board.start_trace();
    let channels = board.scope();
    assert_eq!(channels.len(), 2);
    assert_eq!(
        channels[0].series.digital.last().map(|&(_, level)| level),
        Some(Level::HighZ),
        "an unwired pin holds nothing"
    );
    assert_eq!(
        channels[1].series.digital.last().map(|&(_, level)| level),
        Some(Level::High),
        "a powered board's 3V3 header is high"
    );
}

#[test]
fn a_scope_reports_the_edges_the_chip_drove() {
    let mut board = board();
    board.add_probe("gpio", "esp:GPIO8").unwrap();
    board.start_trace();
    board.set_chip_output(8, Drive::strong(Level::High), 100);
    board.set_chip_output(8, Drive::strong(Level::Low), 200);

    let channels = board.scope();
    assert_eq!(channels.len(), 1);
    let series = &channels[0].series;
    assert_eq!(
        series.digital.first(),
        Some(&(0, Level::HighZ)),
        "the channel starts at the level the net held when recording began"
    );
    assert_eq!(
        series.digital.last(),
        Some(&(200, Level::Low)),
        "both drives are in the record"
    );
    assert_eq!(channels[0].measured.edges, 1);
    assert_eq!(channels[0].measured.high_ns, 100);
}

#[test]
fn a_scope_records_the_solved_voltage_at_each_settle() {
    let mut board = board();
    board.set_analog(true);
    board.add_probe("gpio", "esp:GPIO8").unwrap();
    board.start_trace();
    board.set_chip_output(8, Drive::strong(Level::High), 0);
    board.set_chip_output(8, Drive::strong(Level::Low), 1_000);

    let channels = board.scope();
    let volts = &channels[0].series.volts;
    assert_eq!(volts.len(), 2, "one sample per settle: {volts:?}");
    let high = volts[0].1;
    let low = volts[1].1;
    assert!(high > 3.0, "the driven pin sources logic high: {high}");
    assert!(low < 0.3, "a released pin falls to the pull-down: {low}");
    assert_eq!(channels[0].measured.max_volts, Some(high));
    assert_eq!(channels[0].measured.min_volts, Some(low));
}
