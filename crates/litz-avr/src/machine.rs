//! The ATmega328P as a whole: core, SRAM, and peripherals behind one data
//! bus.

use std::fmt;

use crate::cpu::{Cpu, DataBus, FLASH_SIZE, FLASH_WORDS, Fault, SRAM_END, SRAM_SIZE, SRAM_START};
use crate::hex::{self, HexError};
use crate::peripherals::{I2cRead, I2cWrite, Peripherals, SpiTransfer};

/// Why [`Machine::run`] came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The cycle budget was spent.
    Budget,
    /// The core is asleep and no enabled interrupt could wake it.
    Idle,
    /// The core stopped on purpose (`BREAK`).
    Halted,
    /// The core trapped; the detail is in [`Machine::fault`].
    Trap,
}

/// An image could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The Intel HEX text was malformed.
    Hex(HexError),
    /// A raw binary ran past the end of flash.
    BinaryOutOfRange { offset: u64, length: usize },
    /// Extended records addressed outside the 32 KiB flash.
    NotInFlash,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Hex(error) => write!(f, "{error}"),
            LoadError::BinaryOutOfRange { offset, length } => write!(
                f,
                "binary of {length} bytes at {offset:#x} does not fit in {FLASH_SIZE} bytes of flash"
            ),
            LoadError::NotInFlash => write!(f, "the image does not fit in flash"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<HexError> for LoadError {
    fn from(error: HexError) -> Self {
        LoadError::Hex(error)
    }
}

/// The part of the data space the core reaches through [`DataBus`]: SRAM and
/// every peripheral register.
pub(crate) struct Data {
    sram: [u8; SRAM_SIZE],
    pub(crate) periph: Peripherals,
}

impl Data {
    fn new() -> Self {
        Self {
            sram: [0; SRAM_SIZE],
            periph: Peripherals::new(),
        }
    }
}

impl DataBus for Data {
    fn read_data(&mut self, addr: u16) -> Result<u8, Fault> {
        match addr {
            0x0020..=0x00ff => Ok(self.periph.read(addr)),
            SRAM_START..=SRAM_END => Ok(self.sram[usize::from(addr - SRAM_START)]),
            _ => Err(Fault::DataOutOfBounds {
                addr: u32::from(addr),
            }),
        }
    }

    fn write_data(&mut self, addr: u16, value: u8) -> Result<(), Fault> {
        match addr {
            0x0020..=0x00ff => {
                self.periph.write(addr, value);
                Ok(())
            }
            SRAM_START..=SRAM_END => {
                self.sram[usize::from(addr - SRAM_START)] = value;
                Ok(())
            }
            _ => Err(Fault::DataOutOfBounds {
                addr: u32::from(addr),
            }),
        }
    }

    fn watchdog_reset(&mut self) {
        self.periph.watchdog_reset();
    }
}

/// An ATmega328P.
pub struct Machine {
    cpu: Cpu,
    data: Data,
    fault: Option<Fault>,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    /// A machine at reset, with blank flash.
    pub fn new() -> Self {
        Self {
            cpu: Cpu::new(),
            data: Data::new(),
            fault: None,
        }
    }

    /// Reset the core and peripherals and clear SRAM. Flash survives, so an
    /// image can be loaded once and booted again. Host callbacks are part of
    /// the board, not the chip, and survive the reset.
    pub fn reset(&mut self) {
        self.cpu.reset();
        let gpio = self.data.periph.take_gpio_callback();
        let spi = self.data.periph.take_spi_callback();
        let (i2c_write, i2c_read) = self.data.periph.take_i2c_callbacks();
        let spi_cs = self.data.periph.spi.cs_pin();
        let eeprom = *self.data.periph.eeprom.memory();
        let analog: [u32; 16] =
            core::array::from_fn(|channel| self.data.periph.adc.input(channel as u8));
        let reference = self.data.periph.adc.reference_microvolts();
        self.data = Data::new();
        self.data.periph.spi.set_cs_pin(spi_cs);
        *self.data.periph.eeprom.memory_mut() = eeprom;
        for (channel, microvolts) in analog.into_iter().enumerate() {
            self.data.periph.adc.set_input(channel as u8, microvolts);
        }
        self.data.periph.adc.set_reference(reference);
        if let Some(callback) = gpio {
            self.data.periph.gpio.set_callback(callback);
        }
        if let Some(callback) = spi {
            self.data.periph.spi.set_callback(callback);
        }
        if let Some(callback) = i2c_write {
            self.data.periph.twi.set_write_callback(callback);
        }
        if let Some(callback) = i2c_read {
            self.data.periph.twi.set_read_callback(callback);
        }
        self.fault = None;
    }

    pub fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    pub fn cpu_mut(&mut self) -> &mut Cpu {
        &mut self.cpu
    }

    pub fn peripherals(&self) -> &Peripherals {
        &self.data.periph
    }

    pub fn peripherals_mut(&mut self) -> &mut Peripherals {
        &mut self.data.periph
    }

    pub fn flash(&self) -> &[u8; FLASH_SIZE] {
        self.cpu.flash()
    }

    pub fn flash_mut(&mut self) -> &mut [u8; FLASH_SIZE] {
        self.cpu.flash_mut()
    }

    /// SRAM as a flat slice, for a test or a debug view.
    pub fn sram(&self) -> &[u8] {
        &self.data.sram
    }

    /// Cycles retired since reset.
    pub fn cycles(&self) -> u64 {
        self.cpu.cycles()
    }

    /// The trap that stopped the core, if any.
    pub fn fault(&self) -> Option<Fault> {
        self.fault.or_else(|| self.cpu.fault())
    }

    /// Load Intel HEX text into flash.
    pub fn load_hex(&mut self, text: &str) -> Result<(), LoadError> {
        let image = hex::parse_hex(text)?;
        if image.len() > FLASH_SIZE {
            return Err(LoadError::NotInFlash);
        }
        self.flash_mut()[..image.len()].copy_from_slice(&image);
        Ok(())
    }

    /// Load a raw binary at a byte offset into flash.
    pub fn load_bin(&mut self, bytes: &[u8], offset: u64) -> Result<(), LoadError> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start.checked_add(bytes.len());
        match end {
            Some(end) if end <= FLASH_SIZE => {
                self.flash_mut()[start..end].copy_from_slice(bytes);
                Ok(())
            }
            _ => Err(LoadError::BinaryOutOfRange {
                offset,
                length: bytes.len(),
            }),
        }
    }

    /// Load either format: Intel HEX is detected from its leading `:`, a raw
    /// binary is placed at `offset`.
    pub fn load_image(&mut self, bytes: &[u8], offset: u64) -> Result<(), LoadError> {
        if hex::looks_like_hex(bytes) {
            let text = core::str::from_utf8(bytes)
                .map_err(|_| LoadError::Hex(HexError::NotHex { line: 1 }))?;
            self.load_hex(text)
        } else {
            self.load_bin(bytes, offset)
        }
    }

    /// Point the core at the reset vector, replacing the core and peripheral
    /// state. Flash survives.
    pub fn boot(&mut self) {
        self.reset();
    }

    /// Execute one instruction, or service a pending interrupt.
    ///
    /// An interrupt is taken before the next instruction when `I` is set and a
    /// source is pending: the core pushes the return address, clears `I`, jumps
    /// to the vector's flash address and retires [`crate::peripherals::INTERRUPT_CYCLES`]
    /// cycles. A sleeping core wakes on a pending interrupt; while it sleeps
    /// with an interrupt enabled the peripheral clock still advances one cycle
    /// per call, so a timer can be the wake-up source.
    pub fn step(&mut self) -> Result<u8, Fault> {
        if self.cpu.halted() {
            return Ok(0);
        }
        if self.cpu.sleeping() {
            return self.step_sleeping();
        }
        if self.cpu.interrupts_enabled()
            && let Some(serviced) = self.service_interrupt()?
        {
            return Ok(serviced);
        }
        match self.cpu.step(&mut self.data) {
            Ok(cycles) => {
                self.data.periph.advance(cycles);
                Ok(cycles)
            }
            Err(fault) => {
                self.fault = Some(fault);
                Err(fault)
            }
        }
    }

    fn step_sleeping(&mut self) -> Result<u8, Fault> {
        if !self.cpu.interrupts_enabled() {
            return Ok(0);
        }
        if let Some(serviced) = self.service_interrupt()? {
            return Ok(serviced);
        }
        // No interrupt yet: let the peripherals see one idle cycle so a timer
        // or watchdog can raise one.
        self.cpu.idle_cycle();
        self.data.periph.advance(1);
        Ok(1)
    }

    fn service_interrupt(&mut self) -> Result<Option<u8>, Fault> {
        let Some(vector) = self.data.periph.pending_interrupt() else {
            return Ok(None);
        };
        match self.cpu.take_interrupt(vector, &mut self.data) {
            Ok(cycles) => {
                self.data.periph.acknowledge(vector);
                self.data.periph.advance(cycles);
                Ok(Some(cycles))
            }
            Err(fault) => {
                self.fault = Some(fault);
                Err(fault)
            }
        }
    }

    /// Whether the core is asleep with no enabled interrupt able to wake it.
    pub fn is_idle(&self) -> bool {
        self.cpu.sleeping()
            && !(self.cpu.interrupts_enabled() && self.data.periph.any_interrupt_enabled())
    }

    /// Run for at most `budget_cycles`.
    ///
    /// The budget is checked between instructions, so the last instruction may
    /// run the total slightly past it. A sleeping core returns [`Stop::Idle`]
    /// once no enabled interrupt could wake it; an unknown opcode or an
    /// out-of-range access returns [`Stop::Trap`] with the detail in
    /// [`Machine::fault`].
    pub fn run(&mut self, budget_cycles: u64) -> Stop {
        let start = self.cpu.cycles();
        loop {
            if self.cpu.halted() {
                return if self.cpu.fault().is_some() {
                    Stop::Trap
                } else {
                    Stop::Halted
                };
            }
            if self.is_idle() {
                return Stop::Idle;
            }
            if self.cpu.cycles() - start >= budget_cycles {
                return Stop::Budget;
            }
            if let Err(_fault) = self.step() {
                return Stop::Trap;
            }
        }
    }

    /// Drive an Arduino digital pin from the host.
    pub fn set_input(&mut self, pin: u8, high: bool) {
        self.data.periph.set_input(pin, high);
    }

    /// Set the voltage the ADC reads on one channel, in microvolts.
    ///
    /// The board's analog solver works in volts; multiply by `1_000_000` when
    /// injecting. `MUX = 0..=7` are the Arduino `A0..A7` channels (A6/A7 exist
    /// on the SMD package), `MUX = 14` reads the 1.1 V bandgap and `15` reads
    /// ground.
    pub fn set_analog_input(&mut self, channel: u8, microvolts: u32) {
        self.data.periph.set_analog_input(channel, microvolts);
    }

    /// Set the external/AVcc ADC reference, in microvolts. The default is 5 V.
    pub fn set_analog_reference(&mut self, microvolts: u32) {
        self.data.periph.adc.set_reference(microvolts);
    }

    /// Queue a byte on USART0's receive path.
    pub fn receive_byte(&mut self, byte: u8) {
        self.data.periph.receive(byte);
    }

    /// Queue host bytes for the firmware's USART0 receive path.
    pub fn write_console(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.data.periph.receive(*byte);
        }
    }

    /// Bytes the firmware wrote to `UDR0`, drained.
    pub fn take_console(&mut self) -> Vec<u8> {
        self.data.periph.take_console()
    }

    /// Output edges since the last call.
    pub fn take_outputs(&mut self) -> Vec<(u8, bool)> {
        self.data.periph.take_outputs()
    }

    /// The duty each compare-output pin is programmed to, as the fraction of
    /// its period it spends high.
    pub fn pwm_duties(&self) -> Vec<(u8, f64)> {
        self.data.periph.pwm_duties()
    }

    /// Install a callback for GPIO output edges.
    pub fn set_gpio_callback(&mut self, callback: Box<dyn FnMut(u8, bool)>) {
        self.data.periph.set_gpio_callback(callback);
    }

    /// Install the SPI transfer callback:
    /// `spi_transfer(cs_pin, tx) -> rx`.
    pub fn set_spi_callback(&mut self, callback: SpiTransfer) {
        self.data.periph.spi.set_callback(callback);
    }

    /// The Arduino digital pin reported to the SPI callback as chip select.
    /// Defaults to D10, the Uno's SS pin.
    pub fn set_spi_cs_pin(&mut self, pin: u8) {
        self.data.periph.spi.set_cs_pin(pin);
    }

    /// Install the I²C master-write callback: `i2c_write(address, bytes)`.
    pub fn set_i2c_write_callback(&mut self, callback: I2cWrite) {
        self.data.periph.twi.set_write_callback(callback);
    }

    /// Install the I²C master-read callback: `i2c_read(address, len) -> byte`.
    pub fn set_i2c_read_callback(&mut self, callback: I2cRead) {
        self.data.periph.twi.set_read_callback(callback);
    }

    /// Install both I²C callbacks in one call.
    pub fn set_i2c_callbacks(&mut self, write: I2cWrite, read: I2cRead) {
        self.data.periph.twi.set_write_callback(write);
        self.data.periph.twi.set_read_callback(read);
    }

    /// Program counter in words.
    pub fn pc(&self) -> u16 {
        self.cpu.pc()
    }

    /// Program counter as a byte address, which is what a HEX record uses.
    pub fn pc_bytes(&self) -> u32 {
        self.cpu.pc_bytes()
    }

    /// Flash size in instruction words.
    pub const fn flash_words() -> usize {
        FLASH_WORDS
    }
}
