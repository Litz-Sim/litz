//! A native ESP32 guest, as a BLE central, reaching a scan-list device's GATT
//! table.
//!
//! The packets are the HCI and ATT ones a real NimBLE client sends: create a
//! connection to the address a scan reported, discover services and
//! characteristics, read and write a value. The point is that the table comes
//! from the scan list (the circuit's beacon), not from the controller's one
//! built-in battery service.

#![cfg(feature = "esp32c3")]

use esp_soc::ble::peer::{GattCharacteristic, GattService, Nearby, Session};

const ADDR: [u8; 6] = [0x04, 0x03, 0x02, 0x01, 0xaa, 0xc4];

fn cmd(session: &mut Session, op: u16, payload: &[u8]) {
    let mut packet = vec![1, op as u8, (op >> 8) as u8, payload.len() as u8];
    packet.extend_from_slice(payload);
    session.send(&packet);
}

fn acl(session: &mut Session, pdu: &[u8]) {
    let mut packet = vec![
        2,
        1,
        0x20,
        (pdu.len() + 4) as u8,
        0,
        pdu.len() as u8,
        0,
        4,
        0,
    ];
    packet.extend_from_slice(pdu);
    session.send(&packet);
}

/// The ATT PDU of the next ACL packet, skipping command-complete events.
fn att(session: &mut Session) -> Vec<u8> {
    while let Some(packet) = session.pop_packet() {
        if packet.first() != Some(&2) || packet.len() < 9 {
            continue;
        }
        let len = usize::from(packet[5]) | usize::from(packet[6]) << 8;
        let cid = u16::from(packet[7]) | u16::from(packet[8]) << 8;
        if cid == 4 && len > 0 && packet.len() >= 9 + len {
            return packet[9..9 + len].to_vec();
        }
    }
    panic!("no ACL response");
}

#[test]
fn a_guest_central_discovers_and_uses_the_scan_lists_table() {
    let mut session = Session::default();
    session.set_nearby(vec![Nearby {
        addr: ADDR,
        addr_type: 0,
        rssi: -40,
        adv: vec![2, 1, 6, 5, 9, b'n', b'o', b'd', b'e'],
        scan_response: Vec::new(),
        gatt: vec![GattService {
            uuid: 0x180f,
            characteristics: vec![
                GattCharacteristic {
                    uuid: 0x2a19,
                    flags: 1,
                    value: vec![0x64],
                },
                GattCharacteristic {
                    uuid: 0x2a1a,
                    flags: 3,
                    value: Vec::new(),
                },
            ],
        }],
    }]);

    // LE Create Connection to the scanned address: filter policy none, public
    // address, 7.5 ms interval and window, no latency, 4 s supervision.
    let mut connect = vec![0x06, 0x00, 0x06, 0x00, 0x00, 0x00];
    connect.extend_from_slice(&ADDR);
    connect.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    assert_eq!(connect.len(), 25);
    cmd(&mut session, 0x200d, &connect);

    // Read By Group Type: services from handle 1 through 0xffff.
    acl(&mut session, &[0x10, 1, 0, 0xff, 0xff, 0x00, 0x28]);
    let services = att(&mut session);
    assert_eq!(services[0], 0x11);
    assert_eq!(&services[2..8], &[1, 0, 5, 0, 0x0f, 0x18]);

    // Read By Type: characteristic declarations in the service's range.
    acl(&mut session, &[0x08, 1, 0, 5, 0, 0x03, 0x28]);
    let characteristics = att(&mut session);
    assert_eq!(characteristics[0], 0x09);
    assert!(
        characteristics.windows(2).any(|w| w == [0x19, 0x2a]),
        "{characteristics:02x?}"
    );

    // Read the battery level at its value handle.
    acl(&mut session, &[0x0a, 3, 0]);
    assert_eq!(att(&mut session), vec![0x0b, 0x64]);

    // Write the second characteristic and read it back.
    acl(&mut session, &[0x12, 5, 0, 0xaa]);
    assert_eq!(att(&mut session), vec![0x13]);
    acl(&mut session, &[0x0a, 5, 0]);
    assert_eq!(att(&mut session), vec![0x0b, 0xaa]);
}
