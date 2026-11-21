//! Reading protocols off the channels a run already recorded.
//!
//! A decoder is not a part and does not listen to the circuit: it takes the
//! digital series of named probes — exactly what the scope draws — and turns
//! level changes back into bytes. That keeps decoding zero-load and lets a
//! host decode the same record differently (one UART at 9600, another at
//! 115200) without running the simulation again.
//!
//! A `[[decode]]` entry names probe channels, not points. The probe block says
//! where to watch; the decode block says what to read there.

use crate::config::DecodeSection;
use litz_board::probe::{Channel, Probe};
use litz_core::{Level, Tick};
use litz_decode::{Frame, SpiMode};

/// Every channel a decoder names must be a probe, so a typo is a configuration
/// error at boot rather than a decoder that silently reads nothing.
pub fn check(spec: &DecodeSection, probes: &[Probe]) -> Result<(), String> {
    spec.validate()?;
    for name in channels(spec) {
        if !probes.iter().any(|probe| probe.name == name) {
            return Err(format!(
                "decode {:?}: no probe named {name:?}; add [[probe]] \
                 name = {name:?} point = \"<board>:<GPIO>\" or pass --probe",
                spec.label()
            ));
        }
    }
    Ok(())
}

/// The channels a decoder reads, in the order the kind names them.
fn channels(spec: &DecodeSection) -> Vec<&str> {
    match spec.kind.as_str() {
        "i2c" => vec![
            spec.sda.as_deref().unwrap_or(""),
            spec.scl.as_deref().unwrap_or(""),
        ],
        "spi" => {
            let mut names = vec![
                spec.sck.as_deref().unwrap_or(""),
                spec.mosi.as_deref().unwrap_or(""),
            ];
            if let Some(miso) = spec.miso.as_deref() {
                names.push(miso);
            }
            names.push(spec.cs.as_deref().unwrap_or(""));
            names
        }
        _ => vec![spec.rx.as_deref().unwrap_or("")],
    }
}

/// The record of one channel, by probe name.
fn series<'a>(channels: &'a [Channel], name: &str) -> Result<&'a [(Tick, Level)], String> {
    channels
        .iter()
        .find(|channel| channel.probe.name == name)
        .map(|channel| channel.series.digital.as_slice())
        .ok_or_else(|| format!("decode: no recorded channel named {name:?}"))
}

/// Run one decoder over a finished scope record.
pub fn frames(spec: &DecodeSection, channels: &[Channel]) -> Result<Vec<Frame>, String> {
    spec.validate()?;
    match spec.kind.as_str() {
        "i2c" => {
            let sda = series(channels, spec.sda.as_deref().unwrap_or(""))?;
            let scl = series(channels, spec.scl.as_deref().unwrap_or(""))?;
            Ok(litz_decode::i2c(sda, scl))
        }
        "spi" => {
            let sck = series(channels, spec.sck.as_deref().unwrap_or(""))?;
            let mosi = series(channels, spec.mosi.as_deref().unwrap_or(""))?;
            let miso = spec
                .miso
                .as_deref()
                .map(|name| series(channels, name))
                .transpose()?;
            let cs = series(channels, spec.cs.as_deref().unwrap_or(""))?;
            let mode = match spec.mode.unwrap_or(0) {
                1 => SpiMode::Mode1,
                2 => SpiMode::Mode2,
                3 => SpiMode::Mode3,
                _ => SpiMode::Mode0,
            };
            Ok(litz_decode::spi(sck, mosi, miso, cs, mode))
        }
        _ => {
            let rx = series(channels, spec.rx.as_deref().unwrap_or(""))?;
            Ok(litz_decode::uart(rx, spec.baud))
        }
    }
}

/// One report line per decoded frame, ready for the `litz: ` prefix.
pub fn report<'a>(
    specs: impl IntoIterator<Item = &'a DecodeSection>,
    channels: &[Channel],
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for spec in specs {
        let decoded = frames(spec, channels)?;
        if decoded.is_empty() {
            lines.push(format!("{}: no frames", spec.label()));
        }
        for frame in decoded {
            let mut line = format!("{}: {}", spec.label(), frame.text);
            if let Some(error) = &frame.error {
                line.push_str(&format!(" ({error})"));
            }
            lines.push(line);
        }
    }
    Ok(lines)
}
