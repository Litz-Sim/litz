//! Network monitoring: the Ethernet frames the WiFi station sends and receives (what the virtual
//! network, the NAT or the host relay sees), written to a classic pcap file (`--pcap`) and/or
//! streamed to the board page (`--web`). The station link (`wifi::StationLink`) records into a
//! shared `NetTap`; the timestamps are emulated time in microseconds.
//!
//! Beside it sits the air capture (`AirCapture`/`AirTap`): every 802.11 frame the virtual air
//! carries, written to a pcap whose link type is 105 (IEEE 802.11) rather than 1 (Ethernet), so
//! Wireshark dissects the management, control and data frames as they went over the air. Its
//! records are the same shape as the Ethernet capture's.
//!
//! Recording scheme (one record per frame, never two):
//! * a frame handed to `StationLink::station_tx` is recorded at its entry, before any conversion
//!   to Ethernet, timestamped with the emulated time of that call; it is TX when the guest is the
//!   station and RX when a virtual station is (the guest is then the access point);
//! * the frames an access point or virtual station generates are recorded by their producer — an
//!   access point's `send` and its beacon timer (`VirtualAp`), the virtual station's `send`
//!   (which `send_data` uses, `VirtualSta`) — at the frame's scheduled air time
//!   (`AirFrame::at_us`). Recording at generation means a beacon or management response is
//!   captured even if the receive ring later drops it;
//! * the data frame the network hands over in `StationLink::next_rx` is recorded there, when it is
//!   wrapped into 802.11. That is the only frame `next_rx` records: everything else it returns was
//!   already recorded by its producer, so no frame is recorded twice.
//!
//! Direction is relative to the station of the conversation: `Dir::FromStation` (TX) for a frame
//! the station transmits and `Dir::ToStation` (RX) for one it receives. That holds in both
//! topologies: with the guest as the station and the virtual access point answering, and with the
//! virtual station (`--wifi-sta`) as the station and the guest's access point answering.
//!
//! Frames are captured without the 4-byte FCS: the guest's MAC hands its transmit frames over
//! with an FCS slot and the receive path appends the real FCS, but the virtual AP and station
//! generate plain 802.11 frames, so leaving the tail off both directions keeps one frame format
//! (as the Ethernet capture leaves the FCS off too).
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// The direction of a captured frame, as the websocket message carries it. Relative to the
/// station: `FromStation` is transmitted by it (air capture TX), `ToStation` is received by it
/// (air capture RX).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    FromStation = 0,
    ToStation = 1,
}

impl Dir {
    /// The station transmitted this frame (air capture TX).
    pub const TX: Dir = Dir::FromStation;
    /// The station received this frame (air capture RX).
    pub const RX: Dir = Dir::ToStation;
}

/// The board page's binary message type for a captured Ethernet frame:
/// `[5][dir u8][ts_us u64 le][frame…]`.
pub const WS_NET_TYPE: u8 = 5;
/// The board page's binary message type for a captured 802.11 frame (an air capture):
/// `[6][dir u8][ts_us u64 le][frame…]`.
pub const WS_AIR_TYPE: u8 = 6;
/// The pcap link type of the Ethernet capture.
pub const LINKTYPE_ETHERNET: u32 = 1;
/// The pcap link type of the air capture: raw IEEE 802.11, as Wireshark expects it.
pub const LINKTYPE_IEEE802_11: u32 = 105;
/// The live backlog the page has not been sent yet: beyond either bound, new frames are dropped
/// (counted), so a busy network cannot grow memory while the emulator outruns the push interval.
const LIVE_FRAMES_MAX: usize = 512;
const LIVE_BYTES_MAX: usize = 1 << 20;
/// A pcap record keeps at most this much of a frame (the global header's snaplen).
const SNAPLEN: u32 = 65535;

/// The classic pcap global header: magic a1b2c3d4 (microsecond timestamps), version 2.4, UTC,
/// snaplen 65535, the given link type, little-endian.
pub fn pcap_global_header_for(linktype: u32) -> [u8; 24] {
    let mut h = [0u8; 24];
    h[0..4].copy_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    h[4..6].copy_from_slice(&2u16.to_le_bytes());
    h[6..8].copy_from_slice(&4u16.to_le_bytes());
    h[16..20].copy_from_slice(&SNAPLEN.to_le_bytes());
    h[20..24].copy_from_slice(&linktype.to_le_bytes());
    h
}

/// The Ethernet capture's global header: linktype 1 (Ethernet).
pub fn pcap_global_header() -> [u8; 24] {
    pcap_global_header_for(LINKTYPE_ETHERNET)
}

/// The air capture's global header: linktype 105 (IEEE 802.11).
pub fn air_pcap_global_header() -> [u8; 24] {
    pcap_global_header_for(LINKTYPE_IEEE802_11)
}

/// One pcap record: header (seconds, microseconds, captured and original length) and the frame.
pub fn pcap_record(ts_us: u64, frame: &[u8]) -> Vec<u8> {
    let cap = frame.len().min(SNAPLEN as usize);
    let mut r = Vec::with_capacity(16 + cap);
    r.extend_from_slice(&((ts_us / 1_000_000) as u32).to_le_bytes());
    r.extend_from_slice(&((ts_us % 1_000_000) as u32).to_le_bytes());
    r.extend_from_slice(&(cap as u32).to_le_bytes());
    r.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    r.extend_from_slice(&frame[..cap]);
    r
}

/// The board page's message for one frame, under message type `kind`.
fn ws_message_of(kind: u8, dir: Dir, ts_us: u64, frame: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(10 + frame.len());
    m.push(kind);
    m.push(dir as u8);
    m.extend_from_slice(&ts_us.to_le_bytes());
    m.extend_from_slice(frame);
    m
}

/// The board page's message for one Ethernet frame.
pub fn ws_message(dir: Dir, ts_us: u64, frame: &[u8]) -> Vec<u8> {
    ws_message_of(WS_NET_TYPE, dir, ts_us, frame)
}

/// The board page's message for one 802.11 frame (an air capture).
pub fn ws_air_message(dir: Dir, ts_us: u64, frame: &[u8]) -> Vec<u8> {
    ws_message_of(WS_AIR_TYPE, dir, ts_us, frame)
}

/// Where captured frames go: a pcap sink and/or the live queue for the board page.
#[derive(Default)]
pub struct NetCapture {
    pcap: Option<Box<dyn Write + Send>>,
    live: Option<VecDeque<Vec<u8>>>,
    live_bytes: usize,
    /// frames left out of the live queue because it was full
    pub live_dropped: u64,
    /// frames recorded
    pub frames: u64,
}

/// The capture the station link and the machine share.
pub type NetTap = Arc<Mutex<NetCapture>>;

impl NetCapture {
    /// Write a pcap to `sink`, starting with the global header now.
    pub fn set_pcap(&mut self, mut sink: Box<dyn Write + Send>) -> std::io::Result<()> {
        sink.write_all(&pcap_global_header())?;
        sink.flush()?;
        self.pcap = Some(sink);
        Ok(())
    }
    /// Keep frames for the board page (`take_live`).
    pub fn enable_live(&mut self) {
        self.live.get_or_insert_with(VecDeque::new);
    }
    pub fn live_enabled(&self) -> bool {
        self.live.is_some()
    }
    /// Record one frame. A pcap write error ends the pcap output, with one line saying so.
    pub fn record(&mut self, dir: Dir, ts_us: u64, frame: &[u8]) {
        self.frames += 1;
        if let Some(sink) = self.pcap.as_mut() {
            if let Err(e) = sink
                .write_all(&pcap_record(ts_us, frame))
                .and_then(|_| sink.flush())
            {
                eprintln!("[emu] --pcap: {e}; capture stopped");
                self.pcap = None;
            }
        }
        if let Some(q) = self.live.as_mut() {
            let msg = ws_message(dir, ts_us, frame);
            if q.len() >= LIVE_FRAMES_MAX || self.live_bytes + msg.len() > LIVE_BYTES_MAX {
                self.live_dropped += 1;
                return;
            }
            self.live_bytes += msg.len();
            q.push_back(msg);
        }
    }
    /// The page messages queued since the last call.
    pub fn take_live(&mut self) -> Vec<Vec<u8>> {
        self.live_bytes = 0;
        self.live
            .as_mut()
            .map(|q| q.drain(..).collect())
            .unwrap_or_default()
    }
}

/// Where captured 802.11 frames go: a pcap sink (link type 105) and/or the live queue for the
/// board page. The API mirrors `NetCapture`; only the link type and the live message type differ.
#[derive(Default)]
pub struct AirCapture {
    pcap: Option<Box<dyn Write + Send>>,
    live: Option<VecDeque<Vec<u8>>>,
    live_bytes: usize,
    /// frames left out of the live queue because it was full
    pub live_dropped: u64,
    /// frames recorded
    pub frames: u64,
}

/// The air capture the station link shares.
pub type AirTap = Arc<Mutex<AirCapture>>;

impl AirCapture {
    /// Write a pcap to `sink`, starting with the link type 105 global header now.
    pub fn set_pcap(&mut self, mut sink: Box<dyn Write + Send>) -> std::io::Result<()> {
        sink.write_all(&air_pcap_global_header())?;
        sink.flush()?;
        self.pcap = Some(sink);
        Ok(())
    }
    /// Keep frames for the board page (`take_live`).
    pub fn enable_live(&mut self) {
        self.live.get_or_insert_with(VecDeque::new);
    }
    pub fn live_enabled(&self) -> bool {
        self.live.is_some()
    }
    /// Record one 802.11 frame. A pcap write error ends the pcap output, with one line saying so.
    pub fn record(&mut self, dir: Dir, ts_us: u64, frame: &[u8]) {
        self.frames += 1;
        if let Some(sink) = self.pcap.as_mut() {
            if let Err(e) = sink
                .write_all(&pcap_record(ts_us, frame))
                .and_then(|_| sink.flush())
            {
                eprintln!("[emu] --pcap-air: {e}; capture stopped");
                self.pcap = None;
            }
        }
        if let Some(q) = self.live.as_mut() {
            let msg = ws_air_message(dir, ts_us, frame);
            if q.len() >= LIVE_FRAMES_MAX || self.live_bytes + msg.len() > LIVE_BYTES_MAX {
                self.live_dropped += 1;
                return;
            }
            self.live_bytes += msg.len();
            q.push_back(msg);
        }
    }
    /// The page messages queued since the last call.
    pub fn take_live(&mut self) -> Vec<Vec<u8>> {
        self.live_bytes = 0;
        self.live
            .as_mut()
            .map(|q| q.drain(..).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn pcap_has_global_header_and_a_record_per_frame_both_ways() {
        let buf = Buf::default();
        let mut cap = NetCapture::default();
        cap.set_pcap(Box::new(buf.clone())).unwrap();
        cap.record(Dir::FromStation, 1_500_000, &[0xaa; 60]);
        cap.record(Dir::ToStation, 2_000_007, &[0xbb; 42]);
        let b = buf.0.lock().unwrap().clone();
        assert_eq!(
            &b[..24],
            &[
                0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 1, 0,
                0, 0
            ]
        );
        let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!((u(24), u(28), u(32), u(36)), (1, 500_000, 60, 60));
        assert_eq!(&b[40..100], &[0xaa; 60]);
        assert_eq!((u(100), u(104), u(108), u(112)), (2, 7, 42, 42));
        assert_eq!(&b[116..], &[0xbb; 42]);
        assert_eq!(cap.frames, 2);
    }

    #[test]
    fn air_pcap_has_linktype_105_and_a_record_per_frame_both_ways() {
        let buf = Buf::default();
        let mut cap = AirCapture::default();
        cap.set_pcap(Box::new(buf.clone())).unwrap();
        cap.record(Dir::FromStation, 100_000, &[0x80; 40]); // a beacon, RX/TX relative to the station
        cap.record(Dir::ToStation, 1_500_007, &[0x50; 30]);
        let b = buf.0.lock().unwrap().clone();
        // the same global header as the Ethernet capture, with link type 105 (0x69) instead of 1
        assert_eq!(
            &b[..24],
            &[
                0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 105,
                0, 0, 0
            ]
        );
        let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!((u(24), u(28), u(32), u(36)), (0, 100_000, 40, 40));
        assert_eq!(&b[40..80], &[0x80; 40]);
        assert_eq!((u(80), u(84), u(88), u(92)), (1, 500_007, 30, 30));
        assert_eq!(&b[96..], &[0x50; 30]);
        assert_eq!(cap.frames, 2);
        // and the Ethernet header is unchanged: still link type 1
        assert_eq!(&pcap_global_header()[20..24], &[1, 0, 0, 0]);
        assert_eq!(&air_pcap_global_header()[20..24], &[105, 0, 0, 0]);
    }

    #[test]
    fn air_ws_message_carries_type_6_and_the_station_relative_direction() {
        let m = ws_air_message(Dir::FromStation, 0x0102_0304_0506_0708, &[9, 8, 7]);
        assert_eq!(m, vec![6, 0, 8, 7, 6, 5, 4, 3, 2, 1, 9, 8, 7]);
        assert_eq!(ws_air_message(Dir::ToStation, 0, &[])[..2], [6, 1]);
        let mut cap = AirCapture::default();
        cap.record(Dir::ToStation, 0, &[1]);
        assert!(
            cap.take_live().is_empty(),
            "nothing kept until live is enabled"
        );
        cap.enable_live();
        for i in 0..LIVE_FRAMES_MAX + 3 {
            cap.record(Dir::ToStation, i as u64, &[0; 64]);
        }
        assert_eq!(cap.live_dropped, 3);
        assert_eq!(cap.take_live().len(), LIVE_FRAMES_MAX);
    }

    #[test]
    fn ws_message_layout_and_live_backlog_cap() {
        let m = ws_message(Dir::ToStation, 0x0102_0304_0506_0708, &[9, 8, 7]);
        assert_eq!(m, vec![5, 1, 8, 7, 6, 5, 4, 3, 2, 1, 9, 8, 7]);
        assert_eq!(ws_message(Dir::FromStation, 0, &[])[..2], [5, 0]);
        let mut cap = NetCapture::default();
        cap.record(Dir::FromStation, 0, &[1]);
        assert!(
            cap.take_live().is_empty(),
            "nothing kept until live is enabled"
        );
        cap.enable_live();
        for i in 0..LIVE_FRAMES_MAX + 3 {
            cap.record(Dir::FromStation, i as u64, &[0; 64]);
        }
        assert_eq!(cap.live_dropped, 3);
        let live = cap.take_live();
        assert_eq!(live.len(), LIVE_FRAMES_MAX);
        assert_eq!(live[1][2..10], 1u64.to_le_bytes());
        cap.record(Dir::ToStation, 9, &[0; 64]);
        assert_eq!(
            cap.take_live().len(),
            1,
            "room again after the queue was taken"
        );
    }
}
