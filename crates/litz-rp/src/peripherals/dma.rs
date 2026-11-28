//! The 12-channel DMA engine.
//!
//! Once a channel's `EN` is set and `TRANS_COUNT` is non-zero it copies
//! `DATA_SIZE` elements from `READ_ADDR` to `WRITE_ADDR`, advancing either
//! address when its `INCR` bit is set. A channel that names a peripheral
//! trigger (`TREQ_SEL`) moves only while that trigger has an event waiting:
//! the bus answers [`DmaBus::dreq_ready`] from the block's own FIFO state, so
//! a PIO transmit channel fills the state machine's queue and then waits for
//! it to drain. `TREQ_SEL = 0x3f` is the unconditional trigger. Ring mode and
//! byte swapping are still not modelled.
//!
//! The bus is not part of this block: [`Dma::tick`] only marks time, and
//! [`Dma::service`] performs at most one element against a [`DmaBus`]. The
//! machine calls that once per advance, so a copy of N words takes N steps.

use super::RegisterFile;

/// Per-channel register offsets from the channel base. Every channel owns a
/// 0x40-byte window, and the 0x10/0x20/0x30 sub-windows are read/write aliases
/// of the same four registers.
pub mod reg {
    pub const READ_ADDR: u32 = 0x00;
    pub const WRITE_ADDR: u32 = 0x04;
    pub const TRANS_COUNT: u32 = 0x08;
    pub const CTRL: u32 = 0x0c;

    pub const INTR: u32 = 0x400;
    pub const INTE: u32 = 0x404;
    pub const INTS: u32 = 0x408;
    pub const INTF: u32 = 0x40c;
    pub const BUSY: u32 = 0x418;
    pub const CHAN_ABORT: u32 = 0x444;
    pub const MULTI_CHAN_TRIGGER: u32 = 0x450;
}

/// `CTRL` bits.
pub mod ctrl {
    pub const EN: u32 = 1 << 0;
    pub const HIGH_PRIORITY: u32 = 1 << 1;
    pub const DATA_SIZE_SHIFT: u32 = 2;
    pub const DATA_SIZE_MASK: u32 = 0x3 << DATA_SIZE_SHIFT;
    pub const INCR_READ: u32 = 1 << 4;
    pub const INCR_WRITE: u32 = 1 << 5;
    pub const INCR_READ_REV: u32 = 1 << 6;
    pub const INCR_WRITE_REV: u32 = 1 << 7;
    pub const RING_SIZE_MASK: u32 = 0x3 << 8;
    pub const RING_SEL: u32 = 1 << 10;
    pub const CHAIN_TO_SHIFT: u32 = 11;
    pub const CHAIN_TO_MASK: u32 = 0xf << CHAIN_TO_SHIFT;
    pub const TREQ_SEL_SHIFT: u32 = 15;
    /// Six bits wide: `0x3f` is the permanently-asserted trigger.
    pub const TREQ_SEL_MASK: u32 = 0x3f << TREQ_SEL_SHIFT;
    pub const IRQ_QUIET: u32 = 1 << 21;
    pub const BSWAP: u32 = 1 << 22;
    pub const SNIFF_EN: u32 = 1 << 23;
    /// Read-only status bits; a write cannot set them.
    pub const STATUS_MASK: u32 = (1 << 24) | (1 << 29) | (1 << 30) | (1 << 31);
    pub const BUSY: u32 = 1 << 24;
    pub const WRITE_ERROR: u32 = 1 << 29;
    pub const READ_ERROR: u32 = 1 << 30;
    pub const AHB_ERROR: u32 = 1 << 31;
}

pub const CHANNELS: usize = 12;

/// Element width selected by `CTRL.DATA_SIZE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Byte,
    Half,
    Word,
}

impl Size {
    fn from_ctrl(ctrl: u32) -> Size {
        match (ctrl & ctrl::DATA_SIZE_MASK) >> ctrl::DATA_SIZE_SHIFT {
            0 => Size::Byte,
            1 => Size::Half,
            _ => Size::Word,
        }
    }

    /// Bytes an increment moves the address.
    pub fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Half => 2,
            Size::Word => 4,
        }
    }
}

/// The memory a transfer reads and writes. Implemented by [`crate::bus::Memory`]
/// and by the small harness in this module's tests.
pub trait DmaBus {
    /// Read one element, or `None` when the address does not decode.
    fn dma_read(&mut self, addr: u32, size: Size) -> Option<u32>;
    /// Write one element; `false` when the address does not decode.
    fn dma_write(&mut self, addr: u32, value: u32, size: Size) -> bool;

    /// Whether DMA trigger `sel` has an event waiting right now. The default
    /// keeps an untriggered harness moving: every channel is always ready.
    fn dreq_ready(&mut self, _sel: u32) -> bool {
        true
    }
}

/// One element ready to move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Burst {
    pub channel: usize,
    pub source: u32,
    pub destination: u32,
    pub size: Size,
}

#[derive(Clone, Copy, Debug, Default)]
struct Channel {
    read_addr: u32,
    write_addr: u32,
    trans_count: u32,
    ctrl: u32,
    busy: bool,
}

#[derive(Debug)]
pub struct Dma {
    channels: [Channel; CHANNELS],
    intr: u32,
    inte: u32,
    forced: u32,
    regs: RegisterFile,
}

impl Default for Dma {
    fn default() -> Self {
        Self::new()
    }
}

impl Dma {
    pub fn new() -> Self {
        Dma {
            channels: [Channel::default(); CHANNELS],
            intr: 0,
            inte: 0,
            forced: 0,
            regs: RegisterFile::new(0x500 / 4),
        }
    }

    /// Split a channel-window offset into `(channel, register)`.
    fn channel(off: u32) -> Option<(usize, u32)> {
        if off >= 0x300 {
            return None;
        }
        let index = (off / 0x40) as usize;
        let register = off % 0x10;
        (index < CHANNELS).then_some((index, register))
    }

    /// Start a channel if its enable and count allow it. The channel is busy
    /// until its count drains; a trigger that has no event waiting only
    /// holds [`Dma::service`] off, the way the engine waits on DREQ.
    fn start(&mut self, index: usize) {
        let channel = self.channels[index];
        if channel.ctrl & ctrl::EN != 0 && channel.trans_count > 0 {
            self.channels[index].busy = true;
        }
    }

    fn abort(&mut self, index: usize) {
        if index < CHANNELS {
            self.channels[index].busy = false;
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if let Some((index, register)) = Self::channel(off) {
            let channel = self.channels[index];
            return match register {
                reg::READ_ADDR => channel.read_addr,
                reg::WRITE_ADDR => channel.write_addr,
                reg::TRANS_COUNT => channel.trans_count,
                reg::CTRL => channel.ctrl | if channel.busy { ctrl::BUSY } else { 0 },
                _ => 0,
            };
        }
        match off {
            reg::INTR => self.intr,
            reg::INTE => self.inte,
            reg::INTS => (self.intr | self.forced) & self.inte,
            reg::INTF => self.forced,
            reg::BUSY => self.busy_mask(),
            reg::CHAN_ABORT | reg::MULTI_CHAN_TRIGGER => 0,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        if let Some((index, register)) = Self::channel(off) {
            match register {
                reg::READ_ADDR => self.channels[index].read_addr = value,
                reg::WRITE_ADDR => self.channels[index].write_addr = value,
                reg::TRANS_COUNT => {
                    self.channels[index].trans_count = value;
                    self.start(index);
                }
                reg::CTRL => {
                    self.channels[index].ctrl = value & !ctrl::STATUS_MASK;
                    if value & ctrl::EN == 0 {
                        self.channels[index].busy = false;
                    } else {
                        self.start(index);
                    }
                }
                _ => {}
            }
            return;
        }
        match off {
            reg::INTR => self.intr &= !value,
            reg::INTE => self.inte = value & 0xfff,
            reg::INTF => self.forced = value & 0xfff,
            reg::INTS => {}
            reg::CHAN_ABORT => {
                for index in 0..CHANNELS {
                    if value & (1 << index) != 0 {
                        self.abort(index);
                    }
                }
            }
            reg::MULTI_CHAN_TRIGGER => {
                for index in 0..CHANNELS {
                    if value & (1 << index) != 0 {
                        self.start(index);
                    }
                }
            }
            off => self.regs.write(off, value),
        }
    }

    /// Time passes. Data movement happens in [`Dma::service`], which needs the
    /// bus; this hook exists so the machine can keep the block on the same
    /// tick schedule as the rest.
    pub fn tick(&mut self, _cycles: u64) {}

    pub fn irq(&self) -> bool {
        (self.intr | self.forced) & self.inte != 0
    }

    fn busy_mask(&self) -> u32 {
        let mut mask = 0;
        for (index, channel) in self.channels.iter().enumerate() {
            if channel.busy {
                mask |= 1 << index;
            }
        }
        mask
    }

    /// The next element that will move, lowest channel first.
    pub fn pending(&self) -> Option<Burst> {
        self.channels
            .iter()
            .enumerate()
            .find(|(_, channel)| channel.busy && channel.trans_count > 0)
            .map(|(index, channel)| Burst {
                channel: index,
                source: channel.read_addr,
                destination: channel.write_addr,
                size: Size::from_ctrl(channel.ctrl),
            })
    }

    /// The next element whose trigger is ready, lowest channel first.
    fn pending_ready(&mut self, bus: &mut impl DmaBus) -> Option<Burst> {
        for index in 0..CHANNELS {
            let channel = self.channels[index];
            if !channel.busy || channel.trans_count == 0 {
                continue;
            }
            let treq = (channel.ctrl & ctrl::TREQ_SEL_MASK) >> ctrl::TREQ_SEL_SHIFT;
            if treq != 0x3f && !bus.dreq_ready(treq) {
                continue;
            }
            return Some(Burst {
                channel: index,
                source: channel.read_addr,
                destination: channel.write_addr,
                size: Size::from_ctrl(channel.ctrl),
            });
        }
        None
    }

    /// Move one element through `bus`. Returns whether work was available.
    /// This is the "one burst per tick" step the machine calls.
    pub fn service(&mut self, bus: &mut impl DmaBus) -> bool {
        let Some(burst) = self.pending_ready(bus) else {
            return false;
        };
        let ok = match bus.dma_read(burst.source, burst.size) {
            Some(value) => bus.dma_write(burst.destination, value, burst.size),
            None => false,
        };
        self.complete(burst.channel, ok);
        true
    }

    /// Finish the element issued by [`Dma::pending`], or fail the channel.
    fn complete(&mut self, index: usize, ok: bool) {
        if !ok {
            self.channels[index].ctrl |= ctrl::READ_ERROR;
            self.channels[index].busy = false;
            return;
        }
        let (done, next) = {
            let channel = &mut self.channels[index];
            channel.trans_count = channel.trans_count.saturating_sub(1);
            let step = Size::from_ctrl(channel.ctrl).bytes();
            if channel.ctrl & ctrl::INCR_READ != 0 {
                channel.read_addr = channel.read_addr.wrapping_add(step);
            }
            if channel.ctrl & ctrl::INCR_WRITE != 0 {
                channel.write_addr = channel.write_addr.wrapping_add(step);
            }
            let next = ((channel.ctrl & ctrl::CHAIN_TO_MASK) >> ctrl::CHAIN_TO_SHIFT) as usize;
            (channel.trans_count == 0, next)
        };
        if done {
            self.channels[index].busy = false;
            self.intr |= 1 << index;

            // CHAIN_TO starts the next channel when this one drains.
            if next != index && next < CHANNELS {
                self.start(next);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Ram {
        bytes: Vec<u8>,
    }

    impl Ram {
        fn new() -> Self {
            Ram {
                bytes: vec![0; 0x400],
            }
        }

        fn word(&self, addr: usize) -> u32 {
            u32::from_le_bytes([
                self.bytes[addr],
                self.bytes[addr + 1],
                self.bytes[addr + 2],
                self.bytes[addr + 3],
            ])
        }
    }

    impl DmaBus for Ram {
        fn dma_read(&mut self, addr: u32, size: Size) -> Option<u32> {
            let addr = addr as usize & !3;
            let word = self.word(addr);
            Some(match size {
                Size::Byte => (word >> ((addr & 3) * 8)) & 0xff,
                Size::Half => word & 0xffff,
                Size::Word => word,
            })
        }

        fn dma_write(&mut self, addr: u32, value: u32, size: Size) -> bool {
            let addr = addr as usize & !3;
            let mut word = self.word(addr);
            match size {
                Size::Byte => {
                    let shift = (addr & 3) * 8;
                    word = (word & !(0xff << shift)) | ((value & 0xff) << shift);
                }
                Size::Half => word = (word & 0xffff_0000) | (value & 0xffff),
                Size::Word => word = value,
            }
            self.bytes[addr..addr + 4].copy_from_slice(&word.to_le_bytes());
            true
        }
    }

    fn channel(off: u32, index: usize) -> u32 {
        off + 0x40 * index as u32
    }

    #[test]
    fn memory_to_memory_copy() {
        let mut dma = Dma::new();
        let mut ram = Ram::new();
        for (index, word) in [0x1122_3344u32, 0x5566_7788, 0x99aa_bbcc, 0xddee_ff00]
            .iter()
            .enumerate()
        {
            ram.bytes[0x100 + index * 4..0x104 + index * 4].copy_from_slice(&word.to_le_bytes());
        }

        dma.write(reg::INTE, 1);
        dma.write(channel(reg::READ_ADDR, 0), 0x100);
        dma.write(channel(reg::WRITE_ADDR, 0), 0x200);
        dma.write(
            channel(reg::CTRL, 0),
            ctrl::EN
                | ctrl::INCR_READ
                | ctrl::INCR_WRITE
                | (2 << ctrl::DATA_SIZE_SHIFT)
                | (0x3f << ctrl::TREQ_SEL_SHIFT),
        );
        dma.write(channel(reg::TRANS_COUNT, 0), 4);
        assert_ne!(dma.read(channel(reg::CTRL, 0)) & ctrl::BUSY, 0);

        for _ in 0..4 {
            dma.tick(1);
            assert!(dma.service(&mut ram));
        }
        assert!(!dma.service(&mut ram));
        assert_eq!(dma.read(channel(reg::TRANS_COUNT, 0)), 0);
        assert_eq!(dma.read(channel(reg::CTRL, 0)) & ctrl::BUSY, 0);
        assert!(dma.irq());
        assert_ne!(dma.read(reg::INTS) & 1, 0);
        for index in 0..4 {
            assert_eq!(ram.word(0x200 + index * 4), ram.word(0x100 + index * 4));
        }
    }

    #[test]
    fn abort_stops_a_channel() {
        let mut dma = Dma::new();
        dma.write(channel(reg::READ_ADDR, 2), 0x10);
        dma.write(channel(reg::WRITE_ADDR, 2), 0x20);
        dma.write(
            channel(reg::CTRL, 2),
            ctrl::EN | ctrl::INCR_READ | ctrl::INCR_WRITE,
        );
        dma.write(channel(reg::TRANS_COUNT, 2), 8);
        assert_ne!(dma.read(reg::BUSY) & (1 << 2), 0);
        dma.write(reg::CHAN_ABORT, 1 << 2);
        assert_eq!(dma.read(reg::BUSY) & (1 << 2), 0);
        assert_eq!(dma.pending(), None);
    }

    #[test]
    fn interrupt_clear_and_force() {
        let mut dma = Dma::new();
        dma.write(reg::INTE, 0b11);
        dma.write(reg::INTF, 0b1);
        assert!(dma.irq());
        assert_eq!(dma.read(reg::INTS), 0b1);
        dma.write(reg::INTF, 0);
        assert!(!dma.irq());
        dma.write(reg::INTR, 0b10);
        assert!(!dma.irq());
    }
}
