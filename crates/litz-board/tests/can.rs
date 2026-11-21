//! The board-level CAN fabric: deterministic delivery, ordered monitor, no duplicates.
//!
//! These tests do not involve firmware or an MCU. They exercise the fabric a chip adapter
//! attaches to, so the emulator's CAN behaviour can be reasoned about on its own.

use litz_board::bus::{Buses, CAN_MAX_DATA, CanBus, CanErrorState, CanFrame, CanFrameKind};

fn fabric_with_nodes(nodes: &[u16]) -> Buses {
    let mut buses = Buses::new();
    for &node in nodes {
        buses.can_mut().attach(node);
    }
    buses
}

#[test]
fn two_nodes_transmit_and_receive_in_order() {
    let mut buses = fabric_with_nodes(&[0, 1]);

    buses
        .can
        .transmit(0, CanFrame::standard(0x123, &[0x11, 0x22, 0x33]))
        .unwrap();

    let received = buses.can.take(1).expect("node 1 receives the frame");
    assert_eq!(received, CanFrame::standard(0x123, &[0x11, 0x22, 0x33]));
    assert_eq!(received.id, 0x123);
    assert!(!received.extended);
    assert!(!received.rtr);
    assert_eq!(received.len, 3);
    assert_eq!(&received.data[..3], &[0x11, 0x22, 0x33]);

    assert!(buses.can.take(1).is_none(), "a frame is delivered once");
    assert!(
        buses.can.take(0).is_none(),
        "the transmitter does not receive its own frame"
    );

    let monitor = buses.can.monitor();
    assert_eq!(monitor.len(), 1);
    assert_eq!(monitor[0].node, 0);
    assert_eq!(monitor[0].frame, received);
}

#[test]
fn a_frame_reaches_every_other_node_in_a_stable_order() {
    let mut buses = fabric_with_nodes(&[0, 1, 2]);
    buses
        .can
        .transmit(0, CanFrame::standard(0x010, &[1]))
        .unwrap();
    buses
        .can
        .transmit(0, CanFrame::standard(0x020, &[2]))
        .unwrap();

    for node in [1, 2] {
        assert_eq!(buses.can.pending(node), 2);
        assert_eq!(buses.can.take(node).unwrap().id, 0x010);
        assert_eq!(buses.can.take(node).unwrap().id, 0x020);
        assert!(buses.can.take(node).is_none());
    }

    let ids: Vec<u32> = buses.can.monitor().iter().map(|e| e.frame.id).collect();
    assert_eq!(ids, vec![0x010, 0x020]);
    assert!(buses.can.monitor().iter().all(|e| e.node == 0));

    buses
        .can
        .transmit(2, CanFrame::standard(0x030, &[3]))
        .unwrap();
    assert_eq!(buses.can.take(0).unwrap().id, 0x030);
    assert_eq!(buses.can.take(1).unwrap().id, 0x030);
    assert!(
        buses.can.take(2).is_none(),
        "the new transmitter does not loop back"
    );
}

#[test]
fn extended_ids_and_eight_data_bytes_survive_the_fabric() {
    let mut buses = fabric_with_nodes(&[4, 7]);
    let payload = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, 0x02, 0x03];
    let frame = CanFrame::extended(0x1fff_ffff, &payload);

    buses.can.transmit(4, frame).unwrap();
    let received = buses.can.take(7).expect("extended frame is delivered");
    assert_eq!(received, frame);
    assert!(received.extended);
    assert_eq!(received.id, 0x1fff_ffff);
    assert_eq!(received.len, 8);
    assert_eq!(&received.data[..8], &payload);
    assert_eq!(buses.can.monitor()[0].frame, frame);
}

#[test]
fn remote_frames_carry_their_requested_length() {
    let mut buses = fabric_with_nodes(&[0, 1]);
    let request = CanFrame::remote(0x055, false, 4);
    buses.can.transmit(0, request).unwrap();
    let received = buses.can.take(1).unwrap();
    assert!(received.rtr);
    assert_eq!(received.len, 4);
    assert_eq!(received.data, [0; CAN_MAX_DATA]);
}

#[test]
fn ids_are_masked_and_lengths_clamped() {
    let standard = CanFrame::standard(0x1234_5678, &[1, 2]);
    assert_eq!(standard.id, 0x678, "a standard id is eleven bits");
    let extended = CanFrame::extended(0xffff_ffff, &[0; 8]);
    assert_eq!(
        extended.id, 0x1fff_ffff,
        "an extended id is twenty-nine bits"
    );

    let mut buses = fabric_with_nodes(&[0, 1]);
    buses
        .can
        .transmit(
            0,
            CanFrame {
                id: 0x7ff,
                extended: false,
                rtr: false,
                len: 200,
                data: [9; CAN_MAX_DATA],
                kind: CanFrameKind::Classic,
            },
        )
        .unwrap();
    let received = buses.can.take(1).unwrap();
    assert_eq!(received.len, 8, "DLC is clamped to eight bytes");
}

#[test]
fn an_unattached_transmitter_is_refused_and_attach_is_idempotent() {
    let mut can = CanBus::new();
    can.attach(3);
    can.attach(3);
    assert_eq!(can.nodes().collect::<Vec<_>>(), vec![3]);
    assert!(can.is_attached(3));
    assert!(!can.is_attached(4));
    assert!(can.transmit(4, CanFrame::standard(1, &[])).is_err());
    assert!(can.monitor().is_empty(), "a refused transmit is not logged");
}

#[test]
fn the_monitor_log_can_be_drained_without_affecting_the_nodes() {
    let mut buses = fabric_with_nodes(&[0, 1]);
    buses
        .can
        .transmit(0, CanFrame::standard(0x001, &[]))
        .unwrap();
    buses
        .can
        .transmit(1, CanFrame::standard(0x002, &[]))
        .unwrap();

    let log = buses.can.take_monitor();
    assert_eq!(log.len(), 2);
    assert_eq!((log[0].node, log[0].frame.id), (0, 0x001));
    assert_eq!((log[1].node, log[1].frame.id), (1, 0x002));
    assert!(buses.can.monitor().is_empty());

    assert_eq!(buses.can.take(1).unwrap().id, 0x001);
    assert_eq!(buses.can.take(0).unwrap().id, 0x002);
}

#[test]
fn fd_frames_carry_sixty_four_bytes_and_a_brs_flag() {
    let mut buses = fabric_with_nodes(&[0, 1]);
    let payload: Vec<u8> = (0..64).collect();
    let frame = CanFrame::fd(0x1abc, true, true, &payload);

    assert!(frame.is_fd());
    assert_eq!(frame.len, 64);
    assert_eq!(frame.data, payload.as_slice());
    assert_eq!(
        frame.kind,
        CanFrameKind::Fd {
            bit_rate_switch: true
        }
    );

    buses.can.transmit(0, frame).unwrap();
    assert_eq!(buses.can.take(1), Some(frame));
    assert_eq!(buses.can.monitor()[0].frame, frame);
}

#[test]
fn error_frames_are_broadcast_and_count_receive_errors() {
    let mut buses = fabric_with_nodes(&[0, 1, 2]);
    buses.can.transmit(0, CanFrame::error()).unwrap();

    for node in [1, 2] {
        let received = buses.can.take(node).expect("an error frame reaches peers");
        assert!(received.is_error());
        assert_eq!(
            buses.can.error_state(node),
            Some(CanErrorState {
                tx_error_counter: 0,
                rx_error_counter: 8
            })
        );
    }
    assert!(
        buses.can.monitor()[0].frame.is_error(),
        "the monitor records the error event"
    );
}

#[test]
fn the_lowest_identifier_wins_a_simultaneous_round() {
    let mut buses = fabric_with_nodes(&[0, 1, 2]);
    let outcome = buses
        .can
        .arbitrate(&[
            (0, CanFrame::standard(0x300, &[0x30])),
            (1, CanFrame::standard(0x100, &[0x10])),
        ])
        .unwrap();

    assert_eq!(outcome.winner, Some(1));
    assert_eq!(outcome.losers, vec![0]);
    assert!(outcome.collision.is_empty());
    assert!(outcome.suppressed.is_empty());

    assert_eq!(buses.can.pending(2), 1, "the winner is delivered once");
    assert_eq!(buses.can.take(2).unwrap().id, 0x100);
    assert_eq!(
        buses.can.pending(1),
        0,
        "the winner does not receive its own frame"
    );
    assert_eq!(
        buses.can.pending(0),
        1,
        "the loser becomes a receiver of the winning frame"
    );
    assert_eq!(
        buses.can.error_state(0).unwrap().tx_error_counter,
        8,
        "the loser counts a transmit error"
    );
    assert_eq!(buses.can.monitor().len(), 1, "one wire event");
}

#[test]
fn a_standard_frame_beats_an_extended_frame_with_the_same_base_identifier() {
    let mut buses = fabric_with_nodes(&[0, 1, 2]);
    let outcome = buses
        .can
        .arbitrate(&[
            (0, CanFrame::extended(0x123, &[0xee])),
            (1, CanFrame::standard(0x123, &[0x5a])),
        ])
        .unwrap();

    assert_eq!(outcome.winner, Some(1));
    assert_eq!(outcome.losers, vec![0]);
    assert!(!buses.can.take(2).unwrap().extended);
}

#[test]
fn identical_frames_with_the_same_identifier_are_one_wire_event() {
    let mut buses = fabric_with_nodes(&[0, 1, 2]);
    let frame = CanFrame::standard(0x100, &[1, 2, 3]);
    let outcome = buses.can.arbitrate(&[(0, frame), (1, frame)]).unwrap();

    assert_eq!(outcome.winner, Some(0));
    assert!(outcome.losers.is_empty());
    assert!(outcome.collision.is_empty());
    assert_eq!(buses.can.pending(2), 1, "exactly one copy is delivered");
    assert_eq!(buses.can.pending(0), 0);
    assert_eq!(buses.can.pending(1), 0);
}

#[test]
fn same_identifier_with_different_payloads_reports_a_collision() {
    let mut buses = fabric_with_nodes(&[0, 1, 2]);
    let outcome = buses
        .can
        .arbitrate(&[
            (0, CanFrame::standard(0x100, &[1])),
            (1, CanFrame::standard(0x100, &[2])),
        ])
        .unwrap();

    assert_eq!(outcome.winner, None);
    assert_eq!(outcome.collision, vec![0, 1]);
    assert!(outcome.losers.is_empty());
    assert_eq!(buses.can.pending(2), 0, "nothing reaches the bus");
    assert_eq!(buses.can.error_state(0).unwrap().tx_error_counter, 8);
    assert_eq!(buses.can.error_state(1).unwrap().tx_error_counter, 8);
    assert!(buses.can.monitor().is_empty());
}

#[test]
fn a_bus_off_node_stops_transmitting_until_reset() {
    let mut buses = fabric_with_nodes(&[0, 1]);
    // Every malformed offer adds eight to node 0's transmit counter.
    for _ in 0..32 {
        buses.can.inject_malformed(0).unwrap();
    }
    assert!(buses.can.is_error_passive(0));
    assert!(buses.can.is_bus_off(0));
    assert_eq!(buses.can.error_state(0).unwrap().tx_error_counter, 256);
    assert!(buses.can.transmit(0, CanFrame::standard(1, &[])).is_err());

    buses.can.reset_errors(0).unwrap();
    assert!(!buses.can.is_bus_off(0));
    assert!(!buses.can.is_error_passive(0));
    while buses.can.pending(1) > 0 {
        assert!(buses.can.take(1).unwrap().is_error());
    }
    assert!(buses.can.transmit(0, CanFrame::standard(1, &[])).is_ok());
    assert_eq!(buses.can.take(1).unwrap().id, 1);
}

#[test]
fn an_error_passive_node_recovers_after_a_successful_frame() {
    let mut buses = fabric_with_nodes(&[0, 1]);
    for _ in 0..16 {
        buses.can.inject_malformed(0).unwrap();
    }
    assert_eq!(buses.can.error_state(0).unwrap().tx_error_counter, 128);
    assert!(buses.can.is_error_passive(0));
    assert!(
        buses.can.transmit(0, CanFrame::standard(7, &[])).is_ok(),
        "error-passive is not bus-off: transmission is still allowed"
    );

    // A valid frame from the other node, once taken, moves the counters back down.
    buses.can.transmit(1, CanFrame::standard(8, &[])).unwrap();
    assert_eq!(buses.can.take(0).unwrap().id, 8);
    // The retransmission above was successful (128 -> 127) and the received frame moves it
    // down once more.
    assert_eq!(buses.can.error_state(0).unwrap().tx_error_counter, 126);
    assert!(!buses.can.is_error_passive(0));
}

#[test]
fn same_tick_submissions_arbitrate_together() {
    let mut buses = fabric_with_nodes(&[0, 1, 2]);
    buses
        .can
        .submit_at(10, 0, CanFrame::standard(0x300, &[]))
        .unwrap();
    buses
        .can
        .submit_at(10, 1, CanFrame::standard(0x100, &[]))
        .unwrap();
    buses
        .can
        .submit_at(20, 2, CanFrame::standard(0x050, &[]))
        .unwrap();

    let first = buses.can.advance(10);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].winner, Some(1));
    assert_eq!(first[0].losers, vec![0]);
    assert_eq!(
        buses.can.pending(2),
        1,
        "only the earlier tick has resolved so far"
    );
    assert_eq!(buses.can.take(2).unwrap().id, 0x100);

    let second = buses.can.advance(20);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].winner, Some(2));
    assert_eq!(
        buses.can.take(0).unwrap().id,
        0x100,
        "the first round's loser still had that frame queued"
    );
    assert_eq!(buses.can.take(0).unwrap().id, 0x050);
    assert_eq!(buses.can.take(1).unwrap().id, 0x050);
    assert!(buses.can.advance(20).is_empty());
}
