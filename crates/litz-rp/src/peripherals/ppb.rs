//! The private peripheral bus: NVIC, SysTick and the system control block.
//!
//! Every core has its own copy on real silicon. The model gives core 0 the
//! only one it drives; core 1, when a firmware launches it, sees the same
//! registers through the same memory, which is enough for the `multicore`
//! handshake and the SIO FIFOs it actually uses.

use crate::peripherals::RegisterFile;

/// `SysTick` register offsets from `0xE000E000`.
pub mod systick {
    pub const CTRL: u32 = 0x010;
    pub const LOAD: u32 = 0x014;
    pub const VAL: u32 = 0x018;
    pub const CALIB: u32 = 0x01C;
}

/// System control block offsets from `0xE000E000`.
pub mod scb {
    pub const CPUID: u32 = 0xD00;
    pub const ICSR: u32 = 0xD04;
    pub const VTOR: u32 = 0xD08;
    pub const AIRCR: u32 = 0xD0C;
    pub const SCR: u32 = 0xD10;
    pub const CCR: u32 = 0xD14;
    pub const SHPR2: u32 = 0xD1C;
    pub const SHPR3: u32 = 0xD20;
}

/// NVIC offsets from `0xE000E000`.
pub mod nvic {
    pub const ISER: u32 = 0x100;
    pub const ICER: u32 = 0x180;
    pub const ISPR: u32 = 0x200;
    pub const ICPR: u32 = 0x280;
    pub const IPR: u32 = 0x400;
}

/// The exception number `SysTick` is delivered as.
pub const SYSTICK_EXCEPTION: u16 = 15;

#[derive(Debug)]
pub struct Ppb {
    pub vtor: u32,
    pub systick_ctrl: u32,
    pub systick_load: u32,
    pub systick_val: u32,
    /// `COUNTFLAG` is read-clear, like the hardware.
    pub systick_counted: bool,
    pub enabled: u32,
    pub pending: u32,
    /// Two priority bits per interrupt, in the low half of each byte.
    pub priority: [u8; 32],
    pub shpr2: u32,
    pub shpr3: u32,
    pub scr: u32,
    pub ccr: u32,
    /// `AIRCR.SYSRESETREQ`: the machine restarts the chip between slices.
    pub sysreset: bool,
    pub regs: RegisterFile,
}

impl Default for Ppb {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppb {
    pub fn new() -> Self {
        Ppb {
            // The bootrom leaves VTOR at the ROM; a flash firmware sets it to
            // 0x1000_0000 before it enables any interrupt.
            vtor: 0,
            systick_ctrl: 0,
            systick_load: 0,
            systick_val: 0,
            systick_counted: false,
            enabled: 0,
            pending: 0,
            priority: [0; 32],
            shpr2: 0,
            shpr3: 0,
            scr: 0,
            ccr: 0x200,
            sysreset: false,
            regs: RegisterFile::new(0x1000 / 4),
        }
    }

    pub fn read(&mut self, offset: u32) -> u32 {
        match offset {
            systick::CTRL => {
                (self.systick_ctrl & 0x7) | if self.systick_counted { 1 << 16 } else { 0 }
            }
            systick::LOAD => self.systick_load & 0x00ff_ffff,
            systick::VAL => self.systick_val & 0x00ff_ffff,
            systick::CALIB => 0,
            scb::CPUID => 0x410c_c601,
            scb::ICSR => {
                // The clear bits are write-only; the only state visible here is
                // the SysTick pending latch, which reads back as PENDSTSET.
                if self.systick_ctrl & (1 << 25) != 0 {
                    1 << 26
                } else {
                    0
                }
            }
            scb::VTOR => self.vtor,
            scb::AIRCR => 0xfa05_0000,
            scb::SCR => self.scr,
            scb::CCR => self.ccr,
            scb::SHPR2 => self.shpr2,
            scb::SHPR3 => self.shpr3,
            nvic::ISER => self.enabled,
            nvic::ICER => self.enabled,
            nvic::ISPR => self.pending,
            nvic::ICPR => self.pending,
            off if (nvic::IPR..nvic::IPR + 0x80).contains(&off) => {
                let index = ((off - nvic::IPR) / 4) as usize;
                let mut word = 0;
                for i in 0..4 {
                    if let Some(prio) = self.priority.get(index * 4 + i) {
                        word |= u32::from(*prio & 3) << (8 * i + 6);
                    }
                }
                word
            }
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, offset: u32, value: u32) {
        match offset {
            systick::CTRL => self.systick_ctrl = value & 0x7,
            systick::LOAD => self.systick_load = value & 0x00ff_ffff,
            systick::VAL => {
                self.systick_val = value & 0x00ff_ffff;
                self.systick_counted = false;
            }
            systick::CALIB => {}
            scb::ICSR => {
                if value & (1 << 25) != 0 {
                    self.pending &= !1;
                }
                if value & (1 << 26) != 0 {
                    self.systick_ctrl |= 1 << 25;
                }
                if value & (1 << 27) != 0 {
                    self.pending |= 1 << 15;
                }
            }
            scb::VTOR => self.vtor = value & 0xffff_ff80,
            scb::AIRCR => {
                // VECTKEY guards every write: 0x05FA in the top half.
                if value >> 16 == 0x05fa && value & (1 << 2) != 0 {
                    self.sysreset = true;
                }
            }
            scb::SCR => self.scr = value,
            scb::CCR => self.ccr = value,
            scb::SHPR2 => self.shpr2 = value,
            scb::SHPR3 => self.shpr3 = value,
            nvic::ISER => self.enabled |= value,
            nvic::ICER => self.enabled &= !value,
            nvic::ISPR => self.pending |= value,
            nvic::ICPR => self.pending &= !value,
            off if (nvic::IPR..nvic::IPR + 0x80).contains(&off) => {
                let index = ((off - nvic::IPR) / 4) as usize;
                for i in 0..4 {
                    if let Some(prio) = self.priority.get_mut(index * 4 + i) {
                        *prio = ((value >> (8 * i + 6)) & 3) as u8;
                    }
                }
            }
            off => self.regs.write(off, value),
        }
    }

    /// A peripheral drives its interrupt line; the NVIC latches the request
    /// until the handler acknowledges it. A line that goes low again does not
    /// drop a request that was already pending, which is what the hardware
    /// does for every level-sensitive source on this chip.
    pub fn set_line(&mut self, irq: u16, level: bool) {
        if level && irq < 32 {
            self.pending |= 1 << irq;
        }
    }

    /// Whether the `SysTick` latch is raised, without consuming it.
    pub fn systick_raised(&self) -> bool {
        self.systick_ctrl & (1 << 25) != 0
    }

    /// Whether a `SysTick` exception is still to come: enabled with `TICKINT`.
    /// A `WFI` must not be treated as idle while this holds, because the
    /// counter will wake the core on its own.
    pub fn systick_armed(&self) -> bool {
        self.systick_ctrl & 0b11 == 0b11
    }

    /// The pending `SysTick` request, separate from the NVIC lines.
    pub fn systick_pending(&mut self) -> bool {
        if self.systick_ctrl & (1 << 25) != 0 {
            self.systick_ctrl &= !(1 << 25);
            return true;
        }
        false
    }

    /// The highest-priority enabled pending line, lowest number on a tie.
    pub fn pending_irq(&self) -> Option<u16> {
        let ready = self.pending & self.enabled;
        if ready == 0 {
            return None;
        }
        let mut best: Option<(u8, u16)> = None;
        for irq in 0..32u16 {
            if ready & (1 << irq) == 0 {
                continue;
            }
            let prio = self.priority[irq as usize] & 3;
            if best.is_none_or(|(p, _)| prio < p) {
                best = Some((prio, irq));
            }
        }
        best.map(|(_, irq)| irq)
    }

    pub fn ack(&mut self, irq: u16) {
        if irq < 32 {
            self.pending &= !(1 << irq);
        }
    }

    /// Advance SysTick by `cycles`. Returns whether the counter wrapped, which
    /// is when the machine delivers the exception if it is enabled.
    pub fn tick(&mut self, cycles: u64) -> bool {
        if self.systick_ctrl & 1 == 0 {
            return false;
        }
        let reload = self.systick_load & 0x00ff_ffff;
        let mut remaining = cycles;
        let mut wrapped = false;
        while remaining > 0 {
            // `VAL` counts down to zero and the reload plus COUNTFLAG happen on
            // the clock after that, so a zero counter is one cycle from the
            // event rather than a whole period away. That distinction matters:
            // a `WFI` loop advances the machine one cycle at a time, and a
            // counter that treats zero as a fresh period never wraps.
            let current = u64::from(self.systick_val & 0x00ff_ffff);
            let left = current.max(1);
            if remaining < left {
                self.systick_val = (current - remaining) as u32;
                remaining = 0;
            } else {
                remaining -= left;
                self.systick_val = reload;
                self.systick_counted = true;
                wrapped = true;
                if self.systick_ctrl & (1 << 1) != 0 {
                    self.systick_ctrl |= 1 << 25;
                }
                if reload == 0 {
                    // A zero reload wraps every clock; stop rather than spin
                    // through the rest of the slice one period at a time.
                    break;
                }
            }
        }
        wrapped
    }
}
