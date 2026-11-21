//! The world around the core: RAM, the GPIO block, UART0, and the few
//! registers the repository's fixture firmware touches.
//!
//! Addresses are the ESP32-C3's, because the firmware is position-dependent
//! and the point of this chip is to run that firmware unchanged. Anything not
//! mapped here is a trap, which is how a firmware that reaches for a
//! peripheral this reference does not model fails loudly instead of reading
//! zeroes forever.

use crate::cpu::{Cpu, Memory, Trap};
use std::collections::{BTreeSet, VecDeque};

/// IRAM, where `litz-fixture` links its images.
pub const RAM_BASE: u32 = 0x4038_0000;
pub const RAM_LEN: u32 = 0x0010_0000;
/// DRAM, below IRAM; the fixtures point `sp` here.
pub const DRAM_BASE: u32 = 0x3FC8_0000;
pub const DRAM_LEN: u32 = 0x0004_0000;

const GPIO_BASE: u32 = 0x6000_4000;
const IO_MUX_BASE: u32 = 0x6000_9000;
const UART0_BASE: u32 = 0x6000_0000;
const ADC_BASE: u32 = 0x6004_0000;
const BLOCK: u32 = 0x1000;

/// Console bytes are handed over in batches of this many; one host call per
/// byte would put the component boundary in the hot loop of every print.
const CONSOLE_BATCH: usize = 64;

pub struct Machine {
    pub cpu: Cpu,
    ram: Vec<u8>,
    dram: Vec<u8>,
    /// GPIO output latch and its enable mask.
    out: u32,
    enable: u32,
    /// Levels the host is holding on pads (a set bit is high).
    inputs: u32,
    /// Pads whose drive changed since the last `take_outputs`.
    dirty: BTreeSet<u8>,
    uart_rx: VecDeque<u8>,
    uart_tx: Vec<u8>,
    /// Bytes the chip produced and has not handed to the host yet.
    pub console: Vec<u8>,
    /// Entry point of the image last loaded, for `boot`.
    entry: Option<u32>,
    pub halted: Option<String>,
    pub cycles: u64,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Self {
            cpu: Cpu::new(),
            ram: vec![0; RAM_LEN as usize],
            dram: vec![0; DRAM_LEN as usize],
            out: 0,
            enable: 0,
            inputs: 0,
            dirty: BTreeSet::new(),
            uart_rx: VecDeque::new(),
            uart_tx: Vec::new(),
            console: Vec::new(),
            entry: None,
            halted: None,
            cycles: 0,
        }
    }

    /// Parse an ESP application image and copy its segments into memory,
    /// returning the entry point. The format is the one `litz-fixture` writes
    /// and `esp32sim`'s `boot_app` reads: `0xE9`, a segment count, the entry
    /// word, 16 reserved bytes, then `addr`, `len`, data per segment.
    pub fn load(&mut self, image: &[u8]) -> Result<u32, String> {
        if image.len() < 24 {
            return Err(format!(
                "image is {} bytes; the header alone is 24",
                image.len()
            ));
        }
        if image[0] != 0xE9 {
            return Err(format!(
                "not an ESP image: first byte {:#04x}, expected 0xe9",
                image[0]
            ));
        }
        let entry = u32::from_le_bytes(image[4..8].try_into().expect("four bytes"));
        let count = image[1] as usize;
        let mut at = 24;
        for index in 0..count {
            if at + 8 > image.len() {
                return Err(format!("segment {index} starts past the end of the image"));
            }
            let addr = u32::from_le_bytes(image[at..at + 4].try_into().expect("four bytes"));
            let len =
                u32::from_le_bytes(image[at + 4..at + 8].try_into().expect("four bytes")) as usize;
            at += 8;
            if at + len > image.len() {
                return Err(format!("segment {index} runs past the end of the image"));
            }
            for (i, byte) in image[at..at + len].iter().enumerate() {
                let target = addr.wrapping_add(i as u32);
                if (RAM_BASE..RAM_BASE + RAM_LEN).contains(&target) {
                    self.ram[(target - RAM_BASE) as usize] = *byte;
                } else if (DRAM_BASE..DRAM_BASE + DRAM_LEN).contains(&target) {
                    self.dram[(target - DRAM_BASE) as usize] = *byte;
                } else {
                    return Err(format!(
                        "segment {index}: {len} bytes at {addr:#010x} is not memory"
                    ));
                }
            }
            at += len;
        }
        self.entry = Some(entry);
        Ok(entry)
    }

    /// Start the loaded image: point the core at its entry and return it.
    pub fn boot(&mut self) -> Option<u32> {
        let entry = self.entry?;
        self.cpu.pc = entry;
        Some(entry)
    }

    pub fn reset(&mut self) {
        self.cpu = Cpu::new();
        self.out = 0;
        self.enable = 0;
        self.dirty.clear();
        self.uart_rx.clear();
        self.halted = None;
        // A reset keeps the firmware: that is what a reset button does.
    }

    pub fn set_input(&mut self, pin: u8, high: bool) {
        if pin >= 32 {
            return;
        }
        if high {
            self.inputs |= 1 << pin;
        } else {
            self.inputs &= !(1 << pin);
        }
    }

    pub fn push_console(&mut self, bytes: &[u8]) {
        self.uart_rx.extend(bytes.iter().copied());
    }

    /// Pads whose drive changed since the last call, as `(pin, high, enabled)`.
    pub fn take_outputs(&mut self) -> Vec<(u8, bool, bool)> {
        let pins: Vec<u8> = self.dirty.iter().copied().collect();
        self.dirty.clear();
        pins.into_iter()
            .map(|pin| {
                let bit = 1u32 << pin;
                (pin, self.out & bit != 0, self.enable & bit != 0)
            })
            .collect()
    }

    /// Run at most `budget` instructions.
    pub fn run(&mut self, budget: u64) -> u64 {
        let mut done = 0;
        while done < budget {
            // The core and the machine are separate owners of one address
            // space, so the core is stepped outside `self` and put back.
            let mut cpu = std::mem::take(&mut self.cpu);
            let result = cpu.step(self);
            self.cpu = cpu;
            if let Err(trap) = result {
                let pc = self.cpu.pc;
                self.halted = Some(trap.describe(pc));
                break;
            }
            done += 1;
        }
        self.cycles = self.cycles.saturating_add(done);
        // Whatever was printed is handed over now: the host reads the console
        // after every call, and a partial line must not wait for a batch.
        self.flush_console(true);
        done
    }

    fn flush_console(&mut self, force: bool) {
        if self.uart_tx.len() >= CONSOLE_BATCH || (force && !self.uart_tx.is_empty()) {
            let bytes = std::mem::take(&mut self.uart_tx);
            self.console.extend_from_slice(&bytes);
        }
    }

    /// A whole-register GPIO write. Word-granular on purpose: `W1TS` and
    /// `W1TC` mean nothing when split into four byte stores.
    fn gpio_write(&mut self, off: u32, value: u32) {
        let before_out = self.out;
        let before_enable = self.enable;
        match off {
            0x04 => self.out = value,
            0x08 => self.out |= value,
            0x0c => self.out &= !value,
            0x20 => self.enable = value,
            0x24 => self.enable |= value,
            0x28 => self.enable &= !value,
            _ => return,
        }
        let moved = (self.out ^ before_out) | (self.enable ^ before_enable);
        for pin in 0..32u8 {
            if moved & (1u32 << pin) != 0 {
                self.dirty.insert(pin);
            }
        }
    }

    fn gpio_read(&self, off: u32) -> u32 {
        match off {
            0x04 => self.out,
            0x20 => self.enable,
            0x3c => self.inputs,
            _ => 0,
        }
    }

    fn in_ram(addr: u32) -> Option<(bool, u32)> {
        if (RAM_BASE..RAM_BASE + RAM_LEN).contains(&addr) {
            Some((true, addr - RAM_BASE))
        } else if (DRAM_BASE..DRAM_BASE + DRAM_LEN).contains(&addr) {
            Some((false, addr - DRAM_BASE))
        } else {
            None
        }
    }
}

impl Memory for Machine {
    fn read8(&mut self, addr: u32) -> Result<u8, Trap> {
        if let Some((iram, offset)) = Self::in_ram(addr) {
            let memory = if iram { &self.ram } else { &self.dram };
            return Ok(memory[offset as usize]);
        }
        if (GPIO_BASE..GPIO_BASE + BLOCK).contains(&addr) {
            let word = self.gpio_read((addr - GPIO_BASE) & !3);
            return Ok((word >> (8 * (addr & 3))) as u8);
        }
        if (UART0_BASE..UART0_BASE + BLOCK).contains(&addr) {
            return Ok(match addr - UART0_BASE {
                0x00 => self.uart_rx.pop_front().unwrap_or(0),
                // STATUS: receive count in the low ten bits, the idle line
                // levels in the top bits, as the C3 reports them.
                0x1c => (0xe000_c000 | (self.uart_rx.len() as u32 & 0x3ff)) as u8,
                _ => 0,
            });
        }
        if (IO_MUX_BASE..IO_MUX_BASE + BLOCK).contains(&addr)
            || (ADC_BASE..ADC_BASE + BLOCK).contains(&addr)
        {
            return Ok(0);
        }
        Err(Trap::Bus { addr, len: 1 })
    }

    fn write8(&mut self, addr: u32, value: u8) -> Result<(), Trap> {
        self.write32(addr & !3, u32::from(value) << (8 * (addr & 3)))
    }

    fn read32(&mut self, addr: u32) -> Result<u32, Trap> {
        if let Some((iram, offset)) = Self::in_ram(addr) {
            let memory = if iram { &self.ram } else { &self.dram };
            let at = offset as usize;
            let bytes = [memory[at], memory[at + 1], memory[at + 2], memory[at + 3]];
            return Ok(u32::from_le_bytes(bytes));
        }
        if (GPIO_BASE..GPIO_BASE + BLOCK).contains(&addr) {
            return Ok(self.gpio_read(addr - GPIO_BASE));
        }
        let mut value = 0u32;
        for i in 0..4 {
            value |= u32::from(self.read8(addr.wrapping_add(i))?) << (8 * i);
        }
        Ok(value)
    }

    fn write32(&mut self, addr: u32, value: u32) -> Result<(), Trap> {
        if let Some((iram, offset)) = Self::in_ram(addr) {
            let memory = if iram { &mut self.ram } else { &mut self.dram };
            let at = offset as usize;
            memory[at..at + 4].copy_from_slice(&value.to_le_bytes());
            return Ok(());
        }
        if (GPIO_BASE..GPIO_BASE + BLOCK).contains(&addr) {
            self.gpio_write(addr - GPIO_BASE, value);
            return Ok(());
        }
        if (UART0_BASE..UART0_BASE + BLOCK).contains(&addr) {
            // A store to the FIFO register transmits its low byte; every other
            // UART register is configuration this reference does not model.
            if addr - UART0_BASE < 4 {
                self.uart_tx.push(value as u8);
                self.flush_console(false);
            }
            return Ok(());
        }
        if (IO_MUX_BASE..IO_MUX_BASE + BLOCK).contains(&addr)
            || (ADC_BASE..ADC_BASE + BLOCK).contains(&addr)
        {
            return Ok(());
        }
        Err(Trap::Bus { addr, len: 4 })
    }

    fn tick(&mut self, _pc: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_latch_change_is_reported_once_per_pin() {
        let mut machine = Machine::new();
        machine.gpio_write(0x24, 1 << 8); // enable GPIO8
        machine.gpio_write(0x08, 1 << 8); // drive it high
        assert_eq!(machine.take_outputs(), vec![(8, true, true)]);
        assert!(
            machine.take_outputs().is_empty(),
            "unchanged pins are not repeated"
        );
        machine.gpio_write(0x0c, 1 << 8); // drive it low
        assert_eq!(machine.take_outputs(), vec![(8, false, true)]);
    }

    #[test]
    fn the_uart_status_carries_the_receive_count() {
        let mut machine = Machine::new();
        machine.push_console(b"hi");
        assert_eq!(
            u32::from(machine.read8(UART0_BASE + 0x1c).unwrap()) & 0x3ff,
            2
        );
        assert_eq!(machine.read8(UART0_BASE).unwrap(), b'h');
        assert_eq!(machine.read8(UART0_BASE).unwrap(), b'i');
        assert_eq!(
            u32::from(machine.read8(UART0_BASE + 0x1c).unwrap()) & 0x3ff,
            0
        );
    }

    #[test]
    fn a_word_store_to_the_fifo_transmits_its_low_byte() {
        let mut machine = Machine::new();
        machine.write32(UART0_BASE, u32::from(b'A')).unwrap();
        machine.write32(UART0_BASE, u32::from(b'B')).unwrap();
        machine.console.clear();
        for _ in 0..CONSOLE_BATCH {
            machine.write32(UART0_BASE, u32::from(b'x')).unwrap();
        }
        assert_eq!(machine.console.len(), CONSOLE_BATCH);
    }
}
