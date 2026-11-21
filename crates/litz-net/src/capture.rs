//! A packet capture of the simulated network, in pcap form.
//!
//! Wokwi downloads a PCAP of the frames its gateway carried; this is the same
//! idea one layer up: every TCP segment, UDP datagram and DNS query the router
//! answers is written as an Ethernet/IPv4 frame with a timestamp taken from
//! simulated time. Wireshark opens the result, and two runs of the same
//! firmware produce byte-identical captures because nothing reads a clock.
//!
//! The frames are synthesised from the router's decisions rather than tapped
//! off a backend's radio, so the capture works for an ESP32, a wasm MCU or a
//! test that drives [`crate::Router`] directly. The link layer is Ethernet
//! with fixed locally administered addresses; it is not 802.11, because the
//! frame the guest's MAC wrote is the backend's business and differs between
//! backends.

use crate::{Answer, HOST_IP};

/// The station's address on the simulated link.
pub const STATION_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x15];
/// The router's address on the simulated link.
pub const GATEWAY_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];

/// TCP flags as they appear in the header.
pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;

/// One recorded frame.
#[derive(Clone, Debug)]
struct Frame {
    at_ns: u64,
    bytes: Vec<u8>,
}

/// A capture buffer the router writes into and a caller reads out.
#[derive(Debug)]
pub struct Capture {
    station: [u8; 4],
    frames: Vec<Frame>,
    next_ip_id: u16,
}

impl Capture {
    pub fn new(station: [u8; 4]) -> Self {
        Self {
            station,
            frames: Vec::new(),
            next_ip_id: 1,
        }
    }

    /// The lease the client received; frames use it as their source once set.
    pub fn set_station(&mut self, station: [u8; 4]) {
        self.station = station;
    }

    pub fn station(&self) -> [u8; 4] {
        self.station
    }

    /// How many frames the capture holds.
    pub fn frames(&self) -> usize {
        self.frames.len()
    }

    /// One TCP segment. `from_station` decides the direction.
    #[allow(clippy::too_many_arguments)]
    pub fn tcp(
        &mut self,
        at_ns: u64,
        from_station: bool,
        src_port: u16,
        dst_port: u16,
        seq: u32,
        ack: u32,
        flags: u8,
        payload: &[u8],
    ) {
        let (src, dst) = if from_station {
            (self.station, HOST_IP)
        } else {
            (HOST_IP, self.station)
        };
        let mut tcp = Vec::with_capacity(20 + payload.len());
        tcp.extend_from_slice(&src_port.to_be_bytes());
        tcp.extend_from_slice(&dst_port.to_be_bytes());
        tcp.extend_from_slice(&seq.to_be_bytes());
        tcp.extend_from_slice(&ack.to_be_bytes());
        tcp.push(5 << 4);
        tcp.push(flags);
        tcp.extend_from_slice(&64240u16.to_be_bytes());
        tcp.extend_from_slice(&[0, 0]); // checksum, filled below
        tcp.extend_from_slice(&[0, 0]);
        tcp.extend_from_slice(payload);
        let length = (tcp.len() as u16).to_be_bytes();
        let sum = checksum(&[&src, &dst, &[0, 6], &length, &tcp]);
        tcp[16..18].copy_from_slice(&sum.to_be_bytes());
        self.ip(at_ns, from_station, 6, &tcp);
    }

    /// One UDP datagram.
    pub fn udp(
        &mut self,
        at_ns: u64,
        from_station: bool,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) {
        let mut udp = Vec::with_capacity(8 + payload.len());
        udp.extend_from_slice(&src_port.to_be_bytes());
        udp.extend_from_slice(&dst_port.to_be_bytes());
        udp.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        udp.extend_from_slice(&[0, 0]); // checksum optional in IPv4
        udp.extend_from_slice(payload);
        self.ip(at_ns, from_station, 17, &udp);
    }

    /// A DNS query as the router receives it.
    pub fn dns_query(&mut self, at_ns: u64, id: u16, name: &str, qtype: u16) {
        let payload = dns_message(id, 0x0100, 1, name, qtype, &[]);
        self.udp(at_ns, true, 53, 53, &payload);
    }

    /// A DNS answer as the router sends it.
    pub fn dns_answer(&mut self, at_ns: u64, id: u16, name: &str, qtype: u16, answer: &Answer) {
        let addrs = match answer {
            Answer::Addrs(addrs, _) => addrs.as_slice(),
            Answer::Failed(code) => {
                let payload = dns_message(id, 0x8180 | u16::from(*code), 0, name, qtype, &[]);
                self.udp(at_ns, false, 53, 53, &payload);
                return;
            }
        };
        let payload = dns_message(id, 0x8180, 1, name, qtype, addrs);
        self.udp(at_ns, false, 53, 53, &payload);
    }

    /// The capture as a nanosecond-resolution pcap file.
    pub fn to_pcap(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24 + self.frames.len() * 96);
        out.extend_from_slice(&0xa1b2_3c4du32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&65535u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        for frame in &self.frames {
            let seconds = (frame.at_ns / 1_000_000_000) as u32;
            let nanos = (frame.at_ns % 1_000_000_000) as u32;
            out.extend_from_slice(&seconds.to_le_bytes());
            out.extend_from_slice(&nanos.to_le_bytes());
            out.extend_from_slice(&(frame.bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&(frame.bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&frame.bytes);
        }
        out
    }

    fn ip(&mut self, at_ns: u64, from_station: bool, protocol: u8, payload: &[u8]) {
        let (src, dst, src_mac, dst_mac) = if from_station {
            (self.station, HOST_IP, STATION_MAC, GATEWAY_MAC)
        } else {
            (HOST_IP, self.station, GATEWAY_MAC, STATION_MAC)
        };
        let total = 20 + payload.len();
        let mut header = Vec::with_capacity(total);
        header.push(0x45);
        header.push(0);
        header.extend_from_slice(&(total as u16).to_be_bytes());
        header.extend_from_slice(&self.next_ip_id.to_be_bytes());
        self.next_ip_id = self.next_ip_id.wrapping_add(1);
        header.extend_from_slice(&0x4000u16.to_be_bytes());
        header.push(64);
        header.push(protocol);
        header.extend_from_slice(&[0, 0]);
        header.extend_from_slice(&src);
        header.extend_from_slice(&dst);
        let checksum = checksum(&[&header]);
        header[10..12].copy_from_slice(&checksum.to_be_bytes());
        header.extend_from_slice(payload);

        let mut frame = Vec::with_capacity(14 + header.len());
        frame.extend_from_slice(&dst_mac);
        frame.extend_from_slice(&src_mac);
        frame.extend_from_slice(&0x0800u16.to_be_bytes());
        frame.extend_from_slice(&header);
        self.frames.push(Frame {
            at_ns,
            bytes: frame,
        });
    }
}

/// One's-complement checksum over a sequence of byte slices.
fn checksum(parts: &[&[u8]]) -> u16 {
    let mut sum = 0u32;
    let mut carry = 0u8;
    for part in parts {
        let mut index = 0;
        if carry == 1 {
            if let Some(&first) = part.first() {
                sum += u32::from(first) << 8;
                index = 1;
            }
            carry = 0;
        }
        while index + 1 < part.len() {
            sum += u32::from(u16::from_be_bytes([part[index], part[index + 1]]));
            index += 2;
        }
        if index < part.len() {
            sum += u32::from(part[index]) << 8;
            carry = 1;
        }
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// A DNS message with one question and, when `answers` is not empty, one A
/// record per address.
fn dns_message(
    id: u16,
    flags: u16,
    questions: u16,
    name: &str,
    qtype: u16,
    answers: &[[u8; 4]],
) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(&id.to_be_bytes());
    message.extend_from_slice(&flags.to_be_bytes());
    message.extend_from_slice(&questions.to_be_bytes());
    message.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    message.extend_from_slice(&0u16.to_be_bytes());
    message.extend_from_slice(&0u16.to_be_bytes());
    for label in name.split('.').filter(|label| !label.is_empty()) {
        message.push(label.len() as u8);
        message.extend_from_slice(label.as_bytes());
    }
    message.push(0);
    message.extend_from_slice(&qtype.to_be_bytes());
    message.extend_from_slice(&1u16.to_be_bytes());
    for address in answers {
        message.extend_from_slice(&0xc00cu16.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes());
        message.extend_from_slice(&60u32.to_be_bytes());
        message.extend_from_slice(&4u16.to_be_bytes());
        message.extend_from_slice(address);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_is_a_pcap_with_the_frames_in_order() {
        let mut capture = Capture::new([10, 0, 2, 15]);
        capture.tcp(1_000, true, 49152, 80, 1, 0, SYN, &[]);
        capture.tcp(2_000, false, 80, 49152, 1, 2, SYN | ACK, &[]);
        capture.tcp(
            3_000,
            true,
            49152,
            80,
            2,
            2,
            PSH | ACK,
            b"GET / HTTP/1.0\r\n\r\n",
        );

        let pcap = capture.to_pcap();
        assert_eq!(&pcap[..4], &0xa1b2_3c4du32.to_le_bytes());
        assert_eq!(u32::from_le_bytes(pcap[20..24].try_into().unwrap()), 1);
        // Three record headers and the frames themselves.
        assert!(pcap.len() > 24 + 3 * 16);
        assert_eq!(capture.frames(), 3);
    }

    #[test]
    fn a_dns_answer_carries_the_address() {
        let mut capture = Capture::new([10, 0, 2, 15]);
        capture.dns_answer(
            0,
            0x1234,
            "litz.local",
            1,
            &Answer::Addrs(vec![[10, 0, 2, 3]], 60),
        );
        let pcap = capture.to_pcap();
        assert!(pcap.windows(4).any(|w| w == [10, 0, 2, 3]));
    }

    #[test]
    fn the_ip_checksum_of_a_frame_is_valid() {
        let mut capture = Capture::new([10, 0, 2, 15]);
        capture.udp(0, true, 1000, 53, b"hello");
        let frame = &capture.frames[0].bytes;
        let ip = &frame[14..];
        // Summing an IP header that contains a correct checksum gives ffff.
        assert_eq!(checksum(&[&ip[..20]]), 0);
    }
}
