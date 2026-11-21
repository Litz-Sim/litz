//! Logic and interface chips: gates, shift registers, latches, a counter and a
//! multiplexer.
//!
//! These are all ordinary pin-level parts. One engine, `gate`, serves the
//! 74HC quad packages and the discrete single gates: the part's catalog id
//! selects the function and the package, which is why a diagram can use a
//! `litz-74hc00` or a `litz-nand` without a second engine name.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};

fn index(part: &FlatPart, name: &str) -> Result<u16, BuildError> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {name:?} pin", part.id)))
}

fn strong(level: Level) -> Drive {
    match level {
        Level::HighZ => Drive::HIGH_Z,
        level => Drive::strong(level),
    }
}

fn high(level: Level) -> Option<bool> {
    match level {
        Level::High => Some(true),
        Level::Low => Some(false),
        Level::HighZ => None,
    }
}

// ------------------------------------------------------------------ gates

/// The logic function of one gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateFn {
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
    Not,
    Buffer,
}

impl GateFn {
    fn inputs(self) -> usize {
        match self {
            Self::Not | Self::Buffer => 1,
            _ => 2,
        }
    }

    fn eval(self, a: bool, b: bool) -> bool {
        match self {
            Self::And => a && b,
            Self::Nand => !(a && b),
            Self::Or => a || b,
            Self::Nor => !(a || b),
            Self::Xor => a ^ b,
            Self::Xnor => a == b,
            Self::Not => !a,
            Self::Buffer => a,
        }
    }
}

/// One package of gates: a quad 74HC00 or a single discrete gate.
///
/// Inputs are `Vec` entries of `[A, B]`; a one-input gate leaves the second
/// slot at `u16::MAX` so no real pin can match it. Outputs drive strong levels
/// while both of the gate's inputs are known and float otherwise.
pub struct Gate {
    function: GateFn,
    inputs: Vec<[u16; 2]>,
    outputs: Vec<u16>,
    levels: Vec<[Level; 2]>,
}

impl Gate {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let key = part.type_key.as_str();
        let (function, gates) = match key {
            "litz-74hc00" => (GateFn::Nand, 4),
            "litz-74hc02" => (GateFn::Nor, 4),
            "litz-74hc04" | "litz-74hc14" => (GateFn::Not, 6),
            "litz-74hc08" => (GateFn::And, 4),
            "litz-74hc32" => (GateFn::Or, 4),
            "litz-74hc86" => (GateFn::Xor, 4),
            "litz-and" => (GateFn::And, 1),
            "litz-nand" => (GateFn::Nand, 1),
            "litz-or" => (GateFn::Or, 1),
            "litz-nor" => (GateFn::Nor, 1),
            "litz-xor" => (GateFn::Xor, 1),
            "litz-xnor" => (GateFn::Xnor, 1),
            "litz-not" => (GateFn::Not, 1),
            "litz-buffer" => (GateFn::Buffer, 1),
            other => {
                return Err(BuildError::Message(format!(
                    "part {:?} uses the gate engine with unknown type {other:?}",
                    part.id
                )));
            }
        };
        let discrete = !key.starts_with("litz-74hc");
        let arity = function.inputs();
        let mut inputs = Vec::with_capacity(gates);
        let mut outputs = Vec::with_capacity(gates);
        for gate in 0..gates {
            let prefix = if gates == 1 {
                String::new()
            } else {
                (gate + 1).to_string()
            };
            let a_name = if discrete {
                "A".to_string()
            } else {
                format!("{prefix}A")
            };
            let b_name = if discrete {
                "B".to_string()
            } else {
                format!("{prefix}B")
            };
            let y_name = if discrete {
                "Y".to_string()
            } else {
                format!("{prefix}Y")
            };
            let a = index(part, &a_name)?;
            let b = if arity == 2 {
                index(part, &b_name)?
            } else {
                u16::MAX
            };
            inputs.push([a, b]);
            outputs.push(index(part, &y_name)?);
        }
        Ok(Box::new(Self {
            function,
            inputs,
            outputs,
            levels: vec![[Level::HighZ; 2]; gates],
        }))
    }

    pub fn function(&self) -> GateFn {
        self.function
    }
}

impl Part for Gate {
    fn drive(&self, pin: PinId) -> Drive {
        let Some(gate) = self.outputs.iter().position(|&output| output == pin.0) else {
            return Drive::HIGH_Z;
        };
        let [a, b] = self.levels[gate];
        let Some(a) = high(a) else {
            return Drive::HIGH_Z;
        };
        let b = if self.function.inputs() == 2 {
            let Some(b) = high(b) else {
                return Drive::HIGH_Z;
            };
            b
        } else {
            false
        };
        strong(if self.function.eval(a, b) {
            Level::High
        } else {
            Level::Low
        })
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        for (gate, inputs) in self.inputs.iter().enumerate() {
            if inputs[0] == pin.0 {
                self.levels[gate][0] = level;
            } else if inputs[1] == pin.0 {
                self.levels[gate][1] = level;
            }
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ 74HC164

/// An 8-bit serial-in, parallel-out shift register without a latch.
///
/// The two serial inputs are ANDed; every rising clock edge shifts them into
/// `Q0` and moves the older bits toward `Q7`. A low clear empties the register
/// immediately and holds it empty.
pub struct Hc164 {
    a: u16,
    b: u16,
    clk: u16,
    clr: u16,
    outputs: [u16; 8],
    a_level: Level,
    b_level: Level,
    clk_level: Level,
    clr_level: Level,
    byte: u8,
}

impl Hc164 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut outputs = [0u16; 8];
        for (number, output) in outputs.iter_mut().enumerate() {
            *output = index(part, &format!("Q{number}"))?;
        }
        Ok(Box::new(Self {
            a: index(part, "A")?,
            b: index(part, "B")?,
            clk: index(part, "CLK")?,
            clr: index(part, "CLR")?,
            outputs,
            a_level: Level::HighZ,
            b_level: Level::HighZ,
            clk_level: Level::HighZ,
            clr_level: Level::HighZ,
            byte: 0,
        }))
    }

    pub fn byte(&self) -> u8 {
        self.byte
    }
}

impl Part for Hc164 {
    fn drive(&self, pin: PinId) -> Drive {
        match self.outputs.iter().position(|&output| output == pin.0) {
            Some(bit) => strong(if self.byte & (1 << bit) != 0 {
                Level::High
            } else {
                Level::Low
            }),
            None => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.a {
            self.a_level = level;
        } else if pin.0 == self.b {
            self.b_level = level;
        } else if pin.0 == self.clr {
            self.clr_level = level;
            if level == Level::Low {
                self.byte = 0;
            }
        } else if pin.0 == self.clk {
            let rising = level == Level::High && self.clk_level != Level::High;
            self.clk_level = level;
            if rising && self.clr_level != Level::Low {
                let data = u8::from(self.a_level == Level::High && self.b_level == Level::High);
                self.byte = (self.byte << 1) | data;
            }
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ 74HC573

/// An octal transparent D latch.
///
/// While `LE` is high the outputs follow the inputs; on its falling edge the
/// byte latches. `OE` is active low, so a high output enable releases all eight
/// outputs to high impedance.
pub struct Hc573 {
    data: [u16; 8],
    outputs: [u16; 8],
    le: u16,
    oe: u16,
    data_levels: [Level; 8],
    le_level: Level,
    oe_level: Level,
    latch: u8,
}

impl Hc573 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut data = [0u16; 8];
        let mut outputs = [0u16; 8];
        for (number, (input, output)) in data.iter_mut().zip(outputs.iter_mut()).enumerate() {
            *input = index(part, &format!("D{number}"))?;
            *output = index(part, &format!("Q{number}"))?;
        }
        Ok(Box::new(Self {
            data,
            outputs,
            le: index(part, "LE")?,
            oe: index(part, "OE")?,
            data_levels: [Level::HighZ; 8],
            le_level: Level::HighZ,
            oe_level: Level::HighZ,
            latch: 0,
        }))
    }

    pub fn latch(&self) -> u8 {
        self.latch
    }

    fn sample(&self) -> u8 {
        self.data_levels
            .iter()
            .enumerate()
            .fold(0u8, |byte, (bit, level)| {
                byte | (u8::from(*level == Level::High) << bit)
            })
    }
}

impl Part for Hc573 {
    fn drive(&self, pin: PinId) -> Drive {
        let Some(bit) = self.outputs.iter().position(|&output| output == pin.0) else {
            return Drive::HIGH_Z;
        };
        if self.oe_level == Level::High {
            return Drive::HIGH_Z;
        }
        let level = if self.le_level == Level::High {
            self.data_levels[bit]
        } else if self.latch & (1 << bit) != 0 {
            Level::High
        } else {
            Level::Low
        };
        strong(level)
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(bit) = self.data.iter().position(|&input| input == pin.0) {
            self.data_levels[bit] = level;
        } else if pin.0 == self.le {
            let falling = level != Level::High && self.le_level == Level::High;
            self.le_level = level;
            if falling {
                self.latch = self.sample();
            }
        } else if pin.0 == self.oe {
            self.oe_level = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ CD4017

/// A CD4017 decade counter with ten decoded outputs and a carry-out.
///
/// A rising clock edge advances the one-hot output while reset is low and the
/// clock inhibit is low. Reset forces `Q0`; carry-out is high on counts 0–4,
/// which is what lets two packages form a divide-by-100 chain.
pub struct Cd4017 {
    clk: u16,
    rst: u16,
    inhibit: u16,
    outputs: [u16; 10],
    carry: u16,
    clk_level: Level,
    rst_level: Level,
    inhibit_level: Level,
    count: u8,
}

impl Cd4017 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut outputs = [0u16; 10];
        for (number, output) in outputs.iter_mut().enumerate() {
            *output = index(part, &format!("Q{number}"))?;
        }
        Ok(Box::new(Self {
            clk: index(part, "CLK")?,
            rst: index(part, "RST")?,
            inhibit: index(part, "CLKINH")?,
            outputs,
            carry: index(part, "CO")?,
            clk_level: Level::HighZ,
            rst_level: Level::HighZ,
            inhibit_level: Level::HighZ,
            count: 0,
        }))
    }

    pub fn count(&self) -> u8 {
        self.count
    }
}

impl Part for Cd4017 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.carry {
            return strong(if self.count <= 4 {
                Level::High
            } else {
                Level::Low
            });
        }
        match self.outputs.iter().position(|&output| output == pin.0) {
            Some(number) => strong(if number as u8 == self.count {
                Level::High
            } else {
                Level::Low
            }),
            None => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.rst {
            self.rst_level = level;
            if level == Level::High {
                self.count = 0;
            }
        } else if pin.0 == self.inhibit {
            self.inhibit_level = level;
        } else if pin.0 == self.clk {
            let rising = level == Level::High && self.clk_level != Level::High;
            self.clk_level = level;
            if rising && self.rst_level != Level::High && self.inhibit_level != Level::High {
                self.count = (self.count + 1) % 10;
            }
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

// ------------------------------------------------------------------ CD4051

/// An 8-channel analog multiplexer/demultiplexer.
///
/// The selected channel and the common pin are one conductor through a switch,
/// modelled as a weak bidirectional path so a strong source on either side wins.
/// `INH` high opens every switch.
pub struct Cd4051 {
    selects: [u16; 3],
    inhibit: u16,
    common: u16,
    channels: [u16; 8],
    select_levels: [Level; 3],
    inhibit_level: Level,
    common_level: Level,
    channel_levels: [Level; 8],
}

impl Cd4051 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut channels = [0u16; 8];
        for (number, channel) in channels.iter_mut().enumerate() {
            *channel = index(part, &format!("X{number}"))?;
        }
        Ok(Box::new(Self {
            selects: [index(part, "A")?, index(part, "B")?, index(part, "C")?],
            inhibit: index(part, "INH")?,
            common: index(part, "X")?,
            channels,
            select_levels: [Level::HighZ; 3],
            inhibit_level: Level::HighZ,
            common_level: Level::HighZ,
            channel_levels: [Level::HighZ; 8],
        }))
    }

    /// The channel the select pins name, or `None` while inhibited.
    pub fn selected(&self) -> Option<u8> {
        if self.inhibit_level == Level::High {
            return None;
        }
        let address = usize::from(self.select_levels[0] == Level::High)
            | usize::from(self.select_levels[1] == Level::High) << 1
            | usize::from(self.select_levels[2] == Level::High) << 2;
        Some(address as u8)
    }
}

impl Part for Cd4051 {
    fn drive(&self, pin: PinId) -> Drive {
        let Some(channel) = self.selected().map(usize::from) else {
            return Drive::HIGH_Z;
        };
        if pin.0 == self.common {
            match self.channel_levels[channel] {
                Level::HighZ => Drive::HIGH_Z,
                level => Drive::weak(level),
            }
        } else if pin.0 == self.channels[channel] {
            match self.common_level {
                Level::HighZ => Drive::HIGH_Z,
                level => Drive::weak(level),
            }
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

// ------------------------------------------------------------------ CD4094

/// An 8-bit serial-in, parallel-out shift register with an output latch.
///
/// Bits shift on each rising clock edge. A rising strobe copies the shift
/// register to the output latch; `OE` high presents the latch, low releases the
/// outputs.
pub struct Cd4094 {
    data: u16,
    clk: u16,
    strobe: u16,
    oe: u16,
    outputs: [u16; 8],
    qs: u16,
    data_level: Level,
    clk_level: Level,
    strobe_level: Level,
    oe_level: Level,
    shift: u8,
    latch: u8,
}

impl Cd4094 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut outputs = [0u16; 8];
        for (number, output) in outputs.iter_mut().enumerate() {
            *output = index(part, &format!("Q{number}"))?;
        }
        Ok(Box::new(Self {
            data: index(part, "DATA")?,
            clk: index(part, "CLK")?,
            strobe: index(part, "STROBE")?,
            oe: index(part, "OE")?,
            outputs,
            qs: index(part, "QS")?,
            data_level: Level::HighZ,
            clk_level: Level::HighZ,
            strobe_level: Level::HighZ,
            oe_level: Level::HighZ,
            shift: 0,
            latch: 0,
        }))
    }

    pub fn latch(&self) -> u8 {
        self.latch
    }
}

impl Part for Cd4094 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.qs {
            return strong(if self.shift & 0x80 != 0 {
                Level::High
            } else {
                Level::Low
            });
        }
        let Some(bit) = self.outputs.iter().position(|&output| output == pin.0) else {
            return Drive::HIGH_Z;
        };
        if self.oe_level != Level::High {
            return Drive::HIGH_Z;
        }
        strong(if self.latch & (1 << bit) != 0 {
            Level::High
        } else {
            Level::Low
        })
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.data {
            self.data_level = level;
        } else if pin.0 == self.clk {
            let rising = level == Level::High && self.clk_level != Level::High;
            self.clk_level = level;
            if rising {
                self.shift = (self.shift << 1) | u8::from(self.data_level == Level::High);
            }
        } else if pin.0 == self.strobe {
            let rising = level == Level::High && self.strobe_level != Level::High;
            self.strobe_level = level;
            if rising {
                self.latch = self.shift;
            }
        } else if pin.0 == self.oe {
            self.oe_level = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(id: u16) -> PinId {
        PinId(id)
    }

    fn gate(function: GateFn) -> Gate {
        Gate {
            function,
            inputs: vec![[0, 1]],
            outputs: vec![2],
            levels: vec![[Level::HighZ; 2]],
        }
    }

    #[test]
    fn a_nand_gate_inverts_its_conjunction() {
        let mut g = gate(GateFn::Nand);
        g.set_pin(pin(0), Level::High, 0);
        g.set_pin(pin(1), Level::High, 0);
        assert_eq!(g.drive(pin(2)), Drive::strong(Level::Low));
        g.set_pin(pin(1), Level::Low, 0);
        assert_eq!(g.drive(pin(2)), Drive::strong(Level::High));
        g.set_pin(pin(0), Level::HighZ, 0);
        assert_eq!(
            g.drive(pin(2)),
            Drive::HIGH_Z,
            "an unknown input floats the output"
        );
    }

    #[test]
    fn a_not_gate_uses_one_input() {
        let mut g = Gate {
            inputs: vec![[0, u16::MAX]],
            ..gate(GateFn::Not)
        };
        g.set_pin(pin(0), Level::High, 0);
        assert_eq!(g.drive(pin(2)), Drive::strong(Level::Low));
    }

    #[test]
    fn a_74hc164_shifts_on_the_clock() {
        let mut shift = Hc164 {
            a: 0,
            b: 1,
            clk: 2,
            clr: 3,
            outputs: [4, 5, 6, 7, 8, 9, 10, 11],
            a_level: Level::High,
            b_level: Level::High,
            clk_level: Level::Low,
            clr_level: Level::High,
            byte: 0,
        };
        shift.set_pin(pin(2), Level::High, 0);
        shift.set_pin(pin(2), Level::Low, 0);
        assert_eq!(shift.byte(), 0x01);
        assert_eq!(shift.drive(pin(4)), Drive::strong(Level::High));
        shift.set_pin(pin(2), Level::High, 0);
        assert_eq!(shift.byte(), 0x03);
        shift.set_pin(pin(3), Level::Low, 0);
        assert_eq!(shift.byte(), 0);
    }

    #[test]
    fn a_74hc573_is_transparent_then_latches() {
        let mut latch = Hc573 {
            data: [0, 1, 2, 3, 4, 5, 6, 7],
            outputs: [8, 9, 10, 11, 12, 13, 14, 15],
            le: 16,
            oe: 17,
            data_levels: [Level::HighZ; 8],
            le_level: Level::High,
            oe_level: Level::Low,
            latch: 0,
        };
        latch.set_pin(pin(0), Level::High, 0);
        assert_eq!(
            latch.drive(pin(8)),
            Drive::strong(Level::High),
            "transparent"
        );
        latch.set_pin(pin(16), Level::Low, 0);
        assert_eq!(latch.latch(), 0x01);
        latch.set_pin(pin(0), Level::Low, 0);
        assert_eq!(latch.drive(pin(8)), Drive::strong(Level::High), "held");
        latch.set_pin(pin(17), Level::High, 0);
        assert_eq!(latch.drive(pin(8)), Drive::HIGH_Z, "output enable releases");
    }

    #[test]
    fn a_cd4017_walks_one_hot_outputs() {
        let mut counter = Cd4017 {
            clk: 0,
            rst: 1,
            inhibit: 2,
            outputs: [3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            carry: 13,
            clk_level: Level::Low,
            rst_level: Level::Low,
            inhibit_level: Level::Low,
            count: 0,
        };
        counter.set_pin(pin(0), Level::High, 0);
        assert_eq!(counter.count(), 1);
        assert_eq!(counter.drive(pin(4)), Drive::strong(Level::High));
        assert_eq!(counter.drive(pin(3)), Drive::strong(Level::Low));
        counter.set_pin(pin(0), Level::Low, 0);
        assert_eq!(counter.count(), 1, "only the rising edge advances");
        counter.set_pin(pin(1), Level::High, 0);
        assert_eq!(counter.count(), 0, "reset forces Q0");
    }

    #[test]
    fn a_cd4094_latches_shifted_bits() {
        let mut shift = Cd4094 {
            data: 0,
            clk: 1,
            strobe: 2,
            oe: 3,
            outputs: [4, 5, 6, 7, 8, 9, 10, 11],
            qs: 12,
            data_level: Level::Low,
            clk_level: Level::Low,
            strobe_level: Level::Low,
            oe_level: Level::High,
            shift: 0,
            latch: 0,
        };
        shift.set_pin(pin(0), Level::High, 0);
        shift.set_pin(pin(1), Level::High, 0);
        shift.set_pin(pin(1), Level::Low, 0);
        assert_eq!(shift.shift, 0x01);
        assert_eq!(
            shift.drive(pin(4)),
            Drive::strong(Level::Low),
            "not latched yet"
        );
        shift.set_pin(pin(2), Level::High, 0);
        assert_eq!(shift.latch(), 0x01);
        assert_eq!(shift.drive(pin(4)), Drive::strong(Level::High));
    }

    #[test]
    fn a_cd4051_passes_the_selected_channel_weakly() {
        let mut mux = Cd4051 {
            selects: [0, 1, 2],
            inhibit: 3,
            common: 4,
            channels: [5, 6, 7, 8, 9, 10, 11, 12],
            select_levels: [Level::HighZ; 3],
            inhibit_level: Level::HighZ,
            common_level: Level::HighZ,
            channel_levels: [Level::HighZ; 8],
        };
        mux.set_pin(pin(1), Level::High, 0); // select channel 2
        mux.set_pin(pin(7), Level::High, 0); // channel 2 high
        assert_eq!(
            mux.drive(pin(4)),
            Drive::weak(Level::High),
            "common follows channel 2"
        );
        assert_eq!(mux.drive(pin(5)), Drive::HIGH_Z, "channel 0 is open");
    }
}
