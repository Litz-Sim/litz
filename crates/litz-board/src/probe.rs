//! Named points in the circuit and the measurements taken from them.
//!
//! The board records every net transition while a trace is running and, with
//! analog co-simulation on, the solved voltage on each probed net at every
//! settle. A [`Channel`] is one probe's slice of that record plus the numbers
//! a scope or a test reports: edges, period, duty, pulse widths, voltage
//! range. Nothing here drives or observes a part; the board owns the record
//! and this module turns it into something a host can print or draw.

use litz_core::{Level, TICKS_PER_SEC, Tick};

/// One watched point, as the board resolved it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The name the host chose, e.g. `pwm`.
    pub name: String,
    /// The point as written, e.g. `a:GPIO8`.
    pub point: String,
    /// The net the point sits on.
    pub net: u16,
}

/// What was recorded for one probe's net, in time order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    /// Level changes, including the level the net held when recording started.
    pub digital: Vec<(Tick, Level)>,
    /// Solved voltages at each settle.
    pub volts: Vec<(Tick, f64)>,
}

/// The numbers a channel reports.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measurements {
    /// Transitions between a driven high and a driven low. A release to
    /// high impedance is not an edge.
    pub edges: u64,
    pub high_ns: u64,
    pub low_ns: u64,
    /// The shortest and longest complete high or low pulse in the window.
    pub min_pulse_ns: Option<u64>,
    pub max_pulse_ns: Option<u64>,
    /// From the rising edges: `(rising edges - 1) / (last - first)`.
    pub frequency_hz: Option<f64>,
    /// Time high over time driven, 0 through 1.
    pub duty: Option<f64>,
    pub min_volts: Option<f64>,
    pub max_volts: Option<f64>,
}

/// One probe with its record and the measurements over that record.
#[derive(Clone, Debug)]
pub struct Channel {
    pub probe: Probe,
    pub series: Series,
    pub measured: Measurements,
}

/// The measurements a series carries.
pub fn measure(series: &Series) -> Measurements {
    let mut measured = Measurements::default();
    let digital = &series.digital;

    let mut risings = 0u64;
    let mut first_rising = None;
    let mut last_rising = None;
    for pair in digital.windows(2) {
        let (earlier, before) = pair[0];
        let (later, after) = pair[1];

        // Tri-stating either side takes the net off the wire, so only a
        // change between two driven levels is an edge.
        if is_driven(before) && is_driven(after) && before != after {
            measured.edges += 1;
        }
        match before {
            Level::High => measured.high_ns += later.saturating_sub(earlier),
            Level::Low => measured.low_ns += later.saturating_sub(earlier),
            Level::HighZ => {}
        }
        if before == Level::Low && after == Level::High {
            risings += 1;
            first_rising.get_or_insert(later);
            last_rising = Some(later);
        }
    }

    // A pulse needs a transition on both sides. The first and last runs are
    // open-ended: the recording starts at the first entry and stops at the
    // last change, so neither has a bounded width.
    let mut index = 0;
    while index < digital.len() {
        let level = digital[index].1;
        let mut end = index + 1;
        while end < digital.len() && digital[end].1 == level {
            end += 1;
        }
        if index > 0 && end < digital.len() && is_driven(level) {
            let width = digital[end].0.saturating_sub(digital[index].0);
            measured.min_pulse_ns = Some(
                measured
                    .min_pulse_ns
                    .map_or(width, |shortest| shortest.min(width)),
            );
            measured.max_pulse_ns = Some(
                measured
                    .max_pulse_ns
                    .map_or(width, |longest| longest.max(width)),
            );
        }
        index = end;
    }

    if risings >= 2
        && let (Some(first), Some(last)) = (first_rising, last_rising)
    {
        let span = last.saturating_sub(first);
        if span > 0 {
            let seconds = span as f64 / TICKS_PER_SEC as f64;
            measured.frequency_hz = Some((risings - 1) as f64 / seconds);
        }
    }

    let driven = measured.high_ns + measured.low_ns;
    if driven > 0 {
        measured.duty = Some(measured.high_ns as f64 / driven as f64);
    }

    if let Some(&(_, first)) = series.volts.first() {
        let mut min = first;
        let mut max = first;
        for &(_, volts) in &series.volts[1..] {
            min = min.min(volts);
            max = max.max(volts);
        }
        measured.min_volts = Some(min);
        measured.max_volts = Some(max);
    }

    measured
}

fn is_driven(level: Level) -> bool {
    matches!(level, Level::High | Level::Low)
}

/// Build the channels for `probes` from the board's net trace and analog
/// samples. `initial` is the level every net held when recording started,
/// emitted at `start` so a channel begins with the state it actually had.
pub fn channels(
    probes: &[Probe],
    trace: &[(Tick, u16, Level)],
    analog: &[(Tick, u16, f64)],
    initial: &[Level],
    start: Tick,
) -> Vec<Channel> {
    probes
        .iter()
        .map(|probe| {
            let mut series = Series::default();
            if let Some(&level) = initial.get(usize::from(probe.net)) {
                series.digital.push((start, level));
            }
            for &(tick, net, level) in trace {
                if net == probe.net {
                    series.digital.push((tick, level));
                }
            }
            for &(tick, net, volts) in analog {
                if net == probe.net {
                    series.volts.push((tick, volts));
                }
            }
            let measured = measure(&series);
            Channel {
                probe: probe.clone(),
                series,
                measured,
            }
        })
        .collect()
}

/// The last digital sample at or before `at`, which is the level held then.
fn held_level(points: &[(Tick, Level)], at: Tick) -> Option<Level> {
    let count = points.partition_point(|&(tick, _)| tick <= at);
    count.checked_sub(1).map(|index| points[index].1)
}

/// The last analog sample at or before `at`, which is the voltage held then.
fn held_volts(points: &[(Tick, f64)], at: Tick) -> Option<f64> {
    let count = points.partition_point(|&(tick, _)| tick <= at);
    count.checked_sub(1).map(|index| points[index].1)
}

/// A CSV with one column per channel, one row per time any channel changes,
/// and values held between changes.
///
/// The first column is `time_ns`. A channel with analog samples gets a second
/// column named `<name>_volts`; a level is written `1`, `0` or `z`.
pub fn to_csv(channels: &[Channel]) -> String {
    let mut csv = String::from("time_ns");
    for channel in channels {
        csv.push(',');
        csv.push_str(&channel.probe.name);
        if !channel.series.volts.is_empty() {
            csv.push(',');
            csv.push_str(&channel.probe.name);
            csv.push_str("_volts");
        }
    }
    csv.push('\n');

    // One shared time axis is what a scope draws; each channel holds its most
    // recent sample rather than repeating every point.
    let mut times = Vec::new();
    for channel in channels {
        times.extend(channel.series.digital.iter().map(|&(tick, _)| tick));
        times.extend(channel.series.volts.iter().map(|&(tick, _)| tick));
    }
    times.sort_unstable();
    times.dedup();

    for tick in times {
        csv.push_str(&tick.to_string());
        for channel in channels {
            csv.push(',');
            if let Some(level) = held_level(&channel.series.digital, tick) {
                csv.push(match level {
                    Level::High => '1',
                    Level::Low => '0',
                    Level::HighZ => 'z',
                });
            }
            if !channel.series.volts.is_empty() {
                csv.push(',');
                if let Some(volts) = held_volts(&channel.series.volts, tick) {
                    csv.push_str(&format!("{volts:.6}"));
                }
            }
        }
        csv.push('\n');
    }

    csv
}

/// One line per channel for a terminal report, e.g.
/// `pwm: 1.000 kHz, duty 50.0%, 20 edges, pulse 500us..500us, 0.000..3.300 V`.
pub fn summary(channel: &Channel) -> String {
    let name = &channel.probe.name;
    let measured = &channel.measured;

    if measured.edges == 0 {
        let level = channel
            .series
            .digital
            .last()
            .map_or(Level::HighZ, |&(_, level)| level);
        return format!("{name}: no edges (level {})", level_char(level));
    }

    let mut parts = Vec::new();
    if let Some(hz) = measured.frequency_hz {
        parts.push(human_hz(hz));
    }
    if let Some(duty) = measured.duty {
        parts.push(format!("duty {:.1}%", duty * 100.0));
    }
    parts.push(format!("{} edges", measured.edges));
    if let (Some(shortest), Some(longest)) = (measured.min_pulse_ns, measured.max_pulse_ns) {
        parts.push(format!(
            "pulse {}..{}",
            human_duration(shortest),
            human_duration(longest)
        ));
    }
    if let (Some(min), Some(max)) = (measured.min_volts, measured.max_volts)
        && !channel.series.volts.is_empty()
    {
        parts.push(format!("{min:.3}..{max:.3} V"));
    }
    format!("{name}: {}", parts.join(", "))
}

fn level_char(level: Level) -> char {
    match level {
        Level::High => '1',
        Level::Low => '0',
        Level::HighZ => 'z',
    }
}

fn human_hz(hz: f64) -> String {
    if hz >= 1_000_000.0 {
        format!("{:.3} MHz", hz / 1_000_000.0)
    } else if hz >= 1_000.0 {
        format!("{:.3} kHz", hz / 1_000.0)
    } else {
        format!("{hz:.3} Hz")
    }
}

fn human_duration(ns: u64) -> String {
    if ns >= 1_000_000 {
        trimmed(ns as f64 / 1_000_000.0, "ms")
    } else if ns >= 1_000 {
        trimmed(ns as f64 / 1_000.0, "us")
    } else {
        format!("{ns}ns")
    }
}

/// Where a logic analyzer starts its window: the first transition of
/// `channel` to `level`, with `pre_ticks` of history kept before it and
/// `post_ticks` recorded after it (zero means "to the end of the record").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub channel: String,
    pub level: Level,
    pub pre_ticks: Tick,
    pub post_ticks: Tick,
}

impl Trigger {
    /// The first edge on the named channel to `level`, keeping `pre` before
    /// and `post` after. A channel that starts at the level and never changes
    /// triggers at its first recorded point, which is what an analyzer on an
    /// already-high line does.
    pub fn find(&self, channels: &[Channel]) -> Option<Tick> {
        let channel = channels.iter().find(|c| c.probe.name == self.channel)?;
        let mut previous = None;
        for &(at, level) in &channel.series.digital {
            if level == self.level && previous.is_some_and(|p| p != level) {
                return Some(at);
            }
            previous = Some(level);
        }
        channel
            .series
            .digital
            .iter()
            .find(|(_, level)| *level == self.level)
            .map(|(at, _)| *at)
    }
}

/// Cut every channel down to one trigger's window and measure the result
/// again. The point before the window is kept so the level at the window's
/// start is known, and a channel with no points inside the window still
/// reports the level it held there.
pub fn triggered(channels: &[Channel], trigger: &Trigger) -> Result<Vec<Channel>, String> {
    let at = trigger.find(channels).ok_or_else(|| {
        format!(
            "trigger channel {:?} never reached {}",
            trigger.channel,
            level_char(trigger.level)
        )
    })?;
    let start = at.saturating_sub(trigger.pre_ticks);
    let end = if trigger.post_ticks == 0 {
        u64::MAX
    } else {
        at.saturating_add(trigger.post_ticks)
    };
    Ok(channels
        .iter()
        .map(|channel| {
            let mut digital: Vec<(Tick, Level)> = Vec::new();
            let mut volts: Vec<(Tick, f64)> = Vec::new();
            for &(tick, level) in &channel.series.digital {
                if tick < start {
                    digital.clear();
                    digital.push((tick, level));
                } else if tick <= end {
                    digital.push((tick, level));
                }
            }
            for &(tick, value) in &channel.series.volts {
                if tick < start {
                    volts.clear();
                    volts.push((tick, value));
                } else if tick <= end {
                    volts.push((tick, value));
                }
            }
            let series = Series { digital, volts };
            Channel {
                probe: channel.probe.clone(),
                measured: measure(&series),
                series,
            }
        })
        .collect())
}

/// Three decimals are enough to keep a width recognizable, but trailing zeros
/// make a report harder to scan: 500.000us reads better as 500us.
fn trimmed(value: f64, unit: &str) -> String {
    let mut text = format!("{value:.3}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    format!("{text}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_with(name: &str, digital: &[(Tick, Level)], volts: &[(Tick, f64)]) -> Channel {
        let series = Series {
            digital: digital.to_vec(),
            volts: volts.to_vec(),
        };
        Channel {
            probe: Probe {
                name: name.into(),
                point: name.into(),
                net: 0,
            },
            measured: measure(&series),
            series,
        }
    }

    fn channel(name: &str, digital: &[(Tick, Level)]) -> Channel {
        channel_with(name, digital, &[])
    }

    #[test]
    fn a_square_wave_reports_its_period_and_duty() {
        let wave = [
            (0, Level::Low),
            (100, Level::High),
            (200, Level::Low),
            (300, Level::High),
            (400, Level::Low),
        ];
        let measured = channel("pwm", &wave).measured;
        assert_eq!(measured.edges, 4);
        assert_eq!(measured.high_ns, 200);
        assert_eq!(measured.low_ns, 200);
        assert_eq!(measured.frequency_hz, Some(5_000_000.0));
        assert_eq!(measured.duty, Some(0.5));
        assert_eq!(measured.min_pulse_ns, Some(100));
        assert_eq!(measured.max_pulse_ns, Some(100));
    }

    #[test]
    fn a_short_pulse_sets_the_pulse_range() {
        let wave = [
            (0, Level::Low),
            (100, Level::High),
            (120, Level::Low),
            (220, Level::High),
            (320, Level::Low),
        ];
        let measured = channel("narrow", &wave).measured;
        assert_eq!(measured.min_pulse_ns, Some(20));
        assert_eq!(measured.max_pulse_ns, Some(100));
    }

    #[test]
    fn a_high_z_excursion_is_not_an_edge() {
        let wave = [
            (0, Level::Low),
            (100, Level::High),
            (200, Level::HighZ),
            (300, Level::High),
            (400, Level::Low),
        ];
        let measured = channel("released", &wave).measured;
        assert_eq!(measured.edges, 2);
        assert_eq!(measured.high_ns, 200);
        assert_eq!(measured.low_ns, 100);
        // The released interval is neither high nor low time.
        assert_eq!(measured.duty, Some(200.0 / 300.0));
    }

    #[test]
    fn channels_seed_initial_state_and_filter_by_net() {
        let probes = [
            Probe {
                name: "a".into(),
                point: "a:GPIO1".into(),
                net: 0,
            },
            Probe {
                name: "b".into(),
                point: "b:GPIO2".into(),
                net: 1,
            },
        ];
        let trace = [
            (10, 0, Level::High),
            (20, 1, Level::High),
            (30, 0, Level::Low),
            (40, 1, Level::Low),
            (50, 0, Level::High),
        ];
        let analog = [(5, 0, 1.5), (6, 1, 2.5)];
        let initial = [Level::Low, Level::Low];
        let built = channels(&probes, &trace, &analog, &initial, 0);

        assert_eq!(built.len(), 2);
        assert_eq!(
            built[0].series.digital,
            vec![
                (0, Level::Low),
                (10, Level::High),
                (30, Level::Low),
                (50, Level::High)
            ]
        );
        assert_eq!(built[0].series.volts, vec![(5, 1.5)]);
        assert_eq!(
            built[1].series.digital,
            vec![(0, Level::Low), (20, Level::High), (40, Level::Low)]
        );
        assert_eq!(built[1].series.volts, vec![(6, 2.5)]);
    }

    #[test]
    fn csv_holds_values_between_changes() {
        let pwm = channel(
            "pwm",
            &[(0, Level::Low), (100, Level::High), (200, Level::Low)],
        );
        let aout = channel_with("aout", &[(300, Level::High)], &[(0, 1.5), (150, 3.3)]);
        let csv = to_csv(&[pwm, aout]);
        let expected = "\
time_ns,pwm,aout,aout_volts
0,0,,1.500000
100,1,,1.500000
150,1,,3.300000
200,0,,3.300000
300,0,1,3.300000
";
        assert_eq!(csv, expected);
    }

    #[test]
    fn csv_adds_a_volts_column_only_where_needed() {
        let pwm = channel("pwm", &[(0, Level::Low), (100, Level::High)]);
        assert_eq!(to_csv(&[pwm]), "time_ns,pwm\n0,0\n100,1\n");
    }

    #[test]
    fn summary_reports_frequency_duty_pulses_and_volts() {
        let wave = [
            (0, Level::Low),
            (500_000, Level::High),
            (1_000_000, Level::Low),
            (1_500_000, Level::High),
            (2_000_000, Level::Low),
        ];
        let pwm = channel_with("pwm", &wave, &[(0, 0.0), (1_000_000, 3.3)]);
        assert_eq!(
            summary(&pwm),
            "pwm: 1.000 kHz, duty 50.0%, 4 edges, pulse 500us..500us, 0.000..3.300 V"
        );
    }

    #[test]
    fn summary_reports_the_idle_level_without_edges() {
        let idle = channel("idle", &[(0, Level::High)]);
        assert_eq!(summary(&idle), "idle: no edges (level 1)");
        let floating = channel("floating", &[(0, Level::HighZ)]);
        assert_eq!(summary(&floating), "floating: no edges (level z)");
    }

    #[test]
    fn a_trigger_keeps_the_history_before_its_edge() {
        let clock = channel(
            "sck",
            &[
                (0, Level::Low),
                (100, Level::High),
                (200, Level::Low),
                (300, Level::High),
                (400, Level::Low),
            ],
        );
        let trigger = Trigger {
            channel: "sck".into(),
            level: Level::High,
            pre_ticks: 250,
            post_ticks: 100,
        };
        assert_eq!(trigger.find(std::slice::from_ref(&clock)), Some(100));

        let window = triggered(&[clock], &trigger).unwrap();
        let ticks: Vec<Tick> = window[0].series.digital.iter().map(|(t, _)| *t).collect();
        // The 100 ns rising edge is the trigger. 250 ns of history reaches
        // before time zero, so the record starts at the level it held there.
        assert_eq!(ticks, vec![0, 100, 200]);
        assert_eq!(window[0].measured.edges, 2);
    }

    #[test]
    fn a_trigger_that_never_fires_is_an_error() {
        let clock = channel("sck", &[(0, Level::Low), (100, Level::High)]);
        let trigger = Trigger {
            channel: "sck".into(),
            level: Level::High,
            pre_ticks: 0,
            post_ticks: 0,
        };
        assert_eq!(trigger.find(std::slice::from_ref(&clock)), Some(100));
        let wrong = Trigger {
            channel: "missing".into(),
            ..trigger
        };
        assert!(triggered(&[clock], &wrong).is_err());
    }
}
