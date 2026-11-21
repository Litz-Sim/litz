//! SPI decoding: MSB first, chip select active low.
//!
//! The clock edge that samples data follows from the mode: modes 0 and 3
//! sample on a rising SCK edge, modes 1 and 2 on a falling one. A trailing
//! group of fewer than eight bits is dropped rather than reported as an error.

use litz_core::{Level, Tick};

use crate::{Frame, Wave, merge_events};

const CS: u8 = 0;
const SCK: u8 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpiMode {
    #[default]
    Mode0,
    Mode1,
    Mode2,
    Mode3,
}

impl SpiMode {
    /// CPOL == CPHA: the sampling edge is a rising one.
    fn samples_rising(self) -> bool {
        matches!(self, SpiMode::Mode0 | SpiMode::Mode3)
    }
}

/// Decode SPI frames from SCK, MOSI, optional MISO and CS recordings.
///
/// A frame opens when CS falls and closes when it rises. If the record starts
/// with CS already low, the frame opens at the first SCK change and is dated
/// from its first sample.
pub fn spi(
    sck: &[(Tick, Level)],
    mosi: &[(Tick, Level)],
    miso: Option<&[(Tick, Level)]>,
    cs: &[(Tick, Level)],
    mode: SpiMode,
) -> Vec<Frame> {
    if sck.is_empty() || cs.is_empty() {
        return Vec::new();
    }

    let mosi_wave = Wave::new(mosi);
    let miso_wave = miso.map(Wave::new);
    let has_miso = miso.is_some();
    let sample_level = if mode.samples_rising() {
        Level::High
    } else {
        Level::Low
    };

    // CS first on ties so a falling chip select enables the same-tick clock
    // edge for sampling.
    let events = merge_events(cs, CS, sck, SCK);
    let mut cs_level = cs.first().map(|&(_, level)| level);
    let mut open: Option<Partial> = None;
    let mut frames = Vec::new();

    for &(tick, channel, level) in &events {
        if channel == CS {
            let was = cs_level;
            cs_level = Some(level);
            if level == Level::Low && was != Some(Level::Low) {
                open = Some(Partial::new(Some(tick), has_miso));
            } else if level == Level::High
                && was == Some(Level::Low)
                && let Some(partial) = open.take()
            {
                partial.finish(tick, &mut frames);
            }
        } else {
            // CS already low without a recorded falling edge: the capture
            // started mid-transaction, so date the frame from its first bit.
            if open.is_none() && cs_level == Some(Level::Low) {
                open = Some(Partial::new(None, has_miso));
            }
            if level == sample_level
                && let Some(partial) = open.as_mut()
            {
                partial.sample(tick, &mosi_wave, miso_wave.as_ref());
            }
        }
    }

    if let Some(partial) = open {
        // Close an unterminated frame at the last clock or data change.
        let end = last_tick(sck)
            .max(last_tick(mosi))
            .max(miso.map_or(0, last_tick))
            .max(partial.last_sample.unwrap_or(0));
        partial.finish(end, &mut frames);
    }

    frames
}

struct Partial {
    at: Option<Tick>,
    last_sample: Option<Tick>,
    mosi: Vec<u8>,
    miso: Vec<u8>,
    mosi_byte: u8,
    miso_byte: u8,
    bits: u8,
    has_miso: bool,
}

impl Partial {
    fn new(at: Option<Tick>, has_miso: bool) -> Self {
        Self {
            at,
            last_sample: None,
            mosi: Vec::new(),
            miso: Vec::new(),
            mosi_byte: 0,
            miso_byte: 0,
            bits: 0,
            has_miso,
        }
    }

    fn sample(&mut self, tick: Tick, mosi: &Wave<'_>, miso: Option<&Wave<'_>>) {
        self.at.get_or_insert(tick);
        self.last_sample = Some(tick);
        self.mosi_byte = (self.mosi_byte << 1) | u8::from(mosi.at(tick) == Some(Level::High));
        if let Some(miso) = miso {
            self.miso_byte = (self.miso_byte << 1) | u8::from(miso.at(tick) == Some(Level::High));
        }
        self.bits += 1;
        if self.bits == 8 {
            self.mosi.push(self.mosi_byte);
            if self.has_miso {
                self.miso.push(self.miso_byte);
            }
            self.mosi_byte = 0;
            self.miso_byte = 0;
            self.bits = 0;
        }
    }

    fn finish(self, end: Tick, frames: &mut Vec<Frame>) {
        if self.mosi.is_empty() {
            return;
        }
        let has_miso = self.has_miso;
        let miso = if has_miso { self.miso } else { Vec::new() };
        let text = frame_text(&self.mosi, has_miso.then_some(miso.as_slice()));
        frames.push(Frame {
            at: self.at.unwrap_or(end),
            end,
            bytes: self.mosi,
            miso,
            text,
            error: None,
        });
    }
}

fn last_tick(points: &[(Tick, Level)]) -> Tick {
    points.last().map_or(0, |&(tick, _)| tick)
}

fn frame_text(mosi: &[u8], miso: Option<&[u8]>) -> String {
    let mut text = hex_list(mosi);
    if let Some(miso) = miso {
        text.push_str(" -> ");
        text.push_str(&hex_list(miso));
    }
    text
}

fn hex_list(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("0x{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: Tick = 10;

    struct Bus {
        sck: Vec<(Tick, Level)>,
        mosi: Vec<(Tick, Level)>,
        miso: Vec<(Tick, Level)>,
        cs: Vec<(Tick, Level)>,
        tick: Tick,
    }

    impl Bus {
        fn new(clock_idle: Level) -> Self {
            Self {
                sck: vec![(0, clock_idle)],
                mosi: vec![(0, Level::Low)],
                miso: vec![(0, Level::Low)],
                cs: vec![(0, Level::High)],
                tick: H,
            }
        }

        fn set(line: &mut Vec<(Tick, Level)>, tick: Tick, level: Level) {
            if line.last().map(|&(_, last)| last) != Some(level) {
                line.push((tick, level));
            }
        }

        fn data(&mut self, mosi: Level, miso: Level) {
            Self::set(&mut self.mosi, self.tick, mosi);
            Self::set(&mut self.miso, self.tick, miso);
            self.tick += H;
        }

        fn clock(&mut self, level: Level) {
            Self::set(&mut self.sck, self.tick, level);
            self.tick += H;
        }

        fn chip(&mut self, level: Level) {
            Self::set(&mut self.cs, self.tick, level);
            self.tick += H;
        }

        /// Clock out the top `count` bits of `mosi`/`miso` in the given mode.
        fn bits(&mut self, mosi: u8, miso: u8, mode: SpiMode, count: u32) {
            for n in (0..count).rev() {
                let m = bit_level(mosi, n);
                let s = bit_level(miso, n);
                match mode {
                    // Leading edge is the sample edge for CPHA == 0.
                    SpiMode::Mode0 => {
                        self.data(m, s);
                        self.clock(Level::High);
                        self.clock(Level::Low);
                    }
                    // Leading edge shifts, trailing edge samples.
                    SpiMode::Mode1 => {
                        self.data(m, s);
                        self.clock(Level::High);
                        self.clock(Level::Low);
                    }
                    SpiMode::Mode2 => {
                        self.data(m, s);
                        self.clock(Level::Low);
                        self.clock(Level::High);
                    }
                    SpiMode::Mode3 => {
                        self.clock(Level::Low);
                        self.data(m, s);
                        self.clock(Level::High);
                    }
                }
            }
        }
    }

    fn bit_level(byte: u8, n: u32) -> Level {
        if byte >> n & 1 == 1 {
            Level::High
        } else {
            Level::Low
        }
    }

    #[test]
    fn mode0_decodes_mosi_and_miso() {
        let mut bus = Bus::new(Level::Low);
        let at = bus.tick;
        bus.chip(Level::Low);
        bus.bits(0x9f, 0x12, SpiMode::Mode0, 8);
        bus.bits(0x00, 0x34, SpiMode::Mode0, 8);
        bus.bits(0x00, 0x56, SpiMode::Mode0, 8);
        let end = bus.tick;
        bus.chip(Level::High);

        let frames = spi(
            &bus.sck,
            &bus.mosi,
            Some(&bus.miso),
            &bus.cs,
            SpiMode::Mode0,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].at, at);
        assert_eq!(frames[0].end, end);
        assert_eq!(frames[0].bytes, vec![0x9f, 0x00, 0x00]);
        assert_eq!(frames[0].miso, vec![0x12, 0x34, 0x56]);
        assert_eq!(frames[0].text, "0x9f 0x00 0x00 -> 0x12 0x34 0x56");
        assert_eq!(frames[0].error, None);
    }

    #[test]
    fn mode3_samples_on_the_rising_edge() {
        let mut bus = Bus::new(Level::High);
        bus.chip(Level::Low);
        bus.bits(0xa5, 0x5a, SpiMode::Mode3, 8);
        bus.chip(Level::High);

        let frames = spi(
            &bus.sck,
            &bus.mosi,
            Some(&bus.miso),
            &bus.cs,
            SpiMode::Mode3,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0xa5]);
        assert_eq!(frames[0].miso, vec![0x5a]);
    }

    #[test]
    fn mode1_samples_on_the_falling_edge() {
        let mut bus = Bus::new(Level::Low);
        bus.chip(Level::Low);
        bus.bits(0x81, 0, SpiMode::Mode1, 8);
        bus.chip(Level::High);

        let frames = spi(&bus.sck, &bus.mosi, None, &bus.cs, SpiMode::Mode1);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x81]);
        assert!(frames[0].miso.is_empty());
        assert_eq!(frames[0].text, "0x81");
    }

    #[test]
    fn cs_held_low_opens_at_the_first_clock_change() {
        let mut bus = Bus::new(Level::Low);
        bus.cs = vec![(0, Level::Low)];
        bus.bits(0x55, 0, SpiMode::Mode0, 8);

        let frames = spi(&bus.sck, &bus.mosi, None, &bus.cs, SpiMode::Mode0);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x55]);
        // First sample is the first rising edge, one half period after data.
        assert_eq!(frames[0].at, 2 * H);
    }

    #[test]
    fn trailing_partial_byte_is_dropped() {
        let mut bus = Bus::new(Level::Low);
        bus.chip(Level::Low);
        bus.bits(0x9f, 0, SpiMode::Mode0, 8);
        bus.bits(0b1010, 0, SpiMode::Mode0, 4);
        bus.chip(Level::High);

        let frames = spi(&bus.sck, &bus.mosi, None, &bus.cs, SpiMode::Mode0);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x9f]);
        assert_eq!(frames[0].error, None);
    }

    #[test]
    fn unterminated_cs_closes_at_the_last_change() {
        let mut bus = Bus::new(Level::Low);
        bus.chip(Level::Low);
        bus.bits(0x3c, 0, SpiMode::Mode0, 8);

        let frames = spi(&bus.sck, &bus.mosi, None, &bus.cs, SpiMode::Mode0);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x3c]);
        assert_eq!(frames[0].end, bus.sck.last().unwrap().0);
    }

    #[test]
    fn missing_channels_yield_nothing() {
        assert!(spi(&[], &[], None, &[], SpiMode::Mode0).is_empty());
        assert!(spi(&[(0, Level::Low)], &[], None, &[], SpiMode::Mode0).is_empty());
        assert!(spi(&[], &[], None, &[(0, Level::High)], SpiMode::Mode0).is_empty());
    }
}
