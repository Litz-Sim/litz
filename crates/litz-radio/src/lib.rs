//! A deterministic packet medium shared by the radios on a board.
//!
//! The hub knows nothing about modulation. A part computes how long its own
//! frame occupies the air and hands that number in with the payload, so a
//! LoRa model and an NRF24 model can share one medium without either teaching
//! the hub its register map.
//!
//! Time is the board's monotonic tick. There is no wall clock and no
//! randomness: two runs that make the same calls in the same order schedule
//! the same packets and deliver them in the same order. A packet completes at
//! `sent_at_ns + airtime_ns`; only a packet that has completed can be read
//! from an inbox. Two frames on one network and channel whose airtimes
//! overlap destroy each other, which is what narrowband radios do without a
//! capture effect.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// The handle a board passes to every part. The parts share one hub, so two
/// radios on one board (or on two boards a runner put on the same hub) hear
/// each other.
pub type RadioHubHandle = Rc<RefCell<RadioHub>>;

/// A handle to one registered radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RadioId(pub u32);

/// What a radio is tuned to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RadioConfig {
    /// Logical network. Only radios naming the same network hear each other.
    pub network: String,
    /// Carrier channel. The unit is the part's own: MHz for a LoRa model,
    /// the 2.4 GHz channel number for an NRF24.
    pub channel: u16,
    /// Address a directed packet must name. An empty address receives only
    /// broadcasts.
    pub address: Vec<u8>,
    /// Transmit power, kept for readers that model range or RSSI later.
    pub power_dbm: i8,
    /// Reach in metres. Used only when both ends have a position.
    pub range_m: Option<f32>,
}

/// One frame as a receiver sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// Transmission order, assigned by the hub. Ties never happen.
    pub seq: u64,
    pub from: RadioId,
    /// `None` is a broadcast.
    pub to: Option<Vec<u8>>,
    pub bytes: Vec<u8>,
    pub sent_at_ns: u64,
    pub airtime_ns: u64,
    /// True when another frame on the same network and channel overlapped it.
    /// A collided packet is never delivered.
    pub collided: bool,
}

impl Packet {
    /// The tick at which the last bit has left the air.
    pub fn ends_at_ns(&self) -> u64 {
        self.sent_at_ns.saturating_add(self.airtime_ns)
    }
}

/// Counters a test can assert on without reaching into the schedule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RadioStats {
    /// Frames handed to [`RadioHub::transmit`].
    pub scheduled: u64,
    /// Frames handed to a receiver by [`RadioHub::inbox`], once per receiver.
    pub delivered: u64,
    /// Frames destroyed by an overlap. A frame counts once, however many
    /// others it collided with.
    pub collided: u64,
}

#[derive(Clone, Debug)]
struct Radio {
    config: RadioConfig,
    /// `[x, y]` in metres, or absent when geometry is not modelled.
    position: Option<[f32; 2]>,
}

#[derive(Clone, Debug)]
struct Scheduled {
    packet: Packet,
    network: String,
    channel: u16,
    sender_position: Option<[f32; 2]>,
    /// Radios that have already read this packet. A broadcast is still in the
    /// schedule for the receivers that have not asked yet.
    delivered_to: Vec<RadioId>,
}

/// The medium itself.
#[derive(Debug, Default)]
pub struct RadioHub {
    radios: Vec<Option<Radio>>,
    pending: Vec<Scheduled>,
    next_seq: u64,
    stats: RadioStats,
}

impl RadioHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// A hub wrapped in the handle the board and the parts share.
    pub fn shared() -> RadioHubHandle {
        Rc::new(RefCell::new(Self::new()))
    }

    /// Add a radio and return its handle.
    pub fn register(&mut self, config: RadioConfig) -> RadioId {
        let id = RadioId(self.radios.len() as u32);
        self.radios.push(Some(Radio {
            config,
            position: None,
        }));
        id
    }

    pub fn config(&self, id: RadioId) -> Option<&RadioConfig> {
        self.radios.get(id.0 as usize)?.as_ref().map(|r| &r.config)
    }

    /// Put a radio at a point so range can be enforced. Both ends need a
    /// position before distance matters.
    pub fn set_position(&mut self, id: RadioId, position: [f32; 2]) {
        if let Some(Some(radio)) = self.radios.get_mut(id.0 as usize) {
            radio.position = Some(position);
        }
    }

    pub fn position(&self, id: RadioId) -> Option<[f32; 2]> {
        self.radios.get(id.0 as usize)?.as_ref()?.position
    }

    /// Move a radio to another logical network, as a host call naming a new
    /// one does. Frames already on the air keep the network they were sent on.
    pub fn set_network(&mut self, id: RadioId, network: String) {
        if let Some(Some(radio)) = self.radios.get_mut(id.0 as usize) {
            radio.config.network = network;
        }
    }

    /// Retune a radio, as a driver writing its channel register does.
    pub fn set_channel(&mut self, id: RadioId, channel: u16) {
        if let Some(Some(radio)) = self.radios.get_mut(id.0 as usize) {
            radio.config.channel = channel;
        }
    }

    /// Readdress a radio, as a driver writing its address register does.
    pub fn set_address(&mut self, id: RadioId, address: Vec<u8>) {
        if let Some(Some(radio)) = self.radios.get_mut(id.0 as usize) {
            radio.config.address = address;
        }
    }

    /// Start a transmission at `now_ns`.
    ///
    /// `airtime_ns` is the caller's modulation result. `to = None` broadcasts.
    /// A frame that overlaps another frame on the same network and channel is
    /// marked collided and neither is delivered: LoRa's capture effect is out
    /// of scope, so an overlap is always destructive here.
    pub fn transmit(
        &mut self,
        id: RadioId,
        now_ns: u64,
        payload: &[u8],
        airtime_ns: u64,
        to: Option<&[u8]>,
    ) -> Option<Packet> {
        let radio = self.radios.get(id.0 as usize)?.as_ref()?;
        let network = radio.config.network.clone();
        let channel = radio.config.channel;
        let sender_position = radio.position;
        let mut packet = Packet {
            seq: self.next_seq,
            from: id,
            to: to.map(<[u8]>::to_vec),
            bytes: payload.to_vec(),
            sent_at_ns: now_ns,
            airtime_ns,
            collided: false,
        };
        self.next_seq += 1;
        self.stats.scheduled += 1;

        let start = now_ns;
        let end = packet.ends_at_ns();
        for scheduled in &mut self.pending {
            if scheduled.network != network || scheduled.channel != channel {
                continue;
            }
            let other_start = scheduled.packet.sent_at_ns;
            let other_end = scheduled.packet.ends_at_ns();
            // Half-open intervals: a frame that starts exactly as another
            // finishes does not overlap it.
            let overlaps = start < other_end && other_start < end;
            if !overlaps {
                continue;
            }
            if !scheduled.packet.collided {
                scheduled.packet.collided = true;
                self.stats.collided += 1;
            }
            if !packet.collided {
                packet.collided = true;
                self.stats.collided += 1;
            }
        }

        self.pending.push(Scheduled {
            packet: packet.clone(),
            network,
            channel,
            sender_position,
            delivered_to: Vec::new(),
        });
        Some(packet)
    }

    /// Frames that have finished by `now_ns` and are addressed to `id`.
    ///
    /// The result is sorted by transmission sequence, which is unique and is
    /// the deterministic order a test sees. Reading a packet marks it read
    /// for this radio; a broadcast remains available to the other receivers.
    pub fn inbox(&mut self, id: RadioId, now_ns: u64) -> Vec<Packet> {
        if self.radios.get(id.0 as usize).is_none_or(Option::is_none) {
            return Vec::new();
        }
        let radios = &self.radios;
        let mut pending = std::mem::take(&mut self.pending);
        let mut kept = Vec::with_capacity(pending.len());
        let mut out = Vec::new();
        for mut scheduled in pending.drain(..) {
            if scheduled.packet.ends_at_ns() > now_ns {
                kept.push(scheduled);
                continue;
            }
            if scheduled.packet.collided {
                // Destroyed on the air; nothing to hand out.
                continue;
            }
            let reaches = packet_reaches(radios, &scheduled, id);
            if reaches && !scheduled.delivered_to.contains(&id) {
                scheduled.delivered_to.push(id);
                self.stats.delivered += 1;
                out.push(scheduled.packet.clone());
            }
            // Keep the frame while another registered radio still expects it.
            let waiting = radios.iter().enumerate().any(|(index, radio)| {
                if radio.is_none() {
                    return false;
                }
                let other = RadioId(index as u32);
                other != id
                    && !scheduled.delivered_to.contains(&other)
                    && packet_reaches(radios, &scheduled, other)
            });
            if waiting {
                kept.push(scheduled);
            }
        }
        self.pending = kept;
        out.sort_by_key(|packet| (packet.seq, packet.sent_at_ns, packet.from.0));
        out
    }

    /// The earliest tick at which a frame addressed to `id` finishes, for a
    /// part that asks the board when to advance next. `None` means nothing is
    /// on its way.
    pub fn next_delivery(&self, id: RadioId) -> Option<u64> {
        self.radios.get(id.0 as usize)?.as_ref()?;
        self.pending
            .iter()
            .filter(|scheduled| {
                !scheduled.packet.collided
                    && !scheduled.delivered_to.contains(&id)
                    && packet_reaches(&self.radios, scheduled, id)
            })
            .map(|scheduled| scheduled.packet.ends_at_ns())
            .min()
    }

    /// How many frames are still in the medium (including finished frames a
    /// receiver has not read yet).
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn stats(&self) -> RadioStats {
        self.stats
    }

    /// Frames destroyed by a collision.
    pub fn collisions(&self) -> u64 {
        self.stats.collided
    }
}

/// Whether `scheduled` reaches `id`, including network, channel, address and
/// the optional range rule. The sender never receives its own frame.
fn packet_reaches(radios: &[Option<Radio>], scheduled: &Scheduled, id: RadioId) -> bool {
    if scheduled.packet.from == id {
        return false;
    }
    let Some(Some(receiver)) = radios.get(id.0 as usize) else {
        return false;
    };
    if receiver.config.network != scheduled.network || receiver.config.channel != scheduled.channel
    {
        return false;
    }
    if let Some(to) = &scheduled.packet.to
        && receiver.config.address != *to
    {
        return false;
    }
    // Distance is meaningful only when both ends declare a position. A range
    // on either end is the tighter of the two; no range at all means the
    // geometry is ignored rather than guessed.
    let sender_range = radios
        .get(scheduled.packet.from.0 as usize)
        .and_then(Option::as_ref)
        .and_then(|radio| radio.config.range_m);
    let range = match (receiver.config.range_m, sender_range) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if let (Some(sender), Some(receiver_position), Some(range)) =
        (scheduled.sender_position, receiver.position, range)
        && radios_distance(sender, receiver_position) > range
    {
        return false;
    }
    true
}

fn radios_distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt()
}

/// A queue a part can use for its own receive FIFO without depending on std
/// collections directly.
pub type PacketQueue = VecDeque<Packet>;

#[cfg(test)]
mod tests {
    use super::*;

    fn config(network: &str, channel: u16, address: &[u8]) -> RadioConfig {
        RadioConfig {
            network: network.into(),
            channel,
            address: address.to_vec(),
            power_dbm: 14,
            range_m: None,
        }
    }

    #[test]
    fn radios_on_one_network_and_channel_hear_each_other() {
        let mut hub = RadioHub::new();
        let a = hub.register(config("lora", 868, b""));
        let b = hub.register(config("lora", 868, b""));
        hub.transmit(a, 0, b"hello", 1_000, None);
        assert!(hub.inbox(b, 999).is_empty(), "still in the air");
        let packets = hub.inbox(b, 1_000);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].bytes, b"hello");
        assert_eq!(packets[0].from, a);
        assert!(hub.inbox(a, 1_000).is_empty(), "the sender is excluded");
    }

    #[test]
    fn a_different_channel_does_not_hear_the_packet() {
        let mut hub = RadioHub::new();
        let a = hub.register(config("lora", 868, b""));
        let b = hub.register(config("lora", 869, b""));
        hub.transmit(a, 0, b"hello", 1_000, None);
        assert!(hub.inbox(b, 10_000).is_empty());
    }

    #[test]
    fn a_different_address_does_not_hear_the_packet() {
        let mut hub = RadioHub::new();
        let a = hub.register(config("nrf24", 76, &[1, 2, 3, 4, 5]));
        let b = hub.register(config("nrf24", 76, &[6, 7, 8, 9, 10]));
        hub.transmit(a, 0, b"data", 1_000, Some(&[6, 7, 8, 9, 10]));
        assert!(
            hub.inbox(a, 10_000).is_empty(),
            "the sender is excluded even when addressed to itself"
        );
        assert_eq!(hub.inbox(b, 10_000).len(), 1);

        // A directed packet to a third address reaches nobody in this pair.
        hub.transmit(a, 20_000, b"data", 1_000, Some(&[9, 9, 9, 9, 9]));
        assert!(hub.inbox(b, 30_000).is_empty());
    }

    #[test]
    fn a_broadcast_reaches_every_matching_receiver() {
        let mut hub = RadioHub::new();
        let a = hub.register(config("lora", 868, b"aa"));
        let b = hub.register(config("lora", 868, b"bb"));
        let c = hub.register(config("lora", 868, b"cc"));
        hub.transmit(a, 0, b"all", 1_000, None);
        assert_eq!(hub.inbox(b, 1_000).len(), 1);
        assert_eq!(hub.inbox(c, 1_000).len(), 1);
        // Reading does not consume the copy the other receiver already read,
        // and a second read of the same inbox is empty.
        assert!(hub.inbox(b, 1_000).is_empty());
    }

    #[test]
    fn airtime_decides_when_a_packet_arrives_and_in_what_order() {
        let mut hub = RadioHub::new();
        let a = hub.register(config("lora", 868, b""));
        let b = hub.register(config("lora", 868, b""));
        hub.transmit(a, 0, b"first", 2_000, None);
        hub.transmit(a, 5_000, b"second", 1_000, None);

        assert!(hub.inbox(b, 1_999).is_empty());
        let packets = hub.inbox(b, 6_000);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].bytes, b"first");
        assert_eq!(packets[1].bytes, b"second");
        assert!(packets[0].seq < packets[1].seq);
    }

    #[test]
    fn overlapping_frames_destroy_each_other() {
        let mut hub = RadioHub::new();
        let a = hub.register(config("lora", 868, b""));
        let b = hub.register(config("lora", 868, b""));
        let c = hub.register(config("lora", 868, b""));
        hub.transmit(a, 1_000, b"a", 10_000, None);
        hub.transmit(b, 5_000, b"b", 10_000, None);
        assert_eq!(hub.collisions(), 2);
        assert!(hub.inbox(c, 100_000).is_empty());
        assert_eq!(hub.stats().scheduled, 2);
        assert_eq!(hub.stats().delivered, 0);

        // A frame that begins exactly at the other's end is not an overlap.
        hub.transmit(a, 200_000, b"a", 10_000, None);
        hub.transmit(b, 210_000, b"b", 10_000, None);
        assert_eq!(hub.collisions(), 2);
    }

    #[test]
    fn replaying_the_same_calls_produces_the_same_packets() {
        fn run() -> Vec<Packet> {
            let mut hub = RadioHub::new();
            let a = hub.register(config("lora", 868, b""));
            let b = hub.register(config("lora", 868, b""));
            hub.transmit(a, 0, b"one", 1_000, None);
            hub.transmit(a, 500, b"two", 1_000, None);
            hub.transmit(a, 10_000, b"three", 1_000, None);
            hub.inbox(b, 20_000)
        }
        assert_eq!(run(), run());
    }

    #[test]
    fn range_drops_a_packet_only_when_both_ends_have_a_position() {
        let mut hub = RadioHub::new();
        let mut near = config("lora", 868, b"");
        near.range_m = Some(100.0);
        let a = hub.register(near);
        let b = hub.register(config("lora", 868, b""));
        hub.set_position(a, [0.0, 0.0]);
        hub.set_position(b, [50.0, 0.0]);
        hub.transmit(a, 0, b"near", 1_000, None);
        assert_eq!(hub.inbox(b, 10_000).len(), 1);

        hub.set_position(b, [500.0, 0.0]);
        hub.transmit(a, 20_000, b"far", 1_000, None);
        assert!(hub.inbox(b, 30_000).is_empty());
    }

    #[test]
    fn a_packet_survives_until_every_receiver_has_read_it() {
        let mut hub = RadioHub::new();
        let a = hub.register(config("lora", 868, b""));
        let b = hub.register(config("lora", 868, b""));
        let c = hub.register(config("lora", 868, b""));
        hub.transmit(a, 0, b"shared", 1_000, None);
        assert_eq!(hub.inbox(b, 10_000).len(), 1);
        // c registered before the frame; it can still read it.
        assert_eq!(hub.inbox(c, 10_000).len(), 1);
        assert_eq!(hub.pending(), 0);
    }
}
