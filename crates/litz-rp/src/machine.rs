//! One RP2040: the core, the memory and the peripherals, advanced together.
//!
//! The machine owns the clock. Every instruction returns the cycles it
//! consumed, the peripherals are ticked by that amount, and a pending
//! interrupt is delivered before the next instruction — never in the middle of
//! one — so a run divides into whole cycles and the same budget always lands on
//! the same instruction boundary.

use std::collections::{BTreeMap, BTreeSet};

use crate::bus::{FLASH_BASE, Memory, SRAM_BASE};
use crate::cpu::{Bus, Core, Fault};
use crate::peripherals::Peripherals;
use crate::peripherals::io::gpio_ctrl;
use crate::peripherals::ppb::{self, Ppb};

/// Why a run came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The cycle budget ran out.
    Budget,
    /// Nothing to do until the host changes an input.
    Idle,
    /// The core halted (`BKPT` semihosting or a `WFI` with no timer).
    Halted,
    /// The core faulted; the detail is in [`Machine::fault`].
    Trap,
    /// `AIRCR.SYSRESETREQ`: the chip asks to restart.
    Reset,
    /// The pc reached an armed breakpoint, which has not run yet.
    Breakpoint(u32),
}

/// Interrupt numbers, in the order the RP2040 datasheet lists them.
pub mod irq {
    pub const TIMER_IRQ_0: u16 = 0;
    pub const TIMER_IRQ_1: u16 = 1;
    pub const TIMER_IRQ_2: u16 = 2;
    pub const TIMER_IRQ_3: u16 = 3;
    pub const PWM_IRQ_WRAP: u16 = 4;
    pub const USBCTRL_IRQ: u16 = 5;
    pub const XIP_IRQ: u16 = 6;
    pub const PIO0_IRQ_0: u16 = 7;
    pub const PIO0_IRQ_1: u16 = 8;
    pub const PIO1_IRQ_0: u16 = 9;
    pub const PIO1_IRQ_1: u16 = 10;
    pub const DMA_IRQ_0: u16 = 11;
    pub const DMA_IRQ_1: u16 = 12;
    pub const IO_IRQ_BANK0: u16 = 13;
    pub const IO_IRQ_QSPI: u16 = 14;
    pub const SIO_IRQ_PROC0: u16 = 15;
    pub const SIO_IRQ_PROC1: u16 = 16;
    pub const CLOCKS_IRQ: u16 = 17;
    pub const SPI0_IRQ: u16 = 18;
    pub const SPI1_IRQ: u16 = 19;
    pub const UART0_IRQ: u16 = 20;
    pub const UART1_IRQ: u16 = 21;
    pub const ADC_IRQ_FIFO: u16 = 22;
    pub const I2C0_IRQ: u16 = 23;
    pub const I2C1_IRQ: u16 = 24;
    pub const RTC_IRQ: u16 = 25;
}

/// What the bootrom does before the first instruction of stage 2 runs.
///
/// The real ROM reads the first flash sector, checks its CRC and copies the
/// 256-byte stage 2 into SRAM at `0x2000_0000`, then jumps to the vector table
/// that copy starts with. The model performs the same handoff, so a stock
/// `boot2` image and a stock application run unchanged.
pub const STAGE2_LEN: usize = 256;

/// Where the ROM calls stage 2 back to. Stage 2 ends with `pop {pc}` or
/// `bx lr`; the model watches for this address instead of executing ROM code.
pub const ROM_HANDOFF: u32 = 0x0000_0008;

/// The stack the ROM leaves for stage 2: the top of the first 256 KiB of SRAM.
pub const BOOT_STACK: u32 = 0x2004_0000;

/// IO_BANK0's function select for the UART: a pin with this value carries a
/// UART signal. Which signal depends on the pin, per the datasheet's table.
pub const FUNCSEL_UART: u8 = 2;
/// IO_BANK0's function select for the PWM block.
pub const FUNCSEL_PWM: u8 = 4;
/// IO_BANK0's function select for SIO, the core's own GPIO block.
pub const FUNCSEL_SIO: u8 = 5;
/// IO_BANK0's function selects for PIO0 and PIO1.
pub const FUNCSEL_PIO0: u8 = 6;
pub const FUNCSEL_PIO1: u8 = 7;

/// The GPIOs each UART port transmits on and receives on, in the order the
/// datasheet's function table lists them.
const UART_TX_PINS: [&[u8]; 2] = [&[0, 12, 16, 28], &[4, 8, 20, 24]];
const UART_RX_PINS: [&[u8]; 2] = [&[1, 13, 17, 29], &[5, 9, 21, 25]];

/// The PWM channel a GPIO drives: slice `(gpio >> 1) & 7`, channel A for an
/// even pin and B for an odd one.
fn pwm_channel(gpio: u8) -> usize {
    (usize::from(gpio >> 1) & 7) * 2 + usize::from(gpio & 1)
}

pub struct Machine {
    pub cpu: Core,
    /// Core 1. It exists from reset but runs nothing until core 0 releases it
    /// with the launch message the bootrom waits for.
    pub cpu1: Core,
    /// Whether core 1 has been released.
    core1_started: bool,
    pub mem: Memory,
    /// CPU cycles since reset.
    pub cycles: u64,
    fault: Option<String>,
    console: Vec<u8>,
    /// Board pin levels the host is holding, by GPIO number.
    inputs: BTreeMap<u8, bool>,
    /// The last level published per pin, so only edges are reported.
    published: [Option<bool>; 32],
    outputs: Vec<(u8, bool)>,
    /// Pins whose drive the chip released since the last collection.
    released: Vec<u8>,
    /// ADC voltages in microvolts, by channel.
    analog: [u32; 5],
    /// Bytes each UART port has transmitted since the host last collected
    /// them. Port 0 is the console too; the console keeps its own copy.
    uart_tx: [Vec<u8>; 2],
    /// True while stage 2 runs, before the ROM hands off to the application.
    rom_stage: bool,
    /// Program counters a run stops in front of. The check lives here rather
    /// than in the adapter so a breakpoint lands on an instruction boundary:
    /// a run is one budget, not a series of slices, until something stops it.
    breakpoints: BTreeSet<u32>,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Machine {
            cpu: Core::new(),
            cpu1: Core::new(),
            core1_started: false,
            mem: Memory::new(),
            cycles: 0,
            fault: None,
            console: Vec::new(),
            inputs: BTreeMap::new(),
            published: [None; 32],
            outputs: Vec::new(),
            released: Vec::new(),
            analog: [0; 5],
            uart_tx: [Vec::new(), Vec::new()],
            rom_stage: false,
            breakpoints: BTreeSet::new(),
        }
    }

    /// Replace the armed breakpoint set; an empty slice clears them.
    pub fn set_breakpoints(&mut self, pcs: impl IntoIterator<Item = u32>) {
        self.breakpoints = pcs.into_iter().collect();
    }

    /// Arm one more breakpoint without disturbing the others.
    pub fn add_breakpoint(&mut self, pc: u32) {
        self.breakpoints.insert(pc);
    }

    pub fn periph(&self) -> &Peripherals {
        &self.mem.periph
    }

    pub fn periph_mut(&mut self) -> &mut Peripherals {
        &mut self.mem.periph
    }

    pub fn ppb(&self) -> &Ppb {
        &self.mem.periph.ppb
    }

    pub fn ppb_mut(&mut self) -> &mut Ppb {
        &mut self.mem.periph.ppb
    }

    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    pub fn pc(&self) -> u32 {
        self.cpu.pc()
    }

    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// Load a raw image into flash at `offset` (0 for a stock `boot2` image).
    pub fn load_flash(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        self.mem.load_flash(offset, bytes)
    }

    /// Copy stage 2 out of flash and start it, the way the ROM does.
    ///
    /// A stock `boot2` is code, not a vector table: the ROM copies the first
    /// 256 bytes to SRAM, sets the stack to the top of the first 256 KiB and
    /// calls the copy with LR pointing back into the ROM. When stage 2 returns
    /// there the ROM reads the application's vector table from the flash
    /// immediately after it and jumps.
    pub fn boot(&mut self) {
        self.reset();
        let stage2 = self.mem.flash[..STAGE2_LEN].to_vec();
        self.mem.sram[..STAGE2_LEN].copy_from_slice(&stage2);
        self.cpu.msp = BOOT_STACK;
        self.cpu.psp = 0;
        self.cpu.r[13] = BOOT_STACK;
        self.cpu.r[14] = ROM_HANDOFF | 1;
        self.cpu.r[15] = SRAM_BASE;
        self.cpu.xpsr = crate::cpu::psr::T;
        self.cpu.exception = 0;
        self.cpu.waiting = false;
        self.rom_stage = true;
    }

    /// The ROM's side of the handoff: take the application's stack and entry
    /// point from the vector table at the end of stage 2.
    fn enter_application(&mut self) -> Result<(), Fault> {
        let vector = FLASH_BASE + STAGE2_LEN as u32;
        let sp = self.mem.read32(vector)?;
        let entry = self.mem.read32(vector + 4)?;
        self.cpu.msp = sp;
        self.cpu.psp = 0;
        self.cpu.r[13] = sp;
        self.cpu.r[15] = entry & !1;
        self.cpu.xpsr = crate::cpu::psr::T;
        // Real crt0 points VTOR at this table within a few instructions; the
        // model does it here so an early interrupt does not read a zero vector.
        self.mem.periph.ppb.vtor = vector;
        self.rom_stage = false;
        Ok(())
    }

    /// Put the core back at its reset vector without reloading anything.
    ///
    /// The board bridge is not part of the chip's reset state: the adapter
    /// installs it once, before the first boot, and every block it points at is
    /// rebuilt here. The callbacks are carried across so an I2C or SPI
    /// transaction after a reset still reaches the circuit.
    pub fn reset(&mut self) {
        self.cpu = Core::new();
        self.cpu1 = Core::new();
        self.core1_started = false;
        self.mem.active_core = 0;
        self.cycles = 0;
        self.fault = None;
        self.console.clear();
        self.outputs.clear();
        self.released.clear();
        self.published = [None; 32];
        self.rom_stage = false;

        let i2c: Vec<_> = self
            .mem
            .periph
            .i2c
            .iter_mut()
            .map(|block| block.take_callbacks())
            .collect();
        let spi: Vec<_> = self
            .mem
            .periph
            .spi
            .iter_mut()
            .map(|block| block.take_transfer())
            .collect();

        self.mem.periph = Peripherals::new();

        for (block, (write, read)) in self.mem.periph.i2c.iter_mut().zip(i2c) {
            if let (Some(write), Some(read)) = (write, read) {
                block.set_callbacks(write, read);
            }
        }
        for (block, transfer) in self.mem.periph.spi.iter_mut().zip(spi) {
            if let Some(transfer) = transfer {
                block.set_transfer(transfer);
            }
        }
    }

    /// A host GPIO level for the next slice.
    pub fn set_input(&mut self, gpio: u8, high: bool) {
        self.inputs.insert(gpio, high);
    }

    /// A host voltage on an ADC channel, in microvolts.
    pub fn set_analog_input(&mut self, channel: u8, microvolts: u32) {
        if let Some(slot) = self.analog.get_mut(channel as usize) {
            *slot = microvolts;
        }
    }

    /// GPIO levels published since the last call, as edges.
    pub fn take_outputs(&mut self) -> Vec<(u8, bool)> {
        std::mem::take(&mut self.outputs)
    }

    /// Pins the chip stopped driving — the function select moved, or the
    /// peripheral that owned the pin released its output enable. The adapter
    /// turns each into a high-impedance pad so a passive cannot keep an old
    /// level alive.
    pub fn take_releases(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.released)
    }

    /// Whether core 1 has been released by a launch message.
    pub fn core1_started(&self) -> bool {
        self.core1_started
    }

    /// Bytes the guest wrote to UART0 since the last call.
    pub fn take_console(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.console)
    }

    /// Feed bytes into UART0's receiver.
    pub fn write_console(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.mem.periph.uart[0].rx.push_back(*byte);
        }
        self.mem.periph.uart[0].update_rx_irq();
    }

    /// The line rate the firmware programmed for a UART port, or 0 when it
    /// has not configured one.
    pub fn uart_baud(&self, port: usize) -> u32 {
        let peri_hz = u64::from(self.mem.periph.clocks.clk_peri_hz());
        self.mem.periph.uart[port].baud_hz(peri_hz)
    }

    /// The pads the firmware assigned to a UART port, in the order the
    /// chip's GPIO function table offers them. A pin counts when IO_BANK0
    /// selects the UART function for it, which is what `machine.UART` sets.
    pub fn uart_pins(&self, port: usize) -> (u64, Option<u8>) {
        let (tx_pins, rx_pins) = if port == 0 {
            (UART_TX_PINS[0], UART_RX_PINS[0])
        } else {
            (UART_TX_PINS[1], UART_RX_PINS[1])
        };
        let mut tx = 0u64;
        let mut rx = None;
        for &pin in tx_pins {
            if self.mem.periph.io.funcsel(pin) == FUNCSEL_UART {
                tx |= 1 << pin;
            }
        }
        for &pin in rx_pins {
            if rx.is_none() && self.mem.periph.io.funcsel(pin) == FUNCSEL_UART {
                rx = Some(pin);
            }
        }
        (tx, rx)
    }

    /// Bytes a UART port has transmitted since the last call.
    pub fn take_uart_tx(&mut self, port: usize) -> Vec<u8> {
        std::mem::take(&mut self.uart_tx[port])
    }

    /// How many bytes the receiver will take right now. A disabled receiver
    /// takes none, which holds the wire's bytes until the firmware enables it.
    pub fn uart_rx_room(&self, port: usize) -> usize {
        let uart = &self.mem.periph.uart[port];
        if !uart.receiver_enabled() {
            return 0;
        }
        uart.receive_room()
    }

    /// Deliver bytes from the board's wire into a UART receiver.
    pub fn push_uart(&mut self, port: usize, bytes: &[u8]) {
        for byte in bytes {
            self.mem.periph.uart[port].rx.push_back(*byte);
        }
        self.mem.periph.uart[port].update_rx_irq();
    }

    pub fn is_idle(&self) -> bool {
        self.cpu.waiting && (!self.core1_started || self.cpu1.waiting)
    }

    /// One instruction on each running core, plus whatever the peripherals did
    /// during it.
    ///
    /// The two cores share the clock, so a step is as long as the slower of
    /// them: the machine advances by the larger cycle count and both cores'
    /// next instructions start from the same instant.
    pub fn step(&mut self) -> Result<u64, Fault> {
        if self.rom_stage && self.cpu.pc() == ROM_HANDOFF {
            self.enter_application()?;
        }
        self.present_inputs();
        self.mem.active_core = 0;
        let mut cycles = self.step_core(0)?;
        if self.core1_started {
            self.mem.active_core = 1;
            let second = self.step_core(1);
            self.mem.active_core = 0;
            cycles = cycles.max(second?);
        }
        self.advance(cycles);
        Ok(cycles)
    }

    /// One instruction on `core`, or one cycle of `WFI`.
    fn step_core(&mut self, core: usize) -> Result<u64, Fault> {
        self.deliver_interrupt(core)?;
        let waiting = if core == 0 {
            self.cpu.waiting
        } else {
            self.cpu1.waiting
        };
        if waiting {
            return Ok(1);
        }
        let cycles = if core == 0 {
            self.cpu.step(&mut self.mem)?
        } else {
            self.cpu1.step(&mut self.mem)?
        };
        Ok(u64::from(cycles).max(1))
    }

    /// Run until `budget` cycles have passed or something stops the core.
    pub fn run(&mut self, budget: u64) -> Stop {
        let start = self.cycles;
        while self.cycles - start < budget {
            // Checked before the instruction at the breakpoint runs, and only
            // once the run has advanced: continuing after a stop must leave
            // the breakpoint behind instead of stopping on it again.
            if self.cycles > start {
                if let Some(&pc) = self.breakpoints.get(&self.cpu.pc()) {
                    return Stop::Breakpoint(pc);
                }
                if self.core1_started
                    && let Some(&pc) = self.breakpoints.get(&self.cpu1.pc())
                {
                    return Stop::Breakpoint(pc);
                }
            }
            // A core parked in `WFE` with nothing pending can still be woken
            // by the machine's own schedule: a timer alarm, or the modelled
            // USB host's next enumeration step, which is what moves the
            // console bytes a Pico's REPL waits for. Jump straight to the
            // next one instead of advancing a cycle at a time, and report
            // `Idle` only when neither is pending.
            if self.is_idle() && !self.any_pending() {
                let remaining = budget - (self.cycles - start);
                let due = self
                    .mem
                    .periph
                    .timer
                    .cycles_to_next_alarm()
                    .into_iter()
                    .chain(self.mem.periph.usb.cycles_to_next_step())
                    .min();
                match due {
                    Some(due) => {
                        self.advance(due.min(remaining).max(1));
                        // WFE wakes on an event, not only on a delivered
                        // interrupt. The scheduled event is that event; a
                        // pending interrupt, if any, is taken on the next
                        // step like any other.
                        self.cpu.waiting = false;
                        if self.core1_started {
                            self.cpu1.waiting = false;
                        }
                        continue;
                    }
                    None => return Stop::Idle,
                }
            }
            match self.step() {
                Ok(_) => {}
                Err(Fault::Halted) => {
                    self.fault.get_or_insert_with(|| "core halted".into());
                    return Stop::Halted;
                }
                Err(fault) => {
                    // The instruction address is what turns "no target at
                    // 0x0" into something a firmware author can act on.
                    self.fault = Some(format!("{fault} (pc {:#010x})", self.cpu.pc()));
                    return Stop::Trap;
                }
            }
            // Either core asking for a system reset restarts the chip.
            if self.mem.periph.ppb.sysreset || self.mem.periph.ppb1.sysreset {
                self.mem.periph.ppb.sysreset = false;
                self.mem.periph.ppb1.sysreset = false;
                return Stop::Reset;
            }
        }
        if self.is_idle() && !self.any_pending() {
            return Stop::Idle;
        }
        Stop::Budget
    }

    fn any_pending(&mut self) -> bool {
        // Reading this must not consume the SysTick latch: an idle `WFI` with
        // an armed tick is not idle at all, it is one period from waking.
        let second = self.core1_started
            && (self.mem.periph.ppb1.pending_irq().is_some()
                || self.mem.periph.ppb1.systick_raised()
                || self.mem.periph.ppb1.systick_armed());
        second
            || self.mem.periph.ppb.pending_irq().is_some()
            || self.mem.periph.ppb.systick_raised()
            || self.mem.periph.ppb.systick_armed()
    }

    fn advance(&mut self, cycles: u64) {
        self.cycles += cycles;
        let pads = self.pad_levels();
        let core1 = self.core1_started;
        // A UART bit lasts a number of peripheral cycles, while `cycles`
        // counts system ones, so the shifter needs both clocks.
        let sys_hz = self.mem.periph.clocks.clk_sys_hz();
        let peri_hz = self.mem.periph.clocks.clk_peri_hz();
        let periph = &mut self.mem.periph;
        periph.timer.tick(cycles);
        periph.ticks.tick(cycles);
        periph.watchdog.tick(cycles);
        periph.uart[0].tick(cycles, sys_hz, peri_hz);
        periph.uart[1].tick(cycles, sys_hz, peri_hz);
        periph.spi[0].tick(cycles);
        periph.spi[1].tick(cycles);
        periph.i2c[0].tick(cycles);
        periph.i2c[1].tick(cycles);
        periph.pwm.tick(cycles);
        periph.adc.tick(cycles);
        periph.dma.tick(cycles);
        periph.usb.tick(cycles);
        periph.ppb.tick(cycles);
        if core1 {
            periph.ppb1.tick(cycles);
        }
        // Both PIO blocks see the pads as they were at the start of the
        // interval, which is what their pin inputs sampled.
        periph.pio[0].tick(cycles, pads);
        periph.pio[1].tick(cycles, pads);
        // IO_BANK0's edge detectors compare against the level it saw last, so
        // the block has to see every interval, not only the pins that moved.
        periph.io.set_pads(pads);
        // The DMA engine needs the bus, which is the memory itself, so it is
        // serviced after the block ticks have released the borrow.
        self.mem.service_dma();
        self.collect_irqs();
        self.check_core1_launch();
        self.drain_uart();
        self.publish_pins();
    }

    /// Latch every peripheral's interrupt line into both NVICs.
    fn collect_irqs(&mut self) {
        let core1 = self.core1_started;
        let periph = &mut self.mem.periph;
        let mut irqs: Vec<(u16, bool)> = Vec::new();
        for (base, irq) in [
            (0usize, irq::TIMER_IRQ_0),
            (1, irq::TIMER_IRQ_1),
            (2, irq::TIMER_IRQ_2),
            (3, irq::TIMER_IRQ_3),
        ] {
            irqs.push((irq, periph.timer.irq(base)));
        }
        irqs.push((irq::UART0_IRQ, periph.uart[0].irq()));
        irqs.push((irq::UART1_IRQ, periph.uart[1].irq()));
        irqs.push((irq::SPI0_IRQ, periph.spi[0].irq()));
        irqs.push((irq::SPI1_IRQ, periph.spi[1].irq()));
        irqs.push((irq::I2C0_IRQ, periph.i2c[0].irq()));
        irqs.push((irq::I2C1_IRQ, periph.i2c[1].irq()));
        irqs.push((irq::PWM_IRQ_WRAP, periph.pwm.irq()));
        irqs.push((irq::ADC_IRQ_FIFO, periph.adc.irq()));
        irqs.push((irq::DMA_IRQ_0, periph.dma.irq()));
        irqs.push((irq::RTC_IRQ, periph.watchdog.irq()));
        irqs.push((irq::USBCTRL_IRQ, periph.usb.irq()));
        // Each PIO block raises its two lines per core, and a core takes one
        // when either of its two lines fires.
        irqs.push((irq::PIO0_IRQ_0, periph.pio[0].irq_line(0)));
        irqs.push((irq::PIO0_IRQ_1, periph.pio[0].irq_line(1)));
        irqs.push((irq::PIO1_IRQ_0, periph.pio[1].irq_line(0)));
        irqs.push((irq::PIO1_IRQ_1, periph.pio[1].irq_line(1)));
        // SIO's per-core line follows the FIFO the other core wrote into.
        irqs.push((irq::SIO_IRQ_PROC0, periph.sio.fifo_has_data(0)));
        irqs.push((irq::SIO_IRQ_PROC1, periph.sio.fifo_has_data(1)));
        for &(irq, level) in &irqs {
            periph.ppb.set_line(irq, level);
            if core1 {
                periph.ppb1.set_line(irq, level);
            }
        }
        // IO_BANK0 keeps a separate enable mask per core, so the line it
        // raises is not the same on both.
        periph.ppb.set_line(irq::IO_IRQ_BANK0, periph.io.irq(0));
        if core1 {
            periph.ppb1.set_line(irq::IO_IRQ_BANK0, periph.io.irq(1));
        }
    }

    /// The duty each GPIO routed to the PWM block is programmed to, as the
    /// fraction of its period it spends high.
    ///
    /// The block's channel mapping is fixed in silicon: GPIO `n` is slice
    /// `(n >> 1) & 7`, channel A for an even pin and B for an odd one, so
    /// GP0/GP16 are PWM0A, GP1/GP17 are PWM0B, and so on. A pin counts only
    /// while IO_BANK0 selects the PWM function for it.
    pub fn pwm_duties(&self) -> Vec<(u8, f64)> {
        let mut duties = Vec::new();
        for gpio in 0..32u8 {
            if self.mem.periph.io.funcsel(gpio) != FUNCSEL_PWM {
                continue;
            }
            if let Some(duty) = self.mem.periph.pwm.duty(pwm_channel(gpio)) {
                duties.push((gpio, duty));
            }
        }
        duties
    }

    fn drain_uart(&mut self) {
        for port in 0..self.mem.periph.uart.len() {
            let bytes = std::mem::take(&mut self.mem.periph.uart[port].tx);
            if bytes.is_empty() {
                continue;
            }
            // Each byte leaving the FIFO is one transmit-interrupt event.
            self.mem.periph.uart[port].tx_shifted();
            if port == 0 {
                self.console.extend_from_slice(&bytes);
            }
            self.uart_tx[port].extend_from_slice(&bytes);
        }
    }

    /// Core 1 is released by a message core 0 writes to SIO's FIFO: the bootrom
    /// on the second core spins waiting for that word and, once it arrives,
    /// reads a launch block from it and jumps. `multicore_launch_core1` writes
    /// a pointer to `{entry, sp, vector_table}` in RAM or flash; a value that
    /// is not a readable address is taken as a bare entry with the ROM's
    /// defaults.
    fn check_core1_launch(&mut self) {
        if self.core1_started {
            return;
        }
        let Some((writer, word)) = self.mem.periph.sio.take_pending_write() else {
            return;
        };
        // The word went into the other core's queue and core 1's boot code
        // consumes it, so drop it there rather than leaving it for a running
        // core to read as data.
        let _ = self.mem.periph.sio.fifo_read(usize::from(writer) ^ 1);
        // The block sits wherever the linker put it: `launch_sequence` is
        // writable state, so either SRAM or a flash rodata section.
        let readable = (SRAM_BASE..SRAM_BASE + 0x4_2000).contains(&word)
            || (FLASH_BASE..FLASH_BASE + 0x0100_0000).contains(&word);
        let (entry, sp, vtor) = if readable && word & 3 == 0 {
            let entry = self.mem.read32(word).unwrap_or(0);
            let sp = self.mem.read32(word + 4).unwrap_or(BOOT_STACK);
            let vtor = self.mem.read32(word + 8).unwrap_or(0);
            (entry, sp, vtor)
        } else {
            (word, BOOT_STACK, 0)
        };
        if entry == 0 {
            return;
        }
        let stack = if (SRAM_BASE..SRAM_BASE + 0x4_2000).contains(&sp) {
            sp
        } else {
            BOOT_STACK
        };
        self.cpu1.r[13] = stack;
        self.cpu1.msp = stack;
        self.cpu1.r[15] = entry & !1;
        self.cpu1.xpsr = crate::cpu::psr::T;
        // A launch block names the second core's own vector table; without
        // one it shares the application's, which is the reset value of a
        // core's VTOR.
        self.mem.periph.ppb1.vtor = if vtor != 0 { vtor } else { 0x1000_0100 };
        self.core1_started = true;
    }

    /// Turn every block's drive into pin edges, and report a release when a
    /// pin stops being driven at all.
    fn publish_pins(&mut self) {
        let uart_tx = self.uart_tx_mask();
        for gpio in 0..30u8 {
            let slot = usize::from(gpio);
            match self.pin_digital(gpio, uart_tx) {
                Some(high) => {
                    if self.published[slot] != Some(high) {
                        self.published[slot] = Some(high);
                        self.outputs.push((gpio, high));
                    }
                }
                None => {
                    if self.published[slot].is_some() {
                        self.published[slot] = None;
                        self.released.push(gpio);
                    }
                }
            }
        }
    }

    /// The level a block drives onto `gpio`, or `None` when nothing drives it.
    ///
    /// A pin belongs to whichever peripheral its function select names, so the
    /// answer is the SIO output when the pin is SIO's, the PWM channel's level
    /// when it is PWM's, a PIO state machine's output when it is PIO's, and the
    /// resting high of a UART transmitter when it is the UART's. The mux can
    /// then override what the block would drive (`*OVER`), and a pad with
    /// `OD` set drives nothing at all.
    fn pin_drive(&self, gpio: u8, uart_tx: u64) -> Option<bool> {
        let periph = &self.mem.periph;
        let bit = 1u32 << gpio;
        let (level, enabled) = match periph.io.funcsel(gpio) {
            FUNCSEL_SIO => (periph.sio.out & bit != 0, periph.sio.oe & bit != 0),
            FUNCSEL_PWM => (periph.pwm.level(pwm_channel(gpio))?, true),
            FUNCSEL_PIO0 => (
                periph.pio[0].pad_out & bit != 0,
                periph.pio[0].pad_oe & bit != 0,
            ),
            FUNCSEL_PIO1 => (
                periph.pio[1].pad_out & bit != 0,
                periph.pio[1].pad_oe & bit != 0,
            ),
            FUNCSEL_UART => {
                let port = usize::from(!UART_TX_PINS[0].contains(&gpio));
                (periph.uart[port].line_level(), uart_tx & (1 << gpio) != 0)
            }
            _ => return None,
        };
        let ctrl = periph.io.control(gpio);
        let level = Self::override_signal(ctrl >> gpio_ctrl::OUTOVER_SHIFT, level);
        let enabled = Self::override_signal(ctrl >> gpio_ctrl::OEOVER_SHIFT, enabled);
        (enabled && !periph.pads.output_disabled(gpio)).then_some(level)
    }

    /// One output `*OVER` field: pass the signal through, invert it, or force
    /// it to the field's level.
    fn override_signal(field: u32, value: bool) -> bool {
        match field & 3 {
            0 => value,
            1 => !value,
            2 => true,
            _ => false,
        }
    }

    /// The level a block drives onto `gpio` as a digital line. A PWM pin's
    /// value is a duty cycle, so the board takes it through its own channel
    /// instead of as an edge; every other function, including a UART
    /// transmitter, is a level that changes and is published as one.
    fn pin_digital(&self, gpio: u8, uart_tx: u64) -> Option<bool> {
        match self.mem.periph.io.funcsel(gpio) {
            FUNCSEL_SIO | FUNCSEL_UART | FUNCSEL_PIO0 | FUNCSEL_PIO1 => {
                self.pin_drive(gpio, uart_tx)
            }
            _ => None,
        }
    }

    /// Every pin either UART port transmits on right now.
    fn uart_tx_mask(&self) -> u64 {
        self.uart_pins(0).0 | self.uart_pins(1).0
    }

    fn present_inputs(&mut self) {
        // A pad reads back whatever its block drives onto it, the level the
        // board holds when nothing does, and the pad's own pull when the pin
        // is wired to nothing at all.
        self.mem.periph.sio.inp = self.pad_levels();
    }

    /// The level each pad presents to the chip.
    fn pad_levels(&self) -> u32 {
        let uart_tx = self.uart_tx_mask();
        let mut levels = 0u32;
        for gpio in 0..30u8 {
            let level = if let Some(driven) = self.pin_drive(gpio, uart_tx) {
                driven
            } else if !self.mem.periph.pads.input_enabled(gpio) {
                false
            } else {
                let input = match self.inputs.get(&gpio) {
                    Some(held) => *held,
                    None => {
                        let (up, down) = self.mem.periph.pads.pulls(gpio);
                        up && !down
                    }
                };
                // `INOVER` reads the pad the other way round, or ignores it:
                // 0 passes it, 1 inverts it, 2 forces low and 3 forces high.
                match (self.mem.periph.io.control(gpio) >> gpio_ctrl::INOVER_SHIFT) & 3 {
                    0 => input,
                    1 => !input,
                    2 => false,
                    _ => true,
                }
            };
            if level {
                levels |= 1 << gpio;
            }
        }
        levels
    }

    fn deliver_interrupt(&mut self, core: usize) -> Result<(), Fault> {
        let own = if core == 0 { &self.cpu } else { &self.cpu1 };
        if !own.accepts_interrupt() {
            return Ok(());
        }
        // The NVIC never re-enters an interrupt that is already active. This
        // model has no active-priority state, so an external IRQ handler runs
        // to completion before another peripheral IRQ is delivered. Without
        // this a level-held line (the timer's `INTR` before its handler clears
        // it) would push a new exception frame on every instruction.
        if own.exception >= 16 {
            return Ok(());
        }
        let vector = {
            let ppb = if core == 0 {
                &mut self.mem.periph.ppb
            } else {
                &mut self.mem.periph.ppb1
            };
            if let Some(irq) = ppb.pending_irq() {
                ppb.ack(irq);
                16 + irq
            } else if ppb.systick_pending() {
                ppb::SYSTICK_EXCEPTION
            } else {
                return Ok(());
            }
        };
        let cpu = if core == 0 {
            &mut self.cpu
        } else {
            &mut self.cpu1
        };
        cpu.enter_exception(&mut self.mem, vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flash image whose stage 2 is `push {lr}; pop {pc}` and whose
    /// application vector table points at a `b .` loop.
    fn machine() -> Machine {
        let mut flash = vec![0u8; 0x200];
        flash[0..2].copy_from_slice(&0xb500u16.to_le_bytes());
        flash[2..4].copy_from_slice(&0xbd00u16.to_le_bytes());
        flash[0x100..0x104].copy_from_slice(&0x2004_1000u32.to_le_bytes());
        flash[0x104..0x108].copy_from_slice(&0x1000_0109u32.to_le_bytes());
        flash[0x108..0x10a].copy_from_slice(&0xe7feu16.to_le_bytes());
        let mut machine = Machine::new();
        machine.load_flash(0, &flash).unwrap();
        machine
    }

    #[test]
    fn the_uart_baud_follows_the_peripheral_clock_the_sdk_selects() {
        use crate::peripherals::clocks::{ctrl as clock_ctrl, reg as clock_reg};
        use crate::peripherals::uart::reg as uart_reg;
        let mut machine = machine();
        // What `set_sys_clock_pll` leaves behind: clk_peri on the 48 MHz USB
        // PLL. MicroPython then programs 115 200 as IBRD 26, FBRD 3.
        machine.mem.periph.clocks.write(
            clock_reg::PERI_CTRL,
            clock_ctrl::ENABLE | (2 << clock_ctrl::AUXSRC_SHIFT),
        );
        machine
            .mem
            .periph
            .clocks
            .write(clock_reg::PERI_CTRL + 4, 1 << 8);
        machine.mem.periph.uart[1].write(uart_reg::IBRD, 26);
        machine.mem.periph.uart[1].write(uart_reg::FBRD, 3);
        let baud = machine.uart_baud(1);
        assert!((baud as i64 - 115_200).abs() < 60, "got {baud}");
    }

    #[test]
    fn the_rom_copies_stage_two_and_hands_off_to_the_vector_table() {
        let mut machine = machine();
        machine.boot();
        assert_eq!(machine.pc(), SRAM_BASE, "stage 2 runs from SRAM");
        assert_eq!(machine.cpu.sp(), BOOT_STACK);
        assert_eq!(machine.cpu.r[14] & !1, ROM_HANDOFF);
        assert_eq!(&machine.mem.sram[..2], &0xb500u16.to_le_bytes()[..]);

        machine.step().unwrap(); // push {lr}
        machine.step().unwrap(); // pop {pc}: back to the ROM
        assert_eq!(machine.pc(), ROM_HANDOFF);
        machine.step().unwrap(); // the ROM reads the vector table and enters
        assert_eq!(machine.pc(), 0x1000_0108);
        assert_eq!(machine.cpu.sp(), 0x2004_1000);
        assert_eq!(machine.mem.periph.ppb.vtor, 0x1000_0100);
    }

    /// What the fake board recorded: address and bytes per I2C write.
    type I2cWrites = std::rc::Rc<std::cell::RefCell<Vec<(u8, Vec<u8>)>>>;

    #[test]
    fn a_machine_reset_keeps_the_board_bridge() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut machine = Machine::new();
        let seen: I2cWrites = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        machine.periph_mut().i2c[0].set_callbacks(
            Box::new(move |address, bytes| sink.borrow_mut().push((address, bytes.to_vec()))),
            Box::new(|_, len| vec![0; len]),
        );
        machine.boot();

        use crate::peripherals::i2c::{con, data_cmd, enable, reg};
        let i2c = &mut machine.mem.periph.i2c[0];
        i2c.write(
            reg::CON,
            con::MASTER_MODE | con::RESTART_EN | con::SLAVE_DISABLE,
        );
        i2c.write(reg::ENABLE, enable::ENABLE);
        i2c.write(reg::TAR, 0x48);
        i2c.write(reg::DATA_CMD, 0x5a | data_cmd::STOP);
        assert_eq!(&*seen.borrow(), &[(0x48, vec![0x5a])]);
    }

    #[test]
    fn only_a_pin_the_mux_gives_to_sio_is_driven() {
        use crate::peripherals::sio::reg as sio_reg;
        let mut machine = machine();
        machine.boot();
        let mask = 1 << 5;
        machine.mem.periph.sio.write(sio_reg::GPIO_OE_SET, mask, 0);
        machine.mem.periph.sio.write(sio_reg::GPIO_OUT_SET, mask, 0);
        machine.step().unwrap();
        assert!(
            !machine.take_outputs().iter().any(|&(pin, _)| pin == 5),
            "a pin still on its reset function drives nothing"
        );
        // The same writes now reach the pad once IO_BANK0 names SIO for it.
        machine.mem.periph.io.write(0x004 + 8 * 5, 5);
        machine.step().unwrap();
        assert!(machine.take_outputs().contains(&(5, true)));
        // Releasing the output enable reports the pin as no longer driven.
        machine.mem.periph.sio.write(sio_reg::GPIO_OE_CLR, mask, 0);
        machine.step().unwrap();
        assert!(machine.take_releases().contains(&5));
    }

    #[test]
    fn the_mux_overrides_and_the_pad_can_cut_its_output() {
        use crate::peripherals::io::{gpio_ctrl, pad};
        use crate::peripherals::sio::reg as sio_reg;
        let mut machine = machine();
        machine.boot();
        machine.mem.periph.io.write(0x004 + 8 * 5, 5);
        machine
            .mem
            .periph
            .sio
            .write(sio_reg::GPIO_OE_SET, 1 << 5, 0);
        machine
            .mem
            .periph
            .sio
            .write(sio_reg::GPIO_OUT_SET, 1 << 5, 0);
        machine.step().unwrap();
        assert!(machine.take_outputs().contains(&(5, true)));
        // `OUTOVER = 3` forces the pad low whatever SIO drives.
        machine
            .mem
            .periph
            .io
            .write(0x004 + 8 * 5, 5 | (3 << gpio_ctrl::OUTOVER_SHIFT));
        machine.step().unwrap();
        assert!(machine.take_outputs().contains(&(5, false)));
        // `OEOVER = 3` cuts the drive entirely, which reads as a release.
        machine
            .mem
            .periph
            .io
            .write(0x004 + 8 * 5, 5 | (3 << gpio_ctrl::OEOVER_SHIFT));
        machine.step().unwrap();
        assert!(machine.take_releases().contains(&5));
        // So does the pad's own output-disable bit, with the mux back to SIO.
        machine.mem.periph.io.write(0x004 + 8 * 5, 5);
        machine.mem.periph.pads.write(0x004 + 4 * 5, pad::OD);
        machine.step().unwrap();
        assert!(!machine.take_outputs().iter().any(|&(pin, _)| pin == 5));
    }

    #[test]
    fn a_pio_state_machine_reaches_the_pins_through_the_mux() {
        use crate::peripherals::pio::{pinctrl, reg as pio_reg};
        let mut machine = machine();
        machine.boot();
        machine.mem.periph.io.write(0x004 + 8 * 5, 6);
        let pio = &mut machine.mem.periph.pio[0];
        // SET PINS, 1 then SET PINDIRS, 1 with the set group starting at GP5.
        pio.write(pio_reg::INSTR_MEM0, 0xe001);
        pio.write(pio_reg::INSTR_MEM0 + 4, 0xe081);
        pio.write(
            pio_reg::SM0_CLKDIV + 0x14,
            5 << pinctrl::SET_BASE_SHIFT | 1 << pinctrl::SET_COUNT_SHIFT,
        );
        pio.write(pio_reg::CTRL, 1);
        machine.step().unwrap();
        machine.step().unwrap();
        assert!(machine.take_outputs().contains(&(5, true)));
    }

    #[test]
    fn a_sio_fifo_write_releases_core_one() {
        use crate::peripherals::sio::reg as sio_reg;
        let mut machine = machine();
        machine.boot();
        // The launch block `multicore_launch_core1` builds in SRAM.
        let block = SRAM_BASE + 0x200;
        let entry = 0x2000_0901u32;
        let stack = 0x2004_1000u32;
        let vectors = 0x2000_0800u32;
        machine.mem.write32(block, entry).unwrap();
        machine.mem.write32(block + 4, stack).unwrap();
        machine.mem.write32(block + 8, vectors).unwrap();
        machine.mem.periph.sio.write(sio_reg::FIFO_WR, block, 0);
        assert!(!machine.core1_started());
        machine.step().unwrap();
        assert!(machine.core1_started(), "core 1 is released by the message");
        assert_eq!(machine.cpu1.pc(), entry & !1);
        assert_eq!(machine.cpu1.sp(), stack);
        assert_eq!(machine.mem.periph.ppb1.vtor, vectors);
        // The word was consumed by the launch, not left as data for core 1.
        assert!(!machine.mem.periph.sio.fifo_has_data(1));
    }

    #[test]
    fn a_gpio_edge_raises_the_bank_interrupt_a_core_enabled() {
        use crate::peripherals::io::event;
        let mut machine = machine();
        machine.mem.periph.io.write(0x004 + 8 * 3, 5);
        machine.mem.periph.io.write(0x004 + 8 * 4, 5);
        // Enable the high-level event on GP3 for core 0 and GP4 for core 1.
        machine
            .mem
            .periph
            .io
            .write(0x100, event::LEVEL_HIGH << (3 * 4));
        machine
            .mem
            .periph
            .io
            .write(0x130, event::LEVEL_HIGH << (4 * 4));
        assert!(!machine.mem.periph.io.irq(0));
        // A pad going high latches the edge and holds the level bit.
        machine.mem.periph.io.set_pads(1 << 3);
        assert!(machine.mem.periph.io.irq(0));
        assert!(!machine.mem.periph.io.irq(1), "core 1 enabled another pin");
        assert_eq!(machine.mem.periph.io.latched(3), event::EDGE_HIGH);
        // Write-one-to-clear drops the edge; the level bit holds the line
        // while the pin is high, which is what makes it a level interrupt.
        machine
            .mem
            .periph
            .io
            .write(0x0f0, event::EDGE_HIGH << (3 * 4));
        assert_eq!(machine.mem.periph.io.latched(3), 0);
        machine.mem.periph.io.set_pads(0);
        assert!(!machine.mem.periph.io.irq(0));
        // A pin core 1 watches wakes its own NVIC line.
        machine.mem.periph.io.set_pads(1 << 4);
        assert!(machine.mem.periph.io.irq(1));
    }

    #[test]
    fn a_systick_tick_is_latched_and_delivered_once() {
        let mut machine = machine();
        machine.boot();
        for _ in 0..3 {
            machine.step().unwrap();
        }
        // Point the exception vector at the same `b .` loop and arm a tick.
        machine.mem.periph.ppb.vtor = 0x1000_0100;
        machine.mem.periph.ppb.systick_load = 9;
        machine.mem.periph.ppb.systick_ctrl = 0x7;
        let mut delivered = 0;
        for _ in 0..50 {
            machine.step().unwrap();
            if machine.cpu.exception == ppb::SYSTICK_EXCEPTION {
                delivered += 1;
                // Return from the handler the way compiled code does.
                machine
                    .cpu
                    .return_from_exception(&mut machine.mem, 0xffff_fff9)
                    .unwrap();
            }
        }
        assert!(
            delivered >= 4,
            "expected at least four ticks, saw {delivered}"
        );
    }
}
