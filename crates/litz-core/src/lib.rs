//! Foundation types shared by every Litz crate.
//!
//! Litz is a firmware simulator split along three seams:
//!
//! * [`Bus`] — what the CPU sees: an address space, peripherals hang off it.
//! * [`Mcu`] — a chip: CPU core plus on-chip peripherals, stepped by time.
//! * [`Part`] — anything on the schematic: LED, button, sensor, display. It only
//!   ever sees pin levels, never the MCU's internals.
//!
//! Keeping those three apart is what lets `litz-esp` and `litz-avr` share the
//! same parts bin, and what lets the same core compile for a host CLI and for
//! `wasm32-unknown-unknown`.
//!
//! Time is a `u64` count of nanoseconds ([`Tick`]). Every peripheral declares
//! its own clock period in ticks; a domain whose period does not divide one
//! nanosecond (a 160 MHz core is 6.25 ns per cycle) carries a fractional
//! remainder, which is exactly what a hardware clock divider does. No floating
//! point anywhere in the schedule, so two runs of one firmware produce the
//! same trace on every host.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

/// Nanoseconds since simulation start.
pub type Tick = u64;

/// Ticks in one second. Time is fixed-point by construction.
pub const TICKS_PER_SEC: Tick = 1_000_000_000;

/// A digital pin level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Low,
    High,
    /// High-impedance: the pin is not driving, a net must resolve it from
    /// other drivers or a pull resistor.
    HighZ,
}

/// Identifier of a device instance in a [`Board`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PartId(pub u16);

/// Identifier of one pin of a [`Part`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PinId(pub u16);

/// The address space a CPU core sees.
///
/// Peripherals are not methods on the bus: a bus owns an ordered table of
/// address ranges and forwards to whichever peripheral claims the range. That
/// keeps the chip definition declarative and lets a peripheral be moved from
/// one chip to another without touching the CPU.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16 {
        u16::from(self.read8(addr)) | (u16::from(self.read8(addr + 1)) << 8)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        u32::from(self.read16(addr)) | (u32::from(self.read16(addr + 2)) << 16)
    }
    fn write8(&mut self, addr: u32, value: u8);
    fn write16(&mut self, addr: u32, value: u16) {
        self.write8(addr, value as u8);
        self.write8(addr + 1, (value >> 8) as u8);
    }
    fn write32(&mut self, addr: u32, value: u32) {
        self.write16(addr, value as u16);
        self.write16(addr + 2, (value >> 16) as u16);
    }
}

/// A microcontroller: a CPU plus the peripherals on its bus.
///
/// [`Mcu::step`] advances exactly one instruction (or one hardware event,
/// whichever comes first in time) and returns the tick at which it next needs
/// attention. The scheduler runs the MCU whose next tick is smallest.
pub trait Mcu {
    fn reset(&mut self, now: Tick);

    /// Execute one step and return the earliest tick at which this MCU must be
    /// stepped again.
    fn step(&mut self, now: Tick) -> Tick;

    /// Current level driven by an external pin, if the chip drives it.
    fn pin_level(&self, pin: u32) -> Level;

    /// A [`Part`] changed the level it drives onto a chip pin.
    fn set_pin_level(&mut self, pin: u32, level: Level, now: Tick);
}

/// How hard a node is driven.
///
/// Two levels is the whole electrical model. A push-pull chip pin is `Strong`;
/// a supply rail is `Strong`; a resistor passing a level through is `Weak`.
/// Resolution is "strong beats weak, two strongs that disagree are a short",
/// which covers series resistors, pull-ups and buttons without an analog
/// solver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strength {
    Strong,
    Weak,
}

/// What a pin is doing to its net right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drive {
    pub level: Level,
    pub strength: Strength,
}

impl Drive {
    /// Not driving. `strength` is meaningless when floating; `Weak` is used so
    /// comparisons stay cheap.
    pub const HIGH_Z: Drive = Drive {
        level: Level::HighZ,
        strength: Strength::Weak,
    };

    pub const fn strong(level: Level) -> Self {
        Self {
            level,
            strength: Strength::Strong,
        }
    }

    pub const fn weak(level: Level) -> Self {
        Self {
            level,
            strength: Strength::Weak,
        }
    }

    pub fn is_driving(self) -> bool {
        self.level != Level::HighZ
    }
}

/// A schematic component. Parts never read MCU registers; they react to pin
/// levels and to time, which is the same contract a real part has.
///
/// Pin ids are indices into the part's own pin table, assigned once when the
/// board is flattened, so a driver never has to match strings at run time.
pub trait Part {
    /// What this pin drives onto its net. `HIGH_Z` when the part only listens.
    fn drive(&self, pin: PinId) -> Drive;

    /// Called for every level change on a net this part is attached to.
    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick);

    /// Earliest tick at which this part wants [`Part::advance`] called.
    fn next_event(&self) -> Option<Tick> {
        None
    }

    fn advance(&mut self, _now: Tick) {}

    /// What a UI or a test can read back without knowing the part's type.
    fn state(&self) -> PartState {
        PartState::None
    }

    /// Host interaction: a person pressed this part. Returns whether the part
    /// has such an input at all, so a UI can tell a button from an LED.
    fn press(&mut self, _pressed: bool) -> bool {
        false
    }

    /// Host interaction: a person moved a continuous control, in permille
    /// (0 through 1000). Returns whether the part has such a control.
    fn set_control(&mut self, _value: u16) -> bool {
        false
    }

    /// Host interaction by name, the way a test or a scenario says it: a
    /// button's `pressed`, a sensor's `temperature`, a joystick's `x`. The
    /// value stays text because one name can be a percentage, a boolean or a
    /// physical unit; the part decides how to read it. Returns whether the
    /// part has that control.
    fn set_named_control(&mut self, _name: &str, _value: &str) -> bool {
        false
    }

    /// The analog voltage in volts this pin holds, for parts that source one
    /// (a potentiometer wiper, a sensor output). Digital parts return `None`
    /// on every pin; a net with an analog source is still resolved as a
    /// digital level for everything else on it.
    fn analog(&self, _pin: PinId) -> Option<f32> {
        None
    }

    /// The output current at which a regulated source stops holding its set
    /// voltage and delivers this much instead, in amperes. `None` for a part
    /// that does not limit: a battery, a rail, a plain source.
    fn current_limit(&self) -> Option<f32> {
        None
    }
}

/// The externally observable state of a part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartState {
    Led {
        lit: bool,
    },
    Button {
        pressed: bool,
    },
    /// A two-position switch and which way it is thrown.
    Switch {
        on: bool,
    },
    /// A seven-segment display: one bit per segment, `a`..`g` then `dp`, least
    /// significant bit first.
    Segments {
        mask: u8,
    },
    /// A buzzer or speaker: whether it is being driven, and the rate measured
    /// from the edges its pins actually saw.
    Tone {
        sounding: bool,
        hz: u32,
    },
    /// A servo: the angle the incoming pulse train asks for.
    Servo {
        degrees: u16,
    },
    /// A continuous control such as a potentiometer, in permille of its travel.
    Knob {
        permille: u16,
    },
    /// A packet radio: frames it has sent and received, and the first
    /// [`RADIO_LAST_BYTES`] bytes of the most recent payload (transmitted or
    /// received). `last_len` says how much of `last_bytes` is meaningful.
    Radio {
        tx_count: u32,
        rx_count: u32,
        last_bytes: [u8; RADIO_LAST_BYTES],
        last_len: u8,
    },
    None,
}

/// How much of a radio's last payload [`PartState::Radio`] carries. Longer
/// frames are truncated in the state only; the part still moves every byte.
pub const RADIO_LAST_BYTES: usize = 32;
