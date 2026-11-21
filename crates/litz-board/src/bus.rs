//! Devices that speak a bus instead of a pin.
//!
//! A [`Part`](litz_core::Part) answers one pin at a time. A display, an
//! EEPROM or a sensor answers transactions: the chip starts an I2C transfer,
//! shifts bytes over SPI, or writes a UART frame, and the device replies. Those
//! devices implement [`I2cDevice`], [`SpiDevice`] or [`UartPeer`] and are
//! registered on the buses below.
//!
//! The split is deliberate. A bus device never sees a `Drive`, a net or a
//! level; it sees bytes and the simulated tick they arrived at. The adapter
//! that owns the chip translates its peripheral registers into these calls, so
//! a new chip does not have to know which display is wired to it.
//!
//! Everything here is Rust and has no external runtime: a wasm part speaks the
//! same protocols through `litz:part`, and a built-in part implements these
//! traits directly.

use litz_core::Tick;
use std::any::Any;
use std::collections::{BTreeMap, VecDeque};

/// An I2C target.
///
/// The host calls [`I2cDevice::connect`] once per address phase, then `write`
/// or `read` for every byte, then [`I2cDevice::stop`]. A register read is a
/// write phase followed by a repeated start and a read phase, which is why the
/// callbacks are split this way instead of taking one buffer.
pub trait I2cDevice {
    /// The controller addressed this device. `read` is true for a read phase.
    fn connect(&mut self, read: bool);
    /// The next byte this device drives. Only called during a read phase.
    fn read(&mut self) -> u8;
    /// A byte the controller wrote. Returning false nacks it.
    fn write(&mut self, byte: u8) -> bool;
    /// Stop condition, or the end of the transaction.
    fn stop(&mut self);

    /// Downcast hook for a test or a diagnostic that wants the builtin model
    /// rather than the transaction interface.
    ///
    /// Most implementations leave this at `None`; the builtin devices return
    /// themselves so [`I2c::device_as`] can recover their framebuffer, register
    /// file or latched port. It does not affect how a transaction is handled.
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }

    /// Mutable downcast hook, for a host action that changes the model (a
    /// sensor reading set from a scenario).
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        None
    }
}

/// An SPI target, selected by a chip-select line.
pub trait SpiDevice {
    /// Chip-select changed. Every transfer is bracketed by this.
    fn select(&mut self, selected: bool);
    /// Full-duplex exchange: shift `tx` out and return `rx_len` bytes.
    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8>;

    /// Downcast hook, the SPI counterpart of [`I2cDevice::as_any`].
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }

    /// The mutable counterpart, for a host action that changes the model
    /// (saving a card's image). Devices that only report state leave it out.
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        None
    }
}

/// The far end of a UART.
pub trait UartPeer {
    /// Bytes the chip transmitted.
    fn receive(&mut self, at: Tick, bytes: &[u8]);
    /// Bytes this device has ready for the chip, if any.
    fn poll(&mut self, at: Tick) -> Vec<u8> {
        let _ = at;
        Vec::new()
    }
}

/// One I2C controller's devices, keyed by 7-bit address.
#[derive(Default)]
pub struct I2c {
    devices: BTreeMap<u8, Box<dyn I2cDevice>>,
}

impl I2c {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a device at `address`. Ten-bit addresses and the reserved
    /// 0x00–0x07 range are refused, as is a second device on one address.
    pub fn attach(&mut self, address: u8, device: Box<dyn I2cDevice>) -> Result<(), String> {
        if address > 0x7f {
            return Err(format!("I2C address 0x{address:02x} is not 7-bit"));
        }
        if address < 0x08 {
            return Err(format!("I2C address 0x{address:02x} is reserved"));
        }
        if self.devices.contains_key(&address) {
            return Err(format!("I2C address 0x{address:02x} already has a device"));
        }
        self.devices.insert(address, device);
        Ok(())
    }

    pub fn addresses(&self) -> impl Iterator<Item = u8> + '_ {
        self.devices.keys().copied()
    }

    /// The device at `address`, for an adapter that drives the transaction
    /// callbacks itself (esp32sim's controller does).
    pub fn device(&self, address: u8) -> Option<&(dyn I2cDevice + '_)> {
        match self.devices.get(&address) {
            Some(device) => Some(&**device),
            None => None,
        }
    }

    pub fn device_mut(&mut self, address: u8) -> Option<&mut (dyn I2cDevice + '_)> {
        match self.devices.get_mut(&address) {
            Some(device) => Some(&mut **device),
            None => None,
        }
    }

    /// The builtin model at `address`, when it offers a downcast hook.
    ///
    /// This is a read-back for tests and diagnostics: a device that implements
    /// [`I2cDevice::as_any`] can be inspected through its own type (an
    /// SSD1306's pixels, a PCF8574's latched port) without adding a transaction
    /// that no real controller would send.
    pub fn device_as<T: I2cDevice + 'static>(&self, address: u8) -> Option<&T> {
        self.devices.get(&address)?.as_any()?.downcast_ref::<T>()
    }

    /// The same model, mutably, for a host action that changes its state.
    pub fn device_as_mut<T: I2cDevice + 'static>(&mut self, address: u8) -> Option<&mut T> {
        self.devices
            .get_mut(&address)?
            .as_any_mut()?
            .downcast_mut::<T>()
    }

    /// Write `bytes` and stop. False when nothing is at `address` or the
    /// target nacked a byte.
    pub fn write(&mut self, address: u8, bytes: &[u8]) -> bool {
        let Some(device) = self.devices.get_mut(&address) else {
            return false;
        };
        device.connect(false);
        let mut acked = true;
        for &byte in bytes {
            if !device.write(byte) {
                acked = false;
                break;
            }
        }
        device.stop();
        acked
    }

    /// Read `len` bytes. None when nothing is at `address`.
    pub fn read(&mut self, address: u8, len: usize) -> Option<Vec<u8>> {
        let device = self.devices.get_mut(&address)?;
        device.connect(true);
        let out = (0..len).map(|_| device.read()).collect();
        device.stop();
        Some(out)
    }

    /// A register read: write phase, repeated start, read phase.
    pub fn write_read(&mut self, address: u8, write: &[u8], len: usize) -> Option<Vec<u8>> {
        let device = self.devices.get_mut(&address)?;
        device.connect(false);
        for &byte in write {
            if !device.write(byte) {
                device.stop();
                return None;
            }
        }
        device.connect(true);
        let out = (0..len).map(|_| device.read()).collect();
        device.stop();
        Some(out)
    }
}

/// One SPI controller's devices, keyed by the chip-select GPIO.
#[derive(Default)]
pub struct Spi {
    devices: BTreeMap<u8, Box<dyn SpiDevice>>,
}

impl Spi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attach(&mut self, chip_select: u8, device: Box<dyn SpiDevice>) -> Result<(), String> {
        if self.devices.contains_key(&chip_select) {
            return Err(format!(
                "SPI chip select {chip_select} already has a device"
            ));
        }
        self.devices.insert(chip_select, device);
        Ok(())
    }

    /// A full transaction on `chip_select`. An unpopulated select reads as
    /// 0xFF, which is what a floating MISO line gives a real controller.
    pub fn transfer(&mut self, chip_select: u8, tx: &[u8], rx_len: usize) -> Vec<u8> {
        let Some(device) = self.devices.get_mut(&chip_select) else {
            return vec![0xff; rx_len];
        };
        device.select(true);
        let rx = device.transfer(tx, rx_len);
        device.select(false);
        rx
    }

    pub fn has(&self, chip_select: u8) -> bool {
        self.devices.contains_key(&chip_select)
    }

    /// The builtin model on `chip_select`, when it offers a downcast hook.
    pub fn device_as<T: SpiDevice + 'static>(&self, chip_select: u8) -> Option<&T> {
        self.devices
            .get(&chip_select)?
            .as_any()?
            .downcast_ref::<T>()
    }

    /// The same model, mutably, for a host action that changes its state
    /// (saving an SD card's image after a run).
    pub fn device_as_mut<T: SpiDevice + 'static>(&mut self, chip_select: u8) -> Option<&mut T> {
        self.devices
            .get_mut(&chip_select)?
            .as_any_mut()?
            .downcast_mut::<T>()
    }

    pub fn chip_selects(&self) -> impl Iterator<Item = u8> + '_ {
        self.devices.keys().copied()
    }

    /// The only chip select on this controller, when there is exactly one.
    ///
    /// A chip that does not report pin routes cannot say which select a
    /// transaction meant; one device makes the answer unambiguous, two do not.
    pub fn sole_chip_select(&self) -> Option<u8> {
        let mut selects = self.devices.keys().copied();
        let first = selects.next()?;
        selects.next().is_none().then_some(first)
    }
}

/// One UART port's peer, if a part is wired to it.
#[derive(Default)]
pub struct Uart {
    peers: BTreeMap<u8, Box<dyn UartPeer>>,
}

impl Uart {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attach(&mut self, port: u8, peer: Box<dyn UartPeer>) -> Result<(), String> {
        if self.peers.contains_key(&port) {
            return Err(format!("UART{port} already has a peer"));
        }
        self.peers.insert(port, peer);
        Ok(())
    }

    /// Hand the chip's bytes to the part. False when no part is wired.
    pub fn receive(&mut self, port: u8, at: Tick, bytes: &[u8]) -> bool {
        let Some(peer) = self.peers.get_mut(&port) else {
            return false;
        };
        peer.receive(at, bytes);
        true
    }

    /// Bytes the part has for the chip.
    pub fn poll(&mut self, port: u8, at: Tick) -> Vec<u8> {
        self.peers
            .get_mut(&port)
            .map(|peer| peer.poll(at))
            .unwrap_or_default()
    }

    pub fn has(&self, port: u8) -> bool {
        self.peers.contains_key(&port)
    }

    pub fn ports(&self) -> impl Iterator<Item = u8> + '_ {
        self.peers.keys().copied()
    }
}

/// Maximum data bytes in a classic CAN 2.0A/B frame.
pub const CAN_MAX_CLASSIC_DATA: usize = 8;
/// Maximum data bytes in a CAN FD frame.
pub const CAN_MAX_DATA: usize = 64;

/// Error counters at or above which a node is error-passive (ISO 11898-1).
pub const CAN_ERROR_PASSIVE_THRESHOLD: u16 = 128;
/// Transmit counter at or above which a node is bus-off (ISO 11898-1).
pub const CAN_BUS_OFF_THRESHOLD: u16 = 256;

/// What a [`CanFrame`] carries on the wire.
///
/// `Classic` and `Remote` are the CAN 2.0A/B kinds; `Fd` adds the CAN FD payload and the
/// bit-rate-switch flag; `Error` is the error frame a controller broadcasts when it detects
/// a bus error. `rtr` is kept as a separate field for source compatibility, but
/// [`CanFrame::normalized`] keeps the two views in agreement.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CanFrameKind {
    #[default]
    Classic,
    Fd {
        /// The frame switches to the data-phase bit rate (CAN FD BRS).
        bit_rate_switch: bool,
    },
    Remote,
    Error,
}

/// A CAN frame on the board's CAN fabric.
///
/// The frame is pure data: there is no timestamp. A single [`CanBus::transmit`] delivers in
/// call order; [`CanBus::arbitrate`] and [`CanBus::advance`] add deterministic bus
/// arbitration for simultaneous transmitters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanFrame {
    /// 11-bit standard or 29-bit extended identifier.
    pub id: u32,
    pub extended: bool,
    /// Remote transmission request: `len` is the requested length and `data` is ignored.
    pub rtr: bool,
    pub len: u8,
    pub data: [u8; CAN_MAX_DATA],
    pub kind: CanFrameKind,
}

impl Default for CanFrame {
    fn default() -> Self {
        // `[u8; 64]` has no `Default`, so the empty classic frame is written out.
        CanFrame {
            id: 0,
            extended: false,
            rtr: false,
            len: 0,
            data: [0; CAN_MAX_DATA],
            kind: CanFrameKind::Classic,
        }
    }
}

impl CanFrame {
    /// Mask the identifier to its format, clamp the length to the kind's maximum and zero
    /// the unused data bytes.
    pub fn normalized(mut self) -> Self {
        // `rtr` predates the kind; keep the two representations consistent whichever one
        // the caller set, so a frame built before this field existed still means a remote.
        if self.rtr {
            self.kind = CanFrameKind::Remote;
        }
        if self.kind == CanFrameKind::Remote {
            self.rtr = true;
        }
        self.id &= if self.extended { 0x1fff_ffff } else { 0x7ff };
        match self.kind {
            CanFrameKind::Error => {
                // An error frame has no identifier or data and is never a remote request.
                self.id = 0;
                self.extended = false;
                self.rtr = false;
                self.len = 0;
                self.data = [0; CAN_MAX_DATA];
            }
            CanFrameKind::Remote => {
                // Remote requests exist only in the classic format, whose DLC is 0..=8.
                self.len = self.len.min(CAN_MAX_CLASSIC_DATA as u8);
                self.data = [0; CAN_MAX_DATA];
            }
            CanFrameKind::Classic => {
                self.len = self.len.min(CAN_MAX_CLASSIC_DATA as u8);
                for byte in self.data.iter_mut().skip(self.len as usize) {
                    *byte = 0;
                }
            }
            CanFrameKind::Fd { .. } => {
                self.len = self.len.min(CAN_MAX_DATA as u8);
                for byte in self.data.iter_mut().skip(self.len as usize) {
                    *byte = 0;
                }
            }
        }
        self
    }

    /// A standard (11-bit) classic data frame.
    pub fn standard(id: u32, data: &[u8]) -> Self {
        Self::data_frame(id, false, data)
    }

    /// An extended (29-bit) classic data frame.
    pub fn extended(id: u32, data: &[u8]) -> Self {
        Self::data_frame(id, true, data)
    }

    fn data_frame(id: u32, extended: bool, data: &[u8]) -> Self {
        let len = data.len().min(CAN_MAX_CLASSIC_DATA) as u8;
        let mut frame = CanFrame {
            id,
            extended,
            rtr: false,
            len,
            data: [0; CAN_MAX_DATA],
            kind: CanFrameKind::Classic,
        };
        frame.data[..len as usize].copy_from_slice(&data[..len as usize]);
        frame.normalized()
    }

    /// A remote frame asking for `len` bytes.
    pub fn remote(id: u32, extended: bool, len: u8) -> Self {
        CanFrame {
            id,
            extended,
            rtr: true,
            len,
            data: [0; CAN_MAX_DATA],
            kind: CanFrameKind::Remote,
        }
        .normalized()
    }

    /// A CAN FD data frame, up to sixty-four data bytes.
    pub fn fd(id: u32, extended: bool, bit_rate_switch: bool, data: &[u8]) -> Self {
        let len = data.len().min(CAN_MAX_DATA) as u8;
        let mut frame = CanFrame {
            id,
            extended,
            rtr: false,
            len,
            data: [0; CAN_MAX_DATA],
            kind: CanFrameKind::Fd { bit_rate_switch },
        };
        frame.data[..len as usize].copy_from_slice(&data[..len as usize]);
        frame.normalized()
    }

    /// A controller's error frame: it carries no identifier or payload.
    pub fn error() -> Self {
        CanFrame {
            kind: CanFrameKind::Error,
            ..CanFrame::default()
        }
        .normalized()
    }

    pub fn is_fd(&self) -> bool {
        matches!(self.kind, CanFrameKind::Fd { .. })
    }

    pub fn is_error(&self) -> bool {
        self.kind == CanFrameKind::Error
    }

    /// CAN arbitration priority: lower base identifier wins; on a tie a standard frame wins
    /// over an extended one (the IDE/SRR bit is dominant for standard frames), then the
    /// full extended identifier.
    fn priority_key(&self) -> (u32, bool, u32) {
        let base = self.id & 0x7ff;
        let full = if self.extended {
            self.id & 0x1fff_ffff
        } else {
            base
        };
        (base, self.extended, full)
    }
}

/// One monitored transmission: the node that put it on the bus and the frame itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanMonitorEntry {
    pub node: u16,
    pub frame: CanFrame,
}

/// A node's transmit and receive error counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CanErrorState {
    pub tx_error_counter: u16,
    pub rx_error_counter: u16,
}

impl CanErrorState {
    /// Error-passive when either counter reaches 128.
    pub fn is_error_passive(&self) -> bool {
        self.tx_error_counter >= CAN_ERROR_PASSIVE_THRESHOLD
            || self.rx_error_counter >= CAN_ERROR_PASSIVE_THRESHOLD
    }

    /// Bus-off when the transmit counter reaches 256.
    pub fn is_bus_off(&self) -> bool {
        self.tx_error_counter >= CAN_BUS_OFF_THRESHOLD
    }
}

/// What one simultaneous-transmission round decided.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CanArbitration {
    /// The node whose frame was accepted and delivered, when there was one.
    pub winner: Option<u16>,
    /// Nodes that lost arbitration and must retry.
    pub losers: Vec<u16>,
    /// Nodes whose identical identifiers carried different payloads. No frame was delivered.
    pub collision: Vec<u16>,
    /// Nodes that were bus-off and could not take part.
    pub suppressed: Vec<u16>,
}

/// A deterministic CAN fabric.
///
/// Nodes attach once and are identified by a caller-chosen index (a board index, for the
/// Litz adapter). [`CanBus::transmit`] is the immediate path used by a single transmitter.
/// [`CanBus::arbitrate`] resolves frames offered in the same instant: the lowest identifier
/// wins, losers count an error and retry later, and two frames with the same identifier but
/// different payloads report a collision instead of delivering either. [`CanBus::submit_at`]
/// and [`CanBus::advance`] group calls that carry a tick, so two nodes that transmit in the
/// same tick arbitrate together.
///
/// Error counters are simplified from ISO 11898-1: an arbitration loss or a malformed frame
/// adds eight, a successful frame subtracts one, error-passive starts at 128 and bus-off at
/// 256. A bus-off node refuses to transmit until [`CanBus::reset_errors`] (a controller
/// reset) or until it successfully receives a frame. This is deterministic bookkeeping for
/// tests, not a bit-level error-confinement model.
#[derive(Default)]
pub struct CanBus {
    nodes: BTreeMap<u16, VecDeque<CanFrame>>,
    /// Attach order, so fan-out and iteration are stable.
    order: Vec<u16>,
    monitor: Vec<CanMonitorEntry>,
    errors: BTreeMap<u16, CanErrorState>,
    /// Frames offered for a tick but not yet arbitrated.
    queue: Vec<(Tick, u16, CanFrame)>,
}

impl CanBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach `node`, or do nothing if it is already attached.
    pub fn attach(&mut self, node: u16) {
        if let std::collections::btree_map::Entry::Vacant(slot) = self.nodes.entry(node) {
            slot.insert(VecDeque::new());
            self.errors.entry(node).or_default();
            self.order.push(node);
        }
    }

    pub fn is_attached(&self, node: u16) -> bool {
        self.nodes.contains_key(&node)
    }

    /// Put `frame` on the bus from `node`. Every *other* attached node receives one copy.
    /// Returns an error when the transmitter is not attached or is bus-off.
    pub fn transmit(&mut self, node: u16, frame: CanFrame) -> Result<(), String> {
        if !self.nodes.contains_key(&node) {
            return Err(format!("CAN node {node} is not attached"));
        }
        if self.is_bus_off(node) {
            return Err(format!("CAN node {node} is bus-off"));
        }
        let frame = frame.normalized();
        self.monitor.push(CanMonitorEntry { node, frame });
        if frame.is_error() {
            self.broadcast_error(node);
        } else {
            self.deliver(&[node], frame);
            self.recover_after_success(node);
        }
        Ok(())
    }

    /// Resolve `attempts` as simultaneous transmissions on one bus instant.
    ///
    /// The lowest identifier wins. A frame with a standard identifier beats an extended
    /// frame that shares its eleven-bit base. If the winning identifier appears with
    /// different payloads, every such transmitter reports a collision and nothing is
    /// delivered. Identical frames with the same identifier are one wire event: one copy is
    /// delivered and each transmitter is considered successful.
    pub fn arbitrate(&mut self, attempts: &[(u16, CanFrame)]) -> Result<CanArbitration, String> {
        for (node, _) in attempts {
            if !self.nodes.contains_key(node) {
                return Err(format!("CAN node {node} is not attached"));
            }
        }
        Ok(self.arbitrate_group(attempts))
    }

    /// Offer `frame` from `node` at `tick`. [`CanBus::advance`] arbitrates every offer whose
    /// tick has arrived, so two calls with the same tick behave like one [`CanBus::arbitrate`].
    pub fn submit_at(&mut self, tick: Tick, node: u16, frame: CanFrame) -> Result<(), String> {
        if !self.nodes.contains_key(&node) {
            return Err(format!("CAN node {node} is not attached"));
        }
        if self.is_bus_off(node) {
            return Err(format!("CAN node {node} is bus-off"));
        }
        self.queue.push((tick, node, frame.normalized()));
        Ok(())
    }

    /// Arbitrate every offer dated at or before `tick`, in tick order. Offers for later
    /// ticks stay queued.
    pub fn advance(&mut self, tick: Tick) -> Vec<CanArbitration> {
        let mut due = Vec::new();
        let mut future = Vec::new();
        for entry in std::mem::take(&mut self.queue) {
            if entry.0 <= tick {
                due.push(entry);
            } else {
                future.push(entry);
            }
        }
        self.queue = future;

        let mut groups: BTreeMap<Tick, Vec<(u16, CanFrame)>> = BTreeMap::new();
        for (at, node, frame) in due {
            groups.entry(at).or_default().push((node, frame));
        }
        groups
            .into_values()
            .map(|group| self.arbitrate_group(&group))
            .collect()
    }

    fn arbitrate_group(&mut self, attempts: &[(u16, CanFrame)]) -> CanArbitration {
        let mut outcome = CanArbitration::default();
        let mut contenders = Vec::new();
        let mut error_sender = None;

        for (node, frame) in attempts {
            if self.is_bus_off(*node) {
                outcome.suppressed.push(*node);
            } else if frame.normalized().is_error() {
                // An error frame is dominant: it aborts the current transfer. One is enough
                // to represent the bus event.
                error_sender.get_or_insert(*node);
            } else {
                contenders.push((*node, frame.normalized()));
            }
        }

        if let Some(node) = error_sender {
            let frame = CanFrame::error();
            self.monitor.push(CanMonitorEntry { node, frame });
            self.broadcast_error(node);
            outcome.winner = Some(node);
            outcome
                .losers
                .extend(contenders.iter().map(|(node, _)| *node));
            for (loser, _) in &contenders {
                self.note_arbitration_loss(*loser);
            }
            outcome.losers.sort_unstable();
            outcome.suppressed.sort_unstable();
            return outcome;
        }

        // The lowest priority key wins. Ties are resolved by node index below, which keeps
        // the outcome independent of the order the caller listed the attempts in.
        let Some(&(_, first)) = contenders
            .iter()
            .min_by_key(|(_, frame)| frame.priority_key())
        else {
            outcome.suppressed.sort_unstable();
            return outcome;
        };
        let key = first.priority_key();
        let mut winners: Vec<(u16, CanFrame)> = contenders
            .iter()
            .filter(|(_, frame)| frame.priority_key() == key)
            .copied()
            .collect();
        let mut losers: Vec<(u16, CanFrame)> = contenders
            .iter()
            .filter(|(_, frame)| frame.priority_key() != key)
            .copied()
            .collect();

        // Payloads are part of the frame; equal identifiers with unequal contents would have
        // collided on the wire during the data phase.
        let identical = winners.iter().all(|(_, frame)| *frame == winners[0].1);
        if !identical {
            outcome.collision = winners.iter().map(|(node, _)| *node).collect();
            outcome.collision.sort_unstable();
            outcome.losers = losers.iter().map(|(node, _)| *node).collect();
            outcome.losers.sort_unstable();
            outcome.suppressed.sort_unstable();
            for (node, _) in &winners {
                self.note_arbitration_loss(*node);
            }
            for (node, _) in &losers {
                self.note_arbitration_loss(*node);
            }
            return outcome;
        }

        winners.sort_unstable_by_key(|(node, _)| *node);
        let winner = winners[0].0;
        let frame = winners[0].1;
        self.monitor.push(CanMonitorEntry {
            node: winner,
            frame,
        });
        let transmitted: Vec<u16> = winners.iter().map(|(node, _)| *node).collect();
        self.deliver(&transmitted, frame);
        for (node, _) in &winners {
            self.recover_after_success(*node);
        }

        losers.sort_unstable_by_key(|(node, _)| *node);
        for (node, _) in &losers {
            self.note_arbitration_loss(*node);
        }
        outcome.winner = Some(winner);
        outcome.losers = losers.iter().map(|(node, _)| *node).collect();
        outcome.suppressed.sort_unstable();
        outcome
    }

    fn deliver(&mut self, from: &[u16], frame: CanFrame) {
        for &other in &self.order {
            if from.contains(&other) {
                continue;
            }
            if let Some(queue) = self.nodes.get_mut(&other) {
                queue.push_back(frame);
            }
        }
    }

    /// An error frame is seen by every other attached node, which counts a receive error
    /// when it takes the frame.
    fn broadcast_error(&mut self, from: u16) {
        for &other in &self.order {
            if other == from {
                continue;
            }
            if let Some(queue) = self.nodes.get_mut(&other) {
                queue.push_back(CanFrame::error());
            }
        }
    }

    /// The oldest frame waiting for `node`, if any. A frame is removed when taken, so no
    /// frame is ever delivered twice. Taking an error frame increments the node's receive
    /// counter; taking a valid frame is what lets an error-passive or bus-off node recover.
    pub fn take(&mut self, node: u16) -> Option<CanFrame> {
        let frame = self.nodes.get_mut(&node)?.pop_front()?;
        if frame.is_error() {
            self.note_receive_error(node);
        } else {
            self.recover_after_success(node);
        }
        Some(frame)
    }

    /// How many frames are waiting for `node`.
    pub fn pending(&self, node: u16) -> usize {
        self.nodes.get(&node).map_or(0, VecDeque::len)
    }

    /// Every frame put on the bus, in transmit order (an arbitrated round logs one entry,
    /// the winning frame).
    pub fn monitor(&self) -> &[CanMonitorEntry] {
        &self.monitor
    }

    /// Take the monitor log, leaving an empty one.
    pub fn take_monitor(&mut self) -> Vec<CanMonitorEntry> {
        std::mem::take(&mut self.monitor)
    }

    /// Attached node indices in attach order.
    pub fn nodes(&self) -> impl Iterator<Item = u16> + '_ {
        self.order.iter().copied()
    }

    /// The node's error counters, or `None` when it is not attached.
    pub fn error_state(&self, node: u16) -> Option<CanErrorState> {
        self.errors.get(&node).copied()
    }

    pub fn is_error_passive(&self, node: u16) -> bool {
        self.errors
            .get(&node)
            .is_some_and(CanErrorState::is_error_passive)
    }

    pub fn is_bus_off(&self, node: u16) -> bool {
        self.errors
            .get(&node)
            .is_some_and(CanErrorState::is_bus_off)
    }

    /// Clear a node's error counters, as a controller reset in reset mode does. A bus-off
    /// node rejoins the bus.
    pub fn reset_errors(&mut self, node: u16) -> Result<(), String> {
        let state = self
            .errors
            .get_mut(&node)
            .ok_or_else(|| format!("CAN node {node} is not attached"))?;
        *state = CanErrorState::default();
        Ok(())
    }

    /// A malformed frame offered by `node`: count a transmit error on that node and put an
    /// error frame on the bus for everyone else.
    pub fn inject_malformed(&mut self, node: u16) -> Result<(), String> {
        if !self.nodes.contains_key(&node) {
            return Err(format!("CAN node {node} is not attached"));
        }
        self.note_transmit_error(node);
        let frame = CanFrame::error();
        self.monitor.push(CanMonitorEntry { node, frame });
        self.broadcast_error(node);
        Ok(())
    }

    fn note_transmit_error(&mut self, node: u16) {
        if let Some(state) = self.errors.get_mut(&node) {
            state.tx_error_counter = state.tx_error_counter.saturating_add(8);
        }
    }

    fn note_receive_error(&mut self, node: u16) {
        if let Some(state) = self.errors.get_mut(&node) {
            state.rx_error_counter = state.rx_error_counter.saturating_add(8);
        }
    }

    /// Losing arbitration is not an error on real silicon, but the board model counts it so
    /// a test can reach the error-passive and bus-off states deterministically.
    fn note_arbitration_loss(&mut self, node: u16) {
        self.note_transmit_error(node);
    }

    /// A valid frame moves a node's counters back toward zero. A bus-off node that sees a
    /// valid frame rejoins, which keeps the test model recoverable without a reset handshake.
    fn recover_after_success(&mut self, node: u16) {
        if let Some(state) = self.errors.get_mut(&node) {
            if state.is_bus_off() {
                *state = CanErrorState::default();
            } else {
                state.tx_error_counter = state.tx_error_counter.saturating_sub(1);
                state.rx_error_counter = state.rx_error_counter.saturating_sub(1);
            }
        }
    }
}

/// Every bus on the circuit.
///
/// The key is `(board, controller)`, not the controller alone: two development
/// boards each have a UART0 and an I2C0, and they are different wires even
/// though the numbers match. Board 0 is the board a single-board diagram has.
#[derive(Default)]
pub struct Buses {
    /// The I2C controllers, keyed by the board whose chip drives them.
    pub i2c: BTreeMap<(u16, u8), I2c>,
    pub spi: BTreeMap<(u16, u8), Spi>,
    pub uart: BTreeMap<(u16, u8), Uart>,
    /// The CAN fabric. Nodes are keyed by board index, so two MCUs on one diagram are
    /// distinct endpoints on the same wire.
    pub can: CanBus,
}

impl Buses {
    pub fn new() -> Self {
        Self::default()
    }

    /// The I2C controller `index` of `board`, created on first use.
    pub fn i2c_mut(&mut self, board: u16, index: u8) -> &mut I2c {
        self.i2c.entry((board, index)).or_default()
    }

    /// The SPI controller `index` of `board`, created on first use.
    pub fn spi_mut(&mut self, board: u16, index: u8) -> &mut Spi {
        self.spi.entry((board, index)).or_default()
    }

    /// The UART port `index` of `board`, created on first use.
    pub fn uart_mut(&mut self, board: u16, index: u8) -> &mut Uart {
        self.uart.entry((board, index)).or_default()
    }

    /// The shared CAN fabric.
    pub fn can_mut(&mut self) -> &mut CanBus {
        &mut self.can
    }

    /// The controller and chip select a `SpiPins::cs` mask selects on `board`.
    ///
    /// The mask is every asserted select, so a mirrored route names one device
    /// twice; two *different* devices asserted at once is a circuit fault and
    /// answers `None`, as does a mask with no populated select.
    pub fn spi_selected(&self, board: u16, asserted: u64) -> Option<(u8, u8)> {
        let mut found = None;
        for (&(owner, controller), spi) in &self.spi {
            if owner != board {
                continue;
            }
            for chip_select in spi.chip_selects() {
                if chip_select < 64 && asserted & (1u64 << chip_select) != 0 {
                    if found.is_some() {
                        return None;
                    }
                    found = Some((controller, chip_select));
                }
            }
        }
        found
    }

    /// The only device on `board`'s controller `controller`, when there is
    /// exactly one.
    pub fn spi_sole(&self, board: u16, controller: u8) -> Option<(u8, u8)> {
        let spi = self.spi.get(&(board, controller))?;
        Some((controller, spi.sole_chip_select()?))
    }

    /// The only device on `board` when that board has exactly one controller
    /// and the controller has exactly one device.
    pub fn spi_only_device(&self, board: u16) -> Option<(u8, u8)> {
        let mut mine = self.spi.iter().filter(|((owner, _), _)| *owner == board);
        let (&(_, controller), spi) = mine.next()?;
        if mine.next().is_some() {
            return None;
        }
        Some((controller, spi.sole_chip_select()?))
    }

    /// Whether any board has a bus at all.
    pub fn is_empty(&self) -> bool {
        self.i2c.is_empty()
            && self.spi.is_empty()
            && self.uart.is_empty()
            && self.can.nodes().next().is_none()
            && self.can.monitor().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A register file with a fixed chip id, the shape of a BMP280.
    struct Registers {
        id: u8,
        pointer: u8,
        connected: bool,
    }

    impl I2cDevice for Registers {
        fn connect(&mut self, _read: bool) {
            self.connected = true;
        }

        fn read(&mut self) -> u8 {
            assert!(self.connected, "read without an address phase");
            if self.pointer == 0xd0 { self.id } else { 0 }
        }

        fn write(&mut self, byte: u8) -> bool {
            self.pointer = byte;
            true
        }

        fn stop(&mut self) {
            self.connected = false;
        }
    }

    #[test]
    fn a_register_read_returns_the_chip_id() {
        let mut bus = I2c::new();
        bus.attach(
            0x76,
            Box::new(Registers {
                id: 0x58,
                pointer: 0,
                connected: false,
            }),
        )
        .unwrap();
        assert_eq!(bus.write_read(0x76, &[0xd0], 1), Some(vec![0x58]));
    }

    #[test]
    fn an_empty_address_is_not_acked() {
        let mut bus = I2c::new();
        assert!(!bus.write(0x76, &[0xd0]));
        assert_eq!(bus.read(0x76, 1), None);
        assert_eq!(bus.write_read(0x76, &[0xd0], 1), None);
    }

    #[test]
    fn two_devices_cannot_share_an_address() {
        let mut bus = I2c::new();
        bus.attach(
            0x76,
            Box::new(Registers {
                id: 1,
                pointer: 0,
                connected: false,
            }),
        )
        .unwrap();
        let again = bus.attach(
            0x76,
            Box::new(Registers {
                id: 2,
                pointer: 0,
                connected: false,
            }),
        );
        assert!(again.is_err());
    }

    struct Echo {
        selected: bool,
        seen: Vec<u8>,
    }

    impl SpiDevice for Echo {
        fn select(&mut self, selected: bool) {
            self.selected = selected;
        }

        fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
            assert!(self.selected, "transfer outside chip select");
            self.seen.extend_from_slice(tx);
            let mut out = tx.to_vec();
            out.resize(rx_len, 0);
            out
        }
    }

    #[test]
    fn spi_is_bracketed_by_chip_select_and_echoes() {
        let mut bus = Spi::new();
        bus.attach(
            5,
            Box::new(Echo {
                selected: false,
                seen: Vec::new(),
            }),
        )
        .unwrap();
        assert_eq!(bus.transfer(5, &[0xaa, 0x55], 2), vec![0xaa, 0x55]);
        assert_eq!(
            bus.transfer(6, &[0xaa], 1),
            vec![0xff],
            "an unpopulated select floats high"
        );
    }

    struct Line {
        received: Vec<u8>,
        reply: Vec<u8>,
    }

    impl UartPeer for Line {
        fn receive(&mut self, _at: Tick, bytes: &[u8]) {
            self.received.extend_from_slice(bytes);
        }

        fn poll(&mut self, _at: Tick) -> Vec<u8> {
            std::mem::take(&mut self.reply)
        }
    }

    #[test]
    fn a_uart_peer_receives_and_replies() {
        let mut bus = Uart::new();
        bus.attach(
            0,
            Box::new(Line {
                received: Vec::new(),
                reply: b"OK\n".to_vec(),
            }),
        )
        .unwrap();
        assert!(bus.receive(0, 10, b"AT\n"));
        assert_eq!(bus.poll(0, 20), b"OK\n".to_vec());
        assert!(!bus.receive(1, 30, b"AT\n"), "no peer on UART1");
        assert!(bus.poll(1, 40).is_empty());
    }
}
