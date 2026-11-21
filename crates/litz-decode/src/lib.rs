//! Decode UART, I2C and SPI from probed digital waveforms.
//!
//! A waveform is a record of level changes, not a sampled grid: each entry is
//! the level *after* a change, held until the next entry. The first entry is
//! the level when recording started. All times are ticks (1 ns).
//!
//! Decoders are deliberately tolerant: empty, short or out-of-order records
//! yield no frames or best-effort frames rather than a panic. Where the
//! protocol leaves room for interpretation the choice is documented on the
//! decoder.

use litz_core::{Level, TICKS_PER_SEC, Tick};

mod i2c;
mod spi;
mod uart;

pub use i2c::i2c;
pub use spi::{SpiMode, spi};
pub use uart::uart;

/// A read-only view over a recorded waveform.
#[derive(Clone, Copy)]
pub struct Wave<'a> {
    points: &'a [(Tick, Level)],
}

impl<'a> Wave<'a> {
    pub fn new(points: &'a [(Tick, Level)]) -> Self {
        Self { points }
    }

    /// The level held at `at`: the last point at or before it.
    pub fn at(&self, at: Tick) -> Option<Level> {
        let idx = self.points.partition_point(|&(tick, _)| tick <= at);
        self.points
            .get(idx.checked_sub(1)?)
            .map(|&(_, level)| level)
    }

    /// Change points: (tick, level after the change), in order.
    pub fn edges(&self) -> impl Iterator<Item = (Tick, Level)> + '_ {
        self.points.iter().copied()
    }

    /// Shortest run between consecutive changes, in ticks.
    ///
    /// The first entry is the level held when recording started, not a change,
    /// so the segment before the first change is open on the left: how long
    /// the line had already been idle is not part of the record. It is
    /// skipped, which matters when a capture begins just before the first
    /// byte and that idle looks shorter than one bit. Repeated ticks (a
    /// malformed record) are skipped too: they describe no run.
    pub fn shortest_run(&self) -> Option<u64> {
        self.points
            .windows(2)
            .skip(1)
            .map(|pair| pair[1].0.saturating_sub(pair[0].0))
            .filter(|&run| run > 0)
            .min()
    }

    /// Ticks of the first and last change points.
    pub fn span(&self) -> Option<(Tick, Tick)> {
        let first = self.points.first()?.0;
        let last = self.points.last()?.0;
        Some((first, last))
    }
}

/// One decoded frame or bus transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// When the frame or transaction began.
    pub at: Tick,
    /// When it ended.
    pub end: Tick,
    /// Payload bytes (UART: the byte; I2C: data bytes after the address;
    /// SPI: MOSI bytes).
    pub bytes: Vec<u8>,
    /// SPI MISO bytes; empty for every other protocol.
    pub miso: Vec<u8>,
    /// One-line human summary.
    pub text: String,
    /// Framing or protocol problem, if any.
    pub error: Option<String>,
}

/// Bit period from the shortest run, rounded to the nearest integer baud.
///
/// The shortest run is assumed to be one bit wide, which holds for any
/// recording containing a transition between adjacent bits.
pub fn infer_baud(points: &[(Tick, Level)]) -> Option<u32> {
    let run = Wave::new(points).shortest_run()? as u128;
    if run == 0 {
        return None;
    }
    // Round half up without floating point so the result is exact.
    let baud = (TICKS_PER_SEC as u128 + run / 2) / run;
    if baud == 0 || baud > u32::MAX as u128 {
        return None;
    }
    Some(baud as u32)
}

/// Merge two channel recordings into one time-ordered event list.
///
/// `priority` decides ties: the channel with the lower value is processed
/// first, so the caller can encode bus rules such as "clock before data".
pub(crate) fn merge_events(
    a: &[(Tick, Level)],
    a_priority: u8,
    b: &[(Tick, Level)],
    b_priority: u8,
) -> Vec<(Tick, u8, Level)> {
    let mut events = Vec::with_capacity(a.len() + b.len());
    events.extend(a.iter().map(|&(tick, level)| (tick, a_priority, level)));
    events.extend(b.iter().map(|&(tick, level)| (tick, b_priority, level)));
    events.sort_by_key(|&(tick, priority, _)| (tick, priority));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    const POINTS: &[(Tick, Level)] = &[
        (0, Level::High),
        (100, Level::Low),
        (250, Level::High),
        (400, Level::Low),
    ];

    #[test]
    fn at_returns_the_level_held_at_a_tick() {
        let wave = Wave::new(POINTS);
        assert_eq!(wave.at(0), Some(Level::High));
        assert_eq!(wave.at(99), Some(Level::High));
        assert_eq!(wave.at(100), Some(Level::Low));
        assert_eq!(wave.at(399), Some(Level::High));
        assert_eq!(wave.at(1_000), Some(Level::Low));
    }

    #[test]
    fn at_before_the_recording_is_none() {
        let wave = Wave::new(&POINTS[1..]);
        assert_eq!(wave.at(50), None);
    }

    #[test]
    fn edges_span_and_shortest_run() {
        let wave = Wave::new(POINTS);
        assert_eq!(wave.edges().count(), 4);
        assert_eq!(wave.edges().last(), Some((400, Level::Low)));
        assert_eq!(wave.span(), Some((0, 400)));
        // The first segment starts at the recording's first entry rather than
        // at a change, so the shortest bounded run is 150, not 100.
        assert_eq!(wave.shortest_run(), Some(150));
    }

    #[test]
    fn empty_wave_has_no_span_or_run() {
        let wave = Wave::new(&[]);
        assert_eq!(wave.at(0), None);
        assert_eq!(wave.span(), None);
        assert_eq!(wave.shortest_run(), None);
        assert_eq!(wave.edges().count(), 0);
    }
}
