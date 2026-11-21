//! The RP2040's full-speed USB device controller and its 4 KiB DPSRAM.
//!
//! The silicon is only half a controller: it serialises packets, but the
//! firmware decides what to send and the *host* decides when. This model
//! therefore has two halves:
//!
//! * the register and DPSRAM side, which is what the firmware sees. Buffer
//!   control words are owned jointly: the firmware writes `AVAILABLE` (and
//!   `FULL` for an IN) to hand a buffer over, and the controller clears them
//!   when the transaction completes, exactly as the datasheet's Table 4.1.2.7.4
//!   describes.
//! * a host state machine in [`Usb::tick`], which stands in for the PC on the
//!   other end of the cable. It does not run on a wall clock: it advances one
//!   action per [`HOST_STEP_CYCLES`] of simulated CPU time, so the same
//!   firmware reaches the same console bytes every run. It waits for the
//!   firmware to arm a buffer before issuing a token, which is how a real host
//!   copes with a device that NAKs while it is not ready.
//!
//! Enumeration is a scripted control-transfer sequence (device descriptor,
//! address, configuration descriptor, configuration, CDC line coding). The
//! configuration descriptor is parsed to find the bulk endpoints, so the host
//! follows the descriptors rather than assuming the RP2 TinyUSB defaults.
//! Once configured, bytes the host writes are delivered to the CDC OUT
//! endpoint and bytes the firmware writes to the CDC IN endpoint are appended
//! to [`Usb::output`], which the chip adapter drains into the console.
//!
//! Timing interrupts (`DEV_SOF`) are generated once per simulated millisecond
//! because the RP2040-E15 workaround TinyUSB enables defers bulk-IN arming
//! until a SOF arrives; without them a firmware built with the workaround
//! would arm the first IN packet and never a second.

use std::collections::VecDeque;

use super::WriteMode;

/// The registers sit at USBCTRL_REGS (the DPSRAM occupies the 4 KiB below).
pub const REGS_SIZE: usize = 0x100;

/// The buffer layout from datasheet Table 394.
pub mod dpram {
    /// `SETUP_PACKET_LOW`, then `SETUP_PACKET_HIGH` at 0x04.
    pub const SETUP: usize = 0x00;
    /// `EPn_IN_CONTROL`/`EPn_OUT_CONTROL`, eight bytes per endpoint, EP1 first.
    pub const EP_CTRL: usize = 0x08;
    /// `EPn_IN_BUFFER_CONTROL`/`EPn_OUT_BUFFER_CONTROL`, eight bytes per
    /// endpoint, EP0 first.
    pub const EP_BUF_CTRL: usize = 0x80;
    /// EP0's fixed data buffer. IN and OUT share it: EP0 is single buffered.
    pub const EP0_BUF: usize = 0x100;

    /// Control word for endpoint `ep` in direction `out`.
    pub fn ep_ctrl(ep: usize, out: bool) -> usize {
        EP_CTRL + 8 * (ep - 1) + usize::from(out) * 4
    }

    /// Buffer control word for endpoint `ep` in direction `out`.
    pub fn ep_buf_ctrl(ep: usize, out: bool) -> usize {
        EP_BUF_CTRL + 8 * ep + usize::from(out) * 4
    }
}

/// Register offsets from USBCTRL_REGS, from `hardware/regs/usb.h`.
pub mod reg {
    pub const ADDR_ENDP: u32 = 0x000;
    pub const MAIN_CTRL: u32 = 0x040;
    pub const SOF_WR: u32 = 0x044;
    pub const SOF_RD: u32 = 0x048;
    pub const SIE_CTRL: u32 = 0x04c;
    pub const SIE_STATUS: u32 = 0x050;
    pub const INT_EP_CTRL: u32 = 0x054;
    pub const BUFF_STATUS: u32 = 0x058;
    pub const BUFF_CPU_SHOULD_HANDLE: u32 = 0x05c;
    pub const EP_ABORT: u32 = 0x060;
    pub const EP_ABORT_DONE: u32 = 0x064;
    pub const EP_STALL_ARM: u32 = 0x068;
    pub const NAK_POLL: u32 = 0x06c;
    pub const EP_STATUS_STALL_NAK: u32 = 0x070;
    pub const USB_MUXING: u32 = 0x074;
    pub const USB_PWR: u32 = 0x078;
    pub const USBPHY_DIRECT: u32 = 0x07c;
    pub const USBPHY_DIRECT_OVERRIDE: u32 = 0x080;
    pub const USBPHY_TRIM: u32 = 0x084;
    pub const INTR: u32 = 0x08c;
    pub const INTE: u32 = 0x090;
    pub const INTF: u32 = 0x094;
    pub const INTS: u32 = 0x098;
}

/// `MAIN_CTRL` bits.
pub mod main_ctrl {
    pub const CONTROLLER_EN: u32 = 1 << 0;
    pub const HOST_NDEVICE: u32 = 1 << 1;
}

/// `SIE_CTRL` bits the host side observes.
pub mod sie_ctrl {
    pub const PULLUP_EN: u32 = 1 << 16;
}

/// `SIE_STATUS` bits the host side sets, and the firmware clears through the
/// atomic clear alias.
pub mod sie_status {
    pub const VBUS_DETECTED: u32 = 1 << 0;
    /// The bits a firmware ack clears. `VBUS_DETECTED`, `LINE_STATE` and
    /// `VBUS_OVER_CURR` are read-only, and `CONNECTED` describes the cable
    /// rather than an event: the controller sets it again on the next frame
    /// while the pullup is on, so clearing it does not stick either.
    pub const WRITE_CLEAR: u32 = !(VBUS_DETECTED | (0xf << 2) | (1 << 10) | CONNECTED);
    pub const SETUP_REC: u32 = 1 << 17;
    pub const BUS_RESET: u32 = 1 << 19;
    pub const CONNECTED: u32 = 1 << 16;
    pub const TRANS_COMPLETE: u32 = 1 << 18;
}

/// `USB_PWR` bits.
pub mod pwr {
    pub const VBUS_DETECT: u32 = 1 << 2;
    pub const VBUS_DETECT_OVERRIDE_EN: u32 = 1 << 3;
}

/// `INTR`/`INTE`/`INTS` bit positions.
pub mod intr {
    pub const BUFF_STATUS: u32 = 1 << 4;
    pub const BUS_RESET: u32 = 1 << 12;
    pub const SETUP_REQ: u32 = 1 << 16;
    pub const DEV_SOF: u32 = 1 << 17;
    pub const ABORT_DONE: u32 = 1 << 18;
    pub const EP_STALL_NAK: u32 = 1 << 19;
}

/// Endpoint control word bits (datasheet Table 395).
pub mod ep_ctrl {
    pub const ENABLE: u32 = 1 << 31;
    pub const DOUBLE_BUFFERED: u32 = 1 << 30;
    pub const INTERRUPT_PER_BUFFER: u32 = 1 << 29;
    pub const TYPE_LSB: u32 = 26;
    pub const TYPE_MASK: u32 = 0x3 << TYPE_LSB;
    pub const BUFFER_ADDRESS_MASK: u32 = 0xffff;
    pub const ADDRESS_ALIGN: u32 = !0x3f;
}

/// Buffer control word bits (datasheet Table 396). Bit 16..31 are buffer 1.
pub mod buf_ctrl {
    pub const LEN_MASK: u32 = 0x3ff;
    pub const AVAIL: u32 = 1 << 10;
    pub const STALL: u32 = 1 << 11;
    pub const RESET: u32 = 1 << 12;
    pub const DATA1_PID: u32 = 1 << 13;
    pub const LAST: u32 = 1 << 14;
    pub const FULL: u32 = 1 << 15;
}

/// The full-speed control endpoint's packet size.
const EP0_MPS: usize = 64;
/// The CDC bulk endpoints the descriptors advertise are 64-byte packets too.
const BULK_MPS: usize = 64;
/// One host action per microsecond of simulated time (125 MHz core).
const HOST_STEP_CYCLES: u64 = 125;
/// Host steps to wait for a firmware handshake before driving on. Sixty-four
/// steps is 8 µs of simulated bus time: long enough for an interrupt handler,
/// short enough that firmware which never answers does not wedge the port.
const HANDSHAKE_TIMEOUT: u32 = 64;
/// A frame is 1 ms, so the host sends a SOF this often.
const SOF_CYCLES: u64 = 125_000;
/// How long the host leaves `SIE_STATUS.BUS_RESET` asserted before giving up
/// waiting for firmware to acknowledge it. A real host drives SE0 for 10 ms;
/// this is a fallback only for firmware that never enables the interrupt.
const RESET_TIMEOUT_STEPS: u32 = 5_000;
/// A pull-up that stays low for a millisecond is a disconnect. The RP2040-E5
/// workaround pulses the pull-up for a few microseconds on a bus reset, and a
/// short pulse must not look like an unplug.
const DETACH_STEPS: u32 = 1_000;

/// Bulk endpoint numbers found in the configuration descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CdcEndpoints {
    /// CDC data IN (device to host), e.g. TinyUSB's `0x82`.
    pub in_ep: u8,
    /// CDC data OUT (host to device), e.g. TinyUSB's `0x02`.
    pub out_ep: u8,
}

/// What the host is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Detached,
    /// Attached, waiting for the firmware to acknowledge the bus reset.
    WaitReset,
    GetDevice,
    SetAddress,
    GetConfig,
    SetConfig,
    SetLineCoding,
    GetLineCoding,
    SetControlLineState,
    Running,
}

/// Which part of a control transfer is next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Write the SETUP packet to DPSRAM and raise `SETUP_REQ`.
    Deliver,
    /// Wait for the firmware's interrupt handler to take the packet, so the
    /// buffer it arms belongs to this transfer and not the previous one.
    Arm,
    /// Move the data stage, waiting for the firmware to arm EP0.
    Data,
    /// Complete with a zero-length IN on EP0.
    StatusIn,
    /// Complete with a zero-length OUT on EP0.
    StatusOut,
}

/// Which scripted request a completed control transfer belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    GetDevice,
    SetAddress,
    GetConfig,
    SetConfig,
    SetLineCoding,
    GetLineCoding,
    SetControlLineState,
}

#[derive(Debug)]
struct Control {
    setup: [u8; 8],
    request: Request,
    /// `wLength`.
    length: usize,
    /// True when the data stage (if any) travels device to host.
    dir_in: bool,
    /// Payload for an OUT data stage.
    out: Vec<u8>,
    sent: usize,
    /// Bytes received from an IN data stage.
    received: Vec<u8>,
    /// True when the status stage is an IN transfer.
    status_in: bool,
    step: Step,
    /// The device answered with STALL; complete without data.
    stalled: bool,
    /// An OUT data stage handed the device a buffer that it has not read yet.
    /// The host must not start the next transfer before the device drains it.
    await_out_ack: bool,
    /// Host steps spent waiting on the setup acknowledgement.
    arm_wait: u32,
    /// Host steps spent waiting on the status stage.
    status_wait: u32,
}

impl Control {
    fn new(setup: [u8; 8], request: Request, out: Vec<u8>) -> Self {
        let length = usize::from(u16::from_le_bytes([setup[6], setup[7]]));
        let dir_in = setup[0] & 0x80 != 0;
        // With no data stage the status stage is always an IN. Otherwise it is
        // the direction opposite the data.
        let status_in = length == 0 || !dir_in;
        Control {
            setup,
            request,
            length,
            dir_in,
            out,
            sent: 0,
            received: Vec::new(),
            status_in,
            step: Step::Deliver,
            stalled: false,
            await_out_ack: false,
            arm_wait: 0,
            status_wait: 0,
        }
    }
}

/// The USB device controller.
#[derive(Debug)]
pub struct Usb {
    regs: [u32; REGS_SIZE / 4],
    dpram: [u8; 4096],
    /// `BUFF_STATUS`: the controller sets a bit per completed buffer.
    buf_status: u32,
    /// `BUFF_CPU_SHOULD_HANDLE`: which half of a double buffer completed.
    should_handle: u32,
    /// Frame counter at `SOF_RD`.
    sof_count: u32,
    /// A SOF arrived since the last `SOF_RD` read.
    sof_pending: bool,
    /// Host action accumulator, in CPU cycles.
    host_acc: u64,
    /// SOF accumulator, in CPU cycles.
    sof_acc: u64,
    attached: bool,
    detach_steps: u32,
    reset_steps: u32,
    stage: Stage,
    control: Option<Control>,
    address: u8,
    device_desc: Vec<u8>,
    config_desc: Vec<u8>,
    cdc: Option<CdcEndpoints>,
    /// The double-buffer selector the host has reached per endpoint.
    next_buf: [bool; 16],
    /// Bytes the host wants to send into the CDC OUT endpoint.
    input: VecDeque<u8>,
    /// Bytes the device sent on a bulk IN endpoint, for the console.
    output: Vec<u8>,
}

impl Default for Usb {
    fn default() -> Self {
        Self::new()
    }
}

impl Usb {
    pub fn new() -> Self {
        let mut regs = [0u32; REGS_SIZE / 4];
        // The reset values of the two trim registers, so a firmware that
        // reads them back sees a plausible part rather than zeros.
        regs[(reg::NAK_POLL / 4) as usize] = 0x0010_0010;
        regs[(reg::USBPHY_TRIM / 4) as usize] = 0x0000_1f1f;
        Usb {
            regs,
            dpram: [0; 4096],
            buf_status: 0,
            should_handle: 0,
            sof_count: 0,
            sof_pending: false,
            host_acc: 0,
            sof_acc: 0,
            attached: false,
            detach_steps: 0,
            reset_steps: 0,
            stage: Stage::Detached,
            control: None,
            address: 0,
            device_desc: Vec::new(),
            config_desc: Vec::new(),
            cdc: None,
            next_buf: [false; 16],
            input: VecDeque::new(),
            output: Vec::new(),
        }
    }

    fn reg_index(off: u32) -> usize {
        ((off & !3) / 4) as usize
    }

    /// The raw interrupt flags, derived from the sources rather than latched
    /// separately: clearing `BUFF_STATUS` or `SIE_STATUS.SETUP_REC` must drop
    /// the level that holds the NVIC line.
    fn raw_intr(&self) -> u32 {
        let status = self.regs[Self::reg_index(reg::SIE_STATUS)];
        let mut intr = 0;
        if self.buf_status != 0 {
            intr |= intr::BUFF_STATUS;
        }
        if status & sie_status::BUS_RESET != 0 {
            intr |= intr::BUS_RESET;
        }
        if status & sie_status::SETUP_REC != 0 {
            intr |= intr::SETUP_REQ;
        }
        if self.sof_pending {
            intr |= intr::DEV_SOF;
        }
        if self.regs[Self::reg_index(reg::EP_ABORT_DONE)] != 0 {
            intr |= intr::ABORT_DONE;
        }
        if self.regs[Self::reg_index(reg::EP_STATUS_STALL_NAK)] != 0 {
            intr |= intr::EP_STALL_NAK;
        }
        intr
    }

    /// `INTS`: the raw flags gated by `INTE`, plus the forced bits.
    fn ints(&self) -> u32 {
        let raw = self.raw_intr();
        let enable = self.regs[Self::reg_index(reg::INTE)];
        let force = self.regs[Self::reg_index(reg::INTF)];
        (raw & enable) | force
    }

    /// The NVIC line.
    pub fn irq(&self) -> bool {
        self.ints() != 0
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off & !3 {
            reg::SOF_RD => {
                // Reading the frame number acknowledges a DEV_SOF.
                self.sof_pending = false;
                self.sof_count & 0x7ff
            }
            reg::SIE_STATUS => self.regs[Self::reg_index(reg::SIE_STATUS)],
            reg::BUFF_STATUS => self.buf_status,
            reg::BUFF_CPU_SHOULD_HANDLE => self.should_handle,
            reg::INTR => self.raw_intr(),
            reg::INTS => self.ints(),
            _ => {
                let index = Self::reg_index(off);
                self.regs.get(index).copied().unwrap_or(0)
            }
        }
    }

    /// The storage a register's bit operations act on. `BUFF_STATUS` and
    /// `BUFF_CPU_SHOULD_HANDLE` are derived fields, not `regs` slots.
    fn field_mut(&mut self, off: u32) -> Option<&mut u32> {
        match off & !3 {
            reg::BUFF_STATUS => Some(&mut self.buf_status),
            reg::BUFF_CPU_SHOULD_HANDLE => Some(&mut self.should_handle),
            reg::INTR | reg::INTS => None,
            other => self.regs.get_mut(Self::reg_index(other)),
        }
    }

    /// A write through one of the three alias windows. The aliases are plain
    /// bit operations on the stored value, so they must bypass the
    /// write-one-to-clear rule a plain write to a status register follows.
    pub fn write_with(&mut self, off: u32, mode: WriteMode, value: u32) {
        match mode {
            WriteMode::Normal => self.write(off, value),
            mode => {
                let value = if off & !3 == reg::SIE_STATUS {
                    value & sie_status::WRITE_CLEAR
                } else {
                    value
                };
                let previous = self.field_mut(off).copied().unwrap_or(0);
                let updated = match mode {
                    WriteMode::Set => previous | value,
                    WriteMode::Clear => previous & !value,
                    _ => previous ^ value,
                };
                if let Some(field) = self.field_mut(off) {
                    *field = updated;
                }
                // An abort is an action, not just a latch: the firmware
                // requests one through the SET alias and waits on
                // `EP_ABORT_DONE`, so the completion must follow the alias
                // write too.
                if off & !3 == reg::EP_ABORT {
                    self.complete_abort(updated);
                }
            }
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        let index = Self::reg_index(off);
        match off & !3 {
            // The controller owns these generate-only facts; a firmware write
            // goes through the SET/CLR aliases to acknowledge them.
            reg::INTR | reg::INTS | reg::BUFF_CPU_SHOULD_HANDLE => {}
            // Completion and status flags are write-one-to-clear: TinyUSB
            // acknowledges a buffer by writing its bit back, and a model that
            // stored the write instead would leave the level held high, so the
            // interrupt handler would service the same buffer forever.
            reg::BUFF_STATUS => self.buf_status &= !value,
            reg::SIE_STATUS => {
                self.regs[index] &= !(value & sie_status::WRITE_CLEAR);
            }
            reg::EP_STATUS_STALL_NAK | reg::EP_ABORT_DONE => self.regs[index] &= !value,
            reg::EP_ABORT => {
                self.regs[index] = value;
                self.complete_abort(value);
            }
            _ => {
                if let Some(word) = self.regs.get_mut(index) {
                    *word = value;
                }
            }
        }
    }

    /// An abort stops the endpoint and the controller reports it idle; the
    /// model's host has no transaction in flight between ticks, so completion
    /// is immediate.
    fn complete_abort(&mut self, value: u32) {
        for ep in 0..16 {
            let bit = ep_bit(ep, false);
            if value & bit != 0 {
                self.write_dpram_word(dpram::ep_buf_ctrl(ep, false) as u32, 0);
            }
            let bit = ep_bit(ep, true);
            if value & bit != 0 {
                self.write_dpram_word(dpram::ep_buf_ctrl(ep, true) as u32, 0);
            }
        }
        let done = Self::reg_index(reg::EP_ABORT_DONE);
        self.regs[done] |= value;
    }

    /// Read a word from DPSRAM, for the bus.
    pub fn read_dpram_word(&self, offset: u32) -> u32 {
        let at = (offset as usize) & 0xffc;
        u32::from_le_bytes([
            self.dpram[at],
            self.dpram[at + 1],
            self.dpram[at + 2],
            self.dpram[at + 3],
        ])
    }

    /// Write a word to DPSRAM, for the bus.
    pub fn write_dpram_word(&mut self, offset: u32, value: u32) {
        let at = (offset as usize) & 0xffc;
        self.dpram[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Queue bytes the host should send to the device's CDC OUT endpoint.
    /// `Rp2040::write_console` feeds this alongside UART0's receiver, so a
    /// firmware that reads either console sees the same keystrokes.
    pub fn push_console_input(&mut self, bytes: &[u8]) {
        self.input.extend(bytes.iter().copied());
    }

    /// Take the bytes the device sent on the console's bulk IN endpoint.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// The bulk endpoints the configuration descriptor advertises.
    pub fn cdc_endpoints(&self) -> Option<CdcEndpoints> {
        self.cdc
    }

    /// The device address the host set with `SET_ADDRESS`.
    pub fn device_address(&self) -> u8 {
        self.address
    }

    /// The device descriptor the device returned during enumeration.
    pub fn device_descriptor(&self) -> &[u8] {
        &self.device_desc
    }

    /// The configuration descriptor the device returned during enumeration.
    pub fn configuration_descriptor(&self) -> &[u8] {
        &self.config_desc
    }

    /// True while the host believes a device is on the bus.
    pub fn attached(&self) -> bool {
        self.attached
    }

    /// Advance the host side by `cycles` CPU cycles.
    pub fn tick(&mut self, cycles: u64) {
        if self.attached {
            self.sof_acc += cycles;
            while self.sof_acc >= SOF_CYCLES {
                self.sof_acc -= SOF_CYCLES;
                self.sof_count = (self.sof_count + 1) & 0x7ff;
                self.sof_pending = true;
            }
        } else {
            self.sof_acc = 0;
        }
        self.host_acc += cycles;
        while self.host_acc >= HOST_STEP_CYCLES {
            self.host_acc -= HOST_STEP_CYCLES;
            self.host_step();
        }
    }

    fn host_step(&mut self) {
        let enabled = self.regs[Self::reg_index(reg::MAIN_CTRL)] & main_ctrl::CONTROLLER_EN != 0;
        let pullup = self.regs[Self::reg_index(reg::SIE_CTRL)] & sie_ctrl::PULLUP_EN != 0;
        if enabled && pullup {
            self.detach_steps = 0;
            if !self.attached {
                self.attach();
            } else {
                self.drive_host();
            }
        } else if self.attached {
            self.detach_steps += 1;
            if self.detach_steps >= DETACH_STEPS {
                self.detach();
            }
        }
    }

    fn attach(&mut self) {
        self.attached = true;
        self.detach_steps = 0;
        self.reset_steps = 0;
        self.stage = Stage::WaitReset;
        self.control = None;
        self.address = 0;
        self.cdc = None;
        self.device_desc.clear();
        self.config_desc.clear();
        self.next_buf = [false; 16];
        self.buf_status = 0;
        self.should_handle = 0;
        self.sof_pending = false;
        self.sof_count = 0;
        // A bus reset invalidates every buffer the firmware had armed.
        for ep in 0..16 {
            self.write_dpram_word(dpram::ep_buf_ctrl(ep, false) as u32, 0);
            self.write_dpram_word(dpram::ep_buf_ctrl(ep, true) as u32, 0);
        }
        let status = Self::reg_index(reg::SIE_STATUS);
        self.regs[status] |=
            sie_status::CONNECTED | sie_status::VBUS_DETECTED | sie_status::BUS_RESET;
    }

    fn detach(&mut self) {
        self.attached = false;
        self.stage = Stage::Detached;
        self.control = None;
        self.cdc = None;
        self.detach_steps = 0;
        let status = Self::reg_index(reg::SIE_STATUS);
        self.regs[status] &= !sie_status::CONNECTED;
    }

    fn drive_host(&mut self) {
        if self.stage == Stage::WaitReset {
            let status = Self::reg_index(reg::SIE_STATUS);
            if self.regs[status] & sie_status::BUS_RESET == 0 {
                self.stage = Stage::GetDevice;
            } else {
                self.reset_steps += 1;
                if self.reset_steps >= RESET_TIMEOUT_STEPS {
                    self.regs[status] &= !sie_status::BUS_RESET;
                    self.stage = Stage::GetDevice;
                }
            }
            return;
        }

        if self.control.is_some() {
            self.step_control();
        } else {
            self.begin_request();
        }
        self.poll_bulk();
    }

    /// Start the next scripted control transfer, if any.
    fn begin_request(&mut self) {
        let (setup, payload, request) = match self.stage {
            Stage::GetDevice => (
                [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00],
                Vec::new(),
                Request::GetDevice,
            ),
            Stage::SetAddress => (
                [0x00, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
                Vec::new(),
                Request::SetAddress,
            ),
            Stage::GetConfig => (
                [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 0xff, 0x00],
                Vec::new(),
                Request::GetConfig,
            ),
            Stage::SetConfig => (
                [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
                Vec::new(),
                Request::SetConfig,
            ),
            Stage::SetLineCoding => (
                [0x21, 0x20, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00],
                // 115200 baud, no stop bits, no parity, eight data bits.
                vec![0x00, 0xc2, 0x01, 0x00, 0x00, 0x00, 0x08],
                Request::SetLineCoding,
            ),
            Stage::GetLineCoding => (
                [0xa1, 0x21, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00],
                Vec::new(),
                Request::GetLineCoding,
            ),
            Stage::SetControlLineState => (
                [0x21, 0x22, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00],
                Vec::new(),
                Request::SetControlLineState,
            ),
            Stage::WaitReset | Stage::Running | Stage::Detached => return,
        };
        self.control = Some(Control::new(setup, request, payload));
    }

    fn step_control(&mut self) {
        let Some(control) = self.control.as_ref() else {
            return;
        };
        let (step, dir_in, length) = (control.step, control.dir_in, control.length);

        // A STALL anywhere in the handshake ends the transfer without data.
        if step != Step::Deliver {
            let out = match step {
                Step::Data => !dir_in,
                Step::StatusOut => true,
                Step::StatusIn | Step::Deliver | Step::Arm => false,
            };
            let ctrl = self.read_dpram_word(dpram::ep_buf_ctrl(0, out) as u32);
            if ctrl & buf_ctrl::STALL != 0 {
                self.write_dpram_word(dpram::ep_buf_ctrl(0, out) as u32, 0);
                let nak = Self::reg_index(reg::EP_STATUS_STALL_NAK);
                self.regs[nak] |= ep_bit(0, out);
                if let Some(control) = self.control.as_mut() {
                    control.stalled = true;
                }
                self.finish_control();
                return;
            }
        }

        match step {
            Step::Deliver => {
                self.clear_control_buffers();
                let setup = self.control.as_ref().expect("control in flight").setup;
                self.dpram[dpram::SETUP..dpram::SETUP + 8].copy_from_slice(&setup);
                let status = Self::reg_index(reg::SIE_STATUS);
                self.regs[status] |= sie_status::SETUP_REC;
                if let Some(control) = self.control.as_mut() {
                    control.step = Step::Arm;
                }
            }
            // A status-stage buffer left over from the previous transfer can
            // arrive after this setup was written; waiting for the firmware to
            // acknowledge the packet keeps the two apart, and `Deliver` has
            // already dropped anything armed before it.
            Step::Arm => {
                let pending =
                    self.regs[Self::reg_index(reg::SIE_STATUS)] & sie_status::SETUP_REC != 0;
                let waited = self.control.as_ref().map_or(0, |c| c.arm_wait);
                if pending && waited < HANDSHAKE_TIMEOUT {
                    if let Some(control) = self.control.as_mut() {
                        control.arm_wait += 1;
                    }
                    return;
                }
                let next = if length == 0 {
                    Step::StatusIn
                } else {
                    Step::Data
                };
                if let Some(control) = self.control.as_mut() {
                    control.step = next;
                }
            }
            Step::Data => {
                if dir_in {
                    self.control_in_packet();
                } else {
                    self.control_out_packet();
                }
            }
            // A control-IN has no status OUT to arm, so the firmware usually
            // leaves EP0 OUT untouched; a control-OUT ends with a zero-length
            // IN the firmware does arm. Take it when it comes, and do not hold
            // the bus open if it never does.
            Step::StatusIn | Step::StatusOut => {
                let out = matches!(step, Step::StatusOut);
                let control = self.control.as_ref().expect("control in flight");
                if control.await_out_ack {
                    let ctrl = self.read_dpram_word(dpram::ep_buf_ctrl(0, true) as u32);
                    if ctrl & buf_ctrl::FULL != 0 {
                        return;
                    }
                }
                let ctrl = self.read_dpram_word(dpram::ep_buf_ctrl(0, out) as u32);
                if ctrl & buf_ctrl::AVAIL != 0 {
                    self.write_dpram_word(
                        dpram::ep_buf_ctrl(0, out) as u32,
                        ctrl & !(buf_ctrl::FULL | buf_ctrl::AVAIL),
                    );
                    self.buf_status |= ep_bit(0, out);
                    self.should_handle &= !ep_bit(0, out);
                    self.finish_control();
                    return;
                }
                let waited = self.control.as_ref().map_or(0, |c| c.status_wait);
                if waited < HANDSHAKE_TIMEOUT {
                    if let Some(control) = self.control.as_mut() {
                        control.status_wait += 1;
                    }
                    return;
                }
                self.finish_control();
            }
        }
    }

    /// Drop whatever the firmware had armed on EP0, in both directions.
    fn clear_control_buffers(&mut self) {
        self.write_dpram_word(dpram::ep_buf_ctrl(0, false) as u32, 0);
        self.write_dpram_word(dpram::ep_buf_ctrl(0, true) as u32, 0);
    }

    fn control_in_packet(&mut self) {
        let ctrl = self.read_dpram_word(dpram::ep_buf_ctrl(0, false) as u32);
        if ctrl & buf_ctrl::AVAIL == 0 {
            return;
        }
        let len = ((ctrl & buf_ctrl::LEN_MASK) as usize).min(EP0_MPS);
        // A zero-length packet while data is expected is the previous
        // transfer's status stage, not the answer to this request: drop it and
        // keep waiting for the data.
        let expecting_data = self.control.as_ref().is_some_and(|c| c.length > 0);
        if len == 0 && expecting_data {
            self.write_dpram_word(
                dpram::ep_buf_ctrl(0, false) as u32,
                ctrl & !(buf_ctrl::FULL | buf_ctrl::AVAIL),
            );
            return;
        }
        let packet = &self.dpram[dpram::EP0_BUF..dpram::EP0_BUF + len];
        if let Some(control) = self.control.as_mut() {
            control.received.extend_from_slice(packet);
        }
        self.write_dpram_word(
            dpram::ep_buf_ctrl(0, false) as u32,
            ctrl & !(buf_ctrl::FULL | buf_ctrl::AVAIL),
        );
        self.buf_status |= ep_bit(0, false);
        self.should_handle &= !ep_bit(0, false);

        let finished = ctrl & buf_ctrl::LAST != 0 || len < EP0_MPS;
        let Some(control) = self.control.as_mut() else {
            return;
        };
        if finished || control.received.len() >= control.length {
            control.step = if control.status_in {
                Step::StatusIn
            } else {
                Step::StatusOut
            };
        }
    }

    fn control_out_packet(&mut self) {
        let ctrl = self.read_dpram_word(dpram::ep_buf_ctrl(0, true) as u32);
        if ctrl & buf_ctrl::AVAIL == 0 {
            return;
        }
        let (len, done) = {
            let Some(control) = self.control.as_ref() else {
                return;
            };
            let remaining = control.out.len().saturating_sub(control.sent);
            let len = remaining.min(EP0_MPS);
            let start = control.sent;
            self.dpram[dpram::EP0_BUF..dpram::EP0_BUF + len]
                .copy_from_slice(&control.out[start..start + len]);
            (len, start + len >= control.out.len())
        };
        if let Some(control) = self.control.as_mut() {
            control.sent += len;
            if done {
                control.step = Step::StatusIn;
                control.await_out_ack = true;
            }
        }
        let value = (ctrl & !(buf_ctrl::LEN_MASK | buf_ctrl::AVAIL)) | len as u32 | buf_ctrl::FULL;
        self.write_dpram_word(dpram::ep_buf_ctrl(0, true) as u32, value);
        self.buf_status |= ep_bit(0, true);
        self.should_handle &= !ep_bit(0, true);
    }

    fn finish_control(&mut self) {
        let Some(control) = self.control.take() else {
            return;
        };
        if control.stalled {
            // The script still advances so enumeration does not wedge; the
            // descriptors simply stay empty.
            self.stage = match control.request {
                Request::GetDevice => Stage::SetAddress,
                Request::GetConfig => Stage::SetConfig,
                other => next_stage(other),
            };
            return;
        }
        match control.request {
            Request::GetDevice => {
                self.device_desc = control.received;
                self.stage = Stage::SetAddress;
            }
            Request::SetAddress => {
                self.address = control.setup[2];
                self.stage = Stage::GetConfig;
            }
            Request::GetConfig => {
                self.config_desc = control.received;
                self.parse_config();
                self.stage = Stage::SetConfig;
            }
            other => self.stage = next_stage(other),
        }
    }

    /// Find the first bulk IN and OUT endpoints in the configuration
    /// descriptor. The CDC notification endpoint is interrupt, so it is
    /// skipped; only the two data endpoints carry the console.
    fn parse_config(&mut self) {
        let mut bulk_in = None;
        let mut bulk_out = None;
        let descriptor = &self.config_desc;
        let mut at = 0;
        while at + 2 <= descriptor.len() {
            let len = usize::from(descriptor[at]);
            if len < 2 || at + len > descriptor.len() {
                break;
            }
            if descriptor[at + 1] == 0x05 && len >= 7 {
                let address = descriptor[at + 2];
                let attributes = descriptor[at + 3];
                let number = usize::from(address & 0x0f);
                if attributes & 0x03 == 0x02 {
                    if address & 0x80 != 0 {
                        bulk_in.get_or_insert(number);
                    } else {
                        bulk_out.get_or_insert(number);
                    }
                }
            }
            at += len;
        }
        self.cdc = match (bulk_in, bulk_out) {
            (Some(in_ep), Some(out_ep)) => Some(CdcEndpoints {
                in_ep: in_ep as u8,
                out_ep: out_ep as u8,
            }),
            _ => None,
        };
    }

    /// Service the CDC data endpoints once the firmware has enabled them.
    fn poll_bulk(&mut self) {
        let Some(cdc) = self.cdc else {
            return;
        };
        self.bulk_in(usize::from(cdc.in_ep));
        self.bulk_out(usize::from(cdc.out_ep));
    }

    fn bulk_in(&mut self, ep: usize) {
        let endpoint = self.read_dpram_word(dpram::ep_ctrl(ep, false) as u32);
        if endpoint & ep_ctrl::ENABLE == 0 {
            return;
        }
        let double = endpoint & ep_ctrl::DOUBLE_BUFFERED != 0;
        let buffer = (endpoint & ep_ctrl::BUFFER_ADDRESS_MASK) as usize & !0x3f;
        let control = self.read_dpram_word(dpram::ep_buf_ctrl(ep, false) as u32);
        let buffer_one = double && self.next_buf[ep];
        let half = if buffer_one {
            control >> 16
        } else {
            control & 0xffff
        };
        if half & (buf_ctrl::AVAIL | buf_ctrl::FULL) != buf_ctrl::AVAIL | buf_ctrl::FULL {
            return;
        }
        let len = (half & buf_ctrl::LEN_MASK) as usize;
        let start = buffer + if buffer_one { 64 } else { 0 };
        self.output
            .extend_from_slice(&self.dpram[start..start + len]);
        let cleared = half & !(buf_ctrl::FULL | buf_ctrl::AVAIL);
        let value = if buffer_one {
            (control & 0xffff) | (cleared << 16)
        } else {
            (control & 0xffff_0000) | cleared
        };
        self.write_dpram_word(dpram::ep_buf_ctrl(ep, false) as u32, value);
        self.buf_status |= ep_bit(ep, false);
        if buffer_one {
            self.should_handle |= ep_bit(ep, false);
        } else {
            self.should_handle &= !ep_bit(ep, false);
        }
        if double {
            self.next_buf[ep] = !buffer_one;
        }
    }

    fn bulk_out(&mut self, ep: usize) {
        if self.input.is_empty() {
            return;
        }
        let endpoint = self.read_dpram_word(dpram::ep_ctrl(ep, true) as u32);
        if endpoint & ep_ctrl::ENABLE == 0 {
            return;
        }
        let double = endpoint & ep_ctrl::DOUBLE_BUFFERED != 0;
        let buffer = (endpoint & ep_ctrl::BUFFER_ADDRESS_MASK) as usize & !0x3f;
        let control = self.read_dpram_word(dpram::ep_buf_ctrl(ep, true) as u32);
        let buffer_one = double && self.next_buf[ep];
        let half = if buffer_one {
            control >> 16
        } else {
            control & 0xffff
        };
        if half & buf_ctrl::AVAIL == 0 {
            return;
        }
        let capacity = (half & buf_ctrl::LEN_MASK) as usize;
        let len = self.input.len().min(capacity.min(BULK_MPS));
        let start = buffer + if buffer_one { 64 } else { 0 };
        for (index, byte) in self.input.drain(..len).enumerate() {
            self.dpram[start + index] = byte;
        }
        let value = (half & !(buf_ctrl::LEN_MASK | buf_ctrl::AVAIL)) | len as u32 | buf_ctrl::FULL;
        let value = if buffer_one {
            (control & 0xffff) | (value << 16)
        } else {
            (control & 0xffff_0000) | value
        };
        self.write_dpram_word(dpram::ep_buf_ctrl(ep, true) as u32, value);
        self.buf_status |= ep_bit(ep, true);
        if buffer_one {
            self.should_handle |= ep_bit(ep, true);
        } else {
            self.should_handle &= !ep_bit(ep, true);
        }
        if double {
            self.next_buf[ep] = !buffer_one;
        }
    }
}

fn next_stage(request: Request) -> Stage {
    match request {
        Request::SetConfig => Stage::SetLineCoding,
        Request::SetLineCoding => Stage::GetLineCoding,
        Request::GetLineCoding => Stage::SetControlLineState,
        Request::SetControlLineState => Stage::Running,
        // GET_DESCRIPTOR and SET_ADDRESS are handled explicitly.
        Request::GetDevice | Request::GetConfig | Request::SetAddress => Stage::Running,
    }
}

/// Bit position of endpoint `ep` in `BUFF_STATUS` and friends: 2n for IN,
/// 2n+1 for OUT.
fn ep_bit(ep: usize, out: bool) -> u32 {
    1 << ((ep << 1) | usize::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The descriptor the fake device returns for GET_DESCRIPTOR(DEVICE).
    const DEVICE: [u8; 18] = [
        0x12, 0x01, 0x00, 0x02, 0xef, 0x02, 0x01, 0x40, 0x8a, 0x2e, 0x05, 0x00, 0x00, 0x01, 0x01,
        0x02, 0x03, 0x01,
    ];

    /// An IAD CDC-ACM configuration with the data endpoints TinyUSB uses on
    /// rp2: 0x82 IN and 0x02 OUT, both bulk 64.
    const CONFIG: [u8; 56] = [
        0x09, 0x02, 0x38, 0x00, 0x02, 0x01, 0x00, 0x80, 0x32, // configuration
        0x08, 0x0b, 0x00, 0x02, 0x02, 0x02, 0x00, 0x00, // IAD
        0x09, 0x04, 0x00, 0x00, 0x01, 0x02, 0x02, 0x00, 0x00, // CDC control
        0x07, 0x05, 0x81, 0x03, 0x08, 0x00, 0x10, // notification IN
        0x09, 0x04, 0x01, 0x00, 0x02, 0x0a, 0x00, 0x00, 0x00, // CDC data
        0x07, 0x05, 0x02, 0x02, 0x40, 0x00, 0x00, // bulk OUT
        0x07, 0x05, 0x82, 0x02, 0x40, 0x00, 0x00, // bulk IN
    ];

    fn powered() -> Usb {
        let mut usb = Usb::new();
        usb.write(reg::MAIN_CTRL, main_ctrl::CONTROLLER_EN);
        usb.write(reg::SIE_CTRL, sie_ctrl::PULLUP_EN);
        usb
    }

    /// What the other end of the cable would do with each SETUP packet.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase {
        Idle,
        In,
        StatusIn,
        StatusOut,
    }

    struct Responder {
        phase: Phase,
        data: Vec<u8>,
        offset: usize,
        status_armed: bool,
    }

    impl Responder {
        fn new() -> Self {
            Responder {
                phase: Phase::Idle,
                data: Vec::new(),
                offset: 0,
                status_armed: false,
            }
        }

        fn ep0_in(&self, usb: &Usb) -> u32 {
            usb.read_dpram_word(dpram::ep_buf_ctrl(0, false) as u32)
        }

        fn ep0_out(&self, usb: &Usb) -> u32 {
            usb.read_dpram_word(dpram::ep_buf_ctrl(0, true) as u32)
        }

        /// One pass of the device firmware: consume interrupts, answer SETUP,
        /// and arm whichever EP0 buffer the host is waiting for.
        fn service(&mut self, usb: &mut Usb) {
            // The ISR would clear the events it handled.
            usb.buf_status = 0;
            usb.should_handle = 0;

            // The status flags are write-one-to-clear, so the ack writes the
            // bit itself rather than the value it wants to keep.
            let status = usb.read(reg::SIE_STATUS);
            if status & sie_status::BUS_RESET != 0 {
                usb.write(reg::SIE_STATUS, sie_status::BUS_RESET);
            }
            if status & sie_status::SETUP_REC != 0 {
                let mut setup = [0u8; 8];
                setup.copy_from_slice(&usb.dpram[dpram::SETUP..dpram::SETUP + 8]);
                usb.write(reg::SIE_STATUS, sie_status::SETUP_REC);
                self.offset = 0;
                self.status_armed = false;
                // wValue's high byte is the descriptor type for
                // GET_DESCRIPTOR: 1 is DEVICE, 2 is CONFIGURATION.
                let value = u16::from_le_bytes([setup[2], setup[3]]);
                if setup[0] & 0x80 != 0 && setup[1] == 0x06 {
                    self.data = match value >> 8 {
                        0x01 => DEVICE.to_vec(),
                        0x02 => CONFIG.to_vec(),
                        _ => Vec::new(),
                    };
                    self.phase = Phase::In;
                } else {
                    self.data.clear();
                    self.phase = Phase::StatusIn;
                }
            }

            match self.phase {
                Phase::Idle => {}
                Phase::In => {
                    if self.ep0_in(usb) & buf_ctrl::AVAIL == 0 {
                        if self.offset < self.data.len() {
                            let len = (self.data.len() - self.offset).min(EP0_MPS);
                            let start = self.offset;
                            usb.dpram[dpram::EP0_BUF..dpram::EP0_BUF + len]
                                .copy_from_slice(&self.data[start..start + len]);
                            let last = start + len == self.data.len();
                            let mut value = len as u32 | buf_ctrl::FULL | buf_ctrl::AVAIL;
                            if last {
                                value |= buf_ctrl::LAST;
                            }
                            usb.write_dpram_word(dpram::ep_buf_ctrl(0, false) as u32, value);
                            self.offset += len;
                        } else {
                            self.phase = Phase::StatusOut;
                        }
                    }
                }
                Phase::StatusIn => {
                    if self.ep0_in(usb) & buf_ctrl::AVAIL == 0 {
                        if !self.status_armed {
                            usb.write_dpram_word(
                                dpram::ep_buf_ctrl(0, false) as u32,
                                buf_ctrl::FULL | buf_ctrl::AVAIL | buf_ctrl::LAST,
                            );
                            self.status_armed = true;
                        } else {
                            self.phase = Phase::Idle;
                            self.status_armed = false;
                        }
                    }
                }
                Phase::StatusOut => {
                    if self.ep0_out(usb) & buf_ctrl::AVAIL == 0 {
                        if !self.status_armed {
                            usb.write_dpram_word(
                                dpram::ep_buf_ctrl(0, true) as u32,
                                EP0_MPS as u32 | buf_ctrl::AVAIL,
                            );
                            self.status_armed = true;
                        } else {
                            self.phase = Phase::Idle;
                            self.status_armed = false;
                        }
                    }
                }
            }
        }
    }

    /// Run the host and the fake firmware together for `steps` host actions.
    fn pump(usb: &mut Usb, responder: &mut Responder, steps: usize) {
        for _ in 0..steps {
            usb.tick(HOST_STEP_CYCLES);
            responder.service(usb);
        }
    }

    #[test]
    fn pullup_starts_enumeration_and_reads_the_device_descriptor() {
        let mut usb = powered();
        let mut responder = Responder::new();
        pump(&mut usb, &mut responder, 200);

        assert!(usb.attached(), "the host follows PULLUP_EN");
        assert_eq!(
            usb.device_descriptor(),
            &DEVICE,
            "GET_DESCRIPTOR(DEVICE) returned the guest's descriptor"
        );
    }

    #[test]
    fn enumeration_sets_the_address_and_reads_the_configuration() {
        let mut usb = powered();
        let mut responder = Responder::new();
        pump(&mut usb, &mut responder, 400);

        assert_eq!(usb.device_address(), 1, "SET_ADDRESS reached the device");
        assert_eq!(
            usb.configuration_descriptor(),
            &CONFIG,
            "GET_DESCRIPTOR(CONFIGURATION) returned the guest's descriptor"
        );
        assert_eq!(
            usb.cdc_endpoints(),
            Some(CdcEndpoints {
                in_ep: 2,
                out_ep: 2
            }),
            "the bulk endpoints come from the descriptor, not a constant"
        );
    }

    #[test]
    fn the_host_empties_a_bulk_in_buffer_and_clears_full() {
        let mut usb = Usb::new();
        let buffer = 0x180;
        usb.write_dpram_word(
            dpram::ep_ctrl(2, false) as u32,
            ep_ctrl::ENABLE | (buffer as u32) | (2 << ep_ctrl::TYPE_LSB),
        );
        for (index, byte) in b"hello".iter().enumerate() {
            usb.dpram[buffer + index] = *byte;
        }
        usb.write_dpram_word(
            dpram::ep_buf_ctrl(2, false) as u32,
            5 | buf_ctrl::LAST | buf_ctrl::FULL | buf_ctrl::AVAIL,
        );

        usb.bulk_in(2);

        assert_eq!(usb.take_output(), b"hello");
        let control = usb.read_dpram_word(dpram::ep_buf_ctrl(2, false) as u32);
        assert_eq!(
            control & (buf_ctrl::FULL | buf_ctrl::AVAIL),
            0,
            "the controller clears ownership when it sends the packet"
        );
        assert_eq!(control & buf_ctrl::LEN_MASK, 5);
        assert_ne!(usb.buf_status & ep_bit(2, false), 0);
    }

    #[test]
    fn the_host_delivers_console_bytes_to_a_bulk_out_buffer() {
        // This is the path `Rp2040::write_console` feeds: the same bytes also
        // go to UART0's receiver, so a firmware reading either console sees
        // them. On the USB side they appear in the CDC OUT endpoint.
        let mut usb = Usb::new();
        let buffer = 0x1c0;
        usb.write_dpram_word(
            dpram::ep_ctrl(2, true) as u32,
            ep_ctrl::ENABLE | (buffer as u32) | (2 << ep_ctrl::TYPE_LSB),
        );
        usb.write_dpram_word(
            dpram::ep_buf_ctrl(2, true) as u32,
            BULK_MPS as u32 | buf_ctrl::AVAIL,
        );
        usb.push_console_input(b"x");

        usb.bulk_out(2);

        assert_eq!(usb.dpram[buffer], b'x');
        let control = usb.read_dpram_word(dpram::ep_buf_ctrl(2, true) as u32);
        assert_ne!(control & buf_ctrl::FULL, 0, "the controller filled it");
        assert_eq!(control & buf_ctrl::AVAIL, 0, "the buffer is the CPU's");
        assert_eq!(control & buf_ctrl::LEN_MASK, 1);
        assert_ne!(usb.buf_status & ep_bit(2, true), 0);
        assert!(usb.input.is_empty());
    }
}
