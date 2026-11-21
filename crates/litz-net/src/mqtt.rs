//! A deterministic MQTT 3.1.1 / 5.0 broker behind the router.
//!
//! IoT firmware almost always speaks MQTT, and a simulator that only offers a
//! TCP echo cannot run the sketch a tutorial ships with. The alternative —
//! reaching a real broker on the host — makes the test depend on the internet
//! and on a wall clock. This broker is part of the simulated network instead:
//! connect, subscribe, publish, retain, will and ping all answer from bytes the
//! circuit configured, in order, with no clock and no randomness.
//!
//! It is a broker, not a client library. A packet split across TCP segments is
//! reassembled by [`Broker::feed`], and every answer a client reads comes back
//! through the same flow. Quality of service is honoured at the level the
//! packets describe: a QoS 0 publication is delivered once, a QoS 1 one is
//! acknowledged with PUBACK, and a retaining publication replaces the retained
//! message for its topic.
//!
//! Session state follows the protocol rather than the connection: a client
//! that connects with `clean session = false` keeps its subscriptions and its
//! QoS 1/2 queue across a disconnect, a will is published when the connection
//! drops without DISCONNECT, and `$share/<group>/<filter>` delivers one copy
//! per group. MQTT 5 property blocks are parsed and skipped on input and
//! emitted empty on output; the properties themselves do not change routing.
//!
//! What is not modelled: shared subscription dispatch strategies beyond
//! "lowest client id", message expiry, and request/response topics.

use std::collections::BTreeMap;

/// The largest control packet the broker accepts. A firmware that announces a
/// larger one is refused rather than allowed to grow the buffer without bound.
pub const MAX_PACKET: usize = 256 * 1024;

/// The longest topic or filter, in bytes, the broker keeps.
pub const MAX_TOPIC: usize = 1024;

/// How many waiting QoS 1/2 messages an offline session keeps.
pub const MAX_QUEUED: usize = 32;

/// What a client's bytes did to its connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The packet buffer ends mid-packet; more bytes will finish it.
    More,
    /// The client sent DISCONNECT or a packet the broker closes on.
    Closed,
}

/// A will the broker publishes when the connection drops without DISCONNECT.
#[derive(Clone, Debug)]
struct Will {
    topic: String,
    payload: Vec<u8>,
    qos: u8,
    retain: bool,
}

/// One MQTT session. It outlives the TCP flow when the client asks it to.
#[derive(Debug)]
struct Session {
    level: u8,
    clean: bool,
    /// Subscriptions, with the granted QoS; a shared filter keeps its
    /// `$share/<group>/` prefix.
    filters: Vec<(String, u8)>,
    will: Option<Will>,
    online: bool,
    /// PUBLISH packets already encoded for this session's protocol level,
    /// waiting for a reconnect.
    queued: Vec<Vec<u8>>,
    next_packet: u16,
}

impl Session {
    fn new() -> Self {
        Self {
            level: 4,
            clean: true,
            filters: Vec::new(),
            will: None,
            online: false,
            queued: Vec::new(),
            next_packet: 0,
        }
    }

    fn packet_id(&mut self) -> u16 {
        self.next_packet = self.next_packet.wrapping_add(1).max(1);
        self.next_packet
    }
}

/// A retained publication, handed to a subscriber as soon as it matches.
struct Retained {
    payload: Vec<u8>,
    qos: u8,
}

/// The broker's state: sessions, the flow each is connected on, and what it
/// retained.
#[derive(Default)]
pub struct Broker {
    sessions: BTreeMap<String, Session>,
    /// Flow id → session key.
    clients: BTreeMap<u32, String>,
    retained: BTreeMap<String, Retained>,
}

impl Broker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget a flow and return the frames its will publishes.
    ///
    /// A dropped link is the case a will exists for, so it is routed here and
    /// the caller writes each `(flow, packet)` into that flow. A graceful
    /// DISCONNECT cleared the will before this is called.
    pub fn remove(&mut self, client: u32) -> Vec<(u32, Vec<u8>)> {
        let Some(key) = self.clients.remove(&client) else {
            return Vec::new();
        };
        let will = {
            let Some(session) = self.sessions.get_mut(&key) else {
                return Vec::new();
            };
            if !session.online {
                None
            } else {
                session.online = false;
                session.will.take()
            }
        };
        let mut deliveries = Vec::new();
        if let Some(will) = will {
            if will.retain {
                self.retained.insert(
                    will.topic.clone(),
                    Retained {
                        payload: will.payload.clone(),
                        qos: will.qos,
                    },
                );
            }
            self.route(&will.topic, &will.payload, will.qos, &mut deliveries);
        }
        if self
            .sessions
            .get(&key)
            .is_some_and(|session| session.clean || session.filters.is_empty())
        {
            self.sessions.remove(&key);
        }
        deliveries
    }

    /// How many sessions the broker holds, for a report.
    pub fn clients(&self) -> usize {
        self.sessions
            .values()
            .filter(|session| session.online)
            .count()
    }

    /// Consume whatever complete packets `input` holds.
    ///
    /// Direct answers (CONNACK, SUBACK, PUBACK, PINGRESP) are appended to
    /// `reply`. Publications for other online clients are appended to
    /// `deliveries` as `(flow, bytes)`; the caller writes each one into that
    /// flow. A publication for an offline non-clean session is queued instead.
    pub fn feed(
        &mut self,
        client: u32,
        input: &mut Vec<u8>,
        reply: &mut Vec<u8>,
        deliveries: &mut Vec<(u32, Vec<u8>)>,
    ) -> Event {
        loop {
            if input.is_empty() {
                return Event::More;
            }
            let Some((header, length_at, remaining)) = fixed_header(input) else {
                return Event::More;
            };
            if remaining > MAX_PACKET {
                return Event::Closed;
            }
            let total = length_at + remaining;
            if input.len() < total {
                return Event::More;
            }
            let packet: Vec<u8> = input.drain(..total).collect();
            let body = &packet[length_at..];
            let kind = header >> 4;
            let flags = header & 0x0f;
            let event = self.packet(client, kind, flags, body, reply, deliveries);
            if event == Event::Closed {
                return Event::Closed;
            }
        }
    }

    fn packet(
        &mut self,
        client: u32,
        kind: u8,
        flags: u8,
        body: &[u8],
        reply: &mut Vec<u8>,
        deliveries: &mut Vec<(u32, Vec<u8>)>,
    ) -> Event {
        match kind {
            // CONNECT.
            1 => self.connect(client, body, reply, deliveries),
            // PUBLISH: route it, acknowledge it if it asked, retain it if it
            // asked. The DUP flag does not change a deterministic answer.
            3 => {
                let retained = flags & 0x01 != 0;
                let qos = (flags >> 1) & 0x03;
                if qos > 2 {
                    return Event::Closed;
                }
                let Some((topic, mut at)) = read_string(body, 0) else {
                    return Event::Closed;
                };
                if topic.is_empty()
                    || topic.len() > MAX_TOPIC
                    || topic.contains('+')
                    || topic.contains('#')
                {
                    return Event::Closed;
                }
                let packet_id = if qos == 0 {
                    None
                } else {
                    let Some((id, next)) = read_u16(body, at) else {
                        return Event::Closed;
                    };
                    at = next;
                    Some(id)
                };
                let level = self.level_of(client);
                if level >= 5 {
                    let Some(next) = skip_properties(body, at) else {
                        return Event::Closed;
                    };
                    at = next;
                }
                let payload = body.get(at..).unwrap_or_default().to_vec();
                if retained {
                    if payload.is_empty() {
                        self.retained.remove(&topic);
                    } else {
                        self.retained.insert(
                            topic.clone(),
                            Retained {
                                payload: payload.clone(),
                                qos,
                            },
                        );
                    }
                }
                match qos {
                    0 => {}
                    1 => acknowledge(reply, 0x40, packet_id),
                    _ => acknowledge(reply, 0x50, packet_id),
                }
                self.route(&topic, &payload, qos, deliveries);
                Event::More
            }
            // PUBACK, PUBREC, PUBCOMP: the broker acknowledges deliveries, but
            // a deterministic run does not wait for the round trip.
            4 | 5 | 7 => Event::More,
            // PUBREL: answer with PUBCOMP, the other half of QoS 2.
            6 => {
                if let Some(id) = body
                    .get(0..2)
                    .and_then(|bytes| bytes.try_into().ok())
                    .map(u16::from_be_bytes)
                {
                    acknowledge(reply, 0x70, Some(id));
                }
                Event::More
            }
            // SUBSCRIBE: grant each filter the QoS asked for, then replay the
            // retained messages that match.
            8 => self.subscribe(client, body, reply, deliveries),
            // UNSUBSCRIBE: drop the filters and acknowledge.
            10 => {
                let Some((id, mut at)) = read_u16(body, 0) else {
                    return Event::Closed;
                };
                let level = self.level_of(client);
                if level >= 5 {
                    let Some(next) = skip_properties(body, at) else {
                        return Event::Closed;
                    };
                    at = next;
                }
                let mut removed = Vec::new();
                while at < body.len() {
                    let Some((filter, used)) = read_string(body, at) else {
                        return Event::Closed;
                    };
                    at = used;
                    removed.push(filter);
                }
                if let Some(session) = self.session_of(client) {
                    session
                        .filters
                        .retain(|(held, _)| !removed.iter().any(|gone| gone == held));
                }
                if level >= 5 {
                    reply.extend_from_slice(&[0xb0, 0x04]);
                    reply.extend_from_slice(&id.to_be_bytes());
                    reply.push(0);
                } else {
                    reply.extend_from_slice(&[0xb0, 0x02]);
                    reply.extend_from_slice(&id.to_be_bytes());
                }
                Event::More
            }
            // PINGREQ
            12 => {
                reply.extend_from_slice(&[0xd0, 0x00]);
                Event::More
            }
            // DISCONNECT: graceful, so no will.
            14 => {
                if let Some(session) = self.session_of(client) {
                    session.online = false;
                    session.will = None;
                }
                Event::Closed
            }
            // A packet type no client sends to a broker.
            _ => Event::Closed,
        }
    }

    fn connect(
        &mut self,
        client: u32,
        body: &[u8],
        reply: &mut Vec<u8>,
        _deliveries: &mut Vec<(u32, Vec<u8>)>,
    ) -> Event {
        let Some((name, mut at)) = read_string(body, 0) else {
            return Event::Closed;
        };
        let Some(&level) = body.get(at) else {
            return Event::Closed;
        };
        at += 1;
        let supported = matches!((name.as_str(), level), ("MQTT", 4 | 5) | ("MQIsdp", 3));
        if !supported {
            return Event::Closed;
        }
        let Some(&flags) = body.get(at) else {
            return Event::Closed;
        };
        at += 1;
        let Some(next) = skip(body, at, 2) else {
            return Event::Closed;
        };
        at = next;
        if level >= 5 {
            let Some(next) = skip_properties(body, at) else {
                return Event::Closed;
            };
            at = next;
        }
        let Some((client_id, next)) = read_string(body, at) else {
            return Event::Closed;
        };
        at = next;

        let clean = flags & 0x02 != 0;
        let will = if flags & 0x04 != 0 {
            let qos = (flags >> 3) & 0x03;
            if qos > 2 {
                return Event::Closed;
            }
            let mut at = at;
            if level >= 5 {
                let Some(next) = skip_properties(body, at) else {
                    return Event::Closed;
                };
                at = next;
            }
            let Some((topic, next)) = read_string(body, at) else {
                return Event::Closed;
            };
            let Some((payload, _next)) = read_bytes(body, next) else {
                return Event::Closed;
            };
            Some(Will {
                topic,
                payload,
                qos,
                retain: flags & 0x20 != 0,
            })
        } else {
            None
        };
        // Username and password follow, and are accepted without checking:
        // the circuit has no account table. They still have to parse.
        if flags & 0x80 != 0 {
            let Some(next) = skip_string(body, at) else {
                return Event::Closed;
            };
            at = next;
        }
        if flags & 0x40 != 0 {
            let Some(next) = skip_string(body, at) else {
                return Event::Closed;
            };
            at = next;
        }
        let _ = at;

        let key = if client_id.is_empty() {
            if !clean {
                return Event::Closed;
            }
            format!("anon-{client}")
        } else {
            client_id
        };
        let existed = self.sessions.contains_key(&key);
        if clean {
            self.sessions.remove(&key);
        }
        let session = self
            .sessions
            .entry(key.clone())
            .or_insert_with(Session::new);
        let session_present = existed && !clean;
        session.level = level;
        session.clean = clean;
        session.will = will;
        session.online = true;
        self.clients.insert(client, key.clone());

        if level >= 5 {
            reply.extend_from_slice(&[0x20, 0x03, u8::from(session_present), 0x00, 0x00]);
        } else {
            reply.extend_from_slice(&[0x20, 0x02, u8::from(session_present), 0x00]);
        }
        // A resumed session receives what it missed.
        if let Some(session) = self.sessions.get_mut(&key) {
            for packet in std::mem::take(&mut session.queued) {
                reply.extend_from_slice(&packet);
            }
        }
        Event::More
    }

    fn subscribe(
        &mut self,
        client: u32,
        body: &[u8],
        reply: &mut Vec<u8>,
        deliveries: &mut Vec<(u32, Vec<u8>)>,
    ) -> Event {
        let Some((id, mut at)) = read_u16(body, 0) else {
            return Event::Closed;
        };
        let level = self.level_of(client);
        if level >= 5 {
            let Some(next) = skip_properties(body, at) else {
                return Event::Closed;
            };
            at = next;
        }
        let mut granted = Vec::new();
        let mut added = Vec::new();
        while at < body.len() {
            let Some((filter, used)) = read_string(body, at) else {
                return Event::Closed;
            };
            at = used;
            let Some(&qos) = body.get(at) else {
                return Event::Closed;
            };
            if qos > 2 || filter.is_empty() || filter.len() > MAX_TOPIC {
                return Event::Closed;
            }
            if let Some(shared) = filter.strip_prefix("$share/") {
                let Some((_, rest)) = shared.split_once('/') else {
                    return Event::Closed;
                };
                if rest.is_empty() {
                    return Event::Closed;
                }
            }
            at += 1;
            granted.push(qos);
            added.push(filter);
        }
        if added.is_empty() {
            return Event::Closed;
        }
        let Some(session) = self.session_of(client) else {
            return Event::Closed;
        };
        for (filter, qos) in added.iter().zip(&granted) {
            session.filters.retain(|(held, _)| held != filter);
            session.filters.push((filter.clone(), *qos));
        }
        if level >= 5 {
            reply.extend_from_slice(&[0x90, (3 + granted.len()) as u8]);
            reply.extend_from_slice(&id.to_be_bytes());
            reply.push(0);
        } else {
            reply.extend_from_slice(&[0x90, (2 + granted.len()) as u8]);
            reply.extend_from_slice(&id.to_be_bytes());
        }
        reply.extend_from_slice(&granted);

        for (filter, qos) in added.iter().zip(&granted) {
            let plain = shared_filter(filter).unwrap_or(filter.as_str());
            let matches: Vec<(String, Vec<u8>, u8)> = self
                .retained
                .iter()
                .filter(|(topic, _)| matches(plain, topic))
                .map(|(topic, held)| (topic.clone(), held.payload.clone(), held.qos))
                .collect();
            for (topic, payload, held) in matches {
                let effective = (*qos).min(held);
                let key = self.clients.get(&client).cloned().unwrap_or_default();
                let packet_id = if effective > 0 {
                    self.sessions
                        .get_mut(&key)
                        .map(Session::packet_id)
                        .unwrap_or(1)
                } else {
                    0
                };
                let packet = publish_packet(&topic, &payload, effective, packet_id, true, level);
                deliveries.push((client, packet));
            }
        }
        Event::More
    }

    /// Deliver one publication to every subscriber that matches.
    ///
    /// A shared subscription (`$share/<group>/<filter>`) receives one copy per
    /// group, given to the lowest client id, so the choice is stable across
    /// runs. A client with several matching filters receives one copy.
    fn route(
        &mut self,
        topic: &str,
        payload: &[u8],
        qos: u8,
        deliveries: &mut Vec<(u32, Vec<u8>)>,
    ) {
        let mut plain: BTreeMap<String, u8> = BTreeMap::new();
        let mut shared: BTreeMap<(String, String), (String, u8)> = BTreeMap::new();
        for (key, session) in &self.sessions {
            for (filter, granted) in &session.filters {
                match shared_filter(filter) {
                    Some(plain_filter) => {
                        if matches(plain_filter, topic) {
                            let group = shared_group(filter).unwrap_or_default();
                            let entry = shared
                                .entry((group, plain_filter.to_string()))
                                .or_insert_with(|| (key.clone(), *granted));
                            if key < &entry.0 {
                                *entry = (key.clone(), *granted);
                            }
                        }
                    }
                    None => {
                        if matches(filter, topic) {
                            plain
                                .entry(key.clone())
                                .and_modify(|held| *held = (*held).max(*granted))
                                .or_insert(*granted);
                        }
                    }
                }
            }
        }
        let targets = plain
            .into_iter()
            .chain(shared.into_values())
            .map(|(key, granted)| (key, qos.min(granted)))
            .collect::<Vec<_>>();
        let flows: BTreeMap<String, u32> = self
            .clients
            .iter()
            .map(|(flow, key)| (key.clone(), *flow))
            .collect();
        for (key, effective) in targets {
            let Some(session) = self.sessions.get_mut(&key) else {
                continue;
            };
            let packet_id = if effective > 0 {
                session.packet_id()
            } else {
                0
            };
            let packet = publish_packet(topic, payload, effective, packet_id, false, session.level);
            if let Some(flow) = flows.get(&key)
                && session.online
            {
                deliveries.push((*flow, packet));
            } else if effective > 0 {
                if session.queued.len() >= MAX_QUEUED {
                    session.queued.remove(0);
                }
                session.queued.push(packet);
            }
        }
    }

    fn session_of(&mut self, client: u32) -> Option<&mut Session> {
        let key = self.clients.get(&client)?.clone();
        self.sessions.get_mut(&key)
    }

    fn level_of(&self, client: u32) -> u8 {
        self.clients
            .get(&client)
            .and_then(|key| self.sessions.get(key))
            .map(|session| session.level)
            .unwrap_or(4)
    }
}

/// A PUBLISH packet with `qos` and, when non-zero, a packet identifier.
fn publish_packet(
    topic: &str,
    payload: &[u8],
    qos: u8,
    packet_id: u16,
    retain: bool,
    level: u8,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(2 + topic.len() + 2 + 1 + payload.len());
    body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    if qos > 0 {
        body.extend_from_slice(&packet_id.to_be_bytes());
    }
    if level >= 5 {
        // An empty property block: the broker does not add properties.
        body.push(0);
    }
    body.extend_from_slice(payload);
    let mut packet = vec![0x30 | (qos << 1) | u8::from(retain)];
    remaining_length(body.len(), &mut packet);
    packet.extend_from_slice(&body);
    packet
}

fn acknowledge(reply: &mut Vec<u8>, kind: u8, id: Option<u16>) {
    reply.extend_from_slice(&[kind, 0x02]);
    reply.extend_from_slice(&id.unwrap_or(1).to_be_bytes());
}

/// The fixed header of the first packet in `input`: its first byte, the offset
/// where the body starts, and the remaining length.
fn fixed_header(input: &[u8]) -> Option<(u8, usize, usize)> {
    let header = *input.first()?;
    let mut value = 0usize;
    let mut multiplier = 1usize;
    for (index, &byte) in input.iter().enumerate().skip(1).take(4) {
        value += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            return Some((header, index + 1, value));
        }
        multiplier *= 128;
    }
    None
}

fn remaining_length(length: usize, out: &mut Vec<u8>) {
    let mut value = length;
    loop {
        let mut byte = (value % 128) as u8;
        value /= 128;
        if value > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// A length-prefixed UTF-8 string starting at `at`, with the offset after it.
fn read_string(body: &[u8], at: usize) -> Option<(String, usize)> {
    let (bytes, end) = read_bytes(body, at)?;
    Some((String::from_utf8(bytes).ok()?, end))
}

/// A length-prefixed byte string starting at `at`, with the offset after it.
fn read_bytes(body: &[u8], at: usize) -> Option<(Vec<u8>, usize)> {
    let (length, start) = read_u16(body, at)?;
    let end = start.checked_add(usize::from(length))?;
    Some((body.get(start..end)?.to_vec(), end))
}

fn read_u16(body: &[u8], at: usize) -> Option<(u16, usize)> {
    let bytes = body.get(at..at + 2)?;
    Some((u16::from_be_bytes([bytes[0], bytes[1]]), at + 2))
}

fn skip(body: &[u8], at: usize, count: usize) -> Option<usize> {
    (body.len() >= at.checked_add(count)?).then_some(at + count)
}

fn skip_string(body: &[u8], at: usize) -> Option<usize> {
    read_string(body, at).map(|(_, end)| end)
}

/// Skip an MQTT 5 property block: a variable byte length followed by that many
/// bytes. The broker does not act on properties, but it must not treat them as
/// payload either.
fn skip_properties(body: &[u8], at: usize) -> Option<usize> {
    let (length, start) = varint(body, at)?;
    skip(body, start, length)
}

fn varint(body: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut value = 0usize;
    let mut multiplier = 1usize;
    for (index, &byte) in body.iter().enumerate().skip(at).take(4) {
        value += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
        multiplier *= 128;
    }
    None
}

/// The filter part of a shared subscription, or none for a plain filter.
pub fn shared_filter(filter: &str) -> Option<&str> {
    let rest = filter.strip_prefix("$share/")?;
    rest.split_once('/').map(|(_, filter)| filter)
}

/// The group name of a shared subscription.
pub fn shared_group(filter: &str) -> Option<String> {
    let rest = filter.strip_prefix("$share/")?;
    rest.split_once('/').map(|(group, _)| group.to_string())
}

/// Whether one topic filter matches a topic: `+` is exactly one level, `#` is
/// the rest of the tree and must be last (MQTT 3.1.1, §4.7).
pub fn matches(filter: &str, topic: &str) -> bool {
    let filter_levels: Vec<&str> = filter.split('/').collect();
    let topic_levels: Vec<&str> = topic.split('/').collect();
    let mut index = 0;
    for (position, level) in filter_levels.iter().enumerate() {
        if *level == "#" {
            return true;
        }
        let Some(topic_level) = topic_levels.get(position) else {
            return false;
        };
        if *level != "+" && level != topic_level {
            return false;
        }
        index = position + 1;
    }
    index == topic_levels.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect_packet(client_id: &str, clean: bool, level: u8) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 0x04]);
        body.extend_from_slice(b"MQTT");
        body.push(level);
        body.push(if clean { 0x02 } else { 0x00 });
        body.extend_from_slice(&60u16.to_be_bytes());
        if level >= 5 {
            body.push(0);
        }
        body.extend_from_slice(&(client_id.len() as u16).to_be_bytes());
        body.extend_from_slice(client_id.as_bytes());
        let mut packet = vec![0x10];
        remaining_length(body.len(), &mut packet);
        packet.extend_from_slice(&body);
        packet
    }

    fn connect() -> Vec<u8> {
        connect_packet("", true, 4)
    }

    fn subscribe(id: u16, filter: &str, qos: u8) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&(filter.len() as u16).to_be_bytes());
        body.extend_from_slice(filter.as_bytes());
        body.push(qos);
        let mut packet = vec![0x82];
        remaining_length(body.len(), &mut packet);
        packet.extend_from_slice(&body);
        packet
    }

    fn publish(topic: &str, payload: &[u8], qos: u8, retain: bool) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
        body.extend_from_slice(topic.as_bytes());
        if qos > 0 {
            body.extend_from_slice(&7u16.to_be_bytes());
        }
        body.extend_from_slice(payload);
        let mut packet = vec![0x30 | (qos << 1) | u8::from(retain)];
        remaining_length(body.len(), &mut packet);
        packet.extend_from_slice(&body);
        packet
    }

    fn run(broker: &mut Broker, client: u32, packet: Vec<u8>) -> (Vec<u8>, Vec<(u32, Vec<u8>)>) {
        let mut input = packet;
        let mut reply = Vec::new();
        let mut deliveries = Vec::new();
        assert_eq!(
            broker.feed(client, &mut input, &mut reply, &mut deliveries),
            Event::More
        );
        assert!(input.is_empty(), "the whole packet was consumed");
        (reply, deliveries)
    }

    #[test]
    fn a_connect_is_answered_and_a_ping_keeps_the_link_up() {
        let mut broker = Broker::new();
        let (reply, _) = run(&mut broker, 1, connect());
        assert_eq!(reply, vec![0x20, 0x02, 0x00, 0x00]);
        let (pong, _) = run(&mut broker, 1, vec![0xc0, 0x00]);
        assert_eq!(pong, vec![0xd0, 0x00]);
        assert_eq!(broker.clients(), 1);
    }

    #[test]
    fn a_publish_reaches_a_matching_subscriber() {
        let mut broker = Broker::new();
        run(&mut broker, 1, connect());
        run(&mut broker, 2, connect());
        let (suback, _) = run(&mut broker, 2, subscribe(3, "sensors/+/temp", 0));
        assert_eq!(suback, vec![0x90, 0x03, 0x00, 0x03, 0x00]);
        let (_, deliveries) = run(
            &mut broker,
            1,
            publish("sensors/kitchen/temp", b"21", 0, false),
        );
        assert_eq!(deliveries.len(), 1);
        let (to, packet) = &deliveries[0];
        assert_eq!(*to, 2);
        assert_eq!(packet[0], 0x30);
        assert!(packet.ends_with(b"21"));
        let topic = b"sensors/kitchen/temp";
        assert!(packet.windows(topic.len()).any(|w| w == topic));
    }

    #[test]
    fn a_retained_publication_is_replayed_to_a_late_subscriber() {
        let mut broker = Broker::new();
        run(&mut broker, 1, connect());
        run(&mut broker, 1, publish("state/led", b"on", 0, true));
        run(&mut broker, 2, connect());
        let (_, deliveries) = run(&mut broker, 2, subscribe(4, "state/#", 0));
        assert_eq!(deliveries.len(), 1);
        let (to, packet) = &deliveries[0];
        assert_eq!(*to, 2);
        assert_eq!(packet[0] & 0x01, 1, "the replay keeps the retain flag");
        assert!(packet.ends_with(b"on"));
    }

    #[test]
    fn a_quality_one_publish_is_acknowledged_and_granted_at_the_subscriber_qos() {
        let mut broker = Broker::new();
        run(&mut broker, 1, connect());
        run(&mut broker, 2, connect());
        run(&mut broker, 2, subscribe(5, "a/b", 1));
        let (reply, deliveries) = run(&mut broker, 1, publish("a/b", b"x", 1, false));
        assert_eq!(reply, vec![0x40, 0x02, 0x00, 0x07]);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].1[0] >> 1 & 0x03, 1, "QoS 1 is preserved");
    }

    #[test]
    fn a_packet_split_across_writes_waits_for_the_rest() {
        let mut broker = Broker::new();
        let packet = connect();
        let (head, tail) = packet.split_at(4);
        let mut input = head.to_vec();
        let mut reply = Vec::new();
        let mut deliveries = Vec::new();
        assert_eq!(
            broker.feed(1, &mut input, &mut reply, &mut deliveries),
            Event::More
        );
        assert!(reply.is_empty());
        input.extend_from_slice(tail);
        assert_eq!(
            broker.feed(1, &mut input, &mut reply, &mut deliveries),
            Event::More
        );
        assert_eq!(reply, vec![0x20, 0x02, 0x00, 0x00]);
    }

    #[test]
    fn unsubscribing_stops_delivery_and_disconnect_drops_the_client() {
        let mut broker = Broker::new();
        run(&mut broker, 1, connect());
        run(&mut broker, 2, connect());
        run(&mut broker, 2, subscribe(6, "x/y", 0));
        let unsubscribe = vec![0xa2, 0x07, 0x00, 0x06, 0x00, 0x03, b'x', b'/', b'y'];
        run(&mut broker, 2, unsubscribe);
        let (_, deliveries) = run(&mut broker, 1, publish("x/y", b"z", 0, false));
        assert!(deliveries.is_empty(), "the filter is gone");
        let mut input = vec![0xe0, 0x00];
        let mut reply = Vec::new();
        let mut deliveries = Vec::new();
        assert_eq!(
            broker.feed(2, &mut input, &mut reply, &mut deliveries),
            Event::Closed
        );
        broker.remove(2);
        let (_, deliveries) = run(&mut broker, 1, publish("x/y", b"z", 0, false));
        assert!(deliveries.is_empty());
    }

    #[test]
    fn a_malformed_protocol_name_closes_the_connection() {
        let mut broker = Broker::new();
        let mut packet = connect();
        packet[2] = b'X';
        let mut input = packet;
        let mut reply = Vec::new();
        let mut deliveries = Vec::new();
        assert_eq!(
            broker.feed(1, &mut input, &mut reply, &mut deliveries),
            Event::Closed
        );
    }

    #[test]
    fn a_clean_session_is_forgotten_and_a_persistent_one_is_resumed() {
        let mut broker = Broker::new();
        let (reply, _) = run(&mut broker, 1, connect_packet("meter", false, 4));
        assert_eq!(reply, vec![0x20, 0x02, 0x00, 0x00]);
        run(&mut broker, 1, subscribe(1, "readings", 1));
        // The link drops without DISCONNECT.
        broker.remove(1);
        assert_eq!(broker.clients(), 0);

        run(&mut broker, 2, connect_packet("publisher", true, 4));
        let (_, deliveries) = run(&mut broker, 2, publish("readings", b"42", 1, false));
        assert!(deliveries.is_empty(), "the subscriber is offline");

        // Reconnecting resumes the subscription, the session-present flag is
        // set and the queued QoS 1 message follows the CONNACK.
        let (reply, _) = run(&mut broker, 3, connect_packet("meter", false, 4));
        assert_eq!(reply[0..4], [0x20, 0x02, 0x01, 0x00]);
        let resumed = &reply[4..];
        assert!(
            resumed.starts_with(&[0x32]),
            "a queued QoS 1 PUBLISH: {reply:02x?}"
        );
        assert!(resumed.ends_with(b"42"));
    }

    #[test]
    fn a_will_is_published_when_the_link_drops_but_not_on_disconnect() {
        let mut broker = Broker::new();
        // CONNECT with will flag, will topic "status" and payload "down".
        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 0x04]);
        body.extend_from_slice(b"MQTT");
        body.push(4);
        body.push(0x06); // clean + will
        body.extend_from_slice(&60u16.to_be_bytes());
        body.extend_from_slice(&[0x00, 0x06]);
        body.extend_from_slice(b"device");
        body.extend_from_slice(&[0x00, 0x06]);
        body.extend_from_slice(b"status");
        body.extend_from_slice(&[0x00, 0x04]);
        body.extend_from_slice(b"down");
        let mut packet = vec![0x10];
        remaining_length(body.len(), &mut packet);
        packet.extend_from_slice(&body);
        run(&mut broker, 1, packet);

        run(&mut broker, 2, connect());
        run(&mut broker, 2, subscribe(2, "status", 0));
        let packets = broker.remove(1);
        assert_eq!(packets.len(), 1, "the will reached the subscriber");
        let (to, packet) = &packets[0];
        assert_eq!(*to, 2);
        assert!(packet.ends_with(b"down"), "{packet:02x?}");

        // A graceful DISCONNECT clears the will: the same connect on flow 3,
        // then DISCONNECT before the flow is dropped.
        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 0x04]);
        body.extend_from_slice(b"MQTT");
        body.push(4);
        body.push(0x06);
        body.extend_from_slice(&60u16.to_be_bytes());
        body.extend_from_slice(&[0x00, 0x03]);
        body.extend_from_slice(b"two");
        body.extend_from_slice(&[0x00, 0x06]);
        body.extend_from_slice(b"status");
        body.extend_from_slice(&[0x00, 0x02]);
        body.extend_from_slice(b"up");
        let mut packet = vec![0x10];
        remaining_length(body.len(), &mut packet);
        packet.extend_from_slice(&body);
        run(&mut broker, 3, packet);
        let mut input = vec![0xe0, 0x00];
        let mut reply = Vec::new();
        let mut deliveries = Vec::new();
        assert_eq!(
            broker.feed(3, &mut input, &mut reply, &mut deliveries),
            Event::Closed
        );
        assert!(broker.remove(3).is_empty(), "a graceful exit has no will");
    }

    #[test]
    fn a_shared_subscription_receives_one_copy_per_group() {
        let mut broker = Broker::new();
        run(&mut broker, 1, connect_packet("a", true, 4));
        run(&mut broker, 2, connect_packet("b", true, 4));
        run(&mut broker, 3, connect_packet("pub", true, 4));
        run(&mut broker, 1, subscribe(1, "$share/workers/jobs/+", 0));
        run(&mut broker, 2, subscribe(2, "$share/workers/jobs/+", 0));
        let (_, deliveries) = run(&mut broker, 3, publish("jobs/one", b"x", 0, false));
        assert_eq!(deliveries.len(), 1, "one copy for the group");
        assert_eq!(deliveries[0].0, 1, "the lowest client id is chosen");
    }

    #[test]
    fn mqtt5_connect_and_publish_carry_an_empty_property_block() {
        let mut broker = Broker::new();
        let (reply, _) = run(&mut broker, 1, connect_packet("v5", true, 5));
        assert_eq!(reply, vec![0x20, 0x03, 0x00, 0x00, 0x00]);

        // SUBSCRIBE at level 5: id, property length 0, filter, qos.
        let mut body = Vec::new();
        body.extend_from_slice(&9u16.to_be_bytes());
        body.push(0);
        body.extend_from_slice(&[0x00, 0x01, b't']);
        body.push(0);
        let mut packet = vec![0x82];
        remaining_length(body.len(), &mut packet);
        packet.extend_from_slice(&body);
        let (suback, _) = run(&mut broker, 1, packet);
        assert_eq!(suback, vec![0x90, 0x04, 0x00, 0x09, 0x00, 0x00]);

        run(&mut broker, 2, connect_packet("pub5", true, 5));
        // PUBLISH at level 5: topic, property length 0, payload.
        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 0x01, b't', 0, b'x']);
        let mut packet = vec![0x30];
        remaining_length(body.len(), &mut packet);
        packet.extend_from_slice(&body);
        let (_, deliveries) = run(&mut broker, 2, packet);
        assert_eq!(deliveries.len(), 1);
        let packet = &deliveries[0].1;
        assert_eq!(packet[0] & 0x0f, 0, "QoS 0");
        assert_eq!(
            packet,
            &vec![0x30, 0x05, 0x00, 0x01, b't', 0x00, b'x'],
            "topic length, topic, an empty property block and the payload"
        );
    }

    #[test]
    fn filters_match_levels_not_substrings() {
        assert!(matches("a/+/c", "a/b/c"));
        assert!(matches("a/#", "a/b/c"));
        assert!(matches("#", "anything/at/all"));
        assert!(!matches("a/+", "a/b/c"));
        assert!(!matches("a/b", "a/bc"));
        assert!(!matches("a/b/c", "a/b"));
    }
}
