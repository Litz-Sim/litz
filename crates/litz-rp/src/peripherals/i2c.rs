//! The DW_apb I2C master: one controller per instance, 7- and 10-bit
//! addressing, a 16-byte TX and RX FIFO and the full interrupt set.
//!
//! The controller talks to the board through two callbacks installed by the
//! chip adapter: a write phase and a read phase. A transaction is completed at
//! the point the hardware would hold `TX_ABRT`/`RXFLR`, so a polling firmware
//! sees the same handshake either way.
//!
//! # The transaction model
//!
//! The real block clocks `IC_DATA_CMD` bytes onto the bus one at a time. This
//! model keeps the FIFO as a staging buffer and settles a transaction at the
//! same moments the hardware does:
//!
//! * a byte written without `STOP` is queued in the TX FIFO;
//! * `STOP` on a write flushes the queue to the write callback and raises
//!   `STOP_DET`;
//! * a read command request is counted; `STOP` on the last one calls the read
//!   callback once for the whole run, so a device that serves a register read
//!   with an auto-incrementing cursor (an LM75, a 24C02) is replayed once, not
//!   once per byte;
//! * a repeated start — an explicit `RESTART`, a read that follows queued
//!   writes, or a new `IC_TAR` — first flushes the pending write phase, which
//!   is what lets a driver send a register pointer with `nostop` and then read.
//!
//! The board bridge installed by [`crate::chip`] replays the write phase before
//! the read itself, so a read callback only has to return `len` bytes from an
//! address.
//!
//! # What is approximated
//!
//! Bus timing, clock stretching and arbitration are not modelled: the
//! transaction completes on the register write that ends it. Clearing
//! `IC_ENABLE.ENABLE` does not flush a queued write, because drivers sandwich
//! `enable = 0; tar = addr; enable = 1` between a `nostop` write and its
//! repeated-start read; `ABORT` is the explicit flush. Slave mode is absent,
//! so `RX_DONE`, `RD_REQ` and `GEN_CALL` stay clear and their clear registers
//! are still readable. The write callback returns no acknowledgement, so a
//! device that does not answer cannot be told apart from one that does;
//! `TX_ABRT` with `ABRT_7B_ADDR_NOACK` and `MST_ON_HOLD` is raised when the
//! controller has no usable target (master disabled, or a reserved/absent
//! address in `IC_TAR`). A real NACK from a populated-looking address needs an
//! acknowledgement from the bus, which the `FnMut(u8, &[u8])` callback in
//! [`crate::chip`] does not carry.

use std::collections::VecDeque;

use super::RegisterFile;

/// Called when the master finishes a write phase.
pub type I2cWrite = Box<dyn FnMut(u8, &[u8])>;
/// Called when the master finishes a read phase: the address and byte count.
pub type I2cRead = Box<dyn FnMut(u8, usize) -> Vec<u8>>;

/// Register offsets from the I2C base, in datasheet order.
pub mod reg {
    pub const CON: u32 = 0x00;
    pub const TAR: u32 = 0x04;
    pub const SAR: u32 = 0x08;
    pub const HS_MADDR: u32 = 0x0c;
    pub const DATA_CMD: u32 = 0x10;
    pub const SS_SCL_HCNT: u32 = 0x14;
    pub const SS_SCL_LCNT: u32 = 0x18;
    pub const FS_SCL_HCNT: u32 = 0x1c;
    pub const FS_SCL_LCNT: u32 = 0x20;
    pub const HS_SCL_HCNT: u32 = 0x24;
    pub const HS_SCL_LCNT: u32 = 0x28;
    pub const INTR_STAT: u32 = 0x2c;
    pub const INTR_MASK: u32 = 0x30;
    pub const RAW_INTR_STAT: u32 = 0x34;
    pub const RX_TL: u32 = 0x38;
    pub const TX_TL: u32 = 0x3c;
    pub const CLR_INTR: u32 = 0x40;
    pub const CLR_RX_UNDER: u32 = 0x44;
    pub const CLR_RX_OVER: u32 = 0x48;
    pub const CLR_TX_OVER: u32 = 0x4c;
    pub const CLR_RD_REQ: u32 = 0x50;
    pub const CLR_TX_ABRT: u32 = 0x54;
    pub const CLR_RX_DONE: u32 = 0x58;
    pub const CLR_ACTIVITY: u32 = 0x5c;
    pub const CLR_STOP_DET: u32 = 0x60;
    pub const CLR_START_DET: u32 = 0x64;
    pub const CLR_GEN_CALL: u32 = 0x68;
    pub const ENABLE: u32 = 0x6c;
    pub const STATUS: u32 = 0x70;
    pub const TXFLR: u32 = 0x74;
    pub const RXFLR: u32 = 0x78;
    pub const SDA_HOLD: u32 = 0x7c;
    pub const TX_ABRT_SOURCE: u32 = 0x80;
    pub const SLV_DATA_NACK_ONLY: u32 = 0x84;
    pub const DMA_CR: u32 = 0x88;
    pub const DMA_TDLR: u32 = 0x8c;
    pub const DMA_RDLR: u32 = 0x90;
    pub const SDA_SETUP: u32 = 0x94;
    pub const ACK_GENERAL_CALL: u32 = 0x98;
    pub const ENABLE_STATUS: u32 = 0x9c;
    pub const FS_SPKLEN: u32 = 0xa0;
    pub const HS_SPKLEN: u32 = 0xa4;
    pub const CLR_RESTART_DET: u32 = 0xa8;
    pub const COMP_PARAM_1: u32 = 0xac;
    pub const COMP_VERSION: u32 = 0xb0;
    pub const COMP_TYPE: u32 = 0xb4;
}

/// `IC_CON` bits.
pub mod con {
    pub const MASTER_MODE: u32 = 1 << 0;
    /// Speed field bits `[2:1]`: `01` standard, `10` fast, `11` high speed.
    pub const SPEED_MASK: u32 = 0b11 << 1;
    pub const IC_10BITADDR_SLAVE: u32 = 1 << 3;
    pub const IC_10BITADDR_MASTER: u32 = 1 << 4;
    pub const RESTART_EN: u32 = 1 << 5;
    pub const SLAVE_DISABLE: u32 = 1 << 6;
    pub const STOP_DET_IFADDRESSED: u32 = 1 << 7;
    pub const TX_EMPTY_CTRL: u32 = 1 << 8;
    pub const RX_FIFO_FULL_HLD_CTRL: u32 = 1 << 9;
    pub const VALID: u32 = 0x3ff;
}

/// `IC_TAR` bits.
pub mod tar {
    pub const ADDR_MASK: u32 = 0x3ff;
    pub const GC_OR_START: u32 = 1 << 10;
    pub const SPECIAL: u32 = 1 << 11;
}

/// `IC_DATA_CMD` bits.
pub mod data_cmd {
    pub const DAT_MASK: u32 = 0xff;
    pub const READ: u32 = 1 << 8;
    pub const STOP: u32 = 1 << 9;
    pub const RESTART: u32 = 1 << 10;
}

/// Raw interrupt bits shared by `IC_INTR_STAT`, `IC_INTR_MASK` and
/// `IC_RAW_INTR_STAT`.
pub mod intr {
    pub const RX_UNDER: u32 = 1 << 0;
    pub const RX_OVER: u32 = 1 << 1;
    pub const RX_FULL: u32 = 1 << 2;
    pub const TX_OVER: u32 = 1 << 3;
    pub const TX_EMPTY: u32 = 1 << 4;
    pub const RD_REQ: u32 = 1 << 5;
    pub const TX_ABRT: u32 = 1 << 6;
    pub const RX_DONE: u32 = 1 << 7;
    pub const ACTIVITY: u32 = 1 << 8;
    pub const STOP_DET: u32 = 1 << 9;
    pub const START_DET: u32 = 1 << 10;
    pub const GEN_CALL: u32 = 1 << 11;
    pub const RESTART_DET: u32 = 1 << 12;
    pub const MST_ON_HOLD: u32 = 1 << 13;
    /// Every bit this model can raise.
    pub const VALID: u32 = (1 << 14) - 1;
}

/// `IC_TX_ABRT_SOURCE` bits.
pub mod abrt {
    pub const ABRT_7B_ADDR_NOACK: u32 = 1 << 0;
    pub const ABRT_10ADDR1_NOACK: u32 = 1 << 1;
    pub const ABRT_10ADDR2_NOACK: u32 = 1 << 2;
    pub const ABRT_TXDATA_NOACK: u32 = 1 << 3;
    pub const ABRT_MASTER_DIS: u32 = 1 << 11;
    pub const ABRT_USER_ABRT: u32 = 1 << 16;
    /// `TX_FLUSH_CNT` occupies bits 22:17.
    pub const TX_FLUSH_SHIFT: u32 = 17;
}

/// `IC_STATUS` bits.
pub mod status {
    pub const ACTIVITY: u32 = 1 << 0;
    pub const TFNF: u32 = 1 << 1;
    pub const TFE: u32 = 1 << 2;
    pub const RFNE: u32 = 1 << 3;
    pub const RFF: u32 = 1 << 4;
    pub const MST_ACTIVITY: u32 = 1 << 5;
    pub const SLV_ACTIVITY: u32 = 1 << 6;
}

/// `IC_ENABLE` bits.
pub mod enable {
    pub const ENABLE: u32 = 1 << 0;
    pub const ABORT: u32 = 1 << 1;
    pub const TX_CMD_BLOCK: u32 = 1 << 2;
}

/// The RX and TX FIFOs hold 16 entries, as the block does.
pub const FIFO_DEPTH: usize = 16;

/// The component type the RP2040's I2C reports.
const COMP_TYPE: u32 = 0x4457_0140;
/// The component parameter register this model advertises. Its depth and
/// width fields are informational; firmware does not derive behaviour from it.
const COMP_PARAM_1: u32 = 0x00e0_0383;
/// The design version the block reads back.
const COMP_VERSION: u32 = 0x3132_302a;

pub struct I2c {
    regs: RegisterFile,
    /// The board's write phase, installed by the chip adapter.
    write_cb: Option<I2cWrite>,
    /// The board's read phase.
    read_cb: Option<I2cRead>,
    /// Write bytes queued but not yet handed to the board.
    tx: VecDeque<u8>,
    /// Bytes the last completed read left for the guest.
    rx: VecDeque<u8>,
    /// Read commands issued since the last transaction boundary.
    read_pending: usize,
    /// Sticky interrupt bits: everything except the level-sensitive
    /// `RX_FULL`, `TX_EMPTY` and `ACTIVITY`, which are computed on read.
    sticky: u32,
    /// `IC_TX_ABRT_SOURCE` for the most recent abort.
    abort_source: u32,
    /// A transfer is between START and STOP.
    busy: bool,
}

impl Default for I2c {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for I2c {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("I2c")
            .field("callback", &self.write_cb.is_some())
            .field("tx", &self.tx.len())
            .field("rx", &self.rx.len())
            .finish()
    }
}

impl I2c {
    pub fn new() -> Self {
        I2c {
            regs: RegisterFile::new(0x100 / 4),
            write_cb: None,
            read_cb: None,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            read_pending: 0,
            sticky: 0,
            abort_source: 0,
            busy: false,
        }
    }

    pub fn set_callbacks(&mut self, write: I2cWrite, read: I2cRead) {
        self.write_cb = Some(write);
        self.read_cb = Some(read);
    }

    /// Hand the installed callbacks back, so a machine reset can carry the
    /// board bridge across the rebuild that replaces every block.
    pub fn take_callbacks(&mut self) -> (Option<I2cWrite>, Option<I2cRead>) {
        (self.write_cb.take(), self.read_cb.take())
    }

    fn call_write(&mut self, address: u8, bytes: &[u8]) {
        if let Some(callback) = self.write_cb.as_mut() {
            callback(address, bytes);
        }
    }

    fn call_read(&mut self, address: u8, len: usize) -> Vec<u8> {
        match self.read_cb.as_mut() {
            Some(callback) => callback(address, len),
            None => vec![0xff; len],
        }
    }

    fn enabled(&self) -> bool {
        self.regs.read(reg::ENABLE) & enable::ENABLE != 0
    }

    fn master_mode(&self) -> bool {
        self.regs.read(reg::CON) & con::MASTER_MODE != 0
    }

    fn intr_mask(&self) -> u32 {
        self.regs.read(reg::INTR_MASK) & intr::VALID
    }

    fn rx_threshold(&self) -> usize {
        (self.regs.read(reg::RX_TL) & 0x0f) as usize
    }

    fn tx_threshold(&self) -> usize {
        (self.regs.read(reg::TX_TL) & 0x0f) as usize
    }

    /// The 7-bit address the next transfer addresses, if there is one.
    ///
    /// The board's buses are 7-bit only, and addresses 0x00 through 0x07 are
    /// reserved, so a `TAR` that names one of them has no target at all — the
    /// same conclusion the hardware reaches when every address byte is
    /// unacknowledged.
    ///
    /// `IC_ENABLE.ENABLE` is deliberately not consulted: a driver clears it
    /// between a `nostop` write and the repeated-start read, and the queued
    /// write still has to reach the board at that boundary. New data commands
    /// are gated on `ENABLE` in [`I2c::data_cmd_write`].
    fn target(&self) -> Option<u8> {
        if !self.master_mode() {
            return None;
        }
        if self.regs.read(reg::CON) & con::IC_10BITADDR_MASTER != 0 {
            // Ten-bit addressing is not carried on the board buses.
            return None;
        }
        let tar = self.regs.read(reg::TAR);
        let address = (tar & 0x7f) as u8;
        (address >= 0x08).then_some(address)
    }

    /// Level-sensitive interrupt bits, refreshed from the FIFO state.
    fn raw_status(&self) -> u32 {
        let mut raw = self.sticky;
        if self.enabled() {
            if self.rx.len() > self.rx_threshold() {
                raw |= intr::RX_FULL;
            }
            if self.tx.len() <= self.tx_threshold() {
                raw |= intr::TX_EMPTY;
            }
        }
        if self.busy {
            raw |= intr::ACTIVITY;
        }
        raw
    }

    /// Mark the start of a transfer. A first START is sticky until cleared; a
    /// repeated START also raises `RESTART_DET`.
    fn begin(&mut self) {
        if !self.busy {
            self.sticky |= intr::START_DET;
        }
        self.busy = true;
    }

    /// Abort the transfer in flight. The TX FIFO is flushed and the number of
    /// bytes discarded is reported in `TX_FLUSH_CNT`, as the block does.
    fn abort(&mut self, source: u32, hold_master: bool) {
        let flushed = self.tx.len() as u32;
        self.sticky |= intr::TX_ABRT;
        if hold_master {
            self.sticky |= intr::MST_ON_HOLD;
        }
        self.abort_source = source | (flushed << abrt::TX_FLUSH_SHIFT);
        self.tx.clear();
        self.read_pending = 0;
        self.busy = false;
    }

    /// Complete the queued write phase. `stopped` says whether the byte that
    /// ended it carried `STOP` (as opposed to a repeated START).
    fn flush_write(&mut self, stopped: bool) {
        if self.tx.is_empty() {
            if stopped {
                self.busy = false;
                self.sticky |= intr::STOP_DET;
            }
            return;
        }
        let Some(address) = self.target() else {
            self.abort(abrt::ABRT_7B_ADDR_NOACK, true);
            return;
        };
        let bytes: Vec<u8> = self.tx.drain(..).collect();
        self.call_write(address, &bytes);
        self.busy = false;
        if stopped {
            self.sticky |= intr::STOP_DET;
        }
    }

    /// Complete the queued read phase: one callback for the whole run, so a
    /// device's register cursor advances once.
    fn flush_read(&mut self, stopped: bool) {
        let len = self.read_pending;
        if len == 0 {
            return;
        }
        let Some(address) = self.target() else {
            self.abort(abrt::ABRT_7B_ADDR_NOACK, true);
            return;
        };
        // With no far end, the bus floats high.
        let mut bytes = self.call_read(address, len);
        bytes.resize(len, 0xff);
        for byte in bytes.into_iter().take(len) {
            if self.rx.len() < FIFO_DEPTH {
                self.rx.push_back(byte);
            } else {
                // The real block holds SCL low; the instant model drops the
                // surplus and reports the overrun.
                self.sticky |= intr::RX_OVER;
            }
        }
        self.read_pending = 0;
        self.busy = false;
        if stopped {
            self.sticky |= intr::STOP_DET;
        }
    }

    fn data_cmd_write(&mut self, value: u32) {
        if !self.enabled() {
            return;
        }
        let byte = (value & data_cmd::DAT_MASK) as u8;
        let read = value & data_cmd::READ != 0;
        let stopped = value & data_cmd::STOP != 0;
        let restarted = value & data_cmd::RESTART != 0;

        if read {
            // A read phase starts with a repeated START. Any write bytes
            // still queued were addressed to the same target and must reach
            // the board before the read replays them.
            if restarted {
                self.sticky |= intr::RESTART_DET;
            }
            if restarted || !self.tx.is_empty() {
                self.flush_write(false);
            }
            self.begin();
            self.read_pending += 1;
            if stopped {
                self.flush_read(true);
            }
            return;
        }

        if restarted {
            if self.read_pending > 0 {
                self.flush_read(false);
            }
            self.flush_write(false);
            self.sticky |= intr::RESTART_DET;
        }
        self.begin();
        if self.tx.len() >= FIFO_DEPTH {
            self.sticky |= intr::TX_OVER;
            return;
        }
        self.tx.push_back(byte);
        if stopped {
            self.flush_write(true);
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::DATA_CMD => {
                let byte = self.rx.pop_front();
                if byte.is_none() {
                    // A read of an empty RX FIFO underruns the block.
                    self.sticky |= intr::RX_UNDER;
                }
                u32::from(byte.unwrap_or(0))
            }
            reg::INTR_STAT => self.raw_status() & self.intr_mask(),
            reg::RAW_INTR_STAT => self.raw_status(),
            reg::STATUS => {
                let mut value = 0;
                if self.busy {
                    value |= status::ACTIVITY | status::MST_ACTIVITY;
                }
                if self.tx.len() < FIFO_DEPTH {
                    value |= status::TFNF;
                }
                if self.tx.is_empty() {
                    value |= status::TFE;
                }
                if !self.rx.is_empty() {
                    value |= status::RFNE;
                }
                if self.rx.len() >= FIFO_DEPTH {
                    value |= status::RFF;
                }
                value
            }
            reg::TXFLR => self.tx.len() as u32,
            reg::RXFLR => self.rx.len() as u32,
            reg::TX_ABRT_SOURCE => self.abort_source,
            reg::ENABLE_STATUS => {
                let mut value = u32::from(self.enabled());
                if self.busy {
                    // Nothing can be disabled while a transfer is in flight
                    // in this model, so SLV_DISABLED_WHILE_BUSY stays clear.
                    value &= !(1 << 2);
                }
                value
            }
            reg::COMP_PARAM_1 => COMP_PARAM_1,
            reg::COMP_VERSION => COMP_VERSION,
            reg::COMP_TYPE => COMP_TYPE,
            reg::CLR_INTR => {
                self.sticky = 0;
                self.abort_source = 0;
                0
            }
            reg::CLR_RX_UNDER => {
                self.sticky &= !intr::RX_UNDER;
                0
            }
            reg::CLR_RX_OVER => {
                self.sticky &= !intr::RX_OVER;
                0
            }
            reg::CLR_TX_OVER => {
                self.sticky &= !intr::TX_OVER;
                0
            }
            reg::CLR_RD_REQ => {
                self.sticky &= !intr::RD_REQ;
                0
            }
            reg::CLR_TX_ABRT => {
                self.sticky &= !(intr::TX_ABRT | intr::MST_ON_HOLD);
                self.abort_source = 0;
                0
            }
            reg::CLR_RX_DONE => {
                self.sticky &= !intr::RX_DONE;
                0
            }
            reg::CLR_ACTIVITY => {
                // ACTIVITY is level-sensitive in this model; clear the sticky
                // copy so a read returns the current bus state.
                self.sticky &= !intr::ACTIVITY;
                0
            }
            reg::CLR_STOP_DET => {
                self.sticky &= !intr::STOP_DET;
                0
            }
            reg::CLR_START_DET => {
                self.sticky &= !intr::START_DET;
                0
            }
            reg::CLR_GEN_CALL => {
                self.sticky &= !intr::GEN_CALL;
                0
            }
            reg::CLR_RESTART_DET => {
                self.sticky &= !intr::RESTART_DET;
                0
            }
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CON => self.regs.write(reg::CON, value & con::VALID),
            reg::TAR => {
                // Programming a new target ends any half-finished write phase
                // at the address that was already loaded.
                if !self.tx.is_empty() {
                    self.flush_write(false);
                }
                self.regs.write(reg::TAR, value & 0xfff);
            }
            reg::SAR => self.regs.write(reg::SAR, value & tar::ADDR_MASK),
            reg::DATA_CMD => self.data_cmd_write(value),
            reg::INTR_MASK => self.regs.write(reg::INTR_MASK, value & intr::VALID),
            reg::RX_TL => self.regs.write(reg::RX_TL, value & 0x0f),
            reg::TX_TL => self.regs.write(reg::TX_TL, value & 0x0f),
            reg::ENABLE => {
                if value & enable::ABORT != 0 {
                    self.abort(abrt::ABRT_USER_ABRT, false);
                }
                // ABORT is self-clearing: it is a command, not a state bit.
                self.regs.write(reg::ENABLE, value & enable::ENABLE);
            }
            // Clear registers are read-to-clear, but accepting a write costs
            // nothing and some sequences use the same helper for both.
            reg::CLR_INTR => {
                self.sticky = 0;
                self.abort_source = 0;
            }
            reg::CLR_TX_ABRT => {
                self.sticky &= !(intr::TX_ABRT | intr::MST_ON_HOLD);
                self.abort_source = 0;
            }
            reg::CLR_STOP_DET => self.sticky &= !intr::STOP_DET,
            reg::CLR_START_DET => self.sticky &= !intr::START_DET,
            reg::CLR_RESTART_DET => self.sticky &= !intr::RESTART_DET,
            reg::CLR_RX_UNDER => self.sticky &= !intr::RX_UNDER,
            reg::CLR_RX_OVER => self.sticky &= !intr::RX_OVER,
            reg::CLR_TX_OVER => self.sticky &= !intr::TX_OVER,
            reg::CLR_RD_REQ => self.sticky &= !intr::RD_REQ,
            reg::CLR_RX_DONE => self.sticky &= !intr::RX_DONE,
            reg::CLR_GEN_CALL => self.sticky &= !intr::GEN_CALL,
            reg::STATUS | reg::TXFLR | reg::RXFLR | reg::TX_ABRT_SOURCE => {}
            off => self.regs.write(off, value),
        }
    }

    pub fn tick(&mut self, _cycles: u64) {
        // Transactions complete on the register write that ends them, so the
        // clock has no work to do.
    }

    pub fn irq(&self) -> bool {
        self.raw_status() & self.intr_mask() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn master() -> I2c {
        let mut i2c = I2c::new();
        i2c.write(
            reg::CON,
            con::MASTER_MODE | con::SLAVE_DISABLE | con::RESTART_EN,
        );
        i2c.write(reg::ENABLE, enable::ENABLE);
        i2c
    }

    type WriteLog = Rc<RefCell<Vec<(u8, Vec<u8>)>>>;

    fn record_write(i2c: &mut I2c) -> WriteLog {
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&log);
        i2c.set_callbacks(
            Box::new(move |address, bytes| sink.borrow_mut().push((address, bytes.to_vec()))),
            Box::new(|_, len| vec![0; len]),
        );
        log
    }

    #[test]
    fn a_write_transaction_reaches_the_callback_at_stop() {
        let mut i2c = master();
        let log = record_write(&mut i2c);
        i2c.write(reg::TAR, 0x48);

        i2c.write(reg::DATA_CMD, 0x01u32);
        i2c.write(reg::DATA_CMD, 0x02u32);
        assert!(log.borrow().is_empty(), "no callback before STOP");
        // The last byte carries STOP.
        i2c.write(reg::DATA_CMD, 0x03u32 | data_cmd::STOP);

        assert_eq!(*log.borrow(), vec![(0x48, vec![0x01, 0x02, 0x03])]);
        assert_ne!(i2c.read(reg::RAW_INTR_STAT) & intr::STOP_DET, 0);
        assert_ne!(i2c.read(reg::STATUS) & status::TFE, 0);
    }

    #[test]
    fn a_read_phase_fills_the_rx_fifo_in_order() {
        let mut i2c = master();
        i2c.set_callbacks(
            Box::new(|_, _| {}),
            Box::new(|address, len| {
                assert_eq!(address, 0x48);
                assert_eq!(len, 2);
                vec![0x19, 0x00]
            }),
        );
        i2c.write(reg::TAR, 0x48);

        i2c.write(reg::DATA_CMD, data_cmd::READ);
        assert_eq!(i2c.read(reg::RXFLR), 0, "the run completes at STOP");
        i2c.write(reg::DATA_CMD, data_cmd::READ | data_cmd::STOP);

        assert_eq!(i2c.read(reg::RXFLR), 2);
        assert_ne!(i2c.read(reg::RAW_INTR_STAT) & intr::RX_FULL, 0);
        assert_eq!(i2c.read(reg::DATA_CMD), 0x19);
        assert_eq!(i2c.read(reg::DATA_CMD), 0x00);
        assert_eq!(i2c.read(reg::RXFLR), 0);
        // Draining an empty FIFO is an underrun.
        assert_eq!(i2c.read(reg::DATA_CMD), 0);
        assert_ne!(i2c.read(reg::RAW_INTR_STAT) & intr::RX_UNDER, 0);
    }

    #[test]
    fn a_pointer_write_without_stop_is_replayed_before_the_read() {
        let mut i2c = master();
        let log = record_write(&mut i2c);
        i2c.write(reg::TAR, 0x48);

        i2c.write(reg::DATA_CMD, 0x00u32);
        // The read's first command is a repeated start: flush the pointer.
        i2c.write(reg::DATA_CMD, data_cmd::READ | data_cmd::STOP);

        assert_eq!(*log.borrow(), vec![(0x48, vec![0x00])]);
        assert_eq!(i2c.read(reg::RXFLR), 1);
    }

    #[test]
    fn the_enable_tar_enable_sandwich_keeps_a_queued_pointer() {
        // The sequence a blocking driver writes between a `nostop` write and
        // its read: the pointer must still reach the board when TAR is
        // programmed, even though ENABLE is low at that instant.
        let mut i2c = master();
        let log = record_write(&mut i2c);
        i2c.write(reg::TAR, 0x48);
        i2c.write(reg::DATA_CMD, 0x01u32);
        i2c.write(reg::ENABLE, 0);
        i2c.write(reg::TAR, 0x48);
        i2c.write(reg::ENABLE, enable::ENABLE);

        assert_eq!(*log.borrow(), vec![(0x48, vec![0x01])]);
        i2c.write(reg::DATA_CMD, data_cmd::READ | data_cmd::STOP);
        assert_eq!(i2c.read(reg::RXFLR), 1);
    }

    #[test]
    fn a_transaction_with_no_target_aborts() {
        let mut i2c = master();
        let log = record_write(&mut i2c);
        // IC_TAR is at its reset value 0: a reserved address, so no target.
        i2c.write(reg::DATA_CMD, 0xaau32 | data_cmd::STOP);

        let raw = i2c.read(reg::RAW_INTR_STAT);
        assert_ne!(raw & intr::TX_ABRT, 0);
        assert_ne!(raw & intr::MST_ON_HOLD, 0);
        assert_eq!(
            i2c.read(reg::TX_ABRT_SOURCE) & abrt::ABRT_7B_ADDR_NOACK,
            abrt::ABRT_7B_ADDR_NOACK
        );
        assert!(log.borrow().is_empty());
        assert_eq!(i2c.read(reg::RXFLR), 0, "the write phase was flushed");

        // Reading the clear register drops the abort and the hold.
        assert_eq!(i2c.read(reg::CLR_TX_ABRT), 0);
        assert_eq!(i2c.read(reg::RAW_INTR_STAT) & intr::TX_ABRT, 0);
        assert_eq!(i2c.read(reg::TX_ABRT_SOURCE), 0);
    }

    #[test]
    fn the_abort_command_reports_a_user_abort() {
        let mut i2c = master();
        i2c.write(reg::TAR, 0x48);
        i2c.write(reg::DATA_CMD, 0x01u32);
        i2c.write(reg::ENABLE, enable::ENABLE | enable::ABORT);
        assert_ne!(i2c.read(reg::RAW_INTR_STAT) & intr::TX_ABRT, 0);
        assert_eq!(
            i2c.read(reg::TX_ABRT_SOURCE) & abrt::ABRT_USER_ABRT,
            abrt::ABRT_USER_ABRT
        );
        assert_eq!(i2c.read(reg::TXFLR), 0);
        assert_eq!(i2c.read(reg::ENABLE) & enable::ENABLE, enable::ENABLE);
    }

    #[test]
    fn the_component_registers_identify_a_dw_apb_i2c() {
        let mut i2c = I2c::new();
        assert_eq!(i2c.read(reg::COMP_TYPE), 0x4457_0140);
        assert_ne!(i2c.read(reg::COMP_PARAM_1), 0);
    }

    #[test]
    fn interrupt_mask_gates_the_line() {
        let mut i2c = master();
        i2c.set_callbacks(Box::new(|_, _| {}), Box::new(|_, len| vec![0; len]));
        i2c.write(reg::TAR, 0x48);
        i2c.write(reg::DATA_CMD, data_cmd::READ | data_cmd::STOP);
        assert!(!i2c.irq(), "mask is clear");
        i2c.write(reg::INTR_MASK, intr::RX_FULL);
        assert!(i2c.irq());
        assert_ne!(i2c.read(reg::INTR_STAT) & intr::RX_FULL, 0);
        i2c.write(reg::CLR_INTR, 0);
        assert_eq!(i2c.read(reg::INTR_STAT) & intr::RX_FULL, intr::RX_FULL);
    }
}
