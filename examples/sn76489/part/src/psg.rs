//! The SN76489's registers, counters and mixer, independent of the component
//! wrapper.
//!
//! The chip is a write-only 8-bit peripheral: the data bus is sampled on the
//! rising edge of `/WE` while `/CE` is low. A byte with bit 7 set latches a
//! register and writes its low bits; a byte with bit 7 clear writes the high
//! bits of the latched tone register. The latched register survives data
//! bytes, which is why a two-byte write updates a tone without a second latch.
//!
//! Everything here follows the TI SN76489AN as documented by
//! <https://www.smspower.org/Development/SN76489>: a 15-bit noise shift
//! register tapped at bits 0 and 1, seeded with bit 14 set, and a clock
//! divided by 16 before it reaches the tone counters. The audio output is a
//! mixer summing three square waves and noise through 2 dB per step
//! attenuation; this part presents that sum on `AUDIO` as a thresholded
//! digital level, which is what a scope and a passive buzzer can follow.

/// The NTSC colour-burst clock the chip is normally driven with.
pub const CLOCK_HZ: u32 = 3_579_545;

/// Divider between the external clock and the tone/noise counters.
const PRESCALER: u64 = 16;

/// A pin level as the contract carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Low,
    High,
    Floating,
}

/// Audio out, thresholded to a digital level.
pub const AUDIO: u16 = 0;
/// Chip enable, active low.
pub const CE: u16 = 1;
/// First data line; the byte's bit 0.
pub const D0: u16 = 2;
/// Last data line; the byte's bit 7.
pub const D7: u16 = 9;
pub const GND: u16 = 10;
pub const VCC: u16 = 11;
/// Write enable; the byte is taken on its rising edge.
pub const WE: u16 = 12;

/// The pin table in catalog order (alphabetical, see `litz-part-74hc595`:
/// a manifest collects pins in a `BTreeMap`, so sorted order is the order the
/// host assigns indices in).
pub const PINS: [&str; 13] = [
    "AUDIO", "CE", "D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7", "GND", "VCC", "WE",
];

/// The chip's registers and the state of its four generators.
#[derive(Debug)]
pub struct Sn76489 {
    /// 10-bit half-period for channels 0 through 2.
    tone: [u16; 3],
    /// 4-bit attenuation per channel, 15 silent.
    volume: [u8; 4],
    /// The register a data byte writes: `cc << 1 | t`.
    latched: u8,
    /// Noise feedback mode: XOR taps for white, bit 0 for periodic.
    white: bool,
    /// Noise shift rate: 0 is the fastest, 3 follows tone channel 2.
    noise_rate: u8,
    /// The pins as last resolved.
    bus: u8,
    we: bool,
    ce: bool,
    /// Per-channel square-wave level.
    tone_high: [bool; 3],
    /// When each channel next toggles; `None` while its divider is 0 or 1,
    /// which the chip outputs as a constant level.
    tone_next: [Option<u64>; 3],
    /// 15-bit noise shift register, bit 14 the feedback bit.
    lfsr: u16,
    /// The noise counter's toggling output; the register shifts on its rise.
    noise_high: bool,
    noise_next: Option<u64>,
}

impl Default for Sn76489 {
    fn default() -> Self {
        Self {
            tone: [0; 3],
            // Power-on is silent: every attenuator at 15.
            volume: [15; 4],
            latched: 0,
            white: false,
            noise_rate: 0,
            bus: 0,
            we: false,
            ce: false,
            tone_high: [true; 3],
            tone_next: [None; 3],
            lfsr: SEED,
            noise_high: false,
            noise_next: None,
        }
    }
}

/// The shift register's reset value: every bit clear but the top one, which
/// gives periodic noise a 1-in-15 duty cycle.
const SEED: u16 = 0x4000;

impl Sn76489 {
    pub fn new() -> Self {
        Self::default()
    }

    /// A net on one of this part's pins resolved a level.
    pub fn set_pin(&mut self, pin: u16, level: Level, at_ns: u64) {
        let high = level == Level::High;
        match pin {
            CE => self.ce = high,
            WE => {
                let rising = !self.we && high;
                self.we = high;
                if rising && !self.ce {
                    self.write(self.bus, at_ns);
                }
            }
            D0..=D7 => {
                let bit = 1u8 << (pin - D0);
                if high {
                    self.bus |= bit;
                } else {
                    self.bus &= !bit;
                }
            }
            _ => {}
        }
    }

    /// Take one byte off the data bus the way `/WE` does.
    pub fn write(&mut self, byte: u8, at_ns: u64) {
        if byte & 0x80 != 0 {
            self.latched = (byte >> 4) & 0x07;
            let data = byte & 0x0f;
            self.write_low(data, at_ns);
        } else {
            let data = byte & 0x3f;
            self.write_high(data, at_ns);
        }
    }

    /// The latched half of a byte: low four bits of the register.
    fn write_low(&mut self, data: u8, at_ns: u64) {
        match self.latched {
            0 | 2 | 4 => {
                let channel = usize::from(self.latched / 2);
                self.tone[channel] = (self.tone[channel] & 0x3f0) | u16::from(data);
                self.restart_tone(channel, at_ns);
            }
            1 | 3 | 5 => self.set_volume(usize::from(self.latched / 2), data, at_ns),
            6 => self.write_noise(data, at_ns),
            7 => self.set_volume(3, data, at_ns),
            _ => {}
        }
    }

    /// The data half of a byte: low six bits become the high six bits of a
    /// latched tone register. Volume and noise registers take their low bits.
    fn write_high(&mut self, data: u8, at_ns: u64) {
        match self.latched {
            0 | 2 | 4 => {
                let channel = usize::from(self.latched / 2);
                self.tone[channel] = (self.tone[channel] & 0x00f) | (u16::from(data) << 4);
                self.restart_tone(channel, at_ns);
            }
            1 | 3 | 5 => self.set_volume(usize::from(self.latched / 2), data & 0x0f, at_ns),
            6 => self.write_noise(data & 0x07, at_ns),
            7 => self.set_volume(3, data & 0x0f, at_ns),
            _ => {}
        }
    }

    /// An attenuation write. Coming out of silence restarts the channel's
    /// counter, so a channel that has been muted does not carry a stale edge
    /// time into the moment it becomes audible again.
    fn set_volume(&mut self, channel: usize, attenuation: u8, at_ns: u64) {
        let was_silent = self.volume[channel] == 15;
        self.volume[channel] = attenuation;
        if was_silent && attenuation < 15 {
            if channel < 3 {
                self.restart_tone(channel, at_ns);
            } else {
                self.noise_next = Some(at_ns + self.noise_period_ns());
            }
        }
    }

    /// A write to the noise control register reseeds the shift register and
    /// restarts its counter.
    fn write_noise(&mut self, data: u8, at_ns: u64) {
        self.white = data & 0x04 != 0;
        self.noise_rate = data & 0x03;
        self.lfsr = SEED;
        self.noise_high = false;
        self.noise_next = Some(at_ns + self.noise_period_ns());
    }

    /// Restart one channel's divider at `at_ns`, as a write to its register
    /// does.
    fn restart_tone(&mut self, channel: usize, at_ns: u64) {
        self.tone_next[channel] = self.tone_period_ns(channel).map(|period| at_ns + period);
        if channel == 2 && self.noise_rate == 3 {
            self.noise_next = Some(at_ns + self.noise_period_ns());
        }
    }

    /// Ticks between toggles for one tone channel: half the wave's period, so
    /// `divider * prescaler` external clocks. `None` while the divider is 0 or
    /// 1, which the chip emits as a constant level.
    fn tone_period_ns(&self, channel: usize) -> Option<u64> {
        let divider = self.tone[channel];
        (divider >= 2).then(|| ns_for_cycles(u64::from(divider) * PRESCALER))
    }

    /// Ticks between noise-counter borrows.
    fn noise_period_ns(&self) -> u64 {
        let reset = match self.noise_rate {
            0 => 16,
            1 => 32,
            2 => 64,
            _ => u64::from(self.tone[2].max(1)),
        };
        ns_for_cycles(reset * PRESCALER)
    }

    /// The next moment the part wants attention. A channel at attenuation 15
    /// is silent, so its edges cannot change the mix and are not scheduled:
    /// the counters still hold their phase, and `set_volume` restarts the one
    /// that comes back.
    pub fn next_event(&self) -> Option<u64> {
        self.tone_next
            .iter()
            .enumerate()
            .filter(|(channel, _)| self.volume[*channel] < 15)
            .filter_map(|(_, at)| *at)
            .chain(self.noise_next.filter(|_| self.volume[3] < 15))
            .min()
    }

    /// Run the counters to `now`.
    pub fn advance(&mut self, now: u64) {
        for channel in 0..3 {
            let Some(mut at) = self.tone_next[channel] else {
                continue;
            };
            let Some(period) = self.tone_period_ns(channel) else {
                self.tone_next[channel] = None;
                continue;
            };
            if at > now {
                continue;
            }
            let toggles = (now - at) / period + 1;
            if toggles % 2 == 1 {
                self.tone_high[channel] = !self.tone_high[channel];
            }
            at += toggles * period;
            self.tone_next[channel] = Some(at);
        }

        if let Some(mut at) = self.noise_next {
            let period = self.noise_period_ns();
            if at <= now {
                let steps = (now - at) / period + 1;
                // The counter output rises on every other borrow, and that
                // rise is the shift register's clock. Starting high, the next
                // step falls, so the two starting phases count differently.
                let rises = if self.noise_high {
                    steps / 2
                } else {
                    steps.div_ceil(2)
                };
                for _ in 0..rises {
                    self.shift_noise();
                }
                if steps % 2 == 1 {
                    self.noise_high = !self.noise_high;
                }
                at += steps * period;
                self.noise_next = Some(at);
            }
        }
    }

    /// One shift of the noise register. White noise feeds back the XOR of
    /// bits 0 and 1; periodic noise feeds bit 0 back into itself.
    fn shift_noise(&mut self) {
        let feedback = if self.white {
            let tapped = (self.lfsr & 0x0003).count_ones() & 1;
            tapped as u16
        } else {
            self.lfsr & 1
        };
        self.lfsr = (self.lfsr >> 1) | (feedback << 14);
    }

    /// Instantaneous level of one channel, 0 or 1.
    fn channel_bit(&self, channel: usize) -> bool {
        match channel {
            0..=2 => self.tone_high[channel],
            _ => self.lfsr & 1 != 0,
        }
    }

    /// Attenuation as a linear weight: each step is 2 dB, so a step is
    /// `10^(-0.1)` of the previous amplitude.
    fn weight(&self, channel: usize) -> f32 {
        match self.volume[channel] {
            15 => 0.0,
            steps => 10f32.powf(-(f32::from(steps)) / 10.0),
        }
    }

    /// The mixed output, in channel-volts before the amplifier: 0 through 3.
    pub fn mix(&self) -> f32 {
        (0..4)
            .filter(|&channel| self.channel_bit(channel))
            .map(|channel| self.weight(channel))
            .sum()
    }

    /// The audible weight the mixer can reach right now. The threshold sits
    /// at half of it, so one full-volume channel toggles the output and a
    /// silent mixer holds it low.
    fn mix_ceiling(&self) -> f32 {
        (0..4).map(|channel| self.weight(channel)).sum()
    }

    /// What `AUDIO` drives.
    pub fn output_high(&self) -> bool {
        let ceiling = self.mix_ceiling();
        ceiling > 0.0 && self.mix() * 2.0 >= ceiling
    }

    /// The voltage on `AUDIO`, so a scope sees the mix rather than the
    /// thresholded version of it.
    pub fn analog_volts(&self) -> f32 {
        self.mix().min(3.3)
    }

    /// The first audible tone, which is what a test can name.
    pub fn state(&self) -> (bool, u32) {
        for channel in 0..3 {
            if self.volume[channel] < 15 {
                let hz = tone_hz(self.tone[channel]);
                return (true, hz);
            }
        }
        if self.volume[3] < 15 {
            return (true, self.noise_hz());
        }
        (false, 0)
    }

    /// The noise channel's shift rate in hertz.
    fn noise_hz(&self) -> u32 {
        let cycles = match self.noise_rate {
            0 => 16,
            1 => 32,
            2 => 64,
            _ => u64::from(self.tone[2].max(1)),
        };
        // Two counter borrows per shift, each borrow `cycles` prescaled ticks.
        (u64::from(CLOCK_HZ) / (cycles * PRESCALER * 2)) as u32
    }

    /// The register values, for tests and the host log.
    pub fn registers(&self) -> ([u16; 3], [u8; 4], bool, u8) {
        (self.tone, self.volume, self.white, self.noise_rate)
    }
}

/// The tone a divider produces: `clock / (32 * divider)`, and 0 for the
/// constant levels a divider of 0 or 1 selects.
pub fn tone_hz(divider: u16) -> u32 {
    if divider < 2 {
        0
    } else {
        (u64::from(CLOCK_HZ) / (32 * u64::from(divider))) as u32
    }
}

/// Ticks for a whole number of prescaled cycles.
fn ns_for_cycles(cycles: u64) -> u64 {
    let ns = cycles * 1_000_000_000 / u64::from(CLOCK_HZ);
    ns.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hi(part: &mut Sn76489, pin: u16) {
        part.set_pin(pin, Level::High, 0);
    }

    fn lo(part: &mut Sn76489, pin: u16) {
        part.set_pin(pin, Level::Low, 0);
    }

    /// Put one byte on the bus and strobe `/WE`.
    fn send(part: &mut Sn76489, byte: u8) {
        for bit in 0..8u16 {
            let pin = D0 + bit;
            if byte & (1 << bit) != 0 {
                hi(part, pin);
            } else {
                lo(part, pin);
            }
        }
        hi(part, WE);
        lo(part, WE);
    }

    #[test]
    fn pin_indices_are_in_catalog_order() {
        let mut sorted = PINS;
        sorted.sort_unstable();
        assert_eq!(sorted, PINS);
        assert_eq!((AUDIO, CE, D0, D7, GND, VCC, WE), (0, 1, 2, 9, 10, 11, 12));
    }

    #[test]
    fn the_power_on_state_is_silent() {
        let part = Sn76489::new();
        assert_eq!(part.registers(), ([0; 3], [15; 4], false, 0));
        assert_eq!(part.state(), (false, 0));
        assert!(!part.output_high());
        assert_eq!(part.next_event(), None);
    }

    #[test]
    fn a_tone_is_two_bytes_and_440_hz_on_channel_0() {
        let mut part = Sn76489::new();
        send(&mut part, 0x80 | 0x0e); // latch channel 0 tone, low nibble 0xe
        send(&mut part, 0x0f); // high six bits: 0x0f -> N = 0x0fe
        send(&mut part, 0x90); // channel 0 volume, full
        assert_eq!(part.registers().0[0], 0x0fe);
        assert_eq!(tone_hz(0x0fe), 440);
        assert_eq!(part.state(), (true, 440));
    }

    #[test]
    fn the_latched_register_survives_a_data_byte_for_volume() {
        let mut part = Sn76489::new();
        send(&mut part, 0xb0 | 0x0f); // channel 1 volume: silent
        send(&mut part, 0x00); // data byte: low four bits update the volume
        assert_eq!(part.registers().1[1], 0x00);
    }

    #[test]
    fn a_noise_data_byte_is_not_ignored() {
        let mut part = Sn76489::new();
        send(&mut part, 0xe0 | 0x05); // channel 3 noise, white, rate 1
        assert_eq!((part.registers().2, part.registers().3), (true, 1));
        send(&mut part, 0x04); // data byte: white, rate 0
        assert_eq!((part.registers().2, part.registers().3), (true, 0));
    }

    #[test]
    fn chip_enable_is_active_low() {
        let mut part = Sn76489::new();
        hi(&mut part, CE);
        send(&mut part, 0x9f); // would be channel 0 silent if it landed
        assert_eq!(part.registers().1[0], 15, "no register changed");
        assert_eq!(part.state(), (false, 0));
        lo(&mut part, CE);
        send(&mut part, 0x90);
        assert_eq!(part.registers().1[0], 0);
    }

    #[test]
    fn only_an_audible_channel_is_scheduled() {
        let mut part = Sn76489::new();
        send(&mut part, 0x80 | 0x0e);
        send(&mut part, 0x0f);
        assert_eq!(part.next_event(), None, "attenuation 15 is silent");
        send(&mut part, 0x90);
        assert_eq!(
            part.next_event(),
            Some(part.tone_period_ns(0).unwrap()),
            "unmuting starts the divider at the write"
        );
        send(&mut part, 0x9f);
        assert_eq!(part.next_event(), None, "muting it stops the schedule");
    }

    #[test]
    fn a_square_wave_toggles_at_twice_its_frequency() {
        let mut part = Sn76489::new();
        send(&mut part, 0x80 | 0x0e);
        send(&mut part, 0x0f);
        send(&mut part, 0x90);
        let period = part.tone_period_ns(0).unwrap();
        // 440 Hz is 2.27 ms per cycle, so edges are 1.135 ms apart.
        assert!((period as f64 - 1_136_000.0).abs() < 3_000.0, "{period}");

        let high = part.output_high();
        part.advance(period);
        assert_ne!(part.output_high(), high, "one edge flips the mix");
        part.advance(period * 2);
        assert_eq!(part.output_high(), high, "two edges come back");
    }

    #[test]
    fn attenuation_steps_are_two_decibels() {
        let mut part = Sn76489::new();
        send(&mut part, 0x90); // channel 0 full
        let full = part.weight(0);
        send(&mut part, 0x92); // two steps down
        let quiet = part.weight(0);
        assert!((full - 1.0).abs() < 1e-6);
        assert!((quiet - 0.630_957_2).abs() < 1e-4, "{quiet}");
        send(&mut part, 0x9f);
        assert_eq!(part.weight(0), 0.0, "15 is silence, not 2 dB more");
    }

    #[test]
    fn periodic_noise_walks_a_fifteen_bit_ring() {
        let mut part = Sn76489::new();
        send(&mut part, 0xe0); // periodic, fastest rate
        send(&mut part, 0xf0); // noise volume full
        assert!(!part.registers().2);
        assert_eq!(part.lfsr, SEED);
        let borrow = part.noise_period_ns();
        // Two borrows per shift, so fifteen shifts come back to the seed.
        for index in 1..=15u64 {
            part.advance(borrow * 2 * index);
            if index < 15 {
                assert_ne!(part.lfsr, SEED, "shift {index}");
                assert_ne!(part.lfsr, 0, "the ring is never all zeroes");
            }
        }
        assert_eq!(part.lfsr, SEED, "a 15-bit one-bit ring has period 15");
    }

    #[test]
    fn white_noise_is_not_a_short_ring() {
        let mut part = Sn76489::new();
        send(&mut part, 0xe4); // white, fastest rate
        send(&mut part, 0xf0);
        let borrow = part.noise_period_ns();
        let mut seen = Vec::new();
        for shift in 1..=64u64 {
            part.advance(borrow * 2 * shift);
            seen.push(part.lfsr);
        }
        let distinct: std::collections::BTreeSet<_> = seen.iter().copied().collect();
        assert_eq!(distinct.len(), seen.len(), "a 15-bit LFSR repeats late");
        assert!(seen.iter().all(|&state| state != 0));
    }
}
