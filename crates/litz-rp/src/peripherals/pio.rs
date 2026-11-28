//! The two PIO blocks: four state machines each, nine instructions, the
//! shift registers and their FIFOs, side-set, and the eight IRQ flags.
//!
//! A state machine is a program counter over a 32-word instruction memory, two
//! scratch registers (`X`, `Y`), an input and an output shift register, a
//! four-word FIFO in each direction and a pin group selected by
//! `SMx_PINCTRL`. [`Pio::tick`] advances the block by the machine's cycles: the
//! 16.8 clock divider decides when an instruction runs, the instruction's delay
//! field holds the next one off, and a stalled instruction — a `WAIT`, a
//! blocking `PULL` on an empty FIFO, a `PUSH` into a full one — is retried
//! without advancing the program counter.
//!
//! The block does not know which pins the IO mux has handed it. [`Pio::tick`]
//! is given the level every pad presents to the chip and [`Pio::drive`] is what
//! the machine asks when it builds each pin's level, so a PIO output reaches
//! the board only while `GPIOx_CTRL.FUNCSEL` names this block.
//!
//! One piece of the silicon is simplified: the two-cycle input synchroniser is
//! bypassed, so a pin read by `IN`, `WAIT` or `JMP PIN` is the level the
//! machine last saw rather than the level two cycles ago. `INPUT_SYNC_BYPASS`
//! asks for exactly that, and the delayed path changes no program's result.

use std::collections::VecDeque;

use super::RegisterFile;

/// Register offsets from a PIO block's base.
pub mod reg {
    pub const CTRL: u32 = 0x000;
    pub const FSTAT: u32 = 0x004;
    pub const FDEBUG: u32 = 0x008;
    pub const FLEVEL: u32 = 0x00c;
    pub const TXF0: u32 = 0x010;
    pub const RXF0: u32 = 0x020;
    pub const IRQ: u32 = 0x030;
    pub const IRQ_FORCE: u32 = 0x034;
    pub const INPUT_SYNC_BYPASS: u32 = 0x038;
    pub const DBG_PADOUT: u32 = 0x03c;
    pub const DBG_PADOE: u32 = 0x040;
    pub const DBG_CFGINFO: u32 = 0x044;
    pub const INSTR_MEM0: u32 = 0x048;
    pub const INSTR_MEM31: u32 = 0x0c4;
    pub const SM0_CLKDIV: u32 = 0x0c8;
    pub const SM_STRIDE: u32 = 0x18;
    pub const INTR: u32 = 0x128;
    pub const IRQ0_INTE: u32 = 0x12c;
    pub const IRQ0_INTF: u32 = 0x130;
    pub const IRQ0_INTS: u32 = 0x134;
    pub const IRQ1_INTE: u32 = 0x138;
    pub const IRQ1_INTF: u32 = 0x13c;
    pub const IRQ1_INTS: u32 = 0x140;
}

/// `SMx_CLKDIV` fields: a 16.8 fixed-point divider.
pub mod clkdiv {
    pub const FRAC_SHIFT: u32 = 8;
    pub const INT_SHIFT: u32 = 16;
}

/// `SMx_EXECCTRL` fields.
pub mod execctrl {
    pub const WRAP_BOTTOM_SHIFT: u32 = 7;
    pub const WRAP_BOTTOM_MASK: u32 = 0xf << 7;
    pub const WRAP_TOP_SHIFT: u32 = 12;
    pub const WRAP_TOP_MASK: u32 = 0x1f << 12;
    pub const STATUS_SEL: u32 = 1 << 4;
    pub const JMP_PIN_SHIFT: u32 = 24;
    pub const JMP_PIN_MASK: u32 = 0x1f << 24;
    pub const SIDE_PINDIR: u32 = 1 << 29;
    pub const SIDE_EN: u32 = 1 << 30;
}

/// `SMx_SHIFTCTRL` fields.
pub mod shiftctrl {
    pub const AUTOPUSH: u32 = 1 << 16;
    pub const AUTOPULL: u32 = 1 << 17;
    pub const IN_SHIFTDIR: u32 = 1 << 18;
    pub const OUT_SHIFTDIR: u32 = 1 << 19;
    pub const PUSH_THRESH_SHIFT: u32 = 20;
    pub const PUSH_THRESH_MASK: u32 = 0x1f << 20;
    pub const PULL_THRESH_SHIFT: u32 = 25;
    pub const PULL_THRESH_MASK: u32 = 0x1f << 25;
    pub const FJOIN_TX: u32 = 1 << 30;
    pub const FJOIN_RX: u32 = 1 << 31;
}

/// `SMx_PINCTRL` fields.
pub mod pinctrl {
    pub const OUT_BASE_SHIFT: u32 = 0;
    pub const OUT_BASE_MASK: u32 = 0x1f;
    pub const SET_BASE_SHIFT: u32 = 5;
    pub const SET_BASE_MASK: u32 = 0x1f << 5;
    pub const SIDESET_BASE_SHIFT: u32 = 10;
    pub const SIDESET_BASE_MASK: u32 = 0x1f << 10;
    pub const IN_BASE_SHIFT: u32 = 15;
    pub const IN_BASE_MASK: u32 = 0x1f << 15;
    pub const OUT_COUNT_SHIFT: u32 = 20;
    pub const OUT_COUNT_MASK: u32 = 0x3f << 20;
    pub const SET_COUNT_SHIFT: u32 = 26;
    pub const SET_COUNT_MASK: u32 = 0x7 << 26;
    pub const SIDESET_COUNT_SHIFT: u32 = 29;
    pub const SIDESET_COUNT_MASK: u32 = 0x7 << 29;
}

/// `CTRL` fields.
pub mod ctrl {
    pub const SM_ENABLE: u32 = 0xf;
    pub const SM_RESTART: u32 = 0xf << 4;
    pub const CLKDIV_RESTART: u32 = 0xf << 8;
}

/// `FSTAT` fields: one bit per state machine.
pub mod fstat {
    pub const RXFULL: u32 = 0xf;
    pub const RXEMPTY: u32 = 0xf << 8;
    pub const TXFULL: u32 = 0xf << 16;
    pub const TXEMPTY: u32 = 0xf << 24;
}

pub const STATE_MACHINES: usize = 4;
/// Words per FIFO. Joining the two queues gives one eight-deep queue.
pub const FIFO_DEPTH: usize = 4;
/// Words of instruction memory per block.
pub const INSTRUCTION_MEMORY: usize = 32;

/// One state machine and everything it owns between instructions.
#[derive(Clone, Debug)]
pub struct StateMachine {
    pub clkdiv: u32,
    pub execctrl: u32,
    pub shiftctrl: u32,
    pub pc: u32,
    pub pinctrl: u32,
    pub x: u32,
    pub y: u32,
    pub isr: u32,
    pub osr: u32,
    /// Bits shifted into `ISR` since its last push, and left in `OSR`.
    pub isr_count: u8,
    pub osr_count: u8,
    /// State-machine clocks the instruction's delay field still owes.
    delay: u32,
    /// System clocks accrued towards the divider, in 1/256 clocks.
    clock: u64,
    stalled: bool,
    /// A taken `JMP` has already set `pc`; the sequential advance after the
    /// instruction must not add one on top of it.
    jumped: bool,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    pub fn new() -> Self {
        StateMachine {
            // Reset divider is 1.0: one instruction per system clock.
            clkdiv: 1 << clkdiv::INT_SHIFT,
            execctrl: 0x0001_f000,
            // Both shift registers reset to shift right.
            shiftctrl: shiftctrl::IN_SHIFTDIR | shiftctrl::OUT_SHIFTDIR,
            pc: 0,
            pinctrl: 0,
            x: 0,
            y: 0,
            isr: 0,
            osr: 0,
            isr_count: 0,
            // Both shift registers come out of reset empty, so the first OUT
            // pulls before it shifts anything.
            osr_count: 0,
            delay: 0,
            clock: 0,
            stalled: false,
            jumped: false,
        }
    }

    pub fn pc(&self) -> u32 {
        self.pc & 0x1f
    }

    pub fn stalled(&self) -> bool {
        self.stalled
    }

    fn out_shift_right(&self) -> bool {
        self.shiftctrl & shiftctrl::OUT_SHIFTDIR != 0
    }

    fn in_shift_right(&self) -> bool {
        self.shiftctrl & shiftctrl::IN_SHIFTDIR != 0
    }

    fn push_threshold(&self) -> u8 {
        let value =
            ((self.shiftctrl & shiftctrl::PUSH_THRESH_MASK) >> shiftctrl::PUSH_THRESH_SHIFT) as u8;
        if value == 0 { 32 } else { value }
    }

    fn pull_threshold(&self) -> u8 {
        let value =
            ((self.shiftctrl & shiftctrl::PULL_THRESH_MASK) >> shiftctrl::PULL_THRESH_SHIFT) as u8;
        if value == 0 { 32 } else { value }
    }

    fn wrap_top(&self) -> u32 {
        (self.execctrl & execctrl::WRAP_TOP_MASK) >> execctrl::WRAP_TOP_SHIFT
    }

    fn wrap_bottom(&self) -> u32 {
        (self.execctrl & execctrl::WRAP_BOTTOM_MASK) >> execctrl::WRAP_BOTTOM_SHIFT
    }

    fn out_base(&self) -> u32 {
        (self.pinctrl & pinctrl::OUT_BASE_MASK) >> pinctrl::OUT_BASE_SHIFT
    }

    fn out_count(&self) -> u32 {
        ((self.pinctrl & pinctrl::OUT_COUNT_MASK) >> pinctrl::OUT_COUNT_SHIFT).min(32)
    }

    fn set_base(&self) -> u32 {
        (self.pinctrl & pinctrl::SET_BASE_MASK) >> pinctrl::SET_BASE_SHIFT
    }

    fn set_count(&self) -> u32 {
        (self.pinctrl & pinctrl::SET_COUNT_MASK) >> pinctrl::SET_COUNT_SHIFT
    }

    fn sideset_base(&self) -> u32 {
        (self.pinctrl & pinctrl::SIDESET_BASE_MASK) >> pinctrl::SIDESET_BASE_SHIFT
    }

    fn sideset_count(&self) -> u32 {
        ((self.pinctrl & pinctrl::SIDESET_COUNT_MASK) >> pinctrl::SIDESET_COUNT_SHIFT).min(5)
    }

    fn in_base(&self) -> u32 {
        (self.pinctrl & pinctrl::IN_BASE_MASK) >> pinctrl::IN_BASE_SHIFT
    }

    fn jmp_pin(&self) -> u32 {
        (self.execctrl & execctrl::JMP_PIN_MASK) >> execctrl::JMP_PIN_SHIFT
    }

    fn restart(&mut self) {
        self.pc = 0;
        self.delay = 0;
        self.clock = 0;
        self.isr_count = 0;
        self.osr_count = 32;
        self.stalled = false;
        self.jumped = false;
    }

    /// Shift `count` bits of `value` into the input shift register.
    fn shift_in(&mut self, value: u32, count: u32) {
        if count == 0 {
            return;
        }
        let value = value & bit_mask(count);
        self.isr = if self.in_shift_right() {
            let kept = if count >= 32 { 0 } else { self.isr >> count };
            kept | value.wrapping_shl(32 - count)
        } else {
            let kept = if count >= 32 { 0 } else { self.isr << count };
            kept | value
        };
        self.isr_count = self.isr_count.saturating_add(count as u8).min(32);
    }

    /// Take `count` bits out of the output shift register.
    fn shift_out(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let value = if self.out_shift_right() {
            self.osr & bit_mask(count)
        } else if count >= 32 {
            self.osr
        } else {
            (self.osr >> (32 - count)) & bit_mask(count)
        };
        self.osr = if self.out_shift_right() {
            if count >= 32 { 0 } else { self.osr >> count }
        } else if count >= 32 {
            0
        } else {
            self.osr << count
        };
        self.osr_count = self.osr_count.saturating_sub(count as u8);
        value
    }
}

/// A mask with the low `count` bits set; 32 bits for a count of 32 or more.
fn bit_mask(count: u32) -> u32 {
    match count {
        0 => 0,
        32.. => u32::MAX,
        count => (1u32 << count) - 1,
    }
}

/// One PIO block.
#[derive(Debug)]
pub struct Pio {
    pub ctrl: u32,
    pub sm: [StateMachine; STATE_MACHINES],
    /// The eight interrupt flags.
    pub irq: u32,
    pub irq_force: u32,
    irq0_inte: u32,
    irq0_intf: u32,
    irq1_inte: u32,
    irq1_intf: u32,
    /// Program memory, 16 bits meaningful per word.
    pub program: [u16; INSTRUCTION_MEMORY],
    /// The TX FIFO each state machine drains and the RX FIFO it fills.
    pub tx: [VecDeque<u32>; STATE_MACHINES],
    pub rx: [VecDeque<u32>; STATE_MACHINES],
    /// The level and output enable the block drives per pin. Both are shared
    /// between the four state machines, so the last write to a pin wins.
    pub pad_out: u32,
    pub pad_oe: u32,
    /// The pad levels the block last sampled, one bit per GPIO.
    pads: u32,
    /// The last word a core wrote to a TX FIFO, with the state machine that
    /// owns the queue, kept until the host collects it.
    pending_write: Option<(u8, u32)>,
    /// Writes to `TXF` that found the FIFO full, reported in FDEBUG.
    tx_stalls: [u32; STATE_MACHINES],
    regs: RegisterFile,
}

impl Default for Pio {
    fn default() -> Self {
        Self::new()
    }
}

impl Pio {
    pub fn new() -> Self {
        Pio {
            ctrl: 0,
            sm: [
                StateMachine::new(),
                StateMachine::new(),
                StateMachine::new(),
                StateMachine::new(),
            ],
            irq: 0,
            irq_force: 0,
            irq0_inte: 0,
            irq0_intf: 0,
            irq1_inte: 0,
            irq1_intf: 0,
            program: [0; INSTRUCTION_MEMORY],
            tx: Default::default(),
            rx: Default::default(),
            pad_out: 0,
            pad_oe: 0,
            pads: 0,
            pending_write: None,
            tx_stalls: [0; STATE_MACHINES],
            regs: RegisterFile::new(0x148 / 4),
        }
    }

    fn depth(&self, sm: usize) -> usize {
        if self.sm[sm].shiftctrl & (shiftctrl::FJOIN_TX | shiftctrl::FJOIN_RX) != 0 {
            FIFO_DEPTH * 2
        } else {
            FIFO_DEPTH
        }
    }

    fn tx_full(&self, sm: usize) -> bool {
        if self.sm[sm].shiftctrl & shiftctrl::FJOIN_RX != 0 {
            // The RX queue owns the storage: TX reads as both full and empty.
            return true;
        }
        self.tx[sm].len() >= self.depth(sm)
    }

    fn tx_empty(&self, sm: usize) -> bool {
        if self.sm[sm].shiftctrl & shiftctrl::FJOIN_RX != 0 {
            return true;
        }
        self.tx[sm].is_empty()
    }

    fn rx_full(&self, sm: usize) -> bool {
        if self.sm[sm].shiftctrl & shiftctrl::FJOIN_TX != 0 {
            return true;
        }
        self.rx[sm].len() >= self.depth(sm)
    }

    fn rx_empty(&self, sm: usize) -> bool {
        if self.sm[sm].shiftctrl & shiftctrl::FJOIN_TX != 0 {
            return true;
        }
        self.rx[sm].is_empty()
    }

    /// The word a state machine has received, for a host that wants the stream
    /// rather than the pin waveform.
    pub fn read_rx(&mut self, sm: usize) -> Option<u32> {
        self.rx.get_mut(sm).and_then(|queue| queue.pop_front())
    }

    /// Push a word into a state machine's transmit FIFO; false when it is full.
    pub fn push_tx(&mut self, sm: usize, word: u32) -> bool {
        if sm >= STATE_MACHINES || self.tx_full(sm) {
            return false;
        }
        self.tx[sm].push_back(word);
        true
    }

    /// Whether the transmit FIFO can take another word — the event a DMA
    /// channel writing into this state machine paces itself on.
    pub fn tx_ready(&self, sm: usize) -> bool {
        sm < STATE_MACHINES && !self.tx_full(sm)
    }

    /// Whether the receive FIFO holds a word for a DMA channel to take.
    pub fn rx_ready(&self, sm: usize) -> bool {
        sm < STATE_MACHINES && !self.rx_empty(sm)
    }

    /// The level this block drives on `gpio`, if it drives it at all.
    pub fn drive(&self, gpio: u8) -> Option<bool> {
        let bit = 1u32 << (gpio & 31);
        (self.pad_oe & bit != 0).then(|| self.pad_out & bit != 0)
    }

    /// Whether a state machine's output enable covers this pin, for the
    /// interrupt/status view of the pad.
    pub fn output_enabled(&self, gpio: u8) -> bool {
        self.pad_oe & (1 << (gpio & 31)) != 0
    }

    pub fn irq_flag(&self, flag: u8) -> bool {
        // A forced flag reads as set everywhere, which is what a self-test
        // that pokes `IRQ_FORCE` expects.
        (self.irq | self.irq_force) & (1 << (flag & 7)) != 0
    }

    /// Interrupt line 0 or 1 as the block presents it to the NVIC.
    pub fn irq_line(&self, line: usize) -> bool {
        let (inte, intf) = if line == 0 {
            (self.irq0_inte, self.irq0_intf)
        } else {
            (self.irq1_inte, self.irq1_intf)
        };
        (((self.irq | self.irq_force) & inte) | intf) & 0xff != 0
    }

    /// A word a core wrote to a `TXF`, waiting for the machine to see it.
    pub fn take_pending_write(&mut self) -> Option<(u8, u32)> {
        self.pending_write.take()
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::CTRL => self.ctrl,
            reg::FSTAT => {
                let mut word = 0;
                for sm in 0..STATE_MACHINES {
                    if self.rx_full(sm) {
                        word |= 1 << sm;
                    }
                    if self.rx_empty(sm) {
                        word |= 1 << (8 + sm);
                    }
                    if self.tx_full(sm) {
                        word |= 1 << (16 + sm);
                    }
                    if self.tx_empty(sm) {
                        word |= 1 << (24 + sm);
                    }
                }
                word
            }
            reg::FDEBUG => {
                let mut word = 0;
                for (sm, count) in self.tx_stalls.iter().enumerate() {
                    word |= (count & 0xf) << (sm * 4);
                }
                word
            }
            reg::FLEVEL => {
                let mut word = 0;
                for sm in 0..STATE_MACHINES {
                    word |= (self.tx[sm].len() as u32 & 0xf) << (sm * 8);
                    word |= (self.rx[sm].len() as u32 & 0xf) << (sm * 8 + 4);
                }
                word
            }
            reg::IRQ => self.irq | self.irq_force,
            reg::IRQ_FORCE => self.irq_force,
            reg::DBG_PADOUT => self.pad_out,
            reg::DBG_PADOE => self.pad_oe,
            // Four state machines, 32 instructions, four-deep FIFOs.
            reg::DBG_CFGINFO => {
                (STATE_MACHINES as u32) << 8 | (INSTRUCTION_MEMORY as u32) << 16 | FIFO_DEPTH as u32
            }
            reg::INTR => {
                // One register for both lines: the eight flags, before any
                // enable mask, in the low byte and again in the high one.
                let flags = (self.irq | self.irq_force) & 0xff;
                flags | (flags << 8)
            }
            reg::IRQ0_INTE => self.irq0_inte,
            reg::IRQ0_INTF => self.irq0_intf,
            reg::IRQ0_INTS => ((self.irq | self.irq_force) & self.irq0_inte) | self.irq0_intf,
            reg::IRQ1_INTE => self.irq1_inte,
            reg::IRQ1_INTF => self.irq1_intf,
            reg::IRQ1_INTS => ((self.irq | self.irq_force) & self.irq1_inte) | self.irq1_intf,
            off if (reg::TXF0..reg::TXF0 + 16).contains(&off) => {
                let sm = ((off - reg::TXF0) / 4) as usize;
                self.tx.get_mut(sm).and_then(|q| q.pop_front()).unwrap_or(0)
            }
            off if (reg::RXF0..reg::RXF0 + 16).contains(&off) => {
                let sm = ((off - reg::RXF0) / 4) as usize;
                self.rx.get_mut(sm).and_then(|q| q.pop_front()).unwrap_or(0)
            }
            off if (reg::INSTR_MEM0..=reg::INSTR_MEM31).contains(&off) => {
                u32::from(self.program[((off - reg::INSTR_MEM0) / 4) as usize])
            }
            off if (reg::SM0_CLKDIV..reg::SM0_CLKDIV + reg::SM_STRIDE * 4).contains(&off) => {
                let (sm, field) = Self::split_sm(off);
                match field {
                    0 => self.sm[sm].clkdiv,
                    1 => self.sm[sm].execctrl,
                    2 => self.sm[sm].shiftctrl,
                    3 => self.sm[sm].pc,
                    // `SMx_INSTR` reads as the instruction at the current pc.
                    4 => u32::from(self.program[(self.sm[sm].pc & 0x1f) as usize]),
                    _ => self.sm[sm].pinctrl,
                }
            }
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CTRL => {
                let written = value & (ctrl::SM_ENABLE | ctrl::SM_RESTART | ctrl::CLKDIV_RESTART);
                for sm in 0..STATE_MACHINES {
                    let bit = 1 << sm;
                    if written & ctrl::SM_RESTART & bit != 0 {
                        self.sm[sm].restart();
                    } else if written & ctrl::CLKDIV_RESTART & bit != 0 {
                        self.sm[sm].clock = 0;
                    }
                }
                // The restart bits are self-clearing; only the enables stay.
                self.ctrl = written & ctrl::SM_ENABLE;
            }
            reg::IRQ => {
                // Write one to clear the flagged bits.
                self.irq &= !(value & 0xff);
            }
            reg::IRQ_FORCE => self.irq_force = value & 0xff,
            reg::IRQ0_INTE => self.irq0_inte = value & 0xff,
            reg::IRQ0_INTF => self.irq0_intf = value & 0xff,
            reg::IRQ1_INTE => self.irq1_inte = value & 0xff,
            reg::IRQ1_INTF => self.irq1_intf = value & 0xff,
            reg::INTR | reg::IRQ0_INTS | reg::IRQ1_INTS => {}
            off if (reg::TXF0..reg::TXF0 + 16).contains(&off) => {
                let sm = ((off - reg::TXF0) / 4) as usize;
                if self.tx_full(sm) {
                    self.tx_stalls[sm] = self.tx_stalls[sm].saturating_add(1);
                } else {
                    self.tx[sm].push_back(value);
                    self.pending_write = Some((sm as u8, value));
                }
            }
            off if (reg::RXF0..reg::RXF0 + 16).contains(&off) => {
                let sm = ((off - reg::RXF0) / 4) as usize;
                if self.rx_full(sm) {
                    self.tx_stalls[sm] = self.tx_stalls[sm].saturating_add(1);
                } else {
                    self.rx[sm].push_back(value);
                }
            }
            off if (reg::INSTR_MEM0..=reg::INSTR_MEM31).contains(&off) => {
                self.program[((off - reg::INSTR_MEM0) / 4) as usize] = value as u16;
            }
            off if (reg::SM0_CLKDIV..reg::SM0_CLKDIV + reg::SM_STRIDE * 4).contains(&off) => {
                let (sm, field) = Self::split_sm(off);
                match field {
                    0 => self.sm[sm].clkdiv = value,
                    1 => self.sm[sm].execctrl = value & !(1 << 31),
                    2 => {
                        let joined_changed = (self.sm[sm].shiftctrl ^ value)
                            & (shiftctrl::FJOIN_TX | shiftctrl::FJOIN_RX)
                            != 0;
                        self.sm[sm].shiftctrl = value;
                        if joined_changed {
                            // Changing the join state flushes both queues.
                            self.tx[sm].clear();
                            self.rx[sm].clear();
                        }
                    }
                    3 => self.sm[sm].pc = value & 0x1f,
                    4 => {
                        // Writing an instruction executes it now, the way
                        // `pio_sm_exec` expects; the delay field still holds
                        // the next programmed instruction off.
                        self.execute(sm, value as u16);
                        // A jump written here moved the counter itself; the
                        // next tick starts from it without a stale marker.
                        self.sm[sm].jumped = false;
                    }
                    _ => self.sm[sm].pinctrl = value,
                }
            }
            off => self.regs.write(off, value),
        }
    }

    fn split_sm(off: u32) -> (usize, u32) {
        let rel = off - reg::SM0_CLKDIV;
        (
            (rel / reg::SM_STRIDE) as usize % STATE_MACHINES,
            (rel % reg::SM_STRIDE) / 4,
        )
    }

    /// Advance every enabled state machine by `cycles` system clocks.
    ///
    /// `pads` is the level each GPIO presents to the chip, one bit per pin.
    pub fn tick(&mut self, cycles: u64, pads: u32) {
        self.pads = pads;
        for sm in 0..STATE_MACHINES {
            if self.ctrl & (1 << sm) == 0 {
                continue;
            }
            // The divider is 16.8 fixed point over system clocks. Time and
            // the divider are both counted in 1/65536 of a clock — the
            // register's integer part is exactly that — so the division is
            // exact for every value, including a divider below 1.0, which
            // runs the machine faster than the system clock. A value of zero
            // means the slowest divider, 65536 clocks, not the fastest.
            let raw = self.sm[sm].clkdiv;
            let divider = if raw == 0 {
                65_536u64 << 16
            } else {
                u64::from(raw)
            };
            self.sm[sm].clock += cycles << 16;
            let mut budget = cycles.saturating_mul(1 << 16).max(1);
            while self.sm[sm].clock >= divider && budget > 0 {
                self.sm[sm].clock -= divider;
                budget -= 1;
                if self.sm[sm].delay > 0 {
                    self.sm[sm].delay -= 1;
                    continue;
                }
                let instruction = self.program[(self.sm[sm].pc & 0x1f) as usize];
                let stalled = self.execute(sm, instruction);
                // A taken jump already moved the program counter; everything
                // else that ran moves on by one.
                if self.sm[sm].jumped {
                    self.sm[sm].jumped = false;
                } else if !stalled {
                    self.advance_pc(sm);
                }
            }
        }
    }

    fn advance_pc(&mut self, sm: usize) {
        let top = self.sm[sm].wrap_top();
        let bottom = self.sm[sm].wrap_bottom();
        let mut next = self.sm[sm].pc + 1;
        if next > top {
            next = bottom;
        }
        self.sm[sm].pc = next & 0x1f;
    }

    /// Execute one instruction. Returns `true` when it stalled and must run
    /// again on the next clock.
    fn execute(&mut self, sm: usize, instr: u16) -> bool {
        let sideset_count = self.sm[sm].sideset_count();
        if sideset_count > 0 {
            let field = u32::from((instr >> 8) & 0x1f);
            let side = field >> (5 - sideset_count);
            let data_bits =
                sideset_count - u32::from(self.sm[sm].execctrl & execctrl::SIDE_EN != 0);
            let (enabled, data) = if self.sm[sm].execctrl & execctrl::SIDE_EN != 0 {
                let enable_bit = 1u32 << data_bits;
                (side & enable_bit != 0, side & (enable_bit - 1))
            } else {
                (true, side)
            };
            if enabled {
                let directions = self.sm[sm].execctrl & execctrl::SIDE_PINDIR != 0;
                let base = self.sm[sm].sideset_base();
                self.write_pins(base, data, data_bits, directions);
            }
        }
        let delay_bits = 5 - sideset_count;
        let delay = if delay_bits == 0 {
            0
        } else {
            u32::from(instr >> 8) & ((1 << delay_bits) - 1)
        };
        let fields = u32::from(instr);
        let op = (fields >> 13) & 7;
        let arg = fields & 0x1f;
        let stalled = match op {
            0 => self.execute_jump(sm, (fields >> 5) & 7, arg as u8),
            1 => self.execute_wait(sm, (fields >> 7) & 1, (fields >> 5) & 3, arg),
            2 => self.execute_in(sm, (fields >> 5) & 7, if arg == 0 { 32 } else { arg }),
            3 => self.execute_out(sm, (fields >> 5) & 7, if arg == 0 { 32 } else { arg }),
            4 => self.execute_push_pull(sm, instr),
            5 => self.execute_mov(sm, (fields >> 5) & 7, (fields >> 3) & 3, arg),
            6 => self.execute_irq(sm, instr),
            _ => {
                self.execute_set(sm, (fields >> 5) & 7, arg);
                false
            }
        };
        self.sm[sm].stalled = stalled;
        if !stalled {
            self.sm[sm].delay = delay;
        }
        stalled
    }

    /// Write `count` pins starting at `base` from the low bits of `value`.
    fn write_pins(&mut self, base: u32, value: u32, count: u32, directions: bool) {
        for i in 0..count.min(32) {
            let gpio = (base + i) & 31;
            let bit = 1u32 << gpio;
            if value & (1 << i) != 0 {
                if directions {
                    self.pad_oe |= bit;
                } else {
                    self.pad_out |= bit;
                }
            } else if directions {
                self.pad_oe &= !bit;
            } else {
                self.pad_out &= !bit;
            }
        }
    }

    fn execute_set(&mut self, sm: usize, destination: u32, value: u32) {
        match destination {
            0 => {
                let (base, count) = (self.sm[sm].set_base(), self.sm[sm].set_count());
                self.write_pins(base, value, count, false);
            }
            1 => self.sm[sm].x = value,
            2 => self.sm[sm].y = value,
            4 => {
                let (base, count) = (self.sm[sm].set_base(), self.sm[sm].set_count());
                self.write_pins(base, value, count, true);
            }
            _ => {}
        }
    }

    fn execute_jump(&mut self, sm: usize, condition: u32, address: u8) -> bool {
        let take = match condition {
            0 => true,
            1 => self.sm[sm].x == 0,
            2 => {
                let taken = self.sm[sm].x != 0;
                if taken {
                    self.sm[sm].x = self.sm[sm].x.wrapping_sub(1);
                }
                taken
            }
            3 => self.sm[sm].y == 0,
            4 => {
                let taken = self.sm[sm].y != 0;
                if taken {
                    self.sm[sm].y = self.sm[sm].y.wrapping_sub(1);
                }
                taken
            }
            5 => self.sm[sm].x != self.sm[sm].y,
            6 => self.pads & (1 << self.sm[sm].jmp_pin()) != 0,
            // `JMP !OSRE`: the output shift register still holds bits.
            _ => self.sm[sm].osr_count != 0,
        };
        if take {
            self.sm[sm].pc = u32::from(address) & 0x1f;
            self.sm[sm].jumped = true;
        }
        false
    }

    fn execute_wait(&mut self, sm: usize, polarity: u32, source: u32, index: u32) -> bool {
        match source {
            // GPIO: an absolute pin number.
            0 => (self.pads >> (index & 31) & 1) != polarity,
            // PIN: relative to IN_BASE.
            1 => {
                let gpio = (self.sm[sm].in_base() + index) & 31;
                (self.pads >> gpio & 1) != polarity
            }
            // IRQ: the flag itself, set (polarity 1) or clear (0).
            2 => self.irq_flag((index & 7) as u8) != (polarity == 1),
            _ => false,
        }
    }

    fn execute_in(&mut self, sm: usize, source: u32, count: u32) -> bool {
        let data = match source {
            0 => {
                let base = self.sm[sm].in_base();
                let mut value = 0;
                for i in 0..count {
                    let gpio = (base + i) & 31;
                    if self.pads & (1 << gpio) != 0 {
                        value |= 1 << i;
                    }
                }
                value
            }
            1 => self.sm[sm].x,
            2 => self.sm[sm].y,
            3 => 0,
            6 => self.sm[sm].isr,
            7 => self.sm[sm].osr,
            _ => 0,
        };
        let autopush = self.sm[sm].shiftctrl & shiftctrl::AUTOPUSH != 0;
        let threshold = self.sm[sm].push_threshold();
        // A full register pushes before the shift, and an autopush into a full
        // FIFO stalls the instruction rather than dropping the word.
        if autopush && self.sm[sm].isr_count >= threshold {
            if self.rx_full(sm) {
                return true;
            }
            self.rx[sm].push_back(self.sm[sm].isr);
            self.sm[sm].isr_count = 0;
        }
        self.sm[sm].shift_in(data, count);
        if autopush && self.sm[sm].isr_count >= threshold && !self.rx_full(sm) {
            self.rx[sm].push_back(self.sm[sm].isr);
            self.sm[sm].isr_count = 0;
        }
        false
    }

    fn execute_out(&mut self, sm: usize, destination: u32, count: u32) -> bool {
        let threshold = self.sm[sm].pull_threshold();
        if self.sm[sm].shiftctrl & shiftctrl::AUTOPULL != 0
            && self.sm[sm].osr_count <= 32 - threshold
        {
            if self.tx_empty(sm) {
                return true;
            }
            self.sm[sm].osr = self.tx[sm].pop_front().unwrap_or(0);
            self.sm[sm].osr_count = 32;
        }
        let value = self.sm[sm].shift_out(count);
        match destination {
            0 => {
                let (base, pins) = (self.sm[sm].out_base(), self.sm[sm].out_count());
                self.write_pins(base, value, pins, false);
            }
            1 => self.sm[sm].x = value,
            2 => self.sm[sm].y = value,
            3 => {}
            4 => {
                let (base, pins) = (self.sm[sm].out_base(), self.sm[sm].out_count());
                self.write_pins(base, value, pins, true);
            }
            5 => {
                self.sm[sm].pc = value & 0x1f;
                self.sm[sm].jumped = true;
            }
            6 => self.sm[sm].isr = value,
            7 => {
                self.execute(sm, value as u16);
            }
            _ => {}
        }
        false
    }

    fn execute_push_pull(&mut self, sm: usize, instr: u16) -> bool {
        // Bit 7 names the direction: clear pushes the ISR, set pulls into the
        // OSR. Bit 6 conditions the instruction on the shift counter and bit
        // 5 is BLOCK, which is the assembler's default.
        let push = instr & 0x0080 == 0;
        let conditional = instr & 0x0040 != 0;
        let block = instr & 0x0020 != 0;
        if push {
            if conditional && self.sm[sm].isr_count < self.sm[sm].push_threshold() {
                return false;
            }
            if self.rx_full(sm) {
                if block {
                    return true;
                }
                // A push into a full FIFO loses the word, and the ISR is
                // cleared either way.
            } else {
                self.rx[sm].push_back(self.sm[sm].isr);
            }
            self.sm[sm].isr = 0;
            self.sm[sm].isr_count = 0;
            false
        } else {
            // With autopull on, a PULL is a barrier rather than a reload while
            // the OSR still holds bits.
            if self.sm[sm].shiftctrl & shiftctrl::AUTOPULL != 0 && self.sm[sm].osr_count == 32 {
                return false;
            }
            if conditional && self.sm[sm].osr_count > 32 - self.sm[sm].pull_threshold() {
                return false;
            }
            if self.tx_empty(sm) {
                if block {
                    return true;
                }
                // A non-blocking pull from an empty FIFO copies X, which is how
                // a program keeps clocking with the last word it had.
                self.sm[sm].osr = self.sm[sm].x;
                self.sm[sm].osr_count = 32;
                return false;
            }
            self.sm[sm].osr = self.tx[sm].pop_front().unwrap_or(0);
            self.sm[sm].osr_count = 32;
            false
        }
    }

    fn execute_mov(&mut self, sm: usize, destination: u32, operation: u32, source: u32) -> bool {
        let mut value = match source {
            0 => self.pads.rotate_right(self.sm[sm].in_base()),
            1 => self.sm[sm].x,
            2 => self.sm[sm].y,
            3 => 0,
            4 => {
                // STATUS holds the two FIFO status bits the block selects.
                let tx = u32::from(self.tx_empty(sm));
                let rx = u32::from(self.rx_empty(sm));
                if self.sm[sm].execctrl & execctrl::STATUS_SEL != 0 {
                    rx
                } else {
                    tx
                }
            }
            5 => self.sm[sm].isr,
            6 => self.sm[sm].osr,
            _ => 0,
        };
        value = match operation {
            1 => !value,
            2 => value.reverse_bits(),
            _ => value,
        };
        match destination {
            0 => {
                let (base, count) = (self.sm[sm].out_base(), self.sm[sm].out_count());
                self.write_pins(base, value, count, false);
            }
            1 => self.sm[sm].x = value,
            2 => self.sm[sm].y = value,
            4 => {
                self.execute(sm, value as u16);
            }
            5 => {
                self.sm[sm].pc = value & 0x1f;
                self.sm[sm].jumped = true;
            }
            6 => self.sm[sm].isr = value,
            7 => self.sm[sm].osr = value,
            _ => {}
        }
        false
    }

    fn execute_irq(&mut self, sm: usize, instr: u16) -> bool {
        // Bit 5 waits, bit 6 clears, bit 7 makes the index relative; with
        // neither of the first two the instruction sets the flag.
        let wait = instr & 0x0020 != 0;
        let clear = instr & 0x0040 != 0;
        let relative = instr & 0x0080 != 0;
        let mut index = u32::from(instr & 0x1f);
        if relative {
            // The low two bits name the flag and the machine's own index is
            // added to them, modulo the four machines.
            index = (index & 0x0c) | ((index & 0x03) + sm as u32) & 0x03;
        }
        let flag = (index & 7) as u8;
        if wait {
            if !self.irq_flag(flag) {
                return true;
            }
            // `IRQ WAIT CLEAR` consumes the flag once it sees it set.
            if clear {
                self.irq &= !(1 << flag);
            }
            return false;
        }
        if clear {
            self.irq &= !(1 << flag);
        } else {
            self.irq |= 1 << flag;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enable(pio: &mut Pio, sm: usize) {
        pio.write(reg::CTRL, pio.ctrl | 1 << sm);
    }

    #[test]
    fn set_writes_a_pin() {
        let mut pio = Pio::new();
        // SET PINS, 1 with SET_BASE 4 and SET_COUNT 1.
        pio.program[0] = 0xe001;
        pio.sm[0].pinctrl = 4 << pinctrl::SET_BASE_SHIFT | 1 << pinctrl::SET_COUNT_SHIFT;
        enable(&mut pio, 0);
        pio.tick(1, 0);
        assert_eq!(pio.pad_out & 0x10, 0x10, "SET PINS, 1 drives GP4");
    }

    #[test]
    fn out_shifts_the_transmit_fifo_onto_the_pins() {
        let mut pio = Pio::new();
        // OUT PINS, 4, then a spin.
        pio.program[0] = 0x6004;
        pio.program[1] = 0x0000;
        pio.sm[0].pinctrl = 4 << pinctrl::OUT_COUNT_SHIFT;
        pio.sm[0].shiftctrl |= shiftctrl::AUTOPULL;
        pio.tx[0].push_back(0b1010);
        enable(&mut pio, 0);
        pio.tick(1, 0);
        assert_eq!(pio.pad_out & 0xf, 0b1010, "OUT put the word on GP0..GP3");
    }

    #[test]
    fn a_blocking_pull_stalls_on_an_empty_fifo() {
        let mut pio = Pio::new();
        // PULL BLOCK
        pio.program[0] = 0x80a0;
        enable(&mut pio, 0);
        pio.tick(4, 0);
        assert_eq!(pio.sm[0].pc(), 0, "the pull waits for a word");
        assert!(pio.sm[0].stalled());
        pio.tx[0].push_back(7);
        pio.tick(1, 0);
        assert_eq!(pio.sm[0].pc(), 1);
        assert_eq!(pio.sm[0].osr, 7);
    }

    #[test]
    fn in_shifts_pins_and_push_reaches_the_fifo() {
        let mut pio = Pio::new();
        // IN PINS, 8 ; PUSH BLOCK
        pio.program[0] = 0x4008;
        pio.program[1] = 0x8020;
        pio.sm[0].shiftctrl = shiftctrl::IN_SHIFTDIR;
        enable(&mut pio, 0);
        pio.tick(1, 0b1010_0101);
        pio.tick(1, 0b1010_0101);
        let word = pio.rx[0].pop_front().expect("PUSH moved the ISR");
        assert_eq!(word, 0b1010_0101 << 24, "the pin byte enters from the left");
    }

    #[test]
    fn an_irq_instruction_sets_a_flag_and_waits_for_it() {
        let mut pio = Pio::new();
        // IRQ SET 0 ; IRQ WAIT 0
        pio.program[0] = 0xc000;
        pio.program[1] = 0xc020;
        enable(&mut pio, 0);
        pio.tick(2, 0);
        assert!(pio.irq_flag(0));
        assert_eq!(pio.sm[0].pc(), 2, "the wait found the flag set");
        // The flag is write-one-to-clear.
        pio.write(reg::IRQ, 1);
        assert!(!pio.irq_flag(0));
    }

    #[test]
    fn a_fifo_write_from_a_core_is_reported_once() {
        let mut pio = Pio::new();
        pio.write(reg::TXF0, 0x1234);
        assert_eq!(pio.take_pending_write(), Some((0, 0x1234)));
        assert_eq!(pio.take_pending_write(), None);
    }

    #[test]
    fn the_divider_sets_how_often_an_instruction_runs() {
        let mut pio = Pio::new();
        pio.program[0] = 0xa042; // MOV Y, Y
        pio.program[1] = 0xa042;
        pio.sm[0].clkdiv = 2 << clkdiv::INT_SHIFT;
        enable(&mut pio, 0);
        pio.tick(1, 0);
        assert_eq!(pio.sm[0].pc(), 0, "one clock has not reached the divider");
        pio.tick(1, 0);
        assert_eq!(pio.sm[0].pc(), 1);
        pio.tick(2, 0);
        assert_eq!(pio.sm[0].pc(), 2);
    }

    #[test]
    fn a_delay_field_holds_the_next_instruction_off() {
        let mut pio = Pio::new();
        pio.program[0] = 0xa042 | (3 << 8); // MOV Y, Y with delay 3
        pio.program[1] = 0xa042; // MOV Y, Y
        enable(&mut pio, 0);
        pio.tick(1, 0);
        assert_eq!(pio.sm[0].pc(), 1);
        pio.tick(3, 0);
        assert_eq!(pio.sm[0].pc(), 1, "three clocks of delay");
        pio.tick(1, 0);
        assert_eq!(pio.sm[0].pc(), 2);
    }

    #[test]
    fn a_jmp_pin_reads_the_pad_level() {
        let mut pio = Pio::new();
        // JMP PIN, 4, at both addresses so the fall-through runs it again.
        pio.program[0] = 0x0004 | (6 << 5);
        pio.program[1] = 0x0004 | (6 << 5);
        pio.sm[0].execctrl = 0x0001_f000 | (3 << execctrl::JMP_PIN_SHIFT);
        enable(&mut pio, 0);
        pio.tick(1, 0);
        assert_eq!(pio.sm[0].pc(), 1, "GP3 low falls through");
        pio.tick(1, 0b1000);
        assert_eq!(pio.sm[0].pc(), 4, "GP3 high jumps");
    }
}
