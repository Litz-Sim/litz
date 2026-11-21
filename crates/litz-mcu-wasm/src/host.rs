//! The host side of `litz:mcu@0.2.0`.
//!
//! These are the answers a guest gets when it asks the circuit about a bus, an
//! ADC channel, the network or the air. The logic lives here rather than in
//! the wasmtime bindings so the same code serves both hosts: the native
//! component loader calls it through the WIT traits, and the browser build
//! calls it through `litz_guest_call`, the JSON door the JavaScript adapter
//! uses. Nothing in this module depends on wasmtime.

use litz_board::Board;
use std::cell::RefCell;
use std::rc::Rc;

#[cfg(not(target_arch = "wasm32"))]
use wasmtime::StoreLimits;

/// One access point or nearby network a scan hears.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiNetwork {
    pub ssid: String,
    pub channel: u8,
    pub rssi: i8,
    pub secure: bool,
    pub joined: bool,
}

/// The lease a station receives when it joins.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiLease {
    pub ip: Vec<u8>,
    pub gateway: Vec<u8>,
    pub dns: Vec<u8>,
}

/// One advertiser a BLE scan hears.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BleAdvertisement {
    pub address: Vec<u8>,
    pub name: Option<String>,
    pub rssi: i8,
    pub data: Vec<u8>,
    pub scan_response: Vec<u8>,
}

/// One characteristic a peripheral serves.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GattCharacteristic {
    pub uuid: u16,
    pub access: u8,
    pub value: Vec<u8>,
}

/// One service in a GATT table.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GattService {
    pub uuid: u16,
    pub characteristics: Vec<GattCharacteristic>,
}

/// A write a central made.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GattWrite {
    pub service: u16,
    pub characteristic: u16,
    pub value: Vec<u8>,
}

/// A notification a subscribed central reads.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GattNotification {
    pub service: u16,
    pub characteristic: u16,
    pub value: Vec<u8>,
}

/// One raw frame the shared radio medium hands a station.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioFrame {
    pub sender: Vec<u8>,
    pub to: Option<Vec<u8>>,
    pub bytes: Vec<u8>,
    pub sent_at: u64,
    pub airtime: u64,
}

/// What the host provides to a chip module.
pub struct HostState {
    pub console: Vec<u8>,
    pub fault: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    pub limits: StoreLimits,
    /// The board this chip is wired to, for the 0.2 peripheral and wireless
    /// calls. `None` before `attach`, and for a bare chip.
    pub(crate) board: Option<Rc<RefCell<Board>>>,
    pub(crate) board_index: u16,
    /// The tick the current `run` call started at. Host calls made during the
    /// run use it for bus timestamps; the guest's `cycles` moves only after
    /// the call returns.
    pub(crate) now_ns: u64,
    /// The deterministic network behind the circuit's access point, built
    /// lazily when the guest first asks about Wi-Fi.
    router: Option<litz_net::Router>,
    joined: Option<String>,
    /// Flow ids the router handed out, so an unknown id is an error rather
    /// than an empty read.
    flows: std::collections::BTreeSet<u32>,
}

impl Default for HostState {
    fn default() -> Self {
        Self::new()
    }
}

impl HostState {
    pub fn new() -> Self {
        Self {
            console: Vec::new(),
            fault: None,
            #[cfg(not(target_arch = "wasm32"))]
            limits: wasmtime::StoreLimitsBuilder::new().build(),
            board: None,
            board_index: 0,
            now_ns: 0,
            router: None,
            joined: None,
            flows: std::collections::BTreeSet::new(),
        }
    }

    /// A host attached to one board instance, ready for the 0.2 calls.
    pub fn attached(board: Rc<RefCell<Board>>, board_index: u16) -> Self {
        let mut state = Self::new();
        state.attach(board, board_index);
        state
    }

    pub fn attach(&mut self, board: Rc<RefCell<Board>>, board_index: u16) {
        self.board = Some(board);
        self.board_index = board_index;
    }

    /// The time host calls are stamped at. The browser host sets this before
    /// every guest `run`. It never moves backwards: a previous run's bus or
    /// radio traffic may have spent more wire time than the guest reported
    /// cycles, and waveforms must stay ordered.
    pub fn set_now(&mut self, ns: u64) {
        self.now_ns = self.now_ns.max(ns);
    }

    /// The deterministic network a 0.2 guest joins. It is built from the
    /// board's `NetworkConfig` on first use; a circuit with no router has
    /// nothing to join, so every Wi-Fi call is an error rather than a silent
    /// success.
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

    // ------------------------------------------------------------- peripherals

    pub fn analog_read(&mut self, pin: u8) -> Option<u32> {
        let board = self.board.as_ref()?.borrow();
        let volts = board.node_voltage_gpio_on(self.board_index, pin)?;
        Some((volts.max(0.0) * 1000.0).round() as u32)
    }

    pub fn i2c_transfer(
        &mut self,
        controller: u8,
        address: u8,
        write: &[u8],
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
        let Some(board) = &self.board else {
            return Err("this chip is not attached to a board".to_string());
        };
        let mut board = board.borrow_mut();
        let hz = board.i2c_hz(self.board_index, controller);
        let read = board
            .i2c_write_read_at(
                self.board_index,
                controller,
                address,
                write,
                read_len as usize,
                at,
            )
            .ok_or_else(|| format!("no I2C controller {controller} on this board"))?;
        drop(board);
        self.spend(bytes as u64 * 9 * (1_000_000_000 / u64::from(hz)));
        Ok(read)
    }

    pub fn spi_transfer(
        &mut self,
        controller: u8,
        chip_select: u8,
        tx: &[u8],
        read_len: u32,
    ) -> Result<Vec<u8>, String> {
        let at = self.now_ns;
        let Some(board) = &self.board else {
            return Err("this chip is not attached to a board".to_string());
        };
        let mut board = board.borrow_mut();
        let hz = board.spi_hz(self.board_index, controller);
        let period = 1_000_000_000 / u64::from(hz);
        let rx = board.spi_transfer_at(
            self.board_index,
            controller,
            chip_select,
            tx,
            read_len as usize,
            at,
        );
        drop(board);
        self.spend(tx.len().max(read_len as usize) as u64 * 8 * period);
        Ok(rx)
    }

    pub fn uart_write(&mut self, port: u8, bytes: &[u8]) -> bool {
        let Some(board) = &self.board else {
            return false;
        };
        let mut board = board.borrow_mut();
        let baud = board.uart_baud(self.board_index, port);
        let wired = board.uart_receive(self.board_index, port, self.now_ns, bytes);
        drop(board);
        if wired {
            self.spend(bytes.len() as u64 * 10 * (1_000_000_000 / u64::from(baud)));
        }
        wired
    }

    pub fn uart_read(&mut self, port: u8) -> Vec<u8> {
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

    // ---------------------------------------------------------------- Wi-Fi

    pub fn wifi_scan(&mut self) -> Vec<WifiNetwork> {
        let Some(config) = self.network_config() else {
            return Vec::new();
        };
        let mut networks = Vec::new();
        for ap in &config.access_points {
            networks.push(WifiNetwork {
                ssid: ap.ssid.clone(),
                channel: ap.channel,
                rssi: -40,
                secure: ap.psk.is_some(),
                joined: self.joined.as_deref() == Some(ap.ssid.as_str()),
            });
        }
        for nearby in &config.nearby {
            networks.push(WifiNetwork {
                ssid: nearby.ssid.clone(),
                channel: nearby.channel,
                rssi: nearby.rssi,
                secure: nearby.secure,
                joined: false,
            });
        }
        networks
    }

    pub fn wifi_join(&mut self, ssid: &str, psk: Option<&str>) -> Result<WifiLease, String> {
        let config = self
            .network_config()
            .ok_or_else(|| "the circuit has no simulated access point".to_string())?;
        let ap = config
            .access_points
            .iter()
            .find(|ap| ap.ssid == ssid)
            .ok_or_else(|| format!("no access point named {ssid:?}"))?;
        if let Some(expected) = &ap.psk
            && psk != Some(expected.as_str())
        {
            return Err(format!("wrong password for {ssid:?}"));
        }
        let mut router = litz_net::Router::new(&config);
        router.set_now(self.now_ns);
        router.set_station([10, 0, 2, 15]);
        self.router = Some(router);
        self.joined = Some(ssid.to_string());
        self.flows.clear();
        Ok(WifiLease {
            ip: [10, 0, 2, 15].to_vec(),
            gateway: litz_net::GATEWAY_IP.to_vec(),
            dns: litz_net::HOST_IP.to_vec(),
        })
    }

    pub fn wifi_leave(&mut self) {
        self.joined = None;
        self.router = None;
        self.flows.clear();
    }

    pub fn wifi_resolve(&mut self, name: &str) -> Result<Vec<u8>, String> {
        if self.joined.is_none() {
            return Err("not joined to a network".into());
        }
        let router = self.network().ok_or("not joined to a network")?;
        let id = router
            .resolve(name, 1)
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

    // ------------------------------------------------------------ TCP and UDP

    pub fn tcp_connect(&mut self, host: &[u8], port: u16) -> Result<u32, String> {
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

    pub fn tcp_write(&mut self, id: u32, bytes: &[u8]) -> Result<u32, String> {
        self.flow(id)?;
        let router = self.network().ok_or("not joined to a network")?;
        router
            .tcp_write(id, bytes)
            .map(|n| n as u32)
            .ok_or_else(|| format!("flow {id} is closed"))
    }

    pub fn tcp_read(&mut self, id: u32, max: u32) -> Result<Vec<u8>, String> {
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

    pub fn tcp_close(&mut self, id: u32) {
        if let Some(router) = self.network() {
            router.tcp_close(id);
        }
        self.flows.remove(&id);
    }

    pub fn udp_open(&mut self, port: u16) -> Result<u32, String> {
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

    pub fn udp_send(
        &mut self,
        id: u32,
        host: &[u8],
        _port: u16,
        bytes: &[u8],
    ) -> Result<u32, String> {
        if host.len() != 4 {
            return Err("an IPv4 host is four octets".into());
        }
        self.flow(id)?;
        let router = self.network().ok_or("not joined to a network")?;
        router
            .udp_send(id, bytes)
            .map(|n| n as u32)
            .ok_or_else(|| format!("flow {id} is closed"))
    }

    pub fn udp_read(&mut self, id: u32, max: u32) -> Result<Vec<u8>, String> {
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

    pub fn udp_close(&mut self, id: u32) {
        if let Some(router) = self.network() {
            router.udp_close(id);
        }
        self.flows.remove(&id);
    }

    // ------------------------------------------------------------------- BLE

    pub fn ble_scan(&mut self) -> Vec<BleAdvertisement> {
        let Some(board) = &self.board else {
            return Vec::new();
        };
        board
            .borrow()
            .ble_air(self.board_index)
            .into_iter()
            .map(|beacon| BleAdvertisement {
                address: beacon.address.to_vec(),
                name: beacon.name.clone(),
                rssi: beacon.rssi,
                data: beacon.adv.clone(),
                scan_response: beacon.scan_response.clone(),
            })
            .collect()
    }

    pub fn ble_advertise(
        &mut self,
        address: &[u8],
        data: &[u8],
        scan_response: &[u8],
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
        let address = match address {
            [] => litz_ble::derive_address(data),
            [a, b, c, d, e, f] => [*a, *b, *c, *d, *e, *f],
            _ => return Err("a BLE address is six octets, least significant first".into()),
        };
        let Some(board) = &self.board else {
            return Err("this chip is not attached to a board".into());
        };
        let gatt = board.borrow().gatt_table(self.board_index);
        let beacon = litz_ble::Beacon {
            name: litz_ble::ad::name(data),
            services: litz_ble::ad::services16(data),
            address,
            rssi: -40,
            adv: data.to_vec(),
            scan_response: scan_response.to_vec(),
            gatt,
        };
        board.borrow().advertise(self.board_index, beacon)?;
        Ok(address.to_vec())
    }

    pub fn ble_stop_advertising(&mut self, address: &[u8]) {
        if let Some(board) = &self.board {
            board.borrow().stop_advertising(self.board_index, address);
        }
    }

    pub fn ble_set_gatt(&mut self, services: Vec<GattService>) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        let table = services.into_iter().map(gatt_service_ours).collect();
        board.borrow().set_gatt(self.board_index, table)
    }

    pub fn ble_connect(&mut self, address: &[u8]) -> Result<u16, String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        let handle = board.borrow().gatt_connect(self.board_index, address)?;
        // The connection is only usable once the first connection event has
        // happened: the central scans, the peripheral answers, and both sides
        // learn the channel map and the anchor.
        let event = board
            .borrow()
            .gatt_advance(self.board_index, handle, self.now_ns);
        self.spend(litz_board::CONNECTION_DELAY_NS + event);
        Ok(handle)
    }

    pub fn ble_disconnect(&mut self, handle: u16) {
        if let Some(board) = &self.board {
            let _ = board.borrow().gatt_disconnect(self.board_index, handle);
        }
    }

    pub fn ble_discover(&mut self, handle: u16) -> Result<Vec<GattService>, String> {
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

    pub fn ble_read(
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

    pub fn ble_write(
        &mut self,
        handle: u16,
        service: u16,
        characteristic: u16,
        value: &[u8],
    ) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        board
            .borrow()
            .gatt_write(self.board_index, handle, service, characteristic, value)?;
        let wait = board
            .borrow()
            .gatt_advance(self.board_index, handle, self.now_ns);
        self.spend(wait);
        Ok(())
    }

    pub fn ble_subscribe(
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

    pub fn ble_notifications(&mut self, handle: u16) -> Vec<GattNotification> {
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
            .map(|notification| GattNotification {
                service: notification.service,
                characteristic: notification.characteristic,
                value: notification.value,
            })
            .collect()
    }

    pub fn ble_take_writes(&mut self) -> Vec<GattWrite> {
        let Some(board) = &self.board else {
            return Vec::new();
        };
        board
            .borrow()
            .gatt_take_writes(self.board_index)
            .into_iter()
            .map(|write| GattWrite {
                service: write.service,
                characteristic: write.characteristic,
                value: write.value,
            })
            .collect()
    }

    pub fn ble_notify(
        &mut self,
        service: u16,
        characteristic: u16,
        value: &[u8],
    ) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        board
            .borrow()
            .gatt_notify(self.board_index, service, characteristic, value)
    }

    // ----------------------------------------------------------------- radio

    pub fn radio_configure(
        &mut self,
        network: &str,
        channel: u16,
        address: &[u8],
    ) -> Result<(), String> {
        let board = self
            .board
            .as_ref()
            .ok_or_else(|| "this chip is not attached to a board".to_string())?;
        board
            .borrow()
            .radio_configure(self.board_index, network, channel, address)
    }

    pub fn radio_send(
        &mut self,
        bytes: &[u8],
        to: Option<&[u8]>,
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
            .radio_send(self.board_index, bytes, to, airtime_ns, at)?;
        // The frame occupies the air until its last bit; a station that sends
        // and then listens waits for that much simulated time.
        self.spend(airtime_ns);
        Ok(())
    }

    pub fn radio_receive(&mut self) -> Vec<RadioFrame> {
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
            .map(|(sender, packet)| RadioFrame {
                sender,
                to: packet.to.clone(),
                bytes: packet.bytes,
                sent_at: packet.sent_at_ns,
                airtime: packet.airtime_ns,
            })
            .collect()
    }

    /// The JSON door the browser host uses: one op name, one JSON argument
    /// object, one JSON result. Errors are the contract's own `result<_, string>`
    /// failures and travel as `Err`.
    #[cfg(target_arch = "wasm32")]
    pub fn call(&mut self, op: &str, args: &str) -> Result<String, String> {
        let args: serde_json::Value = if args.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(args).map_err(|e| format!("arguments for {op}: {e}"))?
        };
        let value = match op {
            "analogRead" => json(self.analog_read(arg_u8(&args, "pin")?)),
            "i2cTransfer" => json(self.i2c_transfer(
                arg_u8(&args, "controller")?,
                arg_u8(&args, "address")?,
                &arg_bytes(&args, "write")?,
                arg_u32(&args, "readLen")?,
            )?),
            "spiTransfer" => json(self.spi_transfer(
                arg_u8(&args, "controller")?,
                arg_u8(&args, "chipSelect")?,
                &arg_bytes(&args, "tx")?,
                arg_u32(&args, "readLen")?,
            )?),
            "uartWrite" => {
                json(self.uart_write(arg_u8(&args, "port")?, &arg_bytes(&args, "bytes")?))
            }
            "uartRead" => json(self.uart_read(arg_u8(&args, "port")?)),
            "wifiScan" => json(self.wifi_scan()),
            "wifiJoin" => json(self.wifi_join(
                arg_str(&args, "ssid")?,
                arg_opt_str(&args, "psk")?.as_deref(),
            )?),
            "wifiLeave" => {
                self.wifi_leave();
                json(())
            }
            "wifiResolve" => json(self.wifi_resolve(arg_str(&args, "name")?)?),
            "tcpConnect" => {
                json(self.tcp_connect(&arg_bytes(&args, "host")?, arg_u16(&args, "port")?)?)
            }
            "tcpWrite" => json(self.tcp_write(arg_u32(&args, "id")?, &arg_bytes(&args, "bytes")?)?),
            "tcpRead" => json(self.tcp_read(arg_u32(&args, "id")?, arg_u32(&args, "max")?)?),
            "tcpClose" => {
                self.tcp_close(arg_u32(&args, "id")?);
                json(())
            }
            "udpOpen" => json(self.udp_open(arg_u16(&args, "port")?)?),
            "udpSend" => json(self.udp_send(
                arg_u32(&args, "id")?,
                &arg_bytes(&args, "host")?,
                arg_u16(&args, "port")?,
                &arg_bytes(&args, "bytes")?,
            )?),
            "udpRead" => json(self.udp_read(arg_u32(&args, "id")?, arg_u32(&args, "max")?)?),
            "udpClose" => {
                self.udp_close(arg_u32(&args, "id")?);
                json(())
            }
            "bleScan" => json(self.ble_scan()),
            "bleAdvertise" => json(self.ble_advertise(
                &arg_bytes(&args, "address")?,
                &arg_bytes(&args, "data")?,
                &arg_bytes(&args, "scanResponse")?,
            )?),
            "bleStopAdvertising" => {
                self.ble_stop_advertising(&arg_bytes(&args, "address")?);
                json(())
            }
            "bleSetGatt" => {
                let services = args
                    .get("services")
                    .cloned()
                    .ok_or_else(|| "arguments for bleSetGatt have no \"services\"".to_string())?;
                let services: Vec<GattService> = serde_json::from_value(services)
                    .map_err(|e| format!("bleSetGatt services: {e}"))?;
                json(self.ble_set_gatt(services)?)
            }
            "bleConnect" => json(self.ble_connect(&arg_bytes(&args, "address")?)?),
            "bleDisconnect" => {
                self.ble_disconnect(arg_u16(&args, "handle")?);
                json(())
            }
            "bleDiscover" => json(self.ble_discover(arg_u16(&args, "handle")?)?),
            "bleRead" => json(self.ble_read(
                arg_u16(&args, "handle")?,
                arg_u16(&args, "service")?,
                arg_u16(&args, "characteristic")?,
            )?),
            "bleWrite" => json(self.ble_write(
                arg_u16(&args, "handle")?,
                arg_u16(&args, "service")?,
                arg_u16(&args, "characteristic")?,
                &arg_bytes(&args, "value")?,
            )?),
            "bleSubscribe" => json(self.ble_subscribe(
                arg_u16(&args, "handle")?,
                arg_u16(&args, "service")?,
                arg_u16(&args, "characteristic")?,
                arg_bool(&args, "enabled")?,
            )?),
            "bleNotifications" => json(self.ble_notifications(arg_u16(&args, "handle")?)),
            "bleTakeWrites" => json(self.ble_take_writes()),
            "bleNotify" => json(self.ble_notify(
                arg_u16(&args, "service")?,
                arg_u16(&args, "characteristic")?,
                &arg_bytes(&args, "value")?,
            )?),
            "radioConfigure" => json(self.radio_configure(
                arg_str(&args, "network")?,
                arg_u16(&args, "channel")?,
                &arg_bytes(&args, "address")?,
            )?),
            "radioSend" => json(self.radio_send(
                &arg_bytes(&args, "bytes")?,
                arg_opt_bytes(&args, "to")?.as_deref(),
                arg_u64(&args, "airtimeNs")?,
            )?),
            "radioReceive" => json(self.radio_receive()),
            other => return Err(format!("unknown litz:mcu host call {other:?}")),
        };
        Ok(value)
    }
}

#[cfg(target_arch = "wasm32")]
fn json<T: serde::Serialize>(value: T) -> String {
    serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string())
}

#[cfg(target_arch = "wasm32")]
fn arg<'a>(args: &'a serde_json::Value, name: &str) -> Result<&'a serde_json::Value, String> {
    args.get(name)
        .ok_or_else(|| format!("arguments have no {name:?}"))
}

#[cfg(target_arch = "wasm32")]
fn arg_u8(args: &serde_json::Value, name: &str) -> Result<u8, String> {
    arg(args, name)?
        .as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| format!("{name:?} is not a byte"))
}

#[cfg(target_arch = "wasm32")]
fn arg_u16(args: &serde_json::Value, name: &str) -> Result<u16, String> {
    arg(args, name)?
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .ok_or_else(|| format!("{name:?} is not a u16"))
}

#[cfg(target_arch = "wasm32")]
fn arg_u32(args: &serde_json::Value, name: &str) -> Result<u32, String> {
    arg(args, name)?
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("{name:?} is not a u32"))
}

#[cfg(target_arch = "wasm32")]
fn arg_u64(args: &serde_json::Value, name: &str) -> Result<u64, String> {
    arg(args, name)?
        .as_u64()
        .ok_or_else(|| format!("{name:?} is not a u64"))
}

#[cfg(target_arch = "wasm32")]
fn arg_bool(args: &serde_json::Value, name: &str) -> Result<bool, String> {
    arg(args, name)?
        .as_bool()
        .ok_or_else(|| format!("{name:?} is not a boolean"))
}

#[cfg(target_arch = "wasm32")]
fn arg_str<'a>(args: &'a serde_json::Value, name: &str) -> Result<&'a str, String> {
    arg(args, name)?
        .as_str()
        .ok_or_else(|| format!("{name:?} is not a string"))
}

#[cfg(target_arch = "wasm32")]
fn arg_opt_str(args: &serde_json::Value, name: &str) -> Result<Option<String>, String> {
    match arg(args, name)? {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(text) => Ok(Some(text.clone())),
        _ => Err(format!("{name:?} is not a string")),
    }
}

#[cfg(target_arch = "wasm32")]
fn arg_bytes(args: &serde_json::Value, name: &str) -> Result<Vec<u8>, String> {
    let values = arg(args, name)?
        .as_array()
        .ok_or_else(|| format!("{name:?} is not a byte list"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| format!("{name:?} holds a value that is not a byte"))
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn arg_opt_bytes(args: &serde_json::Value, name: &str) -> Result<Option<Vec<u8>>, String> {
    match arg(args, name)? {
        serde_json::Value::Null => Ok(None),
        _ => arg_bytes(args, name).map(Some),
    }
}

/// The crate's table → the board's, so the board does not depend on the
/// contract's generated types.
fn gatt_service_ours(service: GattService) -> litz_ble::GattService {
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

fn gatt_service_wit(service: litz_ble::GattService) -> GattService {
    GattService {
        uuid: service.uuid,
        characteristics: service
            .characteristics
            .into_iter()
            .map(|characteristic| GattCharacteristic {
                uuid: characteristic.uuid,
                access: characteristic.flags,
                value: characteristic.value,
            })
            .collect(),
    }
}
