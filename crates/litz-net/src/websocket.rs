//! A WebSocket server (RFC 6455) for the simulated network.
//!
//! The handshake is ordinary HTTP, so a firmware's own WebSocket library does
//! the protocol work and the router only has to behave like a server: accept
//! the upgrade, check the key, mask nothing it sends, and unmask what it
//! receives. The service echoes text and binary messages, answers pings and
//! closes politely, which is enough for a client test to prove it negotiated
//! subprotocol-free WebSocket over the simulated link.
//!
//! Nothing here reads a clock or a random source: the accept digest is
//! SHA-1 over the guest's key, and the frames follow the bytes the guest sent.

use std::collections::VecDeque;

/// The GUID RFC 6455 appends to the client key before hashing it.
const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A frame larger than this is refused rather than buffered without bound.
const MAX_FRAME: usize = 1 << 20;

const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xa;

/// One server connection.
pub struct Server {
    request: Vec<u8>,
    upgraded: bool,
    inbox: Vec<u8>,
    out: VecDeque<u8>,
    /// A fragmented message whose first frame arrived.
    fragment: Option<(u8, Vec<u8>)>,
    closed: bool,
    refused: bool,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Self {
            request: Vec::new(),
            upgraded: false,
            inbox: Vec::new(),
            out: VecDeque::new(),
            fragment: None,
            closed: false,
            refused: false,
        }
    }

    /// Accept bytes from the guest. Before the upgrade they are HTTP; after
    /// it they are frames.
    pub fn feed(&mut self, data: &[u8]) {
        if self.closed {
            return;
        }
        if !self.upgraded {
            self.request.extend_from_slice(data);
            if let Some(end) = find(&self.request, b"\r\n\r\n") {
                let head = self.request[..end + 4].to_vec();
                let rest = self.request[end + 4..].to_vec();
                self.request.clear();
                if self.upgrade(&head) {
                    self.upgraded = true;
                    if !rest.is_empty() {
                        self.inbox.extend(rest);
                    }
                } else {
                    self.refused = true;
                    self.closed = true;
                    return;
                }
            }
        } else {
            self.inbox.extend_from_slice(data);
        }
        if self.upgraded {
            self.frames();
        }
    }

    /// Bytes the server owes the guest, drained.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.out.drain(..).collect()
    }

    /// The handshake finished and frames are flowing.
    pub fn upgraded(&self) -> bool {
        self.upgraded
    }

    /// The connection is over (close frame, refused handshake or EOF).
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// Whether a close was sent, so a flow can report end of file after its
    /// last ciphertext leaves.
    pub fn closing(&self) -> bool {
        self.closed || self.refused
    }

    fn upgrade(&mut self, head: &[u8]) -> bool {
        let text = String::from_utf8_lossy(head);
        let mut key = None;
        let mut upgrade = false;
        let mut connection = false;
        let mut version_ok = false;
        for line in text.lines().skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "sec-websocket-key" => key = Some(value.to_string()),
                "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
                "connection" => {
                    connection = value
                        .split(',')
                        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
                }
                "sec-websocket-version" => version_ok = value == "13",
                _ => {}
            }
        }
        let (Some(key), true, true, true) = (key, upgrade, connection, version_ok) else {
            let response =
                "HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
            self.out.extend(response.as_bytes());
            return false;
        };
        let mut digest = key.into_bytes();
        digest.extend_from_slice(GUID);
        let accept = base64(&sha1(&digest));
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        self.out.extend(response.as_bytes());
        true
    }

    fn frames(&mut self) {
        while let Some((frame, used)) = parse_frame(&self.inbox) {
            self.inbox.drain(..used);
            match frame {
                Frame::Close => {
                    self.send_frame(OP_CLOSE, &[0x03, 0xe8]);
                    self.closed = true;
                }
                Frame::Ping(payload) => self.send_frame(OP_PONG, &payload),
                Frame::Pong => {}
                Frame::Data {
                    opcode,
                    fin,
                    payload,
                } => {
                    if opcode == OP_CONTINUATION {
                        let Some((initial, mut message)) = self.fragment.take() else {
                            self.send_frame(OP_CLOSE, &[0x03, 0xea]);
                            self.closed = true;
                            continue;
                        };
                        message.extend(payload);
                        if fin {
                            self.send_frame(initial, &message);
                        } else {
                            self.fragment = Some((initial, message));
                        }
                    } else if fin {
                        self.send_frame(opcode, &payload);
                    } else {
                        self.fragment = Some((opcode, payload));
                    }
                }
            }
        }
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) {
        self.out.extend(frame(opcode, payload));
    }
}

enum Frame {
    Close,
    Ping(Vec<u8>),
    Pong,
    Data {
        opcode: u8,
        fin: bool,
        payload: Vec<u8>,
    },
}

/// Parse one client frame. `None` means the buffer does not hold a whole one.
fn parse_frame(bytes: &[u8]) -> Option<(Frame, usize)> {
    if bytes.len() < 2 {
        return None;
    }
    let fin = bytes[0] & 0x80 != 0;
    let opcode = bytes[0] & 0x0f;
    let masked = bytes[1] & 0x80 != 0;
    if !masked {
        // A client frame must be masked; an unmasked one is a protocol error,
        // but reporting it as a close frame keeps the caller simple.
        return Some((Frame::Close, 2));
    }
    let (length, mut offset) = match bytes[1] & 0x7f {
        126 => {
            if bytes.len() < 4 {
                return None;
            }
            (u16::from_be_bytes([bytes[2], bytes[3]]) as usize, 4)
        }
        127 => {
            if bytes.len() < 10 {
                return None;
            }
            let mut eight = [0u8; 8];
            eight.copy_from_slice(&bytes[2..10]);
            let length = u64::from_be_bytes(eight) as usize;
            (length, 10)
        }
        short => (short as usize, 2),
    };
    if length > MAX_FRAME {
        return Some((Frame::Close, 2));
    }
    if bytes.len() < offset + 4 + length {
        return None;
    }
    let mask = [
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ];
    offset += 4;
    let payload: Vec<u8> = bytes[offset..offset + length]
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ mask[index % 4])
        .collect();
    let used = offset + length;
    match opcode {
        OP_CLOSE => Some((Frame::Close, used)),
        OP_PING => Some((Frame::Ping(payload), used)),
        OP_PONG => Some((Frame::Pong, used)),
        OP_TEXT | OP_BINARY | OP_CONTINUATION => Some((
            Frame::Data {
                opcode,
                fin,
                payload,
            },
            used,
        )),
        _ => Some((Frame::Close, used)),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A server frame as the guest receives it: no mask, FIN set.
pub fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    let length = payload.len();
    if length < 126 {
        out.push(length as u8);
    } else if length <= u16::MAX as usize {
        out.push(126);
        out.extend((length as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend((length as u64).to_be_bytes());
    }
    out.extend(payload);
    out
}

/// A client frame, masked with `mask`, as a guest sends it.
pub fn masked_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    let length = payload.len();
    if length < 126 {
        out.push(0x80 | length as u8);
    } else if length <= u16::MAX as usize {
        out.push(0x80 | 126);
        out.extend((length as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend((length as u64).to_be_bytes());
    }
    out.extend(mask);
    out.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    out
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    let bit_len = (data.len() as u64) * 8;
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend(bit_len.to_be_bytes());

    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[index] = u32::from_be_bytes(*word);
        }
        for index in 16..80 {
            w[index] = (w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (index, word) in w.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (index, word) in h.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let first = chunk[0] as u32;
        let second = chunk.get(1).copied().unwrap_or(0) as u32;
        let third = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (first << 16) | (second << 8) | third;
        out.push(TABLE[(triple >> 18) as usize & 63] as char);
        out.push(TABLE[(triple >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(triple >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[triple as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake(server: &mut Server) -> String {
        server.feed(b"GET /chat HTTP/1.1\r\nHost: api.litz.dev\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n");
        String::from_utf8(server.take_output()).unwrap()
    }

    #[test]
    fn sha1_and_base64_match_the_rfc_vectors() {
        assert_eq!(base64(&sha1(b"abc")), "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=");
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi"), "aGk=");
    }

    #[test]
    fn the_rfc_handshake_accepts_its_sample_key() {
        let mut server = Server::new();
        let response = handshake(&mut server);
        assert!(response.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        assert!(response.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
        assert!(server.upgraded());
    }

    #[test]
    fn text_is_echoed_and_pings_are_answered() {
        let mut server = Server::new();
        handshake(&mut server);
        server.feed(&masked_frame(OP_TEXT, b"hello", [1, 2, 3, 4]));
        assert_eq!(server.take_output(), frame(OP_TEXT, b"hello"));
        server.feed(&masked_frame(OP_PING, b"pi", [9, 9, 9, 9]));
        assert_eq!(server.take_output(), frame(OP_PONG, b"pi"));
    }

    #[test]
    fn a_split_header_and_a_large_frame_are_buffered() {
        let mut server = Server::new();
        server.feed(b"GET / HTTP/1.1\r\nUpgrade: websocket\r\n");
        assert!(server.take_output().is_empty());
        server.feed(b"Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n");
        assert!(server.upgraded());
        let response = String::from_utf8(server.take_output()).unwrap();
        assert!(response.starts_with("HTTP/1.1 101"));
        let payload = vec![7u8; 70_000];
        server.feed(&masked_frame(OP_BINARY, &payload, [4, 3, 2, 1]));
        assert_eq!(server.take_output(), frame(OP_BINARY, &payload));
    }

    #[test]
    fn a_bad_handshake_is_refused_and_a_close_ends_the_connection() {
        let mut server = Server::new();
        server.feed(b"GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n");
        assert!(
            String::from_utf8(server.take_output())
                .unwrap()
                .starts_with("HTTP/1.1 400")
        );
        assert!(server.closed());

        let mut server = Server::new();
        handshake(&mut server);
        server.feed(&masked_frame(OP_CLOSE, &[0x03, 0xe8], [0; 4]));
        assert_eq!(server.take_output(), frame(OP_CLOSE, &[0x03, 0xe8]));
        assert!(server.closed());
    }
}
