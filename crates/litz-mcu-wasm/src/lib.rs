//! A microcontroller that is a WebAssembly component.
//!
//! Two contracts are loadable. `litz:mcu@0.1.0`
//! (`crates/litz-mcu-wit/wit`) is pins and console: a module from a pull
//! request can emit serial bytes and report a fault, and nothing else.
//! `litz:mcu@0.2.0` (`crates/litz-mcu-wit/wit-v2`) adds the calls a real MCU
//! peripheral block needs — analog reads, I2C/SPI/UART transactions, Wi-Fi
//! with TCP/UDP, and BLE scanning and advertising. A board's `board.toml` names the `.wasm`
//! file and the `abi` it speaks; the runner loads it here, and everything above
//! this crate — nets, parts, tests — cannot tell it apart from the built-in
//! ESP32-C3 backend.
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
#[cfg(not(target_arch = "wasm32"))]
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "../litz-mcu-wit/wit",
        world: "simulator",
    });
}

/// The 0.2 world. It is a different package version, so it is a different
/// component type: a 0.1 module and a 0.2 module cannot share a linker, and
/// the manifest's `abi` says which one to build.
#[cfg(not(target_arch = "wasm32"))]
pub mod bindings_v2 {
    wasmtime::component::bindgen!({
        path: "../litz-mcu-wit/wit-v2",
        world: "simulator",
    });
}

/// The host answers a 0.2 guest asks for. Shared by the native loader and the
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
use bindings::litz::mcu::types::{Image, Level as WitLevel, PinInput, Stop as WitStop};
#[cfg(not(target_arch = "wasm32"))]
use litz_board::chip::{Chip, ChipInfo, FlashLayout, ImageKind, Input, RunResult, Stop};
#[cfg(not(target_arch = "wasm32"))]
use litz_board::{Board, Conflict};
#[cfg(not(target_arch = "wasm32"))]
use litz_core::{Drive, Level, PartState};
#[cfg(not(target_arch = "wasm32"))]
use std::cell::RefCell;
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
}

#[cfg(not(target_arch = "wasm32"))]
impl bindings_v2::litz::mcu::types::Host for HostState {}

#[cfg(not(target_arch = "wasm32"))]
impl bindings_v2::litz::mcu::host::Host for HostState {
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

    fn wifi_scan(&mut self) -> Vec<bindings_v2::litz::mcu::types::WifiNetwork> {
        self.wifi_scan()
            .into_iter()
            .map(|network| bindings_v2::litz::mcu::types::WifiNetwork {
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
    ) -> Result<bindings_v2::litz::mcu::types::WifiLease, String> {
        let lease = self.wifi_join(&ssid, psk.as_deref())?;
        Ok(bindings_v2::litz::mcu::types::WifiLease {
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

    fn ble_scan(&mut self) -> Vec<bindings_v2::litz::mcu::types::BleAdvertisement> {
        self.ble_scan()
            .into_iter()
            .map(|beacon| bindings_v2::litz::mcu::types::BleAdvertisement {
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
        services: Vec<bindings_v2::litz::mcu::types::GattService>,
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
    ) -> Result<Vec<bindings_v2::litz::mcu::types::GattService>, String> {
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
    ) -> Vec<bindings_v2::litz::mcu::types::GattNotification> {
        self.ble_notifications(handle)
            .into_iter()
            .map(
                |notification| bindings_v2::litz::mcu::types::GattNotification {
                    service: notification.service,
                    characteristic: notification.characteristic,
                    value: notification.value,
                },
            )
            .collect()
    }

    fn ble_take_writes(&mut self) -> Vec<bindings_v2::litz::mcu::types::GattWrite> {
        self.ble_take_writes()
            .into_iter()
            .map(|write| bindings_v2::litz::mcu::types::GattWrite {
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

    fn radio_receive(&mut self) -> Vec<bindings_v2::litz::mcu::types::RadioFrame> {
        self.radio_receive()
            .into_iter()
            .map(|frame| bindings_v2::litz::mcu::types::RadioFrame {
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
fn gatt_service_in(service: bindings_v2::litz::mcu::types::GattService) -> GattService {
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
fn gatt_service_out(service: GattService) -> bindings_v2::litz::mcu::types::GattService {
    bindings_v2::litz::mcu::types::GattService {
        uuid: service.uuid,
        characteristics: service
            .characteristics
            .into_iter()
            .map(
                |characteristic| bindings_v2::litz::mcu::types::GattCharacteristic {
                    uuid: characteristic.uuid,
                    access: characteristic.access,
                    value: characteristic.value,
                },
            )
            .collect(),
    }
}

/// Which contract version a loaded component speaks. A 0.1 module has only
/// pins and console bytes; a 0.2 module also has the peripheral and wireless
/// host calls.
#[cfg(not(target_arch = "wasm32"))]
enum Instance {
    V1(Simulator),
    V2(bindings_v2::Simulator),
}

/// A loaded chip module, ready to drive a board.
#[cfg(not(target_arch = "wasm32"))]
pub struct WasmChip {
    store: Store<HostState>,
    instance: Instance,
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
}

#[cfg(not(target_arch = "wasm32"))]
impl WasmChip {
    /// Load a component from a file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_bytes(&bytes, &path.display().to_string())
    }

    /// Load a component already in memory (a builtin board carries its module
    /// inside the binary), naming it in any error. The default contract is the
    /// one every existing board used.
    pub fn from_bytes(bytes: &[u8], label: &str) -> Result<Self, String> {
        Self::from_bytes_with_abi(bytes, label, "litz-mcu-1")
    }

    /// Load a component that speaks `abi`, which a board manifest names.
    pub fn from_bytes_with_abi(bytes: &[u8], label: &str, abi: &str) -> Result<Self, String> {
        let v2 = match abi {
            "litz-mcu-1" | "litz-mcu-0.1" | "0.1" => false,
            "litz-mcu-2" | "litz-mcu-0.2" | "0.2" => true,
            other => {
                return Err(format!(
                    "{label}: ABI {other:?} is not one this build understands; \
                     use litz-mcu-1 or litz-mcu-2"
                ));
            }
        };

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

        let (instance, name, cpu_hz, flash) = if v2 {
            let mut linker: Linker<HostState> = Linker::new(&engine);
            bindings_v2::Simulator::add_to_linker::<HostState, HostState>(&mut linker, |state| {
                state
            })
            .map_err(|e| e.to_string())?;
            let instance = bindings_v2::Simulator::instantiate(&mut store, &component, &linker)
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
            (Instance::V2(instance), info.name, info.cpu_hz, flash)
        } else {
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
            (Instance::V1(instance), info.name, info.cpu_hz, flash)
        };

        Ok(Self {
            store,
            instance,
            board: None,
            cpu_hz: cpu_hz.max(1),
            flash,
            name,
            cycles: 0,
            board_index: 0,
            pending: Vec::new(),
            last_conflicts: Vec::new(),
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

    // The two contract versions have different generated types, so each call
    // the board makes is a two-arm match that converts on the way in and out.

    fn call_set_inputs(&mut self, inputs: &[PinInput]) -> Result<(), String> {
        match &self.instance {
            Instance::V1(instance) => instance
                .litz_mcu_chip()
                .call_set_inputs(&mut self.store, inputs)
                .map_err(|e| e.to_string()),
            Instance::V2(instance) => {
                let inputs: Vec<bindings_v2::litz::mcu::types::PinInput> = inputs
                    .iter()
                    .map(|input| bindings_v2::litz::mcu::types::PinInput {
                        pin: input.pin,
                        level: wit_level_v2(input.level),
                    })
                    .collect();
                instance
                    .litz_mcu_chip()
                    .call_set_inputs(&mut self.store, &inputs)
                    .map_err(|e| e.to_string())
            }
        }
    }

    fn call_write_console(&mut self, bytes: &[u8]) -> Result<(), String> {
        match &self.instance {
            Instance::V1(instance) => instance
                .litz_mcu_chip()
                .call_write_console(&mut self.store, bytes)
                .map_err(|e| e.to_string()),
            Instance::V2(instance) => instance
                .litz_mcu_chip()
                .call_write_console(&mut self.store, bytes)
                .map_err(|e| e.to_string()),
        }
    }

    fn call_load_image(
        &mut self,
        kind: ImageKind,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), String> {
        let outcome = match &self.instance {
            Instance::V1(instance) => instance.litz_mcu_chip().call_load_image(
                &mut self.store,
                wit_image(kind),
                offset,
                bytes,
            ),
            Instance::V2(instance) => instance.litz_mcu_chip().call_load_image(
                &mut self.store,
                wit_image_v2(kind),
                offset,
                bytes,
            ),
        };
        outcome
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("load-image: {e}"))
    }

    fn call_boot(&mut self) -> Result<u32, String> {
        let outcome = match &self.instance {
            Instance::V1(instance) => instance.litz_mcu_chip().call_boot(&mut self.store),
            Instance::V2(instance) => instance.litz_mcu_chip().call_boot(&mut self.store),
        };
        outcome
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("boot: {e}"))
    }

    /// Returns the cycles the guest reported and why it stopped.
    fn call_run(&mut self, budget_cycles: u64) -> Result<(u64, Stop), String> {
        match &self.instance {
            Instance::V1(instance) => {
                let result = instance
                    .litz_mcu_chip()
                    .call_run(&mut self.store, budget_cycles)
                    .map_err(|e| e.to_string())?;
                Ok((result.cycles, wit_stop(result.stop)))
            }
            Instance::V2(instance) => {
                let result = instance
                    .litz_mcu_chip()
                    .call_run(&mut self.store, budget_cycles)
                    .map_err(|e| e.to_string())?;
                Ok((result.cycles, wit_stop_v2(result.stop)))
            }
        }
    }

    /// Pads the guest reports as changed, with levels in the board's terms.
    fn call_take_outputs(&mut self) -> Result<Vec<(u8, Level, bool)>, String> {
        match &self.instance {
            Instance::V1(instance) => instance
                .litz_mcu_chip()
                .call_take_outputs(&mut self.store)
                .map(|outputs| {
                    outputs
                        .into_iter()
                        .map(|output| (output.pin, our_level(output.level), output.enabled))
                        .collect()
                })
                .map_err(|e| e.to_string()),
            Instance::V2(instance) => instance
                .litz_mcu_chip()
                .call_take_outputs(&mut self.store)
                .map(|outputs| {
                    outputs
                        .into_iter()
                        .map(|output| (output.pin, our_level_v2(output.level), output.enabled))
                        .collect()
                })
                .map_err(|e| e.to_string()),
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

#[cfg(not(target_arch = "wasm32"))]
fn wit_level_v2(level: WitLevel) -> bindings_v2::litz::mcu::types::Level {
    match level {
        WitLevel::Low => bindings_v2::litz::mcu::types::Level::Low,
        WitLevel::High => bindings_v2::litz::mcu::types::Level::High,
        WitLevel::Floating => bindings_v2::litz::mcu::types::Level::Floating,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn our_level_v2(level: bindings_v2::litz::mcu::types::Level) -> Level {
    match level {
        bindings_v2::litz::mcu::types::Level::Low => Level::Low,
        bindings_v2::litz::mcu::types::Level::High => Level::High,
        bindings_v2::litz::mcu::types::Level::Floating => Level::HighZ,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn wit_stop_v2(stop: bindings_v2::litz::mcu::types::Stop) -> Stop {
    use bindings_v2::litz::mcu::types::Stop;
    match stop {
        Stop::Budget => self::Stop::Budget,
        Stop::Idle => self::Stop::Idle,
        Stop::Halted => self::Stop::Halted,
        Stop::Trap => self::Stop::Trap,
        Stop::Reset => self::Stop::Reset,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn wit_image_v2(kind: ImageKind) -> bindings_v2::litz::mcu::types::Image {
    use bindings_v2::litz::mcu::types::Image;
    match kind {
        ImageKind::Rom => Image::Rom,
        ImageKind::Bootloader => Image::Bootloader,
        ImageKind::PartitionTable => Image::PartitionTable,
        ImageKind::Firmware => Image::Firmware,
    }
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
        self.call_boot()
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
        RunResult { cycles, stop }
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
}
