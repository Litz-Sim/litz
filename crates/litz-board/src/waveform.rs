//! Pin-level edges for a bus transaction, for the probe record.
//!
//! A board's bus fabric exchanges bytes; a scope shows wires. This module
//! turns one into the other without touching the netlist: the edges are
//! returned to the caller, which appends them to the observation record.
//!
//! Nothing here drives a net. The caller already performed the transaction
//! through [`Board::i2c_write_read`](crate::Board::i2c_write_read),
//! [`Board::spi_transfer`](crate::Board::spi_transfer) or
//! [`Board::uart_receive`](crate::Board::uart_receive); these functions only
//! reconstruct what a probe on the bus pins would have seen. Keeping them
//! separate means a decoder or a scope can be tested against a byte stream
//! without a board, a netlist, or a driver.

use litz_core::{Level, TICKS_PER_SEC, Tick};

/// One observed pin transition. `at` is a tick; the pin is a chip GPIO
/// number on the board that owns the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub at: Tick,
    pub pin: u8,
    pub level: Level,
}

/// Record a transition only when it is one.
///
/// Every generator below tracks the level each line already holds, so a line
/// that stays put contributes no edge. This is also what keeps a pin's edges
/// free of adjacent duplicates once the streams are merged.
fn pulse(edges: &mut Vec<Edge>, current: &mut Level, pin: u8, at: Tick, level: Level) {
    if *current != level {
        *current = level;
        edges.push(Edge { at, pin, level });
    }
}

/// Bit `bit` of `byte`, counting from the least significant bit.
fn bit_level(byte: u8, bit: u8) -> Level {
    if (byte >> bit) & 1 == 0 {
        Level::Low
    } else {
        Level::High
    }
}

/// 8N1 UART: idle high, start low, eight data bits LSB first, stop high.
/// `tx` is clocked on `tx_pin`; `rx` (bytes the part sent) on `rx_pin` when
/// one is given. Both start at `at` and run at `baud`.
pub fn uart(
    tx_pin: u8,
    rx_pin: Option<u8>,
    at: Tick,
    baud: u32,
    tx: &[u8],
    rx: &[u8],
) -> Vec<Edge> {
    if baud == 0 {
        return Vec::new();
    }
    let bit = (TICKS_PER_SEC / u64::from(baud)).max(1);
    let mut edges = Vec::new();

    uart_line(&mut edges, tx_pin, at, bit, tx);
    if let Some(pin) = rx_pin {
        uart_line(&mut edges, pin, at, bit, rx);
    }

    // The two directions are independent; merge their clocks into one record.
    edges.sort_by_key(|edge| edge.at);
    edges
}

/// One direction of a UART, from an idle-high line.
fn uart_line(edges: &mut Vec<Edge>, pin: u8, at: Tick, bit: Tick, bytes: &[u8]) {
    let mut level = Level::High;
    let frame = bit.saturating_mul(10);
    let mut start = at;

    for &byte in bytes {
        pulse(edges, &mut level, pin, start, Level::Low);
        for index in 0..8u8 {
            let offset = bit.saturating_mul(u64::from(index) + 1);
            let level_now = bit_level(byte, index);
            pulse(
                edges,
                &mut level,
                pin,
                start.saturating_add(offset),
                level_now,
            );
        }
        // The stop bit is the tenth bit time. It is high, so a line that ends
        // here simply stays high.
        let stop = bit.saturating_mul(9);
        pulse(
            edges,
            &mut level,
            pin,
            start.saturating_add(stop),
            Level::High,
        );
        start = start.saturating_add(frame);
    }
}

/// I2C at `hz`: START, address+write, `write` bytes, repeated START,
/// address+read, `read` bytes, STOP. `SDA` changes while `SCL` is low; the
/// receiver samples on the rising edge.
pub fn i2c(
    sda_pin: u8,
    scl_pin: u8,
    at: Tick,
    hz: u32,
    address: u8,
    write: &[u8],
    read: &[u8],
) -> Vec<Edge> {
    if hz == 0 || (write.is_empty() && read.is_empty()) {
        return Vec::new();
    }

    let period = (TICKS_PER_SEC / u64::from(hz)).max(1);
    let half = (period / 2).max(1);
    let quarter = (period / 4).max(1);

    let mut wires = I2cWires {
        edges: Vec::new(),
        sda_pin,
        scl_pin,
        sda: Level::High,
        scl: Level::High,
    };
    let mut now = at;

    // START: SDA falls while SCL is still high.
    wires.set_sda(now, Level::Low);
    now = now.saturating_add(quarter);

    if !write.is_empty() {
        wires.clock_byte(&mut now, address << 1, Level::Low, half);
        for &byte in write {
            wires.clock_byte(&mut now, byte, Level::Low, half);
        }
    }

    if !read.is_empty() {
        if !write.is_empty() {
            // Repeated START: release SDA, then fall again with SCL high.
            wires.set_sda(now, Level::High);
            now = now.saturating_add(quarter);
            wires.set_sda(now, Level::Low);
            now = now.saturating_add(quarter);
        }

        // The device acknowledges its address; the master acknowledges every
        // read byte but the last, which it nacks so the slave releases SDA.
        wires.clock_byte(&mut now, (address << 1) | 1, Level::Low, half);
        for (index, &byte) in read.iter().enumerate() {
            let ack = if index + 1 == read.len() {
                Level::High
            } else {
                Level::Low
            };
            wires.clock_byte(&mut now, byte, ack, half);
        }
    }

    // STOP is SDA rising while SCL is high. After a write the acknowledge
    // left SDA low, so it can rise straight away. After a read the master
    // nacked with SDA high, so SDA has to be taken low first; that needs one
    // more clock pulse to raise SCL before the release.
    if wires.sda == Level::Low {
        wires.set_sda(now, Level::High);
    } else {
        wires.set_scl(now, Level::Low);
        wires.set_sda(now, Level::Low);
        now = now.saturating_add(quarter);
        wires.set_scl(now, Level::High);
        now = now.saturating_add(quarter);
        wires.set_sda(now, Level::High);
    }

    wires.edges
}

/// The two I2C lines and the levels they currently hold.
struct I2cWires {
    edges: Vec<Edge>,
    sda_pin: u8,
    scl_pin: u8,
    sda: Level,
    scl: Level,
}

impl I2cWires {
    fn set_sda(&mut self, at: Tick, level: Level) {
        pulse(&mut self.edges, &mut self.sda, self.sda_pin, at, level);
    }

    fn set_scl(&mut self, at: Tick, level: Level) {
        pulse(&mut self.edges, &mut self.scl, self.scl_pin, at, level);
    }

    /// Clock out eight data bits (most significant first) and the ninth
    /// acknowledge bit. Entered and left with SCL high. Data is placed while
    /// SCL is low, so the receiver sees it stable on the rising edge.
    fn clock_byte(&mut self, now: &mut Tick, byte: u8, ack: Level, half: Tick) {
        for bit in (0..8u8).rev() {
            self.set_scl(*now, Level::Low);
            self.set_sda(*now, bit_level(byte, bit));
            *now = (*now).saturating_add(half);
            self.set_scl(*now, Level::High);
            *now = (*now).saturating_add(half);
        }

        self.set_scl(*now, Level::Low);
        self.set_sda(*now, ack);
        *now = (*now).saturating_add(half);
        self.set_scl(*now, Level::High);
        *now = (*now).saturating_add(half);
    }
}

/// SPI mode 0..=3 (CPOL, CPHA). CS is active low; MOSI carries `tx`, MISO
/// carries `rx`. The host samples on the rising edge for modes 0 and 3 and
/// on the falling edge for 1 and 2. The clock spends half a period on each
/// level. Every frame has eight clocks per byte; missing `tx` bytes are 0xFF.
#[allow(clippy::too_many_arguments)]
pub fn spi(
    sck_pin: u8,
    mosi_pin: Option<u8>,
    miso_pin: Option<u8>,
    cs_pin: u8,
    at: Tick,
    hz: u32,
    mode: u8,
    tx: &[u8],
    rx: &[u8],
) -> Vec<Edge> {
    if hz == 0 {
        return Vec::new();
    }

    let period = (TICKS_PER_SEC / u64::from(hz)).max(1);
    let half = (period / 2).max(1);

    // Only the two CPOL/CPHA bits matter; anything above is ignored rather
    // than allowed to index out of range.
    let mode = mode & 3;
    let idle = if mode & 2 != 0 {
        Level::High
    } else {
        Level::Low
    };
    // CPHA=0 (modes 0 and 2) samples on the first edge, CPHA=1 on the second.
    // In terms of direction: modes 0 and 3 sample on the rising edge.
    let sample_rising = matches!(mode, 0 | 3);
    let setup = if sample_rising {
        Level::Low
    } else {
        Level::High
    };
    let sample = if sample_rising {
        Level::High
    } else {
        Level::Low
    };

    let bytes = tx.len().max(rx.len());
    let bits = bytes.saturating_mul(8);

    let mut edges = Vec::new();
    let mut cs = Level::High;
    let mut sck = idle;
    let mut mosi = Level::Low;
    let mut miso = Level::Low;
    let mut now = at;

    pulse(&mut edges, &mut cs, cs_pin, now, Level::Low);

    // Modes 0 and 2 idle at the setup level, so the first byte's data is
    // placed at `at` without a clock edge. Modes 1 and 3 idle at the sample
    // level and need a setup edge first.
    let starts_at_setup = sck == setup;
    if starts_at_setup && bits > 0 {
        spi_data(
            &mut edges, mosi_pin, &mut mosi, miso_pin, &mut miso, tx, rx, 0, now,
        );
    }

    for bit in 0..bits {
        if !(starts_at_setup && bit == 0) {
            pulse(&mut edges, &mut sck, sck_pin, now, setup);
            spi_data(
                &mut edges, mosi_pin, &mut mosi, miso_pin, &mut miso, tx, rx, bit, now,
            );
        }

        let sample_at = now.saturating_add(half);
        pulse(&mut edges, &mut sck, sck_pin, sample_at, sample);
        now = now.saturating_add(period);
    }

    // A mode 0 or 2 frame ends on the sample level; the last half clock is
    // the trailing edge back to idle.
    if starts_at_setup && bits > 0 {
        pulse(&mut edges, &mut sck, sck_pin, now, setup);
    }

    pulse(&mut edges, &mut cs, cs_pin, now, Level::High);
    edges
}

/// Place the MOSI and MISO levels for one bit at the setup boundary.
///
/// MOSI shifts out 0xFF past the end of `tx`: that is what a host with no
/// more bytes on hand drives. MISO is left alone past the end of `rx` because
/// a byte the part did not send has no line state to observe.
#[allow(clippy::too_many_arguments)]
fn spi_data(
    edges: &mut Vec<Edge>,
    mosi_pin: Option<u8>,
    mosi: &mut Level,
    miso_pin: Option<u8>,
    miso: &mut Level,
    tx: &[u8],
    rx: &[u8],
    bit: usize,
    at: Tick,
) {
    let byte = bit / 8;
    let offset = (7 - bit % 8) as u8;

    if let Some(pin) = mosi_pin {
        let level = match tx.get(byte) {
            Some(&value) => bit_level(value, offset),
            None => Level::High,
        };
        pulse(edges, mosi, pin, at, level);
    }

    if let Some(pin) = miso_pin
        && let Some(&value) = rx.get(byte)
    {
        pulse(edges, miso, pin, at, bit_level(value, offset));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn levels(edges: &[Edge], pin: u8) -> Vec<(Tick, Level)> {
        edges
            .iter()
            .filter(|edge| edge.pin == pin)
            .map(|edge| (edge.at, edge.level))
            .collect()
    }

    /// The level a line holds at `at`, given the level it idles at.
    fn level_at(edges: &[Edge], pin: u8, at: Tick, idle: Level) -> Level {
        edges
            .iter()
            .rfind(|edge| edge.pin == pin && edge.at <= at)
            .map(|edge| edge.level)
            .unwrap_or(idle)
    }

    #[test]
    fn uart_a_at_9600() {
        let bit = TICKS_PER_SEC / 9600;
        let at = 1_000;
        let edges = uart(4, None, at, 9600, b"A", &[]);
        // 'A' is 0x41: LSB first that is 1 0 0 0 0 0 1 0, so only four of the
        // eight data bit times change the line.
        assert_eq!(
            levels(&edges, 4),
            vec![
                (at, Level::Low),
                (at + bit, Level::High),
                (at + 2 * bit, Level::Low),
                (at + 7 * bit, Level::High),
                (at + 8 * bit, Level::Low),
                (at + 9 * bit, Level::High),
            ]
        );
    }

    #[test]
    fn uart_two_bytes_and_independent_directions() {
        let bit = TICKS_PER_SEC / 9600;

        let edges = uart(4, None, 0, 9600, &[0x00, 0x00], &[]);
        assert_eq!(
            levels(&edges, 4),
            vec![
                (0, Level::Low),
                (9 * bit, Level::High),
                (10 * bit, Level::Low),
                (19 * bit, Level::High),
            ]
        );

        // The receive line keeps clocking after the shorter transmit line has
        // finished.
        let edges = uart(4, Some(5), 0, 9600, &[0x00], &[0x00, 0x00]);
        let tx = levels(&edges, 4);
        let rx = levels(&edges, 5);
        assert_eq!(tx[0], (0, Level::Low));
        assert_eq!(rx[0], (0, Level::Low));
        assert_eq!(tx.last().unwrap().0, 9 * bit);
        assert_eq!(rx.last().unwrap().0, 19 * bit);
    }

    #[test]
    fn uart_zero_baud_is_empty() {
        assert!(uart(4, Some(5), 0, 0, &[0x41], &[0x42]).is_empty());
    }

    #[test]
    fn i2c_write_read_shape() {
        let hz = 100_000;
        let period = TICKS_PER_SEC / u64::from(hz);
        let half = period / 2;
        let quarter = period / 4;
        let at = 500;
        let (sda, scl) = (6, 7);
        let edges = i2c(sda, scl, at, hz, 0x3c, &[0x00, 0xaf], &[0x5a]);

        // START is SDA falling while SCL is still high.
        assert_eq!(levels(&edges, sda)[0], (at, Level::Low));
        assert_eq!(level_at(&edges, scl, at, Level::High), Level::High);

        let rising: Vec<Tick> = edges
            .iter()
            .filter(|edge| edge.pin == scl && edge.level == Level::High)
            .map(|edge| edge.at)
            .collect();

        // Address+W, two write bytes, address+R, one read byte: five bytes,
        // nine clocks each. The extra rising edge is the stop setup pulse.
        let data_clocks = 5 * 9;
        assert_eq!(rising.len(), data_clocks + 1);
        assert_eq!(rising[0], at + quarter + half);

        // The write phase clocks evenly, then the repeated START stretches the
        // gap, then the read phase clocks evenly again.
        for index in 1..3 * 9 {
            assert_eq!(rising[index] - rising[index - 1], period);
        }
        assert_eq!(rising[3 * 9] - rising[3 * 9 - 1], half + 2 * quarter + half);
        for index in 3 * 9 + 1..data_clocks {
            assert_eq!(rising[index] - rising[index - 1], period);
        }

        // Each byte's ninth clock samples its acknowledge bit: low for the
        // address and write acks, low for the read address, high for the last
        // read byte's nack.
        let ack = |index: usize| level_at(&edges, sda, rising[index], Level::High);
        assert_eq!(ack(8), Level::Low);
        assert_eq!(ack(17), Level::Low);
        assert_eq!(ack(26), Level::Low);
        assert_eq!(ack(35), Level::Low);
        assert_eq!(ack(44), Level::High);

        // The repeated START is the one SDA rise while SCL is high that is not
        // the final STOP.
        let sda_edges = levels(&edges, sda);
        let stop = *sda_edges.last().unwrap();
        assert_eq!(stop.1, Level::High);
        assert_eq!(level_at(&edges, scl, stop.0, Level::High), Level::High);
        let repeated = sda_edges
            .iter()
            .filter(|(t, level)| {
                *level == Level::High
                    && *t != stop.0
                    && level_at(&edges, scl, *t, Level::High) == Level::High
            })
            .count();
        assert_eq!(repeated, 1);
    }

    #[test]
    fn i2c_zero_hz_and_empty_transaction_are_empty() {
        assert!(i2c(6, 7, 0, 0, 0x3c, &[0x00], &[0x00]).is_empty());
        assert!(i2c(6, 7, 0, 100_000, 0x3c, &[], &[]).is_empty());
    }

    #[test]
    fn i2c_pure_write_ends_with_stop_and_no_read_clock() {
        let edges = i2c(6, 7, 0, 100_000, 0x3c, &[0x00], &[]);
        // Address+W and one data byte, nine clocks each; no read phase and no
        // extra stop setup clock, because the write ack left SDA low.
        let rising = edges
            .iter()
            .filter(|edge| edge.pin == 7 && edge.level == Level::High)
            .count();
        assert_eq!(rising, 2 * 9);
        let last = *edges.last().unwrap();
        assert_eq!((last.pin, last.level), (6, Level::High));
    }

    #[test]
    fn spi_mode0_and_mode3_polarity() {
        let hz = 1_000_000;
        let period = TICKS_PER_SEC / u64::from(hz);
        let half = period / 2;

        // Mode 0 idles low and samples on the rising edge, so the first clock
        // edge is a rise at the middle of the first bit.
        let edges = spi(2, Some(3), Some(4), 5, 0, hz, 0, &[0x00], &[0x00]);
        let sck = levels(&edges, 2);
        assert_eq!(sck[0], (half, Level::High));
        assert_eq!(
            sck.iter()
                .filter(|(_, level)| *level == Level::High)
                .count(),
            8
        );
        assert_eq!(
            levels(&edges, 5),
            vec![(0, Level::Low), (8 * period, Level::High)]
        );

        // Mode 3 idles high and samples on the rising edge, so the first edge
        // is a fall to the setup level.
        let edges = spi(2, Some(3), Some(4), 5, 0, hz, 3, &[0x00], &[0x00]);
        let sck = levels(&edges, 2);
        assert_eq!(sck[0], (0, Level::Low));
        assert_eq!(
            sck.iter()
                .filter(|(_, level)| *level == Level::High)
                .count(),
            8
        );
        assert_eq!(
            levels(&edges, 5),
            vec![(0, Level::Low), (8 * period, Level::High)]
        );
    }

    #[test]
    fn spi_eight_clocks_per_byte() {
        let hz = 1_000_000;
        let period = TICKS_PER_SEC / u64::from(hz);
        let edges = spi(
            2,
            Some(3),
            Some(4),
            5,
            0,
            hz,
            0,
            &[0x00, 0x00],
            &[0x00, 0x00],
        );
        assert_eq!(
            levels(&edges, 2)
                .iter()
                .filter(|(_, level)| *level == Level::High)
                .count(),
            16
        );
        assert_eq!(
            levels(&edges, 5),
            vec![(0, Level::Low), (16 * period, Level::High)]
        );
    }

    #[test]
    fn spi_missing_tx_is_0xff_and_missing_rx_is_not_emitted() {
        let hz = 1_000_000;

        // No transmit bytes, two receive bytes: MOSI shifts out 0xFF, which
        // rises once and then stays high.
        let edges = spi(2, Some(3), None, 5, 0, hz, 0, &[], &[0x00, 0x00]);
        assert_eq!(levels(&edges, 3), vec![(0, Level::High)]);

        // One receive byte, two transmit bytes: the second MISO byte was not
        // sent, so no MISO edge appears for it.
        let edges = spi(2, None, Some(4), 5, 0, hz, 0, &[0x00, 0x00], &[0x00]);
        assert!(levels(&edges, 4).is_empty());
    }

    #[test]
    fn spi_zero_hz_is_empty() {
        assert!(spi(2, Some(3), Some(4), 5, 0, 0, 0, &[0x00], &[0x00]).is_empty());
    }

    #[test]
    fn edge_lists_are_sorted_and_have_no_adjacent_duplicates() {
        let streams = [
            uart(4, Some(5), 1, 115_200, &[0x41, 0x42], &[0x00]),
            i2c(6, 7, 3, 400_000, 0x3c, &[0x01], &[0x02]),
            spi(
                2,
                Some(3),
                Some(4),
                5,
                7,
                2_000_000,
                2,
                &[0xde, 0xad],
                &[0x00, 0xff],
            ),
        ];

        for edges in &streams {
            for pair in edges.windows(2) {
                assert!(pair[0].at <= pair[1].at);
            }

            let mut last: BTreeMap<u8, Level> = BTreeMap::new();
            for edge in edges {
                if let Some(previous) = last.get(&edge.pin) {
                    assert_ne!(*previous, edge.level);
                }
                last.insert(edge.pin, edge.level);
            }
        }
    }
}
