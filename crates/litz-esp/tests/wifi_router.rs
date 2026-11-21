//! The simulated router seen from the guest's side: a TCP handshake, a request
//! and the reply, all through esp32sim's NAT with no host socket involved.
//!
//! These drive `Nat` directly with the segments the guest's stack would send,
//! so the test is about the router and not about a particular chip's DMA
//! descriptors. The chip-level path is covered by the MicroPython example that
//! joins this network.

#![cfg(any(
    feature = "esp32",
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32s3",
    feature = "esp32p4",
    feature = "esp32s31"
))]

use esp_soc::nat::Nat;
use litz_net::{HOST_IP, NetworkConfig, Service};

const GUEST_MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x00, 0x00, 0x01];
const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
const GUEST_PORT: u16 = 40_000;
const SYN: u8 = 0x02;
const ACK: u8 = 0x10;
const PSH: u8 = 0x08;

/// A TCP segment with no options, as the guest's stack would send it.
fn segment(sport: u16, dport: u16, seq: u32, ack: u32, flags: u8, data: &[u8]) -> Vec<u8> {
    let mut seg = vec![0u8; 20];
    seg[0..2].copy_from_slice(&sport.to_be_bytes());
    seg[2..4].copy_from_slice(&dport.to_be_bytes());
    seg[4..8].copy_from_slice(&seq.to_be_bytes());
    seg[8..12].copy_from_slice(&ack.to_be_bytes());
    seg[12] = 5 << 4;
    seg[13] = flags;
    seg[14..16].copy_from_slice(&5840u16.to_be_bytes());
    seg.extend_from_slice(data);
    seg
}

/// The TCP header of a frame the NAF emitted: Ethernet II, IPv4 without
/// options in these tests, then TCP.
fn tcp(frame: &[u8]) -> &[u8] {
    let ip = 14;
    assert_eq!(frame[ip] >> 4, 4, "not IPv4: {frame:02x?}");
    assert_eq!(frame[ip + 9], 6, "not TCP: {frame:02x?}");
    let header_len = usize::from(frame[ip] & 0x0f) * 4;
    &frame[ip + header_len..]
}

fn field(frame: &[u8], range: std::ops::Range<usize>) -> u32 {
    u32::from_be_bytes(tcp(frame)[range].try_into().unwrap())
}

fn flags(frame: &[u8]) -> u8 {
    tcp(frame)[13]
}

fn data(frame: &[u8]) -> &[u8] {
    let tcp = tcp(frame);
    &tcp[usize::from(tcp[12] >> 4) * 4..]
}

#[test]
fn a_guest_reaches_the_http_service_and_reads_the_page() {
    let config = NetworkConfig::access_point("ssid=litz")
        .with_service(Service::http(80, "hello from litz\n"));
    let mut nat = Nat::with_transport(false, Box::new(litz_esp::EspRouter::new(&config)));
    nat.set_station(GUEST_MAC);

    // SYN: the flow opens at once, so the handshake completes on the first poll.
    assert!(
        nat.tcp_in(
            &GUEST_MAC,
            &GUEST_IP,
            &HOST_IP,
            &segment(GUEST_PORT, 80, 1000, 0, SYN, b""),
            0
        )
        .is_empty()
    );
    let sent = nat.poll(1_000);
    let syn_ack = sent.first().expect("a SYN-ACK");
    assert_eq!(flags(syn_ack) & (SYN | ACK), SYN | ACK, "{syn_ack:02x?}");
    let server_seq = field(syn_ack, 4..8);

    // ACK plus the request in one segment, as a real client sends it.
    nat.tcp_in(
        &GUEST_MAC,
        &GUEST_IP,
        &HOST_IP,
        &segment(
            GUEST_PORT,
            80,
            1001,
            server_seq.wrapping_add(1),
            ACK | PSH,
            b"GET / HTTP/1.0\r\nHost: litz.local\r\n\r\n",
        ),
        2_000,
    );

    let mut reply = Vec::new();
    for seg in nat.poll(3_000) {
        if flags(&seg) & PSH != 0 {
            reply.extend_from_slice(data(&seg));
        }
    }
    let text = String::from_utf8(reply).expect("an ASCII page");
    assert!(text.starts_with("HTTP/1.0 200 OK"), "{text}");
    assert!(text.ends_with("hello from litz\n"), "{text}");
}

#[test]
fn a_guest_that_connects_to_a_closed_port_is_refused() {
    let config = NetworkConfig::access_point("ssid=litz").with_service(Service::http(80, "ok"));
    let mut nat = Nat::with_transport(false, Box::new(litz_esp::EspRouter::new(&config)));
    nat.set_station(GUEST_MAC);

    nat.tcp_in(
        &GUEST_MAC,
        &GUEST_IP,
        &HOST_IP,
        &segment(GUEST_PORT, 9, 1000, 0, SYN, b""),
        0,
    );
    let sent = nat.poll(1_000);
    let reset = sent.first().expect("a reset");
    assert_eq!(flags(reset) & 0x04, 0x04, "expected RST, got {reset:02x?}");
}

#[test]
fn a_tcp_echo_service_returns_the_request() {
    let config =
        NetworkConfig::access_point("ssid=litz").with_service(Service::TcpEcho { port: 7 });
    let mut nat = Nat::with_transport(false, Box::new(litz_esp::EspRouter::new(&config)));
    nat.set_station(GUEST_MAC);
    nat.tcp_in(
        &GUEST_MAC,
        &GUEST_IP,
        &HOST_IP,
        &segment(GUEST_PORT, 7, 1000, 0, SYN, b""),
        0,
    );
    let server_seq = field(&nat.poll(1_000)[0], 4..8);
    nat.tcp_in(
        &GUEST_MAC,
        &GUEST_IP,
        &HOST_IP,
        &segment(
            GUEST_PORT,
            7,
            1001,
            server_seq.wrapping_add(1),
            ACK | PSH,
            b"ping",
        ),
        2_000,
    );
    let echoed: Vec<u8> = nat
        .poll(3_000)
        .iter()
        .filter(|seg| flags(seg) & PSH != 0)
        .flat_map(|seg| data(seg).to_vec())
        .collect();
    assert_eq!(echoed, b"ping");
}

#[test]
fn the_router_names_itself_for_any_lookup() {
    let config = NetworkConfig::for_testing("litz");
    let mut router = config.router();
    let id = router.resolve("anything.example", 1).expect("an A answer");
    assert_eq!(
        router.resolved().map(|(_, answer)| answer),
        Some(litz_net::Answer::Addrs(vec![HOST_IP], 60)),
        "lookup {id} should answer with the router's address"
    );
}

/// A DNS query for `litz.local`, as lwIP sends it: header, QNAME, A, IN.
fn dns_query(id: u16, name: &str) -> Vec<u8> {
    let mut msg = vec![
        (id >> 8) as u8,
        id as u8,
        0x01,
        0x00,
        0,
        1,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    for label in name.split('.') {
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.push(0);
    msg.extend_from_slice(&[0, 1, 0, 1]);
    msg
}

#[test]
fn the_nat_asks_the_router_for_a_name_and_relays_the_answer() {
    let config = NetworkConfig::for_testing("litz");
    let mut nat = Nat::with_transport(false, Box::new(litz_esp::EspRouter::new(&config)));
    nat.set_station(GUEST_MAC);
    let query = dns_query(0x1234, "litz.local");
    assert!(
        nat.dns_query(&GUEST_MAC, &GUEST_IP, 40_000, &HOST_IP, &query, 0),
        "the transport should answer names itself"
    );
    let frames = nat.poll(1_000);
    let frame = frames.first().expect("a DNS reply");
    // Ethernet II + IPv4 + UDP + DNS
    let udp = 14 + 20;
    assert_eq!(frame[14 + 9], 17, "not UDP: {frame:02x?}");
    let answer = &frame[udp + 8..];
    assert_eq!(&answer[..2], &[0x12, 0x34], "the query id comes back");
    assert!(
        answer.windows(4).any(|w| w == HOST_IP),
        "the router's address"
    );
}
