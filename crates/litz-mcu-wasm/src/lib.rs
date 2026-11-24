//! A microcontroller that is a WebAssembly component.
//!
//! There is one contract, `litz:mcu@0.2.0` (`crates/litz-mcu-wit/wit`). It
//! carries pins and console, the calls a real MCU peripheral block needs —
//! analog reads, I2C/SPI/UART transactions, Wi-Fi with TCP/UDP, and BLE
//! scanning and advertising — and a `debug` interface that mirrors the host's
//! `Chip` debug surface. A board's `board.toml` names the `.wasm` file and the
//! `abi` it speaks; the runner loads it here, and everything above this crate
//! — nets, parts, tests — cannot tell it apart from the built-in ESP32-C3
//! backend.
//!
//! What a guest gets: no WASI, no clock, no filesystem, no randomness. Every
//! peripheral and network answer comes from the circuit the host built, so the
//! guest can be a sensor node without reaching outside the sandbox. Memory is
//! capped and so is the fuel a single call may burn, which is what keeps a chip
//! that loops forever from hanging CI: the trap comes back to the host as a
//! stop reason instead.

// The contract, as wit-bindgen sees it. Kept in its own module so the
// generated names cannot collide with this crate's. Component loading is a
// native capability: the browser build runs transpiled components through
// JavaScript instead, so the generated bindings and wasmtime stay out of it.
//
// The path is the contract's one home in `litz-mcu-wit`; there is no copy
// beside this crate to drift from it.
#[cfg(not(target_arch = "wasm32"))]
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "../litz-mcu-wit/wit",
        world: "simulator",
    });
}

/// The host answers a guest's asks for. Shared by the native loader and the
/// browser bridge.
mod host;

pub use host::{
    BleAdvertisement, GattCharacteristic, GattNotification, GattService, GattWrite, HostState,
    RadioFrame, WifiLease, WifiNetwork,
};

/// In the browser the guest is a transpiled component driven by JavaScript;
/// the chip bridge asks this host to answer the same calls the native
/// bindings above answer.
#[cfg(target_arch = "wasm32")]
impl litz_board::host::GuestHost for HostState {
    fn set_now(&mut self, ns: u64) {
        HostState::set_now(self, ns);
    }

    fn call(&mut self, op: &str, args: &str) -> Result<String, String> {
        HostState::call(self, op, args)
    }
}

#[cfg(not(target_arch = "wasm32"))]
use bindings::Simulator;
#[cfg(not(target_arch = "wasm32"))]
use bindings::litz::mcu::types::{
    Image, Level as WitLevel, PinInput, Register as WitRegister, Stop as WitStop,
};
#[cfg(not(target_arch = "wasm32"))]
use litz_board::chip::{Chip, ChipInfo, FlashLayout, ImageKind, Input, RunResult, Stop};
#[cfg(not(target_arch = "wasm32"))]
use litz_board::{Board, Conflict};
#[cfg(not(target_arch = "wasm32"))]
use litz_core::{Drive, Level, PartState};
#[cfg(not(target_arch = "wasm32"))]
use std::cell::{Cell, RefCell};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::rc::Rc;
#[cfg(not(target_arch = "wasm32"))]
use wasmtime::component::{Component, Linker};
#[cfg(not(target_arch = "wasm32"))]
use wasmtime::{Config, Engine, Store, StoreLimitsBuilder};

/// Guest memory ceiling. A chip is a few megabytes at most; anything past
/// this is a bug or an attack, and either way the host should stop it.
#[cfg(not(target_arch = "wasm32"))]
const MAX_MEMORY: usize = 256 << 20;
/// Fuel granted per component call. One fuel unit is roughly one instruction;
/// a call only ever runs `budget` cycles, so this is a generous ceiling that
/// still catches a guest that ignores its budget.
#[cfg(not(target_arch = "wasm32"))]
const FUEL_PER_CALL: u64 = 1 << 34;

/// The host side of the contract, as wit-bindgen expects it. The work is done
/// by `HostState` in `host`; these impls only translate between its plain
/// types and the generated ones, which is what lets the browser bridge reuse
/// the same answers without wasmtime.
#[cfg(not(target_arch = "wasm32"))]
impl wasmtime::component::HasData for HostState {
    type Data<'a> = &'a mut HostState;
}

/// The `types` interface declares data only, so the host side is empty.
#[cfg(not(target_arch = "wasm32"))]
impl bindings::litz::mcu::types::Host for HostState {}

#[cfg(not(target_arch = "wasm32"))]
impl bindings::litz::mcu::host::Host for HostState {
    fn console(&mut self, bytes: Vec<u8>) {
        self.console.extend_from_slice(&bytes);
    }

    fn fault(&mut self, detail: String) {
        if self.fault.is_none() {
            self.fault = Some(detail);
        }
    }

    fn analog_read(&mut self, pin: u8) -> Option<u32> {
        HostState::analog_read(self, pin)
    }

    fn i2c_transfer(
        &mut self,
        controller: u8,
        address: u8,
        write: Vec<u8>,
        read_len: u32,
    ) -> Result<Vec<u8>, String> {
        self.i2c_transfer(controller, address, &write, read_len)
    }

    fn spi_transfer(
        &mut self,
        controller: u8,
        chip_select: u8,
        tx: Vec<u8>,
        read_len: u32,
    ) -> Result<Vec<u8>, String> {
        self.spi_transfer(controller, chip_select, &tx, read_len)
    }

    fn uart_write(&mut self, port: u8, bytes: Vec<u8>) -> bool {
        self.uart_write(port, &bytes)
    }

    fn uart_read(&mut self, port: u8) -> Vec<u8> {
        self.uart_read(port)
    }

    fn wifi_scan(&mut self) -> Vec<bindings::litz::mcu::types::WifiNetwork> {
        self.wifi_scan()
            .into_iter()
            .map(|network| bindings::litz::mcu::types::WifiNetwork {
                ssid: network.ssid,
                channel: network.channel,
                rssi: network.rssi,
                secure: network.secure,
                joined: network.joined,
            })
            .collect()
    }

    fn wifi_join(
        &mut self,
        ssid: String,
        psk: Option<String>,
    ) -> Result<bindings::litz::mcu::types::WifiLease, String> {
        let lease = self.wifi_join(&ssid, psk.as_deref())?;
        Ok(bindings::litz::mcu::types::WifiLease {
            ip: lease.ip,
            gateway: lease.gateway,
            dns: lease.dns,
        })
    }

    fn wifi_leave(&mut self) {
        self.wifi_leave();
    }

    fn wifi_resolve(&mut self, name: String) -> Result<Vec<u8>, String> {
        self.wifi_resolve(&name)
    }

    fn tcp_connect(&mut self, host: Vec<u8>, port: u16) -> Result<u32, String> {
        self.tcp_connect(&host, port)
    }

    fn tcp_write(&mut self, id: u32, bytes: Vec<u8>) -> Result<u32, String> {
        self.tcp_write(id, &bytes)
    }

    fn tcp_read(&mut self, id: u32, max: u32) -> Result<Vec<u8>, String> {
        self.tcp_read(id, max)
    }

    fn tcp_close(&mut self, id: u32) {
        self.tcp_close(id);
    }

    fn udp_open(&mut self, port: u16) -> Result<u32, String> {
        self.udp_open(port)
    }

    fn udp_send(
        &mut self,
        id: u32,
        host: Vec<u8>,
        port: u16,
        bytes: Vec<u8>,
    ) -> Result<u32, String> {
        self.udp_send(id, &host, port, &bytes)
    }

    fn udp_read(&mut self, id: u32, max: u32) -> Result<Vec<u8>, String> {
        self.udp_read(id, max)
    }

    fn udp_close(&mut self, id: u32) {
        self.udp_close(id);
    }

    fn ble_scan(&mut self) -> Vec<bindings::litz::mcu::types::BleAdvertisement> {
        self.ble_scan()
            .into_iter()
            .map(|beacon| bindings::litz::mcu::types::BleAdvertisement {
                address: beacon.address,
                name: beacon.name,
                rssi: beacon.rssi,
                data: beacon.data,
                scan_response: beacon.scan_response,
            })
            .collect()
    }

    fn ble_advertise(
        &mut self,
        address: Vec<u8>,
        data: Vec<u8>,
        scan_response: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        self.ble_advertise(&address, &data, &scan_response)
    }

    fn ble_stop_advertising(&mut self, address: Vec<u8>) {
        self.ble_stop_advertising(&address);
    }

    fn ble_set_gatt(
        &mut self,
        services: Vec<bindings::litz::mcu::types::GattService>,
    ) -> Result<(), String> {
        self.ble_set_gatt(services.into_iter().map(gatt_service_in).collect())
    }

    fn ble_connect(&mut self, address: Vec<u8>) -> Result<u16, String> {
        self.ble_connect(&address)
    }

    fn ble_disconnect(&mut self, handle: u16) {
        self.ble_disconnect(handle);
    }

    fn ble_discover(
        &mut self,
        handle: u16,
    ) -> Result<Vec<bindings::litz::mcu::types::GattService>, String> {
        Ok(self
            .ble_discover(handle)?
            .into_iter()
            .map(gatt_service_out)
            .collect())
    }

    fn ble_read(
        &mut self,
        handle: u16,
        service: u16,
        characteristic: u16,
    ) -> Result<Vec<u8>, String> {
        self.ble_read(handle, service, characteristic)
    }

    fn ble_write(
        &mut self,
        handle: u16,
        service: u16,
        characteristic: u16,
        value: Vec<u8>,
    ) -> Result<(), String> {
        self.ble_write(handle, service, characteristic, &value)
    }

    fn ble_subscribe(
        &mut self,
        handle: u16,
        service: u16,
        characteristic: u16,
        enabled: bool,
    ) -> Result<(), String> {
        self.ble_subscribe(handle, service, characteristic, enabled)
    }

    fn ble_notifications(
        &mut self,
        handle: u16,
    ) -> Vec<bindings::litz::mcu::types::GattNotification> {
        self.ble_notifications(handle)
            .into_iter()
            .map(
                |notification| bindings::litz::mcu::types::GattNotification {
                    service: notification.service,
                    characteristic: notification.characteristic,
                    value: notification.value,
                },
            )
            .collect()
    }

    fn ble_take_writes(&mut self) -> Vec<bindings::litz::mcu::types::GattWrite> {
        self.ble_take_writes()
            .into_iter()
            .map(|write| bindings::litz::mcu::types::GattWrite {
                service: write.service,
                characteristic: write.characteristic,
                value: write.value,
            })
            .collect()
    }

    fn ble_notify(
        &mut self,
        service: u16,
        characteristic: u16,
        value: Vec<u8>,
    ) -> Result<(), String> {
        self.ble_notify(service, characteristic, &value)
    }

    fn radio_configure(
        &mut self,
        network: String,
        channel: u16,
        address: Vec<u8>,
    ) -> Result<(), String> {
        self.radio_configure(&network, channel, &address)
    }

    fn radio_send(
        &mut self,
        bytes: Vec<u8>,
        to: Option<Vec<u8>>,
        airtime_ns: u64,
    ) -> Result<(), String> {
        self.radio_send(&bytes, to.as_deref(), airtime_ns)
    }

    fn radio_receive(&mut self) -> Vec<bindings::litz::mcu::types::RadioFrame> {
        self.radio_receive()
            .into_iter()
            .map(|frame| bindings::litz::mcu::types::RadioFrame {
                sender: frame.sender,
                to: frame.to,
                bytes: frame.bytes,
                sent_at: frame.sent_at,
                airtime: frame.airtime,
            })
            .collect()
    }
}

/// WIT table → the crate's table.
#[cfg(not(target_arch = "wasm32"))]
fn gatt_service_in(service: bindings::litz::mcu::types::GattService) -> GattService {
    GattService {
        uuid: service.uuid,
        characteristics: service
            .characteristics
            .into_iter()
            .map(|characteristic| GattCharacteristic {
                uuid: characteristic.uuid,
                access: characteristic.access,
                value: characteristic.value,
            })
            .collect(),
    }
}

/// The crate's table → the WIT table.
#[cfg(not(target_arch = "wasm32"))]
fn gatt_service_out(service: GattService) -> bindings::litz::mcu::types::GattService {
    bindings::litz::mcu::types::GattService {
        uuid: service.uuid,
        characteristics: service
            .characteristics
            .into_iter()
            .map(
                |characteristic| bindings::litz::mcu::types::GattCharacteristic {
                    uuid: characteristic.uuid,
                    access: characteristic.access,
                    value: characteristic.value,
                },
            )
            .collect(),
    }
}

/// A loaded chip module, ready to drive a board.
#[cfg(not(target_arch = "wasm32"))]
pub struct WasmChip {
    store: Store<HostState>,
    instance: Simulator,
    board: Option<Rc<RefCell<Board>>>,
    /// Which development-board instance on the diagram these pads belong to.
    board_index: u16,
    cpu_hz: u64,
    flash: FlashLayout,
    name: String,
    cycles: u64,
    /// Host actions waiting for their time, sorted by `at_ns`.
    pending: Vec<(u64, Input)>,
    last_conflicts: Vec<Conflict>,
    /// The guest's answer to `debug.available`, asked once at load.
    debug_available: bool,
    /// Whether a debugger has asked about this chip. Until it has, a run does
    /// not spend two extra guest calls keeping a register snapshot warm.
    debug_active: Cell<bool>,
    /// The stopped core's file, cached because `Chip::debug_pc` and
    /// `Chip::debug_register_file` take `&self` and a wasmtime call does not.
    last_pc: u32,
    last_registers: [u32; 33],
    /// Whether the board supplies this chip. A chip with no board is always on.
    powered: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl WasmChip {
    /// Load a component from a file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_bytes(&bytes, &path.display().to_string())
    }

    /// Load a component already in memory (a builtin board carries its module
    /// inside the binary), naming it in any error.
    pub fn from_bytes(bytes: &[u8], label: &str) -> Result<Self, String> {
        Self::from_bytes_with_abi(bytes, label, litz_mcu_wit::ABI)
    }

    /// Load a component that speaks `abi`, which a board manifest names.
    ///
    /// Only the unified contract is understood. A manifest that still names
    /// the retired 0.1 world fails here with an explanation rather than
    /// against a linker that no longer exists.
    pub fn from_bytes_with_abi(bytes: &[u8], label: &str, abi: &str) -> Result<Self, String> {
        match abi {
            "litz-mcu-1" | "litz-mcu-0.1" | "0.1" => {
                return Err(format!(
                    "{label}: ABI {abi:?} is retired. The 0.1 contract was removed; \
                     rebuild the guest against litz:mcu@0.2.0 and set abi = \"litz-mcu-2\""
                ));
            }
            "litz-mcu-2" | "litz-mcu-0.2" | "0.2" => {}
            other => {
                return Err(format!(
                    "{label}: ABI {other:?} is not one this build understands; \
                     use litz-mcu-2"
                ));
            }
        }

        let mut config = Config::new();
        // The component model is what makes the WIT boundary possible; fuel is
        // how a runaway guest is stopped rather than trusted.
        config.wasm_component_model(true);
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| format!("{label}: {e}"))?;
        let component =
            Component::from_binary(&engine, bytes).map_err(|e| format!("{label}: {e}"))?;

        let mut state = HostState::new();
        state.limits = StoreLimitsBuilder::new().memory_size(MAX_MEMORY).build();
        let mut store = Store::new(&engine, state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(FUEL_PER_CALL).map_err(|e| e.to_string())?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        Simulator::add_to_linker::<HostState, HostState>(&mut linker, |state| state)
            .map_err(|e| e.to_string())?;
        let instance = Simulator::instantiate(&mut store, &component, &linker)
            .map_err(|e| format!("{label}: {e}"))?;
        let info = instance
            .litz_mcu_chip()
            .call_info(&mut store)
            .map_err(|e| format!("{label}: info(): {e}"))?;
        let flash = FlashLayout {
            size: info.flash.size,
            bootloader: info.flash.bootloader,
            partition_table: info.flash.partition_table,
            app: info.flash.app,
        };
        // The debug interface is a required export, so instantiation above
        // already proved it exists. Ask whether the core can answer it now,
        // rather than on every browser probe.
        let debug_available = instance
            .litz_mcu_debug()
            .call_available(&mut store)
            .unwrap_or(false);

        Ok(Self {
            store,
            instance,
            board: None,
            cpu_hz: info.cpu_hz.max(1),
            flash,
            name: info.name,
            cycles: 0,
            board_index: 0,
            pending: Vec::new(),
            last_conflicts: Vec::new(),
            debug_available,
            debug_active: Cell::new(false),
            last_pc: 0,
            last_registers: [0; 33],
            powered: true,
        })
    }

    /// Attach the flattened board. Same contract as the built-in backend: the
    /// chip only ever sees pin levels.
    pub fn with_board(self, board: Board) -> Self {
        self.with_shared_board(Rc::new(RefCell::new(board)), 0)
    }

    /// Attach a board another chip already shares, and say which board
    /// instance these pads belong to.
    pub fn with_shared_board(mut self, board: Rc<RefCell<Board>>, index: u16) -> Self {
        self.board = Some(Rc::clone(&board));
        self.board_index = index;
        let host = self.store.data_mut();
        host.board = Some(board);
        host.board_index = index;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Microseconds of device time, for callers that need a finer clock than
    /// `seconds()`.
    fn now_ns(&self) -> u64 {
        self.cycles.saturating_mul(1_000_000_000) / self.cpu_hz
    }

    /// Deliver every queued host action whose time has come.
    fn pump_inputs(&mut self) -> Result<(), String> {
        let now = self.now_ns();
        let mut gpio = Vec::new();
        let mut uart: Vec<Vec<u8>> = Vec::new();
        let mut due = 0;
        while let Some((at, input)) = self.pending.get(due) {
            if *at > now {
                break;
            }
            match input {
                Input::Gpio { pin, level, .. } => gpio.push(PinInput {
                    pin: *pin,
                    level: wit_level(*level),
                }),
                Input::Uart { bytes, .. } => uart.push(bytes.clone()),
            }
            due += 1;
        }
        self.pending.drain(..due);
        if !gpio.is_empty() {
            self.call_set_inputs(&gpio)?;
        }
        for bytes in uart {
            self.console_input(&bytes)?;
        }
        Ok(())
    }

    /// Bytes for the guest's console receive path.
    fn console_input(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.call_write_console(bytes)
    }

    /// The guest's own report, if it made one.
    pub fn fault(&self) -> Option<String> {
        self.store.data().fault.clone()
    }

    fn call_set_inputs(&mut self, inputs: &[PinInput]) -> Result<(), String> {
        self.instance
            .litz_mcu_chip()
            .call_set_inputs(&mut self.store, inputs)
            .map_err(|e| e.to_string())
    }

    fn call_write_console(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.instance
            .litz_mcu_chip()
            .call_write_console(&mut self.store, bytes)
            .map_err(|e| e.to_string())
    }

    fn call_load_image(
        &mut self,
        kind: ImageKind,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.instance
            .litz_mcu_chip()
            .call_load_image(&mut self.store, wit_image(kind), offset, bytes)
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("load-image: {e}"))
    }

    fn call_boot(&mut self) -> Result<u32, String> {
        self.instance
            .litz_mcu_chip()
            .call_boot(&mut self.store)
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("boot: {e}"))
    }

    /// Reset the guest core. `mac` is the station address a network-capable
    /// chip keeps across a reset; empty means the host has no opinion.
    fn call_reset(&mut self, mac: &[u8]) -> Result<(), String> {
        self.instance
            .litz_mcu_chip()
            .call_reset(&mut self.store, mac)
            .map_err(|e| e.to_string())
    }

    /// Whether the board supplies this chip.
    fn board_power(&self) -> bool {
        match &self.board {
            Some(board) => board.borrow().board_powered(self.board_index),
            None => true,
        }
    }

    /// Returns the cycles the guest reported and why it stopped.
    fn call_run(&mut self, budget_cycles: u64) -> Result<(u64, Stop), String> {
        let result = self
            .instance
            .litz_mcu_chip()
            .call_run(&mut self.store, budget_cycles)
            .map_err(|e| e.to_string())?;
        Ok((result.cycles, wit_stop(result.stop)))
    }

    /// Pads the guest reports as changed, with levels in the board's terms.
    fn call_take_outputs(&mut self) -> Result<Vec<(u8, Level, bool)>, String> {
        self.instance
            .litz_mcu_chip()
            .call_take_outputs(&mut self.store)
            .map(|outputs| {
                outputs
                    .into_iter()
                    .map(|output| (output.pin, our_level(output.level), output.enabled))
                    .collect()
            })
            .map_err(|e| e.to_string())
    }

    // ------------------------------------------------------------- debugging

    fn call_debug_pc(&mut self) -> Result<u32, String> {
        self.instance
            .litz_mcu_debug()
            .call_pc(&mut self.store)
            .map_err(|e| e.to_string())
    }

    fn call_debug_registers(&mut self) -> Result<(Vec<WitRegister>, u32), String> {
        let registers = self
            .instance
            .litz_mcu_debug()
            .call_registers(&mut self.store)
            .map_err(|e| e.to_string())?;
        let pc = self.call_debug_pc()?;
        Ok((registers, pc))
    }

    fn call_debug_read(&mut self, addr: u32, len: usize) -> Result<Vec<u8>, String> {
        let len = u32::try_from(len).unwrap_or(u32::MAX);
        self.instance
            .litz_mcu_debug()
            .call_read(&mut self.store, addr, len)
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("read: {e}"))
    }

    fn call_debug_disassemble(&mut self, addr: u32, count: usize) -> Result<String, String> {
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        self.instance
            .litz_mcu_debug()
            .call_disassemble(&mut self.store, addr, count)
            .map_err(|e| e.to_string())
    }

    fn call_debug_set_breakpoints(&mut self, pcs: &[u32]) -> Result<(), String> {
        self.instance
            .litz_mcu_debug()
            .call_set_breakpoints(&mut self.store, pcs)
            .map_err(|e| e.to_string())
    }

    fn call_debug_step(&mut self) -> Result<(u64, Stop), String> {
        let result = self
            .instance
            .litz_mcu_debug()
            .call_step(&mut self.store)
            .map_err(|e| e.to_string())?;
        Ok((result.cycles, wit_stop(result.stop)))
    }

    /// Fold the guest's report into the host: advance the cycle clock, let the
    /// board run to the new time, and apply the pads the guest reports. Shared
    /// by `run` and `debug_step` so a debugger step moves time exactly as a
    /// run does.
    fn absorb(&mut self, cycles: u64, reported: Stop) -> RunResult {
        self.cycles = self.cycles.saturating_add(cycles);

        let now_ns = self.now_ns();
        let outputs = self.call_take_outputs().unwrap_or_default();
        let mut conflicts = Vec::new();
        if let Some(board) = &self.board {
            let mut board = board.borrow_mut();
            // The board's clock follows the guest, as it does for the AVR and
            // ESP adapters: a part with scheduled work runs before the guest's
            // new pin levels are applied, and host calls that share a medium
            // (the radio) see a clock that has moved.
            board.advance_to(now_ns);
            for (pin, level, enabled) in outputs {
                let drive = if enabled {
                    match level {
                        Level::Low => Drive::strong(Level::Low),
                        Level::High => Drive::strong(Level::High),
                        Level::HighZ => Drive::HIGH_Z,
                    }
                } else {
                    Drive::HIGH_Z
                };
                board.set_chip_output_on(self.board_index, pin, drive, now_ns);
            }
            conflicts = board.conflicts().to_vec();
        }
        self.last_conflicts = conflicts;

        let stop = if self.store.data().fault.is_some() {
            Stop::Trap
        } else {
            reported
        };
        if self.debug_active.get() {
            self.refresh_debug_cache();
        }
        RunResult { cycles, stop }
    }

    /// Ask the guest where it is and what its registers hold. Called after a
    /// run or a step once a debugger has shown interest; the values are served
    /// from the cache to the `&self` half of the `Chip` debug surface.
    fn refresh_debug_cache(&mut self) {
        if let Ok((registers, pc)) = self.call_debug_registers() {
            self.last_pc = pc;
            self.last_registers = register_file(&registers, pc);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn wit_level(level: Level) -> WitLevel {
    match level {
        Level::Low => WitLevel::Low,
        Level::High => WitLevel::High,
        Level::HighZ => WitLevel::Floating,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn our_level(level: WitLevel) -> Level {
    match level {
        WitLevel::Low => Level::Low,
        WitLevel::High => Level::High,
        WitLevel::Floating => Level::HighZ,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn wit_stop(stop: WitStop) -> Stop {
    match stop {
        WitStop::Budget => Stop::Budget,
        WitStop::Idle => Stop::Idle,
        WitStop::Halted => Stop::Halted,
        WitStop::Trap => Stop::Trap,
        WitStop::Reset => Stop::Reset,
        WitStop::Breakpoint(pc) => Stop::Breakpoint(pc),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn wit_image(kind: ImageKind) -> Image {
    match kind {
        ImageKind::Rom => Image::Rom,
        ImageKind::Bootloader => Image::Bootloader,
        ImageKind::PartitionTable => Image::PartitionTable,
        ImageKind::Firmware => Image::Firmware,
    }
}

/// Map the guest's named registers onto the host's fixed file.
///
/// The host debug surface is `[u32; 33]`: 32 integer registers then the pc.
/// A guest that names its registers the RISC-V way (the reference core does)
/// lands each one in its canonical slot; any other guest is read in list
/// order, and a short list leaves the tail zeroed. Positional fallback is
/// deliberate rather than clever: a debugger showing the wrong names would be
/// worse than showing none.
/// The RISC-V integer register names, in file order.
#[cfg(not(target_arch = "wasm32"))]
const RISCV_REGISTERS: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];

#[cfg(not(target_arch = "wasm32"))]
fn register_file(registers: &[WitRegister], pc: u32) -> [u32; 33] {
    let named = registers
        .iter()
        .any(|register| RISCV_REGISTERS.contains(&register.name.as_str()));
    let mut file = [0u32; 33];
    if named {
        for (index, want) in RISCV_REGISTERS.iter().enumerate() {
            if let Some(register) = registers
                .iter()
                .find(|register| register.name.as_str() == *want)
            {
                file[index] = register.value;
            }
        }
    } else {
        for (index, register) in registers.iter().take(32).enumerate() {
            file[index] = register.value;
        }
    }
    file[32] = pc;
    file
}

#[cfg(not(target_arch = "wasm32"))]
impl Chip for WasmChip {
    fn info(&self) -> ChipInfo {
        ChipInfo {
            name: self.name.clone(),
            cpu_hz: self.cpu_hz,
            flash: self.flash,
        }
    }

    fn load_image(&mut self, kind: ImageKind, offset: u64, bytes: &[u8]) -> Result<(), String> {
        self.call_load_image(kind, offset, bytes)
    }

    fn boot(&mut self) -> Result<u32, String> {
        self.powered = self.board_power();
        if !self.powered {
            // The board is off; the guest boots when power arrives.
            return Ok(0);
        }
        let entry = self.call_boot()?;
        // One snapshot at boot means a debugger that asks for the pc before
        // its first run sees the entry point rather than zero.
        if self.debug_available {
            self.refresh_debug_cache();
        }
        Ok(entry)
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
        if !self.board_power() {
            if self.powered {
                self.powered = false;
                // Stop the guest from holding any pad, and give it the reset
                // its power loss is.
                if let Err(e) = self.call_reset(&[]) {
                    self.store.data_mut().fault.get_or_insert(e);
                }
                if let Some(board) = &self.board {
                    let now = (self.cycles as u128 * 1_000_000_000 / u128::from(self.cpu_hz.max(1)))
                        as u64;
                    board
                        .borrow_mut()
                        .release_chip_outputs(self.board_index, now);
                }
            }
            // Time passes on the board that is still on; the guest's clock is
            // the adapter's, so it moves even though no code runs.
            self.cycles = self.cycles.saturating_add(budget_cycles);
            return RunResult {
                cycles: budget_cycles,
                stop: Stop::Idle,
            };
        }
        if !self.powered {
            // Power arrived: reset, then start the firmware from its entry.
            if let Err(e) = self.call_reset(&[]) {
                self.store.data_mut().fault.get_or_insert(e);
                return RunResult {
                    cycles: 0,
                    stop: Stop::Trap,
                };
            }
            let entry = match self.call_boot() {
                Ok(entry) => entry,
                Err(e) => {
                    self.store.data_mut().fault.get_or_insert(e);
                    return RunResult {
                        cycles: 0,
                        stop: Stop::Trap,
                    };
                }
            };
            self.powered = true;
            let _ = entry;
            if self.debug_available {
                self.refresh_debug_cache();
            }
        }
        if let Err(message) = self.pump_inputs() {
            self.store.data_mut().fault.get_or_insert(message);
            return RunResult {
                cycles: 0,
                stop: Stop::Trap,
            };
        }

        // Pin levels the board resolved since the last run. The chip never
        // asks what is wired to it; it is told.
        let changed: Vec<PinInput> = match &self.board {
            Some(board) => board
                .borrow_mut()
                .take_chip_inputs_on(self.board_index)
                .into_iter()
                .map(|(pin, level)| PinInput {
                    pin,
                    level: wit_level(level),
                })
                .collect(),
            None => Vec::new(),
        };
        if !changed.is_empty()
            && let Err(e) = self.call_set_inputs(&changed)
        {
            self.store.data_mut().fault = Some(e);
            return RunResult {
                cycles: 0,
                stop: Stop::Trap,
            };
        }

        let _ = self.store.set_fuel(FUEL_PER_CALL);
        // Host calls during the run stamp their bus traffic with this tick.
        // A previous run's host calls may have spent more wire time than the
        // guest reported cycles, so the host clock is kept monotonic.
        let host_now = self.store.data().now_ns.max(self.now_ns());
        self.store.data_mut().now_ns = host_now;
        let (cycles, reported) = match self.call_run(budget_cycles) {
            Ok(result) => result,
            Err(e) => {
                // A trap is a fault like any other: report it and stop, rather
                // than letting it escape as a host panic.
                self.store.data_mut().fault.get_or_insert(e);
                return RunResult {
                    cycles: 0,
                    stop: Stop::Trap,
                };
            }
        };
        self.absorb(cycles, reported)
    }

    fn seconds(&self) -> f64 {
        self.cycles as f64 / self.cpu_hz as f64
    }

    fn write_console(&mut self, bytes: &[u8]) {
        if let Err(e) = self.console_input(bytes) {
            self.store.data_mut().fault.get_or_insert(e);
        }
    }

    fn take_console(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.store.data_mut().console)
    }

    fn state(&self, part: &str) -> Option<PartState> {
        self.board.as_ref()?.borrow().state(part)
    }

    fn press(&mut self, part: &str, pressed: bool) -> bool {
        let Some(board) = self.board.as_ref() else {
            return false;
        };
        let now = self.now_ns();
        board.borrow_mut().press(part, pressed, now)
    }

    fn set_control(&mut self, part: &str, value: u16) -> bool {
        let Some(board) = self.board.as_ref() else {
            return false;
        };
        let now = self.now_ns();
        board.borrow_mut().set_control(part, value, now)
    }

    fn set_named_control(&mut self, part: &str, name: &str, value: &str) -> bool {
        let Some(board) = self.board.as_ref() else {
            return false;
        };
        let now = self.now_ns();
        board.borrow_mut().set_named_control(part, name, value, now)
    }

    fn save_part_display(&mut self, part: &str, path: &std::path::Path) -> Result<(), String> {
        let Some(board) = &self.board else {
            return Err("this chip is not attached to a board".into());
        };
        let image = board
            .borrow()
            .part_framebuffer(part)
            .ok_or_else(|| format!("part {part:?} is not a display with a framebuffer"))?;
        std::fs::write(path, image.to_png()).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn part_pin_level(&self, part: &str, pin: Option<&str>) -> Option<Level> {
        let board = self.board.as_ref()?.borrow();
        match pin {
            Some(pin) => board.part_pin_level(part, pin),
            None => board.chip_pin_level_on(self.board_index, part),
        }
    }

    fn gpio_level(&self, gpio: u8) -> Option<Level> {
        let board = self.board.as_ref()?.borrow();
        board.chip_level_on(self.board_index, gpio)
    }

    fn conflicts(&self) -> Vec<Conflict> {
        self.last_conflicts.clone()
    }

    fn fault(&self) -> Option<String> {
        WasmChip::fault(self)
    }

    fn start_trace(&mut self) {
        if let Some(board) = self.board.as_ref() {
            board.borrow_mut().start_trace();
        }
    }

    fn vcd(&self) -> String {
        self.board
            .as_ref()
            .map(|board| board.borrow().vcd())
            .unwrap_or_default()
    }

    fn break_at(&mut self, pc: u32) {
        self.debug_active.set(true);
        if let Err(e) = self.call_debug_set_breakpoints(&[pc]) {
            self.store.data_mut().fault.get_or_insert(e);
        }
    }

    fn registers(&self) -> String {
        let mut out = String::new();
        for (name, value) in RISCV_REGISTERS.iter().zip(&self.last_registers) {
            out.push_str(&format!("{name:>4} {value:#010x}\n"));
        }
        out.push_str(&format!("  pc {:#010x}\n", self.last_pc));
        out
    }

    fn debug_available(&self) -> bool {
        // A browser or the CLI asks this before anything else, which is the
        // earliest signal that a run should keep its register snapshot fresh.
        self.debug_active.set(true);
        self.debug_available
    }

    fn debug_pc(&self) -> Option<u32> {
        self.debug_active.set(true);
        Some(self.last_pc)
    }

    fn debug_register_file(&self) -> Option<[u32; 33]> {
        self.debug_active.set(true);
        Some(self.last_registers)
    }

    fn debug_read_memory(&mut self, addr: u32, len: usize) -> Vec<u8> {
        self.debug_active.set(true);
        self.call_debug_read(addr, len).unwrap_or_default()
    }

    fn debug_disassemble(&mut self, addr: u32, count: usize) -> String {
        self.debug_active.set(true);
        self.call_debug_disassemble(addr, count).unwrap_or_default()
    }

    fn debug_set_breakpoints(&mut self, pcs: &[u32]) {
        self.debug_active.set(true);
        if let Err(e) = self.call_debug_set_breakpoints(pcs) {
            self.store.data_mut().fault.get_or_insert(e);
        }
    }

    fn debug_step(&mut self) -> Option<Stop> {
        self.debug_active.set(true);
        let _ = self.store.set_fuel(FUEL_PER_CALL);
        // A step advances simulated time like a run, so the board and the
        // host clock must move with it.
        let host_now = self.store.data().now_ns.max(self.now_ns());
        self.store.data_mut().now_ns = host_now;
        match self.call_debug_step() {
            Ok((cycles, reported)) => Some(self.absorb(cycles, reported).stop),
            Err(e) => {
                self.store.data_mut().fault.get_or_insert(e);
                Some(Stop::Trap)
            }
        }
    }
}
