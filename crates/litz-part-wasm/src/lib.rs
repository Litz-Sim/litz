//! A schematic part that is a WebAssembly component.
//!
//! The contract is `litz:part@0.2.0` (`crates/litz-part-wit/wit`). A
//! `part.toml` names a `.wasm` file, the board's `parts::build` loads it here,
//! and everything above this crate — nets, settling, tests — cannot tell it
//! apart from an engine compiled into the board.
//!
//! What the guest gets: no WASI, no clock, no filesystem, no randomness. The
//! world imports exactly two host calls, `log(message)` and `rom()`, and
//! exports one `part` interface: pins in, drives and state out, plus the bus,
//! attribute and framebuffer capabilities a display or an EEPROM needs.
//! Memory is capped and every call gets a fuel budget, so a part that loops
//! forever comes back as a trap instead of hanging the host. A trap cannot be
//! reported through the [`Part`] trait (it has no error channel), so the part
//! falls back to `HIGH_Z`, remembers the message, and [`WasmPart::fault`]
//! exposes it for a caller that wants to ask.
//!
//! A module built against `litz:part@0.1.0` still loads: the host detects the
//! version from the component's exported interface and answers the 0.2-only
//! calls with their empty defaults. [`WasmPart::abi`] says which one it is.
#![forbid(unsafe_code)]

// The 0.2 contract, as wit-bindgen sees it. Kept in its own module so the
// generated names cannot collide with this crate's. The world is named
// `component`, so the generated world type is also `Component`; wasmtime's own
// `Component` is always written by its full path below.
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "../litz-part-wit/wit",
        world: "component",
    });
}

/// The 0.1.0 contract, unchanged, so components built before 0.2 keep loading.
pub mod legacy {
    wasmtime::component::bindgen!({
        path: "../litz-part-wit/wit-legacy",
        world: "component",
    });
}

use bindings::Component as ComponentV2;
use bindings::litz::part::types::{
    AttributeDef as WitAttributeDef, AttributeValue as WitAttributeValue, Damage as WitDamage,
    Drive as WitDrive, FramebufferInfo as WitFramebufferInfo, Level as WitLevel,
    PartState as WitPartState, Strength as WitStrength,
};
use legacy::Component as ComponentV1;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};
use std::cell::{Cell, RefCell};
use std::fmt;
use wasmtime::component::{Component as WasmComponent, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

/// Guest memory ceiling. A part is kilobytes at most; anything past this is a
/// bug or an attack, and either way the host should stop it.
const MAX_MEMORY: usize = 16 << 20;
/// Fuel granted per component call. One fuel unit is roughly one instruction;
/// a part call is a handful, and even a framebuffer upload is thousands, so
/// this is a generous ceiling that still catches a guest that never returns.
/// Small enough that the runaway fixture traps in well under a second.
const FUEL_PER_CALL: u64 = 1 << 24;

/// The contract version a loaded module implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartAbi {
    /// `litz:part@0.1.0`: pins only, no host imports.
    V1,
    /// `litz:part@0.2.0`: pins, buses, attributes, framebuffer, `log`/`rom`.
    V2,
}

impl PartAbi {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "litz:part@0.1.0",
            Self::V2 => "litz:part@0.2.0",
        }
    }
}

impl fmt::Display for PartAbi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A value in a part's attribute table.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue {
    Number(f64),
    Text(String),
    Flag(bool),
}

/// One entry of [`WasmPart::attributes`].
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub name: String,
    pub description: String,
    pub value: AttributeValue,
}

/// The geometry a display advertises through `framebuffer()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramebufferInfo {
    pub width: u32,
    pub height: u32,
}

/// A damaged rectangle returned by `damage()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damage {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// What the host left behind for diagnostics.
struct HostState {
    limits: StoreLimits,
    /// The image `rom()` returns, set before the component is instantiated.
    rom: Vec<u8>,
    /// Lines the guest emitted with `log()`, in order.
    logs: Vec<String>,
}

impl wasmtime::component::HasData for HostState {
    type Data<'a> = &'a mut HostState;
}

/// The `types` interface declares data only, so the host side is empty.
impl bindings::litz::part::types::Host for HostState {}
impl legacy::litz::part::types::Host for HostState {}

impl bindings::litz::part::host::Host for HostState {
    fn log(&mut self, message: String) {
        self.logs.push(message);
    }

    fn rom(&mut self) -> Vec<u8> {
        self.rom.clone()
    }
}

/// One of the two contract versions a loaded component speaks.
enum Instance {
    // Boxed because the two generated worlds differ in size and the enum is
    // stored in every loaded part; the one allocation at load is free in
    // comparison with instantiating a component.
    V2(Box<ComponentV2>),
    V1(Box<ComponentV1>),
}

/// A loaded part module, ready to sit on a board.
pub struct WasmPart {
    // `Part::drive` takes `&self` but a wasmtime store has to be borrowed
    // mutably, so the store lives behind a `RefCell`. Nothing else is shared.
    store: RefCell<Store<HostState>>,
    instance: Instance,
    abi: PartAbi,
    name: String,
    description: String,
    /// Pin names in the order the guest declares them, for diagnostics.
    pins: Vec<String>,
    /// The 7-bit I2C address this instance answers at, once a board attaches
    /// it. The `I2cDevice` trait's `connect` has no address argument, so the
    /// adapter reads it from here; a part sits on at most one bus.
    i2c_address: Cell<u16>,
    /// The last trap, kept rather than swallowed entirely.
    fault: RefCell<Option<String>>,
}

impl WasmPart {
    /// Load a component from bytes, naming it in any error (`module.wasm`, or
    /// the module path a manifest wrote). No ROM is attached.
    pub fn from_bytes(bytes: &[u8], name: &str) -> Result<Self, String> {
        Self::from_bytes_with_rom(bytes, name, &[])
    }

    /// Load a component and attach `rom` as the bytes its `host.rom()` import
    /// returns. The contents are fixed for the lifetime of the instance.
    pub fn from_bytes_with_rom(bytes: &[u8], name: &str, rom: &[u8]) -> Result<Self, String> {
        let mut config = Config::new();
        // The component model is what makes the WIT boundary possible; fuel is
        // how a runaway guest is stopped rather than trusted.
        config.wasm_component_model(true);
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| format!("{name}: {e}"))?;
        let component =
            WasmComponent::from_binary(&engine, bytes).map_err(|e| format!("{name}: {e}"))?;

        let abi = contract_version(&component, &engine, name)?;

        let limits = StoreLimitsBuilder::new().memory_size(MAX_MEMORY).build();
        let mut store = Store::new(
            &engine,
            HostState {
                limits,
                rom: rom.to_vec(),
                logs: Vec::new(),
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| format!("{name}: {e}"))?;

        // Both worlds are linked: the interfaces differ by version, so a 0.1
        // module and a 0.2 module can each find the imports they declare.
        let mut linker: Linker<HostState> = Linker::new(&engine);
        ComponentV2::add_to_linker::<HostState, HostState>(&mut linker, |state| state)
            .map_err(|e| format!("{name}: {e}"))?;
        ComponentV1::add_to_linker::<HostState, HostState>(&mut linker, |state| state)
            .map_err(|e| format!("{name}: {e}"))?;

        let (instance, info) = match abi {
            PartAbi::V2 => {
                let instance = ComponentV2::instantiate(&mut store, &component, &linker)
                    .map_err(|e| format!("{name}: {e}"))?;
                let _ = store.set_fuel(FUEL_PER_CALL);
                let info = instance
                    .litz_part_part()
                    .call_info(&mut store)
                    .map_err(|e| format!("{name}: info(): {e}"))?;
                (Instance::V2(Box::new(instance)), info)
            }
            PartAbi::V1 => {
                let instance = ComponentV1::instantiate(&mut store, &component, &linker)
                    .map_err(|e| format!("{name}: {e}"))?;
                let _ = store.set_fuel(FUEL_PER_CALL);
                let info = instance
                    .litz_part_part()
                    .call_info(&mut store)
                    .map_err(|e| format!("{name}: info(): {e}"))?;
                let info = bindings::litz::part::types::PartInfo {
                    name: info.name,
                    description: info.description,
                    pins: info.pins,
                };
                (Instance::V1(Box::new(instance)), info)
            }
        };

        Ok(Self {
            store: RefCell::new(store),
            instance,
            abi,
            name: info.name,
            description: info.description,
            pins: info.pins,
            i2c_address: Cell::new(0),
            fault: RefCell::new(None),
        })
    }

    /// The contract version the module was built against.
    pub fn abi(&self) -> PartAbi {
        self.abi
    }

    /// The name the guest reports for itself.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The guest's own description, for diagnostics.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The guest's pin table. The board assigns indices from the catalog, so
    /// this is informational; the integration test is what keeps them equal.
    pub fn pins(&self) -> &[String] {
        &self.pins
    }

    /// The last trap this part produced, if any.
    pub fn fault(&self) -> Option<String> {
        self.fault.borrow().clone()
    }

    /// Lines the guest has emitted with `log()`, in order.
    pub fn logs(&self) -> Vec<String> {
        self.store.borrow().data().logs.clone()
    }

    /// The ROM image the host attached at load.
    pub fn rom(&self) -> Vec<u8> {
        self.store.borrow().data().rom.clone()
    }

    /// Record the address a board attached this instance at. The next
    /// `i2c-connect` the bus delivers reports it to the guest.
    pub fn set_i2c_address(&self, address: u16) {
        self.i2c_address.set(address);
    }

    /// The address recorded by [`WasmPart::set_i2c_address`].
    pub fn i2c_address(&self) -> u16 {
        self.i2c_address.get()
    }

    fn note_fault(&self, error: wasmtime::Error) {
        let mut fault = self.fault.borrow_mut();
        if fault.is_none() {
            // Alternate formatting keeps the whole cause chain, so a fuel
            // exhaustion says what it was rather than only where it happened.
            *fault = Some(format!("{error:#}"));
        }
    }

    /// Lend the store with a fresh fuel budget. Each call is independent, so
    /// a lot of small calls cannot starve a later one.
    fn with_store<T>(
        &self,
        call: impl FnOnce(&mut Store<HostState>) -> Result<T, wasmtime::Error>,
    ) -> Result<T, wasmtime::Error> {
        let mut store = self.store.borrow_mut();
        let _ = store.set_fuel(FUEL_PER_CALL);
        call(&mut store)
    }

    // ------------------------------------------------------------- attributes

    /// Every attribute the part exposes, with its current value.
    pub fn attributes(&self) -> Vec<Attribute> {
        let Instance::V2(instance) = &self.instance else {
            return Vec::new();
        };
        match self.with_store(|store| instance.litz_part_part().call_attributes(store)) {
            Ok(defs) => defs.into_iter().map(our_attribute).collect(),
            Err(error) => {
                self.note_fault(error);
                Vec::new()
            }
        }
    }

    /// Set one attribute. A name the part does not know is ignored by the
    /// guest; the contract has no error channel for it.
    pub fn set_attribute(&self, name: &str, value: AttributeValue) {
        let Instance::V2(instance) = &self.instance else {
            return;
        };
        let result = self.with_store(|store| {
            instance
                .litz_part_part()
                .call_set_attribute(store, name, &wit_attribute(value))
        });
        if let Err(error) = result {
            self.note_fault(error);
        }
    }

    // ------------------------------------------------------------- framebuffer

    /// The display geometry, or `None` for a part that is not a display.
    pub fn framebuffer(&self) -> Option<FramebufferInfo> {
        let Instance::V2(instance) = &self.instance else {
            return None;
        };
        match self.with_store(|store| instance.litz_part_part().call_framebuffer(store)) {
            Ok(info) => {
                info.map(|WitFramebufferInfo { width, height }| FramebufferInfo { width, height })
            }
            Err(error) => {
                self.note_fault(error);
                None
            }
        }
    }

    /// Upload RGBA8888 bytes at `offset` into the guest's framebuffer.
    pub fn draw(&self, offset: u32, pixels: &[u8]) {
        let Instance::V2(instance) = &self.instance else {
            return;
        };
        let result =
            self.with_store(|store| instance.litz_part_part().call_draw(store, offset, pixels));
        if let Err(error) = result {
            self.note_fault(error);
        }
    }

    /// The damage rectangle accumulated since the last call, cleared by
    /// reading it.
    pub fn damage(&self) -> Option<Damage> {
        let Instance::V2(instance) = &self.instance else {
            return None;
        };
        match self.with_store(|store| instance.litz_part_part().call_damage(store)) {
            Ok(damage) => damage.map(
                |WitDamage {
                     x,
                     y,
                     width,
                     height,
                 }| Damage {
                    x,
                    y,
                    width,
                    height,
                },
            ),
            Err(error) => {
                self.note_fault(error);
                None
            }
        }
    }

    // ------------------------------------------------------------------- buses

    /// The host addressed this part at `addr`; `read` selects the phase.
    pub fn i2c_connect(&self, addr: u16, read: bool) {
        let Instance::V2(instance) = &self.instance else {
            return;
        };
        let result = self.with_store(|store| {
            instance
                .litz_part_part()
                .call_i2c_connect(store, addr, read)
        });
        if let Err(error) = result {
            self.note_fault(error);
        }
    }

    /// The byte this device drives next. Only meaningful after a read phase.
    pub fn i2c_read(&self) -> u8 {
        let Instance::V2(instance) = &self.instance else {
            return 0;
        };
        match self.with_store(|store| instance.litz_part_part().call_i2c_read(store)) {
            Ok(byte) => byte,
            Err(error) => {
                self.note_fault(error);
                0
            }
        }
    }

    /// A byte the controller wrote. False nacks it.
    pub fn i2c_write(&self, byte: u8) -> bool {
        let Instance::V2(instance) = &self.instance else {
            return false;
        };
        match self.with_store(|store| instance.litz_part_part().call_i2c_write(store, byte)) {
            Ok(acked) => acked,
            Err(error) => {
                self.note_fault(error);
                false
            }
        }
    }

    /// End of the transaction, including before a repeated start.
    pub fn i2c_stop(&self) {
        let Instance::V2(instance) = &self.instance else {
            return;
        };
        if let Err(error) = self.with_store(|store| instance.litz_part_part().call_i2c_stop(store))
        {
            self.note_fault(error);
        }
    }

    /// Chip-select changed; every SPI transfer is bracketed by this.
    pub fn spi_select(&self, selected: bool) {
        let Instance::V2(instance) = &self.instance else {
            return;
        };
        if let Err(error) =
            self.with_store(|store| instance.litz_part_part().call_spi_select(store, selected))
        {
            self.note_fault(error);
        }
    }

    /// Full-duplex SPI exchange. A guest that answers short is padded with
    /// 0xff, the level a floating MISO line reads.
    pub fn spi_transfer(&self, tx: &[u8], rx_len: u32) -> Vec<u8> {
        let Instance::V2(instance) = &self.instance else {
            return vec![0xff; rx_len as usize];
        };
        match self.with_store(|store| {
            instance
                .litz_part_part()
                .call_spi_transfer(store, tx, rx_len)
        }) {
            Ok(mut rx) => {
                let len = rx_len as usize;
                rx.truncate(len);
                rx.resize(len, 0xff);
                rx
            }
            Err(error) => {
                self.note_fault(error);
                vec![0xff; rx_len as usize]
            }
        }
    }

    /// Bytes the chip transmitted, stamped with their simulated arrival time.
    pub fn uart_receive(&self, bytes: &[u8], at_ns: u64) {
        let Instance::V2(instance) = &self.instance else {
            return;
        };
        let result = self.with_store(|store| {
            instance
                .litz_part_part()
                .call_uart_receive(store, bytes, at_ns)
        });
        if let Err(error) = result {
            self.note_fault(error);
        }
    }

    /// Bytes the part has ready for the chip.
    pub fn uart_poll(&self) -> Vec<u8> {
        let Instance::V2(instance) = &self.instance else {
            return Vec::new();
        };
        match self.with_store(|store| instance.litz_part_part().call_uart_poll(store)) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.note_fault(error);
                Vec::new()
            }
        }
    }
}

/// Which `litz:part` interface the component exports.
///
/// The component model keeps the version in the export name, so no call has
/// to be attempted to find out; an unrelated component is refused here with
/// the names it does export, which is the useful part of the error.
fn contract_version(
    component: &WasmComponent,
    engine: &Engine,
    name: &str,
) -> Result<PartAbi, String> {
    let exports: Vec<String> = component
        .component_type()
        .exports(engine)
        .map(|(name, _)| name.to_string())
        .collect();
    if exports
        .iter()
        .any(|export| export.contains("litz:part/part@0.1.0"))
    {
        return Ok(PartAbi::V1);
    }
    if exports
        .iter()
        .any(|export| export.contains("litz:part/part"))
    {
        return Ok(PartAbi::V2);
    }
    Err(format!(
        "{name}: not a litz:part component (exports: [{}])",
        exports.join(", ")
    ))
}

fn wit_level(level: Level) -> WitLevel {
    match level {
        Level::Low => WitLevel::Low,
        Level::High => WitLevel::High,
        Level::HighZ => WitLevel::Floating,
    }
}

fn legacy_level(level: Level) -> legacy::litz::part::types::Level {
    match level {
        Level::Low => legacy::litz::part::types::Level::Low,
        Level::High => legacy::litz::part::types::Level::High,
        Level::HighZ => legacy::litz::part::types::Level::Floating,
    }
}

fn our_drive(drive: WitDrive) -> Drive {
    match drive.level {
        WitLevel::Floating => Drive::HIGH_Z,
        WitLevel::Low => match drive.strength {
            WitStrength::Strong => Drive::strong(Level::Low),
            WitStrength::Weak => Drive::weak(Level::Low),
        },
        WitLevel::High => match drive.strength {
            WitStrength::Strong => Drive::strong(Level::High),
            WitStrength::Weak => Drive::weak(Level::High),
        },
    }
}

fn our_legacy_drive(drive: legacy::litz::part::types::Drive) -> Drive {
    use legacy::litz::part::types::{Level as LegacyLevel, Strength as LegacyStrength};
    match drive.level {
        LegacyLevel::Floating => Drive::HIGH_Z,
        LegacyLevel::Low => match drive.strength {
            LegacyStrength::Strong => Drive::strong(Level::Low),
            LegacyStrength::Weak => Drive::weak(Level::Low),
        },
        LegacyLevel::High => match drive.strength {
            LegacyStrength::Strong => Drive::strong(Level::High),
            LegacyStrength::Weak => Drive::weak(Level::High),
        },
    }
}

fn our_state(state: WitPartState) -> PartState {
    match state {
        WitPartState::Led(lit) => PartState::Led { lit },
        WitPartState::Button(pressed) => PartState::Button { pressed },
        WitPartState::Switch(on) => PartState::Switch { on },
        WitPartState::Segments(mask) => PartState::Segments { mask },
        WitPartState::Tone(tone) => PartState::Tone {
            sounding: tone.sounding,
            hz: tone.hz,
        },
        WitPartState::Servo(degrees) => PartState::Servo { degrees },
        WitPartState::Knob(permille) => PartState::Knob { permille },
        WitPartState::None => PartState::None,
    }
}

fn our_legacy_state(state: legacy::litz::part::types::PartState) -> PartState {
    use legacy::litz::part::types::PartState as LegacyPartState;
    match state {
        LegacyPartState::Led(lit) => PartState::Led { lit },
        LegacyPartState::Button(pressed) => PartState::Button { pressed },
        LegacyPartState::Switch(on) => PartState::Switch { on },
        LegacyPartState::Segments(mask) => PartState::Segments { mask },
        LegacyPartState::Tone(tone) => PartState::Tone {
            sounding: tone.sounding,
            hz: tone.hz,
        },
        LegacyPartState::Servo(degrees) => PartState::Servo { degrees },
        LegacyPartState::Knob(permille) => PartState::Knob { permille },
        LegacyPartState::None => PartState::None,
    }
}

fn our_attribute(def: WitAttributeDef) -> Attribute {
    Attribute {
        name: def.name,
        description: def.description,
        value: our_attribute_value(def.value),
    }
}

fn our_attribute_value(value: WitAttributeValue) -> AttributeValue {
    match value {
        WitAttributeValue::Number(number) => AttributeValue::Number(number),
        WitAttributeValue::Text(text) => AttributeValue::Text(text),
        WitAttributeValue::Flag(flag) => AttributeValue::Flag(flag),
    }
}

fn wit_attribute(value: AttributeValue) -> WitAttributeValue {
    match value {
        AttributeValue::Number(number) => WitAttributeValue::Number(number),
        AttributeValue::Text(text) => WitAttributeValue::Text(text),
        AttributeValue::Flag(flag) => WitAttributeValue::Flag(flag),
    }
}

impl Part for WasmPart {
    fn drive(&self, pin: PinId) -> Drive {
        match &self.instance {
            Instance::V2(instance) => {
                let result =
                    self.with_store(|store| instance.litz_part_part().call_drive(store, pin.0));
                match result {
                    Ok(drive) => our_drive(drive),
                    Err(error) => {
                        self.note_fault(error);
                        Drive::HIGH_Z
                    }
                }
            }
            Instance::V1(instance) => {
                let result =
                    self.with_store(|store| instance.litz_part_part().call_drive(store, pin.0));
                match result {
                    Ok(drive) => our_legacy_drive(drive),
                    Err(error) => {
                        self.note_fault(error);
                        Drive::HIGH_Z
                    }
                }
            }
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        match &self.instance {
            Instance::V2(instance) => {
                let result = self.with_store(|store| {
                    instance
                        .litz_part_part()
                        .call_set_pin(store, pin.0, wit_level(level), now)
                });
                if let Err(error) = result {
                    self.note_fault(error);
                }
            }
            Instance::V1(instance) => {
                let result = self.with_store(|store| {
                    instance
                        .litz_part_part()
                        .call_set_pin(store, pin.0, legacy_level(level), now)
                });
                if let Err(error) = result {
                    self.note_fault(error);
                }
            }
        }
    }

    fn next_event(&self) -> Option<Tick> {
        let result = match &self.instance {
            Instance::V2(instance) => {
                self.with_store(|store| instance.litz_part_part().call_next_event(store))
            }
            Instance::V1(instance) => {
                self.with_store(|store| instance.litz_part_part().call_next_event(store))
            }
        };
        match result {
            Ok(event) => event,
            Err(error) => {
                self.note_fault(error);
                None
            }
        }
    }

    fn advance(&mut self, now: Tick) {
        let result = match &self.instance {
            Instance::V2(instance) => {
                self.with_store(|store| instance.litz_part_part().call_advance(store, now))
            }
            Instance::V1(instance) => {
                self.with_store(|store| instance.litz_part_part().call_advance(store, now))
            }
        };
        if let Err(error) = result {
            self.note_fault(error);
        }
    }

    fn state(&self) -> PartState {
        match &self.instance {
            Instance::V2(instance) => {
                match self.with_store(|store| instance.litz_part_part().call_state(store)) {
                    Ok(state) => our_state(state),
                    Err(error) => {
                        self.note_fault(error);
                        PartState::None
                    }
                }
            }
            Instance::V1(instance) => {
                match self.with_store(|store| instance.litz_part_part().call_state(store)) {
                    Ok(state) => our_legacy_state(state),
                    Err(error) => {
                        self.note_fault(error);
                        PartState::None
                    }
                }
            }
        }
    }

    fn press(&mut self, pressed: bool) -> bool {
        let result = match &self.instance {
            Instance::V2(instance) => {
                self.with_store(|store| instance.litz_part_part().call_press(store, pressed))
            }
            Instance::V1(instance) => {
                self.with_store(|store| instance.litz_part_part().call_press(store, pressed))
            }
        };
        match result {
            Ok(has_input) => has_input,
            Err(error) => {
                self.note_fault(error);
                false
            }
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        let result =
            match &self.instance {
                Instance::V2(instance) => self
                    .with_store(|store| instance.litz_part_part().call_set_control(store, value)),
                Instance::V1(instance) => self
                    .with_store(|store| instance.litz_part_part().call_set_control(store, value)),
            };
        match result {
            Ok(has_control) => has_control,
            Err(error) => {
                self.note_fault(error);
                false
            }
        }
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        match &self.instance {
            Instance::V2(instance) => {
                match self.with_store(|store| instance.litz_part_part().call_analog(store, pin.0)) {
                    Ok(volts) => volts,
                    Err(error) => {
                        self.note_fault(error);
                        None
                    }
                }
            }
            // 0.1 has no analog channel: a digital part is all it can be.
            Instance::V1(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litz_core::Strength;

    /// The component the catalog ships for `litz-74hc595`, built against
    /// 0.1.0 and kept as the compatibility proof.
    const MODULE: &[u8] = include_bytes!("../../litz-catalog/builtin/parts/74hc595.wasm");
    /// The 24C02 reference device, built against 0.2.0.
    const EEPROM: &[u8] = include_bytes!("../../litz-catalog/builtin/parts/24c02.wasm");
    /// The small framebuffer display, built against 0.2.0.
    const DISPLAY: &[u8] = include_bytes!("../../litz-catalog/builtin/parts/display.wasm");
    /// A guest whose `drive` never returns.
    const RUNAWAY: &[u8] = include_bytes!("../tests/fixtures/runaway.wasm");

    #[test]
    fn the_committed_module_loads_and_describes_itself() {
        let part = WasmPart::from_bytes(MODULE, "74hc595.wasm").expect("the module is a component");
        assert_eq!(
            part.abi(),
            PartAbi::V1,
            "the shipped 595 is a 0.1 component"
        );
        assert_eq!(part.name(), "74hc595");
        assert_eq!(
            part.pins(),
            [
                "DS", "MR", "OE", "Q0", "Q1", "Q2", "Q3", "Q4", "Q5", "Q6", "Q7", "SHCP", "STCP"
            ]
        );
    }

    /// A core module has no component type information, so the loader must
    /// refuse it and say which file it was.
    #[test]
    fn a_module_that_is_not_a_component_is_refused() {
        let error = match WasmPart::from_bytes(b"\0asm\x01\0\0\0", "junk.wasm") {
            Ok(_) => panic!("a core module is not a component"),
            Err(error) => error,
        };
        assert!(error.contains("junk.wasm"), "{error}");
    }

    /// The part's own pin table is alphabetical (the catalog's `BTreeMap`
    /// order), and a Q pin drives strong at the latched bit.
    #[test]
    fn the_guest_drives_the_pin_index_the_catalog_assigns() {
        let mut part = WasmPart::from_bytes(MODULE, "74hc595.wasm").expect("loads");
        // Q0 is index 3; with the latch clear it drives low (OE is not wired,
        // so the outputs are enabled).
        assert_eq!(part.drive(PinId(3)), Drive::strong(Level::Low));
        assert_eq!(part.state(), PartState::None);
        assert!(!part.press(true));
        assert!(!part.set_control(500));
        assert_eq!(part.next_event(), None);
        assert_eq!(part.analog(PinId(3)), None, "0.1 has no analog channel");
        assert!(part.attributes().is_empty(), "0.1 has no attributes");
        assert_eq!(part.framebuffer(), None, "0.1 has no framebuffer");
        assert!(part.fault().is_none());
    }

    /// The strength is carried across the boundary: a weak drive from the
    /// guest stays weak in the net resolver.
    #[test]
    fn strength_survives_the_boundary() {
        let drive = our_drive(WitDrive {
            level: WitLevel::High,
            strength: WitStrength::Weak,
        });
        assert_eq!(drive, Drive::weak(Level::High));
        assert_eq!(drive.strength, Strength::Weak);
        let floating = our_drive(WitDrive {
            level: WitLevel::Floating,
            strength: WitStrength::Strong,
        });
        assert_eq!(floating, Drive::HIGH_Z);
    }

    #[test]
    fn the_eeprom_starts_from_the_rom_the_host_supplied() {
        let part = WasmPart::from_bytes_with_rom(EEPROM, "24c02.wasm", &[0xde, 0xad, 0xbe, 0xef])
            .expect("the eeprom is a 0.2 component");
        assert_eq!(part.abi(), PartAbi::V2);
        assert!(
            part.logs().iter().any(|line| line.contains("24c02")),
            "{:?}",
            part.logs()
        );

        // Point at address 0, then read four bytes back.
        part.i2c_connect(0x50, false);
        assert!(part.i2c_write(0x00));
        part.i2c_stop();
        part.i2c_connect(0x50, true);
        let bytes: Vec<u8> = (0..4).map(|_| part.i2c_read()).collect();
        part.i2c_stop();
        assert_eq!(bytes, [0xde, 0xad, 0xbe, 0xef]);
        assert!(part.fault().is_none());
    }

    #[test]
    fn attribute_values_round_trip() {
        let part = WasmPart::from_bytes(EEPROM, "24c02.wasm").expect("loads");
        let before = part.attributes();
        assert!(
            before
                .iter()
                .any(|attr| attr.name == "size" && attr.value == AttributeValue::Number(256.0)),
            "{before:?}"
        );

        part.set_attribute("owner", AttributeValue::Text("litz".into()));
        part.set_attribute("write-protect", AttributeValue::Flag(true));
        let after = part.attributes();
        assert!(
            after
                .iter()
                .any(|attr| attr.name == "owner"
                    && attr.value == AttributeValue::Text("litz".into()))
        );
        assert!(
            after.iter().any(
                |attr| attr.name == "write-protect" && attr.value == AttributeValue::Flag(true)
            )
        );
    }

    #[test]
    fn a_display_reports_its_framebuffer_and_pending_damage() {
        let part = WasmPart::from_bytes(DISPLAY, "display.wasm").expect("loads");
        assert_eq!(
            part.framebuffer(),
            Some(FramebufferInfo {
                width: 8,
                height: 8
            })
        );
        assert_eq!(part.damage(), None, "nothing drawn yet");

        // One red pixel at the origin.
        part.draw(0, &[0xff, 0x00, 0x00, 0xff]);
        assert_eq!(
            part.damage(),
            Some(Damage {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            })
        );
        assert_eq!(part.damage(), None, "damage is consumed by reading it");

        // The first pixel of the second row.
        part.draw(8 * 4, &[0x00, 0xff, 0x00, 0xff]);
        assert_eq!(
            part.damage(),
            Some(Damage {
                x: 0,
                y: 1,
                width: 1,
                height: 1
            })
        );
    }

    #[test]
    fn a_runaway_guest_traps_and_the_fault_is_reported() {
        let part = WasmPart::from_bytes(RUNAWAY, "runaway.wasm").expect("loads");
        assert_eq!(
            part.drive(PinId(0)),
            Drive::HIGH_Z,
            "a trap falls back to high-Z"
        );
        let fault = part.fault().expect("the trap was kept");
        assert!(fault.to_lowercase().contains("fuel"), "{fault}");
    }
}
