//! The [`litz_board::Chip`] view of an STC89C52RC.
//!
//! # Board attachment
//!
//! [`Stc89c52::with_shared_board`] puts the chip on a [`litz_board::Board`]
//! and keeps the board behind `Rc<RefCell<Board>>`, so a test (or a UI) holds
//! the same ownership of the parts. Each run pass settles the board at the
//! chip's time before reading its pins, and publishes the chip's port edges
//! afterwards, exactly as the AVR and RP2040 adapters do.
//!
//! # GPIO numbering
//!
//! Board GPIO numbers are `port * 8 + bit`: `P0.0` is GPIO 0, `P1.0` is GPIO
//! 8, `P3.2` (`INT0`) is GPIO 26 and `P3.7` is GPIO 31. A board manifest that
//! brings the 8051's pins out under those numbers works without any mapping.
//!
//! # Drive strength
//!
//! A port pin written low is a strong low. A pin written high on P1–P3 is a
//! weak high (the internal pull-up); a high on P0 is high impedance, because
//! P0 is open-drain on the real part. A pin reads back its latch ANDed with
//! the external level, so a board's button pulling a high latch low reads as
//! a zero.
//!
//! # Power
//!
//! A board that is switched off resets the core, releases its pads and still
//! advances simulated time; switching it back on boots from the code array's
//! reset vector with the firmware it already held.

use core::cell::RefCell;
use std::rc::Rc;

use litz_board::{
    Board, Chip, ChipInfo, Conflict, FlashLayout, ImageKind, Input, RunResult, Stop as BoardStop,
};
use litz_core::{Level, PartState};

pub use crate::machine::CPU_HZ;
use crate::machine::{CODE_SIZE, Machine, sfr};

/// An STC89C52RC adapter for the board runtime.
pub struct Stc89c52 {
    machine: Machine,
    scheduled: Vec<Input>,
    fault: Option<String>,
    break_at: Option<u16>,
    /// The flattened board the chip drives, when one is attached.
    board: Option<Rc<RefCell<Board>>>,
    /// Which development-board instance on the diagram these pads belong to.
    board_index: u16,
    /// Oscillator clocks retired before the current power era, so the board's
    /// clock stays monotonic across a power cycle.
    elapsed_before: u64,
    /// Whether the core is running. The board's supply decides it; a chip with
    /// no board is always powered.
    powered: bool,
}

impl Default for Stc89c52 {
    fn default() -> Self {
        Self::new()
    }
}

/// Why one bounded slice of the machine came back.
enum Slice {
    Budget,
    Idle,
    Trap,
    Breakpoint(u32),
}

impl Stc89c52 {
    pub fn new() -> Self {
        Self {
            machine: Machine::new(),
            scheduled: Vec::new(),
            fault: None,
            break_at: None,
            board: None,
            board_index: 0,
            elapsed_before: 0,
            powered: true,
        }
    }

    /// Attach a flattened board. The board stays shared so a test (or a UI)
    /// can read part state and press controls after the chip owns it.
    pub fn with_board(self, board: Board) -> Self {
        let shared = Rc::new(RefCell::new(board));
        self.with_shared_board(shared, 0)
    }

    /// Attach a board another chip already shares, and say which board
    /// instance these pads belong to.
    pub fn with_shared_board(mut self, board: Rc<RefCell<Board>>, index: u16) -> Self {
        self.board = Some(board);
        self.board_index = index;
        self.publish_all_outputs();
        self
    }

    /// The attached board, if any.
    pub fn board(&self) -> Option<&Rc<RefCell<Board>>> {
        self.board.as_ref()
    }

    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    pub fn machine_mut(&mut self) -> &mut Machine {
        &mut self.machine
    }

    /// GPIO output edges observed since the last call.
    pub fn take_outputs(&mut self) -> Vec<(u8, bool)> {
        self.machine.take_outputs()
    }

    /// Load Intel HEX text directly, without going through the chip trait.
    pub fn load_hex(&mut self, text: &str) -> Result<(), String> {
        self.machine.load_hex(text).map_err(|e| e.to_string())
    }

    /// Load a raw binary at a byte offset into code flash.
    pub fn load_bin(&mut self, bytes: &[u8], offset: u64) -> Result<(), String> {
        self.machine
            .load_bin(bytes, offset)
            .map_err(|e| e.to_string())
    }

    /// Device cycles since the diagram started, across every power cycle.
    fn clock_cycles(&self) -> u64 {
        self.elapsed_before.saturating_add(self.machine.cycles())
    }

    fn now_ns(&self) -> u64 {
        (u128::from(self.clock_cycles()) * 1_000_000_000 / u128::from(CPU_HZ)) as u64
    }

    /// The first oscillator cycle at or after `ns`.
    fn cycles_for(ns: u64) -> u64 {
        (u128::from(ns) * u128::from(CPU_HZ)).div_ceil(1_000_000_000) as u64
    }

    fn input_ns(input: &Input) -> u64 {
        match input {
            Input::Gpio { at_ns, .. } | Input::Uart { at_ns, .. } => *at_ns,
        }
    }

    /// Whether the board supplies this core. No board means no switch to ask.
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
        self.publish_all_outputs();
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
                // A released external driver lets the port's pull-up take the
                // pin high again.
                Level::HighZ => self.machine.set_input(gpio, true),
            }
        }
    }

    /// Publish the chip's output edges as board drives.
    fn publish_outputs(&mut self) {
        let Some(board) = &self.board else { return };
        let drives = self.machine.take_output_drives();
        if drives.is_empty() {
            return;
        }
        let now = self.now_ns();
        let mut board = board.borrow_mut();
        for (gpio, drive) in drives {
            board.set_chip_output_on(self.board_index, gpio, drive, now);
        }
    }

    /// Publish every port's current level. Used when the board is attached or
    /// the core boots, so a board sees the reset state (all latches high)
    /// rather than only later changes.
    fn publish_all_outputs(&mut self) {
        let Some(board) = &self.board else { return };
        let drives = self.machine.port_drives();
        let now = self.now_ns();
        let mut board = board.borrow_mut();
        for (gpio, drive) in drives {
            board.set_chip_output_on(self.board_index, gpio, drive, now);
        }
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
                    self.machine.set_input(pin, level != Level::Low);
                }
                Input::Uart { bytes, .. } => self.machine.write_console(&bytes),
            }
        }
    }

    /// How many cycles may run before the next host input or board event.
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

    fn run_slice(&mut self, budget: u64) -> Slice {
        let start = self.machine.cycles();
        loop {
            let pc = self.machine.pc();
            if self.machine.cycles() > start
                && (self.break_at == Some(pc) || self.machine.has_breakpoint(pc))
            {
                return Slice::Breakpoint(u32::from(pc));
            }
            if let Some(fault) = self.machine.fault() {
                self.fault = Some(fault.to_string());
                return Slice::Trap;
            }
            if self.machine.is_idle() {
                return Slice::Idle;
            }
            if self.machine.cycles().wrapping_sub(start) >= budget {
                return Slice::Budget;
            }
            if let Err(fault) = self.machine.step() {
                self.fault = Some(fault.to_string());
                return Slice::Trap;
            }
        }
    }
}

impl Chip for Stc89c52 {
    fn info(&self) -> ChipInfo {
        ChipInfo {
            name: "stc89c52".into(),
            cpu_hz: CPU_HZ,
            flash: FlashLayout {
                size: CODE_SIZE as u64,
                bootloader: 0,
                partition_table: 0,
                app: 0,
            },
        }
    }

    fn load_image(&mut self, kind: ImageKind, offset: u64, bytes: &[u8]) -> Result<(), String> {
        if kind != ImageKind::Firmware {
            return Err(format!(
                "stc89c52 loads a firmware image only, not {kind:?}"
            ));
        }
        if crate::hex::looks_like_hex(bytes) {
            if offset != 0 {
                return Err("an Intel HEX image already carries its own addresses".into());
            }
            let text = core::str::from_utf8(bytes)
                .map_err(|_| "a HEX image must be UTF-8 text".to_string())?;
            self.load_hex(text)
        } else {
            self.load_bin(bytes, offset)
        }
    }

    fn boot(&mut self) -> Result<u32, String> {
        self.powered = self.board_power();
        if !self.powered {
            return Ok(0);
        }
        self.boot_from_reset();
        Ok(0)
    }

    fn schedule(&mut self, inputs: Vec<Input>) -> Result<(), String> {
        self.scheduled = inputs;
        self.scheduled.sort_by_key(Self::input_ns);
        Ok(())
    }

    fn run(&mut self, budget_cycles: u64) -> RunResult {
        // A board that is switched off runs no code; the run still spends its
        // time budget so the simulated clock keeps moving.
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
            self.apply_due_inputs();

            let elapsed = self.machine.cycles() - start;
            if elapsed >= budget_cycles {
                break BoardStop::Budget;
            }
            let remaining = budget_cycles - elapsed;
            let slice = self.slice_cycles(remaining);
            let stop = self.run_slice(slice);
            self.publish_outputs();
            self.advance_board_to_now();

            match stop {
                Slice::Budget => {
                    if slice < remaining {
                        continue;
                    }
                    break BoardStop::Budget;
                }
                Slice::Idle => break BoardStop::Idle,
                Slice::Trap => break BoardStop::Trap,
                Slice::Breakpoint(pc) => break BoardStop::Breakpoint(pc),
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

    /// Program flash as loaded. A running program cannot write it, so this is
    /// the image that went in, padded to the part's 8 KiB array.
    fn flash_image(&self) -> Option<Vec<u8>> {
        Some(self.machine.code().to_vec())
    }

    fn break_at(&mut self, pc: u32) {
        self.break_at = Some(pc as u16);
    }

    fn registers(&self) -> String {
        let machine = &self.machine;
        format!(
            "pc={:#06x} sp={:#04x} psw={:#04x} acc={:#04x} b={:#04x} dptr={:#06x}\n",
            machine.pc(),
            machine.sp(),
            machine.psw(),
            machine.acc(),
            machine.b(),
            machine.dptr(),
        )
    }

    fn fault(&self) -> Option<String> {
        self.fault
            .clone()
            .or_else(|| self.machine.fault().map(|fault| fault.to_string()))
    }

    fn debug_available(&self) -> bool {
        true
    }

    fn debug_pc(&self) -> Option<u32> {
        Some(u32::from(self.machine.pc()))
    }

    /// The fixed 33-word register file: ACC, B, DPL, DPH, SP, PSW, DPL1,
    /// DPH1, DPS; the selected bank's R0–R7; zeros; and the PC last.
    fn debug_register_file(&self) -> Option<[u32; 33]> {
        let machine = &self.machine;
        let mut file = [0u32; 33];
        file[0] = u32::from(machine.acc());
        file[1] = u32::from(machine.b());
        file[2] = u32::from(machine.read_sfr(sfr::DPL));
        file[3] = u32::from(machine.read_sfr(sfr::DPH));
        file[4] = u32::from(machine.sp());
        file[5] = u32::from(machine.psw());
        file[6] = u32::from(machine.read_sfr(sfr::DPL1));
        file[7] = u32::from(machine.read_sfr(sfr::DPH1));
        file[8] = u32::from(machine.dps());
        for index in 0..8 {
            file[9 + index] = u32::from(machine.register(index as u8));
        }
        file[32] = u32::from(machine.pc());
        Some(file)
    }

    fn debug_read_memory(&mut self, addr: u32, len: usize) -> Vec<u8> {
        self.machine.debug_read_memory(addr, len)
    }

    fn debug_disassemble(&mut self, addr: u32, count: usize) -> String {
        crate::disasm::disassemble(self.machine.code(), addr, count)
    }

    fn debug_set_breakpoints(&mut self, pcs: &[u32]) {
        self.machine.set_breakpoints(pcs.iter().copied());
    }

    /// One instruction, with the board settled either side of it. A step that
    /// runs to the next instruction reports `Idle`, the same as the other
    /// in-house backends: nothing stopped execution, the caller asked for one
    /// step.
    fn debug_step(&mut self) -> Option<BoardStop> {
        self.advance_board_to_now();
        self.pull_board_inputs();
        let stopped = match self.machine.step() {
            Ok(_) => BoardStop::Idle,
            Err(fault) => {
                self.fault = Some(fault.to_string());
                BoardStop::Trap
            }
        };
        self.publish_outputs();
        self.advance_board_to_now();
        Some(stopped)
    }
}
