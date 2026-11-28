//! The [`litz_board::Chip`] view of the ATmega328P.
//!
//! # Board attachment
//!
//! [`Atmega328p::with_board`] puts the chip on a [`litz_board::Board`] and
//! keeps the board behind `Rc<RefCell<Board>>`, so the adapter and a test hold
//! the same ownership of the parts. The attachment does three things:
//!
//! * The board's resolved pin levels become the chip's external input latch.
//! * The chip's own output edges become strong drives on the board, and the
//!   board's scheduled parts (a rangefinder's ECHO, a sensor's frame) are
//!   advanced on the chip's clock.
//! * The chip's I²C and SPI peripherals answer from the devices on the board's
//!   buses.
//!
//! ## Pin table
//!
//! Board GPIO numbers are the Arduino Uno's header numbering, which is also
//! what [`Machine::set_input`] and the GPIO output events use. The chip's own
//! [`crate::digital_to_port`] turns each into a port bit:
//!
//! | Board GPIO | Arduino pin | Port bit | Notes |
//! |------------|-------------|----------|-------|
//! | 0–7        | D0–D7       | `PORTD[0..8]` | D2/D3 are `INT0`/`INT1` |
//! | 8–13       | D8–D13      | `PORTB[0..6]` | D13 is `PB5`, the Uno LED; D10 is `SS` |
//! | 14–19      | A0–A5       | `PORTC[0..6]` | also ADC channels 0–5 |
//! | 20–21      | A6–A7       | none          | ADC channels 6–7 only (Nano/Pro Mini) |
//!
//! A board that names a header pin with a different GPIO number is a board
//! manifest problem, not an adapter one: this adapter is the Uno's point of
//! view and expects the numeric header pins.
//!
//! The analog pins are fed each pass from [`Board::analog_inputs`], so a
//! potentiometer's divider (or a solved net voltage when analog
//! co-simulation is on) reaches `A0..A7` as microvolts.
//!
//! ## Run-loop order
//!
//! One [`Chip::run`] call repeats these steps until the cycle budget is spent
//! or the core stops:
//!
//! 1. Apply host actions from [`Chip::schedule`] whose time has come.
//! 2. Advance the board to the chip's time (`board.advance_to(now_ns)`), then
//!    drain `board.take_chip_inputs()` into `machine.set_input`.
//! 3. Publish `board.analog_inputs()` on `A0..A7` as microvolts.
//! 4. Run the machine for the budget, shortened when a board part's
//!    `next_event` or a scheduled host input is due sooner.
//! 5. Publish `machine.take_outputs()` as `Drive::strong` on the board at the
//!    time the slice reached, then advance the board again so events the new
//!    drives caused are waiting for the next pass.
//!
//! Cycles become nanoseconds at [`CPU_HZ`]. A board event deadline is rounded
//! up to a cycle, so the machine never overshoots the event and the board
//! always has its edge ready when the chip next polls.

use core::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::cpu::{FLASH_SIZE, FLASH_WORDS};
use crate::hex;
use crate::machine::Machine;
use litz_board::{
    Board, Chip, ChipInfo, Conflict, FlashLayout, ImageKind, Input, RunResult, Stop as BoardStop,
};
use litz_core::{Drive, Level, PartState};

/// The ATmega328P on an Arduino Uno runs at 16 MHz.
pub const CPU_HZ: u64 = 16_000_000;

/// The board's single I²C controller. The ATmega328P has one TWI controller,
/// so both sides number it 0.
pub const I2C_CONTROLLER: u8 = 0;

/// The board's SPI controller the ATmega328P's SPI master talks to. Boards in
/// the catalog expose one controller as `spi0`.
pub const SPI_CONTROLLER: u8 = 0;

/// The first board GPIO that is an Uno analog header pin (`A0`).
pub const ANALOG_FIRST_GPIO: u8 = 14;

/// The last board GPIO that is an Arduino analog header pin (`A7`).
///
/// The Uno stops at `A5` (GPIO 19), but the Nano and Pro Mini bring out
/// `A6`/`A7` on GPIO 20/21 as the chip's dedicated `ADC6`/`ADC7` inputs.
pub const ANALOG_LAST_GPIO: u8 = 21;

/// The ADC channel for a board GPIO, when it is one of `A0..A7`.
pub fn analog_channel(gpio: u8) -> Option<u8> {
    (ANALOG_FIRST_GPIO..=ANALOG_LAST_GPIO)
        .contains(&gpio)
        .then_some(gpio - ANALOG_FIRST_GPIO)
}

/// An ATmega328P adapter for the board runtime.
pub struct Atmega328p {
    machine: Machine,
    scheduled: Vec<Input>,
    fault: Option<String>,
    break_at: Option<u16>,
    /// The flattened board the chip drives, when one is attached. Shared so a
    /// test can inspect parts without taking the board away from the adapter.
    board: Option<Rc<RefCell<Board>>>,
    /// Which development-board instance on the diagram these pads belong to.
    board_index: u16,
    /// The duty last published per compare-output pin, so a slice that did
    /// not touch a timer does not re-stamp the analog view.
    pwm: BTreeMap<u8, f64>,
}

impl Default for Atmega328p {
    fn default() -> Self {
        Self::new()
    }
}

/// Why one bounded slice of the machine came back.
enum Slice {
    Budget,
    Idle,
    Halted,
    Trap,
    Breakpoint(u32),
}

impl Atmega328p {
    pub fn new() -> Self {
        Self {
            machine: Machine::new(),
            scheduled: Vec::new(),
            fault: None,
            break_at: None,
            board: None,
            board_index: 0,
            pwm: BTreeMap::new(),
        }
    }

    /// Attach a flattened board. The board stays shared so a test (or a UI)
    /// can read part state and press controls after the chip owns it.
    pub fn with_board(self, board: Board) -> Self {
        let shared = Rc::new(RefCell::new(board));
        self.with_shared_board(shared, 0)
    }

    /// Attach a board another chip already shares, and say which board
    /// instance these pads belong to. Two MCUs on one diagram have one board
    /// and one index each, so their GPIO 4 are different pads.
    pub fn with_shared_board(mut self, board: Rc<RefCell<Board>>, index: u16) -> Self {
        self.install_bus_bridge(Rc::clone(&board), index);
        self.board = Some(board);
        self.board_index = index;
        self
    }

    /// The attached board, if any.
    pub fn board(&self) -> Option<&Rc<RefCell<Board>>> {
        self.board.as_ref()
    }

    /// Route the chip's bus peripherals to the board's devices.
    ///
    /// The callbacks are `'static` closures over a shared board, which is what
    /// [`Machine::set_i2c_callbacks`] and [`Machine::set_spi_callback`] want.
    /// They survive [`Machine::reset`] (the machine takes and reinstates them),
    /// so a reboot does not detach the board.
    fn install_bus_bridge(&mut self, board: Rc<RefCell<Board>>, index: u16) {
        // The AVR TWI calls `i2c_write(addr, bytes)` once a master-transmit
        // transaction stops, and `i2c_read(addr, len)` for each byte of a read
        // phase. The pending buffer remembers the last write for an address:
        // the first read after such a write uses the board's `write_read`, so
        // a device that treats the write as a register pointer sees it before
        // it answers. A plain read (no preceding write) goes through `read`.
        //
        // The port's TWI core does not report the write phase of a repeated
        // start, so a firmware `Wire.requestFrom` that writes a pointer and
        // immediately restarts reaches the device as a plain read; the device
        // keeps its own cursor in that case.
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
        self.machine.set_i2c_callbacks(
            Box::new(move |address, bytes| {
                {
                    let mut pending = write_pending.borrow_mut();
                    pending.address = Some(address);
                    pending.bytes.clear();
                    pending.bytes.extend_from_slice(bytes);
                }
                let mut board = write_board.borrow_mut();
                board.i2c_begin(index, I2C_CONTROLLER, address);
                board
                    .buses_mut()
                    .i2c_mut(index, I2C_CONTROLLER)
                    .write(address, bytes);
                board.i2c_end(index, I2C_CONTROLLER, address);
                let at = board.now();
                board.observe_i2c(index, I2C_CONTROLLER, address, bytes, &[], at);
            }),
            Box::new(move |address, _len| {
                let register_write = {
                    let mut pending = read_pending.borrow_mut();
                    if pending.address == Some(address) {
                        Some(core::mem::take(&mut pending.bytes))
                    } else {
                        None
                    }
                };
                let mut board = read_board.borrow_mut();
                board.i2c_begin(index, I2C_CONTROLLER, address);
                let (written, outcome) = {
                    let bus = board.buses_mut().i2c_mut(index, I2C_CONTROLLER);
                    match register_write {
                        Some(bytes) if !bytes.is_empty() => {
                            let outcome = bus.write_read(address, &bytes, 1);
                            (bytes, outcome)
                        }
                        _ => (Vec::new(), bus.read(address, 1)),
                    }
                };
                board.i2c_end(index, I2C_CONTROLLER, address);
                let at = board.now();
                if let Some(bytes) = &outcome {
                    board.observe_i2c(index, I2C_CONTROLLER, address, &written, bytes, at);
                }
                outcome
                    .and_then(|bytes| bytes.first().copied())
                    .unwrap_or(0xff)
            }),
        );

        let spi_board = Rc::clone(&board);
        self.machine
            .set_spi_callback(Box::new(move |chip_select, tx| {
                let mut board = spi_board.borrow_mut();
                board
                    .spi_transfer(index, SPI_CONTROLLER, chip_select, &[tx], 1)
                    .first()
                    .copied()
                    .unwrap_or(0xff)
            }));
    }

    /// The chip's clock in nanoseconds.
    fn now_ns(&self) -> u64 {
        (u128::from(self.machine.cycles()) * 1_000_000_000 / u128::from(CPU_HZ)) as u64
    }

    /// The first cycle at or after `ns`, so a deadline is never rounded down
    /// into the past.
    fn cycles_for(ns: u64) -> u64 {
        let cycles = u128::from(ns) * u128::from(CPU_HZ);
        cycles.div_ceil(1_000_000_000) as u64
    }

    /// Pass the rates the firmware programmed into `TWBR`/`TWSR` and
    /// `SPCR`/`SPSR` to the board, so the waveform a transaction leaves
    /// behind has the bus clock the sketch actually asked for. The sync runs
    /// after every instruction: the transaction itself completes in the steps
    /// that follow the register writes, so the rate is already in place.
    fn sync_bus_rates(&mut self) {
        let Some(board) = &self.board else {
            return;
        };
        let cpu_hz = CPU_HZ as u32;
        let periph = self.machine.peripherals();
        let i2c = periph.twi.scl_hz(cpu_hz);
        let spi = periph.spi.clock_hz(cpu_hz);
        let mut board = board.borrow_mut();
        board.set_i2c_hz(self.board_index, I2C_CONTROLLER, i2c);
        board.set_spi_hz(self.board_index, SPI_CONTROLLER, spi);
    }

    /// Move the board's scheduled parts to the chip's current time.
    fn advance_board_to_now(&mut self) {
        if let Some(board) = &self.board {
            let now = self.now_ns();
            board.borrow_mut().advance_to(now);
        }
    }

    /// Apply the levels the board resolved onto the chip's input pads.
    fn pull_board_inputs(&mut self) {
        let Some(board) = &self.board else { return };
        let inputs = board.borrow_mut().take_chip_inputs_on(self.board_index);
        for (gpio, level) in inputs {
            match level {
                Level::High => self.machine.set_input(gpio, true),
                Level::Low => self.machine.set_input(gpio, false),
                // A floating pad has no level to apply. The ATmega's external
                // latch keeps its last value, which is the closest the core
                // has to "released" (there is no pull-up model).
                Level::HighZ => {}
            }
        }
    }

    /// Publish the board's analog voltages on `A0..A7` as microvolts.
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

    /// Publish the chip's output edges as strong drives on the board.
    fn publish_outputs(&mut self) {
        let Some(board) = &self.board else { return };
        let outputs = self.machine.take_outputs();
        let duties = self.machine.pwm_duties();
        if outputs.is_empty() && duties.is_empty() && self.pwm.is_empty() {
            return;
        }
        let now = self.now_ns();
        let mut board = board.borrow_mut();
        for (gpio, high) in outputs {
            let level = if high { Level::High } else { Level::Low };
            board.set_chip_output_on(self.board_index, gpio, Drive::strong(level), now);
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

    /// How many cycles the machine may run before the next scheduled host
    /// input or board event is due, capped by `remaining`.
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

    /// Run at most `budget` cycles, stopping for the same reasons as
    /// [`Machine::run`] plus a host breakpoint.
    fn run_slice(&mut self, budget: u64) -> Slice {
        let start = self.machine.cycles();
        loop {
            if let Some(word) = self.break_at
                && self.machine.pc() == word
                && self.machine.cycles() > start
            {
                return Slice::Breakpoint(u32::from(word) * 2);
            }
            if self.machine.cpu().halted() {
                return match self.machine.fault() {
                    Some(fault) => {
                        self.fault = Some(fault.to_string());
                        Slice::Trap
                    }
                    None => Slice::Halted,
                };
            }
            if self.machine.is_idle() {
                return Slice::Idle;
            }
            if self.machine.cycles() - start >= budget {
                return Slice::Budget;
            }
            if let Err(fault) = self.machine.step() {
                self.fault = Some(fault.to_string());
                return Slice::Trap;
            }
            self.sync_bus_rates();
        }
    }

    /// The underlying machine, for a test that wants registers or SRAM.
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
        self.machine
            .load_hex(text)
            .map_err(|error| error.to_string())
    }

    /// Load a raw binary at a byte offset into flash.
    pub fn load_bin(&mut self, bytes: &[u8], offset: u64) -> Result<(), String> {
        self.machine
            .load_bin(bytes, offset)
            .map_err(|error| error.to_string())
    }

    fn input_ns(input: &Input) -> u64 {
        match input {
            Input::Gpio { at_ns, .. } | Input::Uart { at_ns, .. } => *at_ns,
        }
    }

    /// Apply scheduled host actions whose time has come.
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
                Input::Uart { bytes, .. } => {
                    for byte in bytes {
                        self.machine.receive_byte(byte);
                    }
                }
            }
        }
    }
}

impl Chip for Atmega328p {
    fn info(&self) -> ChipInfo {
        ChipInfo {
            name: "atmega328p".into(),
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
                "atmega328p loads a firmware image only, not {kind:?}"
            ));
        }
        if hex::looks_like_hex(bytes) {
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
        self.machine.boot();
        self.fault = None;
        Ok(0)
    }

    /// Program flash as loaded. A sketch cannot write it, so this is the image
    /// that went in, padded to the device's full array.
    fn flash_image(&self) -> Option<Vec<u8>> {
        Some(self.machine.flash().to_vec())
    }

    fn schedule(&mut self, inputs: Vec<Input>) -> Result<(), String> {
        self.scheduled = inputs;
        self.scheduled.sort_by_key(Self::input_ns);
        Ok(())
    }

    fn run(&mut self, budget_cycles: u64) -> RunResult {
        let start = self.machine.cycles();
        let stop = loop {
            // Settle the board at the chip's time before reading its pins: a
            // part that scheduled an edge since the last slice has it ready
            // now, and a host action due at this instant belongs to this slice.
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
            let stop = self.run_slice(slice);
            self.publish_outputs();
            self.advance_board_to_now();

            match stop {
                Slice::Budget => {
                    if slice < remaining {
                        // A board event or host input ended the slice; go
                        // around so its edge reaches the chip and the rest of
                        // the budget runs from there.
                        continue;
                    }
                    break BoardStop::Budget;
                }
                Slice::Idle => break BoardStop::Idle,
                Slice::Halted => break BoardStop::Halted,
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
        self.machine.cycles() as f64 / CPU_HZ as f64
    }

    fn write_console(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.machine.receive_byte(*byte);
        }
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
        self.break_at = Some((pc / 2) as u16 & (FLASH_WORDS as u16 - 1));
    }

    fn registers(&self) -> String {
        let cpu = self.machine.cpu();
        let mut text = format!(
            "pc={:#06x} sp={:#06x} {}\n",
            self.machine.pc_bytes(),
            cpu.sp(),
            cpu.sreg()
        );
        text.push_str("r00:");
        for (index, value) in cpu.regs().iter().enumerate() {
            text.push_str(&format!(" {value:02x}"));
            if index % 8 == 7 && index != 31 {
                text.push_str(&format!("\nr{:02}:", index + 1));
            }
        }
        text
    }

    fn fault(&self) -> Option<String> {
        self.fault
            .clone()
            .or_else(|| self.machine.fault().map(|fault| fault.to_string()))
    }
}
