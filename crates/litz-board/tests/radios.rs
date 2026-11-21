//! The packet radios on a real board: a test drives the same mode-0 SPI edges
//! a driver would, and two radios share the board's medium.
//!
//! The register sequences are the datasheet's (SX127x and nRF24L01), which is
//! the point: the parts answer a driver written against the silicon rather
//! than a transaction API invented for the test.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level, PartState, Tick};
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-devkit-c-v4","attrs":{}}"#;

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
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit(parts, connections), &registry).expect("flattens"))
        .expect("the board builds")
}

fn part(id: &str, type_key: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"{type_key}","attrs":{attrs}}}"#)
}

fn wire(from: &str, to: &str) -> String {
    format!(r#"["{from}","{to}","",[]]"#)
}

/// The four SPI wires and the interrupt lines of one radio, as board GPIOs.
#[derive(Clone, Copy)]
struct Pins {
    sck: u8,
    mosi: u8,
    miso: u8,
    nss: u8,
    /// The chip-select pin's name: `NSS` on the SX1276, `CSN` on the nRF24.
    cs: &'static str,
    /// The part id, for reading MISO back through the resolved net.
    part: &'static str,
}

fn connections(radio: &str, p: Pins, extra: &[(&str, &str)]) -> Vec<String> {
    let mut out = vec![
        wire(&format!("esp:GPIO{}", p.sck), &format!("{radio}:SCK")),
        wire(&format!("esp:GPIO{}", p.mosi), &format!("{radio}:MOSI")),
        wire(&format!("esp:GPIO{}", p.miso), &format!("{radio}:MISO")),
        wire(&format!("esp:GPIO{}", p.nss), &format!("{radio}:{}", p.cs)),
    ];
    out.extend(
        extra
            .iter()
            .map(|(pin, part_pin)| wire(&format!("esp:{pin}"), &format!("{radio}:{part_pin}"))),
    );
    out
}

fn drive(board: &mut Board, gpio: u8, level: Level, now: Tick) {
    board.set_chip_output(gpio, Drive::strong(level), now);
}

/// Shift `bytes` out of the chip's side and return the bytes the part drove
/// back. In mode 0 the answer to byte *n* is sampled while byte *n+1* is sent,
/// so `returned[i]` is the response to `sent[i - 1]`.
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
    drive(board, pins.nss, Level::Low, *now);
    *now += 100;
}

fn deselect(board: &mut Board, pins: Pins, now: &mut Tick) {
    drive(board, pins.nss, Level::High, *now);
    *now += 100;
}

fn write(board: &mut Board, pins: Pins, addr: u8, values: &[u8], now: &mut Tick) {
    select(board, pins, now);
    let mut bytes = vec![0x80 | addr];
    bytes.extend_from_slice(values);
    transfer(board, pins, &bytes, now);
    deselect(board, pins, now);
}

fn read(board: &mut Board, pins: Pins, addr: u8, count: usize, now: &mut Tick) -> Vec<u8> {
    select(board, pins, now);
    let mut request = vec![addr & 0x7f];
    request.extend(std::iter::repeat_n(0u8, count));
    let returned = transfer(board, pins, &request, now);
    deselect(board, pins, now);
    returned[1..=count].to_vec()
}

const LORA_A_PINS: Pins = Pins {
    sck: 4,
    mosi: 5,
    miso: 18,
    nss: 19,
    cs: "NSS",
    part: "lora_a",
};
const LORA_B_PINS: Pins = Pins {
    sck: 25,
    mosi: 26,
    miso: 27,
    nss: 32,
    cs: "NSS",
    part: "lora_b",
};

const SX_FIFO: u8 = 0x00;
const SX_OP_MODE: u8 = 0x01;
const SX_FIFO_ADDR_PTR: u8 = 0x0d;
const SX_FIFO_TX_BASE: u8 = 0x0e;
const SX_FIFO_RX_BASE: u8 = 0x0f;
const SX_FIFO_RX_CURRENT: u8 = 0x10;
const SX_PAYLOAD_LENGTH: u8 = 0x22;
const SX_VERSION: u8 = 0x42;
const SX_LORA: u8 = 0x80;
const SX_MODE_TX: u8 = 0x02;
const SX_MODE_RX_CONTINUOUS: u8 = 0x03;

fn lora_board(attrs_b: &str) -> Board {
    let a = part(
        "lora_a",
        "litz-sx1276",
        r#"{"network":"lab","frequency":868.0}"#,
    );
    let b = part("lora_b", "litz-sx1276", attrs_b);
    let mut wires = connections("lora_a", LORA_A_PINS, &[]);
    wires.extend(connections("lora_b", LORA_B_PINS, &[("GPIO33", "DIO0")]));
    let connections: Vec<&str> = wires.iter().map(String::as_str).collect();
    board(&[BOARD, &a, &b], &connections)
}

/// Put radio `pins` in continuous receive, with the RX FIFO at offset 0.
fn receive(board: &mut Board, pins: Pins, now: &mut Tick) {
    write(board, pins, SX_FIFO_RX_BASE, &[0], now);
    write(
        board,
        pins,
        SX_OP_MODE,
        &[SX_LORA | SX_MODE_RX_CONTINUOUS],
        now,
    );
}

/// Load `payload` into the TX FIFO and start a transmission.
fn transmit(board: &mut Board, pins: Pins, payload: &[u8], now: &mut Tick) {
    write(board, pins, SX_FIFO_TX_BASE, &[0], now);
    write(board, pins, SX_FIFO_ADDR_PTR, &[0], now);
    write(board, pins, SX_FIFO, payload, now);
    write(board, pins, SX_PAYLOAD_LENGTH, &[payload.len() as u8], now);
    write(board, pins, SX_OP_MODE, &[SX_LORA | SX_MODE_TX], now);
}

#[test]
fn the_version_register_reads_back_over_spi() {
    let mut board = lora_board(r#"{"network":"lab","frequency":868.0}"#);
    let mut now: Tick = 1_000;
    let version = read(&mut board, LORA_A_PINS, SX_VERSION, 1, &mut now);
    assert_eq!(version, vec![0x12]);
}

#[test]
fn one_lora_radio_receives_what_the_other_sends() {
    let mut board = lora_board(r#"{"network":"lab","frequency":868.0}"#);
    let mut now: Tick = 1_000;
    receive(&mut board, LORA_B_PINS, &mut now);
    transmit(&mut board, LORA_A_PINS, b"lora", &mut now);

    let a = board.state("lora_a").expect("the first radio reports");
    assert!(matches!(a, PartState::Radio { tx_count: 1, .. }), "{a:?}");

    // The frame spends its airtime in the medium before anyone hears it.
    board.advance_to(now + 50_000_000);
    let b = board.state("lora_b").expect("the second radio reports");
    assert!(matches!(b, PartState::Radio { rx_count: 1, .. }), "{b:?}");
    assert_eq!(
        board.part_pin_level("lora_b", "DIO0"),
        Some(Level::High),
        "RxDone is on DIO0"
    );

    let start = read(&mut board, LORA_B_PINS, SX_FIFO_RX_CURRENT, 1, &mut now);
    write(&mut board, LORA_B_PINS, SX_FIFO_ADDR_PTR, &start, &mut now);
    let payload = read(&mut board, LORA_B_PINS, SX_FIFO, 4, &mut now);
    assert_eq!(payload, b"lora");
}

#[test]
fn a_radio_on_another_channel_hears_nothing() {
    let mut board = lora_board(r#"{"network":"lab","frequency":433.0}"#);
    let mut now: Tick = 1_000;
    receive(&mut board, LORA_B_PINS, &mut now);
    transmit(&mut board, LORA_A_PINS, b"lora", &mut now);
    board.advance_to(now + 50_000_000);
    let b = board.state("lora_b").unwrap();
    assert!(matches!(b, PartState::Radio { rx_count: 0, .. }), "{b:?}");
}

#[test]
fn a_radio_on_another_network_hears_nothing() {
    let mut board = lora_board(r#"{"network":"other","frequency":868.0}"#);
    let mut now: Tick = 1_000;
    receive(&mut board, LORA_B_PINS, &mut now);
    transmit(&mut board, LORA_A_PINS, b"lora", &mut now);
    board.advance_to(now + 50_000_000);
    let b = board.state("lora_b").unwrap();
    assert!(matches!(b, PartState::Radio { rx_count: 0, .. }), "{b:?}");
}

#[test]
fn the_last_frame_is_reported_in_the_part_state() {
    let mut board = lora_board(r#"{"network":"lab","frequency":868.0}"#);
    let mut now: Tick = 1_000;
    transmit(&mut board, LORA_A_PINS, b"beacon", &mut now);
    match board.state("lora_a").expect("reports") {
        PartState::Radio {
            tx_count,
            last_bytes,
            last_len,
            ..
        } => {
            assert_eq!(tx_count, 1);
            assert_eq!(last_len, 6);
            assert_eq!(&last_bytes[..6], b"beacon");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_second_frame_waiting_its_turn_does_not_collide_with_the_first() {
    let mut board = lora_board(r#"{"network":"lab","frequency":868.0}"#);
    let mut now: Tick = 1_000;
    receive(&mut board, LORA_B_PINS, &mut now);
    transmit(&mut board, LORA_A_PINS, b"one", &mut now);
    // Start the second only after the first has left the air.
    board.advance_to(now + 50_000_000);
    transmit(&mut board, LORA_A_PINS, b"two", &mut now);
    board.advance_to(now + 100_000_000);
    let b = board.state("lora_b").unwrap();
    assert!(matches!(b, PartState::Radio { rx_count: 2, .. }), "{b:?}");
}

const NRF_A_PINS: Pins = Pins {
    sck: 4,
    mosi: 5,
    miso: 18,
    nss: 19,
    cs: "CSN",
    part: "nrf_a",
};
const NRF_B_PINS: Pins = Pins {
    sck: 25,
    mosi: 26,
    miso: 27,
    nss: 32,
    cs: "CSN",
    part: "nrf_b",
};
const NRF_A_CE: u8 = 21;
const NRF_B_CE: u8 = 22;

const NRF_CONFIG: u8 = 0x00;
const NRF_RF_CH: u8 = 0x05;
const NRF_STATUS: u8 = 0x07;
const NRF_CMD_W_REGISTER: u8 = 0x20;
const NRF_CMD_R_RX_PAYLOAD: u8 = 0x61;
const NRF_CMD_W_TX_PAYLOAD: u8 = 0xa0;

fn nrf_board() -> Board {
    let a = part("nrf_a", "litz-nrf24l01", "{}");
    let b = part("nrf_b", "litz-nrf24l01", "{}");
    let mut wires = connections("nrf_a", NRF_A_PINS, &[("GPIO21", "CE")]);
    wires.extend(connections(
        "nrf_b",
        NRF_B_PINS,
        &[("GPIO22", "CE"), ("GPIO33", "IRQ")],
    ));
    let connections: Vec<&str> = wires.iter().map(String::as_str).collect();
    board(&[BOARD, &a, &b], &connections)
}

/// A command plus its data bytes, selected and deselected around the transfer.
fn command(board: &mut Board, pins: Pins, command: u8, data: &[u8], now: &mut Tick) -> Vec<u8> {
    select(board, pins, now);
    let mut bytes = vec![command];
    bytes.extend_from_slice(data);
    let returned = transfer(board, pins, bytes.as_slice(), now);
    deselect(board, pins, now);
    returned
}

fn nrf_register(board: &mut Board, pins: Pins, addr: u8, value: u8, now: &mut Tick) {
    command(board, pins, NRF_CMD_W_REGISTER | addr, &[value], now);
}

#[test]
fn one_nrf24_radio_receives_a_24_ghz_payload() {
    let mut board = nrf_board();
    let mut now: Tick = 1_000;
    // The receiver: powered up, CRC on, primary RX.
    nrf_register(&mut board, NRF_B_PINS, NRF_CONFIG, 0x0b, &mut now);
    drive(&mut board, NRF_B_CE, Level::High, now);

    // The sender: 32-byte payload (the part's default width), then CE.
    let mut payload = [0u8; 32];
    payload[..6].copy_from_slice(b"nrf24!");
    let returned = command(
        &mut board,
        NRF_A_PINS,
        NRF_CMD_W_TX_PAYLOAD,
        &payload,
        &mut now,
    );
    assert_eq!(returned.len(), 33);
    drive(&mut board, NRF_A_CE, Level::High, now);

    board.advance_to(now + 5_000_000);
    let b = board.state("nrf_b").expect("the receiver reports");
    assert!(matches!(b, PartState::Radio { rx_count: 1, .. }), "{b:?}");
    assert_eq!(board.part_pin_level("nrf_b", "IRQ"), Some(Level::Low));

    let status = command(&mut board, NRF_B_PINS, NRF_STATUS, &[0], &mut now);
    assert_eq!(status[1] & 0x40, 0x40, "RX_DR is set in STATUS");

    let read = command(
        &mut board,
        NRF_B_PINS,
        NRF_CMD_R_RX_PAYLOAD,
        &[0u8; 32],
        &mut now,
    );
    assert_eq!(&read[1..7], b"nrf24!");
}

#[test]
fn a_nrf24_channel_write_retunes_the_medium() {
    let mut board = nrf_board();
    let mut now: Tick = 1_000;
    // Move the sender to channel 0 and leave the receiver on 76: the frame is
    // sent but not heard.
    nrf_register(&mut board, NRF_A_PINS, NRF_RF_CH, 0x00, &mut now);
    nrf_register(&mut board, NRF_B_PINS, NRF_CONFIG, 0x0b, &mut now);
    drive(&mut board, NRF_B_CE, Level::High, now);
    let mut payload = [0u8; 32];
    payload[..3].copy_from_slice(b"far");
    command(
        &mut board,
        NRF_A_PINS,
        NRF_CMD_W_TX_PAYLOAD,
        &payload,
        &mut now,
    );
    drive(&mut board, NRF_A_CE, Level::High, now);
    board.advance_to(now + 5_000_000);
    let b = board.state("nrf_b").unwrap();
    assert!(matches!(b, PartState::Radio { rx_count: 0, .. }), "{b:?}");
}
