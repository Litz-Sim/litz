//! The single-cycle IO block: GPIO output, output enable and input for both
//! cores, the inter-core FIFOs, the 32 spinlocks, the hardware divider and the
//! two interpolators.
//!
//! Every pin on the chip ends up here: the IO mux only decides *which*
//! peripheral a pin carries, and a pin whose function select is `SIO` is
//! driven straight from `GPIO_OUT`/`GPIO_OE` with no peripheral in between.
//! That is why this block sits at `0xD000_0000` rather than in the APB window:
//! the bus grants it a single-cycle access.
//!
//! The FIFOs are one queue per core: core 0 writes `FIFO_WR` and core 1 reads
//! it from `FIFO_RD`, and vice versa, so `CPUID` is what tells a firmware
//! which end it is holding. `FIFO_WR` also records the word for the machine,
//! which is how the launch message core 0 sends core 1 is noticed.

use super::RegisterFile;

pub mod reg {
    pub const CPUID: u32 = 0x000;
    pub const GPIO_IN: u32 = 0x004;
    pub const GPIO_HI_IN: u32 = 0x008;
    pub const GPIO_OUT: u32 = 0x010;
    pub const GPIO_OUT_SET: u32 = 0x014;
    pub const GPIO_OUT_CLR: u32 = 0x018;
    pub const GPIO_OUT_XOR: u32 = 0x01c;
    pub const GPIO_OE: u32 = 0x020;
    pub const GPIO_OE_SET: u32 = 0x024;
    pub const GPIO_OE_CLR: u32 = 0x028;
    pub const GPIO_OE_XOR: u32 = 0x02c;
    pub const GPIO_HI_OUT: u32 = 0x030;
    pub const FIFO_ST: u32 = 0x050;
    pub const FIFO_WR: u32 = 0x054;
    pub const FIFO_RD: u32 = 0x058;
    pub const SPINLOCK_ST: u32 = 0x05c;
    pub const DIV_UDIVIDEND: u32 = 0x060;
    pub const DIV_UDIVISOR: u32 = 0x064;
    pub const DIV_SDIVIDEND: u32 = 0x068;
    pub const DIV_SDIVISOR: u32 = 0x06c;
    pub const DIV_QUOTIENT: u32 = 0x070;
    pub const DIV_REMAINDER: u32 = 0x074;
    pub const DIV_CSR: u32 = 0x078;
    pub const INTERP0: u32 = 0x080;
    pub const INTERP1: u32 = 0x0c0;
    pub const INTERP_STRIDE: u32 = 0x40;
    /// The 32 hardware spinlocks start past the interp block, one word each.
    pub const SPINLOCK0: u32 = 0x100;
    pub const SPINLOCK_LAST: u32 = 0x17c;
}

/// `INTERPx_CTRL_LANEy` fields.
pub mod interp_ctrl {
    pub const SHIFT_MASK: u32 = 0x1f;
    pub const MASK_LSB_SHIFT: u32 = 5;
    pub const MASK_LSB_MASK: u32 = 0x1f << 5;
    pub const MASK_MSB_SHIFT: u32 = 10;
    pub const MASK_MSB_MASK: u32 = 0x1f << 10;
    pub const SIGNED: u32 = 1 << 15;
    /// Feed the opposite lane's accumulator into this lane's shift and mask.
    pub const CROSS_INPUT: u32 = 1 << 16;
    /// Feed the opposite lane's result into this lane's accumulator on `POP`.
    pub const CROSS_RESULT: u32 = 1 << 17;
    /// Skip the shift and mask; the base is added to the raw accumulator.
    pub const ADD_RAW: u32 = 1 << 18;
    pub const FORCE_MSB_SHIFT: u32 = 19;
    pub const FORCE_MSB_MASK: u32 = 3 << 19;
    /// Blend mode, the linear interpolation across `BASE0` and `BASE1`.
    pub const BLEND: u32 = 1 << 21;
    /// Clamp mode, in which the lane value is clamped to `BASE0..BASE1`.
    pub const CLAMP: u32 = 1 << 22;
}

/// One interpolator: two accumulators, three bases and two lane controls.
///
/// A lane shifts its accumulator right by `SHIFT`, keeps the bits
/// `MASK_MSB..MASK_LSB` and, under `SIGNED`, extends the sign out of `MASK_MSB`.
/// The value it presents on the bus is that plus the lane's own base, so the
/// usual firmware idiom — an accumulator stepped by `BASE0` on every `POP` —
/// reads `accum + base` and then advances the accumulator by the same base.
///
/// The flags above that: `CROSS_INPUT` shifts the other lane's accumulator
/// instead of this one's, `CROSS_RESULT` writes the other lane's result into
/// this lane's accumulator on `POP`, `ADD_RAW` skips the shift and mask, and
/// `FORCE_MSB` overwrites bits 29:28 of what the bus reads.
///
/// Two lanes together cover the modes the datasheet describes: `BLEND` on lane
/// 0 turns lane 0's presented result into the 8-bit interpolation fraction and
/// lane 1's into `BASE0 + alpha * (BASE1 - BASE0)`, and `CLAMP` on lane 0
/// limits the lane value to `BASE0..BASE1`. `POP_FULL`/`PEEK_FULL` add both
/// lanes' shifted values to `BASE2`, which is how a texture lookup builds an
/// address from two coordinates.
#[derive(Clone, Debug)]
pub struct Interp {
    accum: [u32; 2],
    base: [u32; 3],
    ctrl: [u32; 2],
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            accum: [0; 2],
            base: [0; 3],
            ctrl: [0; 2],
        }
    }

    fn blend(&self) -> bool {
        self.ctrl[0] & interp_ctrl::BLEND != 0
    }

    fn signed(&self, lane: usize) -> bool {
        self.ctrl[lane] & interp_ctrl::SIGNED != 0
    }

    /// The lane's shifted, masked and sign-extended value, before its base is
    /// added — the "shift and mask value" the datasheet's full result uses.
    fn lane_value(&self, lane: usize) -> u32 {
        let ctrl = self.ctrl[lane];
        let input = if ctrl & interp_ctrl::CROSS_INPUT != 0 {
            self.accum[1 - lane]
        } else {
            self.accum[lane]
        };
        if ctrl & interp_ctrl::ADD_RAW != 0 {
            return input;
        }
        let value = input >> (ctrl & interp_ctrl::SHIFT_MASK);
        let lsb = (ctrl & interp_ctrl::MASK_LSB_MASK) >> interp_ctrl::MASK_LSB_SHIFT;
        let msb = (ctrl & interp_ctrl::MASK_MSB_MASK) >> interp_ctrl::MASK_MSB_SHIFT;
        if msb < lsb {
            return 0;
        }
        let mask = if msb >= 31 {
            u32::MAX
        } else {
            ((1u32 << (msb - lsb + 1)) - 1) << lsb
        };
        let value = value & mask;
        // The sign grows out of `MASK_MSB`: bits above it copy the sign bit,
        // and the bits below the mask stay zero.
        if ctrl & interp_ctrl::SIGNED != 0 && msb < 31 && value & (1 << msb) != 0 {
            value | !((1u32 << (msb + 1)) - 1)
        } else {
            value
        }
    }

    /// Whether the shift and mask dropped any set bit for this lane — the
    /// `OVERFx` status the lane's control register reports.
    fn overflow(&self, lane: usize) -> bool {
        let ctrl = self.ctrl[lane];
        if ctrl & interp_ctrl::ADD_RAW != 0 {
            return false;
        }
        let input = if ctrl & interp_ctrl::CROSS_INPUT != 0 {
            self.accum[1 - lane]
        } else {
            self.accum[lane]
        };
        let msb = (ctrl & interp_ctrl::MASK_MSB_MASK) >> interp_ctrl::MASK_MSB_SHIFT;
        let shifted = input >> (ctrl & interp_ctrl::SHIFT_MASK);
        msb < 31 && shifted >> (msb + 1) != 0
    }

    /// The control register as the bus reads it, with the live `OVERF`
    /// status bits on top of the configuration.
    fn ctrl_word(&self, lane: usize) -> u32 {
        let (low, high) = (u32::from(self.overflow(0)), u32::from(self.overflow(1)));
        self.ctrl[lane] | low << 23 | high << 24 | (low | high) << 25
    }

    /// The blend fraction: the low eight bits of lane 1's value.
    fn alpha(&self) -> u32 {
        self.lane_value(1) & 0xff
    }

    /// `BASE0 + alpha * (BASE1 - BASE0)`: the linear interpolation, with the
    /// 16-bit bases read as signed when lane 1 asks for it.
    ///
    /// The die forms a 40-bit product and shifts it right by eight, so the
    /// weights are `256 - alpha` and `alpha` and the shift rounds towards
    /// minus infinity. Interpolating the other way — `x0 + (x1 - x0) * alpha /
    /// 256` with a truncating division — is off by one whenever the product
    /// is negative, which the datasheet's unsigned example hits.
    fn blend_result(&self) -> u32 {
        let alpha = i64::from(self.alpha());
        let (x0, x1) = if self.signed(1) {
            (
                i64::from(self.base[0] as u16 as i16),
                i64::from(self.base[1] as u16 as i16),
            )
        } else {
            (i64::from(self.base[0]), i64::from(self.base[1]))
        };
        ((x0 * (256 - alpha) + x1 * alpha) >> 8) as u32
    }

    /// Clamp the lane value into `BASE0..BASE1`, signed by the lane's flag.
    fn clamp_value(&self, value: u32) -> u32 {
        if self.signed(0) {
            let value = value as i32;
            let low = self.base[0] as i32;
            let high = self.base[1] as i32;
            value.clamp(low.min(high), high.max(low)) as u32
        } else {
            let low = self.base[0].min(self.base[1]);
            let high = self.base[0].max(self.base[1]);
            value.clamp(low, high)
        }
    }

    fn force_msb(&self, lane: usize, value: u32) -> u32 {
        let bits = (self.ctrl[lane] & interp_ctrl::FORCE_MSB_MASK) >> interp_ctrl::FORCE_MSB_SHIFT;
        // The field is ORed into bits 29:28 of what the bus reads; the
        // datapath keeps the whole 32-bit value.
        value | (bits << 28)
    }

    /// What the bus reads from `PEEK_LANEx`/`POP_LANEx`.
    fn lane_result(&self, lane: usize) -> u32 {
        let value = if lane == 0 && self.ctrl[0] & interp_ctrl::CLAMP != 0 {
            self.clamp_value(self.lane_value(0))
        } else if self.blend() && lane == 0 {
            // In blend mode lane 0 presents the fraction itself.
            self.alpha()
        } else if self.blend() && lane == 1 {
            self.blend_result()
        } else {
            self.lane_value(lane).wrapping_add(self.base[lane])
        };
        self.force_msb(lane, value)
    }

    /// What `POP_FULL`/`PEEK_FULL` read: both lanes' shift-and-mask values on
    /// top of `BASE2`, without either lane's base, so two coordinates can be
    /// summed into one address.
    fn full_result(&self) -> u32 {
        let mut value = self.base[2].wrapping_add(self.lane_value(0));
        if !self.blend() {
            value = value.wrapping_add(self.lane_value(1));
        }
        value
    }

    /// Read a lane's result and write both lanes' results back into the
    /// accumulators: each accumulator takes its own lane's result, unless the
    /// lane asks for `CROSS_RESULT`, in which case it takes the other lane's.
    fn pop(&mut self, lane: usize) -> u32 {
        let results = [self.lane_result(0), self.lane_result(1)];
        self.write_back(results);
        results[lane]
    }

    fn pop_full(&mut self) -> u32 {
        let full = self.full_result();
        let results = [self.lane_result(0), self.lane_result(1)];
        self.write_back(results);
        full
    }

    fn write_back(&mut self, results: [u32; 2]) {
        for target in 0..2 {
            self.accum[target] = if self.ctrl[target] & interp_ctrl::CROSS_RESULT != 0 {
                results[1 - target]
            } else {
                results[target]
            };
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x00 => self.accum[0],
            0x04 => self.accum[1],
            0x08 => self.base[0],
            0x0c => self.base[1],
            0x10 => self.base[2],
            0x14 => self.pop(0),
            0x18 => self.pop(1),
            0x1c => self.pop_full(),
            0x20 => self.lane_result(0),
            0x24 => self.lane_result(1),
            0x28 => self.full_result(),
            0x2c => self.ctrl_word(0),
            0x30 => self.ctrl_word(1),
            // Reading these returns the lane's shift-and-mask value; writing
            // them adds to the accumulator, which is the stepping idiom.
            0x34 => self.lane_value(0),
            0x38 => self.lane_value(1),
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            0x00 => self.accum[0] = value,
            0x04 => self.accum[1] = value,
            0x08 => self.base[0] = value,
            0x0c => self.base[1] = value,
            0x10 => self.base[2] = value,
            // The overflow bits are status, not configuration.
            0x2c => self.ctrl[0] = value & !(7 << 23),
            0x30 => self.ctrl[1] = value & !(7 << 23),
            0x34 => self.accum[0] = self.accum[0].wrapping_add(value),
            0x38 => self.accum[1] = self.accum[1].wrapping_add(value),
            // `BASE_1AND0` splits one store into the two 16-bit bases; in
            // blend mode lane 1's sign flag decides how both halves extend.
            0x3c => {
                // Blend mode takes both halves as lane 1 sees them; otherwise
                // each lane's own sign flag extends its base.
                let (signed0, signed1) = if self.blend() {
                    (self.signed(1), self.signed(1))
                } else {
                    (self.signed(0), self.signed(1))
                };
                let extend = |half: u32, signed: bool| {
                    if signed {
                        ((half << 16) as i32 >> 16) as u32
                    } else {
                        half & 0xffff
                    }
                };
                self.base[0] = extend(value & 0xffff, signed0);
                self.base[1] = extend(value >> 16, signed1);
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct Sio {
    /// Driven levels, one bit per GPIO.
    pub out: u32,
    /// Output enables.
    pub oe: u32,
    /// Levels the pads present to the chip.
    pub inp: u32,
    /// What each core receives: `fifo[c]` is the queue core `c` reads.
    pub fifo: [std::collections::VecDeque<u32>; 2],
    /// Bit `n` set means spinlock `n` is taken.
    pub locks: u32,
    pub interp: [Interp; 2],
    /// The last word a core wrote to a FIFO, with the core that wrote it.
    pending_write: Option<(u8, u32)>,
    /// The hardware divider: both operands for each signedness, the last
    /// result, and the dirty flag. The divide completes on the operand write
    /// here, so `READY` is always set.
    div_udividend: u32,
    div_udivisor: u32,
    div_sdividend: u32,
    div_sdivisor: u32,
    div_quotient: u32,
    div_remainder: u32,
    div_dirty: bool,
    div_signed: bool,
    pub regs: RegisterFile,
}

impl Default for Sio {
    fn default() -> Self {
        Self::new()
    }
}

impl Sio {
    pub fn new() -> Self {
        Sio {
            out: 0,
            oe: 0,
            inp: 0,
            fifo: Default::default(),
            locks: 0,
            interp: [Interp::new(), Interp::new()],
            pending_write: None,
            div_udividend: 0,
            div_udivisor: 0,
            div_sdividend: 0,
            div_sdivisor: 0,
            div_quotient: 0,
            div_remainder: 0,
            div_dirty: false,
            div_signed: false,
            regs: RegisterFile::new(0x100 / 4),
        }
    }

    /// Complete the divide the operand write just started. The silicon takes
    /// eight cycles; here the result is ready before the write returns, which
    /// is indistinguishable to software that polls `READY` or reads after the
    /// documented delay.
    fn divide(&mut self) {
        self.div_dirty = true;
        if self.div_signed {
            let dividend = self.div_sdividend as i32;
            let divisor = self.div_sdivisor as i32;
            if divisor == 0 {
                self.div_quotient = if dividend < 0 { 1 } else { u32::MAX };
                self.div_remainder = dividend as u32;
            } else {
                self.div_quotient = dividend.wrapping_div(divisor) as u32;
                self.div_remainder = dividend.wrapping_rem(divisor) as u32;
            }
        } else if let Some(quotient) = self.div_udividend.checked_div(self.div_udivisor) {
            self.div_quotient = quotient;
            self.div_remainder = self.div_udividend % self.div_udivisor;
        } else {
            self.div_quotient = u32::MAX;
            self.div_remainder = self.div_udividend;
        }
    }

    /// The word a core wrote to a FIFO, for the machine to act on.
    pub fn take_pending_write(&mut self) -> Option<(u8, u32)> {
        self.pending_write.take()
    }

    /// Whether core `core` has a word waiting in its FIFO.
    pub fn fifo_has_data(&self, core: usize) -> bool {
        !self.fifo[core.min(1)].is_empty()
    }

    /// Push a word into the other core's FIFO.
    pub fn fifo_write(&mut self, core: usize, value: u32) {
        let target = 1 - core.min(1);
        if self.fifo[target].len() < 8 {
            self.fifo[target].push_back(value);
            self.pending_write = Some((core.min(1) as u8, value));
        }
    }

    /// Take the word waiting for core `core`, if any.
    pub fn fifo_read(&mut self, core: usize) -> Option<u32> {
        self.fifo[core.min(1)].pop_front()
    }

    pub fn read(&mut self, off: u32, core: usize) -> u32 {
        match off {
            reg::CPUID => core.min(1) as u32,
            reg::GPIO_IN => self.inp,
            reg::GPIO_HI_IN => 0,
            reg::GPIO_OUT | reg::GPIO_OUT_SET | reg::GPIO_OUT_CLR | reg::GPIO_OUT_XOR => self.out,
            reg::GPIO_OE | reg::GPIO_OE_SET | reg::GPIO_OE_CLR | reg::GPIO_OE_XOR => self.oe,
            reg::FIFO_ST => {
                let mine = core.min(1);
                let mut word = 0;
                if !self.fifo[mine].is_empty() {
                    word |= 1;
                }
                if self.fifo[1 - mine].len() < 8 {
                    word |= 1 << 1;
                }
                word
            }
            reg::FIFO_RD => self.fifo_read(core).unwrap_or(0),
            reg::SPINLOCK_ST => self.locks,
            reg::DIV_UDIVIDEND => self.div_udividend,
            reg::DIV_UDIVISOR => self.div_udivisor,
            reg::DIV_SDIVIDEND => self.div_sdividend,
            reg::DIV_SDIVISOR => self.div_sdivisor,
            reg::DIV_QUOTIENT => {
                self.div_dirty = false;
                self.div_quotient
            }
            reg::DIV_REMAINDER => self.div_remainder,
            reg::DIV_CSR => 1 | u32::from(self.div_dirty) << 1,
            off if (reg::INTERP0..reg::SPINLOCK0).contains(&off) => {
                let index = ((off - reg::INTERP0) / reg::INTERP_STRIDE) as usize;
                let inner = (off - reg::INTERP0) % reg::INTERP_STRIDE;
                self.interp
                    .get_mut(index)
                    .map(|interp| interp.read(inner))
                    .unwrap_or(0)
            }
            off if (reg::SPINLOCK0..=reg::SPINLOCK_LAST).contains(&off) => {
                let index = (off - reg::SPINLOCK0) / 4;
                if self.locks & (1 << index) != 0 {
                    0
                } else {
                    self.locks |= 1 << index;
                    self.locks
                }
            }
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32, core: usize) {
        match off {
            reg::GPIO_OUT => self.out = value,
            reg::GPIO_OUT_SET => self.out |= value,
            reg::GPIO_OUT_CLR => self.out &= !value,
            reg::GPIO_OUT_XOR => self.out ^= value,
            reg::GPIO_OE => self.oe = value,
            reg::GPIO_OE_SET => self.oe |= value,
            reg::GPIO_OE_CLR => self.oe &= !value,
            reg::GPIO_OE_XOR => self.oe ^= value,
            reg::FIFO_WR => self.fifo_write(core, value),
            reg::DIV_UDIVIDEND => {
                self.div_udividend = value;
                self.div_signed = false;
                self.divide();
            }
            reg::DIV_UDIVISOR => {
                self.div_udivisor = value;
                self.div_signed = false;
                self.divide();
            }
            reg::DIV_SDIVIDEND => {
                self.div_sdividend = value;
                self.div_signed = true;
                self.divide();
            }
            reg::DIV_SDIVISOR => {
                self.div_sdivisor = value;
                self.div_signed = true;
                self.divide();
            }
            // Writing a result register stops any calculation and latches the
            // value, which is how the SDK saves and restores the divider.
            reg::DIV_QUOTIENT => {
                self.div_quotient = value;
                self.div_dirty = true;
            }
            reg::DIV_REMAINDER => {
                self.div_remainder = value;
                self.div_dirty = true;
            }
            reg::DIV_CSR | reg::SPINLOCK_ST => {}
            off if (reg::INTERP0..reg::SPINLOCK0).contains(&off) => {
                let index = ((off - reg::INTERP0) / reg::INTERP_STRIDE) as usize;
                let inner = (off - reg::INTERP0) % reg::INTERP_STRIDE;
                if let Some(interp) = self.interp.get_mut(index) {
                    interp.write(inner, value);
                }
            }
            off if (reg::SPINLOCK0..=reg::SPINLOCK_LAST).contains(&off) => {
                let index = (off - reg::SPINLOCK0) / 4;
                self.locks &= !(1 << index);
            }
            off => self.regs.write(off, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fifo_crosses_the_cores() {
        let mut sio = Sio::new();
        sio.write(reg::FIFO_WR, 0x55, 0);
        assert!(sio.fifo_has_data(1), "core 1 receives what core 0 sent");
        assert!(!sio.fifo_has_data(0));
        assert_eq!(sio.read(reg::CPUID, 1), 1);
        assert_eq!(sio.read(reg::FIFO_RD, 1), 0x55);
        assert_eq!(sio.read(reg::CPUID, 0), 0);
    }

    #[test]
    fn fifo_status_reports_the_reading_cores_view() {
        let mut sio = Sio::new();
        assert_eq!(
            sio.read(reg::FIFO_ST, 0),
            0b10,
            "nothing to read, room to write"
        );
        sio.write(reg::FIFO_WR, 1, 0);
        assert_eq!(sio.read(reg::FIFO_ST, 1) & 1, 1);
    }

    #[test]
    fn a_spinlock_read_takes_it_and_a_write_frees_it() {
        let mut sio = Sio::new();
        let taken = sio.read(reg::SPINLOCK0 + 4 * 3, 0);
        assert_eq!(taken & (1 << 3), 1 << 3);
        assert_eq!(
            sio.read(reg::SPINLOCK0 + 4 * 3, 1),
            0,
            "the other core fails"
        );
        sio.write(reg::SPINLOCK0 + 4 * 3, 0, 1);
        assert_eq!(sio.read(reg::SPINLOCK0 + 4 * 3, 0) & (1 << 3), 1 << 3);
    }

    #[test]
    fn an_interpolator_shifts_masks_and_adds_the_base() {
        let mut sio = Sio::new();
        // Lane 0: ACCUM0 = 0xdeadbeef, shift 8, mask bits 7..4. The datasheet's
        // own worked example ends at 0xb.
        sio.write(reg::INTERP0 + 0x00, 0xdead_beef, 0);
        sio.write(
            reg::INTERP0 + 0x2c,
            8 | (4 << 5) | (7 << 10) | interp_ctrl::SIGNED,
            0,
        );
        assert_eq!(sio.read(reg::INTERP0 + 0x20, 0), 0xffff_ffb0);
        // An unsigned lane masks the same value to its four bits, and the
        // full result adds both lanes' shift-and-mask values to BASE2.
        sio.write(reg::INTERP0 + 0x2c, 8 | (4 << 5) | (7 << 10), 0);
        sio.write(reg::INTERP0 + 0x30, (0 << 5) | (31 << 10), 0);
        sio.write(reg::INTERP0 + 0x04, 0x1234_5678, 0);
        sio.write(reg::INTERP0 + 0x10, 0x1000, 0);
        assert_eq!(sio.read(reg::INTERP0 + 0x20, 0), 0x0000_00b0);
        let full = sio.read(reg::INTERP0 + 0x28, 0);
        assert_eq!(full, 0x1000 + 0xb0 + 0x1234_5678);
    }

    #[test]
    fn a_popping_lane_steps_its_accumulator_by_the_base() {
        let mut sio = Sio::new();
        // The nine times table: ACCUM0 starts at zero and BASE0 is 9, with the
        // lane passing the whole accumulator through.
        sio.write(reg::INTERP0 + 0x2c, 31 << 10, 0);
        sio.write(reg::INTERP0 + 0x08, 9, 0);
        assert_eq!(sio.read(reg::INTERP0 + 0x14, 0), 9);
        assert_eq!(sio.read(reg::INTERP0 + 0x14, 0), 18);
        assert_eq!(sio.read(reg::INTERP0, 0), 18, "POP wrote the step back");
    }

    #[test]
    fn accum_add_advances_a_phase_and_reads_the_lane_value() {
        let mut sio = Sio::new();
        sio.write(reg::INTERP0 + 0x2c, 31 << 10, 0);
        sio.write(reg::INTERP0 + 0x34, 7, 0);
        sio.write(reg::INTERP0 + 0x34, 3, 0);
        assert_eq!(sio.read(reg::INTERP0 + 0x34, 0), 10);
        assert_eq!(sio.read(reg::INTERP0, 0), 10);
    }

    #[test]
    fn blend_interpolates_between_the_two_bases() {
        let mut sio = Sio::new();
        sio.write(reg::INTERP0 + 0x2c, interp_ctrl::BLEND | (31 << 10), 0);
        sio.write(reg::INTERP0 + 0x30, 31 << 10, 0);
        sio.write(reg::INTERP0 + 0x08, 500, 0);
        sio.write(reg::INTERP0 + 0x0c, 1000, 0);
        // The datasheet's example: alpha 255 of 256 lands on 998, not 1000.
        sio.write(reg::INTERP0 + 0x04, 255, 0);
        assert_eq!(sio.read(reg::INTERP0 + 0x24, 0), 998);
        sio.write(reg::INTERP0 + 0x04, 0, 0);
        assert_eq!(sio.read(reg::INTERP0 + 0x24, 0), 500);
        // A signed blend spans negative bases: -1000 to 1000 at one half.
        sio.write(reg::INTERP0 + 0x30, interp_ctrl::SIGNED | (31 << 10), 0);
        sio.write(reg::INTERP0 + 0x3c, 0xe000_f000, 0);
        sio.write(reg::INTERP0 + 0x04, 128, 0);
        assert_eq!(
            sio.read(reg::INTERP0 + 0x24, 0),
            0xffff_e800,
            "the same value the datasheet prints for simple_blend3"
        );
    }

    #[test]
    fn a_divide_by_zero_matches_the_silicon() {
        let mut sio = Sio::new();
        sio.write(reg::DIV_UDIVIDEND, 100, 0);
        sio.write(reg::DIV_UDIVISOR, 0, 0);
        assert_eq!(sio.read(reg::DIV_QUOTIENT, 0), u32::MAX);
        assert_eq!(sio.read(reg::DIV_REMAINDER, 0), 100);
    }
}
