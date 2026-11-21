//! UART decoding: 8N1, LSB first, idle high.

use litz_core::{Level, TICKS_PER_SEC, Tick};

use crate::{Frame, Wave};

/// Decode UART frames at `baud`, or at the inferred baud when `None`.
///
/// The data bits sample at 1.5, 2.5, ... bit periods after the start edge and
/// the stop bit at 9.5. Any sample that is not `High` counts as a zero data
/// bit, so a `HighZ` line reads as low rather than aborting the frame.
pub fn uart(rx: &[(Tick, Level)], baud: Option<u32>) -> Vec<Frame> {
    let Some(baud) = baud.or_else(|| crate::infer_baud(rx)) else {
        return Vec::new();
    };
    if baud == 0 {
        return Vec::new();
    }
    let bit = TICKS_PER_SEC / baud as u64;
    if bit == 0 {
        // A bit period below one tick cannot be sampled.
        return Vec::new();
    }

    let wave = Wave::new(rx);
    let mut frames = Vec::new();
    let mut next_start = Tick::MIN;

    for pair in rx.windows(2) {
        let (prev, level) = (pair[0].1, pair[1].1);
        let start = pair[1].0;
        if level != Level::Low || prev != Level::High || start < next_start {
            continue;
        }
        let frame = decode_frame(&wave, start, bit);
        next_start = frame.end;
        frames.push(frame);
    }

    frames
}

fn decode_frame(wave: &Wave<'_>, start: Tick, bit: Tick) -> Frame {
    let mut value = 0u8;
    for n in 0..8 {
        // Sample at the middle of each bit: 1.5, 2.5, ... 8.5 periods.
        let at = start.saturating_add(bit.saturating_mul(2 * n + 3) / 2);
        if wave.at(at) == Some(Level::High) {
            value |= 1 << n;
        }
    }

    let stop_at = start.saturating_add(bit.saturating_mul(19) / 2);
    let stop_high = wave.at(stop_at) == Some(Level::High);
    let error = (!stop_high).then(|| "stop bit was low".to_string());

    Frame {
        at: start,
        end: start.saturating_add(bit.saturating_mul(10)),
        bytes: vec![value],
        miso: Vec::new(),
        text: byte_text(value),
        error,
    }
}

/// Render one byte as a quoted character when it is text, else as hex.
fn byte_text(value: u8) -> String {
    let printable = (0x20..=0x7e).contains(&value) || matches!(value, b'\n' | b'\r' | b'\t');
    if !printable {
        return format!("0x{value:02x}");
    }
    let escaped = match value {
        b'"' => "\\\"".to_string(),
        b'\\' => "\\\\".to_string(),
        b'\n' => "\\n".to_string(),
        b'\r' => "\\r".to_string(),
        b'\t' => "\\t".to_string(),
        other => (other as char).to_string(),
    };
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infer_baud;

    const BIT: Tick = 1_000; // 1 Mbaud

    /// Hold `level` for `dur` ticks, collapsing a repeated level.
    fn hold(wave: &mut Vec<(Tick, Level)>, tick: &mut Tick, level: Level, dur: Tick) {
        if wave.last().map(|&(_, last)| last) != Some(level) {
            wave.push((*tick, level));
        }
        *tick += dur;
    }

    /// Append one 8N1 byte, starting from whatever the line currently holds.
    fn send(wave: &mut Vec<(Tick, Level)>, tick: &mut Tick, byte: u8, stop: Level) {
        hold(wave, tick, Level::Low, BIT);
        for n in 0..8 {
            let level = if byte >> n & 1 == 1 {
                Level::High
            } else {
                Level::Low
            };
            hold(wave, tick, level, BIT);
        }
        hold(wave, tick, stop, BIT);
    }

    fn waveform(bytes: &[u8]) -> Vec<(Tick, Level)> {
        let mut wave = vec![(0, Level::High)];
        let mut tick = BIT;
        for &byte in bytes {
            send(&mut wave, &mut tick, byte, Level::High);
        }
        wave
    }

    #[test]
    fn decodes_one_byte() {
        let wave = waveform(b"h");
        let frames = uart(&wave, Some(1_000_000));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![b'h']);
        assert_eq!(frames[0].text, "\"h\"");
        assert_eq!(frames[0].at, BIT);
        assert_eq!(frames[0].end, BIT + 10 * BIT);
        assert_eq!(frames[0].error, None);
    }

    #[test]
    fn decodes_a_string() {
        let wave = waveform(b"hi!");
        let frames = uart(&wave, Some(1_000_000));
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].text, "\"h\"");
        assert_eq!(frames[1].text, "\"i\"");
        assert_eq!(frames[2].text, "\"!\"");
        assert_eq!(frames[0].end, frames[1].at);
        assert_eq!(frames[1].end, frames[2].at);
    }

    #[test]
    fn reports_framing_error_but_keeps_byte() {
        let mut wave = vec![(0, Level::High)];
        let mut tick = BIT;
        send(&mut wave, &mut tick, 0xff, Level::Low);
        let frames = uart(&wave, Some(1_000_000));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0xff]);
        assert_eq!(frames[0].error.as_deref(), Some("stop bit was low"));
        assert_eq!(frames[0].text, "0xff");
    }

    #[test]
    fn escapes_control_and_quote_bytes() {
        assert_eq!(byte_text(b'\n'), "\"\\n\"");
        assert_eq!(byte_text(b'"'), "\"\\\"\"");
        assert_eq!(byte_text(b'\\'), "\"\\\\\"");
        assert_eq!(byte_text(0x00), "0x00");
    }

    #[test]
    fn infers_baud_from_shortest_run() {
        let wave = waveform(&[0x55, 0xaa]);
        assert_eq!(infer_baud(&wave), Some(1_000_000));
        let frames = uart(&wave, None);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].bytes, vec![0x55]);
        assert_eq!(frames[1].bytes, vec![0xaa]);
    }

    #[test]
    fn empty_and_short_input_decode_to_nothing() {
        assert!(uart(&[], None).is_empty());
        assert!(uart(&[], Some(1_000_000)).is_empty());
        assert!(uart(&[(0, Level::High)], Some(1_000_000)).is_empty());
        assert_eq!(infer_baud(&[]), None);
        assert_eq!(infer_baud(&[(0, Level::High)]), None);
    }

    #[test]
    fn duplicate_ticks_do_not_panic_or_divide_by_zero() {
        let wave = vec![(0, Level::High), (0, Level::Low), (10, Level::High)];
        assert_eq!(infer_baud(&wave), Some(100_000_000));
        let _ = uart(&wave, None);
    }
}
