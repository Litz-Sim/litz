//! Parts and chips whose behaviour is a component the JavaScript host runs.
//!
//! On a native build a custom part is loaded by `litz-part-wasm` with
//! wasmtime: the component and its host live in one process. A wasm32 build
//! cannot host a second component, so the roles split: the host (JavaScript)
//! instantiates the component, and this module is the Rust side of the
//! contract. Every call the [`Part`] trait needs becomes an import the host
//! answers; the values that cross are integers and byte ranges in linear
//! memory.
//!
//! Outputs that can be longer than one word are written into a buffer this
//! side provides. The scratch buffer grows but is never handed to the host
//! empty: the host is told its capacity and returns what it wrote.
//!
//! The import module is `litz_host`. A host that does not supply it cannot
//! instantiate the module at all, which is a clearer failure than a part that
//! silently reports nothing.
//!
//! A `litz:mcu@0.2.0` guest has a second door: its bus, ADC, Wi-Fi, BLE and
//! radio calls. Those are records rather than words, so they travel as JSON
//! through [`guest_call`] to a [`GuestHost`] registered for the chip. The
//! implementation lives above this crate, in `litz-mcu-wasm`, and is the same
//! one the native runner uses.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use litz_core::{Drive, Level, Part, PartState, PinId, Strength, Tick};

use crate::Conflict;
use crate::bus::{I2cDevice, SpiDevice, UartPeer};
use crate::chip::{Chip, ChipInfo, FlashLayout, ImageKind, Input, RunResult, Stop};

#[link(wasm_import_module = "litz_host")]
unsafe extern "C" {
    /// Instantiate the component registered under `name`. 0 on failure.
    fn part_create(name_ptr: u32, name_len: u32) -> u32;
    fn part_drop(id: u32);
    fn part_set_pin(id: u32, pin: u32, level: u32, at_ns: u64);
    fn part_advance(id: u32, to_ns: u64);
    /// `-1` when the part has no pending event.
    fn part_next_event(id: u32) -> i64;
    /// `level | strength << 8`; level 2 means floating.
    fn part_drive(id: u32, pin: u32) -> u32;
    /// Writes `kind | value << 32` as two little-endian words; 0 on fault.
    fn part_state(id: u32, dst_ptr: u32, cap: u32) -> u32;
    fn part_press(id: u32, pressed: u32) -> u32;
    fn part_set_control(id: u32, permille: u32) -> u32;
    fn part_analog(id: u32, pin: u32) -> f32;
    fn part_i2c_connect(id: u32, address: u32, read: u32);
    fn part_i2c_read(id: u32) -> u32;
    fn part_i2c_write(id: u32, byte: u32) -> u32;
    fn part_i2c_stop(id: u32);
    fn part_spi_select(id: u32, selected: u32);
    fn part_spi_transfer(
        id: u32,
        tx_ptr: u32,
        tx_len: u32,
        rx_len: u32,
        out_ptr: u32,
        out_cap: u32,
    ) -> u32;
    fn part_uart_receive(id: u32, bytes_ptr: u32, len: u32, at_ns: u64);
    fn part_uart_poll(id: u32, out_ptr: u32, out_cap: u32) -> u32;
    /// The last failure of a `part_create` or `chip_create`, as UTF-8 bytes.
    fn host_error(dst_ptr: u32, cap: u32) -> u32;

    // ------------------------------------------------------------------ chips

    /// Instantiate the component registered under `name` as a chip.
    fn chip_create(name_ptr: u32, name_len: u32) -> u32;
    fn chip_drop(id: u32);
    /// The guest's `chip-info` as JSON.
    fn chip_info(id: u32, dst_ptr: u32, cap: u32) -> u32;
    fn chip_load_image(id: u32, kind: u32, offset: u64, ptr: u32, len: u32) -> u32;
    /// The entry address, or -1 with the reason in `host_error`.
    fn chip_boot(id: u32) -> i64;
    /// `count` records of `pin: u32, level: u32`.
    fn chip_set_inputs(id: u32, ptr: u32, count: u32);
    /// Writes `cycles: u64, stop: u32`; returns the bytes written.
    fn chip_run(id: u32, budget: u64, out_ptr: u32) -> u32;
    /// Writes records of `pin: u32, level: u32, enabled: u32`; returns the
    /// bytes written.
    fn chip_take_outputs(id: u32, out_ptr: u32, cap: u32) -> u32;
    fn chip_write_console(id: u32, ptr: u32, len: u32);
}

thread_local! {
    /// Output space for the calls that return more than one word. Reused so a
    /// settle loop does not allocate per call.
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// Console bytes and the fault each host chip reported during its run.
    /// The guest pushes these through exports the host calls while `chip_run`
    /// is on the stack; the chip drains them afterwards, which is the pull
    /// shape [`Chip`] expects.
    static EVENTS: RefCell<BTreeMap<u32, GuestEvents>> = const { RefCell::new(BTreeMap::new()) };
}

#[derive(Default)]
struct GuestEvents {
    console: Vec<u8>,
    fault: Option<String>,
}

/// Record console bytes a host chip wrote during its run.
pub fn push_console(chip: u32, bytes: &[u8]) {
    EVENTS.with(|events| {
        events
            .borrow_mut()
            .entry(chip)
            .or_default()
            .console
            .extend_from_slice(bytes);
    });
}

/// Record the reason a host chip stopped.
pub fn push_fault(chip: u32, detail: String) {
    EVENTS.with(|events| {
        events.borrow_mut().entry(chip).or_default().fault = Some(detail);
    });
}

/// The circuit side of a `litz:mcu@0.2.0` guest's host calls.
///
/// The browser build implements this over `litz_mcu_wasm::HostState`, so a
/// transpiled guest's bus, ADC, Wi-Fi and BLE calls reach the same board code
/// the native loader uses. The values are JSON because the trait sits below
/// the code that knows the contract's records; the implementation owns the
/// shape.
pub trait GuestHost {
    /// The device tick the calls made during the next `run` are stamped at.
    fn set_now(&mut self, ns: u64);
    /// One host call: `op` is the contract's camelCase name, `args` its JSON
    /// object, and the returned JSON is the result. `Err` is the contract's
    /// own `result<_, string>` failure.
    fn call(&mut self, op: &str, args: &str) -> Result<String, String>;
}

type GuestFactory = Box<dyn Fn(&str, Rc<RefCell<crate::Board>>, u16) -> Option<Box<dyn GuestHost>>>;

thread_local! {
    /// Set by the module that can instantiate components. Empty on a native
    /// build, where components never travel through this bridge.
    static GUEST_FACTORY: RefCell<Option<GuestFactory>> = RefCell::new(None);
    /// One host per live chip, keyed by the id the JavaScript host assigned.
    /// Kept here rather than in `HostChip` so a reentrant host call can reach
    /// it while the chip is borrowed by `run`.
    static GUESTS: RefCell<BTreeMap<u32, Box<dyn GuestHost>>> = RefCell::new(BTreeMap::new());
}

/// Register the factory that builds a 0.2 host for a chip instance.
pub fn set_guest_factory(
    factory: impl Fn(&str, Rc<RefCell<crate::Board>>, u16) -> Option<Box<dyn GuestHost>> + 'static,
) {
    GUEST_FACTORY.with(|slot| *slot.borrow_mut() = Some(Box::new(factory)));
}

/// Answer one host call made by the guest with `chip`.
pub fn guest_call(chip: u32, op: &str, args: &str) -> Result<String, String> {
    GUESTS.with(|guests| {
        let mut guests = guests.borrow_mut();
        let host = guests
            .get_mut(&chip)
            .ok_or_else(|| format!("chip {chip} has no host runtime"))?;
        host.call(op, args)
    })
}

/// Tell the host for `chip` the tick the next calls happen at.
pub fn set_guest_now(chip: u32, ns: u64) {
    GUESTS.with(|guests| {
        if let Some(host) = guests.borrow_mut().get_mut(&chip) {
            host.set_now(ns);
        }
    });
}

fn open_guest(
    module: &str,
    board: Rc<RefCell<crate::Board>>,
    board_index: u16,
) -> Option<Box<dyn GuestHost>> {
    GUEST_FACTORY.with(|factory| {
        factory
            .borrow()
            .as_ref()
            .and_then(|factory| factory(module, board, board_index))
    })
}

fn register_guest(chip: u32, host: Box<dyn GuestHost>) {
    GUESTS.with(|guests| {
        guests.borrow_mut().insert(chip, host);
    });
}

fn unregister_guest(chip: u32) {
    GUESTS.with(|guests| {
        guests.borrow_mut().remove(&chip);
    });
}

/// Run `fill` with a writable buffer of at least `len` bytes and return what
/// it reports writing, copied out.
fn read_out(len: usize, fill: impl FnOnce(u32, u32) -> u32) -> Vec<u8> {
    OUT.with(|out| {
        let mut out = out.borrow_mut();
        if out.len() < len {
            out.resize(len, 0);
        }
        let written = fill(out.as_mut_ptr() as usize as u32, out.len() as u32);
        out[..(written as usize).min(out.len())].to_vec()
    })
}

/// The host's message for the last failed `part_create`.
pub(crate) fn last_error() -> String {
    let bytes = read_out(256, |ptr, cap| unsafe { host_error(ptr, cap) });
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A part whose behaviour is a component the JavaScript host instantiated.
pub(crate) struct HostPart {
    id: u32,
    /// The 7-bit address the manifest put this part at, for `i2c-connect`.
    address: u8,
}

impl HostPart {
    /// Ask the host for an instance of the component registered as `module`.
    pub(crate) fn open(module: &str) -> Result<Self, String> {
        let name = module.as_bytes();
        let id = unsafe { part_create(name.as_ptr() as usize as u32, name.len() as u32) };
        if id == 0 {
            let detail = last_error();
            return Err(if detail.is_empty() {
                format!("the host has no runtime for module {module:?}")
            } else {
                detail
            });
        }
        Ok(Self { id, address: 0 })
    }

    /// The address an I2C device answers at, reported to the guest on
    /// `i2c-connect` so it can check its own manifest.
    pub(crate) fn with_i2c_address(mut self, address: u8) -> Self {
        self.address = address;
        self
    }

    fn state(&self) -> PartState {
        let words = read_out(8, |ptr, cap| unsafe { part_state(self.id, ptr, cap) });
        if words.len() < 8 {
            return PartState::None;
        }
        let kind = u32::from_le_bytes(words[0..4].try_into().expect("four bytes"));
        let value = u32::from_le_bytes(words[4..8].try_into().expect("four bytes"));
        match kind {
            1 => PartState::Led { lit: false },
            2 => PartState::Led { lit: true },
            3 => PartState::Button { pressed: false },
            4 => PartState::Button { pressed: true },
            5 => PartState::Switch { on: false },
            6 => PartState::Switch { on: true },
            7 => PartState::Tone {
                sounding: false,
                hz: value,
            },
            8 => PartState::Tone {
                sounding: true,
                hz: value,
            },
            9 => PartState::Servo {
                degrees: value as u16,
            },
            10 => PartState::Segments { mask: value as u8 },
            11 => PartState::Knob {
                permille: value as u16,
            },
            _ => PartState::None,
        }
    }
}

impl Drop for HostPart {
    fn drop(&mut self) {
        unsafe { part_drop(self.id) };
    }
}

impl Part for HostPart {
    fn drive(&self, pin: PinId) -> Drive {
        let packed = unsafe { part_drive(self.id, u32::from(pin.0)) };
        let level = match packed & 0xff {
            0 => Level::Low,
            1 => Level::High,
            _ => return Drive::HIGH_Z,
        };
        let strength = if packed >> 8 & 1 == 0 {
            Strength::Strong
        } else {
            Strength::Weak
        };
        Drive { level, strength }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        let level = match level {
            Level::Low => 0,
            Level::High => 1,
            Level::HighZ => 2,
        };
        unsafe { part_set_pin(self.id, u32::from(pin.0), level, now) };
    }

    fn next_event(&self) -> Option<Tick> {
        let at = unsafe { part_next_event(self.id) };
        (at >= 0).then_some(at as u64)
    }

    fn advance(&mut self, now: Tick) {
        unsafe { part_advance(self.id, now) };
    }

    fn state(&self) -> PartState {
        HostPart::state(self)
    }

    fn press(&mut self, pressed: bool) -> bool {
        (unsafe { part_press(self.id, u32::from(u8::from(pressed))) }) == 1
    }

    fn set_control(&mut self, value: u16) -> bool {
        (unsafe { part_set_control(self.id, u32::from(value)) }) == 1
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        let volts = unsafe { part_analog(self.id, u32::from(pin.0)) };
        volts.is_finite().then_some(volts)
    }
}

impl I2cDevice for HostPart {
    fn connect(&mut self, read: bool) {
        unsafe { part_i2c_connect(self.id, u32::from(self.address), u32::from(u8::from(read))) };
    }

    fn read(&mut self) -> u8 {
        unsafe { part_i2c_read(self.id) as u8 }
    }

    fn write(&mut self, byte: u8) -> bool {
        (unsafe { part_i2c_write(self.id, u32::from(byte)) }) == 1
    }

    fn stop(&mut self) {
        unsafe { part_i2c_stop(self.id) };
    }
}

impl SpiDevice for HostPart {
    fn select(&mut self, selected: bool) {
        unsafe { part_spi_select(self.id, u32::from(selected)) };
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        let tx_ptr = tx.as_ptr() as usize as u32;
        let tx_len = tx.len() as u32;
        let mut rx = read_out(rx_len, |ptr, cap| unsafe {
            part_spi_transfer(self.id, tx_ptr, tx_len, rx_len as u32, ptr, cap)
        });
        rx.resize(rx_len, 0xff);
        rx
    }
}

impl UartPeer for HostPart {
    fn receive(&mut self, at: Tick, bytes: &[u8]) {
        unsafe {
            part_uart_receive(
                self.id,
                bytes.as_ptr() as usize as u32,
                bytes.len() as u32,
                at,
            )
        };
    }

    fn poll(&mut self, _at: Tick) -> Vec<u8> {
        read_out(256, |ptr, cap| unsafe { part_uart_poll(self.id, ptr, cap) })
    }
}

/// A chip whose behaviour is a component the JavaScript host instantiated.
///
/// The shape follows the chip interface of whichever contract the module
/// speaks: images in, pins in, budget in; cycles, outputs and console out. The
/// guest cannot reach the board, so every pin level is pushed here and every
/// drive is applied to the board here. A `litz:mcu@0.2.0` guest can also ask
/// the board about a bus or the air; those calls are answered by the
/// [`GuestHost`] registered for this chip.
pub struct HostChip {
    id: u32,
    info: ChipInfo,
    board: std::rc::Rc<RefCell<crate::Board>>,
    board_index: u16,
    cycles: u64,
    pending: Vec<(u64, Input)>,
    /// True when a 0.2 host was registered for this chip, so its clock has to
    /// be handed to the host before every guest call.
    guest: bool,
}

impl HostChip {
    /// Ask the host for an instance of the component registered as `module`.
    pub fn open(
        module: &str,
        board: std::rc::Rc<RefCell<crate::Board>>,
        board_index: u16,
    ) -> Result<Self, String> {
        let name = module.as_bytes();
        let id = unsafe { chip_create(name.as_ptr() as usize as u32, name.len() as u32) };
        if id == 0 {
            let detail = last_error();
            return Err(if detail.is_empty() {
                format!("the host has no runtime for module {module:?}")
            } else {
                detail
            });
        }
        let guest = open_guest(module, Rc::clone(&board), board_index);
        let has_guest = guest.is_some();
        if let Some(host) = guest {
            register_guest(id, host);
        }
        let mut chip = Self {
            id,
            info: ChipInfo {
                name: module.to_string(),
                cpu_hz: 1,
                flash: FlashLayout {
                    size: 0,
                    bootloader: 0,
                    partition_table: 0,
                    app: 0,
                },
            },
            board,
            board_index,
            cycles: 0,
            pending: Vec::new(),
            guest: has_guest,
        };
        chip.info = chip.read_info()?;
        Ok(chip)
    }

    fn now_ns(&self) -> u64 {
        self.cycles.saturating_mul(1_000_000_000) / self.info.cpu_hz.max(1)
    }

    fn read_info(&self) -> Result<ChipInfo, String> {
        let mut capacity = 512usize;
        loop {
            let bytes = read_out(capacity, |ptr, cap| unsafe { chip_info(self.id, ptr, cap) });
            if bytes.is_empty() && capacity > 4096 {
                return Err("the guest did not describe itself".into());
            }
            if bytes.len() < capacity {
                let text = String::from_utf8_lossy(&bytes);
                return parse_chip_info(&text);
            }
            capacity *= 2;
            if capacity > 64 * 1024 {
                return Err("the guest's chip description is larger than 64 KiB".into());
            }
        }
    }

    fn take_outputs(&mut self) {
        let bytes = read_out(12 * 256, |ptr, cap| unsafe {
            chip_take_outputs(self.id, ptr, cap)
        });
        let now = self.now_ns();
        let mut board = self.board.borrow_mut();
        board.advance_to(now);
        for record in bytes.as_chunks::<12>().0 {
            let pin = u32::from_le_bytes(record[0..4].try_into().expect("four bytes")) as u8;
            let level = match u32::from_le_bytes(record[4..8].try_into().expect("four bytes")) {
                0 => Level::Low,
                1 => Level::High,
                _ => Level::HighZ,
            };
            let enabled = u32::from_le_bytes(record[8..12].try_into().expect("four bytes")) != 0;
            let drive = if enabled {
                Drive::strong(level)
            } else {
                Drive::HIGH_Z
            };
            board.set_chip_output_on(self.board_index, pin, drive, now);
        }
    }

    fn pump_inputs(&mut self) -> Result<(), String> {
        let now = self.now_ns();
        let mut pins: Vec<(u32, u32)> = Vec::new();
        let mut uart: Vec<Vec<u8>> = Vec::new();
        let mut due = 0;
        while let Some((at, input)) = self.pending.get(due) {
            if *at > now {
                break;
            }
            match input {
                Input::Gpio { pin, level, .. } => pins.push((
                    u32::from(*pin),
                    match level {
                        Level::Low => 0,
                        Level::High => 1,
                        Level::HighZ => 2,
                    },
                )),
                Input::Uart { bytes, .. } => uart.push(bytes.clone()),
            }
            due += 1;
        }
        self.pending.drain(..due);
        if !pins.is_empty() {
            let mut packed = Vec::with_capacity(pins.len() * 8);
            for (pin, level) in &pins {
                packed.extend_from_slice(&pin.to_le_bytes());
                packed.extend_from_slice(&level.to_le_bytes());
            }
            unsafe { chip_set_inputs(self.id, packed.as_ptr() as usize as u32, pins.len() as u32) };
        }
        for bytes in uart {
            unsafe {
                chip_write_console(self.id, bytes.as_ptr() as usize as u32, bytes.len() as u32)
            };
        }
        Ok(())
    }
}

/// Read a `chip-info` the guest described as JSON.
fn parse_chip_info(text: &str) -> Result<ChipInfo, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("chip description: {e}"))?;
    let field = |name: &str| {
        value
            .get(name)
            .ok_or_else(|| format!("chip description has no {name:?}"))
    };
    let number = |name: &str| {
        field(name).and_then(|v| {
            v.as_u64()
                .ok_or_else(|| format!("chip description {name:?} is not a number"))
        })
    };
    let flash = field("flash")?;
    let flash_number = |name: &str| {
        flash
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("chip description flash.{name} is missing"))
    };
    Ok(ChipInfo {
        name: field("name")?.as_str().unwrap_or_default().to_string(),
        cpu_hz: number("cpuHz")?,
        flash: FlashLayout {
            size: flash_number("size")?,
            bootloader: flash_number("bootloader")?,
            partition_table: flash_number("partitionTable")?,
            app: flash_number("app")?,
        },
    })
}

impl Drop for HostChip {
    fn drop(&mut self) {
        unregister_guest(self.id);
        unsafe { chip_drop(self.id) };
        EVENTS.with(|events| {
            events.borrow_mut().remove(&self.id);
        });
    }
}

impl Chip for HostChip {
    fn info(&self) -> ChipInfo {
        self.info.clone()
    }

    fn load_image(&mut self, kind: ImageKind, offset: u64, bytes: &[u8]) -> Result<(), String> {
        let kind = match kind {
            ImageKind::Rom => 0,
            ImageKind::Bootloader => 1,
            ImageKind::PartitionTable => 2,
            ImageKind::Firmware => 3,
        };
        let ok = unsafe {
            chip_load_image(
                self.id,
                kind,
                offset,
                bytes.as_ptr() as usize as u32,
                bytes.len() as u32,
            )
        };
        if ok == 1 { Ok(()) } else { Err(last_error()) }
    }

    fn boot(&mut self) -> Result<u32, String> {
        if self.guest {
            set_guest_now(self.id, self.now_ns());
        }
        let entry = unsafe { chip_boot(self.id) };
        if entry < 0 {
            return Err(last_error());
        }
        Ok(entry as u32)
    }

    fn schedule(&mut self, inputs: Vec<Input>) -> Result<(), String> {
        self.pending = inputs
            .into_iter()
            .map(|input| {
                let at = match &input {
                    Input::Gpio { at_ns, .. } | Input::Uart { at_ns, .. } => *at_ns,
                };
                (at, input)
            })
            .collect();
        self.pending.sort_by_key(|(at, _)| *at);
        Ok(())
    }

    fn run(&mut self, budget_cycles: u64) -> RunResult {
        if let Err(message) = self.pump_inputs() {
            push_fault(self.id, message);
            return RunResult {
                cycles: 0,
                stop: Stop::Trap,
            };
        }

        let changed = self
            .board
            .borrow_mut()
            .take_chip_inputs_on(self.board_index);
        if !changed.is_empty() {
            let mut packed = Vec::with_capacity(changed.len() * 8);
            for (pin, level) in &changed {
                packed.extend_from_slice(&u32::from(*pin).to_le_bytes());
                let level = match level {
                    Level::Low => 0u32,
                    Level::High => 1,
                    Level::HighZ => 2,
                };
                packed.extend_from_slice(&level.to_le_bytes());
            }
            unsafe {
                chip_set_inputs(
                    self.id,
                    packed.as_ptr() as usize as u32,
                    changed.len() as u32,
                )
            };
        }

        if self.guest {
            set_guest_now(self.id, self.now_ns());
        }
        let out = read_out(16, |ptr, _cap| unsafe {
            chip_run(self.id, budget_cycles, ptr)
        });
        if out.len() < 16 {
            push_fault(self.id, "the guest returned no run result".into());
            return RunResult {
                cycles: 0,
                stop: Stop::Trap,
            };
        }
        let cycles = u64::from_le_bytes(out[0..8].try_into().expect("eight bytes"));
        let stop = match u32::from_le_bytes(out[8..12].try_into().expect("four bytes")) {
            0 => Stop::Budget,
            1 => Stop::Idle,
            2 => Stop::Halted,
            3 => Stop::Trap,
            4 => Stop::Reset,
            _ => Stop::Budget,
        };
        self.cycles = self.cycles.saturating_add(cycles);
        self.take_outputs();
        let stop = if self.fault().is_some() {
            Stop::Trap
        } else {
            stop
        };
        RunResult { cycles, stop }
    }

    fn seconds(&self) -> f64 {
        self.cycles as f64 / self.info.cpu_hz.max(1) as f64
    }

    fn write_console(&mut self, bytes: &[u8]) {
        if self.guest {
            set_guest_now(self.id, self.now_ns());
        }
        unsafe { chip_write_console(self.id, bytes.as_ptr() as usize as u32, bytes.len() as u32) };
    }

    fn take_console(&mut self) -> Vec<u8> {
        EVENTS.with(|events| {
            std::mem::take(&mut events.borrow_mut().entry(self.id).or_default().console)
        })
    }

    fn state(&self, part: &str) -> Option<PartState> {
        self.board.borrow().state(part)
    }

    fn press(&mut self, part: &str, pressed: bool) -> bool {
        let now = self.now_ns();
        self.board.borrow_mut().press(part, pressed, now)
    }

    fn set_control(&mut self, part: &str, value: u16) -> bool {
        let now = self.now_ns();
        self.board.borrow_mut().set_control(part, value, now)
    }

    fn set_named_control(&mut self, part: &str, name: &str, value: &str) -> bool {
        let now = self.now_ns();
        self.board
            .borrow_mut()
            .set_named_control(part, name, value, now)
    }

    fn part_pin_level(&self, part: &str, pin: Option<&str>) -> Option<Level> {
        let board = self.board.borrow();
        match pin {
            Some(pin) => board.part_pin_level(part, pin),
            None => board.chip_pin_level_on(self.board_index, part),
        }
    }

    fn gpio_level(&self, gpio: u8) -> Option<Level> {
        self.board.borrow().chip_level_on(self.board_index, gpio)
    }

    fn conflicts(&self) -> Vec<Conflict> {
        self.board.borrow().conflicts().to_vec()
    }

    fn fault(&self) -> Option<String> {
        EVENTS.with(|events| {
            events
                .borrow()
                .get(&self.id)
                .and_then(|events| events.fault.clone())
        })
    }

    fn touch(&mut self, x: u16, y: u16, down: bool) -> Result<(), String> {
        self.board.borrow_mut().touch_panel(x, y, down)
    }
}
