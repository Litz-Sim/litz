//! I2C decoding for 7-bit addressed buses.
//!
//! Ten-bit addressing is not decoded: the first byte is always treated as
//! `address << 1 | direction`, which misreads a 10-bit address as a 7-bit one.

use litz_core::{Level, Tick};

use crate::{Frame, merge_events};

const SCL: u8 = 0;
const SDA: u8 = 1;

/// Decode complete I2C transactions from SDA and SCL recordings.
///
/// START and STOP are SDA transitions while SCL is high. Data changes while
/// SCL is low are ignored. A repeated START closes the open transaction (with
/// no error) and opens the next one at the same tick.
pub fn i2c(sda: &[(Tick, Level)], scl: &[(Tick, Level)]) -> Vec<Frame> {
    if sda.is_empty() || scl.is_empty() {
        return Vec::new();
    }

    // SCL has priority on ties so a simultaneous clock rise and data change
    // is seen as an enabled bus, matching how a device latches the bit.
    let events = merge_events(scl, SCL, sda, SDA);

    let mut sda_level = sda.first().map(|&(_, level)| level);
    let mut scl_level = scl.first().map(|&(_, level)| level);
    let mut open: Option<Transaction> = None;
    let mut frames = Vec::new();
    let mut last_tick = 0;

    for (index, &(tick, channel, level)) in events.iter().enumerate() {
        last_tick = tick;
        if channel == SCL {
            scl_level = Some(level);
            if level == Level::High
                && let Some(tx) = open.as_mut()
            {
                // A data change at this same tick has not been visited yet,
                // but the device sees the settled levels, so fold it in.
                let sampled = events[index + 1..]
                    .iter()
                    .take_while(|event| event.0 == tick)
                    .find(|event| event.1 == SDA)
                    .map(|event| event.2)
                    .or(sda_level);
                tx.push_bit(sampled);
            }
        } else {
            let was = sda_level;
            sda_level = Some(level);
            if scl_level != Some(Level::High) {
                continue;
            }
            if was == Some(Level::High) && level == Level::Low {
                // START, possibly repeated: end the previous transaction here.
                if let Some(tx) = open.take() {
                    frames.push(tx.finish(tick, false, None));
                }
                open = Some(Transaction::new(tick));
            } else if was == Some(Level::Low)
                && level == Level::High
                && let Some(tx) = open.take()
            {
                frames.push(tx.finish(tick, true, None));
            }
        }
    }

    if let Some(tx) = open {
        frames.push(tx.finish(last_tick, false, Some("no stop condition")));
    }
    frames
}

struct Transaction {
    start: Tick,
    groups: Vec<(u8, bool)>,
    current: u8,
    bits: u8,
}

impl Transaction {
    fn new(start: Tick) -> Self {
        Self {
            start,
            groups: Vec::new(),
            current: 0,
            bits: 0,
        }
    }

    /// Feed one sampled SCL-high level. Bits are MSB first; every ninth bit
    /// is the acknowledge, low when the receiver acknowledged.
    fn push_bit(&mut self, level: Option<Level>) {
        if self.bits < 8 {
            if level == Some(Level::High) {
                self.current |= 1 << (7 - self.bits);
            }
            self.bits += 1;
        } else {
            let ack = level == Some(Level::Low);
            self.groups.push((self.current, ack));
            self.current = 0;
            self.bits = 0;
        }
    }

    fn finish(self, end: Tick, stopped: bool, error: Option<&str>) -> Frame {
        let mut bytes = Vec::new();
        let mut items = Vec::new();
        if let Some(&(address_byte, _)) = self.groups.first() {
            let address = address_byte >> 1;
            let direction = if address_byte & 1 == 0 {
                "write"
            } else {
                "read"
            };
            items.push(format!("start 0x{address:02x} {direction}"));
            for &(byte, ack) in &self.groups[1..] {
                bytes.push(byte);
                items.push(format!("0x{byte:02x} {}", if ack { "ack" } else { "nack" }));
            }
        } else {
            // A START without a complete address byte: report what is known.
            items.push("start".to_string());
        }
        if stopped {
            items.push("stop".to_string());
        }
        Frame {
            at: self.start,
            end,
            bytes,
            miso: Vec::new(),
            text: items.join(", "),
            error: error.map(|message| message.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF: Tick = 10;

    struct Bus {
        sda: Vec<(Tick, Level)>,
        scl: Vec<(Tick, Level)>,
        tick: Tick,
    }

    impl Bus {
        fn idle() -> Self {
            Self {
                sda: vec![(0, Level::High)],
                scl: vec![(0, Level::High)],
                tick: HALF,
            }
        }

        fn set(line: &mut Vec<(Tick, Level)>, tick: Tick, level: Level) {
            if line.last().map(|&(_, last)| last) != Some(level) {
                line.push((tick, level));
            }
        }

        fn both(&mut self, sda: Option<Level>, scl: Option<Level>) {
            if let Some(level) = scl {
                Self::set(&mut self.scl, self.tick, level);
            }
            if let Some(level) = sda {
                Self::set(&mut self.sda, self.tick, level);
            }
            self.tick += HALF;
        }

        fn start(&mut self) -> Tick {
            let at = self.tick;
            self.both(Some(Level::Low), None);
            self.both(None, Some(Level::Low));
            at
        }

        fn repeated_start(&mut self) -> Tick {
            self.both(Some(Level::High), None);
            self.both(None, Some(Level::High));
            let at = self.tick;
            self.both(Some(Level::Low), None);
            self.both(None, Some(Level::Low));
            at
        }

        fn bit(&mut self, value: bool) {
            let level = if value { Level::High } else { Level::Low };
            self.both(Some(level), None);
            self.both(None, Some(Level::High));
            self.both(None, Some(Level::Low));
        }

        fn write_byte(&mut self, byte: u8) {
            for n in (0..8).rev() {
                self.bit(byte >> n & 1 == 1);
            }
        }

        fn ack(&mut self, ack: bool) {
            let level = if ack { Level::Low } else { Level::High };
            self.both(Some(level), None);
            self.both(None, Some(Level::High));
            self.both(None, Some(Level::Low));
        }

        fn stop(&mut self) -> Tick {
            self.both(Some(Level::Low), None);
            self.both(None, Some(Level::High));
            let at = self.tick;
            self.both(Some(Level::High), None);
            at
        }

        fn decode(&self) -> Vec<Frame> {
            i2c(&self.sda, &self.scl)
        }
    }

    #[test]
    fn decodes_write_with_ack() {
        let mut bus = Bus::idle();
        let start = bus.start();
        bus.write_byte(0x3c << 1);
        bus.ack(true);
        bus.write_byte(0x00);
        bus.ack(true);
        bus.write_byte(0xaf);
        bus.ack(false);
        let stop = bus.stop();

        let frames = bus.decode();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].at, start);
        assert_eq!(frames[0].end, stop);
        assert_eq!(frames[0].bytes, vec![0x00, 0xaf]);
        assert_eq!(
            frames[0].text,
            "start 0x3c write, 0x00 ack, 0xaf nack, stop"
        );
        assert_eq!(frames[0].error, None);
    }

    #[test]
    fn decodes_read_with_nack() {
        let mut bus = Bus::idle();
        bus.start();
        bus.write_byte(0x50 << 1 | 1);
        bus.ack(true);
        bus.write_byte(0x42);
        bus.ack(false);
        bus.stop();

        let frames = bus.decode();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x42]);
        assert_eq!(frames[0].text, "start 0x50 read, 0x42 nack, stop");
    }

    #[test]
    fn repeated_start_closes_the_previous_transaction() {
        let mut bus = Bus::idle();
        bus.start();
        bus.write_byte(0x3c << 1);
        bus.ack(true);
        let restart = bus.repeated_start();
        bus.write_byte(0x50 << 1 | 1);
        bus.ack(true);
        bus.write_byte(0x42);
        bus.ack(false);
        let stop = bus.stop();

        let frames = bus.decode();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].end, restart);
        assert_eq!(frames[0].text, "start 0x3c write");
        assert_eq!(frames[0].error, None);
        assert_eq!(frames[1].at, restart);
        assert_eq!(frames[1].end, stop);
        assert_eq!(frames[1].text, "start 0x50 read, 0x42 nack, stop");
    }

    #[test]
    fn transaction_without_stop_is_reported() {
        let mut bus = Bus::idle();
        bus.start();
        bus.write_byte(0x3c << 1);
        bus.ack(true);
        bus.write_byte(0x55);
        bus.ack(true);

        let frames = bus.decode();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x55]);
        assert_eq!(frames[0].error.as_deref(), Some("no stop condition"));
        assert_eq!(frames[0].text, "start 0x3c write, 0x55 ack");
        assert_eq!(frames[0].end, bus.scl.last().unwrap().0);
    }

    #[test]
    fn empty_or_one_sided_input_yields_nothing() {
        assert!(i2c(&[], &[]).is_empty());
        assert!(i2c(&[(0, Level::High)], &[]).is_empty());
        assert!(i2c(&[], &[(0, Level::High)]).is_empty());
    }
}
