//! The router part: its attributes become the network a radio backend puts
//! on the air. The point of these tests is that the network belongs to the
//! circuit, not to a chip adapter, so a board can be built and read without
//! any radio backend in the loop.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;
use litz_net::Service;

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

fn router(id: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"litz-wifi-router","attrs":{attrs}}}"#)
}

#[test]
fn a_router_part_gives_the_board_its_network() {
    let board = board(&[BOARD, &router("ap1", "{}")]).expect("the board builds");
    let network = board.network().expect("the circuit has a network");
    assert_eq!(network.access_points.len(), 1);
    assert_eq!(network.access_points[0].ssid, "litz");
    assert_eq!(network.access_points[0].channel, 6);
    assert!(network.access_points[0].psk.is_none());
    assert!(
        network.resolve_unknown,
        "an unknown name reaches the router"
    );
    let ports: Vec<u16> = network.services.iter().map(Service::port).collect();
    assert_eq!(
        ports,
        vec![80, 7, 1883],
        "HTTP, a TCP echo and an MQTT broker"
    );
    assert!(matches!(network.services[0], Service::Tcp { port: 80, .. }));
    assert!(matches!(network.services[2], Service::Mqtt { port: 1883 }));
}

#[test]
fn the_attributes_shape_the_network() {
    let attrs = r#"{"ssid":"litz-lab","psk":"secret","channel":11,
                     "echo":"9000, 9001","udp_echo":"5000","nearby":"neighbour"}"#;
    let board = board(&[BOARD, &router("ap1", attrs)]).expect("the board builds");
    let network = board.network().unwrap();
    let ap = &network.access_points[0];
    assert_eq!(ap.ssid, "litz-lab");
    assert_eq!(ap.psk.as_deref(), Some("secret"));
    assert_eq!(ap.channel, 11);
    assert_eq!(network.nearby.len(), 1);
    assert_eq!(network.nearby[0].ssid, "neighbour");
    let ports: Vec<u16> = network.services.iter().map(Service::port).collect();
    assert_eq!(ports, vec![80, 9000, 9001, 5000, 1883]);
    assert!(matches!(
        network.services[3],
        Service::UdpEcho { port: 5000 }
    ));
}

#[test]
fn the_mqtt_attribute_moves_or_turns_off_the_broker() {
    let attrs = r#"{"mqtt":8883}"#;
    let moved = board(&[BOARD, &router("ap1", attrs)]).expect("the board builds");
    let network = moved.network().unwrap();
    assert!(
        network
            .services
            .iter()
            .any(|service| matches!(service, Service::Mqtt { port: 8883 })),
        "{:?}",
        network.services
    );
    assert!(
        !network
            .services
            .iter()
            .any(|service| matches!(service, Service::Mqtt { port: 1883 })),
        "one broker, on the port the diagram names"
    );

    let off = r#"{"mqtt":0}"#;
    let silent = board(&[BOARD, &router("ap1", off)]).expect("the board builds");
    let network = silent.network().unwrap();
    assert!(
        !network
            .services
            .iter()
            .any(|service| matches!(service, Service::Mqtt { .. })),
        "port 0 leaves no broker"
    );
}

#[test]
fn an_empty_http_body_leaves_no_web_server() {
    let attrs = r#"{"http":"","echo":"","mqtt":0}"#;
    let board = board(&[BOARD, &router("ap1", attrs)]).expect("the board builds");
    let network = board.network().unwrap();
    assert!(network.services.is_empty(), "{:?}", network.services);
}

#[test]
fn a_second_router_is_refused() {
    let error = board(&[BOARD, &router("ap1", "{}"), &router("ap2", "{}")])
        .err()
        .expect("two routers are an error");
    assert!(
        error.contains("two routers") && error.contains("ap1") && error.contains("ap2"),
        "{error}"
    );
}

#[test]
fn a_channel_outside_the_band_is_refused() {
    let error = board(&[BOARD, &router("ap1", r#"{"channel":42}"#)])
        .err()
        .expect("channel 42 is out of range");
    assert!(error.contains("channel 42"), "{error}");
}

#[test]
fn a_port_that_is_not_a_number_is_refused() {
    let error = board(&[BOARD, &router("ap1", r#"{"echo":"http"}"#)])
        .err()
        .expect("a named port is not a number");
    assert!(error.contains("not a port number"), "{error}");
}

#[test]
fn a_board_without_a_router_has_no_network() {
    let board = board(&[BOARD]).expect("the board builds");
    assert!(board.network().is_none());
}

#[test]
fn the_secure_attributes_add_tls_and_websocket_ports() {
    let attrs = r#"{"http":"","echo":"","mqtt":0,"https":"secure",
                     "ws":"81, 9002","wss":"8443"}"#;
    let board = board(&[BOARD, &router("ap1", attrs)]).expect("the board builds");
    let network = board.network().unwrap();
    let ports: Vec<u16> = network.services.iter().map(Service::port).collect();
    assert_eq!(ports, vec![443, 81, 9002, 8443]);
    assert!(matches!(
        network.services[0],
        Service::Tls { port: 443, .. }
    ));
    assert!(matches!(
        network.services[1],
        Service::WebSocket { port: 81 }
    ));
    assert!(matches!(
        network.services[3],
        Service::Tls { port: 8443, .. }
    ));
}
