//! The RP2040 adapter for the board runtime.
//!
//! It is the same shape as the ATmega328P's: the chip owns the machine, the
//! board is shared behind `Rc<RefCell<_>>`, and each run slice settles the
//! board at the chip's time before reading its pins and after publishing its
//! outputs. Nothing in the board knows which core is behind the pads.

use std::cell::RefCell;
use std::rc::Rc;

use litz_board::{
    Board, Chip, ChipInfo, Conflict, FlashLayout, ImageKind, Input, RunResult, Stop as BoardStop,
};
use litz_core::{Drive, Level, PartState};

use crate::cpu::Core;
use crate::machine::{Machine, Stop};

/// The RP2040's system clock on a stock Pico: 125 MHz from the 12 MHz crystal.
pub const CPU_HZ: u64 = 125_000_000;

/// Board GPIOs the chip brings out: GP0 through GP29.
pub const SIO_PINS: &[u8] = &[
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29,
];

/// The board's I²C controller the chip's I2C0 talks to.
pub const I2C_CONTROLLER: u8 = 0;
/// The board's SPI controller the chip's SPI0 talks to.
pub const SPI_CONTROLLER: u8 = 0;
/// GP26 through GP29 are ADC channels 0 through 3.
pub const ANALOG_FIRST_GPIO: u8 = 26;
pub const ANALOG_LAST_GPIO: u8 = 29;

pub fn analog_channel(gpio: u8) -> Option<u8> {
    if (ANALOG_FIRST_GPIO..=ANALOG_LAST_GPIO).contains(&gpio) {
        Some(gpio - ANALOG_FIRST_GPIO)
    } else {
        None
    }
}

/// One RP2040 and the board it drives.
pub struct Rp2040 {
    machine: Machine,
    scheduled: Vec<Input>,
    fault: Option<String>,
    break_at: Option<u32>,
    board: Option<Rc<RefCell<Board>>>,
    board_index: u16,
}

impl Default for Rp2040 {
    fn default() -> Self {
        Self::new()
    }
}

impl Rp2040 {
    pub fn new() -> Self {
        Rp2040 {
            machine: Machine::new(),
            scheduled: Vec::new(),
            fault: None,
            break_at: None,
            board: None,
            board_index: 0,
        }
    }

    pub fn with_board(self, board: Board) -> Self {
        let shared = Rc::new(RefCell::new(board));
        self.with_shared_board(shared, 0)
    }

    pub fn with_shared_board(mut self, board: Rc<RefCell<Board>>, index: u16) -> Self {
        self.install_bus_bridge(Rc::clone(&board), index);
        self.board = Some(board);
        self.board_index = index;
        self
    }

    pub fn board(&self) -> Option<&Rc<RefCell<Board>>> {
        self.board.as_ref()
    }

    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    pub fn machine_mut(&mut self) -> &mut Machine {
        &mut self.machine
    }

    /// Route I2C0 and SPI0 to the board's devices.
    ///
    /// The I²C side keeps the last write per address so a device that treats a
    /// write as a register pointer sees it before the read that follows, which
    /// is the same handshake the AVR adapter gives a `Wire.requestFrom`.
    fn install_bus_bridge(&mut self, board: Rc<RefCell<Board>>, index: u16) {
        #[derive(Default)]
        struct Pending {
            address: Option<u8>,
            bytes: Vec<u8>,
        }
        let pending = Rc::new(RefCell::new(Pending::default()));

        let write_board = Rc::clone(&board);
        let write_pending = Rc::clone(&pending);
        let read_board = Rc::clone(&board);
        let read_pending = Rc::clone(&pending);

        let periph = self.machine.periph_mut();
        periph.i2c[usize::from(I2C_CONTROLLER)].set_callbacks(
            Box::new(move |address, bytes| {
                let mut board = write_board.borrow_mut();
                let at = board.now();
                board.i2c_write_read(index, I2C_CONTROLLER, address, bytes, 0);
                {
                    let mut pending = write_pending.borrow_mut();
                    pending.address = Some(address);
                    pending.bytes = bytes.to_vec();
                }
                let _ = at;
            }),
            Box::new(move |address, len| {
                let register_write = {
                    let mut pending = read_pending.borrow_mut();
                    if pending.address == Some(address) {
                        Some(core::mem::take(&mut pending.bytes))
                    } else {
                        None
                    }
                };
                let mut board = read_board.borrow_mut();
                let (written, bytes) = match register_write {
                    Some(bytes) if !bytes.is_empty() => (
                        bytes.clone(),
                        board.i2c_write_read(index, I2C_CONTROLLER, address, &bytes, len),
                    ),
                    _ => (
                        Vec::new(),
                        board.i2c_write_read(index, I2C_CONTROLLER, address, &[], len),
                    ),
                };
                let at = board.now();
                if let Some(bytes) = &bytes {
                    board.observe_i2c(index, I2C_CONTROLLER, address, &written, bytes, at);
                }
                bytes.unwrap_or_else(|| vec![0xff; len])
            }),
        );

        let spi_board = Rc::clone(&board);
        self.machine.periph_mut().spi[usize::from(SPI_CONTROLLER)].set_transfer(Box::new(
            move |chip_select, tx| {
                let mut board = spi_board.borrow_mut();
                board.spi_transfer(index, SPI_CONTROLLER, chip_select, tx, tx.len())
            },
        ));
    }

    fn now_ns(&self) -> u64 {
        (u128::from(self.machine.cycles()) * 1_000_000_000 / u128::from(CPU_HZ)) as u64
    }

    fn cycles_for(ns: u64) -> u64 {
        (u128::from(ns) * u128::from(CPU_HZ)).div_ceil(1_000_000_000) as u64
    }

    fn input_ns(input: &Input) -> u64 {
        match input {
            Input::Gpio { at_ns, .. } | Input::Uart { at_ns, .. } => *at_ns,
        }
    }

    fn advance_board_to_now(&mut self) {
        if let Some(board) = &self.board {
            let now = self.now_ns();
            board.borrow_mut().advance_to(now);
        }
    }

    fn pull_board_inputs(&mut self) {
        let Some(board) = &self.board else { return };
        let inputs = board.borrow_mut().take_chip_inputs_on(self.board_index);
        for (gpio, level) in inputs {
            match level {
                Level::High => self.machine.set_input(gpio, true),
                Level::Low => self.machine.set_input(gpio, false),
                Level::HighZ => {}
            }
        }
    }

    fn publish_analog(&mut self) {
        let Some(board) = &self.board else { return };
        let analog = board.borrow().analog_inputs_on(self.board_index);
        for (gpio, volts) in analog {
            let Some(channel) = analog_channel(gpio) else {
                continue;
            };
            let microvolts =
                (f64::from(volts) * 1_000_000.0).clamp(0.0, f64::from(u32::MAX)) as u32;
            self.machine.set_analog_input(channel, microvolts);
        }
    }

    fn publish_outputs(&mut self) {
        let Some(board) = &self.board else { return };
        let outputs = self.machine.take_outputs();
        if outputs.is_empty() {
            return;
        }
        let now = self.now_ns();
        let mut board = board.borrow_mut();
        for (gpio, high) in outputs {
            let level = if high { Level::High } else { Level::Low };
            board.set_chip_output_on(self.board_index, gpio, Drive::strong(level), now);
        }
    }

    fn apply_due_inputs(&mut self) {
        let cycles = self.machine.cycles();
        let now: u128 = u128::from(cycles) * 1_000_000_000;
        while let Some(input) = self.scheduled.first() {
            let due = u128::from(Self::input_ns(input)) * u128::from(CPU_HZ);
            if due > now {
                break;
            }
            let input = self.scheduled.remove(0);
            match input {
                Input::Gpio { pin, level, .. } => {
                    self.machine.set_input(pin, level == Level::High);
                }
                Input::Uart { bytes, .. } => self.machine.write_console(&bytes),
            }
        }
    }

    /// How many cycles may run before the next host input or board event.
    fn slice_cycles(&self, remaining: u64) -> u64 {
        let now_cycles = self.machine.cycles();
        let mut limit = remaining;
        if let Some(input) = self.scheduled.first() {
            let due = Self::cycles_for(Self::input_ns(input));
            limit = limit.min(due.saturating_sub(now_cycles).max(1));
        }
        if let Some(board) = &self.board
            && let Some(at_ns) = board.borrow().next_event()
        {
            let due = Self::cycles_for(at_ns);
            limit = limit.min(due.saturating_sub(now_cycles).max(1));
        }
        limit.max(1)
    }
}

impl Chip for Rp2040 {
    fn info(&self) -> ChipInfo {
        ChipInfo {
            name: "rp2040".into(),
            cpu_hz: CPU_HZ,
            flash: FlashLayout {
                size: crate::bus::FLASH_SIZE as u64,
                bootloader: 0,
                partition_table: 0,
                app: 0,
            },
        }
    }

    fn load_image(&mut self, kind: ImageKind, offset: u64, bytes: &[u8]) -> Result<(), String> {
        if kind != ImageKind::Firmware {
            return Err(format!("rp2040 loads a firmware image only, not {kind:?}"));
        }
        self.machine
            .load_flash(offset as usize, bytes)
            .map_err(|e| e.to_string())
    }

    fn boot(&mut self) -> Result<u32, String> {
        self.machine.boot();
        self.fault = None;
        Ok(self.machine.pc())
    }

    fn schedule(&mut self, inputs: Vec<Input>) -> Result<(), String> {
        self.scheduled = inputs;
        self.scheduled.sort_by_key(Self::input_ns);
        Ok(())
    }

    fn run(&mut self, budget_cycles: u64) -> RunResult {
        let start = self.machine.cycles();
        let stop = loop {
            self.advance_board_to_now();
            self.pull_board_inputs();
            self.publish_analog();
            self.apply_due_inputs();

            let elapsed = self.machine.cycles() - start;
            if elapsed >= budget_cycles {
                break BoardStop::Budget;
            }
            if let Some(pc) = self.break_at
                && self.machine.pc() == pc
                && self.machine.cycles() > start
            {
                break BoardStop::Breakpoint(pc);
            }
            let remaining = budget_cycles - elapsed;
            let slice = self.slice_cycles(remaining);
            let stop = self.machine.run(slice);
            self.publish_outputs();
            self.advance_board_to_now();
            match stop {
                Stop::Budget => {
                    if slice < remaining {
                        continue;
                    }
                    break BoardStop::Budget;
                }
                Stop::Idle => break BoardStop::Idle,
                Stop::Halted => break BoardStop::Halted,
                Stop::Trap => {
                    self.fault = self.machine.fault().map(str::to_string);
                    break BoardStop::Trap;
                }
                Stop::Reset => break BoardStop::Reset,
            }
        };
        RunResult {
            cycles: self.machine.cycles() - start,
            stop,
        }
    }

    fn seconds(&self) -> f64 {
        self.machine.cycles() as f64 / CPU_HZ as f64
    }

    fn write_console(&mut self, bytes: &[u8]) {
        // UART0 is the console on the wire, and the USB CDC OUT endpoint is
        // the console on the Pico's own USB port. Queue the bytes on both:
        // whichever one the firmware reads gets them. UART0 drops them when
        // its receiver is disabled, which is the normal MicroPython case.
        self.machine.write_console(bytes);
        self.machine.periph_mut().usb.push_console_input(bytes);
    }

    fn take_console(&mut self) -> Vec<u8> {
        let mut bytes = self.machine.take_console();
        // The machine drains UART0 every advance; the USB controller's output
        // is collected here. A firmware that logs to both consoles sends the
        // same bytes down each, so an identical copy is one message.
        let usb = self.machine.periph_mut().usb.take_output();
        if usb != bytes {
            bytes.extend(usb);
        }
        bytes
    }

    fn state(&self, part: &str) -> Option<PartState> {
        self.board.as_ref()?.borrow().state(part)
    }

    fn press(&mut self, part: &str, pressed: bool) -> bool {
        let Some(board) = &self.board else {
            return false;
        };
        let now = self.now_ns();
        board.borrow_mut().press(part, pressed, now)
    }

    fn set_control(&mut self, part: &str, value: u16) -> bool {
        let Some(board) = &self.board else {
            return false;
        };
        let now = self.now_ns();
        board.borrow_mut().set_control(part, value, now)
    }

    fn set_named_control(&mut self, part: &str, name: &str, value: &str) -> bool {
        let Some(board) = &self.board else {
            return false;
        };
        let now = self.now_ns();
        board.borrow_mut().set_named_control(part, name, value, now)
    }

    fn save_part_display(&mut self, part: &str, path: &std::path::Path) -> Result<(), String> {
        let Some(board) = &self.board else {
            return Err("this chip is not attached to a board".into());
        };
        let image = board
            .borrow()
            .part_framebuffer(part)
            .ok_or_else(|| format!("part {part:?} is not a display with a framebuffer"))?;
        std::fs::write(path, image.to_png()).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn part_pin_level(&self, part: &str, pin: Option<&str>) -> Option<Level> {
        let board = self.board.as_ref()?.borrow();
        match pin {
            Some(pin) => board.part_pin_level(part, pin),
            None => board.chip_pin_level_on(self.board_index, part),
        }
    }

    fn gpio_level(&self, gpio: u8) -> Option<Level> {
        let board = self.board.as_ref()?.borrow();
        board.chip_level_on(self.board_index, gpio)
    }

    fn conflicts(&self) -> Vec<Conflict> {
        self.board
            .as_ref()
            .map(|board| board.borrow().conflicts().to_vec())
            .unwrap_or_default()
    }

    fn start_trace(&mut self) {
        if let Some(board) = &self.board {
            board.borrow_mut().start_trace();
        }
    }

    fn vcd(&self) -> String {
        self.board
            .as_ref()
            .map(|board| board.borrow().vcd())
            .unwrap_or_default()
    }

    fn break_at(&mut self, pc: u32) {
        self.break_at = Some(pc);
    }

    fn registers(&self) -> String {
        let cpu: &Core = &self.machine.cpu;
        let mut text = format!(
            "pc={:#010x} sp={:#010x} lr={:#010x} xpsr={:#010x}\n",
            cpu.pc(),
            cpu.sp(),
            cpu.r[14],
            cpu.xpsr
        );
        for (index, value) in cpu.r[..13].iter().enumerate() {
            text.push_str(&format!("r{index:<2}={value:#010x} "));
            if index % 4 == 3 {
                text.push('\n');
            }
        }
        text
    }

    fn fault(&self) -> Option<String> {
        self.fault
            .clone()
            .or_else(|| self.machine.fault().map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::analog_channel;

    #[test]
    fn only_the_four_adc_pins_have_a_channel() {
        assert_eq!(analog_channel(15), None, "GP15 is digital, not ADC0-3");
        assert_eq!(analog_channel(26), Some(0));
        assert_eq!(analog_channel(29), Some(3));
        assert_eq!(analog_channel(30), None);
    }
}
