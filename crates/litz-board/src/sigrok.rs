//! Sigrok session files, the `.sr` archive PulseView and `sigrok-cli` open.
//!
//! A session is a zip with three entries: `version` holding `2`, a `metadata`
//! INI that names the capture file and its channels, and the capture itself.
//! The capture is unframed: one little-endian sample per tick of the
//! samplerate, bit `n` of the sample being channel `n + 1`.
//!
//! The record here is edge-timed, not sampled, so the writer picks a sampling
//! instant and holds each channel at its last transition. That is what a real
//! logic analyzer with an asynchronous sampler sees of the same pins.

use crate::probe::Channel;
use litz_core::{Level, TICKS_PER_SEC};

/// The samplerate used when the caller does not choose one: fast enough to
/// oversample a 115 200 baud UART about 17 times per bit, and small enough
/// that a ten-second capture of eight channels is a few tens of megabytes.
pub const DEFAULT_RATE_HZ: u64 = 2_000_000;

/// The most samples one archive may hold. A capture larger than this is a
/// rate or window the caller should narrow rather than a file worth writing.
const MAX_SAMPLES: u64 = 100_000_000;

/// Write every probe channel as a sigrok session archive.
///
/// A channel that is high impedance has no logic level, so it is recorded as
/// low; a probe whose net was never driven reads the same way.
pub fn to_sr(channels: &[Channel], rate_hz: u64) -> Result<Vec<u8>, String> {
    if channels.is_empty() {
        return Err("a sigrok session needs at least one probe channel".into());
    }
    if rate_hz == 0 {
        return Err("the samplerate must be at least 1 Hz".into());
    }
    let start = channels
        .iter()
        .filter_map(|channel| channel.series.digital.first().map(|&(at, _)| at))
        .min()
        .ok_or("the probes recorded nothing to export")?;
    let end = channels
        .iter()
        .filter_map(|channel| channel.series.digital.last().map(|&(at, _)| at))
        .max()
        .unwrap_or(start);
    let period = (TICKS_PER_SEC / rate_hz).max(1);
    let samples = (end.saturating_sub(start) / period).saturating_add(1);
    if samples > MAX_SAMPLES {
        return Err(format!(
            "{samples} samples at {rate_hz} Hz is more than one archive should hold; \
             raise the samplerate or start the scope later"
        ));
    }

    let unitsize = channels.len().div_ceil(8);
    let mut capture = Vec::with_capacity((samples as usize).saturating_mul(unitsize));
    let mut cursors = vec![0usize; channels.len()];
    let mut levels: Vec<Level> = channels
        .iter()
        .map(|channel| {
            channel
                .series
                .digital
                .first()
                .map(|&(_, level)| level)
                .unwrap_or(Level::Low)
        })
        .collect();
    for index in 0..samples {
        let at = start.saturating_add(index.saturating_mul(period));
        for (channel_index, channel) in channels.iter().enumerate() {
            let transitions = &channel.series.digital;
            while let Some(&(when, level)) = transitions.get(cursors[channel_index]) {
                if when > at {
                    break;
                }
                // A floating line has no logic level; hold the last one it
                // had rather than inventing an edge.
                if level != Level::HighZ {
                    levels[channel_index] = level;
                }
                cursors[channel_index] += 1;
            }
        }
        let mut sample = vec![0u8; unitsize];
        for (channel_index, level) in levels.iter().enumerate() {
            if *level == Level::High {
                sample[channel_index / 8] |= 1 << (channel_index % 8);
            }
        }
        capture.extend_from_slice(&sample);
    }

    let metadata = metadata(channels, rate_hz, unitsize);
    let mut zip = Zip::new();
    zip.add("version", b"2");
    zip.add("metadata", metadata.as_bytes());
    zip.add("logic-1", &capture);
    Ok(zip.finish())
}

fn metadata(channels: &[Channel], rate_hz: u64, unitsize: usize) -> String {
    let mut text = String::new();
    text.push_str("[global]\nsigrok version=0.5.3\n\n[device 1]\n");
    text.push_str("capturefile=logic-1\n");
    text.push_str(&format!("total probes={}\n", channels.len()));
    text.push_str(&format!("samplerate={rate_hz}\n"));
    for (index, channel) in channels.iter().enumerate() {
        text.push_str(&format!("probe{}={}\n", index + 1, channel.probe.name));
    }
    text.push_str(&format!("unitsize={unitsize}\n"));
    text
}

/// A zip writer for the three stored entries a session needs.
///
/// Everything is stored, not deflated: a `.sr` is small enough that a
/// dependency-free writer is worth more than the saved bytes, and the archive
/// is byte-for-byte deterministic, which is what lets a test read it.
struct Zip {
    bytes: Vec<u8>,
    entries: Vec<(String, u32, usize, u32)>,
}

impl Zip {
    fn new() -> Self {
        Zip {
            bytes: Vec::new(),
            entries: Vec::new(),
        }
    }

    fn add(&mut self, name: &str, data: &[u8]) {
        let offset = self.bytes.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        self.bytes.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        self.bytes.extend_from_slice(&20u16.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(&0x0021u16.to_le_bytes());
        self.bytes.extend_from_slice(&crc.to_le_bytes());
        self.bytes.extend_from_slice(&size.to_le_bytes());
        self.bytes.extend_from_slice(&size.to_le_bytes());
        self.bytes
            .extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.extend_from_slice(data);
        self.entries
            .push((name.to_string(), crc, data.len(), offset));
    }

    fn finish(mut self) -> Vec<u8> {
        let directory_start = self.bytes.len() as u32;
        for (name, crc, len, offset) in &self.entries {
            self.bytes.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            self.bytes.extend_from_slice(&20u16.to_le_bytes());
            self.bytes.extend_from_slice(&20u16.to_le_bytes());
            self.bytes.extend_from_slice(&0u16.to_le_bytes());
            self.bytes.extend_from_slice(&0u16.to_le_bytes());
            self.bytes.extend_from_slice(&0u16.to_le_bytes());
            self.bytes.extend_from_slice(&0x0021u16.to_le_bytes());
            self.bytes.extend_from_slice(&crc.to_le_bytes());
            self.bytes.extend_from_slice(&(*len as u32).to_le_bytes());
            self.bytes.extend_from_slice(&(*len as u32).to_le_bytes());
            self.bytes
                .extend_from_slice(&(name.len() as u16).to_le_bytes());
            self.bytes.extend_from_slice(&0u16.to_le_bytes());
            self.bytes.extend_from_slice(&0u16.to_le_bytes());
            self.bytes.extend_from_slice(&0u16.to_le_bytes());
            self.bytes.extend_from_slice(&0u16.to_le_bytes());
            self.bytes.extend_from_slice(&0u32.to_le_bytes());
            self.bytes.extend_from_slice(&offset.to_le_bytes());
            self.bytes.extend_from_slice(name.as_bytes());
        }
        let directory_len = self.bytes.len() as u32 - directory_start;
        self.bytes.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes
            .extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        self.bytes
            .extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        self.bytes.extend_from_slice(&directory_len.to_le_bytes());
        self.bytes.extend_from_slice(&directory_start.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes
    }
}

/// The zip CRC-32 (polynomial 0xedb88320, reflected).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{Probe, Series};

    fn channel(name: &str, digital: Vec<(u64, Level)>) -> Channel {
        Channel {
            probe: Probe {
                name: name.into(),
                point: "a:GPIO4".into(),
                net: 0,
            },
            series: Series {
                digital,
                volts: Vec::new(),
            },
            measured: Default::default(),
        }
    }

    /// Read one stored entry out of an archive the writer produced, so the
    /// test checks the format rather than the struct.
    fn entry<'a>(archive: &'a [u8], want: &str) -> &'a [u8] {
        let mut at = 0usize;
        while at + 30 <= archive.len() {
            let signature = u32::from_le_bytes(archive[at..at + 4].try_into().unwrap());
            if signature != 0x0403_4b50 {
                break;
            }
            let size = u32::from_le_bytes(archive[at + 18..at + 22].try_into().unwrap()) as usize;
            let name_len =
                u16::from_le_bytes(archive[at + 26..at + 28].try_into().unwrap()) as usize;
            let extra = u16::from_le_bytes(archive[at + 28..at + 30].try_into().unwrap()) as usize;
            let name = std::str::from_utf8(&archive[at + 30..at + 30 + name_len]).unwrap();
            let start = at + 30 + name_len + extra;
            if name == want {
                return &archive[start..start + size];
            }
            at = start + size;
        }
        panic!("the archive has no {want:?} entry");
    }

    #[test]
    fn a_session_holds_the_three_entries_a_viewer_opens() {
        let channels = vec![channel("tx", vec![(0, Level::High), (10, Level::Low)])];
        let archive = to_sr(&channels, 1_000_000_000).expect("the session writes");
        assert_eq!(entry(&archive, "version"), b"2");
        let metadata = String::from_utf8(entry(&archive, "metadata").to_vec()).unwrap();
        assert!(metadata.contains("capturefile=logic-1\n"), "{metadata}");
        assert!(metadata.contains("total probes=1\n"), "{metadata}");
        assert!(metadata.contains("samplerate=1000000000\n"), "{metadata}");
        assert!(metadata.contains("probe1=tx\n"), "{metadata}");
        // One sample per simulated nanosecond, high for the first ten.
        let capture = entry(&archive, "logic-1");
        assert_eq!(capture.len(), 11);
        assert_eq!(&capture[..10], &[1u8; 10]);
        assert_eq!(capture[10], 0);
    }

    #[test]
    fn more_than_eight_probes_use_a_wider_sample() {
        let channels: Vec<Channel> = (0..9)
            .map(|index| {
                channel(
                    &format!("p{index}"),
                    vec![(0, if index == 8 { Level::High } else { Level::Low })],
                )
            })
            .collect();
        let archive = to_sr(&channels, 1_000_000_000).unwrap();
        let metadata = String::from_utf8(entry(&archive, "metadata").to_vec()).unwrap();
        assert!(metadata.contains("unitsize=2\n"), "{metadata}");
        // The ninth probe is bit 0 of the second byte.
        let capture = entry(&archive, "logic-1");
        assert_eq!(capture.len(), 2);
        assert_eq!(capture, &[0x00, 0x01]);
    }

    #[test]
    fn a_high_impedance_line_reads_low() {
        let channels = vec![channel("floating", vec![(0, Level::HighZ)])];
        let archive = to_sr(&channels, 1_000_000_000).unwrap();
        assert_eq!(entry(&archive, "logic-1"), &[0]);
    }

    #[test]
    fn an_archive_without_channels_is_refused() {
        assert!(to_sr(&[], 1_000_000).is_err());
    }
}
