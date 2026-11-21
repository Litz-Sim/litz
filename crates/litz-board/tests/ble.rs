//! The beacon part: its attributes become the advertisers a radio backend puts
//! in range. The point of these tests is that Bluetooth belongs to the circuit,
//! not to a chip adapter, so a board can be built and read without any radio
//! backend in the loop.

use litz_board::{Beacon, Board, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;

fn circuit(parts: &[&str]) -> Circuit {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":[]}"#,
    ]
    .concat();
    Circuit::from_json(&json).unwrap()
}

fn board(parts: &[&str]) -> Result<Board, String> {
    let registry = Registry::builtin();
    let flat = FlatBoard::build(&circuit(parts), &registry).map_err(|e| e.to_string())?;
    Board::new(flat).map_err(|e| e.to_string())
}

fn advertised(name: &str, last: u8) -> Beacon {
    Beacon {
        name: Some(name.into()),
        services: Vec::new(),
        address: [0, 0, 0, 0, 0, last],
        rssi: -40,
        adv: vec![2, 1, 6],
        scan_response: Vec::new(),
        gatt: Vec::new(),
    }
}

fn beacon(id: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"litz-ble-beacon","attrs":{attrs}}}"#)
}

#[test]
fn a_beacon_part_puts_an_advertiser_in_range() {
    let board = board(&[BOARD, &beacon("b1", "{}")]).expect("the board builds");
    let config = board.bluetooth().expect("the circuit has beacons");
    assert_eq!(config.beacons.len(), 1);

    let found = &config.beacons[0];
    assert_eq!(found.name.as_deref(), Some("litz"));
    assert!(found.services.is_empty());
    assert_eq!(found.rssi, -60);
    assert!(found.scan_response.is_empty());
    // Flags, then the complete local name; services are opt-in.
    assert_eq!(found.adv, vec![2, 1, 6, 5, 9, b'l', b'i', b't', b'z']);
    assert_eq!(
        found.address[5] >> 6,
        0b11,
        "a derived address is random static"
    );
    assert_eq!(found.address_type(), 1);
}

#[test]
fn the_attributes_shape_the_beacon() {
    let attrs = r#"{"name":"Thermo","address":"c4:aa:01:02:03:04","rssi":-70,
                     "services":"0f18,180a","rsp":"0303aa18"}"#;
    let board = board(&[BOARD, &beacon("b1", attrs)]).expect("the board builds");
    let found = &board.bluetooth().unwrap().beacons[0];
    assert_eq!(found.name.as_deref(), Some("Thermo"));
    assert_eq!(found.services, vec![0x0f18, 0x180a]);
    assert_eq!(found.rssi, -70);
    assert_eq!(found.scan_response, vec![3, 3, 0xaa, 0x18]);
    assert_eq!(found.address_text(), "c4:aa:01:02:03:04");
    assert_eq!(found.address_type(), 1);
}

#[test]
fn explicit_advertising_data_replaces_the_built_payload() {
    let attrs = r#"{"name":"","adv":"0201060303aafe","rsp":"0303aa18"}"#;
    let board = board(&[BOARD, &beacon("b1", attrs)]).expect("the board builds");
    let found = &board.bluetooth().unwrap().beacons[0];
    assert!(found.name.is_none());
    assert_eq!(found.adv, vec![2, 1, 6, 3, 3, 0xaa, 0xfe]);
    assert_eq!(found.scan_response, vec![3, 3, 0xaa, 0x18]);
}

#[test]
fn the_same_circuit_derives_the_same_address() {
    let first = board(&[BOARD, &beacon("b1", "{}")]).expect("the board builds");
    let second = board(&[BOARD, &beacon("b1", "{}")]).expect("the board builds");
    assert_eq!(
        first.bluetooth().unwrap().beacons[0].address,
        second.bluetooth().unwrap().beacons[0].address
    );
}

#[test]
fn name_and_advertising_data_are_exclusive() {
    let error = board(&[BOARD, &beacon("b1", r#"{"name":"litz","adv":"020106"}"#)])
        .err()
        .expect("a name and advertising data together are an error");
    assert!(error.contains("name or adv, not both"), "{error}");
}

#[test]
fn an_address_that_is_not_six_hex_octets_is_refused() {
    let error = board(&[BOARD, &beacon("b1", r#"{"address":"aa:bb:cc"}"#)])
        .err()
        .expect("a short address is an error");
    assert!(error.contains("six hex octets"), "{error}");
}

#[test]
fn an_rssi_outside_the_report_range_is_refused() {
    for value in ["-128", "21", "126"] {
        let attrs = format!(r#"{{"rssi":{value}}}"#);
        let error = board(&[BOARD, &beacon("b1", &attrs)])
            .err()
            .expect("an unreportable signal is an error");
        assert!(
            error.contains("outside -127 through 20"),
            "{value}: {error}"
        );
    }
    // 127 is the report's "not available", not an out-of-range measurement.
    let board = board(&[BOARD, &beacon("b1", r#"{"rssi":127}"#)]).expect("127 is valid");
    assert_eq!(board.bluetooth().unwrap().beacons[0].rssi, 127);
}

#[test]
fn a_name_too_long_for_the_payload_is_refused() {
    let name = "x".repeat(27);
    let attrs = format!(r#"{{"name":"{name}"}}"#);
    let error = board(&[BOARD, &beacon("b1", &attrs)])
        .err()
        .expect("a 27-byte name is an error");
    assert!(error.contains("1 to 26"), "{error}");
}

#[test]
fn advertising_data_over_31_octets_is_refused() {
    let adv = "00".repeat(32);
    let attrs = format!(r#"{{"name":"","adv":"{adv}"}}"#);
    let error = board(&[BOARD, &beacon("b1", &attrs)])
        .err()
        .expect("32 octets of advertising data are an error");
    assert!(error.contains("at most 31"), "{error}");
}

#[test]
fn several_beacons_are_collected_in_board_order() {
    let board = board(&[
        BOARD,
        &beacon("b1", r#"{"name":"one"}"#),
        &beacon("b2", r#"{"name":"two"}"#),
    ])
    .expect("the board builds");
    let config = board.bluetooth().unwrap();
    let names: Vec<_> = config
        .beacons
        .iter()
        .filter_map(|beacon| beacon.name.as_deref())
        .collect();
    assert_eq!(names, vec!["one", "two"]);
    assert_ne!(config.beacons[0].address, config.beacons[1].address);
}

#[test]
fn a_board_without_a_beacon_has_no_bluetooth() {
    let board = board(&[BOARD]).expect("the board builds");
    assert!(board.bluetooth().is_none());
}

#[test]
fn a_runtime_advertisement_is_heard_by_every_board_but_its_own() {
    let board = board(&[BOARD]).expect("the board builds");
    let node = advertised("node", 0x11);
    board
        .advertise(0, node.clone())
        .expect("the address is free");

    assert!(
        board.ble_air(0).is_empty(),
        "a station does not hear its own advertisement"
    );
    let heard = board.ble_air(1);
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].name.as_deref(), Some("node"));
    assert_eq!(heard[0].rssi, -40);

    board.stop_advertising(1, &node.address);
    assert_eq!(board.ble_air(1).len(), 1, "another board cannot stop it");
    board.stop_advertising(0, &node.address);
    assert!(board.ble_air(1).is_empty());
}

#[test]
fn two_advertisers_cannot_share_an_address() {
    let board = board(&[BOARD]).expect("the board builds");
    board.advertise(0, advertised("one", 0x22)).unwrap();
    let error = board
        .advertise(1, advertised("two", 0x22))
        .expect_err("the address is taken");
    assert!(error.contains("already advertising"), "{error}");
}
