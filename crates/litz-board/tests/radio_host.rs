//! The raw radio path a custom chip uses: register on the shared medium, put
//! frames on the air, and read the ones addressed to the station. It is the
//! same medium the nRF24 and SX1276 parts use, so two stations hear each other
//! on one network and channel and nowhere else.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;

fn board() -> Board {
    let json = [r#"{"version":1,"parts":["#, BOARD, r#"],"connections":[]}"#].concat();
    let circuit = Circuit::from_json(&json).unwrap();
    let flat = FlatBoard::build(&circuit, &Registry::builtin()).unwrap();
    Board::new(flat).unwrap()
}

#[test]
fn a_frame_is_heard_after_its_airtime_and_not_before() {
    let board = board();
    board.radio_configure(0, "lab", 15, &[0x01]).unwrap();
    board.radio_configure(1, "lab", 15, &[0x02]).unwrap();
    board
        .radio_send(0, b"hello", None, 50, 100)
        .expect("the frame goes on the air");
    assert!(board.radio_receive(1, 149).is_empty(), "still in the air");
    let frames = board.radio_receive(1, 150);
    assert_eq!(frames.len(), 1);
    let (sender, packet) = &frames[0];
    assert_eq!(sender, &vec![0x01]);
    assert_eq!(packet.bytes, b"hello");
    assert_eq!(packet.sent_at_ns, 100);
    assert_eq!(packet.airtime_ns, 50);
    assert!(
        board.radio_receive(1, 150).is_empty(),
        "a frame is delivered once"
    );
    assert!(board.radio_receive(0, 150).is_empty(), "not to its sender");
}

#[test]
fn only_one_network_channel_and_address_hears_a_frame() {
    let board = board();
    board.radio_configure(0, "lab", 15, &[0x01]).unwrap();
    board.radio_configure(1, "lab", 15, &[0x02]).unwrap();
    board.radio_configure(2, "other", 15, &[0x03]).unwrap();
    board.radio_configure(3, "lab", 20, &[0x04]).unwrap();
    board
        .radio_send(0, b"x", Some(&[0x02]), 10, 0)
        .expect("a directed frame");
    assert_eq!(board.radio_receive(1, 10).len(), 1);
    assert!(board.radio_receive(2, 10).is_empty(), "another network");
    assert!(board.radio_receive(3, 10).is_empty(), "another channel");

    board
        .radio_send(0, b"y", Some(&[0x04]), 10, 20)
        .expect("a directed frame");
    assert!(
        board.radio_receive(1, 30).is_empty(),
        "another address is filtered"
    );
}

#[test]
fn overlapping_frames_destroy_each_other() {
    let board = board();
    board.radio_configure(0, "lab", 15, &[0x01]).unwrap();
    board.radio_configure(1, "lab", 15, &[0x02]).unwrap();
    board.radio_send(0, b"aaa", None, 100, 0).unwrap();
    board.radio_send(1, b"bbb", None, 100, 50).unwrap();
    assert!(board.radio_receive(0, 200).is_empty());
    assert!(board.radio_receive(1, 200).is_empty());
    // A frame that starts exactly as another ends does not overlap.
    board.radio_send(0, b"one", None, 100, 300).unwrap();
    board.radio_send(1, b"two", None, 100, 400).unwrap();
    assert_eq!(board.radio_receive(1, 500).len(), 1);
    assert_eq!(board.radio_receive(0, 500).len(), 1);
}

#[test]
fn a_station_without_a_radio_cannot_transmit() {
    let board = board();
    let error = board.radio_send(0, b"x", None, 10, 0).unwrap_err();
    assert!(error.contains("radio-configure"), "{error}");
    assert!(board.radio_configure(0, "", 15, &[]).is_err());
    assert!(board.radio_send(0, &[], None, 10, 0).is_err());
}

#[test]
fn a_raw_ieee802_15_4_frame_crosses_the_air_unchanged() {
    use litz_ieee802154::Frame;

    // The medium an AT86RF233 hears: PAN 0xabcd, short address 0x0001.
    let station = {
        let mut address = Vec::new();
        address.extend_from_slice(&0xabcdu16.to_le_bytes());
        address.extend_from_slice(&0x0001u16.to_le_bytes());
        address
    };
    let board = board();
    board
        .radio_configure(0, "ieee802154", 15, &station)
        .unwrap();
    board
        .radio_configure(1, "ieee802154", 15, &station)
        .unwrap();
    let frame = Frame::data_short(7, 0xabcd, 0x0001, 0x0002, b"zigbee")
        .encode()
        .expect("a 2015 data frame");
    // The FCS is part of the bytes a custom chip puts on the air.
    assert_eq!(
        &frame[frame.len() - 2..],
        &litz_ieee802154::fcs(&frame[..frame.len() - 2]).to_le_bytes()
    );
    board
        .radio_send(0, &frame, Some(&station), 1000, 0)
        .expect("the raw frame goes on the air");
    assert!(board.radio_receive(1, 999).is_empty(), "still transmitting");
    let frames = board.radio_receive(1, 1000);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].1.bytes, frame, "byte for byte, FCS included");
    assert!(
        Frame::decode(&frames[0].1.bytes).is_ok(),
        "the receiver can decode what the custom chip sent"
    );
}
