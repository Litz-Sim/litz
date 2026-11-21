//! An AT86RF233 IEEE 802.15.4 transceiver on the shared packet medium.
//!
//! The part is a pin-level SPI slave, not a bus device: it decodes SCK, MOSI
//! and /SEL from the edges a driver produces, exactly as the SX1276 and
//! nRF24L01 models do. The transaction shape is the silicon's: a command byte
//! selects a register or the frame buffer, a register access carries exactly
//! one data byte, and a frame-buffer access carries the length byte (the PHR)
//! followed by the PSDU.
//!
//! The 802.15.4 MAC is modelled where it changes what the radio does. A frame
//! written to the buffer is sent as the PSDU the bytes describe; a received
//! frame is decoded, its FCS checked, and only then filtered by PAN and
//! destination address (broadcast included). The medium's channel, network
//! and directed address are the same hand-off the other radios use.
//!
//! Registers outside the subset that changes behaviour keep their written
//! value and read back, so a driver's setup sequence sees no surprises.
//! Security, information elements, the hardware's autonomous acknowledgement
//! path and the analog front end are not modelled.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, RADIO_LAST_BYTES, Tick};
use litz_ieee802154::{Address, BROADCAST_SHORT, CHANNEL_MAX, CHANNEL_MIN, Frame, MAX_PSDU};
use litz_radio::{RadioConfig, RadioHubHandle, RadioId};
use std::collections::VecDeque;

// ------------------------------------------------------------------- layout

fn index(part: &FlatPart, name: &str) -> Option<u16> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
}

fn pins<const N: usize>(part: &FlatPart, names: [&str; N]) -> Result<[u16; N], BuildError> {
    let mut found = [0u16; N];
    for (slot, name) in found.iter_mut().zip(names) {
        *slot = index(part, name).ok_or_else(|| {
            BuildError::Message(format!("part {:?} has no {name:?} pin", part.id))
        })?;
    }
    Ok(found)
}

fn attr_text(part: &FlatPart, name: &str, default: &str) -> String {
    part.attrs
        .get(name)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn attr_u16(part: &FlatPart, name: &str, default: u16) -> u16 {
    part.attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

fn attr_i8(part: &FlatPart, name: &str, default: i8) -> i8 {
    part.attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

/// Parse an extended address written most-significant byte first.
///
/// A driver and the register map both call the low byte IEEE_ADDR_0, so the
/// attribute's `0011223344556677` becomes the on-air order 77..00. Fewer than
/// sixteen hex digits are right-aligned, which is how a reader expects a short
/// literal such as `0x42` to name the low byte.
fn parse_extended(text: &str) -> [u8; 8] {
    let trimmed = text.trim();
    let clean = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    let mut digits: Vec<char> = clean.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if digits.len() % 2 == 1 {
        // An odd count names a value shorter than a byte; the leading zero
        // pairs the high nibble, as a reader expects from `0x4`.
        digits.insert(0, '0');
    }
    let bytes: Vec<u8> = digits
        .chunks(2)
        .filter_map(|pair| {
            let text: String = pair.iter().collect();
            u8::from_str_radix(&text, 16).ok()
        })
        .collect();
    let mut written = [0u8; 8];
    let keep = bytes.len().min(8);
    written[8 - keep..].copy_from_slice(&bytes[bytes.len() - keep..]);
    written.reverse();
    written
}

fn remember(last: &mut [u8; RADIO_LAST_BYTES], last_len: &mut u8, bytes: &[u8]) {
    last.fill(0);
    let count = bytes.len().min(RADIO_LAST_BYTES);
    last[..count].copy_from_slice(&bytes[..count]);
    *last_len = count as u8;
}

fn radio_state(tx_count: u32, rx_count: u32, last: &[u8; RADIO_LAST_BYTES], len: u8) -> PartState {
    PartState::Radio {
        tx_count,
        rx_count,
        last_bytes: *last,
        last_len: len,
    }
}

/// The medium sees a radio as a PAN plus a short address, so a directed frame
/// reaches only the module that owns both. A broadcast goes to everybody on
/// the channel and each module filters it by PAN itself.
fn hub_address(pan: u16, short: u16) -> Vec<u8> {
    let mut address = Vec::with_capacity(4);
    address.extend_from_slice(&pan.to_le_bytes());
    address.extend_from_slice(&short.to_le_bytes());
    address
}

// ----------------------------------------------------------------- SPI slave

/// The four wires of an AT86RF233 mode-0 SPI slave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpiWires {
    sck: u16,
    mosi: u16,
    miso: u16,
    seln: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpiEvent {
    Selected,
    Deselected,
    Byte(u8),
}

/// A byte-oriented mode-0 SPI decoder.
///
/// The master changes MOSI while SCK is low and samples MISO on the rising
/// edge; the slave samples MOSI on that edge and advances its output on the
/// falling edge. The response a part queues while a byte is being received is
/// therefore driven during the next byte, which is full-duplex timing.
struct SpiPort {
    wires: SpiWires,
    selected: bool,
    sck_high: bool,
    mosi_high: bool,
    in_byte: u8,
    in_bits: u8,
    out_byte: Option<u8>,
    out_bits: u8,
    miso_high: bool,
    out_queue: VecDeque<u8>,
}

impl SpiPort {
    fn new(wires: SpiWires) -> Self {
        Self {
            wires,
            selected: false,
            sck_high: false,
            mosi_high: false,
            in_byte: 0,
            in_bits: 0,
            out_byte: None,
            out_bits: 0,
            miso_high: true,
            out_queue: VecDeque::new(),
        }
    }

    fn set_pin(&mut self, pin: u16, level: Level) -> Option<SpiEvent> {
        if pin == self.wires.sck {
            return self.clock(level == Level::High);
        }
        if pin == self.wires.seln {
            return self.select(level == Level::Low);
        }
        if pin == self.wires.mosi {
            self.mosi_high = level == Level::High;
        }
        None
    }

    fn select(&mut self, selected: bool) -> Option<SpiEvent> {
        if selected == self.selected {
            return None;
        }
        self.selected = selected;
        self.in_byte = 0;
        self.in_bits = 0;
        self.out_byte = None;
        self.out_bits = 0;
        self.out_queue.clear();
        self.miso_high = true;
        Some(if selected {
            SpiEvent::Selected
        } else {
            SpiEvent::Deselected
        })
    }

    fn clock(&mut self, high: bool) -> Option<SpiEvent> {
        if high == self.sck_high {
            return None;
        }
        self.sck_high = high;
        if !self.selected {
            return None;
        }
        if high {
            self.in_byte = (self.in_byte << 1) | u8::from(self.mosi_high);
            self.in_bits += 1;
            return None;
        }
        self.shift_out();
        if self.in_bits == 8 {
            let byte = self.in_byte;
            self.in_byte = 0;
            self.in_bits = 0;
            Some(SpiEvent::Byte(byte))
        } else {
            None
        }
    }

    fn shift_out(&mut self) {
        match self.out_byte {
            Some(byte) if self.out_bits < 8 => {
                let shifted = byte << 1;
                self.out_byte = Some(shifted);
                self.out_bits += 1;
                self.miso_high = shifted & 0x80 != 0;
            }
            _ => {
                self.out_byte = None;
                self.miso_high = true;
            }
        }
    }

    fn push(&mut self, byte: u8) {
        self.out_queue.push_back(byte);
        if self.out_byte.is_none() {
            self.load_next();
        }
    }

    fn load_next(&mut self) {
        match self.out_queue.pop_front() {
            Some(byte) => {
                self.out_byte = Some(byte);
                self.out_bits = 1;
                self.miso_high = byte & 0x80 != 0;
            }
            None => {
                self.out_byte = None;
                self.out_bits = 0;
                self.miso_high = true;
            }
        }
    }

    fn miso_drive(&self) -> Drive {
        if self.selected {
            Drive::strong(if self.miso_high {
                Level::High
            } else {
                Level::Low
            })
        } else {
            Drive::HIGH_Z
        }
    }
}

// --------------------------------------------------------------- registers

const REG_TRX_STATUS: u8 = 0x01;
const REG_TRX_STATE: u8 = 0x02;
const REG_TRX_CTRL_1: u8 = 0x04;
const REG_PHY_RSSI: u8 = 0x06;
const REG_PHY_ED_LEVEL: u8 = 0x07;
const REG_PHY_CC_CCA: u8 = 0x08;
const REG_IRQ_MASK: u8 = 0x0e;
const REG_IRQ_STATUS: u8 = 0x0f;
const REG_PART_NUM: u8 = 0x1c;
const REG_VERSION_NUM: u8 = 0x1d;
const REG_MAN_ID_0: u8 = 0x1e;
const REG_MAN_ID_1: u8 = 0x1f;
const REG_MAC_SHORT_ADDR_0: u8 = 0x20;
const REG_MAC_SHORT_ADDR_1: u8 = 0x21;
const REG_MAC_PAN_ID_0: u8 = 0x22;
const REG_MAC_PAN_ID_1: u8 = 0x23;
const REG_MAC_IEEE_ADDR_0: u8 = 0x24;

/// SPI command layout: bits 7:6 name the access, bits 5:0 the address.
const SPI_ACCESS_REG: u8 = 0x80;
const SPI_ACCESS_FB: u8 = 0x20;
const SPI_ACCESS_WRITE: u8 = 0x40;
const SPI_ADDR_MASK: u8 = 0x3f;

const TRX_STATUS_BUSY_TX: u8 = 0x02;
const TRX_STATUS_RX_ON: u8 = 0x03;
const TRX_STATUS_TRX_OFF: u8 = 0x04;
const TRX_STATUS_PLL_ON: u8 = 0x05;
const TRX_STATUS_SLEEP: u8 = 0x06;
const TRX_STATUS_RESET: u8 = 0x1f;

const TRX_CMD_NOP: u8 = 0x00;
const TRX_CMD_TX_START: u8 = 0x01;
const TRX_CMD_FORCE_TRX_OFF: u8 = 0x02;
const TRX_CMD_FORCE_PLL_ON: u8 = 0x03;
const TRX_CMD_RX_ON: u8 = 0x04;
const TRX_CMD_TRX_OFF: u8 = 0x05;
const TRX_CMD_PLL_ON: u8 = 0x06;
const TRX_CMD_SLEEP: u8 = 0x07;
const TRX_CMD_RX_AACK_ON: u8 = 0x08;
const TRX_CMD_TX_ARET_ON: u8 = 0x09;
const TRX_CMD_MASK: u8 = 0x1f;

const IRQ_RX_START: u8 = 0x04;
const IRQ_TRX_END: u8 = 0x08;

const TRX_CTRL_1_IRQ_POLARITY: u8 = 0x01;
const TRX_CTRL_1_IRQ_MASK_MODE: u8 = 0x02;
const TRX_CTRL_1_TX_AUTO_CRC_ON: u8 = 0x20;
const TRX_CTRL_1_RESET: u8 = TRX_CTRL_1_TX_AUTO_CRC_ON | TRX_CTRL_1_IRQ_MASK_MODE;

const PART_NUM_AT86RF233: u8 = 0x0b;
const VERSION_NUM_AT86RF233: u8 = 0x01;

/// The register or frame-buffer access a selected transaction is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Idle,
    RegisterRead,
    RegisterWrite,
    FrameRead,
    FrameWrite,
}

/// An AT86RF233: the register subset a 802.15.4 driver touches, the frame
/// buffer, and the state machine that moves frames onto the shared medium.
pub struct At86rf233 {
    spi: SpiPort,
    rstn: u16,
    irq: u16,
    slptr: u16,
    hub: RadioHubHandle,
    radio: RadioId,
    registers: [u8; 0x40],
    pan: u16,
    short_address: u16,
    /// On-air order: IEEE_ADDR_0 is the least significant byte.
    extended_address: [u8; 8],
    channel: u8,
    state: u8,
    last_command: u8,
    irq_status: u8,
    irq_mask: u8,
    crc_valid: bool,
    /// PHR in byte 0, PSDU in 1..=127, as the silicon lays it out.
    frame: [u8; MAX_PSDU + 1],
    frame_len: u8,
    frame_written: u8,
    fb_read_ptr: u8,
    fb_write_pos: u8,
    access: Access,
    register_addr: u8,
    register_done: bool,
    spi_first: bool,
    rstn_low: bool,
    slptr_high: bool,
    now: Tick,
    tx_done_at: Option<Tick>,
    tx_count: u32,
    rx_count: u32,
    last: [u8; RADIO_LAST_BYTES],
    last_len: u8,
}

impl At86rf233 {
    pub fn build(part: &FlatPart, radios: &RadioHubHandle) -> Result<Box<dyn Part>, BuildError> {
        let [sck, mosi, miso, seln, rstn, irq, slptr] = pins(
            part,
            ["SCK", "MOSI", "MISO", "SELN", "RSTN", "IRQ", "SLPTR"],
        )?;
        let network = attr_text(part, "network", "ieee802154");
        let channel = match attr_u16(part, "channel", 15) {
            value if (u16::from(CHANNEL_MIN)..=u16::from(CHANNEL_MAX)).contains(&value) => {
                value as u8
            }
            // An out-of-page channel is a configuration mistake, not a reason
            // to refuse the circuit; the part stays on the default channel.
            _ => 15,
        };
        let pan = attr_u16(part, "pan", 0xabcd);
        let short_address = attr_u16(part, "address", 0x0001);
        let extended_address =
            parse_extended(&attr_text(part, "extended_address", "0011223344556677"));
        let power = attr_i8(part, "power", 0);
        let radio = radios.borrow_mut().register(RadioConfig {
            network,
            channel: u16::from(channel),
            address: hub_address(pan, short_address),
            power_dbm: power,
            range_m: None,
        });
        let mut model = Self {
            spi: SpiPort::new(SpiWires {
                sck,
                mosi,
                miso,
                seln,
            }),
            rstn,
            irq,
            slptr,
            hub: radios.clone(),
            radio,
            registers: [0; 0x40],
            pan,
            short_address,
            extended_address,
            channel,
            state: TRX_STATUS_TRX_OFF,
            last_command: TRX_CMD_NOP,
            irq_status: 0,
            irq_mask: 0,
            crc_valid: false,
            frame: [0; MAX_PSDU + 1],
            frame_len: 0,
            frame_written: 0,
            fb_read_ptr: 0,
            fb_write_pos: 0,
            access: Access::Idle,
            register_addr: 0,
            register_done: false,
            spi_first: true,
            rstn_low: false,
            slptr_high: false,
            now: 0,
            tx_done_at: None,
            tx_count: 0,
            rx_count: 0,
            last: [0; RADIO_LAST_BYTES],
            last_len: 0,
        };
        model.reset();
        Ok(Box::new(model))
    }

    fn reset(&mut self) {
        self.registers = [0; 0x40];
        self.registers[REG_TRX_STATUS as usize] = TRX_STATUS_TRX_OFF;
        self.registers[REG_TRX_CTRL_1 as usize] = TRX_CTRL_1_RESET;
        self.registers[REG_PHY_RSSI as usize] = 0x60;
        self.registers[REG_PHY_ED_LEVEL as usize] = 0xff;
        self.registers[REG_PHY_CC_CCA as usize] = self.channel & 0x1f;
        // The silicon starts with every interrupt masked; a driver writes the
        // mask it cares about. Keeping that reset value means the IRQ pin is
        // quiet until the driver configures it.
        self.irq_mask = 0;
        self.registers[REG_IRQ_MASK as usize] = 0;
        self.registers[REG_PART_NUM as usize] = PART_NUM_AT86RF233;
        self.registers[REG_VERSION_NUM as usize] = VERSION_NUM_AT86RF233;
        self.registers[REG_MAN_ID_0 as usize] = 0x1f;
        self.registers[REG_MAN_ID_1 as usize] = 0x00;
        self.publish_address_registers();
        self.state = TRX_STATUS_TRX_OFF;
        self.last_command = TRX_CMD_NOP;
        self.irq_status = 0;
        self.crc_valid = false;
        self.frame = [0; MAX_PSDU + 1];
        self.frame_len = 0;
        self.frame_written = 0;
        self.fb_read_ptr = 0;
        self.fb_write_pos = 0;
        self.access = Access::Idle;
        self.register_done = false;
        self.tx_done_at = None;
        self.publish_address();
    }

    fn publish_address_registers(&mut self) {
        self.registers[REG_MAC_SHORT_ADDR_0 as usize] = self.short_address as u8;
        self.registers[REG_MAC_SHORT_ADDR_1 as usize] = (self.short_address >> 8) as u8;
        self.registers[REG_MAC_PAN_ID_0 as usize] = self.pan as u8;
        self.registers[REG_MAC_PAN_ID_1 as usize] = (self.pan >> 8) as u8;
        for (offset, byte) in self.extended_address.iter().enumerate() {
            self.registers[REG_MAC_IEEE_ADDR_0 as usize + offset] = *byte;
        }
    }

    fn publish_address(&mut self) {
        let address = hub_address(self.pan, self.short_address);
        self.hub.borrow_mut().set_address(self.radio, address);
    }

    fn receiving(&self) -> bool {
        self.state == TRX_STATUS_RX_ON
    }

    fn pending_irq(&self) -> u8 {
        if self.registers[REG_TRX_CTRL_1 as usize] & TRX_CTRL_1_IRQ_MASK_MODE != 0 {
            self.irq_status & self.irq_mask
        } else {
            self.irq_status
        }
    }

    fn irq_level(&self) -> Level {
        let active = self.pending_irq() != 0;
        let low_pending = self.registers[REG_TRX_CTRL_1 as usize] & TRX_CTRL_1_IRQ_POLARITY != 0;
        let high = active != low_pending;
        if high { Level::High } else { Level::Low }
    }

    fn peek_register(&self, addr: u8) -> u8 {
        match addr & SPI_ADDR_MASK {
            REG_TRX_STATUS => self.state & 0x1f,
            REG_TRX_STATE => self.registers[REG_TRX_STATE as usize] & TRX_CMD_MASK,
            REG_PHY_RSSI => 0x60 | (if self.crc_valid { 0x80 } else { 0 }),
            REG_IRQ_STATUS => self.irq_status,
            _ => self.registers[(addr & SPI_ADDR_MASK) as usize],
        }
    }

    fn read_register(&mut self, addr: u8) -> u8 {
        let value = self.peek_register(addr);
        if addr & SPI_ADDR_MASK == REG_IRQ_STATUS {
            // IRQ_STATUS is read-to-clear on the silicon.
            self.irq_status = 0;
        }
        value
    }

    fn write_register(&mut self, addr: u8, value: u8) {
        match addr & SPI_ADDR_MASK {
            REG_TRX_STATUS | REG_IRQ_STATUS => {
                // Read-only; a write is ignored.
            }
            REG_TRX_STATE => self.apply_command(value),
            REG_TRX_CTRL_1 => self.registers[REG_TRX_CTRL_1 as usize] = value,
            REG_PHY_CC_CCA => {
                self.registers[REG_PHY_CC_CCA as usize] = value;
                let channel = value & 0x1f;
                if (CHANNEL_MIN..=CHANNEL_MAX).contains(&channel) {
                    self.channel = channel;
                    self.hub
                        .borrow_mut()
                        .set_channel(self.radio, u16::from(channel));
                }
            }
            REG_IRQ_MASK => {
                self.registers[REG_IRQ_MASK as usize] = value;
                self.irq_mask = value;
            }
            REG_MAC_SHORT_ADDR_0 => {
                self.registers[REG_MAC_SHORT_ADDR_0 as usize] = value;
                self.short_address = (self.short_address & 0xff00) | u16::from(value);
                self.publish_address();
            }
            REG_MAC_SHORT_ADDR_1 => {
                self.registers[REG_MAC_SHORT_ADDR_1 as usize] = value;
                self.short_address = (self.short_address & 0x00ff) | (u16::from(value) << 8);
                self.publish_address();
            }
            REG_MAC_PAN_ID_0 => {
                self.registers[REG_MAC_PAN_ID_0 as usize] = value;
                self.pan = (self.pan & 0xff00) | u16::from(value);
                self.publish_address();
            }
            REG_MAC_PAN_ID_1 => {
                self.registers[REG_MAC_PAN_ID_1 as usize] = value;
                self.pan = (self.pan & 0x00ff) | (u16::from(value) << 8);
                self.publish_address();
            }
            REG_MAC_IEEE_ADDR_0..=0x2b => {
                let offset = (addr - REG_MAC_IEEE_ADDR_0) as usize;
                self.extended_address[offset] = value;
                self.registers[(addr & SPI_ADDR_MASK) as usize] = value;
            }
            _ => self.registers[(addr & SPI_ADDR_MASK) as usize] = value,
        }
    }

    fn apply_command(&mut self, value: u8) {
        let command = value & TRX_CMD_MASK;
        self.registers[REG_TRX_STATE as usize] = command;
        self.last_command = command;
        match command {
            TRX_CMD_TX_START => {
                if self.state == TRX_STATUS_PLL_ON {
                    self.start_transmit();
                }
            }
            TRX_CMD_FORCE_TRX_OFF | TRX_CMD_TRX_OFF => self.state = TRX_STATUS_TRX_OFF,
            // Trade-off: the ARET state machine is not modelled, so its
            // PLL-ready state is the ordinary PLL_ON and its receive state is
            // the ordinary RX_ON. A driver that uses ARET still transmits and
            // receives; it just sees no automatic acknowledgement.
            TRX_CMD_FORCE_PLL_ON | TRX_CMD_PLL_ON | TRX_CMD_TX_ARET_ON => {
                self.state = TRX_STATUS_PLL_ON
            }
            TRX_CMD_RX_ON | TRX_CMD_RX_AACK_ON => self.state = TRX_STATUS_RX_ON,
            TRX_CMD_SLEEP => self.state = TRX_STATUS_SLEEP,
            _ => {}
        }
    }

    fn start_transmit(&mut self) {
        if self.tx_done_at.is_some() {
            return;
        }
        let declared = self.frame_len as usize;
        let written = self.frame_written as usize;
        if declared == 0 {
            return;
        }
        let mut psdu = Vec::with_capacity(declared);
        if self.registers[REG_TRX_CTRL_1 as usize] & TRX_CTRL_1_TX_AUTO_CRC_ON != 0
            && written + 2 == declared
        {
            // The driver wrote the header and payload and left the FCS to the
            // radio, which is the reset configuration of the real part.
            psdu.extend_from_slice(&self.frame[1..1 + written]);
            let crc = litz_ieee802154::fcs(&psdu);
            psdu.extend_from_slice(&crc.to_le_bytes());
        } else {
            // Otherwise the buffer already holds the complete PSDU. A buffer
            // that stops early is sent as written; a receiver's FCS check
            // decides what to do with it.
            let len = written.min(declared);
            psdu.extend_from_slice(&self.frame[1..1 + len]);
        }
        if psdu.is_empty() {
            return;
        }
        let airtime = litz_ieee802154::airtime_ns(psdu.len());
        let to = self.destination(&psdu);
        self.hub
            .borrow_mut()
            .transmit(self.radio, self.now, &psdu, airtime, to.as_deref());
        self.tx_done_at = Some(self.now.saturating_add(airtime));
        self.state = TRX_STATUS_BUSY_TX;
        self.tx_count += 1;
        remember(&mut self.last, &mut self.last_len, &psdu);
    }

    /// The hub destination for a frame, or `None` for a broadcast or an
    /// unreadable frame. A frame whose FCS is broken still leaves the antenna;
    /// the receiver is what drops it.
    fn destination(&self, psdu: &[u8]) -> Option<Vec<u8>> {
        match Frame::decode(psdu) {
            Ok(frame) => match frame.destination {
                Address::Short(BROADCAST_SHORT) => None,
                Address::Short(short) => Some(hub_address(frame.destination_pan, short)),
                // An extended destination cannot be expressed as the
                // receiver's PAN + short hub address, so the frame goes to
                // everyone on the channel and each module filters it.
                Address::Extended(_) | Address::None => None,
            },
            Err(_) => None,
        }
    }

    fn accepts(&self, frame: &Frame) -> bool {
        if frame.destination_pan != self.pan {
            return false;
        }
        match frame.destination {
            Address::Short(BROADCAST_SHORT) => true,
            Address::Short(short) => short == self.short_address,
            Address::Extended(address) => address == self.extended_address,
            Address::None => false,
        }
    }

    fn deliver(&mut self, bytes: &[u8]) {
        let frame = match Frame::decode(bytes) {
            Ok(frame) => frame,
            Err(_) => {
                // The FCS did not hold (or the header is outside this model).
                // The silicon reports CRC-invalid and the frame filter drops
                // it; the buffer is left untouched so a driver cannot read a
                // frame the medium already rejected.
                self.crc_valid = false;
                return;
            }
        };
        self.crc_valid = true;
        if !self.accepts(&frame) {
            return;
        }
        let len = bytes.len().min(MAX_PSDU);
        self.frame[0] = len as u8;
        if len > 0 {
            self.frame[1..=len].copy_from_slice(&bytes[..len]);
        }
        self.frame_len = len as u8;
        self.frame_written = len as u8;
        self.irq_status |= IRQ_RX_START | IRQ_TRX_END;
        self.rx_count += 1;
        remember(&mut self.last, &mut self.last_len, bytes);
    }

    fn on_command(&mut self, command: u8) {
        self.access = Access::Idle;
        self.register_done = false;
        if command & SPI_ACCESS_REG != 0 {
            self.register_addr = command & SPI_ADDR_MASK;
            if command & SPI_ACCESS_WRITE != 0 {
                self.access = Access::RegisterWrite;
                self.spi.push(0);
            } else {
                self.access = Access::RegisterRead;
                let value = self.read_register(self.register_addr);
                self.spi.push(value);
            }
        } else if command & SPI_ACCESS_FB != 0 {
            if command & SPI_ACCESS_WRITE != 0 {
                self.access = Access::FrameWrite;
                self.fb_write_pos = 0;
                self.frame_len = 0;
                self.frame_written = 0;
                self.spi.push(0);
            } else {
                self.access = Access::FrameRead;
                self.fb_read_ptr = 0;
                let value = self.frame[0];
                self.spi.push(value);
                self.fb_read_ptr = 1;
            }
        } else {
            // SRAM and AES accesses are outside this model; they answer zero
            // rather than pretending to hold data.
            self.spi.push(0);
        }
    }

    fn on_data(&mut self, byte: u8) {
        match self.access {
            Access::RegisterRead => self.spi.push(0),
            Access::RegisterWrite => {
                if !self.register_done {
                    self.write_register(self.register_addr, byte);
                    self.register_done = true;
                }
                self.spi.push(0);
            }
            Access::FrameRead => {
                let index = self.fb_read_ptr as usize;
                let value = if index <= self.frame_written as usize {
                    self.frame[index.min(self.frame.len() - 1)]
                } else {
                    0
                };
                self.spi.push(value);
                self.fb_read_ptr = self.fb_read_ptr.saturating_add(1);
            }
            Access::FrameWrite => {
                if self.fb_write_pos == 0 {
                    // PHR: the number of PSDU bytes that follow.
                    self.frame_len = byte.min(MAX_PSDU as u8);
                    self.frame[0] = self.frame_len;
                    self.fb_write_pos = 1;
                    self.frame_written = 0;
                } else {
                    let index = self.fb_write_pos as usize;
                    if index < self.frame.len() {
                        self.frame[index] = byte;
                        if index <= self.frame_len as usize {
                            self.frame_written = self.frame_written.saturating_add(1);
                        }
                    }
                    self.fb_write_pos = self.fb_write_pos.saturating_add(1);
                }
                self.spi.push(0);
            }
            Access::Idle => self.spi.push(0),
        }
    }
}

impl Part for At86rf233 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.irq {
            return Drive::strong(self.irq_level());
        }
        if pin.0 == self.spi.wires.miso {
            return self.spi.miso_drive();
        }
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        self.now = now;
        if pin.0 == self.rstn {
            let low = level == Level::Low;
            if low && !self.rstn_low {
                self.reset();
                self.state = TRX_STATUS_RESET;
            } else if !low && self.rstn_low {
                self.state = TRX_STATUS_TRX_OFF;
            }
            self.rstn_low = low;
            return;
        }
        if pin.0 == self.slptr {
            let high = level == Level::High;
            if high && !self.slptr_high && self.state == TRX_STATUS_TRX_OFF {
                self.state = TRX_STATUS_SLEEP;
            } else if !high && self.slptr_high && self.state == TRX_STATUS_SLEEP {
                self.state = TRX_STATUS_TRX_OFF;
            }
            self.slptr_high = high;
            return;
        }
        if let Some(event) = self.spi.set_pin(pin.0, level) {
            match event {
                SpiEvent::Selected => {
                    self.spi_first = true;
                    self.spi.push(0);
                }
                SpiEvent::Deselected => {
                    self.spi_first = true;
                    self.access = Access::Idle;
                }
                SpiEvent::Byte(byte) => {
                    if self.spi_first {
                        self.spi_first = false;
                        self.on_command(byte);
                    } else {
                        self.on_data(byte);
                    }
                }
            }
        }
    }

    fn next_event(&self) -> Option<Tick> {
        let mut next = self.tx_done_at;
        if self.receiving()
            && let Some(at) = self.hub.borrow().next_delivery(self.radio)
        {
            next = Some(next.map_or(at, |scheduled| scheduled.min(at)));
        }
        next
    }

    fn advance(&mut self, now: Tick) {
        self.now = now;
        if let Some(at) = self.tx_done_at
            && now >= at
        {
            self.tx_done_at = None;
            self.state = TRX_STATUS_PLL_ON;
            self.irq_status |= IRQ_TRX_END;
        }
        if !self.receiving() {
            return;
        }
        let packets = self.hub.borrow_mut().inbox(self.radio, now);
        for packet in packets {
            self.deliver(&packet.bytes);
        }
    }

    fn state(&self) -> PartState {
        radio_state(self.tx_count, self.rx_count, &self.last, self.last_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extended_address_attribute_is_written_low_byte_first() {
        assert_eq!(
            parse_extended("0011223344556677"),
            [0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00]
        );
        assert_eq!(
            parse_extended("0x42"),
            [0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn the_hub_address_is_a_pan_and_a_short_address() {
        assert_eq!(hub_address(0xabcd, 0x0001), vec![0xcd, 0xab, 0x01, 0x00]);
    }
}
