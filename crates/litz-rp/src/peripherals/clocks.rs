//! The clock tree: the ring oscillator, XOSC, two PLLs and the
//! gating/divider registers each peripheral clock goes through.
//!
//! Every one of the ten clocks has the same three-register triple at a 0x0c
//! stride: `CTRL` (source select, auxiliary source and enable), `DIV` (24.8
//! fixed point) and `SELECTED` (a read-only handshake). A firmware changes a
//! clock by writing `CTRL`, spinning until `SELECTED` reports the source
//! running, and then programming `DIV`; the handshake is instantaneous here
//! because all the modelled sources are already stable.
//!
//! The one number other blocks depend on is [`Clocks::clk_peri_hz`], the UART,
//! SPI and I2C clock. It divides the 125 MHz system clock by `CLK_PERI_DIV`.
//! [`Clocks::clk_sys_hz`] is fixed at 125 MHz, the Pico's stock system clock.

use super::RegisterFile;

/// Register offsets from the CLOCKS base. Each clock occupies three words at
/// a 0x0c stride (`CTRL`, `DIV`, `SELECTED`).
pub mod reg {
    pub const GPOUT0_CTRL: u32 = 0x00;
    pub const GPOUT1_CTRL: u32 = 0x0c;
    pub const GPOUT2_CTRL: u32 = 0x18;
    pub const GPOUT3_CTRL: u32 = 0x24;
    pub const REF_CTRL: u32 = 0x30;
    pub const SYS_CTRL: u32 = 0x3c;
    pub const PERI_CTRL: u32 = 0x48;
    pub const USB_CTRL: u32 = 0x54;
    pub const ADC_CTRL: u32 = 0x60;
    pub const RTC_CTRL: u32 = 0x6c;

    pub fn ctrl(index: usize) -> u32 {
        0x0c * index as u32
    }
    pub fn div(index: usize) -> u32 {
        ctrl(index) + 4
    }
    pub fn selected(index: usize) -> u32 {
        ctrl(index) + 8
    }
}

/// Clock indices, in register order.
pub mod index {
    pub const GPOUT0: usize = 0;
    pub const REF: usize = 4;
    pub const SYS: usize = 5;
    pub const PERI: usize = 6;
}

/// `CTRL` bits.
pub mod ctrl {
    /// Source select (`CTRL` 0..1).
    pub const SRC_MASK: u32 = 0x3;
    /// Auxiliary source select (`CTRL` 5..8).
    pub const AUXSRC_SHIFT: u32 = 5;
    pub const AUXSRC_MASK: u32 = 0xf << AUXSRC_SHIFT;
    /// Enable (`CTRL` 11).
    pub const ENABLE: u32 = 1 << 11;
}

/// The 125 MHz the model runs the system clock at.
pub const SYS_HZ: u64 = 125_000_000;

#[derive(Clone, Copy, Debug)]
struct Clock {
    ctrl: u32,
    div: u32,
}

impl Default for Clock {
    fn default() -> Self {
        // A reset RP2040 runs every clock from the ring oscillator. `DIV` is
        // 1.0 (integer 1 in bits 15:8), and `ENABLE` is set so a firmware that
        // only touches the peripheral it needs still gets a clock.
        Clock {
            ctrl: ctrl::ENABLE,
            div: 1 << 8,
        }
    }
}

#[derive(Debug)]
pub struct Clocks {
    clocks: [Clock; 10],
    regs: RegisterFile,
}

impl Default for Clocks {
    fn default() -> Self {
        Self::new()
    }
}

impl Clocks {
    pub fn new() -> Self {
        Clocks {
            clocks: [Clock::default(); 10],
            regs: RegisterFile::new(0x100 / 4),
        }
    }

    /// Split a register offset into `(clock, field)` where field is 0 for
    /// `CTRL`, 1 for `DIV` and 2 for `SELECTED`.
    fn split(off: u32) -> Option<(usize, u32)> {
        if off >= 0x78 || off & 3 != 0 {
            return None;
        }
        let index = (off / 0x0c) as usize;
        let field = (off % 0x0c) / 4;
        (index < 10 && field < 3).then_some((index, field))
    }

    /// The value `SELECTED` reports. Only `clk_ref` and `clk_sys` have a
    /// glitchless mux, and their `SELECTED` is one-hot over the source index;
    /// every source is stable here, so the switch completes on the write. The
    /// other eight clocks have no such mux and always report source 0, which
    /// is the bit the SDK polls after programming their auxiliary select.
    fn selected(&self, index: usize) -> u32 {
        match index {
            index::REF => 1 << (self.clocks[index].ctrl & ctrl::SRC_MASK),
            index::SYS => 1 << (self.clocks[index].ctrl & 1),
            _ => 1,
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if let Some((index, field)) = Self::split(off) {
            return match field {
                0 => self.clocks[index].ctrl,
                1 => self.clocks[index].div,
                _ => self.selected(index),
            };
        }
        self.regs.read(off)
    }

    pub fn write(&mut self, off: u32, value: u32) {
        if let Some((index, field)) = Self::split(off) {
            match field {
                0 => self.clocks[index].ctrl = value,
                1 => self.clocks[index].div = value,
                // SELECTED is read-only.
                _ => {}
            }
            return;
        }
        self.regs.write(off, value);
    }

    /// 125 MHz, the clock the core and the timer run from.
    pub fn clk_sys_hz(&self) -> u32 {
        SYS_HZ as u32
    }

    /// The peripheral clock, `CLK_PERI_DIV` into 125 MHz. A zero integer part
    /// means the clock is stopped, as on the hardware.
    pub fn clk_peri_hz(&self) -> u32 {
        let clock = &self.clocks[index::PERI];
        if clock.ctrl & ctrl::ENABLE == 0 {
            return 0;
        }
        let integer = u64::from(clock.div >> 8);
        let fraction = u64::from(clock.div & 0xff);
        if integer == 0 {
            return 0;
        }
        let denominator = integer * 256 + fraction;
        ((SYS_HZ * 256 + denominator / 2) / denominator) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_clock_tree_runs_at_125_mhz() {
        let clocks = Clocks::new();
        assert_eq!(clocks.clk_sys_hz(), 125_000_000);
        assert_eq!(clocks.clk_peri_hz(), 125_000_000);
    }

    #[test]
    fn system_divider_reads_back() {
        let mut clocks = Clocks::new();
        clocks.write(reg::SYS_CTRL, ctrl::ENABLE | 1);
        assert_eq!(clocks.read(reg::SYS_CTRL), ctrl::ENABLE | 1);
        clocks.write(reg::SYS_CTRL + 4, 2 << 8);
        assert_eq!(clocks.read(reg::SYS_CTRL + 4), 2 << 8);
        // Selected is one-hot over the source index: SRC=1 selects the
        // auxiliary mux, so bit 1 is the one that reports it.
        assert_eq!(clocks.read(reg::SYS_CTRL + 8), 2);
        // With SRC back at the reference clock, bit 0 is the one set.
        clocks.write(reg::SYS_CTRL, ctrl::ENABLE);
        assert_eq!(clocks.read(reg::SYS_CTRL + 8), 1);
        // clk_peri has no glitchless mux and always reports 1.
        assert_eq!(clocks.read(reg::PERI_CTRL + 8), 1);
    }

    #[test]
    fn peripheral_divider() {
        let mut clocks = Clocks::new();
        clocks.write(reg::PERI_CTRL, ctrl::ENABLE);
        clocks.write(reg::PERI_CTRL + 4, 2 << 8);
        assert_eq!(clocks.clk_peri_hz(), 62_500_000);
        clocks.write(reg::PERI_CTRL, 0);
        assert_eq!(clocks.clk_peri_hz(), 0);
    }

    #[test]
    fn fractional_divider() {
        let mut clocks = Clocks::new();
        clocks.write(reg::PERI_CTRL, ctrl::ENABLE);
        // 1.5 = integer 1, fraction 0x80.
        clocks.write(reg::PERI_CTRL + 4, (1 << 8) | 0x80);
        assert_eq!(clocks.clk_peri_hz(), 83_333_333);
    }
}
