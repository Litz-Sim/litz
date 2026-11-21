//! Bus transactions as pin-level waveforms.
//!
//! A device on a bus answers bytes; a probe on the wire sees edges. These
//! tests check that the runtime records both: the transaction still reaches
//! the device, and a channel on SDA, SCL, TX or SCK shows what a scope would.

use litz_board::probe::Channel;
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::Level;
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;

fn circuit(parts: &[&str], connections: &[&str]) -> Circuit {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        r#"]}"#,
    ]
    .concat();
    Circuit::from_json(&json).unwrap()
}

fn board(parts: &[&str], connections: &[&str]) -> Board {
    let flat = FlatBoard::build(&circuit(parts, connections), &Registry::builtin())
        .expect("the diagram flattens");
    Board::new(flat).expect("the board builds")
}

fn edges(channels: &[Channel], name: &str) -> Vec<(u64, Level)> {
    channels
        .iter()
        .find(|channel| channel.probe.name == name)
        // A channel opens with the level the net held when the trace started,
        // which for an unwired probe is HighZ. Only real transitions matter.
        .map(|channel| {
            channel
                .series
                .digital
                .iter()
                .copied()
                .filter(|(_, level)| *level != Level::HighZ)
                .collect()
        })
        .unwrap_or_else(|| panic!("no channel named {name}"))
}

fn rises(points: &[(u64, Level)]) -> usize {
    points
        .iter()
        .filter(|(_, level)| *level == Level::High)
        .count()
}

#[test]
fn an_i2c_transaction_shows_on_sda_and_scl() {
    let bmp = r#"{"id":"bmp1","type":"litz-bmp280","attrs":{}}"#;
    let mut board = board(
        &[BOARD, bmp],
        &[
            r#"["esp:GPIO8","bmp1:SDA","",[]]"#,
            r#"["esp:GPIO9","bmp1:SCL","",[]]"#,
            r#"["esp:3V3","bmp1:VCC","",[]]"#,
            r#"["esp:GND.1","bmp1:GND","",[]]"#,
        ],
    );
    board.add_probe("sda", "esp:GPIO8").unwrap();
    board.add_probe("scl", "esp:GPIO9").unwrap();
    board.start_trace();

    let read = board
        .i2c_write_read(0, 0, 0x76, &[0xd0], 1)
        .expect("the BME280 answers its id register");
    assert_eq!(read.len(), 1);

    let channels = board.scope();
    let sda = edges(&channels, "sda");
    let scl = edges(&channels, "scl");
    assert!(!sda.is_empty(), "SDA moved");
    assert!(!scl.is_empty(), "SCL moved");
    // Address+write, the register byte, a repeated START with address+read,
    // and the read byte: nine clocks each, plus the extra pulse that sets up
    // STOP after the master's NACK. The idle high before the first fall is
    // not a clock.
    let first_fall = scl.iter().position(|edge| edge.1 == Level::Low).unwrap();
    assert_eq!(
        scl[first_fall..]
            .iter()
            .filter(|edge| edge.1 == Level::High)
            .count(),
        4 * 9 + 1
    );
    assert!(sda.contains(&(0, Level::Low)), "START pulls SDA down");
}

#[test]
fn a_uart_write_shows_on_the_transmit_pin() {
    let gps = r#"{"id":"gps","type":"litz-gps-neo6m","attrs":{}}"#;
    let mut board = board(
        &[BOARD, gps],
        &[
            r#"["esp:GPIO20","gps:TX","",[]]"#,
            r#"["esp:GPIO21","gps:RX","",[]]"#,
            r#"["esp:3V3","gps:VCC","",[]]"#,
            r#"["esp:GND.1","gps:GND","",[]]"#,
        ],
    );
    board.add_probe("chip_tx", "esp:GPIO21").unwrap();
    board.add_probe("chip_rx", "esp:GPIO20").unwrap();
    board.start_trace();

    assert!(board.uart_receive(0, 0, 1_000, b"AT"));
    let tx = edges(&board.scope(), "chip_tx");
    assert_eq!(tx[0], (1_000, Level::High), "the line idles high");
    assert!(
        tx.contains(&(1_000, Level::Low)),
        "the start bit pulls TX low"
    );
    assert_eq!(tx.last().copied(), Some((1_000 + 19 * 8_680, Level::High)));

    // The part's own answer is observed when the chip polls it.
    let reply = board.uart_poll(0, 0, 2_000_000);
    assert!(!reply.is_empty());
    let rx = edges(&board.scope(), "chip_rx");
    assert!(
        rx.contains(&(2_000_000, Level::Low)),
        "the part's start bit reaches the chip"
    );
}

#[test]
fn an_spi_transfer_shows_on_clock_mosi_and_cs() {
    let adc = r#"{"id":"adc","type":"litz-mcp3008","attrs":{}}"#;
    let mut board = board(
        &[BOARD, adc],
        &[
            r#"["esp:GPIO2","adc:CLK","",[]]"#,
            r#"["esp:GPIO3","adc:DIN","",[]]"#,
            r#"["esp:GPIO4","adc:DOUT","",[]]"#,
            r#"["esp:GPIO5","adc:CS","",[]]"#,
            r#"["esp:3V3","adc:VDD","",[]]"#,
            r#"["esp:3V3","adc:VREF","",[]]"#,
            r#"["esp:GND.1","adc:DGND","",[]]"#,
            r#"["esp:GND.1","adc:AGND","",[]]"#,
        ],
    );
    for (name, gpio) in [("sck", 2), ("mosi", 3), ("miso", 4), ("cs", 5)] {
        board.add_probe(name, &format!("esp:GPIO{gpio}")).unwrap();
    }
    board.start_trace();

    let rx = board.spi_transfer(0, 0, 5, &[0x01, 0x80, 0x00], 3);
    assert_eq!(rx.len(), 3);

    let channels = board.scope();
    assert_eq!(rises(&edges(&channels, "sck")), 24, "eight clocks a byte");
    assert!(!edges(&channels, "mosi").is_empty(), "MOSI shifted out");
    let cs = edges(&channels, "cs");
    assert!(cs.contains(&(0, Level::Low)), "CS is asserted");
    assert_eq!(cs.last().map(|edge| edge.1), Some(Level::High));
}

#[test]
fn a_uart_waveform_uses_the_baud_the_firmware_set() {
    let gps = r#"{"id":"gps","type":"litz-gps-neo6m","attrs":{}}"#;
    let mut board = board(
        &[BOARD, gps],
        &[
            r#"["esp:GPIO20","gps:TX","",[]]"#,
            r#"["esp:GPIO21","gps:RX","",[]]"#,
            r#"["esp:3V3","gps:VCC","",[]]"#,
            r#"["esp:GND.1","gps:GND","",[]]"#,
        ],
    );
    board.add_probe("chip_tx", "esp:GPIO21").unwrap();
    board.start_trace();
    board.set_uart_baud(0, 0, 9_600);

    let bit = 1_000_000_000 / 9_600;
    assert!(board.uart_receive(0, 0, 1_000, b"OK"));
    let tx = edges(&board.scope(), "chip_tx");
    assert_eq!(tx[0], (1_000, Level::High), "the line idles high");
    assert!(
        tx.contains(&(1_000, Level::Low)),
        "the start bit is one bit time"
    );
    assert_eq!(
        tx.last().copied(),
        Some((1_000 + 19 * bit, Level::High)),
        "two frames at 9600 baud end 19 bit times later"
    );
}

#[test]
fn an_i2c_waveform_uses_the_clock_the_firmware_set() {
    let bmp = r#"{"id":"bmp1","type":"litz-bmp280","attrs":{}}"#;
    let mut board = board(
        &[BOARD, bmp],
        &[
            r#"["esp:GPIO8","bmp1:SDA","",[]]"#,
            r#"["esp:GPIO9","bmp1:SCL","",[]]"#,
            r#"["esp:3V3","bmp1:VCC","",[]]"#,
            r#"["esp:GND.1","bmp1:GND","",[]]"#,
        ],
    );
    board.add_probe("scl", "esp:GPIO9").unwrap();
    board.start_trace();
    board.set_i2c_hz(0, 0, 400_000);

    assert!(board.i2c_write_read(0, 0, 0x76, &[0xd0], 1).is_some());
    let scl = edges(&board.scope(), "scl");
    let rises: Vec<u64> = scl
        .iter()
        .filter(|edge| edge.1 == Level::High)
        .map(|edge| edge.0)
        .collect();
    assert!(rises.len() > 2, "the transaction clocks bytes");
    let period = 1_000_000_000 / 400_000;
    let mut gaps: Vec<u64> = rises.windows(2).map(|pair| pair[1] - pair[0]).collect();
    gaps.sort_unstable();
    // START and repeated START stretch the first and middle gaps; the steady
    // clock between them is one period.
    assert_eq!(
        gaps[gaps.len() / 2],
        period,
        "the steady clock period is the programmed 400 kHz, got {rises:?}"
    );
}

#[test]
fn an_spi_waveform_uses_the_clock_the_firmware_set() {
    let adc = r#"{"id":"adc","type":"litz-mcp3008","attrs":{}}"#;
    let mut board = board(
        &[BOARD, adc],
        &[
            r#"["esp:GPIO2","adc:CLK","",[]]"#,
            r#"["esp:GPIO3","adc:DIN","",[]]"#,
            r#"["esp:GPIO4","adc:DOUT","",[]]"#,
            r#"["esp:GPIO5","adc:CS","",[]]"#,
            r#"["esp:3V3","adc:VDD","",[]]"#,
            r#"["esp:3V3","adc:VREF","",[]]"#,
            r#"["esp:GND.1","adc:DGND","",[]]"#,
            r#"["esp:GND.1","adc:AGND","",[]]"#,
        ],
    );
    board.add_probe("sck", "esp:GPIO2").unwrap();
    board.start_trace();
    board.set_spi_hz(0, 0, 4_000_000);

    assert_eq!(board.spi_transfer(0, 0, 5, &[0x01, 0x80, 0x00], 3).len(), 3);
    let sck = edges(&board.scope(), "sck");
    let rises: Vec<u64> = sck
        .iter()
        .filter(|edge| edge.1 == Level::High)
        .map(|edge| edge.0)
        .collect();
    assert_eq!(rises.len(), 24, "eight clocks a byte");
    let period = 1_000_000_000 / 4_000_000;
    assert!(
        rises.windows(2).all(|pair| pair[1] - pair[0] == period),
        "each clock period is the programmed 4 MHz, got {rises:?}"
    );
}

#[test]
fn nothing_is_observed_until_a_trace_starts() {
    let bmp = r#"{"id":"bmp1","type":"litz-bmp280","attrs":{}}"#;
    let mut board = board(
        &[BOARD, bmp],
        &[
            r#"["esp:GPIO8","bmp1:SDA","",[]]"#,
            r#"["esp:GPIO9","bmp1:SCL","",[]]"#,
            r#"["esp:3V3","bmp1:VCC","",[]]"#,
            r#"["esp:GND.1","bmp1:GND","",[]]"#,
        ],
    );
    board.add_probe("sda", "esp:GPIO8").unwrap();
    assert!(board.i2c_write_read(0, 0, 0x76, &[0xd0], 1).is_some());
    assert_eq!(board.trace_len(), 0, "recording is opt-in");
    assert!(board.scope()[0].series.digital.is_empty());
}
