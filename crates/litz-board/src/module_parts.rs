//! Small pin-level parts that are mostly wiring: analog sensor modules, motor
//! and driver modules, switches and level translators.
//!
//! Each model observes the levels on its pins and drives the pins a real part
//! would drive. They take their simulated value from a manifest attribute or
//! [`Part::set_control`], never from a wall clock; the clock generator is the
//! one part here that schedules itself, and it does so through
//! [`Part::next_event`] in simulated nanoseconds.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, TICKS_PER_SEC, Tick};

fn index(part: &FlatPart, name: &str) -> Result<u16, BuildError> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {name:?} pin", part.id)))
}

fn opt_index(part: &FlatPart, name: &str) -> Option<u16> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
}

fn attr_f32(attrs: &std::collections::BTreeMap<String, String>, name: &str, default: f32) -> f32 {
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

/// Parse a frequency string: a number with an optional `k` (10^3) or
/// `m` (10^6) suffix, or a plain decimal.
fn parse_frequency(text: &str) -> f32 {
    let text = text.trim().to_ascii_lowercase();
    let (number, scale) = if let Some(number) = text.strip_suffix('k') {
        (number, 1_000.0)
    } else if let Some(number) = text.strip_suffix('m') {
        (number, 1_000_000.0)
    } else {
        (text.as_str(), 1.0)
    };
    number.trim().parse::<f32>().unwrap_or(10_000.0) * scale
}

fn strong(level: Level) -> Drive {
    match level {
        Level::HighZ => Drive::HIGH_Z,
        level => Drive::strong(level),
    }
}

fn weak(level: Level) -> Drive {
    match level {
        Level::HighZ => Drive::HIGH_Z,
        level => Drive::weak(level),
    }
}

// ------------------------------------------------------------------ analog modules

/// A three-pin analog sensor module: an analog voltage and a threshold output.
///
/// The parts that use it differ only in their manifest defaults and in the
/// polarity of the comparator output, which is why one engine serves soil
/// moisture, sound, vibration, rain and water-level modules. `value` is
/// permille of the module's range; the analog pin sources `vref` scaled by it.
pub struct AnalogSensor {
    analog: Option<u16>,
    digital: Option<u16>,
    value: u16,
    threshold: u16,
    active_low: bool,
    reference: f32,
}

impl AnalogSensor {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let value = attr_f32(&part.attrs, "value", 500.0)
            .round()
            .clamp(0.0, 1000.0) as u16;
        let threshold = attr_f32(&part.attrs, "threshold", 500.0)
            .round()
            .clamp(0.0, 1000.0) as u16;
        Ok(Box::new(Self {
            analog: opt_index(part, "AO"),
            digital: opt_index(part, "DO"),
            value,
            threshold,
            active_low: attr_bool(&part.attrs, "activeLow", false),
            reference: attr_f32(&part.attrs, "vref", 5.0).max(0.0),
        }))
    }

    pub fn value(&self) -> u16 {
        self.value
    }

    pub fn volts(&self) -> f32 {
        self.reference * f32::from(self.value) / 1000.0
    }

    fn asserted(&self) -> bool {
        self.value >= self.threshold
    }
}

impl Part for AnalogSensor {
    fn drive(&self, pin: PinId) -> Drive {
        let Some(digital) = self.digital else {
            return Drive::HIGH_Z;
        };
        if pin.0 != digital {
            return Drive::HIGH_Z;
        }
        let active = self.asserted();
        let level = if self.active_low {
            if active { Level::Low } else { Level::High }
        } else if active {
            Level::High
        } else {
            Level::Low
        };
        strong(level)
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

    fn analog(&self, pin: PinId) -> Option<f32> {
        (Some(pin.0) == self.analog).then(|| self.volts())
    }
}

// ------------------------------------------------------------------ DC motor

/// A brushed DC motor driven by two direction inputs and a PWM input.
///
/// `IN1` high with `IN2` low turns forward, the other way round reverses, and
/// both low coasts. The PWM pin is measured, not assumed: the duty cycle of the
/// high period between falling edges is the speed, so a firmware using
/// `analogWrite` produces a speed the same way the real bridge would.
pub struct DcMotor {
    in1: u16,
    in2: u16,
    pwm: u16,
    levels: [Level; 3],
    pwm_rise: Option<Tick>,
    pwm_last_fall: Option<Tick>,
    duty: u16,
    host: Option<i32>,
}

impl DcMotor {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            in1: index(part, "IN1")?,
            in2: index(part, "IN2")?,
            pwm: index(part, "PWM")?,
            levels: [Level::HighZ; 3],
            pwm_rise: None,
            pwm_last_fall: None,
            duty: 0,
            host: None,
        }))
    }

    /// Signed speed in permille, `-1000` to `1000`.
    pub fn speed(&self) -> i32 {
        if let Some(host) = self.host {
            return host;
        }
        let direction = match (self.levels[0] == Level::High, self.levels[1] == Level::High) {
            (true, false) => 1,
            (false, true) => -1,
            _ => 0,
        };
        direction * i32::from(self.duty)
    }

    fn measured_duty(&mut self, now: Tick, level: Level) {
        if level == Level::High {
            self.pwm_rise = Some(now);
            // A pin held high with no measured period is full speed. Keeping
            // the previous duty until a full cycle arrives would make a steady
            // high read as speed zero.
            if self.pwm_last_fall.is_none() {
                self.duty = 1000;
            }
        } else if let Some(rise) = self.pwm_rise.take() {
            if let Some(last) = self.pwm_last_fall {
                let period = now.saturating_sub(last);
                let high = now.saturating_sub(rise);
                if let Some(duty) = (high * 1000).checked_div(period) {
                    self.duty = duty.min(1000) as u16;
                }
            }
            self.pwm_last_fall = Some(now);
        }
    }
}

impl Part for DcMotor {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 == self.in1 {
            self.levels[0] = level;
        } else if pin.0 == self.in2 {
            self.levels[1] = level;
        } else if pin.0 == self.pwm {
            self.levels[2] = level;
            self.measured_duty(now, level);
        }
    }

    fn state(&self) -> PartState {
        let speed = self.speed();
        let permille = (500 + speed / 2).clamp(0, 1000) as u16;
        PartState::Knob { permille }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let value = i32::from(value.min(1000));
        self.host = Some(if value >= 500 {
            (value - 500) * 2
        } else {
            -(500 - value) * 2
        });
        true
    }
}

// ------------------------------------------------------------------ vibration motor

/// A two-terminal vibration motor: it runs while its positive pin is high.
pub struct VibrationMotor {
    positive: u16,
    negative: u16,
    levels: [Level; 2],
}

impl VibrationMotor {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            positive: index(part, "1")?,
            negative: index(part, "2")?,
            levels: [Level::HighZ; 2],
        }))
    }

    pub fn running(&self) -> bool {
        self.levels[0] == Level::High && self.levels[1] == Level::Low
    }
}

impl Part for VibrationMotor {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.positive {
            self.levels[0] = level;
        } else if pin.0 == self.negative {
            self.levels[1] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Led {
            lit: self.running(),
        }
    }
}

// ------------------------------------------------------------------ traffic light

/// A traffic-light module: three independent lamps on the red, yellow and
/// green inputs. The model reports "on" when any lamp is lit.
pub struct TrafficLight {
    lamps: [u16; 3],
    levels: [Level; 3],
}

impl TrafficLight {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            lamps: [index(part, "R")?, index(part, "Y")?, index(part, "G")?],
            levels: [Level::HighZ; 3],
        }))
    }

    pub fn lit(&self) -> bool {
        self.levels.contains(&Level::High)
    }
}

impl Part for TrafficLight {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(lamp) = self.lamps.iter().position(|&candidate| candidate == pin.0) {
            self.levels[lamp] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Led { lit: self.lit() }
    }
}

// ------------------------------------------------------------------ clock generator

/// A free-running square wave. The edge schedule is simulated time, so a
/// firmware that counts interrupts sees the rate the manifest asked for.
pub struct ClockGenerator {
    out: u16,
    hz: u32,
    high: bool,
    next_at: Option<Tick>,
}

impl ClockGenerator {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let hz = part
            .attrs
            .get("frequency")
            .map(|text| parse_frequency(text))
            .unwrap_or(10_000.0)
            .round()
            .clamp(1.0, 100_000.0) as u32;
        let mut generator = Self {
            out: index(part, "CLK")?,
            hz,
            high: true,
            next_at: None,
        };
        generator.next_at = Some(generator.half_period());
        Ok(Box::new(generator))
    }

    pub fn frequency(&self) -> u32 {
        self.hz
    }

    fn half_period(&self) -> Tick {
        (TICKS_PER_SEC / Tick::from(self.hz.max(1)) / 2).max(1)
    }
}

impl Part for ClockGenerator {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.out {
            strong(if self.high { Level::High } else { Level::Low })
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next_at
    }

    fn advance(&mut self, now: Tick) {
        let half = self.half_period();
        let Some(at) = self.next_at else { return };
        if now < at {
            return;
        }
        // Collapse a late wake-up into one parity flip instead of walking every
        // edge; a paused simulator should not replay minutes of toggles.
        let toggles = (now - at) / half + 1;
        if toggles % 2 == 1 {
            self.high = !self.high;
        }
        self.next_at = Some(at + toggles * half);
    }

    fn state(&self) -> PartState {
        PartState::Tone {
            sounding: self.high,
            hz: self.hz,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.hz = 1 + u32::from(value.min(1000)) * 99_999 / 1000;
        self.next_at = Some(self.half_period());
        true
    }
}

// ------------------------------------------------------------------ A4988

/// The four full-step coil phases, as `(OUT1A, OUT1B, OUT2A, OUT2B)`.
const STEP_PHASES: [(bool, bool, bool, bool); 4] = [
    (true, false, true, false),
    (true, false, false, true),
    (false, true, false, true),
    (false, true, true, false),
];

/// An A4988 stepper driver: STEP pulses move the coil phase, DIR picks the way
/// and the microstep pins scale one pulse. `ENABLE`, `RESET` and `SLEEP` are
/// all active low, as on the chip; a floating pin is not asserted.
pub struct A4988 {
    step: u16,
    dir: u16,
    enable: u16,
    reset: u16,
    sleep: u16,
    microstep_pins: [u16; 3],
    output_pins: [u16; 4],
    dir_level: Level,
    enable_level: Level,
    reset_level: Level,
    sleep_level: Level,
    ms_levels: [Level; 3],
    step_level: Level,
    position: i64,
}

impl A4988 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            step: index(part, "STEP")?,
            dir: index(part, "DIR")?,
            enable: index(part, "ENABLE")?,
            reset: index(part, "RESET")?,
            sleep: index(part, "SLEEP")?,
            microstep_pins: [
                index(part, "MS1")?,
                index(part, "MS2")?,
                index(part, "MS3")?,
            ],
            output_pins: [
                index(part, "1A")?,
                index(part, "1B")?,
                index(part, "2A")?,
                index(part, "2B")?,
            ],
            dir_level: Level::HighZ,
            enable_level: Level::HighZ,
            reset_level: Level::HighZ,
            sleep_level: Level::HighZ,
            ms_levels: [Level::HighZ; 3],
            step_level: Level::HighZ,
            position: 0,
        }))
    }

    pub fn position(&self) -> i64 {
        self.position
    }

    fn enabled(&self) -> bool {
        self.enable_level != Level::High
            && self.reset_level != Level::Low
            && self.sleep_level != Level::Low
    }

    fn microstep(&self) -> i64 {
        let code = usize::from(self.ms_levels[0] == Level::High)
            | usize::from(self.ms_levels[1] == Level::High) << 1
            | usize::from(self.ms_levels[2] == Level::High) << 2;
        match code {
            1 => 2,
            2 => 4,
            3 => 8,
            7 => 16,
            _ => 1,
        }
    }

    fn phase(&self) -> usize {
        let microstep = self.microstep().max(1);
        self.position.div_euclid(microstep).rem_euclid(4) as usize
    }
}

impl Part for A4988 {
    fn drive(&self, pin: PinId) -> Drive {
        let phase = STEP_PHASES[self.phase()];
        let levels = [phase.0, phase.1, phase.2, phase.3];
        self.output_pins
            .iter()
            .position(|&output| output == pin.0)
            .map(|slot| {
                strong(if levels[slot] {
                    Level::High
                } else {
                    Level::Low
                })
            })
            .unwrap_or(Drive::HIGH_Z)
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.step {
            let rising = level == Level::High && self.step_level != Level::High;
            self.step_level = level;
            if rising && self.enabled() {
                let microstep = self.microstep();
                let direction = if self.dir_level == Level::High { 1 } else { -1 };
                self.position += direction * microstep;
            }
            return;
        }
        if pin.0 == self.dir {
            self.dir_level = level;
        } else if pin.0 == self.enable {
            self.enable_level = level;
        } else if pin.0 == self.reset {
            self.reset_level = level;
        } else if pin.0 == self.sleep {
            self.sleep_level = level;
        } else if let Some(slot) = self
            .microstep_pins
            .iter()
            .position(|&candidate| candidate == pin.0)
        {
            self.ms_levels[slot] = level;
        }
    }

    fn state(&self) -> PartState {
        let permille = (self.position.rem_euclid(200) * 1000 / 200) as u16;
        PartState::Knob { permille }
    }
}

// ------------------------------------------------------------------ DPDT relay

/// A two-pole double-throw relay. Both poles follow the same coil, which is
/// energized when `COIL1` is high and `COIL2` is low.
pub struct DpdtRelay {
    coil: [u16; 2],
    coil_levels: [Level; 2],
    poles: [[u16; 3]; 2],
    levels: [[Level; 3]; 2],
    energized: bool,
}

impl DpdtRelay {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut poles = [[0u16; 3]; 2];
        for (number, pole) in poles.iter_mut().enumerate() {
            let suffix = number + 1;
            *pole = [
                index(part, &format!("NC{suffix}"))?,
                index(part, &format!("P{suffix}"))?,
                index(part, &format!("NO{suffix}"))?,
            ];
        }
        Ok(Box::new(Self {
            coil: [index(part, "COIL1")?, index(part, "COIL2")?],
            coil_levels: [Level::HighZ; 2],
            poles,
            levels: [[Level::HighZ; 3]; 2],
            energized: false,
        }))
    }

    pub fn energized(&self) -> bool {
        self.energized
    }

    fn selected(&self) -> usize {
        if self.energized { 2 } else { 0 }
    }
}

impl Part for DpdtRelay {
    fn drive(&self, pin: PinId) -> Drive {
        for (pole, contacts) in self.poles.iter().enumerate() {
            let selected = self.selected();
            if pin.0 == contacts[1] {
                return strong(self.levels[pole][selected]);
            }
            if pin.0 == contacts[selected] {
                return strong(self.levels[pole][1]);
            }
        }
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.coil[0] {
            self.coil_levels[0] = level;
            self.energized =
                self.coil_levels[0] == Level::High && self.coil_levels[1] != Level::High;
            return;
        }
        if pin.0 == self.coil[1] {
            self.coil_levels[1] = level;
            self.energized =
                self.coil_levels[0] == Level::High && self.coil_levels[1] != Level::High;
            return;
        }
        for (pole, contacts) in self.poles.iter().enumerate() {
            if let Some(role) = contacts.iter().position(|&contact| contact == pin.0) {
                self.levels[pole][role] = level;
            }
        }
    }

    fn state(&self) -> PartState {
        PartState::Switch { on: self.energized }
    }
}

// ------------------------------------------------------------------ ULN2003

/// A seven-channel Darlington array. Each input that goes high pulls its open
/// collector output low; a released input lets go, which is why the outputs are
/// high-impedance rather than actively high.
pub struct Uln2003 {
    inputs: [u16; 7],
    outputs: [u16; 7],
    levels: [Level; 7],
}

impl Uln2003 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut inputs = [0u16; 7];
        let mut outputs = [0u16; 7];
        for (channel, (input, output)) in inputs.iter_mut().zip(outputs.iter_mut()).enumerate() {
            *input = index(part, &format!("IN{}", channel + 1))?;
            *output = index(part, &format!("OUT{}", channel + 1))?;
        }
        Ok(Box::new(Self {
            inputs,
            outputs,
            levels: [Level::HighZ; 7],
        }))
    }

    pub fn driving(&self, channel: usize) -> bool {
        channel < 7 && self.levels[channel] == Level::High
    }
}

impl Part for Uln2003 {
    fn drive(&self, pin: PinId) -> Drive {
        match self.outputs.iter().position(|&output| output == pin.0) {
            Some(channel) if self.levels[channel] == Level::High => Drive::strong(Level::Low),
            _ => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(channel) = self.inputs.iter().position(|&input| input == pin.0) {
            self.levels[channel] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Led {
            lit: self.levels.contains(&Level::High),
        }
    }
}

// ------------------------------------------------------------------ 74HC4067

/// A 16-channel analog multiplexer. The four select pins choose one channel,
/// `E` (active high here, matching the chip's inhibit) releases the common pin,
/// and the selected channel and the common are one conductor.
pub struct Cd4067 {
    selects: [u16; 4],
    inhibit: u16,
    common: u16,
    channels: [u16; 16],
    select_levels: [Level; 4],
    inhibit_level: Level,
    common_level: Level,
    channel_levels: [Level; 16],
}

impl Cd4067 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut channels = [0u16; 16];
        for (number, channel) in channels.iter_mut().enumerate() {
            *channel = index(part, &format!("I{number}"))?;
        }
        let mut selects = [0u16; 4];
        for (number, select) in selects.iter_mut().enumerate() {
            *select = index(part, &format!("S{number}"))?;
        }
        Ok(Box::new(Self {
            selects,
            inhibit: index(part, "E")?,
            common: index(part, "COM")?,
            channels,
            select_levels: [Level::HighZ; 4],
            inhibit_level: Level::HighZ,
            common_level: Level::HighZ,
            channel_levels: [Level::HighZ; 16],
        }))
    }

    pub fn selected(&self) -> Option<u8> {
        if self.inhibit_level == Level::High {
            return None;
        }
        let address = self
            .select_levels
            .iter()
            .enumerate()
            .fold(0usize, |address, (bit, level)| {
                address | usize::from(*level == Level::High) << bit
            });
        Some(address as u8)
    }
}

impl Part for Cd4067 {
    fn drive(&self, pin: PinId) -> Drive {
        let Some(channel) = self.selected().map(usize::from) else {
            return Drive::HIGH_Z;
        };
        if pin.0 == self.common {
            weak(self.channel_levels[channel])
        } else if pin.0 == self.channels[channel] {
            weak(self.common_level)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.common {
            self.common_level = level;
        } else if let Some(channel) = self
            .channels
            .iter()
            .position(|&candidate| candidate == pin.0)
        {
            self.channel_levels[channel] = level;
        } else if let Some(select) = self
            .selects
            .iter()
            .position(|&candidate| candidate == pin.0)
        {
            self.select_levels[select] = level;
        } else if pin.0 == self.inhibit {
            self.inhibit_level = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ fuse

/// A fuse: a conductor until its rating is exceeded. The model has no current,
/// so a host control or a test blows it explicitly.
pub struct Fuse {
    a: u16,
    b: u16,
    a_level: Level,
    b_level: Level,
    blown: bool,
}

impl Fuse {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            a: index(part, "1")?,
            b: index(part, "2")?,
            a_level: Level::HighZ,
            b_level: Level::HighZ,
            blown: false,
        }))
    }

    pub fn blown(&self) -> bool {
        self.blown
    }
}

impl Part for Fuse {
    fn drive(&self, pin: PinId) -> Drive {
        if self.blown {
            return Drive::HIGH_Z;
        }
        if pin.0 == self.a {
            weak(self.b_level)
        } else if pin.0 == self.b {
            weak(self.a_level)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.a {
            self.a_level = level;
        } else if pin.0 == self.b {
            self.b_level = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Switch { on: !self.blown }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.blown = value >= 500;
        true
    }
}

// ------------------------------------------------------------------ DIP bank

/// A DIP switch bank with any number of positions, inferred from its pins.
///
/// The eight-position part has its own engine; this one exists so a four-way
/// bank does not need a second copy of the same contact logic.
pub struct DipBank {
    contacts: Vec<(u16, u16)>,
    levels: Vec<(Level, Level)>,
    closed: u64,
}

impl DipBank {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut contacts = Vec::new();
        let mut number = 1;
        while let Some(first) = opt_index(part, &format!("{number}a")) {
            let second = index(part, &format!("{number}b"))?;
            contacts.push((first, second));
            number += 1;
        }
        if contacts.is_empty() {
            return Err(BuildError::Message(format!(
                "part {:?} has no Na/Nb switch contacts",
                part.id
            )));
        }
        let levels = vec![(Level::HighZ, Level::HighZ); contacts.len()];
        Ok(Box::new(Self {
            contacts,
            levels,
            closed: 0,
        }))
    }

    pub fn mask(&self) -> u64 {
        self.closed
    }

    pub fn is_closed(&self, position: usize) -> bool {
        position < 64 && self.closed & (1 << position) != 0
    }

    pub fn set_switch(&mut self, position: usize, closed: bool) {
        if position >= self.contacts.len() {
            return;
        }
        if closed {
            self.closed |= 1 << position;
        } else {
            self.closed &= !(1 << position);
        }
    }

    fn position(&self, pin: u16) -> Option<(usize, bool)> {
        self.contacts
            .iter()
            .enumerate()
            .find_map(|(slot, &(first, second))| {
                if first == pin {
                    Some((slot, false))
                } else if second == pin {
                    Some((slot, true))
                } else {
                    None
                }
            })
    }
}

impl Part for DipBank {
    fn drive(&self, pin: PinId) -> Drive {
        let Some((slot, second)) = self.position(pin.0) else {
            return Drive::HIGH_Z;
        };
        if !self.is_closed(slot) {
            return Drive::HIGH_Z;
        }
        let (first, second_level) = self.levels[slot];
        strong(if second { first } else { second_level })
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        let Some((slot, _second)) = self.position(pin.0) else {
            return;
        };
        // Both legs of one position are the same conductor.
        self.levels[slot] = (level, level);
    }

    fn state(&self) -> PartState {
        PartState::Switch {
            on: self.closed != 0,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.closed = u64::from(value & 0x3ff);
        true
    }
}

// ------------------------------------------------------------------ level shifter

/// A four-channel bidirectional logic-level translator.
///
/// Without voltage domains the useful model is a conductor: the logic level on
/// one side appears weakly on the other, so a 3.3 V GPIO reaches a 5 V input
/// and a strong driver anywhere on the pair still wins.
pub struct LevelShifter {
    channels: Vec<(u16, u16)>,
    levels: Vec<(Level, Level)>,
}

impl LevelShifter {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut channels = Vec::new();
        let mut number = 1;
        while let Some(low) = opt_index(part, &format!("LV{number}")) {
            let high = index(part, &format!("HV{number}"))?;
            channels.push((low, high));
            number += 1;
        }
        if channels.is_empty() {
            return Err(BuildError::Message(format!(
                "part {:?} has no LVn/HVn channel pairs",
                part.id
            )));
        }
        let levels = vec![(Level::HighZ, Level::HighZ); channels.len()];
        Ok(Box::new(Self { channels, levels }))
    }

    fn channel(&self, pin: u16) -> Option<(usize, bool)> {
        self.channels
            .iter()
            .enumerate()
            .find_map(|(slot, &(low, high))| {
                if low == pin {
                    Some((slot, false))
                } else if high == pin {
                    Some((slot, true))
                } else {
                    None
                }
            })
    }
}

impl Part for LevelShifter {
    fn drive(&self, pin: PinId) -> Drive {
        match self.channel(pin.0) {
            Some((slot, false)) => weak(self.levels[slot].1),
            Some((slot, true)) => weak(self.levels[slot].0),
            None => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some((slot, high)) = self.channel(pin.0) {
            if high {
                self.levels[slot].1 = level;
            } else {
                self.levels[slot].0 = level;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(id: u16) -> PinId {
        PinId(id)
    }

    fn sensor(value: u16, threshold: u16, active_low: bool) -> AnalogSensor {
        AnalogSensor {
            analog: Some(1),
            digital: Some(0),
            value,
            threshold,
            active_low,
            reference: 5.0,
        }
    }

    #[test]
    fn an_analog_sensor_module_crosses_its_threshold_both_ways() {
        let mut module = sensor(200, 500, false);
        assert_eq!(module.drive(pin(0)), Drive::strong(Level::Low));
        assert!((module.analog(pin(1)).unwrap() - 1.0).abs() < 0.001);
        module.set_control(900);
        assert_eq!(module.drive(pin(0)), Drive::strong(Level::High));
        assert_eq!(module.analog(pin(0)), None, "only AO sources a voltage");
    }

    #[test]
    fn an_active_low_module_inverts_its_digital_output() {
        let mut module = sensor(900, 500, true);
        assert_eq!(module.drive(pin(0)), Drive::strong(Level::Low));
        module.set_control(100);
        assert_eq!(module.drive(pin(0)), Drive::strong(Level::High));
    }

    fn motor() -> DcMotor {
        DcMotor {
            in1: 0,
            in2: 1,
            pwm: 2,
            levels: [Level::HighZ; 3],
            pwm_rise: None,
            pwm_last_fall: None,
            duty: 0,
            host: None,
        }
    }

    #[test]
    fn a_dc_motor_takes_direction_from_its_inputs_and_speed_from_pwm() {
        let mut motor = motor();
        motor.set_pin(pin(0), Level::High, 0);
        motor.set_pin(pin(1), Level::Low, 0);
        motor.set_pin(pin(2), Level::High, 0);
        assert_eq!(motor.speed(), 1000);
        assert_eq!(motor.state(), PartState::Knob { permille: 1000 });
        // A 25% duty cycle, measured over one period.
        motor.set_pin(pin(2), Level::Low, 250_000);
        motor.set_pin(pin(2), Level::High, 1_000_000);
        motor.set_pin(pin(2), Level::Low, 1_250_000);
        assert_eq!(motor.speed(), 250);
        assert_eq!(motor.state(), PartState::Knob { permille: 625 });
        motor.set_pin(pin(0), Level::Low, 0);
        motor.set_pin(pin(1), Level::High, 0);
        assert_eq!(motor.speed(), -250);
    }

    #[test]
    fn a_dc_motor_host_control_is_signed_speed() {
        let mut motor = motor();
        motor.set_control(1000);
        assert_eq!(motor.speed(), 1000);
        motor.set_control(0);
        assert_eq!(motor.speed(), -1000);
        motor.set_control(500);
        assert_eq!(motor.speed(), 0);
    }

    #[test]
    fn a_vibration_motor_runs_only_with_the_right_polarity() {
        let mut motor = VibrationMotor {
            positive: 0,
            negative: 1,
            levels: [Level::HighZ; 2],
        };
        motor.set_pin(pin(0), Level::High, 0);
        assert!(!motor.running(), "the return pin is floating");
        motor.set_pin(pin(1), Level::Low, 0);
        assert!(motor.running());
        assert_eq!(motor.state(), PartState::Led { lit: true });
    }

    #[test]
    fn a_traffic_light_lights_when_any_lamp_is_driven() {
        let mut light = TrafficLight {
            lamps: [0, 1, 2],
            levels: [Level::HighZ; 3],
        };
        assert_eq!(light.state(), PartState::Led { lit: false });
        light.set_pin(pin(1), Level::High, 0);
        assert_eq!(light.state(), PartState::Led { lit: true });
    }

    #[test]
    fn a_clock_generator_toggles_on_simulated_time() {
        let mut clock = ClockGenerator {
            out: 0,
            hz: 1000,
            high: true,
            next_at: Some(500_000),
        };
        assert_eq!(clock.drive(pin(0)), Drive::strong(Level::High));
        clock.advance(500_000);
        assert_eq!(clock.drive(pin(0)), Drive::strong(Level::Low));
        assert_eq!(clock.next_event(), Some(1_000_000));
        clock.advance(10_000_000);
        assert_eq!(
            clock.drive(pin(0)),
            Drive::strong(Level::High),
            "an odd number of toggles flips"
        );
        assert_eq!(
            clock.state(),
            PartState::Tone {
                sounding: true,
                hz: 1000
            }
        );
    }

    fn a4988() -> A4988 {
        A4988 {
            step: 0,
            dir: 1,
            enable: 2,
            reset: 10,
            sleep: 11,
            microstep_pins: [3, 4, 5],
            output_pins: [6, 7, 8, 9],
            dir_level: Level::HighZ,
            enable_level: Level::HighZ,
            reset_level: Level::HighZ,
            sleep_level: Level::HighZ,
            ms_levels: [Level::HighZ; 3],
            step_level: Level::HighZ,
            position: 0,
        }
    }

    #[test]
    fn an_a4988_counts_step_pulses_in_the_direction_pin_chooses() {
        let mut driver = a4988();
        driver.set_pin(pin(2), Level::Low, 0); // enabled
        driver.set_pin(pin(1), Level::High, 0); // clockwise
        for _ in 0..4 {
            driver.set_pin(pin(0), Level::High, 0);
            driver.set_pin(pin(0), Level::Low, 0);
        }
        assert_eq!(driver.position(), 4);
        assert_eq!(
            driver.drive(pin(6)),
            Drive::strong(Level::High),
            "phase 0 again after four steps"
        );
        driver.set_pin(pin(1), Level::Low, 0);
        driver.set_pin(pin(0), Level::High, 0);
        assert_eq!(driver.position(), 3);
    }

    #[test]
    fn an_a4988_does_not_step_while_disabled() {
        let mut driver = a4988();
        driver.set_pin(pin(2), Level::High, 0);
        driver.set_pin(pin(0), Level::High, 0);
        assert_eq!(driver.position(), 0);
    }

    #[test]
    fn a_dpdt_relay_moves_both_poles_with_one_coil() {
        let mut relay = DpdtRelay {
            coil: [0, 1],
            coil_levels: [Level::HighZ; 2],
            poles: [[2, 3, 4], [5, 6, 7]],
            levels: [[Level::HighZ; 3]; 2],
            energized: false,
        };
        relay.set_pin(pin(2), Level::High, 0); // NC1
        relay.set_pin(pin(4), Level::Low, 0); // NO1
        assert_eq!(
            relay.drive(pin(3)),
            Drive::strong(Level::High),
            "COM rests on NC"
        );
        relay.set_pin(pin(0), Level::High, 0);
        relay.set_pin(pin(1), Level::Low, 0);
        assert!(relay.energized());
        assert_eq!(
            relay.drive(pin(3)),
            Drive::strong(Level::Low),
            "COM moves to NO"
        );
    }

    #[test]
    fn a_uln2003_pulls_an_output_low_for_a_high_input() {
        let mut array = Uln2003 {
            inputs: [0, 1, 2, 3, 4, 5, 6],
            outputs: [7, 8, 9, 10, 11, 12, 13],
            levels: [Level::HighZ; 7],
        };
        array.set_pin(pin(0), Level::High, 0);
        assert_eq!(array.drive(pin(7)), Drive::strong(Level::Low));
        assert_eq!(array.drive(pin(8)), Drive::HIGH_Z);
        array.set_pin(pin(0), Level::Low, 0);
        assert_eq!(array.drive(pin(7)), Drive::HIGH_Z);
    }

    #[test]
    fn a_74hc4067_connects_only_the_selected_channel() {
        let mut mux = Cd4067 {
            selects: [0, 1, 2, 3],
            inhibit: 4,
            common: 5,
            channels: std::array::from_fn(|channel| 6 + channel as u16),
            select_levels: [Level::Low; 4],
            inhibit_level: Level::Low,
            common_level: Level::HighZ,
            channel_levels: [Level::HighZ; 16],
        };
        mux.set_pin(pin(6 + 9), Level::High, 0);
        assert_eq!(mux.drive(pin(5)), Drive::HIGH_Z, "channel 0 selected");
        // Select address 9: 1001.
        mux.set_pin(pin(0), Level::High, 0);
        mux.set_pin(pin(3), Level::High, 0);
        assert_eq!(mux.selected(), Some(9));
        assert_eq!(mux.drive(pin(5)), Drive::weak(Level::High));
        mux.set_pin(pin(4), Level::High, 0);
        assert_eq!(mux.selected(), None, "inhibit releases the common pin");
    }

    #[test]
    fn a_blown_fuse_opens_the_circuit() {
        let mut fuse = Fuse {
            a: 0,
            b: 1,
            a_level: Level::HighZ,
            b_level: Level::HighZ,
            blown: false,
        };
        fuse.set_pin(pin(1), Level::High, 0);
        assert_eq!(fuse.drive(pin(0)), Drive::weak(Level::High));
        assert!(fuse.set_control(900));
        assert!(fuse.blown());
        assert_eq!(fuse.drive(pin(0)), Drive::HIGH_Z);
        assert_eq!(fuse.state(), PartState::Switch { on: false });
    }

    #[test]
    fn a_four_way_dip_bank_closes_only_the_selected_positions() {
        let mut bank = DipBank {
            contacts: vec![(0, 1), (2, 3), (4, 5), (6, 7)],
            levels: vec![(Level::HighZ, Level::HighZ); 4],
            closed: 0,
        };
        bank.set_pin(pin(3), Level::Low, 0);
        assert_eq!(bank.drive(pin(2)), Drive::HIGH_Z);
        bank.set_switch(1, true);
        assert_eq!(bank.drive(pin(2)), Drive::strong(Level::Low));
        assert_eq!(bank.mask(), 0b10);
        bank.set_switch(1, false);
        assert!(!bank.is_closed(1));
    }

    #[test]
    fn a_level_shifter_mirrors_a_level_across_its_channel() {
        let mut shifter = LevelShifter {
            channels: vec![(0, 1), (2, 3)],
            levels: vec![(Level::HighZ, Level::HighZ); 2],
        };
        shifter.set_pin(pin(0), Level::High, 0);
        assert_eq!(shifter.drive(pin(1)), Drive::weak(Level::High));
        shifter.set_pin(pin(3), Level::Low, 0);
        assert_eq!(shifter.drive(pin(2)), Drive::weak(Level::Low));
        assert_eq!(shifter.drive(pin(9)), Drive::HIGH_Z);
    }
}
