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
// generated names cannot collide with this crate's.
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "../litz-mcu-wit/wit",
        world: "simulator",
    });
}

/// The 0.2 world. It is a different package version, so it is a different
/// component type: a 0.1 module and a 0.2 module cannot share a linker, and
/// the manifest's `abi` says which one to build.
pub mod bindings_v2 {
    wasmtime::component::bindgen!({
        path: "../litz-mcu-wit/wit-v2",
        world: "simulator",
    });
}

use bindings::Simulator;
use bindings::litz::mcu::types::{Image, Level as WitLevel, PinInput, Stop as WitStop};
use litz_board::chip::{Chip, ChipInfo, FlashLayout, ImageKind, Input, RunResult, Stop};
use litz_board::{Board, Conflict};
use litz_core::{Drive, Level, PartState};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

/// Guest memory ceiling. A chip is a few megabytes at most; anything past
/// this is a bug or an attack, and either way the host should stop it.
const MAX_MEMORY: usize = 256 << 20;
/// Fuel granted per component call. One fuel unit is roughly one instruction;
/// a call only ever runs `budget` cycles, so this is a generous ceiling that
/// still catches a guest that ignores its budget.
const FUEL_PER_CALL: u64 = 1 << 34;

/// What the host provides to a chip module.
struct HostState {
    console: Vec<u8>,
    fault: Option<String>,
    limits: StoreLimits,
    /// The board this chip is wired to, for the 0.2 peripheral and wireless
    /// calls. `None` before `with_shared_board`, and for a bare chip.
    board: Option<Rc<RefCell<Board>>>,
    board_index: u16,
    /// The tick the current `run` call started at. Host calls made during the
    /// run use it for bus timestamps; the guest's `cycles` moves only after
    /// the call returns.
    now_ns: u64,
    /// The deterministic network behind the circuit's access point, built
    /// lazily when the guest first asks about Wi-Fi.
    router: Option<litz_net::Router>,
    joined: Option<String>,
    /// Flow ids the router handed out, so an unknown id is an error rather
    /// than an empty read.
    flows: std::collections::BTreeSet<u32>,
}

/// The store's data *is* the host interface: one type, no projection.
impl wasmtime::component::HasData for HostState {
    type Data<'a> = &'a mut HostState;
}

/// The `types` interface declares data only, so the host side is empty.
impl bindings::litz::mcu::types::Host for HostState {}

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

impl bindings_v2::litz::mcu::types::Host for HostState {}

/// The deterministic network a 0.2 guest joins. It is built from the board's
/// `NetworkConfig` on first use; a circuit with no router has nothing to
/// join, so every Wi-Fi call is an error rather than a silent success.
impl HostState {
    fn network(&mut self) -> Option<&mut litz_net::Router> {
        if self.router.is_none() {
            let config = {
                let board = self.board.as_ref()?.borrow();
                board.network().cloned()?
            };
            self.router = Some(litz_net::Router::new(&config));
        }
        let now = self.now_ns;
        let router = self.router.as_mut()?;
        router.set_now(now);
        Some(router)
    }

    fn network_config(&self) -> Option<litz_net::NetworkConfig> {
        self.board
            .as_ref()
            .and_then(|board| board.borrow().network().cloned())
    }

    /// Move the host clock past a transaction's wire time. A guest reports
    /// cycles only when `run` returns, so without this every host call in one
    /// run would be stamped at the same tick and their waveforms would
    /// overlap. The advance is the bus rate the board records at.
    fn spend(&mut self, ns: u64) {
        self.now_ns = self.now_ns.saturating_add(ns);
    }

    fn flow(&self, id: u32) -> Result<(), String> {
        if self.flows.contains(&id) {
            Ok(())
        } else {
            Err(format!("no open flow {id}"))
        }
    }
}

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
        let board = self.board.as_ref()?.borrow();
        let volts = board.node_voltage_gpio_on(self.board_index, pin)?;
        Some((volts.max(0.0) * 1000.0).round() as u32)
    }

    fn i2c_transfer(
        &mut self,
        controller: u8,
        address: u8,
        write: Vec<u8>,
        read_len: u32,
    ) -> Result<Vec<u8>, String> {
        let at = self.now_ns;
        let bytes = 1
            + write.len()
            + if read_len > 0 {
                1 + read_len as usize
            } else {
                0
            };
        let mut board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?
            .borrow_mut();
        let hz = board.i2c_hz(self.board_index, controller);
        let read = board
            .i2c_write_read_at(
                self.board_index,
                controller,
                address,
                &write,
                read_len as usize,
                at,
            )
            .ok_or_else(|| format!("no I2C controller {controller} on this board"))?;
        drop(board);
        self.spend(bytes as u64 * 9 * (1_000_000_000 / u64::from(hz)));
        Ok(read)
    }

    fn spi_transfer(
        &mut self,
        controller: u8,
        chip_select: u8,
        tx: Vec<u8>,
        read_len: u32,
    ) -> Result<Vec<u8>, String> {
        let at = self.now_ns;
        let mut board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?
            .borrow_mut();
        let hz = board.spi_hz(self.board_index, controller);
        let period = 1_000_000_000 / u64::from(hz);
        let rx = board.spi_transfer_at(
            self.board_index,
            controller,
            chip_select,
            &tx,
            read_len as usize,
            at,
        );
        drop(board);
        self.spend(tx.len().max(read_len as usize) as u64 * 8 * period);
        Ok(rx)
    }

    fn uart_write(&mut self, port: u8, bytes: Vec<u8>) -> bool {
        let Some(board) = &self.board else {
            return false;
        };
        let mut board = board.borrow_mut();
        let baud = board.uart_baud(self.board_index, port);
        let wired = board.uart_receive(self.board_index, port, self.now_ns, &bytes);
        drop(board);
        if wired {
            self.spend(bytes.len() as u64 * 10 * (1_000_000_000 / u64::from(baud)));
        }
        wired
    }

    fn uart_read(&mut self, port: u8) -> Vec<u8> {
        let Some(board) = &self.board else {
            return Vec::new();
        };
        let mut board = board.borrow_mut();
        let baud = board.uart_baud(self.board_index, port);
        let bytes = board.uart_poll(self.board_index, port, self.now_ns);
        drop(board);
        self.spend(bytes.len() as u64 * 10 * (1_000_000_000 / u64::from(baud)));
        bytes
    }

    fn wifi_scan(&mut self) -> Vec<bindings_v2::litz::mcu::types::WifiNetwork> {
        let Some(config) = self.network_config() else {
            return Vec::new();
        };
        let mut networks = Vec::new();
        for ap in &config.access_points {
            networks.push(bindings_v2::litz::mcu::types::WifiNetwork {
                ssid: ap.ssid.clone(),
                channel: ap.channel,
                rssi: -40,
                secure: ap.psk.is_some(),
                joined: self.joined.as_deref() == Some(ap.ssid.as_str()),
            });
        }
        for nearby in &config.nearby {
            networks.push(bindings_v2::litz::mcu::types::WifiNetwork {
                ssid: nearby.ssid.clone(),
                channel: nearby.channel,
                rssi: nearby.rssi,
                secure: nearby.secure,
                joined: false,
            });
        }
        networks
    }

    fn wifi_join(
        &mut self,
        ssid: String,
        psk: Option<String>,
    ) -> Result<bindings_v2::litz::mcu::types::WifiLease, String> {
        let config = self
            .network_config()
            .ok_or_else(|| "the circuit has no simulated access point".to_string())?;
        let ap = config
            .access_points
            .iter()
            .find(|ap| ap.ssid == ssid)
            .ok_or_else(|| format!("no access point named {ssid:?}"))?;
        if let Some(expected) = &ap.psk
            && psk.as_deref() != Some(expected.as_str())
        {
            return Err(format!("wrong password for {ssid:?}"));
        }
        let mut router = litz_net::Router::new(&config);
        router.set_now(self.now_ns);
        router.set_station([10, 0, 2, 15]);
        self.router = Some(router);
        self.joined = Some(ssid);
        self.flows.clear();
        Ok(bindings_v2::litz::mcu::types::WifiLease {
            ip: [10, 0, 2, 15].to_vec(),
            gateway: litz_net::GATEWAY_IP.to_vec(),
            dns: litz_net::HOST_IP.to_vec(),
        })
    }

    fn wifi_leave(&mut self) {
        self.joined = None;
        self.router = None;
        self.flows.clear();
    }

    fn wifi_resolve(&mut self, name: String) -> Result<Vec<u8>, String> {
        if self.joined.is_none() {
            return Err("not joined to a network".into());
        }
        let router = self.network().ok_or("not joined to a network")?;
        let id = router
            .resolve(&name, 1)
            .ok_or_else(|| format!("{name:?} is not a name the router resolves"))?;
        loop {
            let (answer_id, answer) = router
                .resolved()
                .ok_or_else(|| format!("no answer for {name:?}"))?;
            if answer_id != id {
                continue;
            }
            return match answer {
                litz_net::Answer::Addrs(addrs, _) => addrs
                    .first()
                    .map(|ip| ip.to_vec())
                    .ok_or_else(|| format!("{name:?} has no IPv4 address")),
                litz_net::Answer::Failed(code) => Err(format!("DNS failed with rcode {code}")),
            };
        }
    }

    fn tcp_connect(&mut self, host: Vec<u8>, port: u16) -> Result<u32, String> {
        if host.len() != 4 {
            return Err("an IPv4 host is four octets".into());
        }
        if self.joined.is_none() {
            return Err("not joined to a network".into());
        }
        let router = self.network().ok_or("not joined to a network")?;
        let id = router.tcp_connect(port);
        if !router.connected(id) {
            return Err(format!("connection to port {port} was refused"));
        }
        self.flows.insert(id);
        Ok(id)
    }

    fn tcp_write(&mut self, id: u32, bytes: Vec<u8>) -> Result<u32, String> {
        self.flow(id)?;
        let router = self.network().ok_or("not joined to a network")?;
        router
            .tcp_write(id, &bytes)
            .map(|n| n as u32)
            .ok_or_else(|| format!("flow {id} is closed"))
    }

    fn tcp_read(&mut self, id: u32, max: u32) -> Result<Vec<u8>, String> {
        self.flow(id)?;
        let max = max.min(64 * 1024) as usize;
        let mut buf = vec![0; max];
        let read = {
            let router = self.network().ok_or("not joined to a network")?;
            match router.tcp_read(id, &mut buf) {
                Some(0) => {
                    router.tcp_close(id);
                    None
                }
                Some(n) => {
                    buf.truncate(n);
                    Some(buf)
                }
                None => Some(Vec::new()),
            }
        };
        match read {
            Some(bytes) => Ok(bytes),
            None => {
                self.flows.remove(&id);
                Ok(Vec::new())
            }
        }
    }

    fn tcp_close(&mut self, id: u32) {
        if let Some(router) = self.network() {
            router.tcp_close(id);
        }
        self.flows.remove(&id);
    }

    fn udp_open(&mut self, port: u16) -> Result<u32, String> {
        if self.joined.is_none() {
            return Err("not joined to a network".into());
        }
        let router = self.network().ok_or("not joined to a network")?;
        let id = router
            .udp_open(port)
            .ok_or_else(|| format!("no UDP service on port {port}"))?;
        self.flows.insert(id);
        Ok(id)
    }

    fn udp_send(
        &mut self,
        id: u32,
        host: Vec<u8>,
        _port: u16,
        bytes: Vec<u8>,
    ) -> Result<u32, String> {
        if host.len() != 4 {
            return Err("an IPv4 host is four octets".into());
        }
        self.flow(id)?;
        let router = self.network().ok_or("not joined to a network")?;
        router
            .udp_send(id, &bytes)
            .map(|n| n as u32)
            .ok_or_else(|| format!("flow {id} is closed"))
    }

    fn udp_read(&mut self, id: u32, max: u32) -> Result<Vec<u8>, String> {
        self.flow(id)?;
        let router = self.network().ok_or("not joined to a network")?;
        let mut buf = vec![0; max.min(64 * 1024) as usize];
        match router.udp_recv(id, &mut buf) {
            Some(n) => {
                buf.truncate(n);
                Ok(buf)
            }
            None => Ok(Vec::new()),
        }
    }

    fn udp_close(&mut self, id: u32) {
        if let Some(router) = self.network() {
            router.udp_close(id);
        }
        self.flows.remove(&id);
    }

    fn ble_scan(&mut self) -> Vec<bindings_v2::litz::mcu::types::BleAdvertisement> {
        let Some(board) = &self.board else {
            return Vec::new();
        };
        board
            .borrow()
            .ble_air(self.board_index)
            .into_iter()
            .map(|beacon| bindings_v2::litz::mcu::types::BleAdvertisement {
                address: beacon.address.to_vec(),
                name: beacon.name.clone(),
                rssi: beacon.rssi,
                data: beacon.adv.clone(),
                scan_response: beacon.scan_response.clone(),
            })
            .collect()
    }

    fn ble_advertise(
        &mut self,
        address: Vec<u8>,
        data: Vec<u8>,
        scan_response: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        if data.len() > litz_ble::MAX_ADVERTISING_DATA {
            return Err(format!(
                "advertising data is {} octets; the limit is {}",
                data.len(),
                litz_ble::MAX_ADVERTISING_DATA
            ));
        }
        if scan_response.len() > litz_ble::MAX_ADVERTISING_DATA {
            return Err(format!(
                "scan response is {} octets; the limit is {}",
                scan_response.len(),
                litz_ble::MAX_ADVERTISING_DATA
            ));
        }
        let address = match address.as_slice() {
            [] => litz_ble::derive_address(&data),
            [a, b, c, d, e, f] => [*a, *b, *c, *d, *e, *f],
            _ => return Err("a BLE address is six octets, least significant first".into()),
        };
        let Some(board) = &self.board else {
            return Err("this chip is not attached to a board".into());
        };
        let gatt = board.borrow().gatt_table(self.board_index);
        let beacon = litz_ble::Beacon {
            name: litz_ble::ad::name(&data),
            services: litz_ble::ad::services16(&data),
            address,
            rssi: -40,
            adv: data,
            scan_response,
            gatt,
        };
        board.borrow().advertise(self.board_index, beacon)?;
        Ok(address.to_vec())
    }

    fn ble_stop_advertising(&mut self, address: Vec<u8>) {
        if let Some(board) = &self.board {
            board.borrow().stop_advertising(self.board_index, &address);
        }
    }

    fn ble_set_gatt(
        &mut self,
        services: Vec<bindings_v2::litz::mcu::types::GattService>,
    ) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        let table = services.into_iter().map(gatt_service_ours).collect();
        board.borrow().set_gatt(self.board_index, table)
    }

    fn ble_connect(&mut self, address: Vec<u8>) -> Result<u16, String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        let handle = board.borrow().gatt_connect(self.board_index, &address)?;
        // The connection is only usable once the first connection event has
        // happened: the central scans, the peripheral answers, and both sides
        // learn the channel map and the anchor.
        let event = board
            .borrow()
            .gatt_advance(self.board_index, handle, self.now_ns);
        self.spend(litz_board::CONNECTION_DELAY_NS + event);
        Ok(handle)
    }

    fn ble_disconnect(&mut self, handle: u16) {
        if let Some(board) = &self.board {
            let _ = board.borrow().gatt_disconnect(self.board_index, handle);
        }
    }

    fn ble_discover(
        &mut self,
        handle: u16,
    ) -> Result<Vec<bindings_v2::litz::mcu::types::GattService>, String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        let table = board.borrow().gatt_discover(self.board_index, handle)?;
        let wait = board
            .borrow()
            .gatt_advance(self.board_index, handle, self.now_ns);
        self.spend(wait);
        Ok(table.into_iter().map(gatt_service_wit).collect())
    }

    fn ble_read(
        &mut self,
        handle: u16,
        service: u16,
        characteristic: u16,
    ) -> Result<Vec<u8>, String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        let value = board
            .borrow()
            .gatt_read(self.board_index, handle, service, characteristic)?;
        let wait = board
            .borrow()
            .gatt_advance(self.board_index, handle, self.now_ns);
        self.spend(wait);
        Ok(value)
    }

    fn ble_write(
        &mut self,
        handle: u16,
        service: u16,
        characteristic: u16,
        value: Vec<u8>,
    ) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        board
            .borrow()
            .gatt_write(self.board_index, handle, service, characteristic, &value)?;
        let wait = board
            .borrow()
            .gatt_advance(self.board_index, handle, self.now_ns);
        self.spend(wait);
        Ok(())
    }

    fn ble_subscribe(
        &mut self,
        handle: u16,
        service: u16,
        characteristic: u16,
        enabled: bool,
    ) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        board.borrow().gatt_subscribe(
            self.board_index,
            handle,
            service,
            characteristic,
            enabled,
        )?;
        let wait = board
            .borrow()
            .gatt_advance(self.board_index, handle, self.now_ns);
        self.spend(wait);
        Ok(())
    }

    fn ble_notifications(
        &mut self,
        handle: u16,
    ) -> Vec<bindings_v2::litz::mcu::types::GattNotification> {
        let Some(board) = &self.board else {
            return Vec::new();
        };
        let notifications = board
            .borrow()
            .gatt_notifications(self.board_index, handle)
            .unwrap_or_default();
        if !notifications.is_empty() {
            // A notification is carried by a connection event; polling it
            // costs the wait for that event, exactly like a read.
            let wait = board
                .borrow()
                .gatt_advance(self.board_index, handle, self.now_ns);
            self.spend(wait);
        }
        notifications
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
        let Some(board) = &self.board else {
            return Vec::new();
        };
        board
            .borrow()
            .gatt_take_writes(self.board_index)
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
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        board
            .borrow()
            .gatt_notify(self.board_index, service, characteristic, &value)
    }

    fn radio_configure(
        &mut self,
        network: String,
        channel: u16,
        address: Vec<u8>,
    ) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        board
            .borrow()
            .radio_configure(self.board_index, &network, channel, &address)
    }

    fn radio_send(
        &mut self,
        bytes: Vec<u8>,
        to: Option<Vec<u8>>,
        airtime_ns: u64,
    ) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        let at = self
            .now_ns
            .max(board.borrow().radio_clock())
            .max(board.borrow().now());
        board
            .borrow()
            .radio_send(self.board_index, &bytes, to.as_deref(), airtime_ns, at)?;
        // The frame occupies the air until its last bit; a station that sends
        // and then listens waits for that much simulated time.
        self.spend(airtime_ns);
        Ok(())
    }

    fn radio_receive(&mut self) -> Vec<bindings_v2::litz::mcu::types::RadioFrame> {
        let Some(board) = &self.board else {
            return Vec::new();
        };
        let at = self
            .now_ns
            .max(board.borrow().radio_clock())
            .max(board.borrow().now());
        board
            .borrow()
            .radio_receive(self.board_index, at)
            .into_iter()
            .map(
                |(sender, packet)| bindings_v2::litz::mcu::types::RadioFrame {
                    sender,
                    to: packet.to.clone(),
                    bytes: packet.bytes,
                    sent_at: packet.sent_at_ns,
                    airtime: packet.airtime_ns,
                },
            )
            .collect()
    }
}

/// WIT table → the crate's table, so the board does not depend on bindings.
fn gatt_service_ours(service: bindings_v2::litz::mcu::types::GattService) -> litz_ble::GattService {
    litz_ble::GattService {
        uuid: service.uuid,
        characteristics: service
            .characteristics
            .into_iter()
            .map(|characteristic| litz_ble::GattCharacteristic {
                uuid: characteristic.uuid,
                flags: characteristic.access,
                value: characteristic.value,
            })
            .collect(),
    }
}

fn gatt_service_wit(service: litz_ble::GattService) -> bindings_v2::litz::mcu::types::GattService {
    bindings_v2::litz::mcu::types::GattService {
        uuid: service.uuid,
        characteristics: service
            .characteristics
            .into_iter()
            .map(
                |characteristic| bindings_v2::litz::mcu::types::GattCharacteristic {
                    uuid: characteristic.uuid,
                    access: characteristic.flags,
                    value: characteristic.value,
                },
            )
            .collect(),
    }
}

/// Which contract version a loaded component speaks. A 0.1 module has only
/// pins and console bytes; a 0.2 module also has the peripheral and wireless
/// host calls.
enum Instance {
    V1(Simulator),
    V2(bindings_v2::Simulator),
}

/// A loaded chip module, ready to drive a board.
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

        let limits = StoreLimitsBuilder::new().memory_size(MAX_MEMORY).build();
        let mut store = Store::new(
            &engine,
            HostState {
                console: Vec::new(),
                fault: None,
                limits,
                board: None,
                board_index: 0,
                now_ns: 0,
                router: None,
                joined: None,
                flows: std::collections::BTreeSet::new(),
            },
        );
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

fn wit_level(level: Level) -> WitLevel {
    match level {
        Level::Low => WitLevel::Low,
        Level::High => WitLevel::High,
        Level::HighZ => WitLevel::Floating,
    }
}

fn our_level(level: WitLevel) -> Level {
    match level {
        WitLevel::Low => Level::Low,
        WitLevel::High => Level::High,
        WitLevel::Floating => Level::HighZ,
    }
}

fn wit_stop(stop: WitStop) -> Stop {
    match stop {
        WitStop::Budget => Stop::Budget,
        WitStop::Idle => Stop::Idle,
        WitStop::Halted => Stop::Halted,
        WitStop::Trap => Stop::Trap,
        WitStop::Reset => Stop::Reset,
    }
}

fn wit_image(kind: ImageKind) -> Image {
    match kind {
        ImageKind::Rom => Image::Rom,
        ImageKind::Bootloader => Image::Bootloader,
        ImageKind::PartitionTable => Image::PartitionTable,
        ImageKind::Firmware => Image::Firmware,
    }
}

fn wit_level_v2(level: WitLevel) -> bindings_v2::litz::mcu::types::Level {
    match level {
        WitLevel::Low => bindings_v2::litz::mcu::types::Level::Low,
        WitLevel::High => bindings_v2::litz::mcu::types::Level::High,
        WitLevel::Floating => bindings_v2::litz::mcu::types::Level::Floating,
    }
}

fn our_level_v2(level: bindings_v2::litz::mcu::types::Level) -> Level {
    match level {
        bindings_v2::litz::mcu::types::Level::Low => Level::Low,
        bindings_v2::litz::mcu::types::Level::High => Level::High,
        bindings_v2::litz::mcu::types::Level::Floating => Level::HighZ,
    }
}

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

fn wit_image_v2(kind: ImageKind) -> bindings_v2::litz::mcu::types::Image {
    use bindings_v2::litz::mcu::types::Image;
    match kind {
        ImageKind::Rom => Image::Rom,
        ImageKind::Bootloader => Image::Bootloader,
        ImageKind::PartitionTable => Image::PartitionTable,
        ImageKind::Firmware => Image::Firmware,
    }
}

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
