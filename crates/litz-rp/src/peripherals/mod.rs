//! The RP2040 peripheral blocks, one module each.
//!
//! Every block follows the same shape so the address decoder in
//! [`crate::bus`] stays a table rather than a chain of special cases:
//!
//! * it owns a [`RegisterFile`] for the offsets it does not give meaning to,
//!   which is what the chip really does (an unimplemented register reads back
//!   what was written, and a firmware that only pokes one bit keeps working);
//! * `read`/`write` take an offset from the block's base and return the effect
//!   the write had on pins or interrupts;
//! * `tick` advances the block by CPU cycles and is where a counter, a FIFO or
//!   an alarm moves.
//!
//! None of them know about the core: the machine asks each block whether it
//! has an interrupt pending and delivers it through the NVIC.

use crate::bus::Block;

pub mod adc;
pub mod clocks;
pub mod dma;
pub mod i2c;
pub mod io;
pub mod pio;
pub mod ppb;
pub mod pwm;
pub mod resets;
pub mod sio;
pub mod spi;
pub mod ticks;
pub mod timer;
pub mod uart;
pub mod usb;
pub mod watchdog;
pub mod xip;

pub use adc::Adc;
pub use clocks::Clocks;
pub use dma::Dma;
pub use i2c::I2c;
pub use io::{IoBank, PadsBank};
pub use pio::Pio;
pub use ppb::Ppb;
pub use pwm::Pwm;
pub use resets::Resets;
pub use sio::Sio;
pub use spi::Spi;
pub use ticks::Ticks;
pub use timer::Timer;
pub use uart::Uart;
pub use usb::Usb;
pub use watchdog::Watchdog;
pub use xip::{XipCtrl, XipSsi};

/// The 30-bit alias windows the RP2040 puts on top of every peripheral.
///
/// A write to `base + 0x2000` sets bits, `+0x3000` clears them and `+0x1000`
/// XORs them; reading any alias returns the register. Firmware uses them for
/// interrupt acknowledge and atomic bit writes, so handling them once here is
/// what keeps every block's own `write` simple.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    Normal,
    Set,
    Clear,
    Xor,
}

/// Split a peripheral address into its block offset and alias window.
pub fn decode_alias(offset: u32) -> (u32, WriteMode) {
    match (offset >> 12) & 3 {
        0 => (offset & 0x0fff, WriteMode::Normal),
        // The atomic XOR window sits at +0x1000, SET at +0x2000, CLR at
        // +0x3000, on every block on this chip.
        1 => (offset & 0x0fff, WriteMode::Xor),
        2 => (offset & 0x0fff, WriteMode::Set),
        _ => (offset & 0x0fff, WriteMode::Clear),
    }
}

/// Plain storage for the registers a block does not model.
#[derive(Clone, Debug, Default)]
pub struct RegisterFile {
    words: Vec<u32>,
}

impl RegisterFile {
    pub fn new(words: usize) -> Self {
        RegisterFile {
            words: vec![0; words],
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        self.words.get((offset / 4) as usize).copied().unwrap_or(0)
    }

    pub fn write(&mut self, offset: u32, value: u32) {
        if let Some(word) = self.words.get_mut((offset / 4) as usize) {
            *word = value;
        }
    }

    /// Apply an alias window to a register that was already written normally.
    pub fn write_mode(&mut self, offset: u32, value: u32, mode: WriteMode) {
        let current = self.read(offset);
        let value = match mode {
            WriteMode::Normal => value,
            WriteMode::Set => current | value,
            WriteMode::Clear => current & !value,
            WriteMode::Xor => current ^ value,
        };
        self.write(offset, value);
    }
}

/// The whole peripheral space, in one place.
///
/// The machine owns one of these and routes addresses to it; the blocks do not
/// see each other except through the two things that straddle them: a pin's
/// function select in [`IoBank`] and the clock gate in [`Clocks`].
#[derive(Debug)]
pub struct Peripherals {
    pub sio: Sio,
    pub io: IoBank,
    pub pads: PadsBank,
    pub resets: Resets,
    pub clocks: Clocks,
    pub timer: Timer,
    pub ticks: Ticks,
    pub watchdog: Watchdog,
    pub uart: [Uart; 2],
    pub spi: [Spi; 2],
    pub i2c: [I2c; 2],
    pub pwm: Pwm,
    pub pio: [Pio; 2],
    pub adc: Adc,
    pub dma: Dma,
    pub xip_ctrl: XipCtrl,
    pub xip_ssi: XipSsi,
    pub usb: Usb,
    /// The core-0 processor block: NVIC, SysTick, VTOR.
    pub ppb: Ppb,
    /// Core 1's own copy of the same block. The RP2040 gives each core a
    /// private NVIC and SysTick, and the bus routes `PPB_BASE` by the core
    /// that is executing.
    pub ppb1: Ppb,
    /// SYSINFO, which nothing writes.
    pub sysinfo: RegisterFile,
    /// SYSCFG: the processor sync bypasses, which are storage in this model.
    pub syscfg: RegisterFile,
    /// PSM: power state machine request registers. `DONE` is synthesised in
    /// `block_read`.
    pub psm: RegisterFile,
    /// XOSC: the crystal oscillator configuration. `STATUS.STABLE` is always
    /// set.
    pub xosc: RegisterFile,
    /// PLL_SYS and PLL_USB: the feedback dividers and power registers.
    /// `CS.LOCK` is always set.
    pub pll_sys: RegisterFile,
    pub pll_usb: RegisterFile,
    /// BUSCTRL: bus priority registers.
    pub busctrl: RegisterFile,
    /// RTC: the real-time clock's setup and interrupt registers.
    pub rtc: RegisterFile,
    /// ROSC: the ring oscillator's trim registers. `STATUS` reports stable.
    pub rosc: RegisterFile,
    /// VREG_AND_CHIP_RESET. Storage is enough: firmware only writes the
    /// regulator settings and reads them back.
    pub vreg: RegisterFile,
    /// TBMAN. Its platform register tells the SDK whether it is running on an
    /// FPGA; a zero here is silicon, which is what the model stands for.
    pub tbman: RegisterFile,
    /// Registers of blocks that are present in the map but not modelled yet.
    pub unmapped: RegisterFile,
}

impl Default for Peripherals {
    fn default() -> Self {
        Self::new()
    }
}

impl Peripherals {
    pub fn new() -> Self {
        Peripherals {
            sio: Sio::new(),
            io: IoBank::new(),
            pads: PadsBank::new(),
            resets: Resets::new(),
            clocks: Clocks::new(),
            timer: Timer::new(),
            ticks: Ticks::new(),
            watchdog: Watchdog::new(),
            // The two UARTs and buses are identical instances; a firmware
            // picks one by address.
            uart: [Uart::new(), Uart::new()],
            spi: [Spi::new(), Spi::new()],
            i2c: [I2c::new(), I2c::new()],
            pwm: Pwm::new(),
            pio: [Pio::new(), Pio::new()],
            adc: Adc::new(),
            dma: Dma::new(),
            xip_ctrl: XipCtrl::new(),
            xip_ssi: XipSsi::new(),
            usb: Usb::new(),
            ppb: Ppb::new(),
            ppb1: Ppb::new(),
            sysinfo: RegisterFile::new(0x100 / 4),
            syscfg: RegisterFile::new(0x100 / 4),
            psm: RegisterFile::new(0x100 / 4),
            xosc: RegisterFile::new(0x100 / 4),
            pll_sys: RegisterFile::new(0x100 / 4),
            pll_usb: RegisterFile::new(0x100 / 4),
            busctrl: RegisterFile::new(0x100 / 4),
            rtc: RegisterFile::new(0x100 / 4),
            rosc: RegisterFile::new(0x100 / 4),
            vreg: RegisterFile::new(0x100 / 4),
            tbman: RegisterFile::new(0x100 / 4),
            unmapped: RegisterFile::new(0x1000 / 4),
        }
    }

    /// Whether DMA trigger `sel` has an event waiting.
    ///
    /// The sources whose model keeps a queue answer from it: a PIO FIFO, the
    /// UART receiver. A transmit-side trigger reads ready while the model's
    /// byte queue has room, which for these blocks is always. The sources
    /// whose model has no queue of its own — timer alarms, PWM wraps, the bus
    /// controllers and the ADC — report ready, which makes a channel paced by
    /// them transfer at the engine's rate.
    pub fn dreq_ready(&self, sel: u32) -> bool {
        match sel {
            13 => !self.uart[0].rx.is_empty(),
            15 => !self.uart[1].rx.is_empty(),
            // Eight triggers per PIO block: transmit and receive for each of
            // the four state machines, PIO0 first.
            16..=31 => {
                let block = ((sel - 16) / 8) as usize;
                let rest = (sel - 16) % 8;
                let sm = (rest / 2) as usize;
                if rest % 2 == 0 {
                    self.pio[block].tx_ready(sm)
                } else {
                    self.pio[block].rx_ready(sm)
                }
            }
            _ => true,
        }
    }

    /// Read a word from `block` at `offset` from its base. The alias windows
    /// are already folded into the offset, so a read of `base + 0x2004` looks
    /// at `+0x4` like every other read.
    pub fn block_read(&mut self, block: Block, offset: u32) -> u32 {
        let (offset, _) = decode_alias(offset);
        match block {
            // A real RP2040 identifies itself as an ASIC, part 2, revision 2
            // (B2) at CHIP_ID; `rp2040_chip_version()` reads the revision from
            // here, and B2 leaves the RP2040-E5 USB workaround (which needs
            // GPIO15) out of the picture. PLATFORM reports ASIC at bit 1.
            Block::Sysinfo => match offset {
                0x00 => 0x2000_2927,
                0x04 => 0x2,
                _ => self.sysinfo.read(offset),
            },
            Block::Syscfg => self.syscfg.read(offset),
            Block::Clocks => self.clocks.read(offset),
            Block::Resets => self.resets.read(offset),
            // PSM.DONE (0x0c) says every power domain is up. Returning all
            // ones is what a firmware's `while (!(psm->done & mask))` poll
            // expects; the request registers are storage.
            Block::Psm => {
                if offset == 0x0c {
                    0xffff_ffff
                } else {
                    self.psm.read(offset)
                }
            }
            Block::IoBank0 => self.io.read(offset),
            Block::IoQspi => 0,
            Block::PadsBank0 => self.pads.read(offset),
            Block::PadsQspi => 0,
            // XOSC.STATUS (0x04) reads STABLE (bit 31) as soon as the block
            // is asked; the crystal is instantaneous in this model.
            Block::Xosc => {
                let value = self.xosc.read(offset);
                if offset == 0x04 {
                    value | (1 << 31)
                } else {
                    value
                }
            }
            // PLL CS (0x00) reads LOCK (bit 31).
            Block::PllSys => {
                let value = self.pll_sys.read(offset);
                if offset == 0x00 {
                    value | (1 << 31)
                } else {
                    value
                }
            }
            Block::PllUsb => {
                let value = self.pll_usb.read(offset);
                if offset == 0x00 {
                    value | (1 << 31)
                } else {
                    value
                }
            }
            Block::BusCtrl => self.busctrl.read(offset),
            Block::Uart0 => self.uart[0].read(offset),
            Block::Uart1 => self.uart[1].read(offset),
            Block::Spi0 => self.spi[0].read(offset),
            Block::Spi1 => self.spi[1].read(offset),
            Block::I2c0 => self.i2c[0].read(offset),
            Block::I2c1 => self.i2c[1].read(offset),
            Block::Adc => self.adc.read(offset),
            Block::Pwm => self.pwm.read(offset),
            Block::Timer => self.timer.read(offset),
            Block::Watchdog => self.watchdog.read(offset),
            // RTC.CTRL (0x0c) bit 1 is RTC_ACTIVE, the handshake firmware
            // spins on after enabling the RTC and on again after disabling
            // it. The model has no asynchronous enable, so ACTIVE follows
            // ENABLE (bit 0) one-for-one.
            Block::Rtc => {
                let value = self.rtc.read(offset);
                if offset == 0x0c {
                    (value & !2) | ((value & 1) << 1)
                } else {
                    value
                }
            }
            // ROSC.STATUS (0x18) reports ENABLED (bit 12) and STABLE
            // (bit 31); firmware waits on the latter.
            Block::Rosc => {
                let value = self.rosc.read(offset);
                if offset == 0x18 {
                    value | (1 << 31) | (1 << 12)
                } else {
                    value
                }
            }
            Block::VregAndChipReset => self.vreg.read(offset),
            Block::Tbman => self.tbman.read(offset),
            Block::Ticks => self.ticks.read(offset),
            Block::Dma => self.dma.read(offset),
            Block::UsbCtrl => self.usb.read(offset),
            Block::Pio0 => self.pio[0].read(offset),
            Block::Pio1 => self.pio[1].read(offset),
            Block::XipCtrl => self.xip_ctrl.read(offset),
            Block::XipSsi => self.xip_ssi.read(offset),
        }
    }

    /// Write a word to `block`, honouring the `+0x1000` XOR, `+0x2000` SET and
    /// `+0x3000` CLR alias windows.
    pub fn block_write(&mut self, block: Block, offset: u32, value: u32) {
        let (offset, mode) = decode_alias(offset);
        // The USB controller owns its bit-level semantics: status registers
        // are write-one-to-clear, so folding the alias into the value here
        // would apply the clear twice.
        if matches!(block, Block::UsbCtrl) {
            self.usb.write_with(offset, mode, value);
            return;
        }
        let value = match mode {
            WriteMode::Normal => value,
            WriteMode::Set => self.block_read(block, offset) | value,
            WriteMode::Clear => self.block_read(block, offset) & !value,
            WriteMode::Xor => self.block_read(block, offset) ^ value,
        };
        match block {
            Block::Syscfg => self.syscfg.write(offset, value),
            Block::Clocks => self.clocks.write(offset, value),
            Block::Resets => self.resets.write(offset, value),
            // The request registers around `DONE` are writable; `DONE` itself
            // is synthesised in `block_read`.
            Block::Psm => {
                if offset != 0x0c {
                    self.psm.write(offset, value);
                }
            }
            Block::IoBank0 => self.io.write(offset, value),
            Block::PadsBank0 => self.pads.write(offset, value),
            Block::Xosc => self.xosc.write(offset, value),
            Block::PllSys => self.pll_sys.write(offset, value),
            Block::PllUsb => self.pll_usb.write(offset, value),
            Block::BusCtrl => self.busctrl.write(offset, value),
            Block::Uart0 => self.uart[0].write(offset, value),
            Block::Uart1 => self.uart[1].write(offset, value),
            Block::Spi0 => self.spi[0].write(offset, value),
            Block::Spi1 => self.spi[1].write(offset, value),
            Block::I2c0 => self.i2c[0].write(offset, value),
            Block::I2c1 => self.i2c[1].write(offset, value),
            Block::Adc => self.adc.write(offset, value),
            Block::Pwm => self.pwm.write(offset, value),
            Block::Timer => self.timer.write(offset, value),
            Block::Watchdog => self.watchdog.write(offset, value),
            Block::Rtc => self.rtc.write(offset, value),
            Block::Rosc => self.rosc.write(offset, value),
            Block::VregAndChipReset => self.vreg.write(offset, value),
            Block::Tbman => self.tbman.write(offset, value),
            Block::Ticks => self.ticks.write(offset, value),
            Block::Dma => self.dma.write(offset, value),
            Block::XipCtrl => self.xip_ctrl.write(offset, value),
            Block::XipSsi => self.xip_ssi.write(offset, value),
            Block::UsbCtrl => {}
            Block::Pio0 => self.pio[0].write(offset, value),
            Block::Pio1 => self.pio[1].write(offset, value),
            // Read-only blocks: writes are dropped, which is what the chip
            // does to a write into a status register it does not implement.
            Block::Sysinfo | Block::IoQspi | Block::PadsQspi => {}
        }
    }
}
