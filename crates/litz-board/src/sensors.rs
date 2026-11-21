//! Sensors and their little protocols: NEC infrared frames, flame and gas
//! modules, a heartbeat waveform, the HX711 load-cell ADC and a NEO-6M GPS.
//!
//! The IR receiver decodes pulse distances on its data line; the heartbeat
//! sensor and HX711 schedule their own conversions; the GPS emits NMEA on its
//! UART at a fixed rate. Every delay is measured from the [`Tick`] the board
//! passes in, never from wall time.

use crate::BuildError;
use crate::bus::UartPeer;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, TICKS_PER_SEC, Tick};

const LOGIC_HIGH_VOLTS: f32 = 3.3;

fn index(part: &FlatPart, name: &str) -> Result<u16, BuildError> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {name:?} pin", part.id)))
}

fn attr_f32(attrs: &std::collections::BTreeMap<String, String>, name: &str, default: f32) -> f32 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

fn attr_f64(attrs: &std::collections::BTreeMap<String, String>, name: &str, default: f64) -> f64 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

fn attr_bool(
    attrs: &std::collections::BTreeMap<String, String>,
    name: &str,
    default: bool,
) -> bool {
    attrs
        .get(name)
        .map(|text| matches!(text.trim(), "true" | "1" | "yes"))
        .unwrap_or(default)
}

fn strong(level: Level) -> Drive {
    match level {
        Level::HighZ => Drive::HIGH_Z,
        level => Drive::strong(level),
    }
}

// ------------------------------------------------------------------ IR

/// One decoded NEC frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NecFrame {
    pub address: u8,
    pub command: u8,
    /// The four bytes as received, LSB-first after the leader.
    pub raw: u32,
}

/// A 38 kHz infrared receiver that decodes NEC frames from its data line.
///
/// A receiver module has already stripped the 38 kHz carrier, so its data line
/// is an active-low pulse train: a 9 ms mark and 4.5 ms space open a frame,
/// then each bit is a 562.5 us mark followed by a 562.5 us (zero) or 1687.5 us
/// (one) space. The model measures the level that just ended on every edge and
/// assembles the 32 bits LSB-first. A partial frame is discarded after 250 ms
/// of silence, which is the [`Part::next_event`] deadline.
pub struct IrReceiver {
    dat: u16,
    last_edge: Option<(Tick, Level)>,
    leader: bool,
    in_frame: bool,
    bits: u8,
    word: u32,
    last_frame: Option<NecFrame>,
    frames: u32,
    timeout_at: Option<Tick>,
}

const IR_TIMEOUT_NS: Tick = 250_000_000;
const NEC_LEADER_MARK: Tick = 9_000_000;
const NEC_STOP_SPACE: Tick = 3_000_000;
const NEC_ONE_SPACE: Tick = 1_000_000;

impl IrReceiver {
    pub fn new() -> Self {
        Self {
            dat: 0,
            last_edge: None,
            leader: false,
            in_frame: false,
            bits: 0,
            word: 0,
            last_frame: None,
            frames: 0,
            timeout_at: None,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            dat: index(part, "DAT")?,
            ..Self::new()
        }))
    }

    /// The last decoded command as `address << 8 | command`.
    pub fn last_code(&self) -> Option<u32> {
        self.last_frame
            .map(|frame| u32::from(frame.address) << 8 | u32::from(frame.command))
    }

    pub fn last_frame(&self) -> Option<NecFrame> {
        self.last_frame
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    fn finish(&mut self) {
        if self.bits == 32 {
            let frame = NecFrame {
                address: self.word as u8,
                command: (self.word >> 16) as u8,
                raw: self.word,
            };
            self.last_frame = Some(frame);
            self.frames += 1;
        }
        self.in_frame = false;
        self.leader = false;
        self.bits = 0;
        self.word = 0;
    }

    fn edge(&mut self, ended: Level, duration: Tick) {
        match ended {
            Level::Low => {
                // A mark. Nine milliseconds is the NEC leader; anything else
                // is either a bit mark or the repeat frame's short mark.
                if (NEC_LEADER_MARK - 1_000_000..=NEC_LEADER_MARK + 1_000_000).contains(&duration) {
                    self.leader = true;
                    self.in_frame = true;
                    self.bits = 0;
                    self.word = 0;
                }
            }
            _ => {
                // A space. The leader's 4.5 ms space is expected once.
                if self.leader {
                    self.leader = false;
                    return;
                }
                if !self.in_frame || self.bits >= 32 {
                    return;
                }
                if duration >= NEC_STOP_SPACE {
                    self.finish();
                    return;
                }
                if duration > NEC_ONE_SPACE {
                    self.word |= 1 << self.bits;
                }
                self.bits += 1;
                if self.bits == 32 {
                    self.finish();
                }
            }
        }
    }
}

impl Default for IrReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for IrReceiver {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 != self.dat {
            return;
        }
        if let Some((start, previous)) = self.last_edge {
            self.edge(previous, now.saturating_sub(start));
        }
        self.last_edge = Some((now, level));
        self.timeout_at = Some(now + IR_TIMEOUT_NS);
    }

    fn next_event(&self) -> Option<Tick> {
        self.timeout_at
    }

    fn advance(&mut self, now: Tick) {
        if let Some(at) = self.timeout_at
            && now >= at
        {
            if self.in_frame {
                self.in_frame = false;
                self.leader = false;
                self.bits = 0;
                self.word = 0;
            }
            self.timeout_at = None;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ flame

/// A flame sensor module: an analog intensity and a threshold comparator.
///
/// The analog output rises with the flame; the digital output is high once the
/// intensity crosses the manifest's threshold.
pub struct FlameSensor {
    dout: u16,
    aout: u16,
    intensity: u16,
    threshold: u16,
}

impl FlameSensor {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            dout: index(part, "DO")?,
            aout: index(part, "AO")?,
            intensity: attr_f32(&part.attrs, "intensity", 500.0)
                .round()
                .clamp(0.0, 1000.0) as u16,
            threshold: attr_f32(&part.attrs, "threshold", 500.0)
                .round()
                .clamp(0.0, 1000.0) as u16,
        }))
    }

    pub fn intensity(&self) -> u16 {
        self.intensity
    }

    pub fn volts(&self) -> f32 {
        LOGIC_HIGH_VOLTS * f32::from(self.intensity) / 1000.0
    }
}

impl Part for FlameSensor {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dout {
            strong(if self.intensity >= self.threshold {
                Level::High
            } else {
                Level::Low
            })
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.intensity,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.intensity = value.min(1000);
        true
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.aout).then(|| self.volts())
    }
}

// ------------------------------------------------------------------ gas

/// An MQ-2 style gas sensor: concentration in ppm, an analog voltage and an
/// active-low threshold output.
pub struct GasSensor {
    dout: u16,
    aout: u16,
    ppm: f32,
    threshold: f32,
    control: u16,
}

/// Full-scale concentration the analog output maps onto the 5 V sensor rail.
const GAS_MAX_PPM: f32 = 10_000.0;
const GAS_RAIL_VOLTS: f32 = 5.0;

impl GasSensor {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let ppm = attr_f32(&part.attrs, "ppm", 400.0).clamp(0.0, GAS_MAX_PPM);
        Ok(Box::new(Self {
            dout: index(part, "DOUT")?,
            aout: index(part, "AOUT")?,
            ppm,
            threshold: attr_f32(&part.attrs, "threshold", 4.4),
            control: (ppm / GAS_MAX_PPM * 1000.0) as u16,
        }))
    }

    pub fn ppm(&self) -> f32 {
        self.ppm
    }

    pub fn volts(&self) -> f32 {
        GAS_RAIL_VOLTS * self.ppm / GAS_MAX_PPM
    }

    pub fn set_ppm(&mut self, ppm: f32) {
        self.ppm = ppm.clamp(0.0, GAS_MAX_PPM);
        self.control = (self.ppm / GAS_MAX_PPM * 1000.0) as u16;
    }
}

impl Part for GasSensor {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dout {
            // The module's comparator pulls DOUT low above the threshold.
            strong(if self.volts() >= self.threshold {
                Level::Low
            } else {
                Level::High
            })
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.control,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let value = value.min(1000);
        self.control = value;
        self.ppm = GAS_MAX_PPM * f32::from(value) / 1000.0;
        true
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.aout).then(|| self.volts())
    }
}

// ------------------------------------------------------------------ TCRT5000

/// A TCRT5000 reflective optical sensor, the usual line-tracking module.
///
/// DO is active low: the module's comparator pulls it low when the surface
/// under the sensor reflects, which is the white case. A black line absorbs
/// the infrared beam, so DO rests high. AO reports the reflectance as a
/// fraction of the 3.3 V logic rail.
pub struct Tcrt5000 {
    aout: u16,
    dout: u16,
    white: bool,
    reflectance: u16,
}

impl Tcrt5000 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let line = part
            .attrs
            .get("line")
            .map(String::as_str)
            .unwrap_or("white");
        Ok(Box::new(Self {
            aout: index(part, "AO")?,
            dout: index(part, "DO")?,
            // Inferred: any surface other than an explicitly dark one reflects.
            white: !matches!(line.trim().to_ascii_lowercase().as_str(), "black" | "dark"),
            reflectance: attr_f32(&part.attrs, "reflectance", 800.0)
                .round()
                .clamp(0.0, 1000.0) as u16,
        }))
    }

    pub fn surface_is_white(&self) -> bool {
        self.white
    }

    pub fn reflectance(&self) -> u16 {
        self.reflectance
    }

    pub fn volts(&self) -> f32 {
        LOGIC_HIGH_VOLTS * f32::from(self.reflectance) / 1000.0
    }
}

impl Part for Tcrt5000 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dout {
            strong(if self.white { Level::Low } else { Level::High })
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.reflectance,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.reflectance = value.min(1000);
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        match name {
            "line" | "surface" => {
                self.white =
                    !matches!(value.trim().to_ascii_lowercase().as_str(), "black" | "dark");
                true
            }
            "reflectance" => {
                let Ok(value) = value.trim().parse::<f32>() else {
                    return false;
                };
                self.reflectance = value.round().clamp(0.0, 1000.0) as u16;
                true
            }
            _ => false,
        }
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.aout).then(|| self.volts())
    }
}

// ------------------------------------------------------------------ LM393

/// An LM393 comparator module, the kind used for sound and light detectors.
///
/// OUT is high while `+` is above `-`, and low otherwise. A pin the board
/// drives High or Low is read as 3.3 V or 0 V; a floating input falls back to
/// the manifest's `value` or `threshold`, because the digital runtime resolves
/// analog inputs to a level rather than handing the part a voltage.
///
/// Inferred: the module carries a pull-up on the open-collector comparator
/// output, so the pin reads as a driven high rather than high impedance.
pub struct Lm393 {
    plus: u16,
    minus: u16,
    out: u16,
    plus_level: Level,
    minus_level: Level,
    value: f32,
    threshold: f32,
    reference: f32,
}

impl Lm393 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            plus: index(part, "+")?,
            minus: index(part, "-")?,
            out: index(part, "OUT")?,
            plus_level: Level::HighZ,
            minus_level: Level::HighZ,
            value: attr_f32(&part.attrs, "value", 0.0),
            threshold: attr_f32(&part.attrs, "threshold", 1.65),
            reference: attr_f32(&part.attrs, "vref", LOGIC_HIGH_VOLTS).max(0.0),
        }))
    }

    fn input_volts(level: Level, fallback: f32) -> f32 {
        match level {
            Level::High => LOGIC_HIGH_VOLTS,
            Level::Low => 0.0,
            Level::HighZ => fallback,
        }
    }

    /// Whether `+` is above `-`, which is the one thing the comparator decides.
    pub fn output_high(&self) -> bool {
        Self::input_volts(self.plus_level, self.value)
            > Self::input_volts(self.minus_level, self.threshold)
    }
}

impl Part for Lm393 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.out {
            strong(if self.output_high() {
                Level::High
            } else {
                Level::Low
            })
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.plus {
            self.plus_level = level;
        } else if pin.0 == self.minus {
            self.minus_level = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Switch {
            on: self.output_high(),
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let value = value.min(1000);
        self.value = self.reference * f32::from(value) / 1000.0;
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        let Ok(volts) = value.trim().parse::<f32>() else {
            return false;
        };
        match name {
            "value" | "plus" => {
                self.value = volts;
                true
            }
            "threshold" | "minus" => {
                self.threshold = volts;
                true
            }
            _ => false,
        }
    }
}

// ------------------------------------------------------------------ A3144

/// An A3144 unipolar Hall-effect switch.
///
/// The output is an open collector: a magnetic field turns the internal
/// transistor on and pulls OUT low, while with no field the pin is released to
/// an external pull-up (and so reads high impedance here).
pub struct HallSensor {
    out: u16,
    magnetic: bool,
}

impl HallSensor {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            out: index(part, "OUT")?,
            magnetic: attr_bool(&part.attrs, "magnetic", false),
        }))
    }

    pub fn magnetic(&self) -> bool {
        self.magnetic
    }
}

impl Part for HallSensor {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.out && self.magnetic {
            // Open collector: it only ever sinks.
            Drive::strong(Level::Low)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Switch { on: self.magnetic }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.magnetic = value >= 500;
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        if name != "magnetic" {
            return false;
        }
        self.magnetic = crate::parts::truthy(value);
        true
    }
}

// ------------------------------------------------------------------ flex / FSR

/// A resistive bend or force sensor, read as an analog voltage.
///
/// Inferred: the manifest's value is a fraction of the supply rail rather than
/// a resistance, because the pin-level view has no divider to solve. The same
/// model serves a flex sensor (`bend`) and an FSR (`force`); only the attribute
/// name and default differ.
pub struct FlexSensor {
    aout: u16,
    value: u16,
    reference: f32,
    attr: &'static str,
}

impl FlexSensor {
    pub fn build_flex(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Self::build(part, "bend", 300.0)
    }

    pub fn build_force(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Self::build(part, "force", 300.0)
    }

    fn build(
        part: &FlatPart,
        attr: &'static str,
        default: f32,
    ) -> Result<Box<dyn Part>, BuildError> {
        let fallback = attr_f32(&part.attrs, "value", default);
        Ok(Box::new(Self {
            aout: index(part, "AO")?,
            value: attr_f32(&part.attrs, attr, fallback)
                .round()
                .clamp(0.0, 1000.0) as u16,
            reference: attr_f32(&part.attrs, "vref", LOGIC_HIGH_VOLTS).max(0.0),
            attr,
        }))
    }

    pub fn value(&self) -> u16 {
        self.value
    }

    pub fn volts(&self) -> f32 {
        self.reference * f32::from(self.value) / 1000.0
    }
}

impl Part for FlexSensor {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.value,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.value = value.min(1000);
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        if name != self.attr && name != "value" {
            return false;
        }
        let Ok(value) = value.trim().parse::<f32>() else {
            return false;
        };
        self.value = value.round().clamp(0.0, 1000.0) as u16;
        true
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.aout).then(|| self.volts())
    }
}

// ------------------------------------------------------------------ heartbeat

/// A pulse sensor: a digital beat and an analog waveform.
///
/// The part raises `OUT` for 100 ms once per beat, on the board's clock. The
/// analog output rests at half the rail and rises while the pulse is high,
/// which is enough for a firmware that measures a period to recover the rate.
pub struct HeartBeatSensor {
    out: u16,
    aout: u16,
    bpm: u32,
    beats: u32,
    output: Level,
    pulse_until: Option<Tick>,
    next_beat: Option<Tick>,
    control: u16,
}

const HEARTBEAT_PULSE: Tick = 100_000_000;
const MIN_BPM: u32 = 30;
const MAX_BPM: u32 = 240;

impl HeartBeatSensor {
    pub fn new() -> Self {
        Self {
            out: 0,
            aout: 1,
            bpm: 72,
            beats: 0,
            output: Level::Low,
            pulse_until: None,
            next_beat: Some(0),
            control: 200,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let bpm = attr_f32(&part.attrs, "bpm", 72.0)
            .round()
            .clamp(MIN_BPM as f32, MAX_BPM as f32) as u32;
        let mut sensor = Self {
            out: index(part, "OUT")?,
            aout: index(part, "AOUT")?,
            ..Self::new()
        };
        sensor.set_bpm(bpm);
        Ok(Box::new(sensor))
    }

    pub fn bpm(&self) -> u32 {
        self.bpm
    }

    pub fn beats(&self) -> u32 {
        self.beats
    }

    pub fn pulse(&self) -> bool {
        self.output == Level::High
    }

    pub fn set_bpm(&mut self, bpm: u32) {
        self.bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        self.control = ((self.bpm - MIN_BPM) * 1000 / (MAX_BPM - MIN_BPM)) as u16;
    }

    fn period(&self) -> Tick {
        TICKS_PER_SEC * 60 / Tick::from(self.bpm.max(1))
    }
}

impl Default for HeartBeatSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for HeartBeatSensor {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.out {
            strong(self.output)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.pulse_until.or(self.next_beat)
    }

    fn advance(&mut self, now: Tick) {
        if let Some(until) = self.pulse_until
            && now >= until
        {
            self.output = Level::Low;
            self.pulse_until = None;
            self.next_beat = Some(now + self.period());
        }
        if let Some(at) = self.next_beat
            && now >= at
            && self.pulse_until.is_none()
        {
            self.output = Level::High;
            self.pulse_until = Some(now + HEARTBEAT_PULSE);
            self.next_beat = None;
            self.beats += 1;
        }
    }

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.control,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let value = value.min(1000);
        let bpm = MIN_BPM + u32::from(value) * (MAX_BPM - MIN_BPM) / 1000;
        self.set_bpm(bpm);
        self.control = value;
        true
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.aout).then(|| if self.pulse() { 2.4 } else { 1.65 })
    }
}

// ------------------------------------------------------------------ HX711

/// An HX711 24-bit load-cell amplifier on its two-wire interface.
///
/// `DT` is low when a conversion is ready; each rising edge of `SCK` shifts out
/// one bit, most significant first. Twenty-five or more pulses put the part
/// back into a conversion, which takes 100 ms (10 samples per second). The raw
/// value is a signed 24-bit two's-complement word scaled from the manifest's
/// weight and capacity.
pub struct Hx711 {
    dt: u16,
    sck: u16,
    ready: bool,
    sck_level: Level,
    shift: u8,
    dout_level: Level,
    ready_at: Option<Tick>,
    raw: i32,
    weight: f64,
    capacity: f64,
    full_scale: f64,
    control: u16,
}

const HX711_CONVERSION_NS: Tick = 100_000_000;

impl Hx711 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let capacity = attr_f64(&part.attrs, "capacity", 5000.0).max(1.0);
        let weight = attr_f64(&part.attrs, "weight", 0.0).clamp(0.0, capacity);
        let full_scale = if part
            .attrs
            .get("type")
            .is_some_and(|kind| kind.trim() == "50kg")
        {
            21_000.0
        } else {
            2_100.0
        };
        Ok(Box::new(Self {
            dt: index(part, "DT")?,
            sck: index(part, "SCK")?,
            ready: true,
            sck_level: Level::Low,
            shift: 0,
            dout_level: Level::Low,
            ready_at: None,
            raw: (weight / capacity * full_scale).round() as i32,
            weight,
            capacity,
            full_scale,
            control: (weight / capacity * 1000.0) as u16,
        }))
    }

    pub fn weight_grams(&self) -> f64 {
        self.weight
    }

    pub fn raw(&self) -> i32 {
        self.raw
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn set_weight_grams(&mut self, grams: f64) {
        self.weight = grams.clamp(0.0, self.capacity);
        self.raw = (self.weight / self.capacity * self.full_scale).round() as i32;
        self.control = (self.weight / self.capacity * 1000.0) as u16;
    }

    fn finish_conversion(&mut self, now: Tick) {
        self.ready = false;
        self.ready_at = Some(now + HX711_CONVERSION_NS);
        self.shift = 0;
        self.dout_level = Level::High;
    }
}

impl Part for Hx711 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dt {
            strong(self.dout_level)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 != self.sck {
            return;
        }
        let rising = level == Level::High && self.sck_level != Level::High;
        self.sck_level = level;
        if !rising {
            return;
        }
        if !self.ready {
            return;
        }
        if self.shift < 24 {
            let bit = (self.raw >> (23 - self.shift)) & 1;
            self.shift += 1;
            self.dout_level = if bit != 0 { Level::High } else { Level::Low };
        } else {
            self.finish_conversion(now);
        }
    }

    fn next_event(&self) -> Option<Tick> {
        if self.ready { None } else { self.ready_at }
    }

    fn advance(&mut self, now: Tick) {
        if let Some(at) = self.ready_at
            && now >= at
        {
            self.ready = true;
            self.ready_at = None;
            self.shift = 0;
            self.dout_level = Level::Low;
        }
    }

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.control,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let value = value.min(1000);
        self.set_weight_grams(self.capacity * f64::from(value) / 1000.0);
        self.control = value;
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        if !matches!(name, "weight" | "load" | "grams") {
            return false;
        }
        let Ok(grams) = value.trim().parse::<f64>() else {
            return false;
        };
        self.set_weight_grams(grams);
        self.control = if self.capacity > 0.0 {
            ((grams / self.capacity) * 1000.0)
                .round()
                .clamp(0.0, 1000.0) as u16
        } else {
            0
        };
        true
    }
}

// ------------------------------------------------------------------ GPS

/// A u-blox NEO-6M GPS that emits NMEA over UART at 9600 baud.
///
/// The sentence clock is the simulated time the adapter passes to
/// [`UartPeer::poll`]: one GGA/RMC pair per second. A late poll emits at most a
/// few seconds of backlog and resynchronises, so a paused simulator does not
/// dump hours of sentences at once.
pub struct GpsNeo6m {
    latitude: f64,
    longitude: f64,
    speed_knots: f64,
    start: Option<Tick>,
    next_at: Option<Tick>,
    pending: Vec<u8>,
    sentences: Vec<String>,
    emitted: u64,
}

impl GpsNeo6m {
    pub fn from_attrs(part: &FlatPart) -> Self {
        Self {
            latitude: attr_f64(&part.attrs, "latitude", 47.6062),
            longitude: attr_f64(&part.attrs, "longitude", -122.3321),
            speed_knots: attr_f64(&part.attrs, "speed", 0.5),
            start: None,
            next_at: None,
            pending: Vec::new(),
            sentences: Vec::new(),
            emitted: 0,
        }
    }

    /// Every sentence the part has produced, in emission order.
    pub fn sentences(&self) -> &[String] {
        &self.sentences
    }

    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    fn emit(&mut self, seconds: u64) {
        let time = format!(
            "{:02}{:02}{:02}.00",
            (seconds / 3600) % 24,
            (seconds / 60) % 60,
            seconds % 60
        );
        let lat = Self::degrees(
            self.latitude,
            2,
            if self.latitude >= 0.0 { 'N' } else { 'S' },
        );
        let lon = Self::degrees(
            self.longitude,
            3,
            if self.longitude >= 0.0 { 'E' } else { 'W' },
        );
        let gga = format!("GPGGA,{time},{lat},{lon},1,08,0.9,10.0,M,0.0,M,,");
        let rmc = format!(
            "GPRMC,{time},A,{lat},{lon},{:05.1},054.7,010124,,,A",
            self.speed_knots
        );
        for body in [gga, rmc] {
            let sentence = sentence(&body);
            self.pending.extend_from_slice(sentence.as_bytes());
            self.sentences.push(sentence);
        }
        self.emitted += 1;
    }

    fn degrees(value: f64, width: usize, hemisphere: char) -> String {
        let value = value.abs();
        let degrees = value.trunc();
        let minutes = (value - degrees) * 60.0;
        format!("{degrees:0width$}{minutes:07.4},{hemisphere}")
    }
}

fn sentence(body: &str) -> String {
    let checksum = body.bytes().fold(0u8, |checksum, byte| checksum ^ byte);
    format!("${body}*{checksum:02X}\r\n")
}

impl UartPeer for GpsNeo6m {
    fn receive(&mut self, _at: Tick, _bytes: &[u8]) {
        // Configuration input is accepted and ignored: the model has no
        // baud-rate or sentence-mask state to change.
    }

    fn poll(&mut self, at: Tick) -> Vec<u8> {
        if self.start.is_none() {
            self.start = Some(at);
            self.next_at = Some(at);
        }
        let mut emitted = 0;
        while let Some(next) = self.next_at {
            if at < next || emitted >= 5 {
                if emitted >= 5 {
                    self.next_at = Some(at + TICKS_PER_SEC);
                }
                break;
            }
            let seconds = self.emitted;
            self.emit(seconds);
            self.next_at = Some(next + TICKS_PER_SEC);
            emitted += 1;
        }
        std::mem::take(&mut self.pending)
    }
}

/// A Plantower PMS5003 particulate sensor: a 32-byte frame on its UART once a
/// measurement interval, with the checksum the driver verifies.
///
/// The interval is measured from the tick the board passes to `poll`, so a
/// firmware that sleeps between reads sees frames accumulate at the configured
/// rate and a run is reproducible. A sleep command (`0xE4`) stops the frames
/// until the wake command (`0xE1`), as the real part does.
pub struct Pms5003 {
    pm1: u16,
    pm25: u16,
    pm10: u16,
    interval_ns: Tick,
    start: Option<Tick>,
    next_at: Option<Tick>,
    asleep: bool,
    pending: Vec<u8>,
    frames: u64,
}

impl Pms5003 {
    pub fn new(pm1: u16, pm25: u16, pm10: u16, interval_ms: u64) -> Self {
        Self {
            pm1,
            pm25,
            pm10,
            interval_ns: interval_ms.saturating_mul(1_000_000).max(1),
            start: None,
            next_at: None,
            asleep: false,
            pending: Vec::new(),
            frames: 0,
        }
    }

    pub fn from_attrs(part: &FlatPart) -> Self {
        let value =
            |name: &str, default: f32| attr_f32(&part.attrs, name, default).max(0.0).round() as u16;
        let interval = part
            .attrs
            .get("interval")
            .and_then(|text| text.trim().parse::<u64>().ok())
            .unwrap_or(1000);
        Self::new(
            value("pm1", 4.0),
            value("pm25", 7.0),
            value("pm10", 9.0),
            interval,
        )
    }

    /// How many frames the sensor has measured.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    fn measure(&mut self) {
        // Plantower payload: PM1.0, PM2.5 and PM10 in CF=1 and atmospheric
        // units (the same values here), then seven housekeeping words.
        let mut words = [0u16; 13];
        words[0] = self.pm1;
        words[1] = self.pm25;
        words[2] = self.pm10;
        words[3] = self.pm1;
        words[4] = self.pm25;
        words[5] = self.pm10;
        let mut frame = vec![0x42, 0x4d, 0x00, 0x1c];
        for word in words {
            frame.extend_from_slice(&word.to_be_bytes());
        }
        let checksum: u32 = frame.iter().map(|byte| u32::from(*byte)).sum();
        frame.extend_from_slice(&(checksum as u16).to_be_bytes());
        self.pending.extend(frame);
        self.frames += 1;
    }
}

impl UartPeer for Pms5003 {
    fn receive(&mut self, _at: Tick, bytes: &[u8]) {
        for byte in bytes {
            match byte {
                0xe4 => self.asleep = true,
                0xe1 => self.asleep = false,
                _ => {}
            }
        }
    }

    fn poll(&mut self, at: Tick) -> Vec<u8> {
        if self.start.is_none() {
            self.start = Some(at);
            self.next_at = Some(at + self.interval_ns);
        }
        if self.asleep {
            return std::mem::take(&mut self.pending);
        }
        let mut measured = 0;
        while let Some(next) = self.next_at {
            if at < next || measured >= 4 {
                if measured >= 4 {
                    self.next_at = Some(at + self.interval_ns);
                }
                break;
            }
            self.measure();
            self.next_at = Some(next + self.interval_ns);
            measured += 1;
        }
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(id: u16) -> PinId {
        PinId(id)
    }

    fn nec_waveform(address: u8, command: u8) -> Vec<(Tick, Level)> {
        let word = u32::from(address)
            | u32::from(!address) << 8
            | u32::from(command) << 16
            | u32::from(!command) << 24;
        let mut at = 1_000_000;
        let mut edges = vec![(at, Level::High)];
        at += 1_000_000;
        edges.push((at, Level::Low)); // 9 ms leader mark
        at += 9_000_000;
        edges.push((at, Level::High)); // leader space
        at += 4_500_000;
        for bit in 0..32 {
            edges.push((at, Level::Low));
            at += 562_500;
            edges.push((at, Level::High));
            at += if word & (1 << bit) != 0 {
                1_687_500
            } else {
                562_500
            };
        }
        edges.push((at, Level::Low)); // final mark
        edges.push((at + 562_500, Level::High));
        edges
    }

    #[test]
    fn an_ir_receiver_decodes_a_nec_frame() {
        let mut receiver = IrReceiver::new();
        let mut previous = 0;
        for (at, level) in nec_waveform(0x10, 0x45) {
            receiver.set_pin(pin(0), level, at);
            previous = at;
        }
        receiver.set_pin(pin(0), Level::High, previous + 1);
        assert_eq!(
            receiver.last_frame(),
            Some(NecFrame {
                address: 0x10,
                command: 0x45,
                raw: 0xba45ef10
            })
        );
        assert_eq!(receiver.last_code(), Some(0x1045));
        assert_eq!(receiver.frames(), 1);
    }

    #[test]
    fn an_ir_receiver_discards_a_partial_frame_on_timeout() {
        let mut receiver = IrReceiver::new();
        receiver.set_pin(pin(0), Level::Low, 0);
        receiver.set_pin(pin(0), Level::High, 9_000_000);
        receiver.set_pin(pin(0), Level::Low, 13_500_000);
        let timeout = receiver.next_event().expect("a timeout is scheduled");
        receiver.advance(timeout);
        assert_eq!(receiver.next_event(), None);
        assert_eq!(receiver.last_code(), None);
    }

    #[test]
    fn a_flame_sensor_raises_its_threshold_output() {
        let mut flame = FlameSensor {
            dout: 0,
            aout: 1,
            intensity: 100,
            threshold: 500,
        };
        assert_eq!(flame.drive(pin(0)), Drive::strong(Level::Low));
        assert!((flame.analog(pin(1)).unwrap() - 0.33).abs() < 0.001);
        flame.set_control(900);
        assert_eq!(flame.drive(pin(0)), Drive::strong(Level::High));
    }

    #[test]
    fn a_gas_sensor_pulls_dout_low_above_the_threshold() {
        let mut gas = GasSensor {
            dout: 0,
            aout: 1,
            ppm: 400.0,
            threshold: 4.4,
            control: 40,
        };
        assert_eq!(gas.drive(pin(0)), Drive::strong(Level::High));
        gas.set_ppm(9_500.0);
        assert!(gas.analog(pin(1)).unwrap() > 4.4);
        assert_eq!(gas.drive(pin(0)), Drive::strong(Level::Low));
    }

    #[test]
    fn a_heartbeat_sensor_pulses_on_the_clock() {
        let mut heart = HeartBeatSensor::new();
        assert_eq!(heart.next_event(), Some(0));
        heart.advance(0);
        assert!(heart.pulse());
        assert_eq!(heart.beats(), 1);
        heart.advance(HEARTBEAT_PULSE);
        assert!(!heart.pulse());
        let next = heart.next_event().expect("the next beat is scheduled");
        heart.advance(next);
        assert_eq!(heart.beats(), 2);
        assert_eq!(heart.bpm(), 72);
    }

    #[test]
    fn an_hx711_shifts_out_a_signed_24_bit_value() {
        let mut scale = Hx711 {
            dt: 0,
            sck: 1,
            ready: true,
            sck_level: Level::Low,
            shift: 0,
            dout_level: Level::Low,
            ready_at: None,
            raw: 0x00_1234,
            weight: 1.0,
            capacity: 5000.0,
            full_scale: 2100.0,
            control: 0,
        };
        let mut read = 0i32;
        for _ in 0..24 {
            scale.set_pin(pin(1), Level::High, 0);
            let bit = if scale.drive(pin(0)) == Drive::strong(Level::High) {
                1
            } else {
                0
            };
            read = read << 1 | bit;
            scale.set_pin(pin(1), Level::Low, 0);
        }
        assert_eq!(read, 0x1234);
        scale.set_pin(pin(1), Level::High, 1_000_000);
        assert!(!scale.ready());
        assert_eq!(scale.next_event(), Some(1_000_000 + HX711_CONVERSION_NS));
        scale.advance(1_000_000 + HX711_CONVERSION_NS);
        assert!(scale.ready());
    }

    #[test]
    fn a_tcrt5000_follows_a_white_or_black_line() {
        let mut sensor = Tcrt5000 {
            aout: 0,
            dout: 1,
            white: true,
            reflectance: 800,
        };
        assert_eq!(
            sensor.drive(pin(1)),
            Drive::strong(Level::Low),
            "a white surface reflects and pulls DO low"
        );
        assert!((sensor.analog(pin(0)).unwrap() - 2.64).abs() < 0.001);

        assert!(sensor.set_named_control("line", "black"));
        assert_eq!(
            sensor.drive(pin(1)),
            Drive::strong(Level::High),
            "a black line absorbs and releases DO"
        );
        assert!(sensor.set_control(500));
        assert!((sensor.analog(pin(0)).unwrap() - 1.65).abs() < 0.001);
    }

    #[test]
    fn an_lm393_output_flips_when_plus_exceeds_minus() {
        let mut comparator = Lm393 {
            plus: 0,
            minus: 1,
            out: 2,
            plus_level: Level::HighZ,
            minus_level: Level::HighZ,
            value: 2.0,
            threshold: 1.0,
            reference: 3.3,
        };
        assert!(comparator.output_high(), "2.0 V beats the 1.0 V reference");
        assert_eq!(comparator.drive(pin(2)), Drive::strong(Level::High));
        assert_eq!(comparator.state(), PartState::Switch { on: true });

        comparator.set_pin(pin(1), Level::High, 0); // the inverting input rises
        assert!(!comparator.output_high());
        assert_eq!(comparator.drive(pin(2)), Drive::strong(Level::Low));

        comparator.set_pin(pin(0), Level::High, 0); // equal inputs are not above
        assert_eq!(comparator.drive(pin(2)), Drive::strong(Level::Low));
    }

    #[test]
    fn an_a3144_hall_sensor_sinks_when_magnetic() {
        let mut hall = HallSensor {
            out: 0,
            magnetic: false,
        };
        assert_eq!(
            hall.drive(pin(0)),
            Drive::HIGH_Z,
            "open collector with no field"
        );
        assert!(hall.set_named_control("magnetic", "true"));
        assert_eq!(hall.drive(pin(0)), Drive::strong(Level::Low));
        assert_eq!(hall.state(), PartState::Switch { on: true });

        assert!(hall.set_control(0));
        assert_eq!(hall.drive(pin(0)), Drive::HIGH_Z);
    }

    #[test]
    fn a_flex_sensor_reports_its_bend_as_a_voltage() {
        let mut flex = FlexSensor {
            aout: 0,
            value: 300,
            reference: 3.3,
            attr: "bend",
        };
        assert!((flex.volts() - 0.99).abs() < 0.001);
        assert_eq!(flex.analog(pin(0)), Some(flex.volts()));
        assert!(flex.set_named_control("bend", "1000"));
        assert!((flex.volts() - 3.3).abs() < 0.001);
        assert_eq!(flex.state(), PartState::Knob { permille: 1000 });
    }

    #[test]
    fn a_force_sensor_reports_its_pressure_as_a_voltage() {
        let mut fsr = FlexSensor {
            aout: 0,
            value: 500,
            reference: 5.0,
            attr: "force",
        };
        assert!((fsr.volts() - 2.5).abs() < 0.001);
        assert!(fsr.set_named_control("force", "200"));
        assert!((fsr.volts() - 1.0).abs() < 0.001);
        assert!(fsr.set_control(1000));
        assert!((fsr.volts() - 5.0).abs() < 0.001);
    }

    #[test]
    fn a_gps_emits_one_nmea_pair_per_second() {
        let mut gps = GpsNeo6m {
            latitude: 47.6062,
            longitude: -122.3321,
            speed_knots: 0.5,
            start: None,
            next_at: None,
            pending: Vec::new(),
            sentences: Vec::new(),
            emitted: 0,
        };
        let first = gps.poll(0);
        assert!(String::from_utf8_lossy(&first).contains("$GPGGA,"));
        assert!(String::from_utf8_lossy(&first).contains("$GPRMC,"));
        assert_eq!(gps.emitted(), 1);
        assert!(gps.poll(999_999_999).is_empty());
        let second = gps.poll(TICKS_PER_SEC);
        assert!(!second.is_empty());
        assert_eq!(gps.emitted(), 2);
        let text = String::from_utf8_lossy(&second);
        let body = text.lines().next().unwrap();
        let (body, checksum) = body[1..].split_once('*').unwrap();
        let expected = body.bytes().fold(0u8, |sum, byte| sum ^ byte);
        assert_eq!(checksum, format!("{expected:02X}"));
    }
}
