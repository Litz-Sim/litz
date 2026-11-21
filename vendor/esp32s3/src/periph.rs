//! ESP32-S3 peripherals. The IP shared with other chips lives in `esp-periph`; here are the
//! S3-only blocks (interrupt matrix, WiFi MAC, PCNT, GP-SPI, LCD_CAM, EXTMEM, WDEV, regi2c) and
//! the `Peripherals` set whose one table (`ENTRIES`) drives dispatch, sources, ticks and deadlines.
use emu_core::{ClockDomain, ClockTree};
use esp_periph::{GpSpi, Ledc, LedcLayout, Mcpwm};
use esp_periph::{device_set, mmio, Device, DeviceSet, Dispatch, Misc, WriteEffect};
pub use esp_periph::rmt::RmtDma as Rmt;
pub use esp_periph::{read_desc, reset_cause_name, Aes, DirtyMem, DmaDesc, Efuse, Gdma, GdmaInCh, GdmaOutCh, Gpio, I2s, RegRam, RmtTxCh, Rsa, RtcCntl, Sha, SpiMem, SystemRegs, Systimer, Timer, TimerGroup, Uart, UartLayout, UsbSerialJtag,
                    APB_HZ, DMA_ADDR_BASE, GDMA_CHANNELS, GDMA_CH_STRIDE, RMT_MEM_WORDS, RST_POWERON, RST_RTCWDT_CPU, RST_RTCWDT_RTC, RST_RTCWDT_SYS, RST_SW_CPU, RST_SW_SYS, RTC_SLOW_HZ, SYSTIMER_HZ, XTAL_HZ};
use std::collections::HashMap;

pub const PERIPH_BASE: u32 = 0x6000_0000;
pub const PERIPH_END: u32 = 0x600D_0000;

// interrupt sources (soc/interrupts.h, with the enum's explicit gaps)
pub const SRC_GPIO: usize = 16;
pub const SRC_UART0: usize = 27;
pub const SRC_UART1: usize = 28;
pub const SRC_UART2: usize = 29;
pub const SRC_SPI2: usize = 21;
/// ETS_SPI3_INTR_SOURCE (IDF v5.5.5 components/soc/esp32s3/include/soc/interrupts.h:41; v4.4.8
/// components/soc/esp32s3/include/soc/periph_defs.h:81).
pub const SRC_SPI3: usize = 22;
pub const SRC_PWM0: usize = 31;
pub const SRC_PWM1: usize = 32;
pub const SRC_LEDC: usize = 35;
/// ETS_TWAI_INTR_SOURCE (IDF v5.5.5 components/soc/esp32s3/include/soc/interrupts.h:37-39):
/// LEDC is 35, EFUSE 36, TWAI 37. The controller's register block number is 0x2b and its
/// base is 0x6002B000 (reg_base.h:45); 0x2b is not its interrupt source number.
pub const SRC_TWAI: usize = 37;
pub const SRC_PCNT: usize = 41;
pub const SRC_AES: usize = 77;
pub const SRC_LCD_CAM: usize = 24;
pub const SRC_I2S0: usize = 25;
pub const SRC_I2S1: usize = 26;
pub const SRC_RMT: usize = 40;
pub const SRC_I2C0: usize = 42;
pub const SRC_I2C1: usize = 43;
pub const SRC_TG0_T0: usize = 50;
pub const SRC_TG0_T1: usize = 51;
pub const SRC_TG0_WDT: usize = 52;
pub const SRC_TG1_T0: usize = 53;
pub const SRC_TG1_T1: usize = 54;
pub const SRC_TG1_WDT: usize = 55;
pub const SRC_SYSTIMER_T0: usize = 57;
pub const SRC_SYSTIMER_T1: usize = 58;
pub const SRC_SYSTIMER_T2: usize = 59;
pub const SRC_DMA_IN_CH0: usize = 66;
pub const SRC_DMA_OUT_CH0: usize = 71;
pub const SRC_FROM_CPU0: usize = 79;
pub const SRC_USB_SERIAL_JTAG: usize = 96;
pub const NUM_SOURCES: usize = 99;

pub const CPU_HZ: u64 = 240_000_000;

// ------------------------------------------------------------------ Interrupt matrix
/// `status` is the live source state, refreshed by `Peripherals::pre_access` before a read.
pub struct IntMatrix { pub map: [[u32; NUM_SOURCES]; 2], ram: RegRam, pub status: [u32; 4] }
impl Default for IntMatrix { fn default() -> Self { Self::new() } }

impl IntMatrix {
    pub fn new() -> Self { IntMatrix { map: [[6; NUM_SOURCES]; 2], ram: RegRam::new(), status: [0; 4] } }
    pub fn read(&self, off: u32) -> u32 { let status = &self.status;
        let (core, o) = if off >= 0x800 { (1usize, off - 0x800) } else { (0usize, off) };
        let idx = (o >> 2) as usize;
        if idx < NUM_SOURCES { return self.map[core][idx]; }
        match o { 0x18c => status[0], 0x190 => status[1], 0x194 => status[2], 0x198 => status[3], 0x7fc => 0x2007210, _ => self.ram.read(off) }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        let (core, o) = if off >= 0x800 { (1usize, off - 0x800) } else { (0usize, off) };
        let idx = (o >> 2) as usize;
        if idx < NUM_SOURCES { self.map[core][idx] = v & 0x1f; } else { self.ram.write(off, v); }
    }
}

pub const SRC_RSA: usize = 76;
// ------------------------------------------------------------------ WiFi MAC (blocks 0x33/0x34)
pub const SRC_WIFI_MAC: usize = 0;
/// The 802.11 MAC, shared with the C3 (`esp_soc::wifi::WifiMac`).
pub use crate::wifi::WifiMac;

// ------------------------------------------------------------------ PCNT (pulse counter)
/// Four units, two channels each: a signal input counts on rising/falling edges (mode 0 ignore,
/// 1 increment, 2 decrement) and a control input modifies that (hctrl/lctrl mode 0 keep, 1 invert,
/// 2 disable). Inputs arrive through the GPIO matrix (signals 33 + 4*unit .. 36 + 4*unit).
/// Counters are 16-bit signed; high/low limits, thresholds and zero crossings raise the unit's interrupt.
/// CONF0 bits 0..=9 are the glitch-filter width and bit 10 its enable (ESP32 `pcnt_struct.h`);
/// an edge whose opposite arrives inside that many APB ticks is discarded.
#[derive(Clone, Copy)]
struct PendingEdge { delta: i32, due: u64 }

pub struct Pcnt { pub conf: [[u32; 3]; 4], pub cnt: [i16; 4], pub status: [u32; 4], pub int_raw: u32, pub int_ena: u32, pub ctrl: u32, ram: RegRam, pub events: u64,
                   /// APB ticks since power-on, the time base of the glitch filter.
                   pub now: u64, pending: [[Option<PendingEdge>; 2]; 4] }
impl Default for Pcnt { fn default() -> Self { Self::new() } }

impl Pcnt {
    pub fn new() -> Self { Pcnt { conf: [[0; 3]; 4], cnt: [0; 4], status: [0; 4], int_raw: 0, int_ena: 0, ctrl: 0, ram: RegRam::new(), events: 0, now: 0, pending: [[None; 2]; 4] } }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0x00..=0x2c => { let u = (off / 12) as usize; self.conf[u][((off % 12) / 4) as usize] }
            0x30..=0x3c => self.cnt[((off - 0x30) / 4) as usize] as u16 as u32,
            0x40 => self.int_raw, 0x44 => self.int_raw & self.int_ena, 0x48 => self.int_ena,
            0x50..=0x5c => self.status[((off - 0x50) / 4) as usize],
            0x60 => self.ctrl, 0xfc => 0x1912_0400,
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x00..=0x2c => { let u = (off / 12) as usize; self.conf[u][((off % 12) / 4) as usize] = v; }
            0x48 => self.int_ena = v, 0x4c => self.int_raw &= !v,
            // CNT_RST is a write-one pulse, not a held bit; only the pause bits (1,3,..15)
            // and CLK_EN persist, so a firmware that resets a counter and keeps counting
            // is not left disabled.
            0x60 => {
                self.ctrl = v & (0x00aa | (1 << 16));
                for u in 0..4 {
                    if v & (1 << (2 * u)) != 0 { self.cnt[u] = 0; self.pending[u] = [None; 2]; }
                }
            }
            _ => self.ram.write(off, v),
        }
    }
    /// Advance the glitch-filter clock in APB ticks and release any edge that has stayed
    /// stable for the configured width.
    pub fn advance(&mut self, ticks: u64) {
        self.now = self.now.saturating_add(ticks);
        self.fire_due();
    }
    fn fire_due(&mut self) {
        for u in 0..4 {
            for ch in 0..2 {
                if let Some(p) = self.pending[u][ch] {
                    if p.due <= self.now {
                        self.pending[u][ch] = None;
                        self.count(u, p.delta);
                    }
                }
            }
        }
    }
    /// A GPIO input changed: `sig(idx)` gives the level of matrix input signal `idx` (None = not routed).
    pub fn gpio_edge(&mut self, pin: u8, level: bool, sig: &dyn Fn(u32) -> Option<(u8, bool)>) {
        // Edges queued during one device tick all arrive at that tick's time.
        self.fire_due();
        for u in 0..4 {
            if self.ctrl & (1 << (2 * u)) != 0 || self.ctrl & (1 << (2 * u + 1)) != 0 { continue; }   // reset held / paused
            let conf0 = self.conf[u][0];
            for ch in 0..2u32 {
                let Some((sp, _)) = sig(33 + 4 * u as u32 + ch) else { continue };
                if sp != pin { continue; }
                let sh = 16 + 8 * ch;
                let mode = if level { (conf0 >> (sh + 2)) & 3 } else { (conf0 >> sh) & 3 };          // pos_mode / neg_mode
                let mut delta: i32 = match mode { 1 => 1, 2 => -1, _ => 0 };
                if delta != 0 {
                    let ctrl_level = sig(35 + 4 * u as u32 + ch).is_none_or(|(_, l)| l);
                    let cm = if ctrl_level { (conf0 >> (sh + 4)) & 3 } else { (conf0 >> (sh + 6)) & 3 };   // hctrl / lctrl
                    match cm { 1 => delta = -delta, 2 => delta = 0, _ => {} }
                }
                // The filter sees every transition, even one this mode would ignore: an
                // opposite edge inside the window means the held edge was a glitch.
                self.edge(u, ch as usize, delta);
            }
        }
    }
    /// Apply an edge immediately, or hold it until it has survived the glitch window.
    /// Two opposite edges inside one window are one short pulse and neither counts.
    fn edge(&mut self, u: usize, ch: usize, delta: i32) {
        let conf0 = self.conf[u][0];
        let width = (conf0 & 0x3ff) as u64;
        if conf0 & (1 << 10) == 0 || width == 0 {
            if delta != 0 { self.count(u, delta); }
            return;
        }
        if let Some(p) = self.pending[u][ch] {
            if self.now < p.due {
                self.pending[u][ch] = None;
                return;
            }
        }
        if delta != 0 {
            self.pending[u][ch] = Some(PendingEdge { delta, due: self.now.saturating_add(width) });
        }
    }
    fn count(&mut self, u: usize, delta: i32) {
        let conf0 = self.conf[u][0]; let conf1 = self.conf[u][1]; let conf2 = self.conf[u][2];
        let old = self.cnt[u] as i32; let mut new = old + delta;
        let mut ev = 0u32;
        if conf0 & (1 << 12) != 0 && new >= (conf2 & 0xffff) as i16 as i32 { ev |= 1 << 5; new = 0; }         // h_lim
        if conf0 & (1 << 13) != 0 && new <= (conf2 >> 16) as i16 as i32 { ev |= 1 << 4; new = 0; }             // l_lim
        if conf0 & (1 << 14) != 0 && new == (conf1 & 0xffff) as i16 as i32 { ev |= 1 << 3; }                   // thres0
        if conf0 & (1 << 15) != 0 && new == (conf1 >> 16) as i16 as i32 { ev |= 1 << 2; }                      // thres1
        // STATUS bits 0..=1 are CNT_MODE: 0 positive-to-zero, 1 negative-to-zero; bit 6 is
        // ZERO_LAT (pcnt_struct.h). Keep each latched event separate from the mode field.
        if conf0 & (1 << 11) != 0 && new == 0 && old != 0 { ev |= 1 << 6; ev |= if delta > 0 { 0 } else { 1 }; }
        self.cnt[u] = new as i16; self.events += 1;
        if ev != 0 { self.status[u] = ev; self.int_raw |= 1 << u; }
    }
}

// ------------------------------------------------------------------ TWAI (CAN 2.0A/B)
/// TWAI register offsets, confirmed against ESP-IDF v5.5
/// `components/soc/esp32s3/register/soc/twai_struct.h` (the classic ESP32 header has the
/// same map). Every register is a byte in the low bits of a 32-bit word: MODE 0x00,
/// CMD 0x04, STATUS 0x08, INT_RAW 0x0c, INT_ENA 0x10, BUS_TIMING_0 0x18,
/// BUS_TIMING_1 0x1c, ALC 0x2c, ECC 0x30, EWL 0x34, RX_ERR 0x38, TX_ERR 0x3c, the
/// 13-byte TX/RX buffer at 0x40..0x70, RX_MESSAGE_COUNTER 0x74 and CLOCK_DIVIDER 0x7c.
/// BUS_TIMING_0 is 16 bits (13-bit BRP + 2-bit SJW); the other byte registers are 8 bits.
///
/// (The offsets in the task brief for ALC/ECC/RX_ERR/TX_ERR — 0x14/0x20/0x24/0x28 — do
/// not match either chip's header; ALC is at 0x2c and the counters at 0x38/0x3c.)
pub const TWAI_STATUS_RBS: u32 = 1 << 0;   // receive buffer status: a frame is visible
pub const TWAI_STATUS_DOS: u32 = 1 << 1;   // data overrun
pub const TWAI_STATUS_TBS: u32 = 1 << 2;   // transmit buffer status: 1 = free
pub const TWAI_STATUS_TCS: u32 = 1 << 3;   // transmission complete
pub const TWAI_STATUS_RS: u32 = 1 << 4;    // receive status
pub const TWAI_STATUS_TS: u32 = 1 << 5;    // transmit status
pub const TWAI_STATUS_ES: u32 = 1 << 6;    // error status
pub const TWAI_STATUS_BS: u32 = 1 << 7;    // bus status (bus-off)

pub const TWAI_INT_RI: u32 = 1 << 0;       // receive interrupt
pub const TWAI_INT_TI: u32 = 1 << 1;       // transmit interrupt
pub const TWAI_INT_EI: u32 = 1 << 2;
pub const TWAI_INT_DOI: u32 = 1 << 3;
pub const TWAI_INT_EPI: u32 = 1 << 5;
pub const TWAI_INT_ALI: u32 = 1 << 6;
pub const TWAI_INT_BEI: u32 = 1 << 7;

pub const TWAI_CMD_TR: u32 = 1 << 0;       // transmission request
pub const TWAI_CMD_AT: u32 = 1 << 1;       // abort / single-shot modifier
pub const TWAI_CMD_RRB: u32 = 1 << 2;      // release receive buffer
pub const TWAI_CMD_CDO: u32 = 1 << 3;      // clear data overrun
pub const TWAI_CMD_SRR: u32 = 1 << 4;      // self reception request

pub const TWAI_MODE_RM: u32 = 1 << 0;      // reset mode
pub const TWAI_MODE_LOM: u32 = 1 << 1;     // listen only
pub const TWAI_MODE_STM: u32 = 1 << 2;     // self test / loopback
pub const TWAI_MODE_AFM: u32 = 1 << 3;     // single acceptance filter

/// Emulated CAN FD payload window: one byte per 32-bit register at 0x80 + 4*i. The classic
/// S3 buffer is thirteen bytes, so the model puts an FD payload here and keeps 0x78 as its
/// control byte. Both registers are otherwise unmodelled (`RegRam`), so firmware written
/// against the classic map is unaffected.
pub const TWAI_FD_DATA_BASE: u32 = 0x80;
/// Emulated FD control: bit 0 = FD frame, bit 1 = bit-rate switch (BRS).
pub const TWAI_FD_CTRL: u32 = 0x78;
/// Maximum CAN FD payload bytes.
pub const TWAI_FD_MAX_DATA: usize = 64;

/// Error-counter thresholds (ISO 11898-1). The registers are the classic 8-bit counters but
/// the model keeps them wide so a test can cross 256 with repeated arbitration losses.
pub const TWAI_ERROR_PASSIVE: u32 = 128;
pub const TWAI_BUS_OFF: u32 = 256;

/// The kind of frame in a [`TwaiFrame`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TwaiFrameKind {
    Classic,
    Fd { bit_rate_switch: bool },
    Remote,
    Error,
}

/// A frame as the TWAI controller sees it, including CAN FD payloads. The board callback
/// wire type (`esp_soc::board::CanFrame`) stays classic; [`TwaiFrame::encode_wire`] extends
/// it with a documented tail only when the frame is FD or an error frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TwaiFrame {
    pub kind: TwaiFrameKind,
    pub id: u32,
    pub extended: bool,
    pub len: u8,
    pub data: [u8; TWAI_FD_MAX_DATA],
}

impl TwaiFrame {
    pub fn classic(id: u32, extended: bool, remote: bool, len: u8, data: &[u8]) -> Self {
        let kind = if remote { TwaiFrameKind::Remote } else { TwaiFrameKind::Classic };
        let mut frame = TwaiFrame { kind, id, extended, len, data: [0; TWAI_FD_MAX_DATA] };
        let n = (frame.len as usize).min(data.len()).min(TWAI_FD_MAX_DATA);
        frame.data[..n].copy_from_slice(&data[..n]);
        frame.normalized()
    }

    pub fn fd(id: u32, extended: bool, bit_rate_switch: bool, data: &[u8]) -> Self {
        let mut frame = TwaiFrame { kind: TwaiFrameKind::Fd { bit_rate_switch }, id, extended, len: data.len().min(TWAI_FD_MAX_DATA) as u8, data: [0; TWAI_FD_MAX_DATA] };
        let n = frame.len as usize;
        frame.data[..n].copy_from_slice(&data[..n]);
        frame.normalized()
    }

    pub fn error() -> Self {
        TwaiFrame { kind: TwaiFrameKind::Error, id: 0, extended: false, len: 0, data: [0; TWAI_FD_MAX_DATA] }
    }

    pub fn from_board(frame: &esp_soc::board::CanFrame) -> Self {
        let mut data = [0u8; TWAI_FD_MAX_DATA];
        data[..esp_soc::board::CAN_MAX_DATA].copy_from_slice(&frame.data);
        TwaiFrame { kind: if frame.rtr { TwaiFrameKind::Remote } else { TwaiFrameKind::Classic }, id: frame.id, extended: frame.extended, len: frame.len, data }.normalized()
    }

    fn normalized(mut self) -> Self {
        self.id &= if self.extended { 0x1fff_ffff } else { 0x7ff };
        match self.kind {
            TwaiFrameKind::Error => { self.id = 0; self.extended = false; self.len = 0; self.data = [0; TWAI_FD_MAX_DATA]; }
            TwaiFrameKind::Fd { .. } => self.len = self.len.min(TWAI_FD_MAX_DATA as u8),
            TwaiFrameKind::Classic | TwaiFrameKind::Remote => self.len = self.len.min(esp_soc::board::CAN_MAX_DATA as u8),
        }
        for byte in self.data.iter_mut().skip(self.len as usize) { *byte = 0; }
        self
    }

    /// CAN arbitration priority: lower base identifier, then a standard frame over an
    /// extended frame with the same base, then the full 29-bit identifier.
    fn priority_key(&self) -> (u32, bool, u32) {
        let base = self.id & 0x7ff;
        (base, self.extended, if self.extended { self.id & 0x1fff_ffff } else { base })
    }

    /// Wire encoding for the board callback.
    ///
    /// Classic and remote frames use the unchanged fourteen-byte
    /// `esp_soc::board::CanFrame` layout. A CAN FD frame keeps a valid fourteen-byte prefix
    /// (the first eight data bytes, so a classic-only board still decodes a coherent frame)
    /// and appends `[14] = length`, `[15..79] = the full payload`; flag bit 2 marks FD and
    /// bit 3 marks BRS. An error frame is fourteen bytes with flag bit 4 set. A board that
    /// only knows the classic decoder ignores the tail.
    pub fn encode_wire(&self) -> Vec<u8> {
        let frame = self.normalized();
        match frame.kind {
            TwaiFrameKind::Error => {
                let mut out = vec![0u8; esp_soc::board::CAN_FRAME_BYTES];
                out[0] = 1 << 4;
                out
            }
            TwaiFrameKind::Fd { bit_rate_switch } => {
                let mut out = vec![0u8; 15 + TWAI_FD_MAX_DATA];
                out[0] = u8::from(frame.extended) | (1 << 2) | (u8::from(bit_rate_switch) << 3);
                out[1..5].copy_from_slice(&frame.id.to_le_bytes());
                out[5] = frame.len.min(esp_soc::board::CAN_MAX_DATA as u8);
                let prefix = frame.len.min(esp_soc::board::CAN_MAX_DATA as u8) as usize;
                out[6..6 + prefix].copy_from_slice(&frame.data[..prefix]);
                out[14] = frame.len;
                out[15..].copy_from_slice(&frame.data);
                out
            }
            TwaiFrameKind::Classic | TwaiFrameKind::Remote => {
                let mut out = vec![0u8; esp_soc::board::CAN_FRAME_BYTES];
                out[0] = u8::from(frame.extended) | (u8::from(frame.kind == TwaiFrameKind::Remote) << 1);
                out[1..5].copy_from_slice(&frame.id.to_le_bytes());
                out[5] = frame.len.min(esp_soc::board::CAN_MAX_DATA as u8);
                let n = frame.len.min(esp_soc::board::CAN_MAX_DATA as u8) as usize;
                out[6..6 + n].copy_from_slice(&frame.data[..n]);
                out
            }
        }
    }

    /// Decode either the classic wire layout or the FD extension above. `None` is a
    /// malformed board response; [`Twai::receive_malformed`] counts it.
    pub fn decode_wire(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < esp_soc::board::CAN_FRAME_BYTES { return None; }
        let flags = bytes[0];
        if flags & (1 << 4) != 0 { return Some(TwaiFrame::error()); }
        let id = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        let extended = flags & 1 != 0;
        if flags & (1 << 2) != 0 {
            if bytes.len() < 15 { return None; }
            let len = bytes[14];
            if len as usize > TWAI_FD_MAX_DATA || bytes.len() < 15 + len as usize { return None; }
            let mut data = [0u8; TWAI_FD_MAX_DATA];
            data[..len as usize].copy_from_slice(&bytes[15..15 + len as usize]);
            return Some(TwaiFrame {
                kind: TwaiFrameKind::Fd { bit_rate_switch: flags & (1 << 3) != 0 },
                id,
                extended,
                len,
                data,
            }.normalized());
        }
        let board = esp_soc::board::CanFrame::decode(bytes)?;
        Some(TwaiFrame::from_board(&board))
    }
}

/// What [`Twai::arbitrate_against`] decided for one frame pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TwaiArbitration { Won, Lost, Collision }

/// SJA1000-style TWAI controller for CAN 2.0A/B, extended with a deterministic CAN FD and
/// error-confinement surface.
///
/// The classic registers are unchanged: reset/normal/listen-only/loopback modes, one TX
/// buffer, a small RX FIFO, status and interrupt bits. On top of them the model adds:
///
/// * FD frames, enabled by DLC values 9..=15 (the CAN FD length codes) or by
///   [`TWAI_FD_CTRL`] bit 0, with the payload in the [`TWAI_FD_DATA_BASE`] window and BRS
///   in [`TWAI_FD_CTRL`] bit 1.
/// * Error counters that move: an arbitration loss or a malformed/received error frame adds
///   eight, a completed frame subtracts one. STATUS.ES is set from 128, STATUS.BS from 256,
///   and a bus-off controller refuses to latch a transmission until it is reset (MODE.RM) or
///   a valid frame arrives. This is deterministic bookkeeping, not a bit-level timing model.
pub struct Twai {
    pub mode: u32,
    pub bus_timing_0: u32,
    pub bus_timing_1: u32,
    pub int_raw: u32,
    pub int_ena: u32,
    pub alc: u32,
    pub ecc: u32,
    pub ewl: u32,
    pub rx_err: u32,
    pub tx_err: u32,
    pub clock_divider: u32,
    /// TX/RX buffer bytes, one per 32-bit register at 0x40 + 4*i.
    pub data: [u8; 13],
    /// FD payload staging, one byte per register at [`TWAI_FD_DATA_BASE`] + 4*i.
    pub fd_data: [u8; TWAI_FD_MAX_DATA],
    /// FD control (0x78): bit 0 FD, bit 1 BRS.
    pub fd_ctrl: u32,
    /// Frames received from the board but not yet released with CMD.RRB.
    rx: std::collections::VecDeque<TwaiFrame>,
    /// A command latched a transmission; the SoC takes it and asks the board.
    tx_request: Option<(TwaiFrame, bool)>,  // (frame, self-reception)
    tx_free: bool,
    tcs: bool,
    rs: bool,
    ram: RegRam,
    pub log: bool,
}
impl Default for Twai { fn default() -> Self { Self::new() } }

impl Twai {
    pub fn new() -> Self {
        Twai { mode: TWAI_MODE_RM, bus_timing_0: 0, bus_timing_1: 0, int_raw: 0, int_ena: 0,
               alc: 0, ecc: 0, ewl: 96, rx_err: 0, tx_err: 0, clock_divider: 0, data: [0; 13],
               fd_data: [0; TWAI_FD_MAX_DATA], fd_ctrl: 0,
               rx: std::collections::VecDeque::new(), tx_request: None, tx_free: true, tcs: false,
               rs: false, ram: RegRam::new(), log: false }
    }
    pub fn in_reset_mode(&self) -> bool { self.mode & TWAI_MODE_RM != 0 }
    /// A running controller may accept frames from the board; reset and listen-only do not.
    pub fn can_receive(&self) -> bool { !self.in_reset_mode() }
    /// A running controller may drive the bus; reset and listen-only may not, and neither
    /// does a bus-off controller.
    pub fn can_transmit(&self) -> bool { !self.in_reset_mode() && self.mode & TWAI_MODE_LOM == 0 && !self.bus_off() }
    pub fn has_rx_frame(&self) -> bool { !self.rx.is_empty() }
    pub fn rx_pending(&self) -> usize { self.rx.len() }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }
    pub fn error_passive(&self) -> bool { self.rx_err >= TWAI_ERROR_PASSIVE || self.tx_err >= TWAI_ERROR_PASSIVE }
    pub fn bus_off(&self) -> bool { self.tx_err >= TWAI_BUS_OFF }

    fn status(&self) -> u32 {
        let mut s = 0;
        if !self.rx.is_empty() { s |= TWAI_STATUS_RBS; }
        // The model completes a transmission as soon as the board accepts it, so the buffer
        // is free whenever no request is latched.
        if self.tx_free { s |= TWAI_STATUS_TBS; }
        if self.tcs { s |= TWAI_STATUS_TCS; }
        if self.rs { s |= TWAI_STATUS_RS; }
        if self.tx_free && self.tcs { s |= TWAI_STATUS_TS; }
        if self.error_passive() { s |= TWAI_STATUS_ES; }
        if self.bus_off() { s |= TWAI_STATUS_ES | TWAI_STATUS_BS; }
        s
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x00 => self.mode,
            0x04 => 0,                       // command bits are self-clearing
            0x08 => self.status(),
            // Reading INT_RAW returns the latched bits and clears all but RI. RI is the
            // receive FIFO level, which only CMD.RRB clears.
            0x0c => { let v = self.int_raw; self.int_raw &= TWAI_INT_RI; v }
            0x10 => self.int_ena,
            0x18 => self.bus_timing_0,
            0x1c => self.bus_timing_1,
            0x2c => self.alc,
            0x30 => self.ecc,
            0x34 => self.ewl,
            0x38 => self.rx_err,
            0x3c => self.tx_err,
            0x78 => if self.has_rx_frame() {
                match self.rx.front().map(|f| f.kind) {
                    Some(TwaiFrameKind::Fd { bit_rate_switch }) => 1 | (u32::from(bit_rate_switch) << 1),
                    _ => 0,
                }
            } else { self.fd_ctrl },
            // The TX and RX buffers share these registers. While a received frame is waiting,
            // a read presents it, exactly as the hardware's shared buffer does.
            0x40..=0x70 => {
                if self.has_rx_frame() { self.show_rx_buffer(); }
                u32::from(self.data[((off - 0x40) / 4) as usize])
            }
            0x74 => (self.rx.len() as u32) & 0x7f,
            0x7c => self.clock_divider,
            0x80..=0x17c => {
                let i = ((off - TWAI_FD_DATA_BASE) / 4) as usize;
                let byte = self.rx.front().map_or(self.fd_data[i], |f| f.data[i]);
                u32::from(byte)
            }
            _ => self.ram.read(off),
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x00 => {
                self.mode = v & 0x0f;
                if self.in_reset_mode() {
                    // Reset mode aborts a latched transmission, frees the buffer and clears
                    // the error counters and confinement state, as the SJA1000 does. Doing
                    // it on every write (not only the transition) makes a firmware reset a
                    // reliable bus-off recovery even when RM was already asserted.
                    self.tx_request = None;
                    self.tx_free = true;
                    self.reset_errors();
                    self.fd_ctrl = 0;
                }
            }
            0x04 => self.command(v),
            0x0c => self.int_raw &= !v,      // W1C, though silicon clears by reading
            0x10 => self.int_ena = v,
            0x18 => self.bus_timing_0 = v,
            0x1c => self.bus_timing_1 = v,
            0x2c => self.alc = v & 0xff,
            0x30 => self.ecc = v & 0xff,
            0x34 => self.ewl = v & 0xff,
            0x38 => self.rx_err = v & 0xff,
            0x3c => self.tx_err = v & 0xff,
            0x40..=0x70 => {
                let i = ((off - 0x40) / 4) as usize;
                self.data[i] = v as u8;
                if i == 0 { self.tcs = false; }   // a fresh TX frame clears TCS
            }
            0x78 => self.fd_ctrl = v & 0x3,
            0x7c => self.clock_divider = v & 0x1ff,
            0x80..=0x17c => self.fd_data[((off - TWAI_FD_DATA_BASE) / 4) as usize] = v as u8,
            _ => self.ram.write(off, v),
        }
    }

    fn command(&mut self, v: u32) {
        if v & TWAI_CMD_RRB != 0 { self.release_rx(); }
        if v & TWAI_CMD_CDO != 0 { /* data overrun is never asserted, nothing to clear */ }
        // Abort alone (AT without TR/SRR) halts a pending transmission and completes it.
        if v & TWAI_CMD_AT != 0 && v & (TWAI_CMD_TR | TWAI_CMD_SRR) == 0 {
            self.tx_request = None;
            self.tx_free = true;
            self.tcs = true;
            self.int_raw |= TWAI_INT_TI;
        }
        // A transmission request latches the buffer contents. Reset and listen-only ignore it.
        // Self-test mode (STM) loops the frame back without a CMD.SRR.
        if self.can_transmit() && v & (TWAI_CMD_TR | TWAI_CMD_SRR) != 0 && self.tx_free {
            let self_rx = v & TWAI_CMD_SRR != 0 || self.mode & TWAI_MODE_STM != 0;
            self.tx_request = Some((self.buffer_frame(), self_rx));
            self.tx_free = false;
            self.tcs = false;
        }
        if self.log { eprintln!("[twai] CMD <- {v:#04x} (tx pending {})", self.tx_request.is_some()); }
    }

    /// Decode the classic 13-byte TX buffer, extended with FD when DLC is 9..=15 or
    /// [`TWAI_FD_CTRL`] bit 0 is set. Layout (IDF `twai_ll_frame_buffer_t`):
    /// byte 0 = DLC | self-reception<<4 | single-shot<<5 | RTR<<6 | extended<<7;
    /// standard ID = two left-aligned big-endian bytes, then 8 data bytes;
    /// extended ID = four left-aligned big-endian bytes, then 8 data bytes.
    fn buffer_frame(&self) -> TwaiFrame {
        let info = self.data[0];
        let extended = info & 0x80 != 0;
        let rtr = info & 0x40 != 0;
        let dlc = info & 0x0f;
        let (id, data_at) = if extended {
            let raw = u32::from_le_bytes([self.data[1], self.data[2], self.data[3], self.data[4]]).swap_bytes() >> 3;
            (raw & 0x1fff_ffff, 5usize)
        } else {
            let raw = u16::from_le_bytes([self.data[1], self.data[2]]).swap_bytes() >> 5;
            (u32::from(raw) & 0x7ff, 3usize)
        };
        let fd = self.fd_ctrl & 1 != 0 || dlc > 8;
        let kind = if rtr { TwaiFrameKind::Remote } else if fd { TwaiFrameKind::Fd { bit_rate_switch: self.fd_ctrl & 2 != 0 } } else { TwaiFrameKind::Classic };
        let len = if rtr || !fd { dlc.min(esp_soc::board::CAN_MAX_DATA as u8) } else { fd_len_from_dlc(dlc) };
        let mut data = [0u8; TWAI_FD_MAX_DATA];
        if !rtr {
            let n = len as usize;
            if fd && n > esp_soc::board::CAN_MAX_DATA {
                data[..n].copy_from_slice(&self.fd_data[..n]);
            } else {
                data[..n].copy_from_slice(&self.data[data_at..data_at + n]);
            }
        }
        TwaiFrame { kind, id, extended, len, data }.normalized()
    }

    /// Publish a frame from the board into the RX buffer and raise RI. The RX FIFO holds
    /// frames the driver has not released yet; it is capped at the hardware's 64 messages.
    pub fn receive(&mut self, frame: &esp_soc::board::CanFrame) {
        self.receive_frame(&TwaiFrame::from_board(frame));
    }

    /// The FD-aware counterpart of [`Twai::receive`].
    pub fn receive_frame(&mut self, frame: &TwaiFrame) {
        if frame.kind == TwaiFrameKind::Error {
            // An error frame is a bus event, not a mailbox message: count it and return.
            self.note_rx_error(8);
            return;
        }
        if self.rx.len() >= 64 { self.int_raw |= TWAI_INT_DOI; return; }
        self.rx.push_back(frame.normalized());
        self.int_raw |= TWAI_INT_RI;
        self.rs = true;
        if self.log { eprintln!("[twai] RX id {:#x} ext {} dlc {} fd {}", frame.id, frame.extended, frame.len, matches!(frame.kind, TwaiFrameKind::Fd { .. })); }
    }

    /// A board frame that did not decode: count a receive error (short or malformed wire).
    pub fn receive_malformed(&mut self) {
        self.note_rx_error(8);
        if self.log { eprintln!("[twai] received a malformed frame"); }
    }

    fn release_rx(&mut self) {
        let _ = self.rx.pop_front();
        if self.rx.is_empty() {
            self.int_raw &= !TWAI_INT_RI;
            self.rs = false;
        }
    }

    /// Store the oldest received frame in the RX buffer registers for the driver to read.
    fn show_rx_buffer(&mut self) {
        let Some(frame) = self.rx.front().copied() else { return; };
        let dlc = match frame.kind {
            TwaiFrameKind::Fd { .. } => fd_dlc_from_len(frame.len),
            _ => frame.len,
        };
        let info = (dlc & 0x0f) | (u8::from(matches!(frame.kind, TwaiFrameKind::Remote)) << 6) | (u8::from(frame.extended) << 7);
        self.data = [0; 13];
        self.data[0] = info;
        let data_at = if frame.extended {
            let swapped = ((frame.id & 0x1fff_ffff) << 3).swap_bytes();
            self.data[1..5].copy_from_slice(&swapped.to_le_bytes());
            5usize
        } else {
            let swapped = (((frame.id & 0x7ff) << 5) as u16).swap_bytes();
            self.data[1..3].copy_from_slice(&swapped.to_le_bytes());
            3usize
        };
        if !matches!(frame.kind, TwaiFrameKind::Remote) {
            let n = (frame.len as usize).min(esp_soc::board::CAN_MAX_DATA);
            self.data[data_at..data_at + n].copy_from_slice(&frame.data[..n]);
        }
    }

    /// An arbitration loss, as reported by a bus model that compared identifiers. Real
    /// silicon does not count this as an error; the model does, deliberately, so a test can
    /// reach error-passive and bus-off deterministically.
    pub fn record_arbitration_loss(&mut self) {
        self.note_tx_error(8);
        self.int_raw |= TWAI_INT_ALI;
    }

    /// Compare this controller's TX buffer against another frame offered on the same bus.
    /// Returns `Won` for the lower identifier, `Lost` for the higher one, and `Collision`
    /// when the identifiers match but the payload differs. A loss or collision is counted on
    /// this controller's transmit counter.
    pub fn arbitrate_against(&mut self, other: &TwaiFrame) -> TwaiArbitration {
        let own = self.buffer_frame();
        let ours = own.priority_key();
        let theirs = other.priority_key();
        if ours < theirs {
            TwaiArbitration::Won
        } else if ours > theirs {
            self.record_arbitration_loss();
            TwaiArbitration::Lost
        } else if own == *other {
            TwaiArbitration::Won
        } else {
            self.record_arbitration_loss();
            self.note_rx_error(8);
            TwaiArbitration::Collision
        }
    }

    fn note_tx_error(&mut self, amount: u32) {
        self.tx_err = self.tx_err.saturating_add(amount);
        self.int_raw |= TWAI_INT_EI;
        if self.error_passive() { self.int_raw |= TWAI_INT_EPI; }
        if self.bus_off() { self.int_raw |= TWAI_INT_EPI | TWAI_INT_BEI; }
    }

    fn note_rx_error(&mut self, amount: u32) {
        self.rx_err = self.rx_err.saturating_add(amount);
        self.int_raw |= TWAI_INT_EI;
        if self.error_passive() { self.int_raw |= TWAI_INT_EPI; }
    }

    /// A completed frame moves both counters one step back toward zero; the confinement
    /// predicate then drops as soon as a counter falls below its threshold.
    pub fn record_success(&mut self) {
        self.tx_err = self.tx_err.saturating_sub(1);
        self.rx_err = self.rx_err.saturating_sub(1);
    }

    /// Clear both counters and the latched confinement interrupts (reset-mode recovery).
    pub fn reset_errors(&mut self) {
        self.tx_err = 0;
        self.rx_err = 0;
        self.int_raw &= !(TWAI_INT_EPI | TWAI_INT_BEI | TWAI_INT_ALI);
    }

    /// The SoC calls this after any register write: a latched request is handed to the board.
    pub fn take_tx_request(&mut self) -> Option<(TwaiFrame, bool)> { self.tx_request.take() }

    /// Mark a taken transmission complete: buffer free, TCS set, transmit interrupt raised.
    pub fn complete_tx(&mut self) {
        self.tx_free = true;
        self.tcs = true;
        self.int_raw |= TWAI_INT_TI;
    }
}

/// CAN FD DLC to payload length (ISO 11898-1 Table 4).
fn fd_len_from_dlc(dlc: u8) -> u8 {
    match dlc {
        9 => 12,
        10 => 16,
        11 => 20,
        12 => 24,
        13 => 32,
        14 => 48,
        15 => 64,
        n => n.min(esp_soc::board::CAN_MAX_DATA as u8),
    }
}

/// Payload length to the smallest CAN FD DLC that holds it.
fn fd_dlc_from_len(len: u8) -> u8 {
    match len {
        0..=8 => len,
        9..=12 => 9,
        13..=16 => 10,
        17..=20 => 11,
        21..=24 => 12,
        25..=32 => 13,
        33..=48 => 14,
        _ => 15,
    }
}

// ------------------------------------------------------------------ LCD_CAM (camera side)
/// LCD_CAM captures sensor frames through GDMA trigger 5 while CAM_START is set.
/// The sensor frame clock and VSYNC continue while capture is stopped.
pub struct LcdCam { pub ram: RegRam, cam_ctrl: u32, pub cam_ctrl1: u32, pub int_raw: u32, int_ena: u32, running: bool,
                    pub frame_cycles: u64, pub acc: u64, pub frames: u64, pub dropped: u64,
                    // LCD side (RGB / DPI mode): the panel is refreshed from a GDMA out-channel on trigger 5
                    pub lcd_clock: u32, pub lcd_user: u32, pub lcd_ctrl: u32, pub lcd_ctrl1: u32, pub lcd_acc: u64, pub lcd_frames: u64, pub lcd_line: Vec<u8>, pub lcd_fifo: std::collections::VecDeque<u8>, pub lcd_log: bool,
                    cam_frame: Option<std::sync::Arc<Vec<u8>>>, cam_active: bool, cam_clock_active: bool,
                    clock_enabled: bool, pub frame_dropped: bool, pub frame_start: u64 }
impl Default for LcdCam { fn default() -> Self { Self::new() } }

impl LcdCam {
    pub fn new() -> Self { LcdCam { ram: RegRam::new(), cam_ctrl: 0, cam_ctrl1: 0, int_raw: 0, int_ena: 0, running: false, cam_active: false, cam_clock_active: false, cam_frame: None, clock_enabled: false, frame_dropped: false, frame_start: 0, frame_cycles: CPU_HZ / 10, acc: 0, frames: 0, dropped: 0,
                                    lcd_clock: 0, lcd_user: 0, lcd_ctrl: 0, lcd_ctrl1: 0, lcd_acc: 0, lcd_frames: 0, lcd_line: Vec::new(), lcd_fifo: std::collections::VecDeque::new(), lcd_log: false } }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }
    /// LCD RGB mode running: LCD_START (USER bit 27) with LCD_RGB_MODE_EN (CTRL bit 31).
    pub fn lcd_running(&self) -> bool { self.lcd_user & (1 << 27) != 0 && self.lcd_ctrl & (1 << 31) != 0 }
    /// (active width, active height, bytes per pixel, CPU cycles per frame) from the timing registers.
    pub fn lcd_geometry(&self) -> (u32, u32, u32, u64) {
        // the registers hold (value - 1): lcd_ll_set_horizontal/vertical_timing
        let ha = ((self.lcd_ctrl1 >> 8) & 0xfff) + 1; let ht = ((self.lcd_ctrl1 >> 20) & 0xfff) + 1;
        let va = ((self.lcd_ctrl >> 11) & 0x3ff) + 1; let vt = ((self.lcd_ctrl >> 21) & 0x3ff) + 1;
        let bpp = if self.lcd_user & (1 << 23) != 0 { 2 } else { 1 };
        // lcd_clk = src / (div_num + div_b/div_a); pclk = lcd_clk / (clkcnt_n + 1) unless CLK_EQU_SYSCLK
        let src = match (self.lcd_clock >> 29) & 3 { 1 => 40_000_000f64, 2 => 240_000_000.0, _ => 160_000_000.0 };
        let div_num = ((self.lcd_clock >> 9) & 0xff).max(1) as f64; let div_b = ((self.lcd_clock >> 17) & 0x3f) as f64; let div_a = ((self.lcd_clock >> 23) & 0x3f) as f64;
        let lcd_clk = src / (div_num + if div_a > 0.0 { div_b / div_a } else { 0.0 });
        let n = if self.lcd_clock & (1 << 6) != 0 { 1.0 } else { (self.lcd_clock & 0x3f) as f64 + 1.0 };
        let pclk = (lcd_clk / n).max(1_000_000.0) as u64;
        let frame_px = (ht as u64) * (vt as u64);
        (ha, va, bpp, frame_px * CPU_HZ / pclk)
    }
    pub fn read(&self, off: u32) -> u32 {
        match off { 0x00 => self.lcd_clock, 0x04 => self.cam_ctrl, 0x08 => self.cam_ctrl1, 0x14 => self.lcd_user & !((1 << 20) | (1 << 28)), 0x1c => self.lcd_ctrl, 0x20 => self.lcd_ctrl1,
                    0x64 => self.int_ena, 0x68 => self.int_raw, 0x6c => self.int_raw & self.int_ena, _ => self.ram.read(off) }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x00 => self.lcd_clock = v,
            0x14 => { let was = self.lcd_running(); self.lcd_user = v;
                      if v & (1 << 28) != 0 { self.lcd_line.clear(); self.lcd_fifo.clear(); self.lcd_acc = 0; }                       // LCD_RESET
                      if !was && self.lcd_running() { self.lcd_line.clear(); self.lcd_acc = 0; }
                      if self.lcd_log { eprintln!("[lcd] USER <- {:#010x} (start {} reset {} update {})", v, v >> 27 & 1, v >> 28 & 1, v >> 20 & 1); } }
            0x18 => { if v & (1 << 27) != 0 { self.lcd_fifo.clear(); if self.lcd_log { eprintln!("[lcd] AFIFO reset"); } } self.ram.write(off, v); }   // LCD_MISC.AFIFO_RESET
            0x1c => self.lcd_ctrl = v, 0x20 => self.lcd_ctrl1 = v,
            0x04 => { self.cam_ctrl = v & !(1 << 4); self.refresh_cam_active(); }                                                                          // CAM_UPDATE (self-clearing)
            0x08 => { self.cam_ctrl1 = v & !(3 << 30); self.running = v & (1 << 29) != 0; if v & (1 << 30) != 0 { self.running = false; self.cam_ctrl1 &= !(1 << 29); } self.refresh_cam_active(); }   // CAM_START / CAM_RESET
            0x64 => { self.int_ena = v; self.refresh_cam_active(); }, 0x70 => self.int_raw &= !v,
            _ => self.ram.write(off, v),
        }
    }
    /// Cache activity at register/frame changes so idle ticks only read a flag.
    fn refresh_cam_active(&mut self) {
        // IDF v5.5.4 soc/lcd_cam_reg.h: CAM_CLK_SEL bits 29..30, zero disables XCLK.
        self.cam_active = self.clock_enabled && (self.cam_ctrl >> 29) & 3 != 0 && (self.running || self.int_ena & (1 << 2) != 0);
        self.cam_clock_active = self.clock_enabled && (self.cam_ctrl >> 29) & 3 != 0 && (self.cam_active || self.cam_frame.is_some());
    }
    pub fn cam_ctrl(&self) -> u32 { self.cam_ctrl }
    pub fn cam_active(&self) -> bool { self.cam_active }
    pub fn cam_clock_active(&self) -> bool { self.cam_clock_active }
    pub fn running(&self) -> bool { self.running }
    pub fn cam_frame(&self) -> Option<&std::sync::Arc<Vec<u8>>> { self.cam_frame.as_ref() }
    pub fn set_cam_frame(&mut self, frame: Option<std::sync::Arc<Vec<u8>>>) { self.cam_frame = frame; self.frame_start = self.acc; self.refresh_cam_active(); }
    pub fn set_clock_enabled(&mut self, enabled: bool) { self.clock_enabled = enabled; self.refresh_cam_active(); }
    /// Advance the sensor clock independently of CAM_START and CAM_RESET.
    pub fn frame_due(&mut self, cycles: u64) -> bool { self.acc = self.acc.saturating_add(cycles); if self.acc >= self.frame_cycles.max(1) { self.acc %= self.frame_cycles.max(1); true } else { false } }
}

// ------------------------------------------------------------------ EXTMEM (cache controller; MMU table lives in the bus)
pub struct Extmem { pub ram: RegRam }
impl Default for Extmem { fn default() -> Self { Self::new() } }

impl Extmem {
    pub fn new() -> Self { Extmem { ram: RegRam::new() } }
    pub fn read(&self, off: u32) -> u32 {
        let v = self.ram.read(off);
        match off {
            0x28 => v | (1 << 3),                 // DCACHE_SYNC_CTRL: SYNC_DONE
            0x88 => v | (1 << 1),                 // ICACHE_SYNC_CTRL: SYNC_DONE
            0x40 | 0x94 => v | (1 << 1),          // *CACHE_PRELOAD_CTRL: PRELOAD_DONE
            0x4c | 0xa0 => v | (1 << 3),          // *CACHE_AUTOLOAD_CTRL: AUTOLOAD_DONE
            0x34 => v | (1 << 1),                 // DCACHE_OCCUPY_CTRL: OCCUPY_DONE
            0x150 | 0x154 => if v & 1 != 0 { v | (1 << 2) } else { v & !(1 << 2) },   // *CACHE_FREEZE: FREEZE_DONE follows FREEZE_ENA
            0x1c | 0x7c => v | (1 << 2),      // *CACHE_LOCK_CTRL: 0x1cONE
            0x130 => 0x1001,                      // CACHE_STATE: icache/dcache idle
            0x3fc => 0x2101070,
            _ => v,
        }
    }
    pub fn write(&mut self, off: u32, v: u32) { self.ram.write(off, v); }
}

// ------------------------------------------------------------------ WDEV (radio) block: only the hardware RNG register matters to us
pub struct Wdev { state: u64, ram: RegRam }
impl Default for Wdev { fn default() -> Self { Self::new() } }

impl Wdev {
    pub fn new() -> Self { Wdev { state: 0x9E37_79B9_7F4A_7C15, ram: RegRam::new() } }
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x7c => { let mut x = self.state; x ^= x << 13; x ^= x >> 7; x ^= x << 17; self.state = x; (x >> 16) as u32 }   // WDEV_RND_REG (xorshift64)
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) { self.ram.write(off, v); }
}

// ------------------------------------------------------------------ I2C_MST: analog "regi2c" master (PLL / SAR ADC trim registers)
pub struct I2cMst { pub ram: RegRam, pub ana: esp_periph::Regi2c }
impl Default for I2cMst { fn default() -> Self { Self::new() } }

impl I2cMst {
    pub fn new() -> Self { I2cMst { ram: RegRam::new(), ana: Default::default() } }
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x0 => self.ana.read(0, self.ram.read(off), |_| &0),
            // analog-block handshakes (BBPLL cal, pkdet, txdc/rxdc comparators...): the blob writes a start bit and
            // polls a done bit in 26:24; comparator sign bits 31:30 read as 0 — enough for its search loops to run
            0x40..=0x5c => (self.ram.read(off) & 0x3fff_ffff) | (7 << 24),
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        if off == 0 { self.ana.write(0, v); }
        self.ram.write(off, v);
    }
}


// ------------------------------------------------------------------ S3-only devices as `Device`
impl Device for IntMatrix {
    fn read(&mut self, off: u32) -> u32 { IntMatrix::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { IntMatrix::write(self, off, v); WriteEffect::INTMAP }
}
impl Device for Pcnt {
    fn read(&mut self, off: u32) -> u32 { Pcnt::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { Pcnt::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
    /// The glitch filter counts APB ticks, so the dispatch loop hands the unit that clock.
    fn clock(&self) -> Option<ClockDomain> { Some(ClockDomain::Apb) }
    fn tick(&mut self, ticks: u64) { self.advance(ticks); }
}
impl Device for Twai {
    fn read(&mut self, off: u32) -> u32 { Twai::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { Twai::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
    fn debug(&mut self, on: bool) { self.log = on; }
}
impl Device for LcdCam {
    fn read(&mut self, off: u32) -> u32 { LcdCam::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { LcdCam::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
    fn debug(&mut self, on: bool) { self.lcd_log = on; }
}
impl Device for Extmem {
    fn read(&mut self, off: u32) -> u32 { Extmem::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { Extmem::write(self, off, v); WriteEffect::NONE }
}
impl Device for Wdev {
    fn read(&mut self, off: u32) -> u32 { Wdev::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { Wdev::write(self, off, v); WriteEffect::NONE }
}
impl Device for I2cMst {
    fn read(&mut self, off: u32) -> u32 { I2cMst::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { I2cMst::write(self, off, v); WriteEffect::NONE }
}
/// The FE (RF front end) block is otherwise unmodelled; the one bit the WiFi blob polls is the
/// inferred "IQ estimation done" flag at +0x174. Ideal-radio PHY bring-up completes
/// even without an AP; this is not association or a silicon calibration timing model.
/// ESP-IDF v5.5.4 soc/fe_reg.h:23-31 names IQ_EST bits in FE_GEN_CTRL (+0x90),
/// not this undocumented result register; the IQ naming comes from those bits.
pub struct FeIq { word: u32 }
impl Device for FeIq {
    fn read(&mut self, _off: u32) -> u32 { self.word | (1 << 16) }
    fn write(&mut self, _off: u32, v: u32) -> WriteEffect { self.word = v; WriteEffect::NONE }
}

// ------------------------------------------------------------------ all together
pub struct Peripherals {
    pub usb: UsbSerialJtag,
    pub uart: [Uart; 3],
    pub systimer: Systimer,
    pub timg: [TimerGroup; 2],
    pub intmatrix: IntMatrix,
    pub gpio: Gpio,
    pub rtc: RtcCntl,
    pub efuse: Efuse,
    pub system: SystemRegs,
    pub extmem: Extmem,
    pub spi0: SpiMem,
    pub spi1: SpiMem,
    pub i2c: [crate::i2c::I2c; 2],
    pub lcd_cam: LcdCam,
    pub spi2: GpSpi,
    /// GP-SPI3 (Arduino `HSPI`), the same register block as SPI2, at DR_REG_SPI3_BASE 0x60025000
    /// (IDF v5.5.5 components/soc/esp32s3/register/soc/reg_base.h:39; v4.4.8 soc.h:74).
    pub spi3: GpSpi,
    pub pcnt: Pcnt,
    pub twai: Twai,
    pub wifi: WifiMac,
    pub fe: FeIq,
    pub aes: Aes,
    pub rsa: Rsa,
    pub sha: Sha,
    pub wdev: Wdev,
    pub i2c_mst: I2cMst,
    pub gdma: Gdma,
    pub i2s0: I2s,
    pub i2s1: I2s,
    pub rmt: Rmt,
    pub io_mux: RegRam,
    /// register RAM behind unmodelled blocks, first-touch logging, pc attribution
    pub misc: Misc,
    /// experiment hook: ESP_EMU_FAKE_READ=addr:or[:and],... applied to register reads
    pub fake_reads: HashMap<u32, (u32, u32)>,
    clock: ClockTree<4>,
    pub spi_exec: bool,       // SPI1 command pending execution against the flash array
    last_status: [u32; 4],
    pub intmatrix_dirty: bool,
    pub ledc: Ledc,
    pub mcpwm: [Mcpwm; 2],
}

// Every peripheral, where it sits, and its interrupt source numbers. Entries for one block are
// tried in order: a range-limited entry goes before the full-block one behind it.
device_set! { Peripherals; clock: (clock) CPU_HZ, [(ClockDomain::Systimer, 15), (ClockDomain::Apb, 3), (ClockDomain::RtcSlow, 1600), (ClockDomain::Cpu, 1)];
    0x00 "UART0" (uart[0]) => [SRC_UART0];
    0x10 "UART1" (uart[1]) => [SRC_UART1];
    0x2e "UART2" (uart[2]) => [SRC_UART2];
    0x38 "USB_SERIAL_JTAG" (usb) => [SRC_USB_SERIAL_JTAG];
    0x23 "SYSTIMER" (systimer) => [SRC_SYSTIMER_T0, SRC_SYSTIMER_T1, SRC_SYSTIMER_T2];
    0x1f "TIMG0" (timg[0]) => [SRC_TG0_T0, SRC_TG0_T1];
    0x20 "TIMG1" (timg[1]) => [SRC_TG1_T0, SRC_TG1_T1];
    0x19 "LEDC" optional (ledc) => [SRC_LEDC];
    0x1e "MCPWM0" optional (mcpwm[0]) => [SRC_PWM0];
    0x2c "MCPWM1" optional (mcpwm[1]) => [SRC_PWM1];
    0xc2 "INTERRUPT" (intmatrix) => [];
    0x04 "GPIO" (gpio) => [SRC_GPIO];
    0x08 "RTC" (rtc) => [];
    0x07 "EFUSE" (efuse) => [];
    0xc0 "SYSTEM" (system) => [SRC_FROM_CPU0, SRC_FROM_CPU0 + 1, SRC_FROM_CPU0 + 2, SRC_FROM_CPU0 + 3];
    0xc4 "EXTMEM" (extmem) => [];
    0x09 "IO_MUX" (io_mux) => [];
    0x02 "SPI1" (spi1) => [];
    0x03 "SPI0" (spi0) => [];
    0x3b "SHA" (sha) => [];
    0x0e "I2C_MST" (i2c_mst) => [];
    0x3f "GDMA" (gdma) => [SRC_DMA_OUT_CH0, SRC_DMA_OUT_CH0 + 1, SRC_DMA_OUT_CH0 + 2, SRC_DMA_OUT_CH0 + 3, SRC_DMA_OUT_CH0 + 4,
                           SRC_DMA_IN_CH0, SRC_DMA_IN_CH0 + 1, SRC_DMA_IN_CH0 + 2, SRC_DMA_IN_CH0 + 3, SRC_DMA_IN_CH0 + 4];
    0x0f "I2S0" (i2s0) => [SRC_I2S0];
    0x2d "I2S1" (i2s1) => [SRC_I2S1];
    0x16 "RMT" (rmt) => [SRC_RMT];
    0x13 "I2C0" optional (i2c[0]) => [SRC_I2C0];
    0x27 "I2C1" optional (i2c[1]) => [SRC_I2C1];
    0x41 "LCD_CAM" (lcd_cam) => [SRC_LCD_CAM];
    0x24 "SPI2" (spi2) => [SRC_SPI2];
    0x25 "SPI3" (spi3) => [SRC_SPI3];
    0x17 "PCNT" (pcnt) => [SRC_PCNT];
    0x2b "TWAI" (twai) => [SRC_TWAI];
    0x33 "WIFI_MAC" (wifi) => [SRC_WIFI_MAC];
    0x34 "WIFI_MAC" alias (wifi) delta 0x1000 => [];
    // block 0x35 is the MAC's third block for a few registers (TSF, interrupts) and WDEV otherwise
    0x35 "WIFI_MAC" alias (wifi) delta 0x2000 @ 0x0c..=0x1f => [];
    0x35 "WIFI_MAC" alias (wifi) delta 0x2000 @ 0x118..=0x11f => [];
    0x35 "WIFI_MAC" alias (wifi) delta 0x2000 @ 0x128..=0x12b => [];
    0x35 "WDEV" (wdev) => [];
    0x3a "AES" (aes) => [SRC_AES];
    0x3c "RSA" (rsa) => [SRC_RSA];
    0x06 "FE" (fe) @ 0x174..=0x177 => [];
}

impl DeviceSet for Peripherals {
    const BASE: u32 = PERIPH_BASE;
    fn block_name(block: u32) -> &'static str { Peripherals::block_name(block) }
    fn misc(&self) -> &Misc { &self.misc }
    fn misc_mut(&mut self) -> &mut Misc { &mut self.misc }
    /// The three registers whose value depends on another device.
    fn pre_access(&mut self, block: u32, _off: u32, write: bool) {
        match block {
            0xc2 if !write => self.intmatrix.status = self.source_status(),   // INTERRUPT_*_STATUS reads the live sources
            0x35 => self.wifi.now_cycles = self.clock.cycles(),               // TSF timestamps
            0x08 => self.rtc.now_cycles = self.clock.cycles(),                // SENS ADC samples analog sources at this time
            _ => {}
        }
    }
}

impl Peripherals {
    /// CPU cycles since this peripheral set was built.
    pub(crate) fn clock_cycles(&self) -> u64 { self.clock.cycles() }
    pub fn new(mac: [u8; 6]) -> Self {
        Peripherals {
            usb: UsbSerialJtag::new(CPU_HZ), uart: [Uart::new(UartLayout::S3), Uart::new(UartLayout::S3), Uart::new(UartLayout::S3)], systimer: Systimer::new(),
            timg: [TimerGroup::new(), TimerGroup::new()], intmatrix: IntMatrix::new(), gpio: Gpio::new(), ledc: Ledc::new(LedcLayout::S3), mcpwm: [Mcpwm::new(160, 9), Mcpwm::new(166, 9)], rtc: RtcCntl::new(),
            efuse: Efuse::new(mac), system: SystemRegs::new(0x30), extmem: Extmem::new(), spi0: SpiMem::new(false), spi1: SpiMem::new(true),
            i2c: [crate::i2c::I2c::new(), crate::i2c::I2c::new()], lcd_cam: LcdCam::new(), spi2: GpSpi::new(), spi3: GpSpi::with_host(3), pcnt: Pcnt::new(), twai: Twai::new(), wifi: WifiMac::new(CPU_HZ), fe: FeIq { word: 0 },
            aes: Aes::new(), rsa: Rsa::new(), sha: Sha::new(), wdev: Wdev::new(), i2c_mst: I2cMst::new(), gdma: Gdma::new(), i2s0: I2s::new(CPU_HZ), i2s1: I2s::new(CPU_HZ), rmt: Rmt::new(CPU_HZ),
            io_mux: RegRam::new(), misc: Misc::new(), fake_reads: std::env::var("ESP_EMU_FAKE_READ").ok().map(|v| v.split(',').filter_map(|e| { let mut p = e.split(':'); let a = u32::from_str_radix(p.next()?.trim_start_matches("0x"), 16).ok()?; let o = u32::from_str_radix(p.next().unwrap_or("0").trim_start_matches("0x"), 16).ok()?; let m = u32::from_str_radix(p.next().unwrap_or("ffffffff").trim_start_matches("0x"), 16).ok()?; Some((a, (o, m))) }).collect()).unwrap_or_default(),
            clock: Self::new_clock(),
            spi_exec: false, last_status: [0; 4], intmatrix_dirty: false,
        }
    }

    pub fn block_name_pub(block: u32) -> String { Self::block_name(block).to_string() }
    fn block_name(block: u32) -> &'static str {
        match block {
            0x00 => "UART0", 0x02 => "SPI1", 0x03 => "SPI0", 0x04 => "GPIO", 0x05 => "FE2", 0x06 => "FE", 0x07 => "EFUSE", 0x08 => "RTC", 0x09 => "IO_MUX",
            0x0b => "HINF", 0x0c => "UHCI1", 0x0f => "I2S0", 0x10 => "UART1", 0x11 => "BT", 0x13 => "I2C0", 0x14 => "UHCI0", 0x15 => "SLCHOST", 0x16 => "RMT", 0x17 => "PCNT",
            0x18 => "SLC", 0x19 => "LEDC", 0x1c => "NRX", 0x1d => "BB", 0x1e => "PWM0", 0x1f => "TIMG0", 0x20 => "TIMG1", 0x21 => "RTC_SLOWMEM", 0x23 => "SYSTIMER",
            0x24 => "SPI2", 0x25 => "SPI3", 0x26 => "APB_CTRL", 0x27 => "I2C1", 0x28 => "SDMMC", 0x2a => "PERI_BACKUP", 0x2b => "TWAI", 0x2c => "PWM1", 0x2d => "I2S1", 0x2e => "UART2", 0x33 => "WIFI_MAC", 0x34 => "WIFI_MAC2", 0x35 => "WDEV", 0x0e => "I2C_MST",
            0x38 => "USB_SERIAL_JTAG", 0x39 => "USB_WRAP", 0x3a => "AES", 0x3b => "SHA", 0x3c => "RSA", 0x3d => "DS", 0x3e => "HMAC", 0x3f => "GDMA", 0x40 => "APB_SARADC", 0x41 => "LCD_CAM",
            0xc0 => "SYSTEM", 0xc1 => "SENSITIVE", 0xc2 => "INTERRUPT", 0xc4 => "EXTMEM", 0xc5 => "MMU", 0xce => "ASSIST_DEBUG", 0xcf => "ASSIST_DEBUG2", 0xd0 => "WCL",
            _ => "?",
        }
    }

    pub fn uart_route(&self, port: usize) -> esp_soc::uart::UartRoute {
        // ESP-IDF v5.5.4 components/soc/esp32s3/include/soc/clk_tree_defs.h:
        // UART_SCLK_RTC = SOC_MOD_CLK_RC_FAST, SOC_CLK_RC_FAST_FREQ_APPROX = 17.5 MHz.
        // IDF 4.4 used 20 MHz (RTC_CLK_FREQ).
        let clock = self.uart[port].clock_config();
        esp_soc::uart::UartPins::S3.route(port, &self.gpio, &self.io_mux,
            self.uart[port].baud(clock, 17_500_000))
    }

    pub fn uart_pin_input(&mut self, input: &esp_soc::uart::UartInput) {
        let routes: [_; 3] = std::array::from_fn(|port| self.uart_route(port));
        esp_soc::uart::uart_pin_input(&mut self.uart, input, &routes);
    }

    pub fn read32(&mut self, addr: u32) -> u32 {
        let mut v = mmio::read32(self, addr);
        if !self.fake_reads.is_empty() { if let Some(&(o, m)) = self.fake_reads.get(&addr) { v = (v & m) | o; } }
        v
    }

    pub fn write32(&mut self, addr: u32, v: u32) {
        // IDF v5.5.4 components/soc/esp32s3/register/soc/io_mux_reg.h:42-50 (FUN_PD/PU).
        if (0x60009004..=0x600090c4).contains(&addr) {
            self.gpio.set_pad(((addr - 0x60009004) / 4) as u8, v);
        }
        if matches!(addr, 0x6001_3004 | 0x6002_7004) && v & (1 << 5) != 0 {
            let bus = usize::from(addr == 0x6002_7004);
            if self.i2c[bus].has_pinned_devices() {
                let sda = if bus == 0 { 90 } else { 92 };
                let pins = self.i2c_pin(sda).zip(self.i2c_pin(sda - 1));
                self.i2c[bus].set_pins(pins);
            }
        }
        if addr == PERIPH_BASE + 0xc0020 && v & (1 << 11) != 0 { self.ledc = Ledc::new(LedcLayout::S3); }
        if addr == PERIPH_BASE + 0xc0020 {
            for (group, bit) in [17, 20].iter().enumerate() { if v & (1 << bit) != 0 { self.mcpwm[group] = Mcpwm::new(160 + group as u32 * 6, 9); } }
        }
        let fx = mmio::write32(self, addr, v);
        if fx.contains(WriteEffect::SPI_EXEC) { self.spi_exec = true; }
        if fx.contains(WriteEffect::INTMAP) { self.intmatrix_dirty = true; }
        // ESP-IDF v5.5.4 soc/system_reg.h: PERIP_CLK_EN1/RST_EN1 LCD_CAM bit 8.
        if addr == PERIPH_BASE + 0xc001c || addr == PERIPH_BASE + 0xc0024 {
            self.lcd_cam.set_clock_enabled(self.system.read(0x1c) & (1 << 8) != 0 && self.system.read(0x24) & (1 << 8) == 0);
        }
        if addr == PERIPH_BASE + 0xc0018 || addr == PERIPH_BASE + 0xc0020 {
            for (group, bit) in [17, 20].iter().enumerate() { self.mcpwm[group].clock_enabled = self.system.read(0x18) & (1 << bit) != 0 && self.system.read(0x20) & (1 << bit) == 0; }
            self.ledc.clock_enabled = self.system.read(0x18) & (1 << 11) != 0 && self.system.read(0x20) & (1 << 11) == 0;
            for block in [0x19, 0x1e, 0x2c] { self.refresh_optional(block); }
        }
    }

    /// Returns the number of words applied.
    pub fn init_regs(&mut self, addr: u32, v: u32) -> bool {
        if !(PERIPH_BASE..PERIPH_END).contains(&addr) { return false; }
        let block = (addr - PERIPH_BASE) >> 12; let off = addr & 0xfff;
        match block {
            0x08 => self.rtc.ram.write(off, v),
            0xc0 => { if (0x30..=0x3c).contains(&off) { return false; } self.system.ram.write(off, v) }
            0xc4 => self.extmem.ram.write(off, v),
            0x09 => self.io_mux.write(off, v),
            0x02 => { if off == 0 || (0x58..=0x94).contains(&off) { return false; } self.spi1.regs.write(off, v) }
            0x03 => { if off == 0 { return false; } self.spi0.regs.write(off, v) }
            0x0e => self.i2c_mst.ram.write(off, v),
            0x26 | 0xc1 => self.misc.generic.entry(block).or_default().write(off, v),
            _ => return false,
        }
        true
    }

    /// Advance device time by `cycles` CPU cycles (every clocked device gets its own clock's
    /// ticks), then route GPIO input edges to the pulse counters.
    pub fn tick(&mut self, cycles: u64) -> bool {
        let mut irq_changed = Dispatch::tick(self, cycles);
        if !self.gpio.input_changes.is_empty() {
            let before = self.pcnt.irq();
            let gpio = &mut self.gpio;
            let input = gpio.input;
            let selections = &gpio.func_in_sel;
            let sig = |idx: u32| -> Option<(u8, bool)> {
                let sel = *selections.get(idx as usize)?;
                if sel & 0x80 == 0 { return None; }                       // not routed through the matrix
                let pin = (sel & 0x3f) as u8; if pin >= 49 { return None; }
                let lvl = (input >> pin) & 1 != 0; Some((pin, lvl ^ (sel & 0x40 != 0)))
            };
            for (pin, level) in gpio.input_changes.drain(..) { self.pcnt.gpio_edge(pin, level, &sig); }
            irq_changed |= before != self.pcnt.irq();
        }
        irq_changed
    }

    /// Raw per-source interrupt status (4 × 32 bits).
    pub fn source_status(&self) -> [u32; 4] { Dispatch::source_status(self) }

    /// The I2S controller carrying the board's audio output (whichever has played samples).
    pub fn audio(&self) -> &I2s { if self.i2s1.frames_out > self.i2s0.frames_out { &self.i2s1 } else { &self.i2s0 } }

    /// Cycles until the next systimer or timer-group alarm can fire — the only device events
    /// firmware times precisely. Slightly conservative (one device tick early), never late.
    pub fn cycles_until_timer(&self) -> u32 { Dispatch::cycles_until_deadline(self) }
    pub fn lines_dirty(&mut self) -> bool { let st = self.source_status(); if st != self.last_status { self.last_status = st; true } else { false } }
    /// Interrupt lines for both cores in one pass over the sources that are active.
    pub fn cpu_lines_both(&self) -> (u32, u32) {
        let st = self.last_status;
        let (mut l0, mut l1) = (0u32, 0u32);
        for (w, &word) in st.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let b = bits.trailing_zeros(); bits &= bits - 1;
                let src = w * 32 + b as usize; if src >= NUM_SOURCES { break; }
                let n0 = self.intmatrix.map[0][src]; if n0 < 32 { l0 |= 1 << n0; }
                let n1 = self.intmatrix.map[1][src]; if n1 < 32 { l1 |= 1 << n1; }
            }
        }
        (l0, l1)
    }

    /// CPU interrupt lines for `core` (bit n = Xtensa interrupt n) after the interrupt matrix.
    pub fn cpu_lines(&self, core: usize) -> u32 {
        let st = self.source_status();
        let mut lines = 0u32;
        for src in 0..NUM_SOURCES {
            if st[src / 32] & (1 << (src % 32)) != 0 { let n = self.intmatrix.map[core][src]; if n < 32 { lines |= 1 << n; } }
        }
        lines
    }
    /// SYSTEM_CORE_1_CONTROL_0: (clkgate_en, reseting, runstall)
    pub fn core1_control(&self) -> (bool, bool, bool) { let v = self.system.ram.read(0); (v & 2 != 0, v & 4 != 0, v & 1 != 0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn std_id_bytes(id: u32) -> [u8; 2] { (((id & 0x7ff) << 5) as u16).swap_bytes().to_le_bytes() }
    fn ext_id_bytes(id: u32) -> [u8; 4] { ((id & 0x1fff_ffff) << 3).swap_bytes().to_le_bytes() }

    fn write_frame_header(twai: &mut Twai, info: u8, id: &[u8]) {
        twai.write(0x40, u32::from(info));
        for (i, byte) in id.iter().enumerate() { twai.write(0x44 + 4 * i as u32, u32::from(*byte)); }
    }

    #[test]
    fn twai_fd_transmit_and_receive_roundtrip() {
        let mut twai = Twai::new();
        twai.write(0x00, 0);                       // normal mode
        twai.write(TWAI_FD_CTRL, 0x3);             // FD, BRS

        let payload: Vec<u8> = (0..64).map(|i| i as u8).collect();
        write_frame_header(&mut twai, 15 | 0x80, &ext_id_bytes(0x1abc_def));   // DLC 15 = 64 bytes, extended
        for (i, byte) in payload.iter().enumerate() {
            twai.write(TWAI_FD_DATA_BASE + 4 * i as u32, u32::from(*byte));
        }
        twai.write(0x04, TWAI_CMD_TR | TWAI_CMD_SRR);

        let (frame, self_rx) = twai.take_tx_request().expect("FD frame is latched");
        assert_eq!(frame.kind, TwaiFrameKind::Fd { bit_rate_switch: true });
        assert_eq!((frame.id, frame.extended, frame.len), (0x1abc_def, true, 64));
        assert_eq!(&frame.data[..], payload.as_slice());
        assert!(self_rx, "SRR delivered the frame locally");

        twai.receive_frame(&frame);
        assert_eq!(twai.read(0x08) & TWAI_STATUS_RBS, TWAI_STATUS_RBS);
        assert_eq!(twai.read(0x74), 1);
        assert_eq!(twai.read(TWAI_FD_CTRL), 0x3, "RX buffer reports FD and BRS");
        assert_eq!((twai.read(0x40) as u8) & 0x0f, 15, "the FD DLC code is visible");
        assert_eq!(twai.read(TWAI_FD_DATA_BASE + 4 * 63) as u8, 63);

        twai.write(0x04, TWAI_CMD_RRB);
        assert_eq!(twai.read(0x74), 0);
        assert_eq!(twai.read(0x08) & TWAI_STATUS_RBS, 0);
    }

    #[test]
    fn twai_fd_wire_roundtrip_keeps_the_classic_prefix() {
        let payload: Vec<u8> = (0..64).map(|i| 0xff - i as u8).collect();
        let frame = TwaiFrame::fd(0x1ff_ffff, true, false, &payload);
        let wire = frame.encode_wire();
        assert!(wire.len() >= 15 + TWAI_FD_MAX_DATA);
        assert_eq!(TwaiFrame::decode_wire(&wire), Some(frame));

        let classic = TwaiFrame::classic(0x123, false, false, 3, &[1, 2, 3]);
        let wire = classic.encode_wire();
        assert_eq!(wire.len(), esp_soc::board::CAN_FRAME_BYTES, "classic stays fourteen bytes");
        assert_eq!(TwaiFrame::decode_wire(&wire), Some(classic));
    }

    #[test]
    fn twai_arbitration_loss_counts_and_raises_ali() {
        let mut twai = Twai::new();
        twai.write(0x00, 0);
        write_frame_header(&mut twai, 1, &std_id_bytes(0x100));   // own standard id 0x100

        let lower = TwaiFrame::classic(0x080, false, false, 1, &[0x55]);
        assert_eq!(twai.arbitrate_against(&lower), TwaiArbitration::Lost);
        assert_eq!(twai.tx_err, 8, "a loss adds eight to the transmit counter");
        assert_ne!(twai.int_raw & TWAI_INT_ALI, 0);

        let higher = TwaiFrame::classic(0x200, false, false, 1, &[0x55]);
        assert_eq!(twai.arbitrate_against(&higher), TwaiArbitration::Won);
        assert_eq!(twai.tx_err, 8, "winning leaves the counter alone");

        let same_id_other_data = TwaiFrame::classic(0x100, false, false, 1, &[0x66]);
        assert_eq!(twai.arbitrate_against(&same_id_other_data), TwaiArbitration::Collision);
        assert_eq!(twai.tx_err, 16);
        assert_eq!(twai.rx_err, 8, "a collision also counts as a receive error");
    }

    #[test]
    fn twai_bus_off_stops_transmission_until_reset() {
        let mut twai = Twai::new();
        twai.write(0x00, 0);
        for _ in 0..32 { twai.record_arbitration_loss(); }   // 32 * 8 = 256

        assert!(twai.error_passive());
        assert!(twai.bus_off());
        assert_eq!(twai.read(0x08) & (TWAI_STATUS_ES | TWAI_STATUS_BS), TWAI_STATUS_ES | TWAI_STATUS_BS);
        assert!(!twai.can_transmit());

        twai.write(0x40, 0);                        // DLC 0, standard
        twai.write(0x04, TWAI_CMD_TR);
        assert!(twai.take_tx_request().is_none(), "a bus-off controller does not latch a frame");

        twai.write(0x00, TWAI_MODE_RM);
        assert_eq!(twai.tx_err, 0, "reset mode clears the counters");
        assert!(!twai.bus_off());
        twai.write(0x00, 0);
        assert!(twai.can_transmit(), "leaving reset restores transmission");
    }

    #[test]
    fn twai_malformed_frame_counts_a_receive_error() {
        let mut twai = Twai::new();
        twai.write(0x00, 0);
        twai.receive_malformed();
        assert_eq!(twai.rx_err, 8);
        assert_ne!(twai.int_raw & TWAI_INT_EI, 0);
        twai.receive_malformed();
        assert_eq!(twai.rx_err, 16);
        assert!(!twai.error_passive(), "16 is below the error-passive threshold");
    }

    fn input(pin: u8) -> impl Fn(u32) -> Option<(u8, bool)> {
        move |idx| if idx == 33 { Some((pin, false)) } else { None }
    }

    #[test]
    fn pcnt_counts_up_and_down_on_configured_edges() {
        let mut pcnt = Pcnt::new();
        let sig = input(4);
        pcnt.conf[0][0] = (1 << 18) | (1 << 16);    // ch0: rising increment, falling increment
        pcnt.gpio_edge(4, true, &sig);
        assert_eq!(pcnt.cnt[0], 1);
        pcnt.gpio_edge(4, false, &sig);
        assert_eq!(pcnt.cnt[0], 2);

        pcnt.conf[0][0] = (1 << 18) | (2 << 16);    // rising increment, falling decrement
        pcnt.gpio_edge(4, true, &sig);
        assert_eq!(pcnt.cnt[0], 3);
        pcnt.gpio_edge(4, false, &sig);
        assert_eq!(pcnt.cnt[0], 2);
    }

    #[test]
    fn pcnt_high_and_low_limits_reset_and_raise() {
        let mut pcnt = Pcnt::new();
        let sig = input(4);
        pcnt.conf[0][0] = (1 << 18) | (1 << 12);    // rising increment, high limit enabled
        pcnt.conf[0][2] = 2;                         // high limit 2
        pcnt.int_ena = 1;
        pcnt.gpio_edge(4, true, &sig);
        assert_eq!(pcnt.cnt[0], 1);
        pcnt.gpio_edge(4, true, &sig);
        assert_eq!(pcnt.cnt[0], 0, "reaching the high limit clears the counter");
        assert_ne!(pcnt.status[0] & (1 << 5), 0);
        assert!(pcnt.irq());

        let mut pcnt = Pcnt::new();
        pcnt.conf[0][0] = (2 << 16) | (1 << 13);    // falling decrement, low limit enabled
        pcnt.conf[0][2] = 0;                         // low limit 0
        pcnt.cnt[0] = 1;
        pcnt.int_ena = 1;
        pcnt.gpio_edge(4, false, &sig);
        assert_eq!(pcnt.cnt[0], 0);
        pcnt.gpio_edge(4, false, &sig);
        assert_eq!(pcnt.cnt[0], 0, "reaching the low limit clears the counter");
        assert_ne!(pcnt.status[0] & (1 << 4), 0);
        assert!(pcnt.irq());
    }

    #[test]
    fn pcnt_glitch_filter_discards_short_pulses() {
        let mut pcnt = Pcnt::new();
        let sig = input(4);
        pcnt.conf[0][0] = (1 << 18) | (1 << 10) | 5;   // rising increment, filter 5 APB ticks

        pcnt.gpio_edge(4, true, &sig);                 // held until now 5
        pcnt.advance(2);
        pcnt.gpio_edge(4, false, &sig);                // opposite inside the window: glitch
        pcnt.advance(10);
        assert_eq!(pcnt.cnt[0], 0, "a pulse narrower than the filter is discarded");

        pcnt.gpio_edge(4, true, &sig);                 // due at now 12 + 5
        pcnt.advance(5);
        assert_eq!(pcnt.cnt[0], 1, "a stable edge is counted once the window closes");
        pcnt.gpio_edge(4, false, &sig);                // falling mode is 0: no count
        pcnt.advance(5);
        assert_eq!(pcnt.cnt[0], 1);
    }

    #[test]
    fn pcnt_reset_pulse_clears_count_and_pending_edges() {
        let mut pcnt = Pcnt::new();
        let sig = input(4);
        pcnt.conf[0][0] = (1 << 18) | (1 << 10) | 5;
        pcnt.gpio_edge(4, true, &sig);
        pcnt.advance(5);
        assert_eq!(pcnt.cnt[0], 1);

        pcnt.gpio_edge(4, true, &sig);                 // a held edge
        pcnt.write(0x60, 1);                            // CNT_RST_U0 pulse
        assert_eq!(pcnt.cnt[0], 0);
        assert_eq!(pcnt.ctrl & 1, 0, "CNT_RST is a pulse, not a held bit");
        pcnt.advance(100);
        assert_eq!(pcnt.cnt[0], 0, "the reset drops the pending edge");

        pcnt.gpio_edge(4, true, &sig);
        pcnt.advance(5);
        assert_eq!(pcnt.cnt[0], 1, "the unit still counts after a reset pulse");
    }

    #[test]
    fn pcnt_pause_holds_the_counter() {
        let mut pcnt = Pcnt::new();
        let sig = input(4);
        pcnt.conf[0][0] = 1 << 18;
        pcnt.write(0x60, 1 << 1);                       // CNT_PAUSE_U0
        pcnt.gpio_edge(4, true, &sig);
        assert_eq!(pcnt.cnt[0], 0);
        pcnt.write(0x60, 0);
        pcnt.gpio_edge(4, true, &sig);
        assert_eq!(pcnt.cnt[0], 1);
    }

    #[test]
    fn pcnt_zero_crossing_records_its_direction() {
        let mut pcnt = Pcnt::new();
        let sig = input(4);
        pcnt.conf[0][0] = (1 << 18) | (1 << 11);       // rising increment, zero compare
        pcnt.cnt[0] = -1;
        pcnt.gpio_edge(4, true, &sig);
        assert_eq!(pcnt.cnt[0], 0);
        assert_ne!(pcnt.status[0] & (1 << 6), 0, "ZERO_LAT");
        assert_eq!(pcnt.status[0] & 3, 0, "positive-to-zero");

        let mut pcnt = Pcnt::new();
        pcnt.conf[0][0] = (2 << 16) | (1 << 11);       // falling decrement, zero compare
        pcnt.cnt[0] = 1;
        pcnt.gpio_edge(4, false, &sig);
        assert_eq!(pcnt.cnt[0], 0);
        assert_ne!(pcnt.status[0] & (1 << 6), 0, "ZERO_LAT");
        assert_eq!(pcnt.status[0] & 3, 1, "negative-to-zero");
    }
}
