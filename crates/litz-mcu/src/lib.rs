//! Building blocks shared by every chip family.
//!
//! A chip crate (`litz-esp`, later `litz-avr`, `litz-rp`) is expected to be a
//! thin composition of these pieces: a memory map, a clock tree, an interrupt
//! controller, an ISA core and a set of peripherals. None of those know about
//! each other's concrete types.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use litz_core::{TICKS_PER_SEC, Tick};

/// One contiguous span of the CPU address space.
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub base: u32,
    pub len: u32,
    pub name: &'static str,
}

impl MemoryRegion {
    pub const fn new(name: &'static str, base: u32, len: u32) -> Self {
        Self { name, base, len }
    }

    pub const fn contains(&self, addr: u32) -> bool {
        addr >= self.base && addr - self.base < self.len
    }
}

/// Ordered table of regions. Lookup is a linear scan: chips have tens of
/// regions, not thousands, and a sorted `Vec` keeps the map deterministic
/// without a hash seed.
#[derive(Default)]
pub struct MemoryMap {
    regions: Vec<MemoryRegion>,
}

impl MemoryMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, region: MemoryRegion) {
        self.regions.push(region);
        self.regions.sort_by_key(|r| r.base);
    }

    pub fn find(&self, addr: u32) -> Option<&MemoryRegion> {
        self.regions.iter().find(|r| r.contains(addr))
    }
}

/// A clock whose period is not a whole number of nanoseconds.
///
/// A 160 MHz core is one cycle per 6.25 ns. Rounding each cycle down would
/// lose 0.25 ns per cycle — hours of drift over a long run — so the remainder
/// is carried: this is the same fixed-point divider the hardware uses.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    hz: u64,
    remainder: u128,
}

impl Clock {
    pub const fn new(hz: u64) -> Self {
        Self { hz, remainder: 0 }
    }

    pub const fn hz(&self) -> u64 {
        self.hz
    }

    /// Advance `cycles` cycles and return how many whole ticks (ns) that took,
    /// carrying the fractional part into the next call.
    pub fn advance(&mut self, cycles: u64) -> Tick {
        let total = self.remainder + u128::from(cycles) * u128::from(TICKS_PER_SEC);
        self.remainder = total % u128::from(self.hz);
        (total / u128::from(self.hz)) as Tick
    }
}

/// What a CPU core needs from the rest of the chip.
pub trait Core {
    /// Execute one instruction. Returns cycles consumed.
    fn step(&mut self) -> u32;
    fn reset(&mut self);
}

/// IRQ lines, in chip-specific order. The chip crate defines the numbering;
/// this trait only fixes the handshake.
pub trait InterruptController {
    fn set_pending(&mut self, irq: u16, pending: bool);
    fn pending(&self) -> Option<u16>;
    fn ack(&mut self, irq: u16);
}

/// ESP32-family chips.
///
/// The concrete implementations live in `litz-esp`; this module is the seam the
/// umbrella crate re-exports as `litz::esp`, so a user never has to know which
/// crate a chip family landed in.
pub mod esp {}
