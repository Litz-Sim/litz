//! Pin-level packet radios that share one deterministic medium.
//!
//! Both models are SPI slaves decoded from the edges on SCK/MOSI/NSS, not from
//! a host SPI peripheral. They speak mode 0 (CPOL 0, CPHA 0), MSB first, eight
//! bits per transfer, and a chip select brackets each transaction. That is the
//! shape a driver written against the real silicon produces, so the models
//! answer the register sequences such a driver sends rather than a made-up
//! transaction API.
//!
//! Neither model touches a wall clock. A frame is scheduled at the tick of the
//! SPI write that armed it, and [`RadioHub`](litz_radio::RadioHub) delivers it
//! when its airtime has elapsed.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, RADIO_LAST_BYTES, Tick};
use litz_radio::{RadioConfig, RadioHubHandle, RadioId};
use std::collections::VecDeque;

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

fn attr_f32(part: &FlatPart, name: &str, default: f32) -> f32 {
    part.attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
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

// -------------------------------------------------------------- SPI decoding

/// The four wires of a mode-0 SPI slave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpiWires {
    sck: u16,
    mosi: u16,
    miso: u16,
    nss: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpiEvent {
    Selected,
    Deselected,
    Byte(u8),
}

/// A byte-oriented SPI slave decoder.
///
/// In mode 0 the master changes MOSI while SCK is low and samples MISO on the
/// rising edge; the slave samples MOSI on that same edge and advances its
/// output on the falling edge. Eight rising edges make a byte. The response
/// for the next transfer is whatever the part queues after the previous byte
/// completes, which is exactly full-duplex timing.
struct SpiSlave {
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

impl SpiSlave {
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
        if pin == self.wires.nss {
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

    /// Advance the output one bit on a falling edge. The first bit is loaded
    /// with the byte, so this presents bit 6 after the first sample and
    /// releases the line after the eighth.
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

// ------------------------------------------------------------------- SX1276

const SX_FIFO: u8 = 0x00;
const SX_OP_MODE: u8 = 0x01;
const SX_FRF_MSB: u8 = 0x06;
const SX_FRF_MID: u8 = 0x07;
const SX_FRF_LSB: u8 = 0x08;
const SX_PA_CONFIG: u8 = 0x09;
const SX_FIFO_ADDR_PTR: u8 = 0x0d;
const SX_FIFO_TX_BASE: u8 = 0x0e;
const SX_FIFO_RX_BASE: u8 = 0x0f;
const SX_FIFO_RX_CURRENT: u8 = 0x10;
const SX_IRQ_FLAGS_MASK: u8 = 0x11;
const SX_IRQ_FLAGS: u8 = 0x12;
const SX_RX_NB_BYTES: u8 = 0x13;
const SX_MODEM_CONFIG_1: u8 = 0x1d;
const SX_MODEM_CONFIG_2: u8 = 0x1e;
const SX_PREAMBLE_MSB: u8 = 0x20;
const SX_PREAMBLE_LSB: u8 = 0x21;
const SX_PAYLOAD_LENGTH: u8 = 0x22;
const SX_MODEM_CONFIG_3: u8 = 0x26;
const SX_SYNC_WORD: u8 = 0x39;
const SX_VERSION: u8 = 0x42;

const SX_IRQ_RX_DONE: u8 = 0x40;
const SX_IRQ_VALID_HEADER: u8 = 0x10;
const SX_IRQ_TX_DONE: u8 = 0x08;

const SX_MODE_STANDBY: u8 = 1;
const SX_MODE_TX: u8 = 2;
const SX_MODE_RX_CONTINUOUS: u8 = 3;
const SX_MODE_RX_SINGLE: u8 = 4;

/// An SX1276 LoRa transceiver, subset of the register map a LoRa driver uses.
///
/// The model answers the identity register, keeps the configuration registers
/// a driver reads back, moves bytes through the 256-byte FIFO, and turns the
/// FIFO/RX/TX sequence into packets on the shared medium. It does not model
/// RSSI, frequency synthesis, hopping, CAD, spreading-factor auto-detection or
/// the analog front end; the attributes describe the link and the airtime.
pub struct Sx1276 {
    spi: SpiSlave,
    reset: u16,
    dio0: u16,
    dio1: u16,
    hub: RadioHubHandle,
    radio: RadioId,
    /// Attributes that set the link at construction. Register writes change
    /// the airtime; the channel stays the frequency the part was built with
    /// unless Frf is written.
    sf: u8,
    bandwidth_khz: f32,
    frequency_mhz: f32,
    power: i8,
    registers: [u8; 0x80],
    fifo: [u8; 256],
    fifo_ptr: u8,
    fifo_written: bool,
    spi_first: bool,
    spi_write: bool,
    spi_addr: u8,
    reset_level: Level,
    now: Tick,
    tx_done_at: Option<Tick>,
    tx_count: u32,
    rx_count: u32,
    last: [u8; RADIO_LAST_BYTES],
    last_len: u8,
}

impl Sx1276 {
    pub fn build(part: &FlatPart, radios: &RadioHubHandle) -> Result<Box<dyn Part>, BuildError> {
        let [sck, mosi, miso, nss, reset, dio0, dio1] = pins(
            part,
            ["SCK", "MOSI", "MISO", "NSS", "RESET", "DIO0", "DIO1"],
        )?;
        let network = attr_text(part, "network", "lora");
        let frequency = attr_f32(part, "frequency", 868.0).max(1.0);
        let sf = attr_u16(part, "sf", 7).clamp(7, 12) as u8;
        let bandwidth_khz = attr_f32(part, "bandwidth", 125.0).max(7.8);
        let power = attr_i8(part, "power", 14);
        let radio = radios.borrow_mut().register(RadioConfig {
            network,
            channel: frequency.round() as u16,
            address: Vec::new(),
            power_dbm: power,
            range_m: None,
        });
        let mut part = Self {
            spi: SpiSlave::new(SpiWires {
                sck,
                mosi,
                miso,
                nss,
            }),
            reset,
            dio0,
            dio1,
            hub: radios.clone(),
            radio,
            sf,
            bandwidth_khz,
            frequency_mhz: frequency,
            power,
            registers: [0; 0x80],
            fifo: [0; 256],
            fifo_ptr: 0,
            fifo_written: false,
            spi_first: true,
            spi_write: false,
            spi_addr: 0,
            reset_level: Level::HighZ,
            now: 0,
            tx_done_at: None,
            tx_count: 0,
            rx_count: 0,
            last: [0; RADIO_LAST_BYTES],
            last_len: 0,
        };
        part.reset_registers();
        Ok(Box::new(part))
    }

    fn reset_registers(&mut self) {
        self.registers = [0; 0x80];
        self.registers[SX_OP_MODE as usize] = 0x80; // sleep, LoRa
        self.registers[SX_PA_CONFIG as usize] = 0x80 | (self.power.max(0) as u8 & 0x0f);
        self.registers[SX_MODEM_CONFIG_1 as usize] =
            (bandwidth_code(self.bandwidth_khz) << 4) | (1 << 1); // 4/5 coding
        self.registers[SX_MODEM_CONFIG_2 as usize] = (self.sf << 4) | 0x04; // CRC on
        self.registers[SX_MODEM_CONFIG_3 as usize] = 0x04; // AGC on
        self.registers[SX_SYNC_WORD as usize] = 0x12; // private LoRa
        self.registers[SX_VERSION as usize] = 0x12;
        let frequency = self.frequency_mhz;
        self.set_frf(frequency);
        self.set_preamble(8);
        self.registers[SX_PAYLOAD_LENGTH as usize] = 0;
        self.fifo = [0; 256];
        self.fifo_ptr = 0;
        self.fifo_written = false;
        self.tx_done_at = None;
    }

    fn frequency_mhz(&self) -> f32 {
        let frf = (u32::from(self.registers[SX_FRF_MSB as usize]) << 16
            | u32::from(self.registers[SX_FRF_MID as usize]) << 8
            | u32::from(self.registers[SX_FRF_LSB as usize])) as f32;
        if frf == 0.0 {
            // A hardware reset has no carrier yet; keep the attribute's.
            self.frequency_mhz
        } else {
            // Frf counts 32 MHz / 2^19 ≈ 61.035 Hz per step.
            frf * 61.035_156 / 1_000_000.0
        }
    }

    fn set_frf(&mut self, frequency_mhz: f32) {
        let frf = (frequency_mhz * 16_384.0).round() as u32;
        self.registers[SX_FRF_MSB as usize] = (frf >> 16) as u8;
        self.registers[SX_FRF_MID as usize] = (frf >> 8) as u8;
        self.registers[SX_FRF_LSB as usize] = frf as u8;
    }

    fn set_preamble(&mut self, symbols: u16) {
        self.registers[SX_PREAMBLE_MSB as usize] = (symbols >> 8) as u8;
        self.registers[SX_PREAMBLE_LSB as usize] = symbols as u8;
    }

    fn preamble(&self) -> u16 {
        u16::from(self.registers[SX_PREAMBLE_MSB as usize]) << 8
            | u16::from(self.registers[SX_PREAMBLE_LSB as usize])
    }

    fn effective_sf(&self) -> u8 {
        let register = (self.registers[SX_MODEM_CONFIG_2 as usize] >> 4) & 0x0f;
        if (6..=12).contains(&register) {
            register
        } else {
            self.sf
        }
    }

    fn effective_bandwidth_khz(&self) -> f32 {
        let code = (self.registers[SX_MODEM_CONFIG_1 as usize] >> 4) & 0x0f;
        bandwidth_from_code(code).unwrap_or(self.bandwidth_khz)
    }

    /// LoRa time-on-air, in nanoseconds.
    ///
    /// `T_sym = 2^SF / BW`. A frame is a preamble of `preamble + 4.25` symbols
    /// followed by
    ///
    /// ```text
    /// n_payload = 8 + ceil((8*PL - 4*SF + 28 + 16*CRC - 20*IH)
    ///                      / (4*(SF - 2*DE))) * (CR + 4)
    /// ```
    ///
    /// symbols, where `DE` is the low-data-rate optimisation (symbol time over
    /// 16 ms), `IH` the implicit-header bit and `CR` the coding rate. That is
    /// the standard Semtech formula, with the fractional preamble and symbol
    /// time carried in floating point.
    fn airtime_ns(&self, payload_len: u8) -> u64 {
        let sf = self.effective_sf();
        let bandwidth_hz = (self.effective_bandwidth_khz() * 1000.0).max(1.0);
        let symbol_s = f64::from(1u32 << sf) / f64::from(bandwidth_hz);
        let coding_rate =
            i32::from(((self.registers[SX_MODEM_CONFIG_1 as usize] >> 1) & 0x07) + 4).clamp(5, 8);
        let crc = i32::from(self.registers[SX_MODEM_CONFIG_2 as usize] & 0x04 != 0);
        let implicit_header = i32::from(self.registers[SX_MODEM_CONFIG_1 as usize] & 0x01 != 0);
        let low_rate = i32::from(symbol_s > 0.016);
        let numerator =
            8 * i32::from(payload_len) - 4 * i32::from(sf) + 28 + 16 * crc - 20 * implicit_header;
        let denominator = 4 * (i32::from(sf) - 2 * low_rate);
        let blocks = if denominator > 0 {
            (f64::from(numerator) / f64::from(denominator))
                .ceil()
                .max(0.0)
        } else {
            0.0
        };
        let payload_symbols = 8.0 + blocks * f64::from(coding_rate);
        let preamble_symbols = f64::from(self.preamble()) + 4.25;
        let seconds = (preamble_symbols + payload_symbols) * symbol_s;
        (seconds * 1e9).ceil().max(1.0) as u64
    }

    fn rx_mode(&self) -> bool {
        matches!(
            self.registers[SX_OP_MODE as usize] & 0x07,
            SX_MODE_RX_CONTINUOUS | SX_MODE_RX_SINGLE
        )
    }

    fn unmasked_irq(&self) -> u8 {
        self.registers[SX_IRQ_FLAGS as usize] & !self.registers[SX_IRQ_FLAGS_MASK as usize]
    }

    fn peek_register(&self, addr: u8) -> u8 {
        match addr {
            SX_FIFO => self.fifo[self.fifo_ptr as usize],
            SX_VERSION => 0x12,
            _ => self.registers[addr as usize & 0x7f],
        }
    }

    fn write_register(&mut self, addr: u8, value: u8) {
        match addr {
            SX_FIFO => {
                self.fifo[self.fifo_ptr as usize] = value;
                self.fifo_ptr = self.fifo_ptr.wrapping_add(1);
                self.fifo_written = true;
            }
            SX_OP_MODE => self.apply_op_mode(value),
            SX_FRF_LSB => {
                self.registers[addr as usize] = value;
                let frequency = self.frequency_mhz();
                if frequency > 0.0 {
                    self.hub
                        .borrow_mut()
                        .set_channel(self.radio, frequency.round() as u16);
                }
            }
            SX_IRQ_FLAGS => {
                // Writing one clears that flag, as the real part does.
                let current = self.registers[addr as usize];
                self.registers[addr as usize] = current & !value;
            }
            SX_FIFO_ADDR_PTR => {
                self.registers[addr as usize] = value;
                self.fifo_ptr = value;
            }
            _ => self.registers[addr as usize & 0x7f] = value,
        }
    }

    fn apply_op_mode(&mut self, value: u8) {
        match value & 0x07 {
            SX_MODE_TX => {
                self.start_transmit();
                // A plain TX request returns to standby once the frame is
                // queued; TX_CONTINUOUS (not modelled) is not in the subset.
                self.registers[SX_OP_MODE as usize] = (value & !0x07) | SX_MODE_STANDBY;
            }
            _ => self.registers[SX_OP_MODE as usize] = value,
        }
    }

    fn start_transmit(&mut self) {
        let length = usize::from(self.registers[SX_PAYLOAD_LENGTH as usize]);
        if length == 0 || !self.fifo_written {
            return;
        }
        let base = self.registers[SX_FIFO_TX_BASE as usize] as usize;
        let payload: Vec<u8> = (0..length)
            .map(|offset| self.fifo[(base + offset) % self.fifo.len()])
            .collect();
        let airtime = self.airtime_ns(length.min(255) as u8);
        self.hub
            .borrow_mut()
            .transmit(self.radio, self.now, &payload, airtime, None);
        self.tx_done_at = Some(self.now.saturating_add(airtime));
        self.tx_count += 1;
        remember(&mut self.last, &mut self.last_len, &payload);
        self.fifo_written = false;
    }

    fn deliver(&mut self, packet: litz_radio::Packet) {
        let base = self.registers[SX_FIFO_RX_BASE as usize] as usize;
        for (offset, byte) in packet.bytes.iter().enumerate() {
            self.fifo[(base + offset) % self.fifo.len()] = *byte;
        }
        self.registers[SX_FIFO_RX_CURRENT as usize] = base as u8;
        self.registers[SX_RX_NB_BYTES as usize] = packet.bytes.len().min(255) as u8;
        self.registers[SX_IRQ_FLAGS as usize] |= SX_IRQ_RX_DONE | SX_IRQ_VALID_HEADER;
        self.rx_count += 1;
        remember(&mut self.last, &mut self.last_len, &packet.bytes);
        if self.registers[SX_OP_MODE as usize] & 0x07 == SX_MODE_RX_SINGLE {
            self.registers[SX_OP_MODE as usize] =
                (self.registers[SX_OP_MODE as usize] & !0x07) | SX_MODE_STANDBY;
        }
    }

    fn on_spi_byte(&mut self, byte: u8) {
        if self.spi_first {
            self.spi_first = false;
            self.spi_write = byte & 0x80 != 0;
            self.spi_addr = byte & 0x7f;
            if self.spi_write {
                self.spi.push(0);
            } else {
                let value = self.peek_register(self.spi_addr);
                self.spi.push(value);
            }
            return;
        }
        if self.spi_write {
            self.write_register(self.spi_addr, byte);
            // The FIFO is one address with an internal pointer: a burst keeps
            // reading or writing it while the pointer walks, as on the part.
            if self.spi_addr != SX_FIFO {
                self.spi_addr = (self.spi_addr + 1) & 0x7f;
            }
            self.spi.push(0);
        } else {
            if self.spi_addr == SX_FIFO {
                self.fifo_ptr = self.fifo_ptr.wrapping_add(1);
            } else {
                self.spi_addr = (self.spi_addr + 1) & 0x7f;
            }
            let value = self.peek_register(self.spi_addr);
            self.spi.push(value);
        }
    }
}

fn bandwidth_code(khz: f32) -> u8 {
    const CODES: [f32; 10] = [
        7.8, 10.4, 15.6, 20.8, 31.25, 41.7, 62.5, 125.0, 250.0, 500.0,
    ];
    let mut best = 7u8;
    let mut distance = f32::MAX;
    for (code, value) in CODES.iter().enumerate() {
        let delta = (value - khz).abs();
        if delta < distance {
            distance = delta;
            best = code as u8;
        }
    }
    best
}

fn bandwidth_from_code(code: u8) -> Option<f32> {
    const CODES: [f32; 10] = [
        7.8, 10.4, 15.6, 20.8, 31.25, 41.7, 62.5, 125.0, 250.0, 500.0,
    ];
    CODES.get(code as usize).copied()
}

impl Part for Sx1276 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dio0 {
            return Drive::strong(if self.unmasked_irq() & SX_IRQ_RX_DONE != 0 {
                Level::High
            } else {
                Level::Low
            });
        }
        if pin.0 == self.dio1 {
            return Drive::strong(if self.unmasked_irq() & SX_IRQ_TX_DONE != 0 {
                Level::High
            } else {
                Level::Low
            });
        }
        if pin.0 == self.spi.wires.miso {
            return self.spi.miso_drive();
        }
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        self.now = now;
        if pin.0 == self.reset {
            let rising = level == Level::High && self.reset_level != Level::High;
            self.reset_level = level;
            if rising {
                self.reset_registers();
            }
            return;
        }
        if let Some(event) = self.spi.set_pin(pin.0, level) {
            match event {
                SpiEvent::Selected => {
                    self.spi_first = true;
                    self.spi.push(0x00);
                }
                SpiEvent::Deselected => self.spi_first = true,
                SpiEvent::Byte(byte) => self.on_spi_byte(byte),
            }
        }
    }

    fn next_event(&self) -> Option<Tick> {
        let mut next = self.tx_done_at;
        if self.rx_mode()
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
            self.registers[SX_IRQ_FLAGS as usize] |= SX_IRQ_TX_DONE;
        }
        if !self.rx_mode() {
            return;
        }
        let packets = self.hub.borrow_mut().inbox(self.radio, now);
        if let Some(packet) = packets.into_iter().last() {
            self.deliver(packet);
        }
    }

    fn state(&self) -> PartState {
        radio_state(self.tx_count, self.rx_count, &self.last, self.last_len)
    }
}

// ---------------------------------------------------------------- NRF24L01

const NRF_CONFIG: u8 = 0x00;
const NRF_EN_AA: u8 = 0x01;
const NRF_EN_RXADDR: u8 = 0x02;
const NRF_SETUP_AW: u8 = 0x03;
const NRF_SETUP_RETR: u8 = 0x04;
const NRF_RF_CH: u8 = 0x05;
const NRF_RF_SETUP: u8 = 0x06;
const NRF_STATUS: u8 = 0x07;
const NRF_OBSERVE_TX: u8 = 0x08;
const NRF_RPD: u8 = 0x09;
const NRF_RX_ADDR_P0: u8 = 0x0a;
const NRF_RX_ADDR_P1: u8 = 0x0b;
const NRF_TX_ADDR: u8 = 0x10;
const NRF_RX_PW_P0: u8 = 0x11;
const NRF_FIFO_STATUS: u8 = 0x17;
const NRF_FEATURE: u8 = 0x1d;

const NRF_CMD_R_REGISTER: u8 = 0x00;
const NRF_CMD_W_REGISTER: u8 = 0x20;
const NRF_CMD_R_RX_PAYLOAD: u8 = 0x61;
const NRF_CMD_W_TX_PAYLOAD: u8 = 0xa0;
const NRF_CMD_FLUSH_TX: u8 = 0xe1;
const NRF_CMD_FLUSH_RX: u8 = 0xe2;
const NRF_CMD_ACTIVATE: u8 = 0x50;
const NRF_CMD_R_RX_PL_WID: u8 = 0x60;
const NRF_CMD_W_TX_PAYLOAD_NOACK: u8 = 0xb0;

const NRF_STATUS_RX_DR: u8 = 0x40;
const NRF_STATUS_TX_DS: u8 = 0x20;
/// In auto-acknowledgement mode, the transmitter never retransmits a frame
/// here, so the bit is defined and cleared like the others but never rises.
const NRF_STATUS_MAX_RT: u8 = 0x10;
const NRF_STATUS_FLAGS: u8 = NRF_STATUS_RX_DR | NRF_STATUS_TX_DS | NRF_STATUS_MAX_RT;

/// An nRF24L01+ transceiver, command set and register subset a driver uses.
///
/// It models the one-pipe case that covers ordinary point-to-point use: pipe
/// 0's address decides what the receiver hears, TX_ADDR decides where a frame
/// goes, and RF_CH retunes the medium. The remaining pipes keep their
/// addresses and payload widths so a driver's setup sequence reads back, but
/// do not widen the receiver. Enhanced ShockBurst auto-acknowledgement,
/// retransmission, dynamic payloads and the 250 kbps rate are accepted in
/// their registers and not modelled on the air.
pub struct Nrf24l01 {
    spi: SpiSlave,
    ce: u16,
    irq: u16,
    hub: RadioHubHandle,
    radio: RadioId,
    payload_size: u8,
    registers: [u8; 0x20],
    rx_address: [[u8; 5]; 6],
    tx_address: [u8; 5],
    rx_width: [u8; 6],
    rx_queue: VecDeque<Vec<u8>>,
    tx_payload: Option<Vec<u8>>,
    tx_in_flight: bool,
    tx_done_at: Option<Tick>,
    ce_high: bool,
    rate_2m: bool,
    spi_first: bool,
    spi_read: bool,
    spi_write: bool,
    spi_write_payload: bool,
    spi_read_payload: bool,
    spi_addr: u8,
    spi_offset: u8,
    payload_buf: Vec<u8>,
    spi_rx: Option<Vec<u8>>,
    spi_rx_index: usize,
    now: Tick,
    tx_count: u32,
    rx_count: u32,
    last: [u8; RADIO_LAST_BYTES],
    last_len: u8,
}

impl Nrf24l01 {
    pub fn build(part: &FlatPart, radios: &RadioHubHandle) -> Result<Box<dyn Part>, BuildError> {
        let [sck, mosi, miso, csn, ce, irq] =
            pins(part, ["SCK", "MOSI", "MISO", "CSN", "CE", "IRQ"])?;
        let network = attr_text(part, "network", "nrf24");
        let channel = attr_u16(part, "channel", 76).min(125);
        let rate_2m = matches!(
            attr_text(part, "rate", "1M").to_ascii_lowercase().as_str(),
            "2m" | "2" | "2mbps"
        );
        let address = parse_address(&attr_text(part, "address", "e7e7e7e7e7"));
        let payload_size = attr_u16(part, "payload_size", 32).clamp(1, 32) as u8;
        let radio = radios.borrow_mut().register(RadioConfig {
            network,
            channel,
            address: address.to_vec(),
            power_dbm: 0,
            range_m: None,
        });
        let mut registers = [0u8; 0x20];
        registers[NRF_CONFIG as usize] = 0x08; // EN_CRC, powered down, TX mode
        registers[NRF_EN_AA as usize] = 0x3f;
        registers[NRF_EN_RXADDR as usize] = 0x03;
        registers[NRF_SETUP_AW as usize] = 0x03; // five-byte addresses
        registers[NRF_SETUP_RETR as usize] = 0x03;
        registers[NRF_RF_CH as usize] = channel as u8;
        registers[NRF_RF_SETUP as usize] = if rate_2m { 0x0e } else { 0x06 };
        Ok(Box::new(Self {
            spi: SpiSlave::new(SpiWires {
                sck,
                mosi,
                miso,
                nss: csn,
            }),
            ce,
            irq,
            hub: radios.clone(),
            radio,
            payload_size,
            registers,
            rx_address: [address; 6],
            tx_address: address,
            rx_width: [payload_size; 6],
            rx_queue: VecDeque::new(),
            tx_payload: None,
            tx_in_flight: false,
            tx_done_at: None,
            ce_high: false,
            rate_2m,
            spi_first: true,
            spi_read: false,
            spi_write: false,
            spi_write_payload: false,
            spi_read_payload: false,
            spi_addr: 0,
            spi_offset: 0,
            payload_buf: Vec::new(),
            spi_rx: None,
            spi_rx_index: 0,
            now: 0,
            tx_count: 0,
            rx_count: 0,
            last: [0; RADIO_LAST_BYTES],
            last_len: 0,
        }))
    }

    fn status_flags(&self) -> u8 {
        self.registers[NRF_STATUS as usize] & NRF_STATUS_FLAGS
    }

    fn status(&self) -> u8 {
        let mut status = self.status_flags();
        status |= if self.rx_queue.is_empty() { 0x0e } else { 0 };
        if self.tx_payload.is_some() {
            status |= 0x01;
        }
        status
    }

    fn fifo_status(&self) -> u8 {
        let mut status = 0u8;
        if self.rx_queue.is_empty() {
            status |= 0x01;
        }
        if self.rx_queue.len() >= 3 {
            status |= 0x02;
        }
        if self.tx_payload.is_none() {
            status |= 0x10;
        } else {
            status |= 0x20;
        }
        status
    }

    fn irq_active(&self) -> bool {
        // CONFIG's mask bits sit exactly over the three STATUS flags.
        self.status_flags() & !self.registers[NRF_CONFIG as usize] & NRF_STATUS_FLAGS != 0
    }

    fn rx_mode(&self) -> bool {
        self.registers[NRF_CONFIG as usize] & 0x01 != 0
    }

    fn read_register_at(&self, address: u8, offset: u8) -> u8 {
        match address {
            NRF_STATUS => self.status(),
            NRF_FIFO_STATUS => self.fifo_status(),
            NRF_RX_ADDR_P0 | NRF_RX_ADDR_P1 => {
                self.rx_address[(address - NRF_RX_ADDR_P0) as usize][offset.min(4) as usize]
            }
            0x0c..=0x0f => self.rx_address[(address - NRF_RX_ADDR_P0) as usize][0],
            NRF_TX_ADDR => self.tx_address[offset.min(4) as usize],
            NRF_RX_PW_P0..=0x16 => self.rx_width[(address - NRF_RX_PW_P0) as usize],
            // No auto-acknowledgement, so no packet is lost and none is
            // retransmitted: both counters in OBSERVE_TX stay at zero.
            NRF_OBSERVE_TX => 0,
            // There is no RSSI model, so the detector reports no carrier.
            NRF_RPD => 0,
            _ => self.registers[address as usize & 0x1f],
        }
    }

    fn write_register_at(&mut self, address: u8, offset: u8, value: u8) {
        match address {
            NRF_STATUS => {
                // Writing one clears that flag.
                self.registers[NRF_STATUS as usize] =
                    self.status_flags() & !value & NRF_STATUS_FLAGS;
            }
            NRF_RF_CH => {
                let channel = u16::from(value & 0x7f).min(125);
                self.registers[NRF_RF_CH as usize] = channel as u8;
                self.hub.borrow_mut().set_channel(self.radio, channel);
            }
            NRF_RF_SETUP => {
                self.registers[NRF_RF_SETUP as usize] = value;
                self.rate_2m = value & 0x08 != 0;
            }
            NRF_RX_ADDR_P0 | NRF_RX_ADDR_P1 => {
                let pipe = (address - NRF_RX_ADDR_P0) as usize;
                self.rx_address[pipe][offset.min(4) as usize] = value;
                if pipe == 0 {
                    self.hub
                        .borrow_mut()
                        .set_address(self.radio, self.rx_address[0].to_vec());
                }
            }
            0x0c..=0x0f => {
                let pipe = (address - NRF_RX_ADDR_P0) as usize;
                self.rx_address[pipe][0] = value;
            }
            NRF_TX_ADDR => self.tx_address[offset.min(4) as usize] = value,
            NRF_RX_PW_P0..=0x16 => {
                self.rx_width[(address - NRF_RX_PW_P0) as usize] = value;
            }
            _ => self.registers[address as usize & 0x1f] = value,
        }
    }

    fn airtime_ns(&self, payload_len: usize) -> u64 {
        // 130 us of startup, then preamble + 5-byte address + 9 control bits
        // + payload + 2-byte CRC. Rounding those fields to 10 bytes keeps the
        // number plausible without pretending to model the packet engine.
        let bits = (payload_len + 10) * 8;
        let rate = if self.rate_2m {
            2_000_000u64
        } else {
            1_000_000
        };
        130_000 + (bits as u64 * 1_000_000_000) / rate
    }

    fn start_transmit(&mut self) {
        if self.tx_in_flight {
            return;
        }
        let Some(payload) = self.tx_payload.clone() else {
            return;
        };
        let airtime = self.airtime_ns(payload.len());
        self.hub.borrow_mut().transmit(
            self.radio,
            self.now,
            &payload,
            airtime,
            Some(&self.tx_address),
        );
        self.tx_in_flight = true;
        self.tx_done_at = Some(self.now.saturating_add(airtime));
        self.tx_count += 1;
        remember(&mut self.last, &mut self.last_len, &payload);
    }

    fn next_rx_byte(&mut self) -> u8 {
        let byte = self
            .spi_rx
            .as_ref()
            .and_then(|payload| payload.get(self.spi_rx_index))
            .copied()
            .unwrap_or(0);
        self.spi_rx_index += 1;
        byte
    }

    fn on_command(&mut self, command: u8) {
        self.spi_read = false;
        self.spi_write = false;
        self.spi_write_payload = false;
        self.spi_read_payload = false;
        self.spi_addr = command & 0x1f;
        self.spi_offset = 0;
        match command & 0xe0 {
            NRF_CMD_R_REGISTER => {
                self.spi_read = true;
                let value = self.read_register_at(self.spi_addr, 0);
                self.spi.push(value);
            }
            NRF_CMD_W_REGISTER => {
                self.spi_write = true;
                self.spi.push(0);
            }
            _ => match command {
                NRF_CMD_R_RX_PAYLOAD => {
                    self.spi_read_payload = true;
                    self.spi_rx = self.rx_queue.pop_front();
                    let byte = self.next_rx_byte();
                    self.spi.push(byte);
                }
                NRF_CMD_W_TX_PAYLOAD | NRF_CMD_W_TX_PAYLOAD_NOACK => {
                    self.spi_write_payload = true;
                    self.payload_buf.clear();
                    self.spi.push(0);
                }
                NRF_CMD_FLUSH_TX => {
                    if !self.tx_in_flight {
                        self.tx_payload = None;
                    }
                }
                NRF_CMD_FLUSH_RX => self.rx_queue.clear(),
                NRF_CMD_ACTIVATE => self.registers[NRF_FEATURE as usize] |= 0x01,
                NRF_CMD_R_RX_PL_WID => {
                    let width = self
                        .rx_queue
                        .front()
                        .map_or(0, |payload| payload.len() as u8);
                    self.spi.push(width);
                }
                _ => self.spi.push(0),
            },
        }
    }

    fn on_spi_byte(&mut self, byte: u8) {
        if self.spi_first {
            self.spi_first = false;
            self.on_command(byte);
            return;
        }
        if self.spi_read {
            self.spi_offset = self.spi_offset.saturating_add(1);
            let value = self.read_register_at(self.spi_addr, self.spi_offset);
            self.spi.push(value);
        } else if self.spi_write {
            self.write_register_at(self.spi_addr, self.spi_offset, byte);
            self.spi_offset = self.spi_offset.saturating_add(1);
            self.spi.push(0);
        } else if self.spi_write_payload {
            if self.payload_buf.len() < usize::from(self.payload_size) {
                self.payload_buf.push(byte);
            }
            if self.payload_buf.len() == usize::from(self.payload_size) {
                self.tx_payload = Some(std::mem::take(&mut self.payload_buf));
                if self.ce_high {
                    self.start_transmit();
                }
            }
            self.spi.push(0);
        } else if self.spi_read_payload {
            let next = self.next_rx_byte();
            self.spi.push(next);
        } else {
            self.spi.push(0);
        }
    }
}

fn parse_address(text: &str) -> [u8; 5] {
    let mut address = [0u8; 5];
    let clean = text.trim().trim_start_matches("0x");
    let mut bytes = clean.as_bytes().chunks(2).filter_map(|pair| {
        let text = std::str::from_utf8(pair).ok()?;
        u8::from_str_radix(text, 16).ok()
    });
    for slot in &mut address {
        match bytes.next() {
            Some(byte) => *slot = byte,
            None => break,
        }
    }
    address
}

impl Part for Nrf24l01 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.irq {
            // The nRF24 IRQ is active low: it falls on RX_DR, TX_DS or MAX_RT.
            return Drive::strong(if self.irq_active() {
                Level::Low
            } else {
                Level::High
            });
        }
        if pin.0 == self.spi.wires.miso {
            return self.spi.miso_drive();
        }
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        self.now = now;
        if pin.0 == self.ce {
            let high = level == Level::High;
            if high && !self.ce_high {
                self.ce_high = true;
                self.start_transmit();
            } else if !high {
                self.ce_high = false;
            }
            return;
        }
        if let Some(event) = self.spi.set_pin(pin.0, level) {
            match event {
                SpiEvent::Selected => {
                    self.spi_first = true;
                    let status = self.status();
                    self.spi.push(status);
                }
                SpiEvent::Deselected => {
                    self.spi_first = true;
                    self.spi_read = false;
                    self.spi_write = false;
                    self.spi_write_payload = false;
                    self.spi_read_payload = false;
                }
                SpiEvent::Byte(byte) => self.on_spi_byte(byte),
            }
        }
    }

    fn next_event(&self) -> Option<Tick> {
        let mut next = self.tx_done_at;
        if self.rx_mode()
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
            self.tx_in_flight = false;
            self.tx_payload = None;
            self.registers[NRF_STATUS as usize] |= NRF_STATUS_TX_DS;
        }
        if !self.rx_mode() {
            return;
        }
        let packets = self.hub.borrow_mut().inbox(self.radio, now);
        for packet in packets {
            if self.rx_queue.len() >= 3 {
                break;
            }
            self.rx_queue.push_back(packet.bytes.clone());
            self.registers[NRF_STATUS as usize] |= NRF_STATUS_RX_DR;
            self.rx_count += 1;
            remember(&mut self.last, &mut self.last_len, &packet.bytes);
        }
    }

    fn state(&self) -> PartState {
        radio_state(self.tx_count, self.rx_count, &self.last, self.last_len)
    }
}
