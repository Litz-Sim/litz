//! Decoding a probe record the way a project asks for it.

use litz_board::probe::{Channel, Measurements, Probe, Series};
use litz_core::Level;
use litz_engine::config::DecodeSection;
use litz_engine::decode;

fn channel(name: &str, points: &[(u64, Level)]) -> Channel {
    Channel {
        probe: Probe {
            name: name.into(),
            point: name.into(),
            net: 0,
        },
        series: Series {
            digital: points.to_vec(),
            volts: Vec::new(),
        },
        measured: Measurements::default(),
    }
}

/// One 8N1 frame of `byte`, idle high before and after. `bit` is the bit
/// period in ticks.
fn uart_frame(start: u64, bit: u64, byte: u8) -> Vec<(u64, Level)> {
    let mut points = vec![(0, Level::High), (start, Level::Low)];
    let mut at = start;
    for index in 0..8 {
        let level = if byte & (1 << index) == 0 {
            Level::Low
        } else {
            Level::High
        };
        at += bit;
        if points.last().map(|&(_, held)| held) != Some(level) {
            points.push((at, level));
        }
    }
    at += bit;
    if points.last().map(|&(_, held)| held) != Some(Level::High) {
        points.push((at, Level::High));
    }
    points
}

#[test]
fn a_project_decoder_turns_a_probe_record_into_text() {
    let points = uart_frame(1_000, 104_167, b'A');
    let channels = [channel("tx", &points)];
    let spec: DecodeSection = "uart:rx=tx,baud=9600".parse().unwrap();

    let lines = decode::report([&spec], &channels).expect("the channel exists");
    assert_eq!(lines, vec![r#"uart tx: "A""#]);
}

#[test]
fn an_absent_bit_rate_is_taken_from_the_shortest_run() {
    let points = uart_frame(1_000, 104_167, 0x55);
    let channels = [channel("tx", &points)];
    let spec: DecodeSection = "uart:rx=tx".parse().unwrap();

    let lines = decode::report([&spec], &channels).expect("the channel exists");
    assert_eq!(lines, vec![r#"uart tx: "U""#]);
}

#[test]
fn a_decoder_that_sees_no_frame_says_so() {
    let channels = [channel("tx", &[(0, Level::High)])];
    let spec: DecodeSection = "uart:rx=tx".parse().unwrap();
    assert_eq!(
        decode::report([&spec], &channels).unwrap(),
        vec!["uart tx: no frames"]
    );
}

#[test]
fn a_decoder_naming_an_unprobed_channel_is_refused() {
    let channels = [channel("tx", &uart_frame(1_000, 100, b'A'))];
    let spec: DecodeSection = "uart:rx=missing".parse().unwrap();

    let error = decode::report([&spec], &channels).unwrap_err();
    assert!(error.contains("missing"), "{error}");
}

#[test]
fn check_accepts_a_name_a_probe_uses_and_refuses_any_other() {
    let probes = [Probe {
        name: "sda".into(),
        point: "esp:GPIO4".into(),
        net: 3,
    }];
    let good: DecodeSection = "i2c:sda=sda,scl=scl".parse().unwrap();
    let error = decode::check(&good, &probes).unwrap_err();
    assert!(error.contains("scl"), "{error}");

    let other: DecodeSection = "i2c:sda=missing,scl=scl".parse().unwrap();
    let error = decode::check(&other, &probes).unwrap_err();
    assert!(error.contains("missing"), "{error}");
}

/// A real board, a real device and a probed bus: the transaction the part
/// answered is the waveform the decoder reads back. This is the join between
/// the byte-level bus fabric and the pin-level recorder.
#[test]
fn a_board_transaction_decodes_from_its_probed_pins() {
    use litz_board::{Board, FlatBoard};
    use litz_catalog::Registry;
    use litz_diagram::Circuit;

    let diagram = r#"{
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
    let flat =
        FlatBoard::build(&Circuit::from_json(diagram).unwrap(), &Registry::builtin()).unwrap();
    let mut board = Board::new(flat).unwrap();
    board.add_probe("sda", "esp:GPIO8").unwrap();
    board.add_probe("scl", "esp:GPIO9").unwrap();
    board.start_trace();
    board
        .i2c_write_read(0, 0, 0x76, &[0xd0], 1)
        .expect("the sensor answers");

    let channels = board.scope();
    let spec: DecodeSection = "i2c:sda=sda,scl=scl".parse().unwrap();
    let lines = decode::report([&spec], &channels).unwrap();
    // A write phase and a repeated START become two frames from one call.
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("start 0x76 write"), "{}", lines[0]);
    assert!(lines[0].contains("0xd0 ack"), "{}", lines[0]);
    assert!(lines[1].contains("start 0x76 read"), "{}", lines[1]);
    assert!(lines[1].contains("0x58 nack"), "{}", lines[1]);
}
