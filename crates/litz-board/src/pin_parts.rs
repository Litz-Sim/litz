//! Builtin pin-level parts.
//!
//! Each model watches the levels on its own pins and drives the pins a real
//! part would drive. Sensors that a person can move (temperature, humidity,
//! distance, light, motion) take their value from [`Part::set_control`] or
//! from the manifest, never from wall time: every delay is computed from the
//! [`Tick`] the board passes in.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};

/// Logic high is the 3.3 V rail, the ADC's full scale on this board.
const LOGIC_HIGH_VOLTS: f32 = 3.3;

fn index(part: &FlatPart, name: &str) -> Option<u16> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
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

fn pins<const N: usize>(part: &FlatPart, names: [&str; N]) -> Result<[u16; N], BuildError> {
    let mut found = [0u16; N];
    for (slot, name) in found.iter_mut().zip(names) {
        *slot = index(part, name).ok_or_else(|| {
            BuildError::Message(format!("part {:?} has no {name:?} pin", part.id))
        })?;
    }
    Ok(found)
}

fn attr_f32(attrs: &std::collections::BTreeMap<String, String>, name: &str, default: f32) -> f32 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

fn attr_u16(attrs: &std::collections::BTreeMap<String, String>, name: &str, default: u16) -> u16 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

fn strong(level: Level) -> Drive {
    match level {
        Level::HighZ => Drive::HIGH_Z,
        level => Drive::strong(level),
    }
}

// ------------------------------------------------------------------ DHT22

/// A DHT22 reading, in tenths so the frame and the accessor agree exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DhtReading {
    pub humidity_tenths: i16,
    pub temperature_tenths: i16,
}

/// A DHT22 temperature/humidity sensor on the single-wire protocol.
///
/// The host pulls the data line low for at least 1 ms and releases it; the
/// sensor then answers with 80 us low, 80 us high and forty bits, each a 50 us
/// low period followed by a 26 us (zero) or 70 us (one) high period. The five
/// bytes are humidity, temperature and a modulo-256 checksum. The edges are
/// scheduled through [`Part::next_event`], so the frame follows simulated time.
pub struct Dht22 {
    data: u16,
    /// What the part is currently driving; `HighZ` when it is listening.
    output: Level,
    edges: Vec<(Tick, Level)>,
    next: usize,
    low_at: Option<Tick>,
    temperature_tenths: i16,
    humidity_tenths: i16,
    control: u16,
    last_frame: Option<[u8; 5]>,
    complete: bool,
}

/// The response starts at least this long after the host releases the line.
const DHT_RESPONSE_DELAY: Tick = 30_000;
const DHT_START_LOW: Tick = 80_000;
const DHT_START_HIGH: Tick = 80_000;
const DHT_BIT_LOW: Tick = 50_000;
const DHT_ZERO_HIGH: Tick = 26_000;
const DHT_ONE_HIGH: Tick = 70_000;
/// A host pulse shorter than this is noise, not a start condition.
const DHT_MIN_START_LOW: Tick = 1_000_000;

impl Dht22 {
    pub fn with_reading(humidity_tenths: i16, temperature_tenths: i16) -> Self {
        Self {
            data: 0,
            output: Level::HighZ,
            edges: Vec::new(),
            next: 0,
            low_at: None,
            temperature_tenths,
            humidity_tenths,
            control: 500,
            last_frame: None,
            complete: false,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [data] = pins(part, ["SDA"])?;
        let humidity = attr_f32(&part.attrs, "humidity", 50.0);
        let temperature = attr_f32(&part.attrs, "temperature", 25.0);
        let mut sensor = Self::with_reading(
            (humidity * 10.0).round() as i16,
            (temperature * 10.0).round() as i16,
        );
        sensor.data = data;
        sensor.control = temperature_permille(sensor.temperature_tenths);
        Ok(Box::new(sensor))
    }

    /// The last five-byte frame the sensor put on the wire, once one has run.
    pub fn last_frame(&self) -> Option<[u8; 5]> {
        self.last_frame
    }

    /// The decoded reading, when a complete frame with a valid checksum exists.
    pub fn reading(&self) -> Option<DhtReading> {
        if !self.complete {
            return None;
        }
        let frame = self.last_frame?;
        let checksum = frame[0]
            .wrapping_add(frame[1])
            .wrapping_add(frame[2])
            .wrapping_add(frame[3]);
        if checksum != frame[4] {
            return None;
        }
        let humidity = (u16::from(frame[0]) << 8 | u16::from(frame[1])) as i16;
        let raw_temperature = u16::from(frame[2] & 0x7f) << 8 | u16::from(frame[3]);
        let temperature = if frame[2] & 0x80 != 0 {
            -(raw_temperature as i16)
        } else {
            raw_temperature as i16
        };
        Some(DhtReading {
            humidity_tenths: humidity,
            temperature_tenths: temperature,
        })
    }

    pub fn temperature_tenths(&self) -> i16 {
        self.temperature_tenths
    }

    pub fn humidity_tenths(&self) -> i16 {
        self.humidity_tenths
    }

    fn busy(&self) -> bool {
        self.next < self.edges.len()
    }

    fn encode(&self) -> [u8; 5] {
        let humidity = self.humidity_tenths.max(0) as u16;
        let temperature = if self.temperature_tenths < 0 {
            (-self.temperature_tenths as u16) | 0x8000
        } else {
            self.temperature_tenths as u16
        };
        let bytes = [
            (humidity >> 8) as u8,
            humidity as u8,
            (temperature >> 8) as u8,
            temperature as u8,
        ];
        let checksum = bytes[0]
            .wrapping_add(bytes[1])
            .wrapping_add(bytes[2])
            .wrapping_add(bytes[3]);
        [bytes[0], bytes[1], bytes[2], bytes[3], checksum]
    }

    fn start_frame(&mut self, release: Tick) {
        let frame = self.encode();
        self.last_frame = Some(frame);
        self.complete = false;

        let mut edges = Vec::with_capacity(4 + 40 * 2);
        let base = release + DHT_RESPONSE_DELAY;
        edges.push((base, Level::Low));
        edges.push((base + DHT_START_LOW, Level::High));
        let mut at = base + DHT_START_LOW + DHT_START_HIGH;
        edges.push((at, Level::Low));
        for bit in 0..40 {
            let value = (frame[bit / 8] >> (7 - (bit % 8))) & 1;
            at += DHT_BIT_LOW;
            edges.push((at, Level::High));
            at += if value == 1 {
                DHT_ONE_HIGH
            } else {
                DHT_ZERO_HIGH
            };
            edges.push((at, Level::Low));
        }
        // After the last bit the bus goes idle and the sensor releases it.
        edges.push((at + DHT_BIT_LOW, Level::HighZ));

        self.edges = edges;
        self.next = 0;
        self.output = Level::HighZ;
    }
}

/// Map a temperature in tenths of a degree onto the 0–1000 control range,
/// across the DHT22's -40..80 °C span.
fn temperature_permille(tenths: i16) -> u16 {
    let span = i32::from(tenths) + 400;
    (span * 1000 / 1200).clamp(0, 1000) as u16
}

impl Part for Dht22 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.data {
            strong(self.output)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 != self.data || self.busy() {
            return;
        }
        match level {
            Level::Low => {
                if self.low_at.is_none() {
                    self.low_at = Some(now);
                }
            }
            Level::High | Level::HighZ => {
                if let Some(start) = self.low_at.take()
                    && now.saturating_sub(start) >= DHT_MIN_START_LOW
                {
                    self.start_frame(now);
                }
            }
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.edges.get(self.next).map(|(at, _)| *at)
    }

    fn advance(&mut self, now: Tick) {
        while let Some(&(at, level)) = self.edges.get(self.next) {
            if at > now {
                break;
            }
            self.output = level;
            self.next += 1;
        }
        if self.next >= self.edges.len() {
            self.complete = !self.edges.is_empty();
            self.edges.clear();
            self.next = 0;
        }
    }

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.control,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let value = value.min(1000);
        self.control = value;
        let tenths = -400 + (i32::from(value) * 1200 / 1000);
        self.temperature_tenths = tenths.clamp(-400, 800) as i16;
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        let Ok(value) = value.trim().parse::<f32>() else {
            return false;
        };
        match name {
            "temperature" => {
                self.temperature_tenths = (value * 10.0).round().clamp(-400.0, 800.0) as i16;
                self.control = temperature_permille(self.temperature_tenths);
                true
            }
            "humidity" => {
                self.humidity_tenths = (value * 10.0).round().clamp(0.0, 1000.0) as i16;
                true
            }
            _ => false,
        }
    }
}

// ------------------------------------------------------------------ HC-SR04

/// An HC-SR04 ultrasonic rangefinder.
///
/// A high pulse of at least 10 us on TRIG starts a measurement. The sensor
/// raises ECHO after a short delay and keeps it high for 58 us per centimetre
/// of round-trip distance. Both edges are scheduled through
/// [`Part::next_event`], so the pulse width is simulated time, not wall time.
pub struct HcSr04 {
    trig: u16,
    echo: u16,
    distance_cm: f32,
    trig_high: Option<Tick>,
    edges: Vec<(Tick, Level)>,
    next: usize,
    echo_level: Level,
    control: u16,
}

/// ECHO follows TRIG by this much, before the pulse starts.
const HC_ECHO_DELAY: Tick = 50_000;
/// Nanoseconds per centimetre of round-trip flight time.
const NS_PER_CM: Tick = 58_000;

impl HcSr04 {
    pub fn with_distance(distance_cm: f32) -> Self {
        Self {
            trig: 0,
            echo: 1,
            distance_cm: distance_cm.clamp(2.0, 400.0),
            trig_high: None,
            edges: Vec::new(),
            next: 0,
            echo_level: Level::Low,
            control: distance_permille(distance_cm),
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [trig, echo] = pins(part, ["TRIG", "ECHO"])?;
        let distance = attr_f32(&part.attrs, "distance", 100.0).clamp(2.0, 400.0);
        let mut sensor = Self::with_distance(distance);
        sensor.trig = trig;
        sensor.echo = echo;
        Ok(Box::new(sensor))
    }

    pub fn distance_cm(&self) -> f32 {
        self.distance_cm
    }

    /// The width of the ECHO pulse this distance produces.
    pub fn echo_width_ns(&self) -> Tick {
        (f64::from(self.distance_cm) * NS_PER_CM as f64).round() as Tick
    }

    fn schedule(&mut self, now: Tick) {
        let start = now + HC_ECHO_DELAY;
        let end = start + self.echo_width_ns();
        self.edges = vec![(start, Level::High), (end, Level::Low)];
        self.next = 0;
    }
}

fn distance_permille(distance_cm: f32) -> u16 {
    (((distance_cm - 2.0) / 398.0) * 1000.0)
        .round()
        .clamp(0.0, 1000.0) as u16
}

impl Part for HcSr04 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.echo {
            strong(self.echo_level)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 != self.trig {
            return;
        }
        match level {
            Level::High => {
                if self.trig_high.is_none() {
                    self.trig_high = Some(now);
                }
            }
            Level::Low | Level::HighZ => {
                if let Some(start) = self.trig_high.take()
                    && now.saturating_sub(start) >= 10_000
                    && self.next >= self.edges.len()
                {
                    self.schedule(now);
                }
            }
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.edges.get(self.next).map(|(at, _)| *at)
    }

    fn advance(&mut self, now: Tick) {
        while let Some(&(at, level)) = self.edges.get(self.next) {
            if at > now {
                break;
            }
            self.echo_level = level;
            self.next += 1;
        }
        if self.next >= self.edges.len() {
            self.edges.clear();
            self.next = 0;
        }
    }

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.control,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let value = value.min(1000);
        self.control = value;
        self.distance_cm = 2.0 + (f32::from(value) / 1000.0) * 398.0;
        true
    }
}

// ------------------------------------------------------------------ PIR

/// A passive-infrared motion sensor: OUT is high while motion is present.
pub struct PirMotionSensor {
    out: u16,
    motion: bool,
}

impl PirMotionSensor {
    pub fn new() -> Self {
        Self {
            out: 0,
            motion: false,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [out] = pins(part, ["OUT"])?;
        Ok(Box::new(Self { out, motion: false }))
    }

    pub fn motion(&self) -> bool {
        self.motion
    }
}

impl Default for PirMotionSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for PirMotionSensor {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.out {
            strong(if self.motion { Level::High } else { Level::Low })
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Switch { on: self.motion }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.motion = value >= 500;
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        if name != "motion" {
            return false;
        }
        self.motion = crate::parts::truthy(value);
        true
    }
}

// ------------------------------------------------------------------ LDR

/// A photoresistor module: AO rises with light, DO trips above mid-scale.
pub struct Photoresistor {
    ao: u16,
    dout: u16,
    light: u16,
}

impl Photoresistor {
    pub fn with_light(permille: u16) -> Self {
        Self {
            ao: 0,
            dout: 1,
            light: permille.min(1000),
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [ao, dout] = pins(part, ["AO", "DO"])?;
        let light = attr_u16(&part.attrs, "light", 500).min(1000);
        Ok(Box::new(Self { ao, dout, light }))
    }

    pub fn light(&self) -> u16 {
        self.light
    }

    pub fn volts(&self) -> f32 {
        LOGIC_HIGH_VOLTS * f32::from(self.light) / 1000.0
    }
}

impl Part for Photoresistor {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dout {
            strong(if self.light >= 500 {
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
            permille: self.light,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.light = value.min(1000);
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        // The module's documented range is 0 through 1000 lux; a scenario
        // that writes a percentage instead still lands on the same scale.
        let Ok(value) = value.trim().parse::<f32>() else {
            return false;
        };
        match name {
            "lux" | "light" | "value" => {
                self.light = value.round().clamp(0.0, 1000.0) as u16;
                true
            }
            _ => false,
        }
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.ao).then_some(self.volts())
    }
}

// ------------------------------------------------------------------ NTC

/// An NTC thermistor module: AO is the temperature as a voltage.
///
/// The control spans -20..100 °C linearly, matching the module's typical
/// divider; the analog output is the same fraction of the 3.3 V rail.
pub struct NtcSensor {
    ao: u16,
    dout: u16,
    temperature_tenths: i32,
}

const NTC_MIN_TENTHS: i32 = -200;
const NTC_MAX_TENTHS: i32 = 1000;

impl NtcSensor {
    pub fn with_celsius(temperature: f32) -> Self {
        let tenths = (temperature * 10.0)
            .round()
            .clamp(NTC_MIN_TENTHS as f32, NTC_MAX_TENTHS as f32) as i32;
        Self {
            ao: 0,
            dout: 1,
            temperature_tenths: tenths,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [ao, dout] = pins(part, ["AO", "DO"])?;
        let temperature = attr_f32(&part.attrs, "temperature", 25.0);
        let mut sensor = Self::with_celsius(temperature);
        sensor.ao = ao;
        sensor.dout = dout;
        Ok(Box::new(sensor))
    }

    pub fn temperature_celsius(&self) -> f32 {
        self.temperature_tenths as f32 / 10.0
    }

    fn permille(&self) -> u16 {
        let span = self.temperature_tenths - NTC_MIN_TENTHS;
        (span * 1000 / (NTC_MAX_TENTHS - NTC_MIN_TENTHS)).clamp(0, 1000) as u16
    }

    pub fn volts(&self) -> f32 {
        LOGIC_HIGH_VOLTS * f32::from(self.permille()) / 1000.0
    }
}

impl Part for NtcSensor {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dout {
            strong(if self.temperature_tenths >= 500 {
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
            permille: self.permille(),
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let value = i32::from(value.min(1000));
        self.temperature_tenths = NTC_MIN_TENTHS + value * (NTC_MAX_TENTHS - NTC_MIN_TENTHS) / 1000;
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        if name != "temperature" {
            return false;
        }
        let Ok(celsius) = value.trim().parse::<f32>() else {
            return false;
        };
        self.temperature_tenths = (celsius * 10.0)
            .round()
            .clamp(NTC_MIN_TENTHS as f32, NTC_MAX_TENTHS as f32)
            as i32;
        true
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.ao).then_some(self.volts())
    }
}

// ------------------------------------------------------------------ RGB LED

/// An RGB LED with three independently driven dies.
///
/// The pins are digital, so a channel is full on or off. A firmware that
/// toggles a pin quickly still gets a readable colour because the last level
/// wins; brightness is a display concern, not a logic one.
pub struct RgbLed {
    channels: [u16; 3],
    levels: [Level; 3],
}

impl RgbLed {
    pub fn new() -> Self {
        Self {
            channels: [0, 1, 2],
            levels: [Level::HighZ; 3],
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let channels = pins(part, ["R", "G", "B"])?;
        Ok(Box::new(Self {
            channels,
            levels: [Level::HighZ; 3],
        }))
    }

    /// The colour the pins currently ask for, 0 or 255 per channel.
    pub fn color(&self) -> (u8, u8, u8) {
        let channel = |index: usize| {
            if self.levels[index] == Level::High {
                255
            } else {
                0
            }
        };
        (channel(0), channel(1), channel(2))
    }

    fn role(&self, pin: u16) -> Option<usize> {
        self.channels.iter().position(|&channel| channel == pin)
    }
}

impl Default for RgbLed {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for RgbLed {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(role) = self.role(pin.0) {
            self.levels[role] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Led {
            lit: self.levels.contains(&Level::High),
        }
    }
}

// ------------------------------------------------------------------ LED bar

/// A ten-segment LED bar graph: one anode per segment and a common cathode.
pub struct LedBarGraph {
    anodes: [u16; 10],
    levels: [Level; 10],
}

impl LedBarGraph {
    pub fn new() -> Self {
        let mut anodes = [0u16; 10];
        for (index, anode) in anodes.iter_mut().enumerate() {
            *anode = index as u16;
        }
        Self {
            anodes,
            levels: [Level::HighZ; 10],
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut anodes = [0u16; 10];
        for (number, anode) in anodes.iter_mut().enumerate() {
            *anode = index(part, &format!("A{}", number + 1)).ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no A{} pin", part.id, number + 1))
            })?;
        }
        Ok(Box::new(Self {
            anodes,
            levels: [Level::HighZ; 10],
        }))
    }

    /// How many segments are lit.
    pub fn level(&self) -> u16 {
        self.levels
            .iter()
            .filter(|&&level| level == Level::High)
            .count() as u16
    }
}

impl Default for LedBarGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for LedBarGraph {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(index) = self.anodes.iter().position(|&anode| anode == pin.0) {
            self.levels[index] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.level() * 100,
        }
    }
}

// ------------------------------------------------------------------ relay

/// A single-pole double-throw relay: the coil moves COM from NC to NO.
pub struct Relay {
    coil: u16,
    no: u16,
    nc: u16,
    com: u16,
    levels: [Level; 3],
    on: bool,
}

impl Relay {
    pub fn new() -> Self {
        Self {
            coil: 0,
            no: 1,
            nc: 2,
            com: 3,
            levels: [Level::HighZ; 3],
            on: false,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [coil, no, nc, com] = pins(part, ["IN", "NO", "NC", "COM"])?;
        Ok(Box::new(Self {
            coil,
            no,
            nc,
            com,
            levels: [Level::HighZ; 3],
            on: false,
        }))
    }

    pub fn energized(&self) -> bool {
        self.on
    }

    fn selected(&self) -> usize {
        if self.on { 0 } else { 1 }
    }
}

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for Relay {
    fn drive(&self, pin: PinId) -> Drive {
        let source = if pin.0 == self.com {
            Some(self.selected())
        } else if (pin.0 == self.no && self.on) || (pin.0 == self.nc && !self.on) {
            Some(2)
        } else {
            None
        };
        match source {
            Some(index) => strong(self.levels[index]),
            None => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.coil {
            self.on = level == Level::High;
            return;
        }
        let role = if pin.0 == self.no {
            Some(0)
        } else if pin.0 == self.nc {
            Some(1)
        } else if pin.0 == self.com {
            Some(2)
        } else {
            None
        };
        let Some(role) = role else { return };
        // Only the terminal's own external level is recorded. A contact is a
        // conductor, so `drive` maps the selected throw's level onto COM and
        // vice versa; mirroring it here would leave the old throw's level on
        // COM when the coil throws.
        self.levels[role] = level;
    }

    fn state(&self) -> PartState {
        PartState::Switch { on: self.on }
    }
}

// ------------------------------------------------------------------ L293D

/// An L293D dual half-bridge motor driver, modelled on one channel.
pub struct L293d {
    inputs: [u16; 3],
    outputs: [u16; 2],
    levels: [Level; 3],
}

impl L293d {
    pub fn new() -> Self {
        Self {
            inputs: [0, 1, 2],
            outputs: [3, 4],
            levels: [Level::HighZ; 3],
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let inputs = pins(part, ["IN1", "IN2", "EN"])?;
        let outputs = pins(part, ["OUT1", "OUT2"])?;
        Ok(Box::new(Self {
            inputs,
            outputs,
            levels: [Level::HighZ; 3],
        }))
    }

    /// `(OUT1, OUT2)` as the bridge would drive them.
    pub fn outputs(&self) -> (Level, Level) {
        if self.levels[2] != Level::High {
            return (Level::HighZ, Level::HighZ);
        }
        match (self.levels[0] == Level::High, self.levels[1] == Level::High) {
            (true, false) => (Level::High, Level::Low),
            (false, true) => (Level::Low, Level::High),
            // Both inputs equal is a brake: both outputs are pulled low.
            _ => (Level::Low, Level::Low),
        }
    }
}

impl Default for L293d {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for L293d {
    fn drive(&self, pin: PinId) -> Drive {
        let (out1, out2) = self.outputs();
        if pin.0 == self.outputs[0] {
            strong(out1)
        } else if pin.0 == self.outputs[1] {
            strong(out2)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(index) = self.inputs.iter().position(|&input| input == pin.0) {
            self.levels[index] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Switch {
            on: self.levels[2] == Level::High && self.levels[0] != self.levels[1],
        }
    }
}

// ------------------------------------------------------------------ 74HC165

/// A 74HC165 8-bit parallel-in, serial-out shift register.
///
/// PL low loads D0..D7; PL high enables shifting. On each rising CP with CE
/// low the byte shifts left and DS enters at bit 0, while Q7 reports bit 7.
pub struct Hc165 {
    data: [u16; 8],
    pl: u16,
    cp: u16,
    ce: u16,
    ds: u16,
    q7: u16,
    q7n: u16,
    inputs: [Level; 8],
    pl_level: Level,
    cp_level: Level,
    ce_level: Level,
    ds_level: Level,
    byte: u8,
}

impl Hc165 {
    pub fn new() -> Self {
        let mut data = [0u16; 8];
        for (index, pin) in data.iter_mut().enumerate() {
            *pin = index as u16;
        }
        Self {
            data,
            pl: 8,
            cp: 9,
            ce: 10,
            ds: 11,
            q7: 12,
            q7n: 13,
            inputs: [Level::HighZ; 8],
            pl_level: Level::HighZ,
            cp_level: Level::HighZ,
            ce_level: Level::HighZ,
            ds_level: Level::HighZ,
            byte: 0,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut data = [0u16; 8];
        for (number, pin) in data.iter_mut().enumerate() {
            *pin = index(part, &format!("D{number}")).ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no D{number} pin", part.id))
            })?;
        }
        let [pl, cp, ce, ds, q7, q7n] = pins(part, ["PL", "CP", "CE", "DS", "Q7", "Q7_N"])?;
        Ok(Box::new(Self {
            data,
            pl,
            cp,
            ce,
            ds,
            q7,
            q7n,
            inputs: [Level::HighZ; 8],
            pl_level: Level::HighZ,
            cp_level: Level::HighZ,
            ce_level: Level::HighZ,
            ds_level: Level::HighZ,
            byte: 0,
        }))
    }

    pub fn byte(&self) -> u8 {
        self.byte
    }

    fn reload(&mut self) {
        self.byte = self
            .inputs
            .iter()
            .enumerate()
            .fold(0u8, |byte, (index, level)| {
                byte | ((*level == Level::High) as u8) << index
            });
    }

    fn shift(&mut self) {
        let serial = (self.ds_level == Level::High) as u8;
        self.byte = (self.byte << 1) | serial;
    }
}

impl Default for Hc165 {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for Hc165 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.q7 {
            strong(if self.byte & 0x80 != 0 {
                Level::High
            } else {
                Level::Low
            })
        } else if pin.0 == self.q7n {
            strong(if self.byte & 0x80 == 0 {
                Level::High
            } else {
                Level::Low
            })
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(index) = self.data.iter().position(|&data| data == pin.0) {
            self.inputs[index] = level;
            if self.pl_level == Level::Low {
                self.reload();
            }
            return;
        }
        if pin.0 == self.pl {
            let previous = self.pl_level;
            self.pl_level = level;
            if level == Level::Low && previous != Level::Low {
                self.reload();
            }
        } else if pin.0 == self.cp {
            let previous = self.cp_level;
            self.cp_level = level;
            if level == Level::High
                && previous != Level::High
                && self.pl_level == Level::High
                && self.ce_level == Level::Low
            {
                self.shift();
            }
        } else if pin.0 == self.ce {
            self.ce_level = level;
        } else if pin.0 == self.ds {
            self.ds_level = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ 74HC138

/// A 74HC138 3-to-8 line decoder with active-low outputs.
///
/// G1 (E3) is active high and G2A/G2B (E1/E2) active low. When enabled the
/// selected output is low and the rest are high; when disabled every output
/// is high.
pub struct Hc138 {
    selects: [u16; 3],
    enables: [u16; 3],
    outputs: [u16; 8],
    levels: [Level; 6],
}

impl Hc138 {
    pub fn new() -> Self {
        Self {
            selects: [0, 1, 2],
            enables: [3, 4, 5],
            outputs: [6, 7, 8, 9, 10, 11, 12, 13],
            levels: [Level::HighZ; 6],
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let selects = pins(part, ["A0", "A1", "A2"])?;
        let enables = pins(part, ["E1", "E2", "E3"])?;
        let mut outputs = [0u16; 8];
        for (number, output) in outputs.iter_mut().enumerate() {
            *output = index(part, &format!("Y{number}")).ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no Y{number} pin", part.id))
            })?;
        }
        Ok(Box::new(Self {
            selects,
            enables,
            outputs,
            levels: [Level::HighZ; 6],
        }))
    }

    fn enabled(&self) -> bool {
        self.levels[3] == Level::Low
            && self.levels[4] == Level::Low
            && self.levels[5] == Level::High
    }

    /// The selected output number, or `None` while the part is disabled.
    pub fn selected(&self) -> Option<u8> {
        if !self.enabled() {
            return None;
        }
        let address = usize::from(self.levels[0] == Level::High)
            | usize::from(self.levels[1] == Level::High) << 1
            | usize::from(self.levels[2] == Level::High) << 2;
        Some(address as u8)
    }

    fn output_level(&self, index: usize) -> Level {
        match self.selected() {
            Some(selected) if usize::from(selected) == index => Level::Low,
            _ => Level::High,
        }
    }
}

impl Default for Hc138 {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for Hc138 {
    fn drive(&self, pin: PinId) -> Drive {
        match self.outputs.iter().position(|&output| output == pin.0) {
            Some(index) => strong(self.output_level(index)),
            None => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(index) = self.selects.iter().position(|&select| select == pin.0) {
            self.levels[index] = level;
        } else if let Some(index) = self.enables.iter().position(|&enable| enable == pin.0) {
            self.levels[3 + index] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ 74HC238

/// A 74HC238 3-to-8 line decoder with active-high outputs.
///
/// It is the non-inverting sibling of the 74HC138: the same two active-low
/// enables (E1, E2) and active-high enable (E3) and the same A0..A2 select,
/// but the selected output is high and the other seven are low. When disabled
/// every output is low.
pub struct Hc238 {
    selects: [u16; 3],
    enables: [u16; 3],
    outputs: [u16; 8],
    levels: [Level; 6],
}

impl Hc238 {
    pub fn new() -> Self {
        Self {
            selects: [0, 1, 2],
            enables: [3, 4, 5],
            outputs: [6, 7, 8, 9, 10, 11, 12, 13],
            levels: [Level::HighZ; 6],
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let selects = pins(part, ["A0", "A1", "A2"])?;
        let enables = pins(part, ["E1", "E2", "E3"])?;
        let mut outputs = [0u16; 8];
        for (number, output) in outputs.iter_mut().enumerate() {
            *output = index(part, &format!("Y{number}")).ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no Y{number} pin", part.id))
            })?;
        }
        Ok(Box::new(Self {
            selects,
            enables,
            outputs,
            levels: [Level::HighZ; 6],
        }))
    }

    fn enabled(&self) -> bool {
        self.levels[3] == Level::Low
            && self.levels[4] == Level::Low
            && self.levels[5] == Level::High
    }

    /// The selected output number, or `None` while the part is disabled.
    pub fn selected(&self) -> Option<u8> {
        if !self.enabled() {
            return None;
        }
        let address = usize::from(self.levels[0] == Level::High)
            | usize::from(self.levels[1] == Level::High) << 1
            | usize::from(self.levels[2] == Level::High) << 2;
        Some(address as u8)
    }

    fn output_level(&self, index: usize) -> Level {
        match self.selected() {
            Some(selected) if usize::from(selected) == index => Level::High,
            _ => Level::Low,
        }
    }
}

impl Default for Hc238 {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for Hc238 {
    fn drive(&self, pin: PinId) -> Drive {
        match self.outputs.iter().position(|&output| output == pin.0) {
            Some(index) => strong(self.output_level(index)),
            None => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(index) = self.selects.iter().position(|&select| select == pin.0) {
            self.levels[index] = level;
        } else if let Some(index) = self.enables.iter().position(|&enable| enable == pin.0) {
            self.levels[3 + index] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ 74HC139

/// A 74HC139 dual 2-to-4 line decoder with active-low outputs.
///
/// Each half has two selects (`1A0`/`1A1`, `2A0`/`2A1`) and one active-low
/// enable (`1E`, `2E`). While a half is enabled its selected output is low and
/// the other three are high; while disabled all four outputs are high.
pub struct Hc139 {
    selects: [[u16; 2]; 2],
    enables: [u16; 2],
    outputs: [[u16; 4]; 2],
    select_levels: [[Level; 2]; 2],
    enable_levels: [Level; 2],
}

impl Hc139 {
    pub fn new() -> Self {
        Self {
            selects: [[0, 1], [2, 3]],
            enables: [4, 5],
            outputs: [[6, 7, 8, 9], [10, 11, 12, 13]],
            select_levels: [[Level::HighZ; 2]; 2],
            enable_levels: [Level::HighZ; 2],
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut selects = [[0u16; 2]; 2];
        let mut outputs = [[0u16; 4]; 2];
        let mut enables = [0u16; 2];
        for (decoder, select_pair) in selects.iter_mut().enumerate() {
            let prefix = decoder + 1;
            for (bit, slot) in select_pair.iter_mut().enumerate() {
                let name = format!("{prefix}A{bit}");
                *slot = index(part, &name).ok_or_else(|| {
                    BuildError::Message(format!("part {:?} has no {name:?} pin", part.id))
                })?;
            }
            for (output, slot) in outputs[decoder].iter_mut().enumerate() {
                let name = format!("{prefix}Y{output}");
                *slot = index(part, &name).ok_or_else(|| {
                    BuildError::Message(format!("part {:?} has no {name:?} pin", part.id))
                })?;
            }
            let name = format!("{prefix}E");
            enables[decoder] = index(part, &name).ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no {name:?} pin", part.id))
            })?;
        }
        Ok(Box::new(Self {
            selects,
            enables,
            outputs,
            select_levels: [[Level::HighZ; 2]; 2],
            enable_levels: [Level::HighZ; 2],
        }))
    }

    /// The selected output of one half, or `None` while that half is disabled.
    pub fn selected(&self, decoder: usize) -> Option<u8> {
        if decoder >= 2 || self.enable_levels[decoder] != Level::Low {
            return None;
        }
        let address = usize::from(self.select_levels[decoder][0] == Level::High)
            | usize::from(self.select_levels[decoder][1] == Level::High) << 1;
        Some(address as u8)
    }

    fn output_level(&self, decoder: usize, index: usize) -> Level {
        match self.selected(decoder) {
            Some(selected) if usize::from(selected) == index => Level::Low,
            _ => Level::High,
        }
    }
}

impl Default for Hc139 {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for Hc139 {
    fn drive(&self, pin: PinId) -> Drive {
        for decoder in 0..2 {
            if let Some(index) = self.outputs[decoder]
                .iter()
                .position(|&output| output == pin.0)
            {
                return strong(self.output_level(decoder, index));
            }
        }
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        for decoder in 0..2 {
            if let Some(bit) = self.selects[decoder]
                .iter()
                .position(|&select| select == pin.0)
            {
                self.select_levels[decoder][bit] = level;
                return;
            }
        }
        if let Some(decoder) = self.enables.iter().position(|&enable| enable == pin.0) {
            self.enable_levels[decoder] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------- MAX7219 single

const MAX7219_ROWS: usize = 8;

/// A single MAX7219 8x8 matrix controller wired to individual pins.
///
/// The protocol is the one the SPI [`Max7219`](crate::protocol_displays::Max7219)
/// uses: DIN is sampled on every rising CLK while LOAD is low, sixteen bits
/// most significant first; a rising LOAD latches the word, whose high byte is
/// the register and low byte its value. Registers 1..8 hold the row pixels,
/// 0x0C shutdown, 0x0B the scan limit and 0x0F display test. DOUT repeats the
/// bit that the shift register overflowed, which lets units be chained.
pub struct Max7219Single {
    din: u16,
    clk: u16,
    load: u16,
    dout: Option<u16>,
    din_level: Level,
    clk_level: Level,
    load_level: Level,
    dout_level: Level,
    shift: u16,
    rows: [u8; MAX7219_ROWS],
    shutdown: bool,
    scan_limit: u8,
    intensity: u8,
    decode: u8,
}

impl Max7219Single {
    pub fn new() -> Self {
        Self {
            din: 0,
            clk: 1,
            load: 2,
            dout: Some(3),
            din_level: Level::Low,
            clk_level: Level::Low,
            load_level: Level::High,
            dout_level: Level::Low,
            shift: 0,
            rows: [0; MAX7219_ROWS],
            shutdown: false,
            scan_limit: 7,
            intensity: 8,
            decode: 0,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [din, clk, load] = pins(part, ["DIN", "CLK", "LOAD"])?;
        Ok(Box::new(Self {
            din,
            clk,
            load,
            dout: index(part, "DOUT"),
            ..Self::new()
        }))
    }

    /// The row registers, register 1 first, bit 7 leftmost.
    pub fn rows(&self) -> [u8; MAX7219_ROWS] {
        self.rows
    }

    /// One pixel, `x` counted from the left and `y` from the top.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        y < MAX7219_ROWS && x < 8 && self.rows[y] & (1 << (7 - x)) != 0
    }

    pub fn lit_pixels(&self) -> usize {
        self.rows.iter().map(|row| row.count_ones() as usize).sum()
    }

    pub fn shutdown(&self) -> bool {
        self.shutdown
    }

    pub fn scan_limit(&self) -> u8 {
        self.scan_limit
    }

    pub fn intensity(&self) -> u8 {
        self.intensity
    }

    pub fn decode_mode(&self) -> u8 {
        self.decode
    }

    fn word(&mut self, word: u16) {
        let register = (word >> 8) as u8;
        let value = word as u8;
        match register {
            0x01..=0x08 => self.rows[usize::from(register - 1)] = value,
            0x09 => self.decode = value,
            0x0a => self.intensity = value & 0x0f,
            0x0b => self.scan_limit = value & 0x07,
            0x0c => self.shutdown = value == 0,
            0x0f => {
                // As in the bus model, display test fills or clears the rows.
                if value != 0 {
                    self.rows.fill(0xff);
                } else {
                    self.rows.fill(0);
                }
            }
            _ => {}
        }
    }
}

impl Default for Max7219Single {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for Max7219Single {
    fn drive(&self, pin: PinId) -> Drive {
        if self.dout == Some(pin.0) {
            strong(self.dout_level)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.din {
            self.din_level = level;
        } else if pin.0 == self.clk {
            let rising = level == Level::High && self.clk_level != Level::High;
            self.clk_level = level;
            // LOAD high ends a transfer; data is ignored until it falls again.
            if rising && self.load_level != Level::High {
                let outgoing = self.shift >> 15 != 0;
                self.shift = (self.shift << 1) | u16::from(self.din_level == Level::High);
                self.dout_level = if outgoing { Level::High } else { Level::Low };
            }
        } else if pin.0 == self.load {
            let rising = level == Level::High && self.load_level != Level::High;
            self.load_level = level;
            if rising {
                let word = self.shift;
                self.word(word);
                self.shift = 0;
            }
        }
    }

    fn state(&self) -> PartState {
        PartState::Led {
            lit: !self.shutdown && self.lit_pixels() != 0,
        }
    }
}

// ------------------------------------------------------ IR emitting diode

/// An infrared emitting diode: an LED that happens to radiate at 940 nm.
///
/// The digital runtime does not couple the light it emits to another part, so
/// this model is an ordinary LED: lit when the anode is high and the cathode
/// low. The catalog's [`IrReceiver`](crate::sensors::IrReceiver) decodes NEC
/// edges on its own data line; a test injects a code by driving that line with
/// the receiver's `remote` control, never through this emitter.
pub struct IrEmittingDiode {
    a: u16,
    c: u16,
    levels: [Level; 2],
    lit: bool,
    infrared: bool,
}

impl IrEmittingDiode {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [a, c] = pins(part, ["A", "C"])?;
        Ok(Box::new(Self {
            a,
            c,
            levels: [Level::HighZ; 2],
            lit: false,
            infrared: attr_bool(&part.attrs, "infrared", true),
        }))
    }

    pub fn is_lit(&self) -> bool {
        self.lit
    }

    /// Whether the manifest describes an infrared die rather than a visible one.
    pub fn infrared(&self) -> bool {
        self.infrared
    }
}

impl Part for IrEmittingDiode {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.a {
            self.levels[0] = level;
        } else if pin.0 == self.c {
            self.levels[1] = level;
        }
        self.lit = self.levels[0] == Level::High && self.levels[1] == Level::Low;
    }

    fn state(&self) -> PartState {
        PartState::Led { lit: self.lit }
    }
}

// -------------------------------------------------- latching pushbutton

/// A push-on, push-off latching switch with the same four legs as a tactile
/// button. Each rising press toggles the contact; releasing it does nothing.
///
/// The existing `pushbutton` engine is momentary and its manifest has no field
/// that could request latching, which is why this part has its own engine.
pub struct LatchingPushbutton {
    /// `[1.l, 1.r, 2.l, 2.r]`
    legs: [u16; 4],
    levels: [Level; 4],
    latched: bool,
    held: bool,
}

impl LatchingPushbutton {
    pub fn new() -> Self {
        Self {
            legs: [0, 1, 2, 3],
            levels: [Level::HighZ; 4],
            latched: false,
            held: false,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let legs = pins(part, ["1.l", "1.r", "2.l", "2.r"])?;
        Ok(Box::new(Self {
            legs,
            levels: [Level::HighZ; 4],
            latched: false,
            held: false,
        }))
    }

    pub fn is_latched(&self) -> bool {
        self.latched
    }

    fn contact(&self, pin: u16) -> Option<usize> {
        self.legs.iter().position(|&leg| leg == pin).map(|i| i / 2)
    }

    fn level_of_contact(&self, contact: usize) -> Level {
        let a = self.levels[contact * 2];
        if a != Level::HighZ {
            a
        } else {
            self.levels[contact * 2 + 1]
        }
    }

    /// One physical press: the falling edge of the actuator toggles the latch.
    fn actuate(&mut self, pressed: bool) {
        if pressed {
            if !self.held {
                self.latched = !self.latched;
            }
            self.held = true;
        } else {
            self.held = false;
        }
    }
}

impl Default for LatchingPushbutton {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for LatchingPushbutton {
    fn drive(&self, pin: PinId) -> Drive {
        if !self.latched {
            return Drive::HIGH_Z;
        }
        match self.contact(pin.0) {
            Some(0) => strong(self.level_of_contact(1)),
            Some(1) => strong(self.level_of_contact(0)),
            _ => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(i) = self.legs.iter().position(|&leg| leg == pin.0) {
            self.levels[i] = level;
            self.levels[i ^ 1] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Button {
            pressed: self.latched,
        }
    }

    fn press(&mut self, pressed: bool) -> bool {
        self.actuate(pressed);
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        if name != "pressed" {
            return false;
        }
        self.actuate(crate::parts::truthy(value));
        true
    }
}

// ------------------------------------------------------------ analog shells
//
// The analog view in `crate::analog` decides what these parts do electrically
// from the engine name, the attributes and the resolved nets. The pin-level
// side only has to exist: a capacitor is an open circuit at DC, an inductor is
// a short, and the three active devices contribute no digital drive of their
// own (the analog solver supplies their terminal behaviour).

/// A capacitor. At DC it is an open circuit, so it never drives a net.
pub struct Capacitor;

impl Capacitor {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let _ = pins(part, ["1", "2"])?;
        Ok(Box::new(Self))
    }
}

impl Part for Capacitor {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

/// An inductor. At DC it is a short, which in the digital model is a weak
/// pass-through between its two terminals.
pub struct Inductor {
    a: u16,
    b: u16,
    la: Level,
    lb: Level,
}

impl Inductor {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [a, b] = pins(part, ["1", "2"])?;
        Ok(Box::new(Self {
            a,
            b,
            la: Level::HighZ,
            lb: Level::HighZ,
        }))
    }
}

impl Part for Inductor {
    fn drive(&self, pin: PinId) -> Drive {
        let other = if pin.0 == self.a {
            self.lb
        } else if pin.0 == self.b {
            self.la
        } else {
            Level::HighZ
        };
        if other == Level::HighZ {
            Drive::HIGH_Z
        } else {
            Drive::weak(other)
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.a {
            self.la = level;
        } else if pin.0 == self.b {
            self.lb = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

/// A bipolar transistor shell: the analog model drives C/B/E.
pub struct Bjt;

impl Bjt {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let _ = pins(part, ["C", "B", "E"])?;
        Ok(Box::new(Self))
    }
}

impl Part for Bjt {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

/// A MOSFET shell: the analog model drives D/G/S.
pub struct Mosfet;

impl Mosfet {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let _ = pins(part, ["D", "G", "S"])?;
        Ok(Box::new(Self))
    }
}

impl Part for Mosfet {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

/// An operational amplifier shell: the analog model drives OUT from + and -.
pub struct Opamp;

impl Opamp {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let _ = pins(part, ["+", "-", "OUT"])?;
        Ok(Box::new(Self))
    }
}

impl Part for Opamp {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------- batch 3 analog shells
//
// These parts have no digital behaviour: the analog view in `crate::analog`
// decides what they do electrically from the engine name and the attributes.
// The pin-level side only has to exist so `Board::new` accepts the part and so
// a power or ground pin stays an endpoint on its net. Every one of them holds
// its pins high impedance.

/// A diode shell: `A` and `C` are the junction the analog solver models.
pub struct Diode;

impl Diode {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let _ = pins(part, ["A", "C"])?;
        Ok(Box::new(Self))
    }
}

impl Part for Diode {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

/// A battery: an ideal source between `+` and `-`, read by the analog view.
pub struct Battery {
    volts: f32,
}

impl Battery {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let _ = pins(part, ["+", "-"])?;
        Ok(Box::new(Self {
            volts: attr_f32(&part.attrs, "voltage", 1.5),
        }))
    }

    /// The terminal voltage in volts.
    pub fn volts(&self) -> f32 {
        self.volts
    }
}

impl Part for Battery {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

/// A bench power supply: the same ideal source, with a different default.
pub struct PowerSupply {
    positive: u16,
    volts: f32,
    limit: Option<f32>,
}

/// The highest voltage a host may dial a bench supply to, in volts. Beyond it
/// the analog view would be modelling a supply nobody puts on a bench.
const SUPPLY_MAX_VOLTS: f32 = 24.0;

/// The highest current limit a host may dial, in amperes.
const SUPPLY_MAX_AMPS: f32 = 20.0;

impl PowerSupply {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [positive, _] = pins(part, ["+", "-"])?;
        let limit = attr_f32(&part.attrs, "current_limit", 0.0);
        Ok(Box::new(Self {
            positive,
            volts: attr_f32(&part.attrs, "voltage", 5.0).clamp(0.0, SUPPLY_MAX_VOLTS),
            limit: (limit > 0.0).then(|| limit.min(SUPPLY_MAX_AMPS)),
        }))
    }

    /// The output voltage in volts.
    pub fn volts(&self) -> f32 {
        self.volts
    }

    /// The output current limit in amperes, if one is set.
    pub fn limit(&self) -> Option<f32> {
        self.limit
    }
}

impl Part for PowerSupply {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.positive).then_some(self.volts)
    }

    fn current_limit(&self) -> Option<f32> {
        self.limit
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        let Ok(value) = value.trim().parse::<f32>() else {
            return false;
        };
        if !value.is_finite() {
            return false;
        }
        match name {
            "voltage" | "volts" => {
                self.volts = value.clamp(0.0, SUPPLY_MAX_VOLTS);
                true
            }
            // Zero turns the limit off: a bench supply with no current dial.
            "current" | "current_limit" | "limit" => {
                self.limit = (value > 0.0).then(|| value.min(SUPPLY_MAX_AMPS));
                true
            }
            _ => false,
        }
    }
}

/// A three-terminal linear regulator (7805, 7812, 7905, LM317).
///
/// The shell carries the part's nominal output voltage, which is also what the
/// analog view regulates to; the drop-out behaviour lives in
/// [`crate::analog`]. An LM317's output is adjustable, so its shell keeps the
/// setting a host dials and the fixed variants ignore that control.
pub struct VoltageRegulator {
    variant: String,
    output: u16,
    output_volts: f32,
}

impl VoltageRegulator {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [_, _, output] = pins(part, ["VI", "GND", "VO"])?;
        let variant = part
            .attrs
            .get("variant")
            .cloned()
            .unwrap_or_else(|| "7805".into());
        let output_volts = match variant.trim() {
            "7812" => 12.0,
            "7905" => -5.0,
            "LM317" => attr_f32(&part.attrs, "voltage", 5.0).clamp(1.25, 24.0),
            _ => 5.0,
        };
        Ok(Box::new(Self {
            variant,
            output,
            output_volts,
        }))
    }

    pub fn variant(&self) -> &str {
        &self.variant
    }

    /// The nominal output voltage in volts.
    pub fn output_volts(&self) -> f32 {
        self.output_volts
    }

    fn adjustable(&self) -> bool {
        self.variant.trim() == "LM317"
    }
}

impl Part for VoltageRegulator {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.output).then_some(self.output_volts)
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        if !self.adjustable() {
            return false;
        }
        let Ok(value) = value.trim().parse::<f32>() else {
            return false;
        };
        if !value.is_finite() {
            return false;
        }
        match name {
            "voltage" | "volts" => {
                self.output_volts = value.clamp(1.25, 24.0);
                true
            }
            _ => false,
        }
    }
}

/// A step-down or step-up switching converter module.
///
/// It regulates its output to `voltage` from whatever its input holds: a buck
/// can only come down, so a setpoint above the input sags to the input; a
/// boost can only go up, so a setpoint below the input passes it through.
/// `max_amps` caps the output current the module can deliver.
pub struct DcDc {
    output: u16,
    output_volts: f32,
    max_amps: f32,
}

impl DcDc {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let [_, _, output] = pins(part, ["VI", "GND", "VO"])?;
        Ok(Box::new(Self {
            output,
            output_volts: attr_f32(&part.attrs, "voltage", 5.0).clamp(0.0, 48.0),
            max_amps: attr_f32(&part.attrs, "max_amps", 2.0).max(0.0),
        }))
    }
}

impl Part for DcDc {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        (pin.0 == self.output).then_some(self.output_volts)
    }

    fn current_limit(&self) -> Option<f32> {
        (self.max_amps > 0.0).then_some(self.max_amps)
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        let Ok(value) = value.trim().parse::<f32>() else {
            return false;
        };
        if !value.is_finite() {
            return false;
        }
        match name {
            "voltage" | "volts" => {
                self.output_volts = value.clamp(0.0, 48.0);
                true
            }
            _ => false,
        }
    }
}

/// A generic analog-only part with no modelled terminal behaviour.
///
/// It exists so the catalog can offer a part (a zener in reverse, an
/// optocoupler, anything whose electrical model is not in the netlist) before
/// the solver gains a model for it: the pins are endpoints, and the part never
/// drives a digital level.
pub struct AnalogShell;

impl AnalogShell {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        if part.pins.is_empty() {
            return Err(BuildError::Message(format!(
                "part {:?} has no pins",
                part.id
            )));
        }
        Ok(Box::new(Self))
    }
}

impl Part for AnalogShell {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------- 4-digit seven-segment
//
// A multiplexed module: four common terminals and one shared bank of eight
// segment anodes. The module itself keeps no latch — a firmware lights one
// digit at a time — so the model records the segment mask it saw while each
// digit was selected. That is exactly the state the glass holds between
// refreshes, and enough to read the displayed number back.

const SEGMENT_PINS_4: [&str; 8] = ["A", "B", "C", "D", "E", "F", "G", "DP"];

/// A four-digit, multiplexed seven-segment display (common cathode).
///
/// A digit is selected by pulling its `DIGn` pin low; the shared segment pins
/// then light the matching elements with a high level. The model stores the
/// last mask seen for each digit and reports the selected digit through
/// [`PartState::Segments`]. [`SevenSegment4Digit::text`] decodes all four
/// stored masks, which is what a test or a UI reads.
pub struct SevenSegment4Digit {
    digits: [u16; 4],
    segments: [u16; 8],
    digit_levels: [Level; 4],
    segment_levels: [Level; 8],
    shown: [u8; 4],
    active: Option<usize>,
}

impl SevenSegment4Digit {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut digits = [0u16; 4];
        for (number, digit) in digits.iter_mut().enumerate() {
            *digit = index(part, &format!("DIG{}", number + 1)).ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no DIG{} pin", part.id, number + 1))
            })?;
        }
        let mut segments = [0u16; 8];
        for (slot, name) in segments.iter_mut().zip(SEGMENT_PINS_4) {
            *slot = index(part, name).ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no {name:?} pin", part.id))
            })?;
        }
        Ok(Box::new(Self {
            digits,
            segments,
            digit_levels: [Level::HighZ; 4],
            segment_levels: [Level::HighZ; 8],
            shown: [0; 4],
            active: None,
        }))
    }

    /// The stored segment mask of one digit, `a`..`g` then `dp`.
    pub fn digit_mask(&self, digit: usize) -> Option<u8> {
        self.shown.get(digit).copied()
    }

    /// The four stored masks, in digit order.
    pub fn digits(&self) -> [u8; 4] {
        self.shown
    }

    /// The displayed four characters, decoded from the stored masks.
    pub fn text(&self) -> String {
        self.shown
            .iter()
            .map(|&mask| decode_segment(mask))
            .collect()
    }

    /// Which digit the module is currently lighting, if any.
    pub fn active_digit(&self) -> Option<usize> {
        self.active
    }

    fn refresh(&mut self) {
        // Common cathode: a low digit pin enables the column, and a high
        // segment lights. A digit that is not selected keeps the mask it was
        // last shown with, which is what persists between refreshes.
        match self
            .digit_levels
            .iter()
            .position(|level| *level == Level::Low)
        {
            Some(digit) => {
                self.shown[digit] = self.segment_mask(Level::High);
                self.active = Some(digit);
            }
            None => self.active = None,
        }
    }

    fn segment_mask(&self, lit: Level) -> u8 {
        self.segment_levels
            .iter()
            .enumerate()
            .fold(0u8, |mask, (bit, level)| {
                mask | (u8::from(*level == lit) << bit)
            })
    }
}

/// Decode one seven-segment mask to the character it draws.
fn decode_segment(mask: u8) -> char {
    match mask & 0x7f {
        0x3f => '0',
        0x06 => '1',
        0x5b => '2',
        0x4f => '3',
        0x66 => '4',
        0x6d => '5',
        0x7d => '6',
        0x07 => '7',
        0x7f => '8',
        0x6f => '9',
        0x77 => 'A',
        0x7c => 'b',
        0x39 => 'C',
        0x5e => 'd',
        0x79 => 'E',
        0x71 => 'F',
        0x40 => '-',
        0x00 => ' ',
        _ => '?',
    }
}

impl Part for SevenSegment4Digit {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(digit) = self.digits.iter().position(|&candidate| candidate == pin.0) {
            self.digit_levels[digit] = level;
        } else if let Some(segment) = self
            .segments
            .iter()
            .position(|&candidate| candidate == pin.0)
        {
            self.segment_levels[segment] = level;
        } else {
            return;
        }
        self.refresh();
    }

    fn state(&self) -> PartState {
        PartState::Segments {
            mask: self.active.map(|digit| self.shown[digit]).unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(id: u16) -> PinId {
        PinId(id)
    }

    #[test]
    fn a_dht22_frame_has_the_right_checksum() {
        // 65.0 % humidity and 23.5 °C.
        let mut sensor = Dht22::with_reading(650, 235);
        sensor.set_pin(pin(0), Level::Low, 0);
        sensor.set_pin(pin(0), Level::HighZ, 1_100_000);

        while let Some(at) = sensor.next_event() {
            sensor.advance(at);
        }

        let frame = sensor.last_frame().expect("the sensor answered");
        let checksum = frame[0]
            .wrapping_add(frame[1])
            .wrapping_add(frame[2])
            .wrapping_add(frame[3]);
        assert_eq!(frame[4], checksum, "frame {frame:?}");
        assert_eq!(
            sensor.reading(),
            Some(DhtReading {
                humidity_tenths: 650,
                temperature_tenths: 235
            })
        );
    }

    #[test]
    fn a_dht22_encodes_a_negative_temperature_with_the_sign_bit() {
        let mut sensor = Dht22::with_reading(400, -55);
        sensor.set_pin(pin(0), Level::Low, 0);
        sensor.set_pin(pin(0), Level::HighZ, 1_000_000);
        while let Some(at) = sensor.next_event() {
            sensor.advance(at);
        }
        let reading = sensor.reading().expect("a valid frame");
        assert_eq!(reading.temperature_tenths, -55);
    }

    #[test]
    fn a_dht22_ignores_a_short_start_pulse() {
        let mut sensor = Dht22::with_reading(500, 250);
        sensor.set_pin(pin(0), Level::Low, 0);
        sensor.set_pin(pin(0), Level::HighZ, 500_000);
        assert_eq!(sensor.next_event(), None);
    }

    #[test]
    fn an_hc_sr04_echo_width_encodes_distance() {
        let mut sensor = HcSr04::with_distance(100.0);
        sensor.set_pin(pin(0), Level::High, 0);
        sensor.set_pin(pin(0), Level::Low, 10_000);

        let start = sensor.next_event().expect("ECHO was scheduled");
        assert_eq!(start, 60_000, "ECHO follows TRIG by 50 us");
        sensor.advance(start);
        assert_eq!(sensor.drive(pin(1)), Drive::strong(Level::High));

        let end = sensor.next_event().expect("the falling edge was scheduled");
        assert_eq!(end, start + 5_800_000, "100 cm is 5.8 ms");
        sensor.advance(end);
        assert_eq!(sensor.drive(pin(1)), Drive::strong(Level::Low));
        assert_eq!(sensor.next_event(), None);
    }

    #[test]
    fn an_hc_sr04_control_changes_the_distance() {
        let mut sensor = HcSr04::with_distance(100.0);
        assert!(sensor.set_control(0));
        assert!((sensor.distance_cm() - 2.0).abs() < 0.01);
        assert!(sensor.set_control(1000));
        assert!((sensor.distance_cm() - 400.0).abs() < 0.01);
        assert_eq!(sensor.echo_width_ns(), 23_200_000);
    }

    #[test]
    fn a_pir_sensor_raises_its_output_on_motion() {
        let mut pir = PirMotionSensor::new();
        assert_eq!(pir.drive(pin(0)), Drive::strong(Level::Low));
        assert!(pir.set_control(800));
        assert_eq!(pir.drive(pin(0)), Drive::strong(Level::High));
        assert_eq!(pir.state(), PartState::Switch { on: true });
        assert!(pir.set_control(200));
        assert_eq!(pir.drive(pin(0)), Drive::strong(Level::Low));
    }

    #[test]
    fn a_photoresistor_dims_its_analog_output() {
        let mut ldr = Photoresistor::with_light(500);
        assert_eq!(ldr.analog(pin(0)), Some(1.65));
        assert_eq!(ldr.drive(pin(1)), Drive::strong(Level::High));
        assert!(ldr.set_control(100));
        let volts = ldr.analog(pin(0)).unwrap();
        assert!((volts - 0.33).abs() < 0.001, "{volts}");
        assert_eq!(ldr.drive(pin(1)), Drive::strong(Level::Low));
    }

    #[test]
    fn an_ntc_sensor_reports_a_voltage_for_temperature() {
        let mut ntc = NtcSensor::with_celsius(25.0);
        assert_eq!(ntc.temperature_celsius(), 25.0);
        assert!(
            (ntc.volts() - 3.3 * 450.0 / 1200.0).abs() < 0.001,
            "{}",
            ntc.volts()
        );

        assert!(ntc.set_control(1000));
        assert_eq!(ntc.temperature_celsius(), 100.0);
        assert!((ntc.volts() - 3.3).abs() < 0.001);
        assert_eq!(ntc.drive(pin(1)), Drive::strong(Level::High));
    }

    #[test]
    fn an_rgb_led_mixes_three_pins_to_a_colour() {
        let mut led = RgbLed::new();
        assert_eq!(led.color(), (0, 0, 0));
        led.set_pin(pin(0), Level::High, 0);
        led.set_pin(pin(1), Level::Low, 0);
        led.set_pin(pin(2), Level::High, 0);
        assert_eq!(led.color(), (255, 0, 255));
        assert_eq!(led.state(), PartState::Led { lit: true });
        led.set_pin(pin(0), Level::Low, 0);
        led.set_pin(pin(2), Level::Low, 0);
        assert_eq!(led.state(), PartState::Led { lit: false });
    }

    #[test]
    fn a_led_bar_graph_lights_a_count_of_segments() {
        let mut bar = LedBarGraph::new();
        for anode in 0..4 {
            bar.set_pin(pin(anode), Level::High, 0);
        }
        assert_eq!(bar.level(), 4);
        assert_eq!(bar.state(), PartState::Knob { permille: 400 });
        bar.set_pin(pin(2), Level::Low, 0);
        assert_eq!(bar.level(), 3);
    }

    #[test]
    fn a_relay_switches_the_common_to_no() {
        let mut relay = Relay::new();
        // Pin ids: coil 0, NO 1, NC 2, COM 3.
        relay.set_pin(pin(2), Level::High, 0);
        assert_eq!(
            relay.drive(pin(3)),
            Drive::strong(Level::High),
            "COM rests on NC"
        );

        relay.set_pin(pin(0), Level::High, 0);
        assert_eq!(relay.state(), PartState::Switch { on: true });
        assert_eq!(
            relay.drive(pin(3)),
            Drive::HIGH_Z,
            "COM now follows NO, which floats"
        );
        assert_eq!(relay.drive(pin(1)), Drive::HIGH_Z);

        relay.set_pin(pin(1), Level::Low, 0);
        assert_eq!(relay.drive(pin(3)), Drive::strong(Level::Low));
        assert_eq!(relay.drive(pin(2)), Drive::HIGH_Z, "NC is open");
    }

    #[test]
    fn an_l293d_drives_the_motor_outputs() {
        let mut driver = L293d::new();
        // IN1, IN2, EN, OUT1, OUT2.
        driver.set_pin(pin(2), Level::High, 0);
        driver.set_pin(pin(0), Level::High, 0);
        driver.set_pin(pin(1), Level::Low, 0);
        assert_eq!(driver.outputs(), (Level::High, Level::Low));
        assert_eq!(driver.drive(pin(3)), Drive::strong(Level::High));
        assert_eq!(driver.drive(pin(4)), Drive::strong(Level::Low));

        driver.set_pin(pin(1), Level::High, 0);
        assert_eq!(
            driver.outputs(),
            (Level::Low, Level::Low),
            "both inputs brake"
        );

        driver.set_pin(pin(2), Level::Low, 0);
        assert_eq!(
            driver.outputs(),
            (Level::HighZ, Level::HighZ),
            "disabled coasts"
        );
    }

    #[test]
    fn a_74hc165_shifts_its_parallel_byte() {
        let mut shift = Hc165::new();
        // D0 is pin 0 and D7 is pin 7; PL, CP, CE, DS, Q7, Q7_N follow.
        shift.set_pin(pin(7), Level::High, 0);
        shift.set_pin(pin(0), Level::High, 0);
        shift.set_pin(pin(8), Level::Low, 0); // PL low loads 0x81
        assert_eq!(shift.byte(), 0x81);
        shift.set_pin(pin(8), Level::High, 0);
        shift.set_pin(pin(10), Level::Low, 0); // CE low
        assert_eq!(shift.drive(pin(12)), Drive::strong(Level::High), "Q7 is D7");

        shift.set_pin(pin(9), Level::High, 0);
        assert_eq!(shift.byte(), 0x02);
        assert_eq!(shift.drive(pin(12)), Drive::strong(Level::Low), "Q7 is D6");
        for _ in 0..7 {
            shift.set_pin(pin(9), Level::Low, 0);
            shift.set_pin(pin(9), Level::High, 0);
        }
        assert_eq!(shift.byte(), 0x00);
    }

    #[test]
    fn a_capacitor_is_an_open_circuit_and_an_inductor_is_a_short() {
        assert_eq!(Capacitor.drive(pin(0)), Drive::HIGH_Z);
        let mut inductor = Inductor {
            a: 0,
            b: 1,
            la: Level::HighZ,
            lb: Level::HighZ,
        };
        inductor.set_pin(pin(1), Level::High, 0);
        assert_eq!(inductor.drive(pin(0)), Drive::weak(Level::High));
    }

    #[test]
    fn the_active_analog_shells_hold_their_pins_high_z() {
        for device in [&Bjt as &dyn Part, &Mosfet as &dyn Part, &Opamp as &dyn Part] {
            for pin_index in 0..3 {
                assert_eq!(device.drive(pin(pin_index)), Drive::HIGH_Z);
            }
        }
    }

    #[test]
    fn a_74hc138_decodes_three_bits() {
        let mut decoder = Hc138::new();
        // A0, A1, A2, E1, E2, E3; Y0 starts at 6.
        decoder.set_pin(pin(3), Level::Low, 0);
        decoder.set_pin(pin(4), Level::Low, 0);
        decoder.set_pin(pin(5), Level::High, 0);
        decoder.set_pin(pin(0), Level::High, 0);
        decoder.set_pin(pin(1), Level::Low, 0);
        decoder.set_pin(pin(2), Level::High, 0);
        assert_eq!(decoder.selected(), Some(5));
        assert_eq!(decoder.drive(pin(6 + 5)), Drive::strong(Level::Low));
        assert_eq!(decoder.drive(pin(6)), Drive::strong(Level::High));

        decoder.set_pin(pin(5), Level::Low, 0);
        assert_eq!(decoder.selected(), None);
        assert_eq!(decoder.drive(pin(6 + 5)), Drive::strong(Level::High));
    }

    #[test]
    fn the_analog_shells_hold_every_pin_high_z() {
        let part = |engine_pins: &[&str]| FlatPart {
            id: "x".into(),
            type_key: "x".into(),
            engine: crate::flatten::Engine::Builtin("x".into()),
            pins: engine_pins.iter().map(|name| (*name).to_string()).collect(),
            pin_kinds: vec![litz_diagram::PinKind::Passive; engine_pins.len()],
            attrs: Default::default(),
            buses: Vec::new(),
            device: None,
        };
        let diode = part(&["A", "C"]);
        let battery = part(&["+", "-"]);
        let regulator = part(&["VI", "GND", "VO"]);
        let shell = part(&["1", "2", "3", "4"]);
        for (built, pins) in [
            (Diode::build(&diode).unwrap() as Box<dyn Part>, 2),
            (Battery::build(&battery).unwrap(), 2),
            (PowerSupply::build(&battery).unwrap(), 2),
            (VoltageRegulator::build(&regulator).unwrap(), 3),
            (AnalogShell::build(&shell).unwrap(), 4),
        ] {
            for index in 0..pins {
                assert_eq!(built.drive(pin(index)), Drive::HIGH_Z, "pin {index}");
            }
            assert_eq!(built.state(), PartState::None);
        }
    }

    #[test]
    fn a_battery_shell_builds_from_its_voltage_attribute() {
        let part = FlatPart {
            id: "b".into(),
            type_key: "litz-battery".into(),
            engine: crate::flatten::Engine::Builtin("battery".into()),
            pins: vec!["+".into(), "-".into()],
            pin_kinds: vec![litz_diagram::PinKind::Power, litz_diagram::PinKind::Ground],
            attrs: [("voltage".to_string(), "9".to_string())]
                .into_iter()
                .collect(),
            buses: Vec::new(),
            device: None,
        };
        // The shell parses the attribute and holds it high impedance; reading
        // the parsed value is covered by the catalog integration test through
        // the analog solver, which sees the same attribute.
        let battery = Battery::build(&part).unwrap();
        assert_eq!(battery.state(), PartState::None);
        assert!(
            Battery::build(&FlatPart {
                pins: Vec::new(),
                ..part
            })
            .is_err()
        );
    }

    #[test]
    fn a_seven_segment_4digit_records_each_multiplexed_digit() {
        let mut display = SevenSegment4Digit {
            digits: [0, 1, 2, 3],
            segments: [4, 5, 6, 7, 8, 9, 10, 11],
            digit_levels: [Level::HighZ; 4],
            segment_levels: [Level::HighZ; 8],
            shown: [0; 4],
            active: None,
        };
        // Digit 1: segments b and c lit, the pattern for "1".
        display.set_pin(pin(5), Level::High, 0); // B
        display.set_pin(pin(6), Level::High, 0); // C
        display.set_pin(pin(0), Level::Low, 0);
        assert_eq!(display.state(), PartState::Segments { mask: 0b0000_0110 });
        assert_eq!(display.active_digit(), Some(0));
        assert_eq!(display.digit_mask(0), Some(0x06));

        // Digit 2: a, b, c, d, g lit, the pattern for "3".
        display.set_pin(pin(0), Level::High, 0);
        for (segment, on) in [
            (0, true),
            (1, true),
            (2, true),
            (3, true),
            (4, false),
            (5, false),
            (6, true),
        ] {
            display.set_pin(
                pin(4 + segment),
                if on { Level::High } else { Level::Low },
                0,
            );
        }
        display.set_pin(pin(1), Level::Low, 0);
        assert_eq!(display.digit_mask(1), Some(0x4f));
        assert_eq!(display.text(), "13  ");
        assert_eq!(display.state(), PartState::Segments { mask: 0x4f });
    }

    #[test]
    fn a_segment_mask_decodes_to_its_character() {
        for (mask, ch) in [
            (0x3f, '0'),
            (0x06, '1'),
            (0x5b, '2'),
            (0x4f, '3'),
            (0x66, '4'),
            (0x6d, '5'),
            (0x7d, '6'),
            (0x07, '7'),
            (0x7f, '8'),
            (0x6f, '9'),
            (0x00, ' '),
        ] {
            assert_eq!(decode_segment(mask), ch, "mask {mask:#04x}");
        }
    }

    #[test]
    fn a_74hc238_decodes_to_one_active_high_output() {
        let mut decoder = Hc238::new();
        // A0, A1, A2, E1, E2, E3; Y0 starts at 6.
        decoder.set_pin(pin(3), Level::Low, 0);
        decoder.set_pin(pin(4), Level::Low, 0);
        decoder.set_pin(pin(5), Level::High, 0);
        decoder.set_pin(pin(0), Level::High, 0);
        decoder.set_pin(pin(1), Level::Low, 0);
        decoder.set_pin(pin(2), Level::High, 0);
        assert_eq!(decoder.selected(), Some(5));
        assert_eq!(decoder.drive(pin(6 + 5)), Drive::strong(Level::High));
        assert_eq!(decoder.drive(pin(6)), Drive::strong(Level::Low));

        // E3 is active high: dropping it disables every output to low.
        decoder.set_pin(pin(5), Level::Low, 0);
        assert_eq!(decoder.selected(), None);
        assert_eq!(decoder.drive(pin(6 + 5)), Drive::strong(Level::Low));
    }

    #[test]
    fn a_74hc139_decodes_two_independent_banks() {
        let mut decoder = Hc139::new();
        // Bank 1: selects 0/1, enable 4, outputs 6..9.
        decoder.set_pin(pin(4), Level::Low, 0);
        decoder.set_pin(pin(1), Level::High, 0); // address 2
        assert_eq!(decoder.selected(0), Some(2));
        assert_eq!(decoder.drive(pin(6 + 2)), Drive::strong(Level::Low));
        assert_eq!(decoder.drive(pin(6)), Drive::strong(Level::High));

        // Bank 2 is independent and still disabled.
        assert_eq!(decoder.selected(1), None);
        assert_eq!(decoder.drive(pin(10)), Drive::strong(Level::High));
        decoder.set_pin(pin(5), Level::Low, 0);
        decoder.set_pin(pin(2), Level::High, 0); // address 1
        assert_eq!(decoder.selected(1), Some(1));
        assert_eq!(decoder.drive(pin(11)), Drive::strong(Level::Low));
    }

    fn shift_max7219(matrix: &mut Max7219Single, word: u16) {
        matrix.set_pin(pin(2), Level::Low, 0); // LOAD low opens the transfer
        for bit in (0..16).rev() {
            let level = if word & (1 << bit) != 0 {
                Level::High
            } else {
                Level::Low
            };
            matrix.set_pin(pin(0), level, 0); // DIN, most significant first
            matrix.set_pin(pin(1), Level::High, 0);
            matrix.set_pin(pin(1), Level::Low, 0);
        }
        matrix.set_pin(pin(2), Level::High, 0); // rising LOAD latches the word
    }

    #[test]
    fn a_max7219_single_latches_row_registers() {
        // DIN 0, CLK 1, LOAD 2, DOUT 3.
        let mut matrix = Max7219Single::new();
        shift_max7219(&mut matrix, 0x01ff); // register 1, all eight pixels
        assert_eq!(matrix.rows()[0], 0xff);
        assert!(matrix.pixel(0, 0));
        assert!(matrix.pixel(7, 0));
        assert!(!matrix.pixel(0, 1));
        assert_eq!(matrix.lit_pixels(), 8);
        assert_eq!(matrix.state(), PartState::Led { lit: true });

        shift_max7219(&mut matrix, 0x0c00); // shutdown register, value 0
        assert!(matrix.shutdown());
        assert_eq!(matrix.state(), PartState::Led { lit: false });
    }

    #[test]
    fn an_ir_emitting_diode_lights_like_an_led() {
        let mut diode = IrEmittingDiode {
            a: 0,
            c: 1,
            levels: [Level::HighZ; 2],
            lit: false,
            infrared: true,
        };
        diode.set_pin(pin(0), Level::High, 0);
        diode.set_pin(pin(1), Level::Low, 0);
        assert!(diode.is_lit());
        assert!(diode.infrared());
        assert_eq!(diode.state(), PartState::Led { lit: true });

        diode.set_pin(pin(1), Level::High, 0);
        assert!(!diode.is_lit());
    }

    #[test]
    fn a_latching_pushbutton_toggles_on_each_press() {
        let mut button = LatchingPushbutton::new();
        // Pin ids: contacts 1 and 2 on legs 0..3.
        button.set_pin(pin(3), Level::Low, 0);
        assert_eq!(button.drive(pin(0)), Drive::HIGH_Z, "open until latched");

        button.press(true);
        button.press(false);
        assert!(button.is_latched());
        assert_eq!(button.drive(pin(0)), Drive::strong(Level::Low));
        assert_eq!(button.state(), PartState::Button { pressed: true });

        button.press(true);
        button.press(false);
        assert!(!button.is_latched());
        assert_eq!(button.drive(pin(0)), Drive::HIGH_Z);
    }
}
