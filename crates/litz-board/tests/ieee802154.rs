//! Two AT86RF233 modules on one board exchange IEEE 802.15.4 frames.
//!
//! The test drives the mode-0 SPI edges a driver produces: a command byte,
//! one data byte for a register, and a length byte plus PSDU for the frame
//! buffer. The frames themselves are built by `litz-ieee802154`, so the bytes
//! on the medium are the real 2015 MAC data frame and the part's filter sees
//! the same addressing fields a Zigbee or Thread radio would.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level, PartState, Tick};
use litz_diagram::Circuit;
use litz_ieee802154::{BROADCAST_SHORT, Frame, airtime_ns};

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-devkit-c-v4","attrs":{}}"#;

// AT86RF233 register addresses, from the datasheet.
const TRX_STATUS: u8 = 0x01;
const TRX_STATE: u8 = 0x02;
const PHY_CC_CCA: u8 = 0x08;
const IRQ_MASK: u8 = 0x0e;
const IRQ_STATUS: u8 = 0x0f;
const PART_NUM: u8 = 0x1c;
const VERSION_NUM: u8 = 0x1d;
const MAN_ID_0: u8 = 0x1e;
const MAN_ID_1: u8 = 0x1f;

// SPI access commands: bit 7 selects registers, bit 6 selects a write.
const CMD_REG_READ: u8 = 0x80;
const CMD_REG_WRITE: u8 = 0xc0;
const CMD_FB_READ: u8 = 0x20;
const CMD_FB_WRITE: u8 = 0x60;

// TRX_CMD values written to TRX_STATE.
const CMD_RX_ON: u8 = 0x04;
const CMD_PLL_ON: u8 = 0x06;
const CMD_TX_START: u8 = 0x01;

// TRX_STATUS values.
const STATUS_BUSY_TX: u8 = 0x02;
const STATUS_PLL_ON: u8 = 0x05;

// IRQ_STATUS bits.
const IRQ_RX_START: u8 = 0x04;
const IRQ_TRX_END: u8 = 0x08;

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

fn board(parts: &[String], connections: &[String]) -> Board {
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let connections: Vec<&str> = connections.iter().map(String::as_str).collect();
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit(&parts, &connections), &registry).expect("flattens"))
        .expect("the board builds")
}

fn part(id: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"litz-at86rf233","attrs":{attrs}}}"#)
}

fn wire(from: &str, to: &str) -> String {
    format!(r#"["{from}","{to}","",[]]"#)
}

/// The SPI wires and interrupt line of one module, as board GPIOs.
#[derive(Clone, Copy)]
struct Pins {
    sck: u8,
    mosi: u8,
    miso: u8,
    seln: u8,
    irq: Option<u8>,
    /// The instance id, for reading MISO and the IRQ pin back.
    part: &'static str,
}

const PINS_A: Pins = Pins {
    sck: 4,
    mosi: 5,
    miso: 18,
    seln: 19,
    irq: Some(21),
    part: "rf_a",
};
const PINS_B: Pins = Pins {
    sck: 25,
    mosi: 26,
    miso: 27,
    seln: 32,
    irq: Some(33),
    part: "rf_b",
};
const PINS_C: Pins = Pins {
    sck: 8,
    mosi: 9,
    miso: 10,
    seln: 22,
    irq: None,
    part: "rf_c",
};

fn connections(radio: &str, pins: Pins) -> Vec<String> {
    let mut out = vec![
        wire(&format!("esp:GPIO{}", pins.sck), &format!("{radio}:SCK")),
        wire(&format!("esp:GPIO{}", pins.mosi), &format!("{radio}:MOSI")),
        wire(&format!("esp:GPIO{}", pins.miso), &format!("{radio}:MISO")),
        wire(&format!("esp:GPIO{}", pins.seln), &format!("{radio}:SELN")),
    ];
    if let Some(irq) = pins.irq {
        out.push(wire(&format!("esp:GPIO{irq}"), &format!("{radio}:IRQ")));
    }
    out
}

fn radios(modules: &[(&str, &str, Pins)]) -> Board {
    let mut parts = vec![BOARD.to_string()];
    let mut wires = Vec::new();
    for &(id, attrs, pins) in modules {
        parts.push(part(id, attrs));
        wires.extend(connections(id, pins));
    }
    board(&parts, &wires)
}

fn drive(board: &mut Board, gpio: u8, level: Level, now: Tick) {
    board.set_chip_output(gpio, Drive::strong(level), now);
}

/// Shift `bytes` out of the chip side and return what the part drove back.
/// In mode 0 `returned[i]` answers `sent[i - 1]`, the full-duplex timing.
fn transfer(board: &mut Board, pins: Pins, sent: &[u8], now: &mut Tick) -> Vec<u8> {
    let mut returned = Vec::with_capacity(sent.len());
    for &byte in sent {
        let mut got = 0u8;
        for bit in (0..8).rev() {
            let level = if byte >> bit & 1 != 0 {
                Level::High
            } else {
                Level::Low
            };
            drive(board, pins.mosi, level, *now);
            *now += 100;
            drive(board, pins.sck, Level::High, *now);
            *now += 100;
            if board.part_pin_level(pins.part, "MISO") == Some(Level::High) {
                got |= 1 << bit;
            }
            drive(board, pins.sck, Level::Low, *now);
            *now += 100;
        }
        returned.push(got);
    }
    returned
}

fn select(board: &mut Board, pins: Pins, now: &mut Tick) {
    drive(board, pins.seln, Level::Low, *now);
    *now += 100;
}

fn deselect(board: &mut Board, pins: Pins, now: &mut Tick) {
    drive(board, pins.seln, Level::High, *now);
    *now += 100;
}

fn read_register(board: &mut Board, pins: Pins, addr: u8, now: &mut Tick) -> u8 {
    select(board, pins, now);
    let returned = transfer(board, pins, &[CMD_REG_READ | (addr & 0x3f), 0], now);
    deselect(board, pins, now);
    returned[1]
}

fn write_register(board: &mut Board, pins: Pins, addr: u8, value: u8, now: &mut Tick) {
    select(board, pins, now);
    transfer(board, pins, &[CMD_REG_WRITE | (addr & 0x3f), value], now);
    deselect(board, pins, now);
}

/// Write a frame buffer: the PHR first, then the complete PSDU.
fn write_frame(board: &mut Board, pins: Pins, psdu: &[u8], now: &mut Tick) {
    select(board, pins, now);
    let mut bytes = Vec::with_capacity(psdu.len() + 2);
    bytes.push(CMD_FB_WRITE);
    bytes.push(psdu.len() as u8);
    bytes.extend_from_slice(psdu);
    transfer(board, pins, &bytes, now);
    deselect(board, pins, now);
}

/// Write a frame buffer whose PHR declares more bytes than are written, which
/// is how a driver leaves the last two FCS bytes to the radio.
fn write_frame_declaring(board: &mut Board, pins: Pins, phr: u8, body: &[u8], now: &mut Tick) {
    select(board, pins, now);
    let mut bytes = Vec::with_capacity(body.len() + 2);
    bytes.push(CMD_FB_WRITE);
    bytes.push(phr);
    bytes.extend_from_slice(body);
    transfer(board, pins, &bytes, now);
    deselect(board, pins, now);
}

/// Read the frame buffer: the returned length byte, then the PSDU.
fn read_frame(board: &mut Board, pins: Pins, now: &mut Tick) -> Vec<u8> {
    select(board, pins, now);
    let head = transfer(board, pins, &[CMD_FB_READ, 0, 0], now);
    let len = head[1] as usize;
    let mut psdu = Vec::with_capacity(len);
    if len > 0 {
        psdu.push(head[2]);
    }
    if len > 1 {
        let rest = transfer(board, pins, &vec![0u8; len - 1], now);
        psdu.extend_from_slice(&rest);
    }
    deselect(board, pins, now);
    psdu.truncate(len);
    psdu
}

fn listen(board: &mut Board, pins: Pins, now: &mut Tick) {
    // The reset mask is zero, so a driver enables the interrupts it waits on.
    write_register(board, pins, IRQ_MASK, 0xff, now);
    write_register(board, pins, TRX_STATE, CMD_RX_ON, now);
}

fn transmit(board: &mut Board, pins: Pins, psdu: &[u8], now: &mut Tick) {
    write_register(board, pins, TRX_STATE, CMD_PLL_ON, now);
    write_frame(board, pins, psdu, now);
    write_register(board, pins, TRX_STATE, CMD_TX_START, now);
}

fn counts(board: &Board, id: &str) -> (u32, u32) {
    match board.state(id) {
        Some(PartState::Radio {
            tx_count, rx_count, ..
        }) => (tx_count, rx_count),
        other => panic!("{id}: {other:?}"),
    }
}

fn data_frame(destination: u16, payload: &[u8]) -> Vec<u8> {
    Frame::data_short(7, 0xabcd, destination, 0x0001, payload)
        .encode()
        .expect("the test frame encodes")
}

fn two_modules(attrs_b: &str) -> Board {
    radios(&[("rf_a", "{}", PINS_A), ("rf_b", attrs_b, PINS_B)])
}

#[test]
fn one_module_receives_the_bytes_the_other_wrote() {
    let mut board = two_modules(r#"{"address":2}"#);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);
    let psdu = data_frame(0x0002, b"hello ieee802154");
    transmit(&mut board, PINS_A, &psdu, &mut now);
    board.advance_to(now + airtime_ns(psdu.len()) + 1_000_000);

    assert_eq!(counts(&board, "rf_a").0, 1, "the sender reports one frame");
    assert_eq!(
        counts(&board, "rf_b").1,
        1,
        "the receiver reports one frame"
    );
    assert_eq!(
        read_frame(&mut board, PINS_B, &mut now),
        psdu,
        "the frame buffer holds the bytes that were written"
    );
    assert_eq!(
        board.part_pin_level("rf_b", "IRQ"),
        Some(Level::High),
        "TRX_END drives the IRQ line"
    );

    let status = read_register(&mut board, PINS_B, IRQ_STATUS, &mut now);
    assert_eq!(
        status & (IRQ_RX_START | IRQ_TRX_END),
        IRQ_RX_START | IRQ_TRX_END
    );
    assert_eq!(
        read_register(&mut board, PINS_B, IRQ_STATUS, &mut now),
        0,
        "IRQ_STATUS clears when read"
    );
}

#[test]
fn a_driver_may_leave_the_fcs_to_the_radio() {
    let mut board = two_modules(r#"{"address":2}"#);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);
    let complete = data_frame(0x0002, b"auto crc");
    // TRX_CTRL_1 resets with TX_AUTO_CRC_ON set: the driver writes the header
    // and payload, declares the PSDU length in the PHR, and the radio appends
    // the two FCS bytes.
    write_register(&mut board, PINS_A, TRX_STATE, CMD_PLL_ON, &mut now);
    write_frame_declaring(
        &mut board,
        PINS_A,
        complete.len() as u8,
        &complete[..complete.len() - 2],
        &mut now,
    );
    write_register(&mut board, PINS_A, TRX_STATE, CMD_TX_START, &mut now);
    board.advance_to(now + airtime_ns(complete.len()) + 1_000_000);

    assert_eq!(counts(&board, "rf_b").1, 1);
    assert_eq!(
        read_frame(&mut board, PINS_B, &mut now),
        complete,
        "the radio computed the FCS the driver omitted"
    );
}

#[test]
fn the_medium_holds_a_frame_for_its_airtime() {
    let mut board = two_modules(r#"{"address":2}"#);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);
    let psdu = data_frame(0x0002, b"timed");
    let airtime = airtime_ns(psdu.len());
    let before = now;
    transmit(&mut board, PINS_A, &psdu, &mut now);
    let after = now;

    // The transmission begins inside the SPI write, after `before`.
    board.advance_to(before + airtime);
    assert_eq!(counts(&board, "rf_b").1, 0, "the frame is still on the air");
    board.advance_to(after + airtime + 1);
    assert_eq!(counts(&board, "rf_b").1, 1, "the frame has completed");
}

#[test]
fn a_frame_on_another_channel_is_not_received() {
    let mut board = two_modules(r#"{"address":2,"channel":20}"#);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);
    transmit(&mut board, PINS_A, &data_frame(0x0002, b"far"), &mut now);
    board.advance_to(now + 10_000_000);
    assert_eq!(counts(&board, "rf_b").1, 0);
    assert_eq!(
        read_register(&mut board, PINS_B, PHY_CC_CCA, &mut now) & 0x1f,
        20,
        "the channel attribute is in PHY_CC_CCA"
    );
}

#[test]
fn a_channel_register_write_retunes_the_medium() {
    let mut board = two_modules(r#"{"address":2}"#);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);

    // First frame: both modules are on channel 15.
    let first = data_frame(0x0002, b"first");
    transmit(&mut board, PINS_A, &first, &mut now);
    board.advance_to(now + 10_000_000);
    assert_eq!(counts(&board, "rf_b").1, 1);

    // Move the receiver to channel 20 through the register a driver writes.
    write_register(&mut board, PINS_B, PHY_CC_CCA, 20, &mut now);
    let second = data_frame(0x0002, b"second");
    transmit(&mut board, PINS_A, &second, &mut now);
    board.advance_to(now + 10_000_000);
    assert_eq!(
        counts(&board, "rf_b").1,
        1,
        "the receiver is no longer on the sender's channel"
    );
}

#[test]
fn a_frame_addressed_elsewhere_is_filtered() {
    let mut board = two_modules(r#"{"address":2}"#);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);
    transmit(
        &mut board,
        PINS_A,
        &data_frame(0x0003, b"not you"),
        &mut now,
    );
    board.advance_to(now + 10_000_000);
    assert_eq!(counts(&board, "rf_b").1, 0);
}

#[test]
fn a_broadcast_is_heard_by_every_matching_module() {
    let mut board = radios(&[
        ("rf_a", "{}", PINS_A),
        ("rf_b", r#"{"address":2}"#, PINS_B),
        ("rf_c", r#"{"address":3}"#, PINS_C),
    ]);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);
    listen(&mut board, PINS_C, &mut now);
    let psdu = data_frame(BROADCAST_SHORT, b"all stations");
    transmit(&mut board, PINS_A, &psdu, &mut now);
    board.advance_to(now + 10_000_000);

    assert_eq!(counts(&board, "rf_b").1, 1);
    assert_eq!(counts(&board, "rf_c").1, 1);
    assert_eq!(read_frame(&mut board, PINS_B, &mut now), psdu);
    assert_eq!(read_frame(&mut board, PINS_C, &mut now), psdu);
}

#[test]
fn a_broadcast_on_another_pan_is_filtered() {
    let mut board = radios(&[
        ("rf_a", "{}", PINS_A),
        ("rf_b", r#"{"address":2}"#, PINS_B),
        ("rf_c", r#"{"address":3,"pan":4660}"#, PINS_C),
    ]);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);
    listen(&mut board, PINS_C, &mut now);
    transmit(
        &mut board,
        PINS_A,
        &data_frame(BROADCAST_SHORT, b"pan abcd"),
        &mut now,
    );
    board.advance_to(now + 10_000_000);

    // Both modules hear the broadcast on the channel; only the one on the
    // frame's PAN keeps it.
    assert_eq!(counts(&board, "rf_b").1, 1);
    assert_eq!(counts(&board, "rf_c").1, 0);
}

#[test]
fn a_frame_with_a_bad_fcs_is_dropped() {
    let mut board = two_modules(r#"{"address":2}"#);
    let mut now: Tick = 1_000;
    listen(&mut board, PINS_B, &mut now);
    let mut psdu = data_frame(0x0002, b"broken");
    let last = psdu.len() - 1;
    psdu[last] ^= 0x01;
    transmit(&mut board, PINS_A, &psdu, &mut now);
    board.advance_to(now + 10_000_000);
    assert_eq!(
        counts(&board, "rf_b").1,
        0,
        "the receiver's FCS check rejects the frame"
    );
}

#[test]
fn the_register_map_identifies_the_part_and_reports_its_state() {
    let mut board = two_modules(r#"{"address":2}"#);
    let mut now: Tick = 1_000;
    assert_eq!(read_register(&mut board, PINS_A, PART_NUM, &mut now), 0x0b);
    assert_eq!(
        read_register(&mut board, PINS_A, VERSION_NUM, &mut now),
        0x01
    );
    assert_eq!(read_register(&mut board, PINS_A, MAN_ID_0, &mut now), 0x1f);
    assert_eq!(read_register(&mut board, PINS_A, MAN_ID_1, &mut now), 0x00);

    // Transitions follow the TRX_CMD the driver writes.
    write_register(&mut board, PINS_A, TRX_STATE, CMD_PLL_ON, &mut now);
    assert_eq!(
        read_register(&mut board, PINS_A, TRX_STATUS, &mut now) & 0x1f,
        STATUS_PLL_ON
    );
    write_frame(&mut board, PINS_A, &data_frame(0x0002, b"state"), &mut now);
    write_register(&mut board, PINS_A, TRX_STATE, CMD_TX_START, &mut now);
    assert_eq!(
        read_register(&mut board, PINS_A, TRX_STATUS, &mut now) & 0x1f,
        STATUS_BUSY_TX
    );
}
