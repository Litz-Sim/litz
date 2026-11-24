//! A minimal MCU simulator for `{{id}}`.
//!
//! This is a toy register machine, not a CPU: 4 KiB of RAM, a 64-byte program
//! in flash, and one byte-instruction per cycle. It is a starting point, so
//! replace [`Machine`] with your part's peripherals and leave the glue at the
//! bottom of the file alone.
//!
//! `wit-bindgen` resolves `path` against this crate's `Cargo.toml`, so `../wit`
//! is the copy of the `litz:mcu@0.2.0` world that `litz catalog new board`
//! placed next to the manifest.

mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "simulator",
    });
}

use bindings::exports::litz::mcu::chip::Guest;
use bindings::exports::litz::mcu::debug::Guest as DebugGuest;
use bindings::litz::mcu::types::{
    ChipInfo, FlashLayout, Image, Level, MemoryRegion, PinInput, PinOutput, Register, RunResult,
    Stop,
};
use std::cell::RefCell;
use std::collections::BTreeSet;

/// The only readable/executable memory region: 4 KiB of RAM.
const MEMORY_BASE: u32 = 0x0000_0000;
const MEMORY_LEN: usize = 4096;

/// A debug window exposing the levels the host holds on each pin, one byte
/// per pad (0 = floating, 1 = low, 2 = high).
const INPUT_BASE: u32 = 0x0000_1000;

/// A debug window over the program, so `debug.read` can show the instructions
/// the pc runs. The core fetches from it directly.
const FLASH_BASE: u32 = 0x0000_2000;

/// The program window. A real chip has megabytes of flash; 64 bytes is enough
/// for the instruction set in this file.
const FLASH_LEN: usize = 64;

/// The rate the host counts `run-result.cycles` at.
const CPU_HZ: u64 = 16_000_000;

/// Opcodes. Operands follow their opcode and are not charged as cycles.
const OP_HALT: u8 = 0; // stop the core
const OP_EMIT: u8 = 1; // emit the next byte to the console
const OP_PIN: u8 = 2; // drive (pin, high) from two operands
const OP_LOOP: u8 = 3; // jump back to address 0
const OP_HOST: u8 = 4; // a host call, named by the next byte

thread_local! {
    /// One machine per component instance. A component has one thread, so
    /// `thread_local` gives a host running two chips its own state without a
    /// `static mut` (this crate uses no unsafe).
    static MACHINE: RefCell<Machine> = RefCell::new(Machine::new());
}

struct Machine {
    ram: [u8; MEMORY_LEN],
    flash: [u8; FLASH_LEN],
    pc: usize,
    halted: bool,
    /// Bytes the core wants the host to print.
    console: Vec<u8>,
    /// Host-held pin levels, indexed by pin number.
    inputs: [u8; 256],
    /// Drives that changed since the last `take-outputs`, as `(pin, high)`.
    outputs: Vec<(u8, bool)>,
    /// The level currently driven on each pad, so only changes are reported.
    drive: [Option<bool>; 256],
    /// Addresses `debug.set-breakpoints` armed.
    breakpoints: BTreeSet<u32>,
}

impl Machine {
    fn new() -> Self {
        Self {
            ram: [0; MEMORY_LEN],
            flash: demo_program(),
            pc: 0,
            halted: false,
            console: Vec::new(),
            inputs: [0; 256],
            outputs: Vec::new(),
            drive: [None; 256],
            breakpoints: BTreeSet::new(),
        }
    }

    /// Reset the core. Flash survives, the pads stop driving, and RAM is
    /// cleared the way a real reset would leave it.
    fn reset(&mut self) {
        self.ram.fill(0);
        self.pc = 0;
        self.halted = false;
        self.console.clear();
        self.outputs.clear();
        self.drive = [None; 256];
    }

    /// Replace the image. This scaffold has one 64-byte window, so `offset`
    /// is ignored; a real chip would place the bytes at `offset` in flash.
    fn load(&mut self, bytes: &[u8]) {
        self.flash.fill(0);
        let len = bytes.len().min(FLASH_LEN);
        self.flash[..len].copy_from_slice(&bytes[..len]);
        self.reset();
    }

    fn boot(&mut self) -> Result<u32, String> {
        if self.flash.iter().all(|&byte| byte == 0) {
            return Err("no program in flash: load an image before boot".into());
        }
        self.pc = 0;
        self.halted = false;
        Ok(0)
    }

    fn set_input(&mut self, pin: u8, level: Level) {
        self.inputs[pin as usize] = match level {
            Level::Low => 1,
            Level::High => 2,
            Level::Floating => 0,
        };
    }

    /// Execute up to `budget` instructions, one per cycle, and report them at
    /// [`CPU_HZ`]. An infinite firmware loop therefore still returns. An armed
    /// breakpoint at the current pc stops before the instruction runs.
    fn run(&mut self, budget: u64) -> RunResult {
        let mut cycles = 0u64;
        while cycles < budget && !self.halted {
            let pc = self.pc as u32;
            if self.breakpoints.contains(&pc) {
                return RunResult {
                    cycles,
                    stop: Stop::Breakpoint(pc),
                };
            }
            if !self.execute() {
                break;
            }
            cycles += 1;
        }
        RunResult {
            cycles,
            stop: if self.halted { Stop::Halted } else { Stop::Budget },
        }
    }

    /// Execute exactly one instruction, ignoring a breakpoint at the current
    /// pc so the debugger can step off it.
    fn step(&mut self) -> RunResult {
        if self.halted {
            return RunResult {
                cycles: 0,
                stop: Stop::Halted,
            };
        }
        let executed = self.execute();
        RunResult {
            cycles: u64::from(executed),
            stop: if self.halted { Stop::Halted } else { Stop::Budget },
        }
    }

    /// Run one instruction. False when there was nothing left to execute.
    fn execute(&mut self) -> bool {
        let Some(&op) = self.flash.get(self.pc) else {
            self.halted = true;
            return false;
        };
        self.pc += 1;
        match op {
            OP_HALT => self.halted = true,
            OP_EMIT => match self.flash.get(self.pc).copied() {
                Some(byte) => {
                    self.pc += 1;
                    self.console.push(byte);
                }
                None => self.halted = true,
            },
            OP_PIN => {
                let pin = self.flash.get(self.pc).copied();
                let level = self.flash.get(self.pc + 1).copied();
                self.pc += 2;
                match (pin, level) {
                    (Some(pin), Some(level)) => {
                        let high = level != 0;
                        if self.drive[pin as usize] != Some(high) {
                            self.drive[pin as usize] = Some(high);
                            self.outputs.push((pin, high));
                        }
                    }
                    _ => self.halted = true,
                }
            }
            OP_LOOP => self.pc = 0,
            OP_HOST => self.host_call(),
            _ => self.halted = true,
        }
        true
    }

    /// The next program byte, or none at the end of the image.
    fn next(&mut self) -> Option<u8> {
        let byte = self.flash.get(self.pc).copied()?;
        self.pc += 1;
        Some(byte)
    }

    fn line(&mut self, text: &str) {
        self.console.extend_from_slice(text.as_bytes());
        self.console.push(b'\n');
    }

    /// One `litz:mcu@0.2.0` host call. This is the half a custom chip exists
    /// for: the guest runs a program, and the program reaches the sensors,
    /// the buses and the radio through the host.
    fn host_call(&mut self) {
        use bindings::litz::mcu::host;
        match self.next() {
            // Scan for access points.
            Some(0) => {
                let networks = host::wifi_scan();
                if networks.is_empty() {
                    self.line("wifi: none");
                }
                for network in networks {
                    let lock = if network.secure { " secure" } else { "" };
                    let joined = if network.joined { " joined" } else { "" };
                    self.line(&format!(
                        "wifi {} ch{} {}dBm{lock}{joined}",
                        network.ssid, network.channel, network.rssi
                    ));
                }
            }
            // Read one I2C register: controller, address, register.
            Some(1) => {
                let (Some(controller), Some(address), Some(register)) =
                    (self.next(), self.next(), self.next())
                else {
                    self.halted = true;
                    return;
                };
                match host::i2c_transfer(controller, address, &[register], 1) {
                    Ok(bytes) => {
                        let value = bytes.first().copied().unwrap_or(0xff);
                        self.line(&format!("i2c {address:02x} {value:02x}"));
                    }
                    Err(error) => self.line(&format!("i2c error {error}")),
                }
            }
            // Every BLE advertiser in range.
            Some(2) => {
                let advertisers = host::ble_scan();
                if advertisers.is_empty() {
                    self.line("ble: none");
                }
                for advertiser in advertisers {
                    let address = advertiser
                        .address
                        .iter()
                        .rev()
                        .map(|octet| format!("{octet:02x}"))
                        .collect::<Vec<_>>()
                        .join(":");
                    let name = advertiser.name.unwrap_or_default();
                    self.line(&format!("ble {address} {name} {}", advertiser.rssi));
                }
            }
            // The solved voltage on one pad, in millivolts.
            Some(3) => {
                let Some(pin) = self.next() else {
                    self.halted = true;
                    return;
                };
                match host::analog_read(pin) {
                    Some(millivolts) => self.line(&format!("adc {pin} {millivolts}mV")),
                    None => self.line(&format!("adc {pin} none")),
                }
            }
            // Write bytes to a UART port: port, length, bytes.
            Some(4) => {
                let (Some(port), Some(len)) = (self.next(), self.next()) else {
                    self.halted = true;
                    return;
                };
                let mut bytes = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => bytes.push(byte),
                        None => break,
                    }
                }
                let wired = host::uart_write(port, &bytes);
                self.line(&format!("uart {port} {}", if wired { "ok" } else { "none" }));
            }
            // Join the network an access point offers: length, ssid.
            Some(5) => {
                let Some(len) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut ssid = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => ssid.push(byte),
                        None => break,
                    }
                }
                let ssid = String::from_utf8_lossy(&ssid).into_owned();
                match host::wifi_join(&ssid, None) {
                    Ok(lease) => self.line(&format!(
                        "joined {ssid} at {}",
                        lease
                            .ip
                            .iter()
                            .map(u8::to_string)
                            .collect::<Vec<_>>()
                            .join(".")
                    )),
                    Err(error) => self.line(&format!("join error {error}")),
                }
            }
            // Connect to the router's echo service: port (two bytes), length,
            // payload. Prints whatever comes back.
            Some(6) => {
                let (Some(port_hi), Some(port_lo), Some(len)) =
                    (self.next(), self.next(), self.next())
                else {
                    self.halted = true;
                    return;
                };
                let mut payload = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => payload.push(byte),
                        None => break,
                    }
                }
                let port = u16::from_be_bytes([port_hi, port_lo]);
                match host::tcp_connect(&[10, 0, 2, 2], port) {
                    Ok(id) => {
                        let _ = host::tcp_write(id, &payload);
                        let reply = host::tcp_read(id, 64).unwrap_or_default();
                        let text = String::from_utf8_lossy(&reply).into_owned();
                        self.line(&format!("tcp {port} {text}"));
                        host::tcp_close(id);
                    }
                    Err(error) => self.line(&format!("tcp error {error}")),
                }
            }
            // Advertise a payload: data length, data, scan-response length,
            // scan response. The host derives the address.
            Some(7) => {
                let Some(len) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut data = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => data.push(byte),
                        None => break,
                    }
                }
                let Some(response_len) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut response = Vec::new();
                for _ in 0..response_len {
                    match self.next() {
                        Some(byte) => response.push(byte),
                        None => break,
                    }
                }
                match host::ble_advertise(&[], &data, &response) {
                    Ok(address) => {
                        let text = address
                            .iter()
                            .rev()
                            .map(|octet| format!("{octet:02x}"))
                            .collect::<Vec<_>>()
                            .join(":");
                        self.line(&format!("ble adv {text}"));
                    }
                    Err(error) => self.line(&format!("ble adv error {error}")),
                }
            }
            // Stop advertising on an address: length, six octets.
            Some(8) => {
                let Some(len) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut address = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => address.push(byte),
                        None => break,
                    }
                }
                host::ble_stop_advertising(&address);
                self.line("ble stop");
            }
            // Register the GATT table this station serves. Program bytes:
            // service count, then per service: uuid hi/lo, characteristic
            // count, then per characteristic: uuid hi/lo, access, value
            // length, value bytes.
            Some(9) => {
                let Some(count) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut services = Vec::new();
                for _ in 0..count {
                    let (Some(hi), Some(lo), Some(chars)) =
                        (self.next(), self.next(), self.next())
                    else {
                        self.halted = true;
                        return;
                    };
                    let mut characteristics = Vec::new();
                    for _ in 0..chars {
                        let (Some(chi), Some(clo), Some(access), Some(len)) =
                            (self.next(), self.next(), self.next(), self.next())
                        else {
                            self.halted = true;
                            return;
                        };
                        let mut value = Vec::new();
                        for _ in 0..len {
                            match self.next() {
                                Some(byte) => value.push(byte),
                                None => break,
                            }
                        }
                        characteristics.push(bindings::litz::mcu::types::GattCharacteristic {
                            uuid: u16::from_be_bytes([chi, clo]),
                            access,
                            value,
                        });
                    }
                    services.push(bindings::litz::mcu::types::GattService {
                        uuid: u16::from_be_bytes([hi, lo]),
                        characteristics,
                    });
                }
                match host::ble_set_gatt(&services) {
                    Ok(()) => self.line(&format!("gatt {count}")),
                    Err(error) => self.line(&format!("gatt error {error}")),
                }
            }
            // Connect: address length, address. Zero means the first
            // advertiser a scan hears.
            Some(10) => {
                let Some(len) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut address = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => address.push(byte),
                        None => break,
                    }
                }
                if address.is_empty() {
                    if let Some(beacon) = host::ble_scan().into_iter().next() {
                        address = beacon.address;
                    }
                }
                match host::ble_connect(&address) {
                    Ok(handle) => self.line(&format!("link {handle}")),
                    Err(error) => self.line(&format!("link error {error}")),
                }
            }
            // Discover one connection's services: handle.
            Some(11) => {
                let Some(handle) = self.next() else {
                    self.halted = true;
                    return;
                };
                match host::ble_discover(u16::from(handle)) {
                    Ok(services) => {
                        for service in services {
                            let chars = service
                                .characteristics
                                .iter()
                                .map(|characteristic| format!("{:04x}", characteristic.uuid))
                                .collect::<Vec<_>>()
                                .join(",");
                            self.line(&format!("svc {:04x} {chars}", service.uuid));
                        }
                    }
                    Err(error) => self.line(&format!("discover error {error}")),
                }
            }
            // Read one characteristic: handle, service hi/lo, characteristic
            // hi/lo. Prints the bytes as hex.
            Some(12) => {
                let (Some(handle), Some(shi), Some(slo), Some(chi), Some(clo)) = (
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                ) else {
                    self.halted = true;
                    return;
                };
                let service = u16::from_be_bytes([shi, slo]);
                let characteristic = u16::from_be_bytes([chi, clo]);
                match host::ble_read(u16::from(handle), service, characteristic) {
                    Ok(value) => {
                        let hex: String =
                            value.iter().map(|byte| format!("{byte:02x}")).collect();
                        self.line(&format!("read {service:04x} {characteristic:04x} {hex}"));
                    }
                    Err(error) => self.line(&format!("read error {error}")),
                }
            }
            // Write one characteristic: handle, service hi/lo, characteristic
            // hi/lo, length, bytes.
            Some(13) => {
                let (Some(handle), Some(shi), Some(slo), Some(chi), Some(clo), Some(len)) = (
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                ) else {
                    self.halted = true;
                    return;
                };
                let mut value = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => value.push(byte),
                        None => break,
                    }
                }
                let service = u16::from_be_bytes([shi, slo]);
                let characteristic = u16::from_be_bytes([chi, clo]);
                match host::ble_write(u16::from(handle), service, characteristic, &value) {
                    Ok(()) => self.line(&format!("write {service:04x} {characteristic:04x}")),
                    Err(error) => self.line(&format!("write error {error}")),
                }
            }
            // Subscribe: handle, service hi/lo, characteristic hi/lo, enabled.
            Some(14) => {
                let (Some(handle), Some(shi), Some(slo), Some(chi), Some(clo), Some(enabled)) = (
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                ) else {
                    self.halted = true;
                    return;
                };
                let service = u16::from_be_bytes([shi, slo]);
                let characteristic = u16::from_be_bytes([chi, clo]);
                match host::ble_subscribe(
                    u16::from(handle),
                    service,
                    characteristic,
                    enabled != 0,
                ) {
                    Ok(()) => self.line(&format!("sub {service:04x} {characteristic:04x}")),
                    Err(error) => self.line(&format!("sub error {error}")),
                }
            }
            // Notifications received on a connection: handle.
            Some(15) => {
                let Some(handle) = self.next() else {
                    self.halted = true;
                    return;
                };
                for notification in host::ble_notifications(u16::from(handle)) {
                    let hex: String = notification
                        .value
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    self.line(&format!(
                        "notify {:04x} {:04x} {hex}",
                        notification.service, notification.characteristic
                    ));
                }
            }
            // Writes centrals made to this station's table.
            Some(16) => {
                for write in host::ble_take_writes() {
                    let hex: String =
                        write.value.iter().map(|byte| format!("{byte:02x}")).collect();
                    self.line(&format!(
                        "wrote {:04x} {:04x} {hex}",
                        write.service, write.characteristic
                    ));
                }
            }
            // Notify subscribers: service hi/lo, characteristic hi/lo,
            // length, bytes.
            Some(17) => {
                let (Some(shi), Some(slo), Some(chi), Some(clo), Some(len)) = (
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                    self.next(),
                ) else {
                    self.halted = true;
                    return;
                };
                let mut value = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => value.push(byte),
                        None => break,
                    }
                }
                let service = u16::from_be_bytes([shi, slo]);
                let characteristic = u16::from_be_bytes([chi, clo]);
                match host::ble_notify(service, characteristic, &value) {
                    Ok(()) => self.line(&format!("notified {service:04x} {characteristic:04x}")),
                    Err(error) => self.line(&format!("notify error {error}")),
                }
            }
            // Disconnect: handle.
            Some(18) => {
                let Some(handle) = self.next() else {
                    self.halted = true;
                    return;
                };
                host::ble_disconnect(u16::from(handle));
                self.line(&format!("closed {handle}"));
            }
            // Join the shared packet medium: network length, network,
            // channel hi/lo, address length, address.
            Some(19) => {
                let Some(len) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut network = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => network.push(byte),
                        None => break,
                    }
                }
                let (Some(hi), Some(lo), Some(address_len)) =
                    (self.next(), self.next(), self.next())
                else {
                    self.halted = true;
                    return;
                };
                let mut address = Vec::new();
                for _ in 0..address_len {
                    match self.next() {
                        Some(byte) => address.push(byte),
                        None => break,
                    }
                }
                let network = String::from_utf8_lossy(&network).into_owned();
                let channel = u16::from_be_bytes([hi, lo]);
                match host::radio_configure(&network, channel, &address) {
                    Ok(()) => self.line(&format!("radio {network} ch{channel}")),
                    Err(error) => self.line(&format!("radio error {error}")),
                }
            }
            // Send one raw frame: length, bytes, address length, address
            // (zero broadcasts), airtime (four bytes, nanoseconds).
            Some(20) => {
                let Some(len) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut bytes = Vec::new();
                for _ in 0..len {
                    match self.next() {
                        Some(byte) => bytes.push(byte),
                        None => break,
                    }
                }
                let Some(address_len) = self.next() else {
                    self.halted = true;
                    return;
                };
                let mut address = Vec::new();
                for _ in 0..address_len {
                    match self.next() {
                        Some(byte) => address.push(byte),
                        None => break,
                    }
                }
                let (Some(hi), Some(a), Some(b), Some(lo)) =
                    (self.next(), self.next(), self.next(), self.next())
                else {
                    self.halted = true;
                    return;
                };
                let airtime = u32::from_be_bytes([hi, a, b, lo]);
                let to = if address.is_empty() {
                    None
                } else {
                    Some(address.as_slice())
                };
                match host::radio_send(&bytes, to, u64::from(airtime)) {
                    Ok(()) => self.line(&format!("radio tx {}", bytes.len())),
                    Err(error) => self.line(&format!("radio tx error {error}")),
                }
            }
            // Frames addressed to this station that have finished.
            Some(21) => {
                for frame in host::radio_receive() {
                    let from = frame
                        .sender
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<Vec<_>>()
                        .join("");
                    let hex: String =
                        frame.bytes.iter().map(|byte| format!("{byte:02x}")).collect();
                    self.line(&format!("radio rx {from} {hex}"));
                }
            }
            _ => self.halted = true,
        }
    }

    fn take_outputs(&mut self) -> Vec<PinOutput> {
        self.outputs
            .drain(..)
            .map(|(pin, high)| PinOutput {
                pin,
                level: if high { Level::High } else { Level::Low },
                enabled: true,
            })
            .collect()
    }

    /// Debug read. RAM, the pin window and the program window are mapped;
    /// anything else is an error, not zeroes.
    fn read(&self, addr: u32, len: u32) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(len as usize);
        for offset in 0..len {
            let at = addr.wrapping_add(offset);
            let byte = if at >= MEMORY_BASE && (at - MEMORY_BASE) < MEMORY_LEN as u32 {
                self.ram[(at - MEMORY_BASE) as usize]
            } else if at >= INPUT_BASE && (at - INPUT_BASE) < self.inputs.len() as u32 {
                self.inputs[(at - INPUT_BASE) as usize]
            } else if at >= FLASH_BASE && (at - FLASH_BASE) < FLASH_LEN as u32 {
                self.flash[(at - FLASH_BASE) as usize]
            } else {
                return Err(format!("{at:#010x}: unmapped address"));
            };
            out.push(byte);
        }
        Ok(out)
    }

    /// Disassemble `count` instructions of this toy ISA. Operands are part of
    /// the instruction, so the walk follows them rather than assuming a fixed
    /// width.
    fn disassemble(&self, addr: u32, count: u32) -> String {
        let mut pc = addr as usize;
        let mut out = String::new();
        for _ in 0..count {
            let Some(&op) = self.flash.get(pc) else {
                break;
            };
            let here = pc;
            pc += 1;
            let text = match op {
                OP_HALT => "halt".to_string(),
                OP_EMIT => match self.flash.get(pc) {
                    Some(&byte) => {
                        pc += 1;
                        format!("emit {:#04x}", byte)
                    }
                    None => "emit <truncated>".to_string(),
                },
                OP_PIN => match (self.flash.get(pc), self.flash.get(pc + 1)) {
                    (Some(&pin), Some(&level)) => {
                        pc += 2;
                        format!("pin {pin} {}", if level != 0 { "high" } else { "low" })
                    }
                    _ => "pin <truncated>".to_string(),
                },
                OP_LOOP => "loop".to_string(),
                OP_HOST => match self.flash.get(pc) {
                    Some(&which) => {
                        pc += 1;
                        format!("host {which}")
                    }
                    None => "host <truncated>".to_string(),
                },
                other => format!(".byte {other:#04x}"),
            };
            out.push_str(&format!("{here:#06x}: {text}\n"));
        }
        out
    }

    /// Replace the armed breakpoint set.
    fn set_breakpoints(&mut self, pcs: &[u32]) {
        self.breakpoints = pcs.iter().copied().collect();
    }
}

/// A program that prints "litz\n" and loops forever. Loading an image
/// replaces it; delete this and the program window to start from nothing.
fn demo_program() -> [u8; FLASH_LEN] {
    let mut flash = [0u8; FLASH_LEN];
    let mut at = 0;
    for &byte in b"litz\n" {
        flash[at] = OP_EMIT;
        flash[at + 1] = byte;
        at += 2;
    }
    // Printing once and stopping is friendlier than a loop that floods the
    // console before the author has written a program at all.
    flash[at] = OP_HALT;
    flash
}

struct Chip;

fn with_machine<T>(f: impl FnOnce(&mut Machine) -> T) -> T {
    MACHINE.with(|machine| f(&mut machine.borrow_mut()))
}

fn flush_console(machine: &mut Machine) {
    if !machine.console.is_empty() {
        let bytes = std::mem::take(&mut machine.console);
        bindings::litz::mcu::host::console(&bytes);
    }
}

impl Guest for Chip {
    fn info() -> ChipInfo {
        ChipInfo {
            name: "{{id}}".into(),
            description: "A minimal register-machine MCU for {{id}}".into(),
            cpu_hz: CPU_HZ,
            memory: vec![
                MemoryRegion {
                    name: "ram".into(),
                    start: MEMORY_BASE,
                    len: MEMORY_LEN as u32,
                },
                MemoryRegion {
                    name: "flash".into(),
                    start: FLASH_BASE,
                    len: FLASH_LEN as u32,
                },
            ],
            flash: FlashLayout {
                size: 4 << 20,
                bootloader: 0,
                partition_table: 0,
                app: 0,
            },
        }
    }

    fn load_image(kind: Image, _offset: u64, bytes: Vec<u8>) -> Result<(), String> {
        // Only the application image means anything to this core; ROM and
        // partition tables are accepted and ignored so a manifest written for
        // a real board still loads.
        match kind {
            Image::Firmware => {
                with_machine(|machine| machine.load(&bytes));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn reset(_mac: Vec<u8>) {
        with_machine(|machine| {
            flush_console(machine);
            machine.reset();
        });
    }

    fn boot() -> Result<u32, String> {
        with_machine(|machine| machine.boot())
    }

    fn set_inputs(inputs: Vec<PinInput>) {
        with_machine(|machine| {
            for input in inputs {
                machine.set_input(input.pin, input.level);
            }
        });
    }

    fn run(budget_cycles: u64) -> RunResult {
        with_machine(|machine| {
            let result = machine.run(budget_cycles);
            flush_console(machine);
            result
        })
    }

    fn take_outputs() -> Vec<PinOutput> {
        with_machine(|machine| machine.take_outputs())
    }

    fn write_console(bytes: Vec<u8>) {
        // This scaffold echoes terminal input; a real chip would push it into
        // its UART receive FIFO instead.
        with_machine(|machine| machine.console.extend_from_slice(&bytes));
    }
}

impl DebugGuest for Chip {
    fn available() -> bool {
        true
    }

    fn pc() -> u32 {
        with_machine(|machine| machine.pc as u32)
    }

    fn registers() -> Vec<Register> {
        // This toy core's whole architectural state is the pc, which the
        // debugger reads separately. Returning an empty list is honest: the
        // host shows only the pc rather than zeros dressed up as registers.
        Vec::new()
    }

    fn read(addr: u32, len: u32) -> Result<Vec<u8>, String> {
        with_machine(|machine| machine.read(addr, len))
    }

    fn disassemble(addr: u32, count: u32) -> String {
        with_machine(|machine| machine.disassemble(addr, count))
    }

    fn set_breakpoints(pcs: Vec<u32>) {
        with_machine(|machine| machine.set_breakpoints(&pcs));
    }

    fn step() -> RunResult {
        with_machine(|machine| {
            let result = machine.step();
            flush_console(machine);
            result
        })
    }
}

bindings::export!(Chip with_types_in bindings);
