//! GATT: a central connects to an advertiser, discovers its table, reads and
//! writes characteristics, and a peripheral answers its writes and notifies.
//!
//! The table belongs to the advertiser — a circuit beacon or a runtime
//! peripheral — so a test can exercise the whole path without a radio backend.

use litz_ble::{Beacon, FLAG_NOTIFY, FLAG_READ, FLAG_WRITE, GattCharacteristic, GattService};
use litz_board::{Board, FlatBoard};
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

fn beacon(id: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"litz-ble-beacon","attrs":{attrs}}}"#)
}

fn runtime_beacon(last: u8, gatt: Vec<GattService>) -> Beacon {
    Beacon {
        name: Some("node".into()),
        services: Vec::new(),
        address: [0, 0, 0, 0, 0, last],
        rssi: -40,
        adv: vec![2, 1, 6, 5, 9, b'n', b'o', b'd', b'e'],
        scan_response: Vec::new(),
        gatt,
    }
}

fn table() -> Vec<GattService> {
    vec![GattService::new(
        0x180f,
        vec![
            GattCharacteristic::new(0x2a19, FLAG_READ | FLAG_NOTIFY, vec![64]),
            GattCharacteristic::new(0x2a1a, FLAG_WRITE, Vec::new()),
        ],
    )]
}

#[test]
fn a_circuit_beacon_answers_discovery_reading_and_writing() {
    let attrs = r#"{"name":"Thermo","address":"c4:aa:01:02:03:04",
                     "gatt":"180f.2a19=64, 180f.2a1a+w"}"#;
    let board = board(&[BOARD, &beacon("b1", attrs)]).expect("the board builds");
    let address = [0x04, 0x03, 0x02, 0x01, 0xaa, 0xc4];
    let handle = board
        .gatt_connect(0, &address)
        .expect("the beacon accepts connections");

    let services = board.gatt_discover(0, handle).unwrap();
    assert_eq!(services[0].uuid, 0x180f);
    assert!(
        services[0].characteristics[0].value.is_empty(),
        "values are read"
    );
    assert_eq!(
        board.gatt_read(0, handle, 0x180f, 0x2a19).unwrap(),
        vec![0x64]
    );

    board
        .gatt_write(0, handle, 0x180f, 0x2a1a, &[1, 2])
        .expect("the characteristic is writable");
    assert_eq!(
        board.gatt_read(0, handle, 0x180f, 0x2a1a).unwrap_err(),
        "180f/2a1a is not readable",
        "a write does not make a characteristic readable"
    );
}

#[test]
fn a_characteristic_without_read_or_write_refuses_both() {
    let attrs = r#"{"name":"Thermo","address":"c4:aa:01:02:03:04",
                     "gatt":"180f.2a19+n"}"#;
    let board = board(&[BOARD, &beacon("b1", attrs)]).expect("the board builds");
    let handle = board
        .gatt_connect(0, &[0x04, 0x03, 0x02, 0x01, 0xaa, 0xc4])
        .unwrap();
    assert!(
        board
            .gatt_read(0, handle, 0x180f, 0x2a19)
            .unwrap_err()
            .contains("not readable")
    );
    assert!(
        board
            .gatt_write(0, handle, 0x180f, 0x2a19, &[1])
            .unwrap_err()
            .contains("not writable")
    );
    board
        .gatt_subscribe(0, handle, 0x180f, 0x2a19, true)
        .expect("the characteristic notifies");
}

#[test]
fn a_beacon_without_a_table_refuses_the_connection() {
    let board = board(&[BOARD, &beacon("b1", r#"{"name":"Tag"}"#)]).expect("the board builds");
    let address = board.ble_air(0)[0].address;
    let error = board.gatt_connect(0, &address).unwrap_err();
    assert!(error.contains("does not accept connections"), "{error}");
}

#[test]
fn a_runtime_peripheral_serves_its_table_and_reads_centrals_writes() {
    let board = board(&[BOARD]).expect("the board builds");
    board.set_gatt(1, table()).expect("the table registers");
    board
        .advertise(1, runtime_beacon(9, Vec::new()))
        .expect("the peripheral advertises");

    let address = [0, 0, 0, 0, 0, 9];
    let handle = board.gatt_connect(0, &address).expect("connect");
    board
        .gatt_subscribe(0, handle, 0x180f, 0x2a19, true)
        .expect("subscribe to the battery level");

    board
        .gatt_write(0, handle, 0x180f, 0x2a1a, &[0xaa])
        .expect("write the writable characteristic");
    let writes = board.gatt_take_writes(1);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].characteristic, 0x2a1a);
    assert_eq!(writes[0].value, vec![0xaa]);
    assert!(
        board.gatt_take_writes(1).is_empty(),
        "a write is delivered once"
    );

    board
        .gatt_notify(1, 0x180f, 0x2a19, &[63])
        .expect("notify the level");
    let notifications = board.gatt_notifications(0, handle).unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].value, vec![63]);
    assert_eq!(
        board.gatt_read(0, handle, 0x180f, 0x2a19).unwrap(),
        vec![63]
    );

    board.gatt_disconnect(0, handle).expect("disconnect");
    assert!(
        board.gatt_read(0, handle, 0x180f, 0x2a19).is_err(),
        "a closed handle is gone"
    );
}

#[test]
fn a_central_only_hears_advertisers_other_than_itself() {
    let board = board(&[BOARD]).expect("the board builds");
    board.set_gatt(0, table()).unwrap();
    board
        .advertise(0, runtime_beacon(7, Vec::new()))
        .expect("the board advertises");
    // A station never hears its own advertisement, so it cannot connect to it.
    assert!(board.ble_air(0).iter().all(|beacon| beacon.address[5] != 7));
    assert!(board.gatt_connect(0, &[0, 0, 0, 0, 0, 7]).is_err());
}

#[test]
fn a_connection_runs_at_its_interval_not_instantly() {
    let attrs = r#"{"name":"Thermo","address":"c4:aa:01:02:03:04",
                     "gatt":"180f.2a19=64"}"#;
    let board = board(&[BOARD, &beacon("b1", attrs)]).expect("the board builds");
    let address = [0x04, 0x03, 0x02, 0x01, 0xaa, 0xc4];
    let handle = board.gatt_connect(0, &address).expect("connect");

    // The first request waits for the first connection event; the next one
    // waits a whole interval, because an interval carries one ATT exchange.
    let start = 1_000_000;
    let first = board.gatt_advance(0, handle, start);
    assert_eq!(first, litz_board::CONNECTION_EVENT_NS);
    let second = board.gatt_advance(0, handle, start + first);
    assert_eq!(second, litz_board::CONNECTION_INTERVAL_NS);

    // Values are still available, and the wait is charged by the host; the
    // board itself only reports it.
    assert_eq!(
        board.gatt_read(0, handle, 0x180f, 0x2a19).unwrap(),
        vec![0x64],
        "the attribute value is hex: 64 is one hundred"
    );
    let event = board.gatt_next_event(0, handle).unwrap();
    assert_eq!(
        event,
        start + 2 * litz_board::CONNECTION_INTERVAL_NS,
        "the next event follows this one by one interval"
    );
}

#[test]
fn the_ble_generation_changes_only_when_an_advertiser_does() {
    let board = board(&[BOARD]).expect("the board builds");
    let before = board.ble_generation();
    board.advertise(0, runtime_beacon(7, Vec::new())).unwrap();
    let started = board.ble_generation();
    assert!(started > before);
    board.stop_advertising(0, &[0, 0, 0, 0, 0, 7]);
    assert!(board.ble_generation() > started);
    let stopped = board.ble_generation();
    board.stop_advertising(0, &[0, 0, 0, 0, 0, 7]);
    assert_eq!(
        board.ble_generation(),
        stopped,
        "stopping an absent advertiser changes nothing"
    );
}
