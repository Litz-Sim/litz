//! One CH32V003: core, memory and peripherals advanced together.
//!
//! The machine owns the clock. Every instruction returns the cycles it took,
//! the peripherals are ticked by that amount, and a pending interrupt is
//! delivered by the PFIC before the next instruction, never mid-instruction.
//!
//! The run-loop order is the RP2040 machine's: a `wfi` with nothing pending
//! advances one cycle at a time until something happens, and an idle core with
//! no timer, watchdog or scheduled event left reports [`Stop::Idle`] rather
//! than spinning to the end of the budget.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::bus::Memory;
use crate::cpu::{Bus, Core, Fault};
use crate::peripherals::pfic::irq;
use crate::peripherals::{Peripherals, Port, gpio, timers};

/// The core's clock. The CH32V003's maximum is 48 MHz and the [`Chip`]
/// contract reports it; the RCC chooses what the timers and USART see.
///
/// [`Chip`]: litz_board::Chip
pub const CPU_HZ: u64 = 48_000_000;

/// Why a run came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The cycle budget ran out.
    Budget,
    /// Nothing to do until the host changes an input.
    Idle,
    /// The core halted (`ecall`, `ebreak`).
    Halted,
    /// The core faulted; the detail is in [`Machine::fault`].
    Trap,
    /// A watchdog asked for a reset.
    Reset,
    /// The pc reached an armed breakpoint, which has not run yet.
    Breakpoint(u32),
}

pub struct Machine {
    pub cpu: Core,
    pub mem: Memory,
    /// CPU cycles since reset.
    pub cycles: u64,
    fault: Option<String>,
    console: Vec<u8>,
    /// Board pin levels the host is holding, by GPIO number.
    inputs: BTreeMap<u8, bool>,
    /// The last drive published per pin, so only edges are reported.
    published: [Option<bool>; gpio::PIN_COUNT],
    outputs: Vec<(u8, litz_core::Level)>,
    /// Bytes USART1 transmitted since the host last collected them.
    uart_tx: Vec<u8>,
    /// Bytes the board has for USART1 that the receiver had no room for.
    pending_rx: VecDeque<u8>,
    /// Program counters a run stops in front of.
    breakpoints: BTreeSet<u32>,
    reset_requested: bool,
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
            mem: Memory::new(),
            cycles: 0,
            fault: None,
            console: Vec::new(),
            inputs: BTreeMap::new(),
            published: [None; gpio::PIN_COUNT],
            outputs: Vec::new(),
            uart_tx: Vec::new(),
            pending_rx: VecDeque::new(),
            breakpoints: BTreeSet::new(),
            reset_requested: false,
        }
    }

    pub fn set_breakpoints(&mut self, pcs: impl IntoIterator<Item = u32>) {
        self.breakpoints = pcs.into_iter().collect();
    }

    pub fn add_breakpoint(&mut self, pc: u32) {
        self.breakpoints.insert(pc);
    }

    pub fn periph(&self) -> &Peripherals {
        &self.mem.periph
    }

    pub fn periph_mut(&mut self) -> &mut Peripherals {
        &mut self.mem.periph
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

    pub fn load_flash(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        self.mem.load_flash(offset, bytes)
    }

    /// Put the core and every peripheral back at their reset state while
    /// keeping the flash array and the board bridge.
    pub fn reset(&mut self) {
        let (i2c_write, i2c_read) = self.mem.periph.i2c.take_callbacks();
        let spi_transfer = self.mem.periph.spi.take_transfer();

        self.mem.periph = Peripherals::new();
        if let (Some(write), Some(read)) = (i2c_write, i2c_read) {
            self.mem.periph.i2c.set_callbacks(write, read);
        }
        if let Some(transfer) = spi_transfer {
            self.mem.periph.spi.set_transfer(transfer);
        }
        self.mem.flash_controller.reset();

        self.cpu = Core::new();
        let _ = self.cpu.reset(&mut self.mem);
        self.cycles = 0;
        self.fault = None;
        self.console.clear();
        self.outputs.clear();
        self.published = [None; gpio::PIN_COUNT];
        self.uart_tx.clear();
        self.pending_rx.clear();
        self.reset_requested = false;
    }

    /// Start the firmware from the reset vector.
    pub fn boot(&mut self) {
        self.reset();
    }

    pub fn set_input(&mut self, gpio: u8, high: bool) {
        self.inputs.insert(gpio, high);
    }

    pub fn set_analog_input(&mut self, channel: u8, microvolts: u32) {
        self.mem.periph.adc.set_input(channel, microvolts);
    }

    /// GPIO edges published since the last call. `HighZ` releases a pad that
    /// stopped being an output.
    pub fn take_outputs(&mut self) -> Vec<(u8, litz_core::Level)> {
        std::mem::take(&mut self.outputs)
    }

    /// The duty each timer channel routes to a pad is programmed to.
    pub fn pwm_duties(&self) -> Vec<(u8, f64)> {
        let mut duties = Vec::new();
        for channel in 0..4usize {
            if let Some(duty) = self.mem.periph.tim1.duty(channel) {
                duties.push((timers::tim1_pin(channel), duty));
            }
            if let Some(duty) = self.mem.periph.tim2.duty(channel) {
                duties.push((timers::tim2_pin(channel), duty));
            }
        }
        duties
    }

    pub fn take_console(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.console)
    }

    /// Feed bytes into USART1's receiver.
    pub fn write_console(&mut self, bytes: &[u8]) {
        self.mem.periph.usart.push_input(bytes);
    }

    /// Bytes USART1 has transmitted since the last call.
    pub fn take_uart_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.uart_tx)
    }

    /// Deliver bytes from the board's wire into the receiver, keeping what
    /// does not fit for later.
    pub fn push_uart(&mut self, bytes: &[u8]) {
        self.pending_rx.extend(bytes.iter().copied());
        self.drain_pending_rx();
    }

    /// Move as much of the wire's backlog into the receiver as fits now.
    pub fn drain_pending_rx(&mut self) {
        let room = self.uart_rx_room();
        let take = room.min(self.pending_rx.len());
        if take > 0 {
            let bytes: Vec<u8> = self.pending_rx.drain(..take).collect();
            self.mem.periph.usart.push_input(&bytes);
        }
    }

    pub fn uart_rx_room(&self) -> usize {
        self.mem.periph.usart.receive_room()
    }

    pub fn uart_baud(&self) -> u32 {
        self.mem.periph.usart.baud_hz(self.mem.periph.rcc.apb2_hz())
    }

    /// The pads USART1 carries when the firmware has enabled a direction.
    ///
    /// The model's default is TX PD5 (GPIO 13) and RX PD6 (GPIO 14); setting
    /// bit 2 of `AFIO_PCFR1` moves them to PC0 (GPIO 0) and PC1 (GPIO 1),
    /// which is this model's spelling of the USART1 remap.
    pub fn uart_pins(&mut self) -> (u64, Option<u8>) {
        if !self.mem.periph.usart.enabled() {
            return (0, None);
        }
        let remapped = self.mem.periph.afio.read(0x04) & 0x4 != 0;
        let (tx, rx) = if remapped { (0u8, 1u8) } else { (13u8, 14u8) };
        let tx_mask = if self.mem.periph.usart.transmitter_enabled() {
            1u64 << tx
        } else {
            0
        };
        let rx = if self.mem.periph.usart.receiver_enabled() {
            Some(rx)
        } else {
            None
        };
        (tx_mask, rx)
    }

    pub fn is_idle(&self) -> bool {
        self.cpu.waiting
    }

    /// One instruction, plus whatever the peripherals did during it.
    pub fn step(&mut self) -> Result<u64, Fault> {
        self.present_inputs();
        self.deliver_interrupt()?;
        if self.cpu.waiting {
            if self.mem.periph.pfic.any_pending() {
                // A pending line wakes `wfi` even when interrupts are masked;
                // the instruction after it runs next.
                self.cpu.waiting = false;
            } else {
                self.advance(1);
                return Ok(1);
            }
        }
        let cycles = self.cpu.step(&mut self.mem)?;
        if cycles == 0 {
            // `wfi` does not return cycles; the clock still has to move.
            self.advance(1);
            Ok(1)
        } else {
            self.advance(cycles);
            Ok(cycles)
        }
    }

    /// Run until `budget` cycles have passed or something stops the core.
    pub fn run(&mut self, budget: u64) -> Stop {
        let start = self.cycles;
        while self.cycles - start < budget {
            if self.cycles > start
                && let Some(&pc) = self.breakpoints.get(&self.cpu.pc())
            {
                return Stop::Breakpoint(pc);
            }
            if self.cpu.waiting && !self.any_pending() {
                let remaining = budget - (self.cycles - start);
                match self.cycles_to_next_event() {
                    Some(due) => {
                        self.advance(due.min(remaining).max(1));
                        self.cpu.waiting = false;
                        continue;
                    }
                    None => return Stop::Idle,
                }
            }
            match self.step() {
                Ok(_) => {}
                Err(Fault::Halted { .. }) => {
                    self.fault.get_or_insert_with(|| "core halted".into());
                    return Stop::Halted;
                }
                Err(fault) => {
                    self.fault = Some(format!("{fault}"));
                    return Stop::Trap;
                }
            }
            if self.reset_requested {
                self.reset_requested = false;
                return Stop::Reset;
            }
        }
        if self.cpu.waiting && !self.any_pending() {
            return Stop::Idle;
        }
        Stop::Budget
    }

    fn any_pending(&self) -> bool {
        self.mem.periph.pfic.any_pending()
    }

    fn cycles_to_next_event(&self) -> Option<u64> {
        let periph = &self.mem.periph;
        let tim1_hz = periph.rcc.timer_hz(periph.rcc.apb2_hz());
        let tim2_hz = periph.rcc.timer_hz(periph.rcc.apb1_hz());
        [
            periph.tim1.cycles_to_next(CPU_HZ, tim1_hz),
            periph.tim2.cycles_to_next(CPU_HZ, tim2_hz),
            periph.systick.cycles_to_next(),
            periph.iwdg.cycles_to_next(CPU_HZ),
            periph.wwdg.cycles_to_next(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn present_inputs(&mut self) {
        for gpio in 0..gpio::PIN_COUNT as u8 {
            self.mem
                .periph
                .gpio
                .set_external(gpio, self.inputs.get(&gpio).copied());
        }
        self.mem.periph.sample_exti();
    }

    fn advance(&mut self, cycles: u64) {
        self.cycles += cycles;
        let tim1_hz = self.mem.periph.rcc.timer_hz(self.mem.periph.rcc.apb2_hz());
        let tim2_hz = self.mem.periph.rcc.timer_hz(self.mem.periph.rcc.apb1_hz());
        self.mem.periph.tim1.tick(cycles, tim1_hz, CPU_HZ);
        self.mem.periph.tim2.tick(cycles, tim2_hz, CPU_HZ);
        self.mem.periph.systick.tick(cycles);
        self.mem.periph.adc.tick(cycles);
        let iwdg = self.mem.periph.iwdg.tick(cycles, CPU_HZ);
        let wwdg = self.mem.periph.wwdg.tick(cycles);
        if iwdg || wwdg {
            self.reset_requested = true;
        }
        self.collect_irqs();
        self.drain_uart();
        self.publish_pins();
    }

    fn collect_irqs(&mut self) {
        let periph = &mut self.mem.periph;
        let lines = [
            (irq::EXTI7_0, periph.exti.irq()),
            (irq::TIM1_UP, periph.tim1.irq_update()),
            (irq::TIM1_CC, periph.tim1.irq_compare()),
            (
                irq::TIM2,
                periph.tim2.irq_update() || periph.tim2.irq_compare(),
            ),
            (irq::USART1, periph.usart.irq()),
            (irq::I2C1_EV, periph.i2c.irq_event()),
            (irq::I2C1_ER, periph.i2c.irq_error()),
            (irq::SPI1, periph.spi.irq()),
            (irq::ADC1, periph.adc.irq()),
            (irq::SYSTICK, periph.systick.irq()),
            (irq::WWDG, periph.wwdg.irq()),
        ];
        for (source, high) in lines {
            periph.pfic.set_line(source, high);
        }
    }

    fn drain_uart(&mut self) {
        let bytes = self.mem.periph.usart.take_tx();
        if bytes.is_empty() {
            return;
        }
        self.console.extend_from_slice(&bytes);
        self.uart_tx.extend_from_slice(&bytes);
    }

    fn publish_pins(&mut self) {
        for gpio in 0..gpio::PIN_COUNT as u8 {
            let drive = self.mem.periph.gpio.drive(gpio);
            let slot = &mut self.published[usize::from(gpio)];
            match (drive, *slot) {
                (Some(high), Some(previous)) if high == previous => {}
                (Some(high), _) => {
                    *slot = Some(high);
                    self.outputs.push((
                        gpio,
                        if high {
                            litz_core::Level::High
                        } else {
                            litz_core::Level::Low
                        },
                    ));
                }
                (None, Some(_)) => {
                    *slot = None;
                    self.outputs.push((gpio, litz_core::Level::HighZ));
                }
                (None, None) => {}
            }
        }
    }

    fn deliver_interrupt(&mut self) -> Result<(), Fault> {
        if !self.cpu.accepts_interrupt() {
            return Ok(());
        }
        if let Some((source, priority)) =
            self.mem.periph.pfic.best_pending(self.cpu.current_priority)
        {
            self.mem.periph.pfic.acknowledge(source);
            self.cpu.enter_interrupt(&mut self.mem, source, priority)?;
        }
        Ok(())
    }

    /// Cover the SRAM so a firmware can put its stack and buffers there.
    pub fn sram(&self) -> &[u8] {
        &self.mem.sram
    }

    /// The SRAM base, for a test that addresses its own buffers.
    pub const SRAM_BASE: u32 = crate::bus::SRAM_BASE;

    /// Clear a latched fault, for a debugger that has fixed the program.
    pub fn clear_fault(&mut self) {
        self.fault = None;
    }

    /// Which port a GPIO number belongs to, for a debug print.
    pub fn port_of(gpio: u8) -> Option<Port> {
        gpio::locate(gpio).map(|(port, _)| port)
    }
}

/// Read a word through the bus without keeping a mutable alias.
impl Machine {
    pub fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
        self.mem.read32(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entry 0 is the reset vector; the code is `lui sp, 0x20000;
    /// addi x1, x0, 1; addi x2, x0, 2; jal x0, 0` at 0x10.
    fn machine() -> Machine {
        let mut machine = Machine::new();
        let program: [u32; 4] = [0x2000_0137, 0x0010_0093, 0x0020_0113, 0x0000_006f];
        let mut image = Vec::new();
        image.extend_from_slice(&0x10u32.to_le_bytes());
        image.extend_from_slice(&[0u8; 12]);
        for word in program {
            image.extend_from_slice(&word.to_le_bytes());
        }
        machine.load_flash(0, &image).unwrap();
        machine.boot();
        machine
    }

    #[test]
    fn the_machine_runs_a_straight_line_program() {
        let mut machine = machine();
        assert_eq!(machine.pc(), 0x10);
        let stop = machine.run(10);
        assert_eq!(stop, Stop::Budget);
        assert_eq!(machine.cpu.reg(1), 1);
        assert_eq!(machine.cpu.reg(2), 2);
    }

    #[test]
    fn a_reset_keeps_flash_and_reinstalls_the_bus_bridge() {
        let mut machine = machine();
        machine.run(4);
        assert_eq!(machine.cpu.reg(1), 1);
        machine.reset();
        assert_eq!(machine.pc(), 0x10);
        assert_eq!(machine.cpu.reg(1), 0);
        assert_eq!(machine.mem.flash[0x00], 0x10);
    }
}
