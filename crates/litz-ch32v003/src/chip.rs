//! The CH32V003 adapter for the board runtime.
//!
//! It has the same shape as the RP2040's: the chip owns the machine, the board
//! is shared behind `Rc<RefCell<_>>`, and each run slice settles the board at
//! the chip's time before reading its pins and after publishing its outputs.
//! Nothing in the board knows which core is behind the pads.
//!
//! # Pin numbering
//!
//! Board GPIO numbers are this backend's own pad numbers, listed in
//! [`SIO_PINS`]: `0..=7` are PC0..PC7, `8..=15` are PD0..PD7, 16 is PA1 and 17
//! is PA2. A board manifest maps its header pins onto those numbers.
//!
//! # Run-loop order
//!
//! 1. Apply scheduled host actions whose time has come.
//! 2. Advance the board to the chip's time, then drain
//!    [`Board::take_chip_inputs_on`] into the GPIO input latches.
//! 3. Publish [`Board::analog_inputs_on`] as microvolts on the ADC channels.
//! 4. Run the machine for the budget, shortened when a board part's
//!    `next_event` or a scheduled input is due sooner.
//! 5. Publish GPIO edges and PWM duties, pump USART1 to and from the board,
//!    and settle the board again so its reaction waits for the next pass.
//!
//! Cycles become nanoseconds at [`CPU_HZ`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use litz_board::{
    Board, Chip, ChipInfo, Conflict, FlashLayout, ImageKind, Input, RunResult, Stop as BoardStop,
};
use litz_core::{Drive, Level, PartState};

use crate::bus::{FLASH_SIZE, Memory};
use crate::cpu::Fault;
use crate::machine::{CPU_HZ, Machine, Stop};

pub use crate::machine::CPU_HZ as CORE_HZ;

/// The 18 pads the part brings out, in this backend's numbering.
pub const SIO_PINS: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17];

/// The board's I²C controller I2C1 talks to.
pub const I2C_CONTROLLER: u8 = 0;
/// The board's SPI controller SPI1 talks to.
pub const SPI_CONTROLLER: u8 = 0;

/// The ADC channel a pad samples, when it has one.
///
/// The CH32V003's analog-capable pads are PA1/PA2 and PC4, PD2..PD6. In this
/// backend's numbering that is:
///
/// | GPIO | Pad | ADC channel |
/// |------|-----|-------------|
/// | 16 | PA1 | 1 |
/// | 17 | PA2 | 0 |
/// | 4  | PC4 | 2 |
/// | 10 | PD2 | 3 |
/// | 11 | PD3 | 4 |
/// | 12 | PD4 | 5 |
/// | 13 | PD5 | 6 |
/// | 14 | PD6 | 7 |
pub fn analog_channel(gpio: u8) -> Option<u8> {
    match gpio {
        16 => Some(1),
        17 => Some(0),
        4 => Some(2),
        10 => Some(3),
        11 => Some(4),
        12 => Some(5),
        13 => Some(6),
        14 => Some(7),
        _ => None,
    }
}

/// One CH32V003 and the board it drives.
pub struct Ch32v003 {
    machine: Machine,
    scheduled: Vec<Input>,
    fault: Option<String>,
    board: Option<Rc<RefCell<Board>>>,
    board_index: u16,
    /// Cycles the core ran before the current power era, so the board's clock
    /// stays monotonic across a power cycle.
    elapsed_before: u64,
    powered: bool,
    /// The PWM duty last handed to the board per pin.
    pwm: BTreeMap<u8, f64>,
}

impl Default for Ch32v003 {
    fn default() -> Self {
        Self::new()
    }
}

impl Ch32v003 {
    pub fn new() -> Self {
        Ch32v003 {
            machine: Machine::new(),
            scheduled: Vec::new(),
            fault: None,
            board: None,
            board_index: 0,
            elapsed_before: 0,
            powered: true,
            pwm: BTreeMap::new(),
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

    /// Route I2C1 and SPI1 to the board's devices.
    ///
    /// The I²C bridge keeps the last write per address so a device that treats
    /// a write as a register pointer sees it before the read that follows, the
    /// same handshake the RP2040 adapter gives a `Wire.requestFrom`.
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
        periph.i2c.set_callbacks(
            Box::new(move |address, bytes| {
                {
                    let mut pending = write_pending.borrow_mut();
                    pending.address = Some(address);
                    pending.bytes = bytes.to_vec();
                }
                let mut board = write_board.borrow_mut();
                let at = board.now();
                board.i2c_write_read(index, I2C_CONTROLLER, address, bytes, 0);
                board.observe_i2c(index, I2C_CONTROLLER, address, bytes, &[], at);
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
        self.machine
            .periph_mut()
            .spi
            .set_transfer(Box::new(move |chip_select, tx| {
                let mut board = spi_board.borrow_mut();
                board.spi_transfer(index, SPI_CONTROLLER, chip_select, tx, tx.len())
            }));
    }

    fn clock_cycles(&self) -> u64 {
        self.elapsed_before.saturating_add(self.machine.cycles())
    }

    fn now_ns(&self) -> u64 {
        (u128::from(self.clock_cycles()) * 1_000_000_000 / u128::from(CPU_HZ)) as u64
    }

    fn board_power(&self) -> bool {
        match &self.board {
            Some(board) => board.borrow().board_powered(self.board_index),
            None => true,
        }
    }

    fn power_off(&mut self) {
        self.elapsed_before = self.clock_cycles();
        self.machine.reset();
        self.powered = false;
        self.pwm.clear();
        if let Some(board) = &self.board {
            let now = self.now_ns();
            board
                .borrow_mut()
                .release_chip_outputs(self.board_index, now);
        }
    }

    fn power_on(&mut self) {
        self.boot_from_reset();
        self.powered = true;
    }

    fn boot_from_reset(&mut self) {
        self.elapsed_before = self.clock_cycles();
        self.machine.boot();
        self.fault = None;
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

    /// Move USART1 bytes between the machine and the board.
    fn pump_uart(&mut self) {
        let Some(board) = self.board.clone() else {
            return;
        };
        let (tx, rx) = self.machine.uart_pins();
        // Bytes the receiver had for the pad wait here and enter as room
        // appears, whether or not this pass carried a new burst.
        self.machine.drain_pending_rx();
        if tx == 0 && rx.is_none() {
            return;
        }
        let now = self.now_ns();
        let baud = self.machine.uart_baud();
        let sent = self.machine.take_uart_tx();
        let index = self.board_index;
        {
            let mut board = board.borrow_mut();
            board.set_uart_pins(index, 0, tx, rx);
            if baud > 0 {
                board.set_uart_baud(index, 0, baud);
            }
            if !sent.is_empty() {
                board.uart_receive(index, 0, now, &sent);
            }
            if rx.is_some() {
                let got = board.uart_poll(index, 0, now);
                if !got.is_empty() {
                    self.machine.push_uart(&got);
                }
            }
        }
    }

    fn publish_outputs(&mut self) {
        let Some(board) = &self.board else { return };
        let outputs = self.machine.take_outputs();
        let duties = self.machine.pwm_duties();
        if outputs.is_empty() && duties.is_empty() && self.pwm.is_empty() {
            return;
        }
        let now = self.now_ns();
        let mut board = board.borrow_mut();
        for (gpio, level) in outputs {
            let drive = match level {
                Level::HighZ => Drive::HIGH_Z,
                Level::High => Drive::strong(Level::High),
                Level::Low => Drive::strong(Level::Low),
            };
            board.set_chip_output_on(self.board_index, gpio, drive, now);
        }
        for &gpio in self.pwm.keys() {
            if !duties.iter().any(|&(pin, _)| pin == gpio) {
                board.set_chip_pwm_on(self.board_index, gpio, None, now);
            }
        }
        for &(gpio, duty) in &duties {
            board.set_chip_pwm_on(self.board_index, gpio, Some(duty), now);
        }
        self.pwm = duties.into_iter().collect();
    }

    fn apply_due_inputs(&mut self) {
        let cycles = self.clock_cycles();
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

    fn slice_cycles(&self, remaining: u64) -> u64 {
        let now_cycles = self.clock_cycles();
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

impl Chip for Ch32v003 {
    fn info(&self) -> ChipInfo {
        ChipInfo {
            name: "ch32v003".into(),
            cpu_hz: CPU_HZ,
            flash: FlashLayout {
                size: FLASH_SIZE as u64,
                bootloader: 0,
                partition_table: 0,
                app: 0,
            },
        }
    }

    fn load_image(&mut self, kind: ImageKind, offset: u64, bytes: &[u8]) -> Result<(), String> {
        if kind != ImageKind::Firmware {
            return Err(format!(
                "ch32v003 loads a firmware image only, not {kind:?}"
            ));
        }
        self.machine
            .load_flash(offset as usize, bytes)
            .map_err(|e| e.to_string())
    }

    fn boot(&mut self) -> Result<u32, String> {
        self.powered = self.board_power();
        if !self.powered {
            // The core is held in reset until the board is supplied. Flash is
            // already loaded, so the first powered run boots from it.
            return Ok(0);
        }
        self.boot_from_reset();
        Ok(self.machine.pc())
    }

    fn schedule(&mut self, inputs: Vec<Input>) -> Result<(), String> {
        self.scheduled = inputs;
        self.scheduled.sort_by_key(Self::input_ns);
        Ok(())
    }

    fn run(&mut self, budget_cycles: u64) -> RunResult {
        if !self.board_power() {
            if self.powered {
                self.power_off();
            }
            self.elapsed_before = self.elapsed_before.saturating_add(budget_cycles);
            return RunResult {
                cycles: budget_cycles,
                stop: BoardStop::Idle,
            };
        }
        if !self.powered {
            self.power_on();
        }
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
            let remaining = budget_cycles - elapsed;
            let slice = self.slice_cycles(remaining);
            let stop = self.machine.run(slice);
            self.publish_outputs();
            self.pump_uart();
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
                Stop::Breakpoint(pc) => break BoardStop::Breakpoint(pc),
            }
        };
        RunResult {
            cycles: self.machine.cycles() - start,
            stop,
        }
    }

    fn seconds(&self) -> f64 {
        self.clock_cycles() as f64 / CPU_HZ as f64
    }

    fn write_console(&mut self, bytes: &[u8]) {
        self.machine.write_console(bytes);
    }

    fn take_console(&mut self) -> Vec<u8> {
        self.machine.take_console()
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
        self.machine.add_breakpoint(pc);
    }

    fn registers(&self) -> String {
        let cpu = &self.machine.cpu;
        let mut text = format!(
            "pc={:#010x} sp={:#010x} ra={:#010x} mstatus={:#010x}\n",
            cpu.pc,
            cpu.reg(2),
            cpu.reg(1),
            cpu.mstatus
        );
        for index in 0..16 {
            text.push_str(&format!("x{index:<2}={:#010x} ", cpu.reg(index)));
            if index % 4 == 3 {
                text.push('\n');
            }
        }
        text
    }

    fn flash_image(&self) -> Option<Vec<u8>> {
        Some(self.machine.mem.flash.clone())
    }

    fn debug_available(&self) -> bool {
        true
    }

    fn debug_pc(&self) -> Option<u32> {
        Some(self.machine.pc())
    }

    /// The 32-word register file with `x16`–`x31` at zero, plus the pc last.
    fn debug_register_file(&self) -> Option<[u32; 33]> {
        let cpu = &self.machine.cpu;
        let mut file = [0u32; 33];
        file[..16].copy_from_slice(&cpu.regs[..16]);
        file[32] = cpu.pc;
        Some(file)
    }

    fn debug_read_memory(&mut self, addr: u32, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len.min(4096));
        let end = addr.saturating_add(len as u32);
        let mut at = addr;
        while at < end {
            let Some(byte) = self.machine.mem.debug_read_byte(at) else {
                break;
            };
            out.push(byte);
            at = at.wrapping_add(1);
        }
        out
    }

    fn debug_disassemble(&mut self, addr: u32, count: usize) -> String {
        crate::disasm::disassemble(&mut self.machine.mem, addr, count)
    }

    fn debug_set_breakpoints(&mut self, pcs: &[u32]) {
        self.machine.set_breakpoints(pcs.iter().copied());
    }

    fn debug_step(&mut self) -> Option<BoardStop> {
        self.advance_board_to_now();
        self.pull_board_inputs();
        self.publish_analog();
        let stopped = match self.machine.step() {
            Ok(_) => None,
            Err(Fault::Halted { .. }) => {
                self.fault.get_or_insert_with(|| "core halted".into());
                Some(BoardStop::Halted)
            }
            Err(_) => {
                self.fault = Some(
                    self.machine
                        .fault()
                        .map(str::to_string)
                        .unwrap_or_else(|| "core trapped".into()),
                );
                Some(BoardStop::Trap)
            }
        };
        self.publish_outputs();
        self.pump_uart();
        self.advance_board_to_now();
        Some(stopped.unwrap_or(BoardStop::Idle))
    }

    fn fault(&self) -> Option<String> {
        self.fault
            .clone()
            .or_else(|| self.machine.fault().map(str::to_string))
    }
}

/// A helper for a debugger that wants the raw memory behind the adapter.
impl Ch32v003 {
    pub fn memory(&self) -> &Memory {
        &self.machine.mem
    }
}
