//! Resolving levels and running the parts.
//!
//! A change on one net can change a part's drive, which changes another net.
//! Within a tick the board is settled to a fixed point: recompute every net,
//! notify the parts on nets that moved, repeat. Eight passes is far more than
//! any passive network of this size needs; not converging is itself a fault.

use crate::BuildError;
use crate::analog::{self, AnalogConfig, AnalogSolution};
use crate::bus::Buses;
use crate::flatten::{Device, Endpoint, Engine, FlatBoard};
use crate::pixels::Framebuffer;
use crate::probe::{self, Channel, Probe};
use crate::waveform;
use crate::{devices, parts};
use litz_core::{Drive, Level, Part, PartState, PinId, Strength, Tick};
use litz_diagram::PinKind;
use litz_radio::{RadioHub, RadioHubHandle};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

const MAX_SETTLE_ITERATIONS: usize = 8;
/// How many rounds of "advance every due part, settle, look again" one call
/// may take. A part that keeps scheduling events in the past is a bug, not a
/// reason to hang a test.
const MAX_ADVANCE_ROUNDS: usize = 64;
/// Default analog step when [`Board::analog_advance`] is used before any
/// [`Board::analog_transient`] call has chosen one: 1 µs. A millisecond time
/// constant gets a thousand steps per tau, comfortably inside the solver's
/// first-order error budget.
pub const ANALOG_DEFAULT_DT_NS: u64 = 1_000;
/// Cap on the steps one [`Board::analog_advance`] may take, so a caller that
/// advances by a second in 1 ns steps is refused rather than left spinning.
const MAX_TRANSIENT_STEPS: u64 = 1_000_000;
/// How many runtime BLE advertisers one circuit's air may hold. The circuit's
/// own beacons are not counted; a guest cannot grow the record without bound.
const MAX_ADVERTISERS: usize = 16;
/// How many GATT links one board may hold at once.
const MAX_GATT_LINKS: usize = 32;
/// The largest raw radio frame a station may put on the shared medium.
const MAX_RADIO_PAYLOAD: usize = 256;
/// How many writes a peripheral may leave unread before the oldest is dropped.
const MAX_PENDING_WRITES: usize = 32;

/// The line rate the observation record draws a UART byte at when nothing
/// declares one. 115 200 is what the ESP32 and AVR boards in this repository
/// boot their console at; a decoder infers the rate from the record anyway.
pub const OBSERVED_UART_BAUD: u32 = 115_200;
/// The SCL rate the observation record draws an I2C transaction at: standard
/// mode, which every device on these boards accepts.
pub const OBSERVED_I2C_HZ: u32 = 100_000;
/// The clock rate the observation record draws an SPI transaction at.
pub const OBSERVED_SPI_HZ: u32 = 1_000_000;
/// CPOL/CPHA of an observed SPI frame. Mode 0 is what the catalog's devices
/// use; a part configured otherwise is still recorded, with its clock and
/// data lines swapped relative to the sample edge.
pub const OBSERVED_SPI_MODE: u8 = 0;

/// A transient run kept across calls.
///
/// The netlist is built once; every step updates `solution`, so reading a node
/// between steps costs a clone and never rebuilds the circuit. Any drive,
/// press, control or configuration change discards the whole run.
struct TransientRun {
    run: litz_analog::Transient,
    circuit: analog::Circuit,
    dt_ns: u64,
    at_ns: Tick,
    solution: AnalogSolution,
}

/// How often a net is driven two ways at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub net: u16,
    pub detail: String,
}

/// The line rate each backend configured for a bus, keyed by `(board,
/// controller)`. A byte-level transfer carries no clock of its own, so this is
/// what turns it back into edges at the firmware's timing rather than at a
/// fixed sample rate. A bus with no entry uses the observation defaults above.
#[derive(Clone, Debug, Default)]
struct BusRates {
    uart: BTreeMap<(u16, u8), u32>,
    i2c: BTreeMap<(u16, u8), u32>,
    spi: BTreeMap<(u16, u8), u32>,
}

fn not_found(service: u16, characteristic: u16) -> String {
    format!("the table has no characteristic {service:04x}/{characteristic:04x}")
}

/// One open GATT connection: which central, which advertiser's table, and the
/// characteristics that central subscribed to.
struct GattLink {
    handle: u16,
    central: u16,
    /// The board that owns the table, or `None` for a circuit beacon.
    peripheral: Option<u16>,
    address: [u8; 6],
    subscriptions: Vec<(u16, u16)>,
    notifications: Vec<litz_ble::GattNotification>,
    /// The next connection event, in simulated time. Every request is served
    /// at an event, so a burst of host calls cannot overtake the air.
    next_event: Tick,
}

/// How long after a connection request the first connection event happens.
/// Bluetooth LE calls this the connection delay; 7.5 ms is the value a central
/// programs when it has no estimate for the peripheral's anchor.
pub const CONNECTION_DELAY_NS: u64 = 7_500_000;

/// The connection interval the board uses: 30 ms, the value a central picks
/// when it wants throughput without draining a coin cell.
pub const CONNECTION_INTERVAL_NS: u64 = 30_000_000;

/// How much of an interval an empty connection event occupies: two 150 µs
/// windows, the central's and the peripheral's.
pub const CONNECTION_EVENT_NS: u64 = 300_000;

pub struct Board {
    flat: FlatBoard,
    parts: Vec<Box<dyn Part>>,
    /// The shared radio medium. The parts hold clones of this handle, and the
    /// board holds one so the medium outlives every part even when no part is
    /// a radio.
    radios: RadioHubHandle,
    /// Devices that answer transactions instead of pins. The board owns them;
    /// an adapter borrows the board to reach one.
    buses: Buses,
    /// The network behind the access point, when the circuit places a router
    /// part. It belongs to the board, not to any chip adapter, so the same
    /// circuit carries the same network on every backend.
    network: Option<litz_net::NetworkConfig>,
    /// The advertisers the circuit places. Like the network, it belongs to the
    /// board: any radio backend reads it and puts the same beacons in its air.
    bluetooth: litz_ble::BleConfig,
    /// Advertisements a chip started at runtime, with the board index that
    /// owns each. A station hears every advertiser except its own.
    advertised: RefCell<Vec<(u16, litz_ble::Beacon)>>,
    /// Bumped whenever the runtime advertisements change, so a native radio
    /// backend can refresh its scan list without cloning the air every step.
    ble_generation: std::cell::Cell<u64>,
    /// The GATT table each board serves as a peripheral. A table outlives one
    /// advertisement: a guest may register it before it starts advertising.
    gatt: RefCell<BTreeMap<u16, Vec<litz_ble::GattService>>>,
    /// Values written to a table that no board owns (a circuit beacon's), and
    /// the current values of a runtime table once a central has written them.
    gatt_overrides: RefCell<BTreeMap<[u8; 6], Vec<litz_ble::GattService>>>,
    /// Open GATT connections, one per (central, peripheral) pair.
    links: RefCell<Vec<GattLink>>,
    next_link: RefCell<u16>,
    /// Writes each peripheral board has not read yet.
    writes: RefCell<BTreeMap<u16, Vec<litz_ble::GattWrite>>>,
    /// The radio each board registered on the shared medium, if any.
    radio_ids: RefCell<BTreeMap<u16, litz_radio::RadioId>>,
    /// The furthest point in simulated time any station has used the medium.
    /// A station that polls later than its own clock sees frames the air has
    /// already carried, without making the air travel backwards.
    radio_clock: RefCell<Tick>,
    net_level: Vec<Level>,
    /// The chip's drive per `(board, GPIO)`. Board 0 is the one every
    /// existing single-chip backend addresses; a second board's pin with the
    /// same GPIO number is a separate entry because the board index is part
    /// of the key.
    chip_drive: BTreeMap<(u16, u8), Drive>,
    /// The resolved level on each `(board, GPIO)` pad.
    chip_level: BTreeMap<(u16, u8), Level>,
    /// Levels waiting to be reported to a chip, as `(board, GPIO, level)`.
    pending_inputs: Vec<(u16, u8, Level)>,
    conflicts: Vec<Conflict>,
    /// Every net transition since [`Board::start_trace`], for a VCD. Off by
    /// default: recording is opt-in so a CI run pays nothing for it.
    trace: Option<Vec<(Tick, u16, Level)>>,
    /// Pin transitions a peripheral produced behind the pad layer: a UART
    /// byte, an I2C clock, an SPI frame. They are net transitions too, but
    /// they are not produced by a `Drive`, so they live apart from `trace`
    /// and are merged in time order when a probe or a VCD is read.
    observed: Vec<(Tick, u16, Level)>,
    /// The level each net was last observed at, so a line that starts a
    /// transaction from a floating state gets the idle level a pull-up would
    /// have held it at before its first active edge.
    observed_levels: BTreeMap<u16, Level>,
    /// Net levels at the moment tracing started, which is the VCD's time 0.
    trace_initial: Vec<Level>,
    /// The tick the record started at; every channel's first sample.
    trace_started: Tick,
    /// Solved volts on each probed net at every settle, alongside the digital
    /// trace. Only filled while a trace runs, probes exist and analog
    /// co-simulation is on.
    analog_trace: Option<Vec<(Tick, u16, f64)>>,
    /// The points a host asked to watch.
    probes: Vec<Probe>,
    /// The last tick the board settled at. A probe's record starts here.
    now: Tick,
    /// Whether analog co-simulation is part of this run. Off by default: a
    /// digital-only board never builds a netlist and never touches the solver.
    analog: bool,
    /// Rail voltages and part models the analog view uses.
    analog_config: AnalogConfig,
    /// The last solved operating point, cleared whenever a drive or a host
    /// control changes. Inside a `RefCell` so an ADC read through `&self`
    /// (which is how `analog_inputs` is called) can still reuse it.
    analog_cache: RefCell<Option<AnalogSolution>>,
    /// The live transient run, if the board is in transient mode. `None` means
    /// DC mode, which is the default and the only mode the digital runtime
    /// needs.
    analog_transient: Option<TransientRun>,
    /// Line rates the backends configured, so a reconstructed waveform follows
    /// the firmware rather than a fixed sample rate.
    bus_rates: BusRates,
}

impl Board {
    /// A board with its own private radio medium.
    pub fn new(flat: FlatBoard) -> Result<Self, BuildError> {
        Self::new_with_radios(flat, RadioHub::shared())
    }

    /// A board on a caller-supplied medium.
    ///
    /// Two boards given one hub are two independent chips in one radio
    /// neighbourhood: a packet a part on the first sends is heard by a part on
    /// the second when their networks, channels and addresses agree. The board
    /// keeps the handle alive for as long as its parts live.
    pub fn new_with_radios(flat: FlatBoard, radios: RadioHubHandle) -> Result<Self, BuildError> {
        let parts = flat
            .parts
            .iter()
            .map(|part| build_part(part, &radios))
            .collect::<Result<Vec<_>, _>>()?;
        let buses = devices::build(&flat)?;
        let network = crate::network_parts::board_network(&flat.parts)?;
        let bluetooth = crate::ble_parts::config(&flat.parts)?;
        let mut board = Self {
            net_level: vec![Level::HighZ; flat.nets.len()],
            flat,
            parts,
            radios,
            buses,
            network,
            bluetooth,
            advertised: RefCell::new(Vec::new()),
            ble_generation: std::cell::Cell::new(0),
            gatt: RefCell::new(BTreeMap::new()),
            gatt_overrides: RefCell::new(BTreeMap::new()),
            links: RefCell::new(Vec::new()),
            next_link: RefCell::new(1),
            writes: RefCell::new(BTreeMap::new()),
            radio_ids: RefCell::new(BTreeMap::new()),
            radio_clock: RefCell::new(0),
            chip_drive: BTreeMap::new(),
            chip_level: BTreeMap::new(),
            pending_inputs: Vec::new(),
            conflicts: Vec::new(),
            trace: None,
            observed: Vec::new(),
            observed_levels: BTreeMap::new(),
            trace_initial: Vec::new(),
            trace_started: 0,
            analog_trace: None,
            probes: Vec::new(),
            now: 0,
            analog: false,
            analog_config: AnalogConfig::default(),
            analog_cache: RefCell::new(None),
            analog_transient: None,
            bus_rates: BusRates::default(),
        };
        board.settle(0);
        // The first settle produced the board's initial input levels; they are
        // still pending so an adapter that attaches afterwards applies them.
        Ok(board)
    }

    pub fn flat(&self) -> &FlatBoard {
        &self.flat
    }

    /// The radio medium this board and its parts share.
    pub fn radios(&self) -> &RadioHubHandle {
        &self.radios
    }

    /// The network the circuit's router part describes, when it has one.
    /// The Bluetooth advertisers the circuit places, in board order. `None`
    /// means the diagram has none, so no backend turns a controller on.
    /// Put an advertisement on the air from `board`.
    ///
    /// The circuit's beacons are static; this is the runtime half, so a custom
    /// chip can be an advertiser rather than only a scanner. Addresses are
    /// unique across the air, and the number of live advertisers is bounded so
    /// a guest cannot grow the record without limit.
    pub fn advertise(&self, board: u16, beacon: litz_ble::Beacon) -> Result<(), String> {
        let mut advertised = self.advertised.borrow_mut();
        if advertised.len() >= MAX_ADVERTISERS {
            return Err(format!(
                "the air already carries {MAX_ADVERTISERS} runtime advertisers"
            ));
        }
        if self
            .bluetooth
            .beacons
            .iter()
            .chain(advertised.iter().map(|(_, beacon)| beacon))
            .any(|other| other.address == beacon.address)
        {
            return Err(format!("{} is already advertising", beacon.address_text()));
        }
        advertised.push((board, beacon));
        self.ble_generation.set(self.ble_generation.get() + 1);
        Ok(())
    }

    /// Stop one board's advertisement on `address`. Stopping an address that
    /// is not advertising is a no-op, as the host contract says.
    pub fn stop_advertising(&self, board: u16, address: &[u8]) {
        if address.len() != 6 {
            return;
        }
        let before = self.advertised.borrow().len();
        self.advertised
            .borrow_mut()
            .retain(|(owner, beacon)| !(*owner == board && beacon.address.as_slice() == address));
        if self.advertised.borrow().len() != before {
            self.ble_generation.set(self.ble_generation.get() + 1);
        }
    }

    /// A counter that changes whenever a runtime advertisement starts or
    /// stops. A backend that mirrors the air into a native controller compares
    /// it each step instead of cloning the beacon list.
    pub fn ble_generation(&self) -> u64 {
        self.ble_generation.get()
    }

    /// What a station on `board` hears: the circuit's advertisers plus every
    /// runtime advertisement another board started, in address order.
    pub fn ble_air(&self, board: u16) -> Vec<litz_ble::Beacon> {
        let mut air = self.bluetooth.beacons.clone();
        air.extend(
            self.advertised
                .borrow()
                .iter()
                .filter(|(owner, _)| *owner != board)
                .map(|(_, beacon)| beacon.clone()),
        );
        air.sort_by_key(|beacon| beacon.address);
        air
    }

    pub fn bluetooth(&self) -> Option<&litz_ble::BleConfig> {
        (!self.bluetooth.is_empty()).then_some(&self.bluetooth)
    }

    /// Register the GATT table `board` serves as a peripheral. The table is
    /// kept apart from any advertisement, so a guest may register it before
    /// calling `ble-advertise` and replace it later.
    pub fn set_gatt(&self, board: u16, services: Vec<litz_ble::GattService>) -> Result<(), String> {
        if services.len() > litz_ble::MAX_SERVICES {
            return Err(format!(
                "a GATT table holds at most {} services",
                litz_ble::MAX_SERVICES
            ));
        }
        for service in &services {
            if service.characteristics.len() > litz_ble::MAX_CHARACTERISTICS {
                return Err(format!(
                    "service {:04x} holds at most {} characteristics",
                    service.uuid,
                    litz_ble::MAX_CHARACTERISTICS
                ));
            }
            for characteristic in &service.characteristics {
                if characteristic.value.len() > litz_ble::MAX_VALUE {
                    return Err(format!(
                        "characteristic {:04x} is {} bytes; the limit is {}",
                        characteristic.uuid,
                        characteristic.value.len(),
                        litz_ble::MAX_VALUE
                    ));
                }
            }
        }
        self.gatt.borrow_mut().insert(board, services);
        Ok(())
    }

    /// The table `board` serves, as registered.
    pub fn gatt_table(&self, board: u16) -> Vec<litz_ble::GattService> {
        self.gatt.borrow().get(&board).cloned().unwrap_or_default()
    }

    /// Connect to the advertiser at `address` from the station on `central`.
    ///
    /// The table is the advertiser's: a circuit beacon's `gatt` attribute, or
    /// the table its owning board registered. An advertiser without a table is
    /// a pure beacon and refuses the connection, as a real one would.
    pub fn gatt_connect(&self, central: u16, address: &[u8]) -> Result<u16, String> {
        let address: [u8; 6] = address
            .try_into()
            .map_err(|_| "a BLE address is six octets, least significant first")?;
        let air = self.ble_air(central);
        let Some(beacon) = air.iter().find(|beacon| beacon.address == address) else {
            return Err(format!(
                "{} is not advertising in range",
                litz_ble::address_text(&address)
            ));
        };
        let peripheral = self
            .advertised
            .borrow()
            .iter()
            .find(|(_, advertisement)| advertisement.address == address)
            .map(|(owner, _)| *owner);
        let table = self.gatt_current(&address, peripheral);
        if table.is_empty() {
            return Err(format!(
                "{} is a beacon; it does not accept connections",
                beacon.address_text()
            ));
        }
        if self.links.borrow().len() >= MAX_GATT_LINKS {
            return Err(format!(
                "the air already carries {MAX_GATT_LINKS} GATT links"
            ));
        }
        let handle = self.free_link_handle();
        self.links.borrow_mut().push(GattLink {
            handle,
            central,
            peripheral,
            address,
            subscriptions: Vec::new(),
            notifications: Vec::new(),
            next_event: 0,
        });
        Ok(handle)
    }

    /// Move `handle` to its next connection event and say how much simulated
    /// time that costs the central: the wait until the event plus the air the
    /// request and its response occupy.
    ///
    /// A connection is not a pipe. Every ATT request rides one connection
    /// event, events repeat once per interval, and a guest that issues two
    /// reads back to back waits a whole interval between them. A host that
    /// calls this after each GATT operation spends the result through its own
    /// clock, so the timing is visible in a trace, in the guest's timers and
    /// in the pcap rather than collapsing to zero.
    pub fn gatt_advance(&self, central: u16, handle: u16, now: Tick) -> u64 {
        let mut links = self.links.borrow_mut();
        let Some(link) = links
            .iter_mut()
            .find(|link| link.handle == handle && link.central == central)
        else {
            return 0;
        };
        let event = link.next_event.max(now);
        link.next_event = event.saturating_add(CONNECTION_INTERVAL_NS);
        (event - now).saturating_add(CONNECTION_EVENT_NS)
    }

    /// When the next connection event on `handle` starts, without moving it.
    /// A central that signed a value or is polling notifications uses this to
    /// wait for the event that carries it.
    pub fn gatt_next_event(&self, central: u16, handle: u16) -> Option<Tick> {
        self.links
            .borrow()
            .iter()
            .find(|link| link.handle == handle && link.central == central)
            .map(|link| link.next_event)
    }

    /// Close one connection.
    pub fn gatt_disconnect(&self, central: u16, handle: u16) -> Result<(), String> {
        let mut links = self.links.borrow_mut();
        let Some(index) = links
            .iter()
            .position(|link| link.handle == handle && link.central == central)
        else {
            return Err(format!("no connection {handle} on this station"));
        };
        links.remove(index);
        Ok(())
    }

    /// The services and characteristics of a connection, without values.
    pub fn gatt_discover(
        &self,
        central: u16,
        handle: u16,
    ) -> Result<Vec<litz_ble::GattService>, String> {
        let (address, peripheral) = self.link(central, handle)?;
        Ok(litz_ble::gatt::discovery(
            &self.gatt_current(&address, peripheral),
        ))
    }

    /// Read one characteristic.
    pub fn gatt_read(
        &self,
        central: u16,
        handle: u16,
        service: u16,
        characteristic: u16,
    ) -> Result<Vec<u8>, String> {
        let (address, peripheral) = self.link(central, handle)?;
        let table = self.gatt_current(&address, peripheral);
        let found = litz_ble::gatt::characteristic(&table, service, characteristic)
            .ok_or_else(|| not_found(service, characteristic))?;
        if !found.readable() {
            return Err(format!(
                "{service:04x}/{characteristic:04x} is not readable"
            ));
        }
        Ok(found.value.clone())
    }

    /// Write one characteristic. The peripheral reads the write through
    /// [`Board::gatt_take_writes`].
    pub fn gatt_write(
        &self,
        central: u16,
        handle: u16,
        service: u16,
        characteristic: u16,
        value: &[u8],
    ) -> Result<(), String> {
        if value.len() > litz_ble::MAX_VALUE {
            return Err(format!(
                "a write is {} bytes; the limit is {}",
                value.len(),
                litz_ble::MAX_VALUE
            ));
        }
        let (address, peripheral) = self.link(central, handle)?;
        let mut table = self.gatt_current(&address, peripheral);
        let found = litz_ble::gatt::characteristic(&table, service, characteristic)
            .ok_or_else(|| not_found(service, characteristic))?;
        if !found.writable() {
            return Err(format!(
                "{service:04x}/{characteristic:04x} is not writable"
            ));
        }
        litz_ble::gatt::set_value(&mut table, service, characteristic, value.to_vec());
        self.gatt_overrides.borrow_mut().insert(address, table);
        if let Some(owner) = peripheral {
            let mut writes = self.writes.borrow_mut();
            let queue = writes.entry(owner).or_default();
            if queue.len() >= MAX_PENDING_WRITES {
                queue.remove(0);
            }
            queue.push(litz_ble::GattWrite {
                service,
                characteristic,
                value: value.to_vec(),
            });
        }
        Ok(())
    }

    /// Subscribe to (or stop subscribing to) a characteristic's notifications.
    pub fn gatt_subscribe(
        &self,
        central: u16,
        handle: u16,
        service: u16,
        characteristic: u16,
        enabled: bool,
    ) -> Result<(), String> {
        let (address, peripheral) = self.link(central, handle)?;
        let table = self.gatt_current(&address, peripheral);
        let found = litz_ble::gatt::characteristic(&table, service, characteristic)
            .ok_or_else(|| not_found(service, characteristic))?;
        if enabled && !found.notifiable() {
            return Err(format!(
                "{service:04x}/{characteristic:04x} does not notify"
            ));
        }
        let mut links = self.links.borrow_mut();
        let link = links
            .iter_mut()
            .find(|link| link.handle == handle && link.central == central)
            .ok_or_else(|| format!("no connection {handle} on this station"))?;
        link.subscriptions
            .retain(|subscription| *subscription != (service, characteristic));
        if enabled {
            link.subscriptions.push((service, characteristic));
        }
        Ok(())
    }

    /// Notifications received on a connection since the last call.
    pub fn gatt_notifications(
        &self,
        central: u16,
        handle: u16,
    ) -> Result<Vec<litz_ble::GattNotification>, String> {
        let mut links = self.links.borrow_mut();
        let link = links
            .iter_mut()
            .find(|link| link.handle == handle && link.central == central)
            .ok_or_else(|| format!("no connection {handle} on this station"))?;
        Ok(std::mem::take(&mut link.notifications))
    }

    /// Writes centrals made to `board`'s table since the last call.
    pub fn gatt_take_writes(&self, board: u16) -> Vec<litz_ble::GattWrite> {
        self.writes.borrow_mut().remove(&board).unwrap_or_default()
    }

    /// Notify every subscribed central that `board`'s characteristic changed.
    pub fn gatt_notify(
        &self,
        board: u16,
        service: u16,
        characteristic: u16,
        value: &[u8],
    ) -> Result<(), String> {
        if value.len() > litz_ble::MAX_VALUE {
            return Err(format!(
                "a notification is {} bytes; the limit is {}",
                value.len(),
                litz_ble::MAX_VALUE
            ));
        }
        let mut table = self.gatt_table(board);
        if table.is_empty() {
            return Err("this station has no GATT table".into());
        }
        let found = litz_ble::gatt::characteristic(&table, service, characteristic)
            .ok_or_else(|| not_found(service, characteristic))?;
        if !found.notifiable() {
            return Err(format!(
                "{service:04x}/{characteristic:04x} does not notify"
            ));
        }
        litz_ble::gatt::set_value(&mut table, service, characteristic, value.to_vec());
        self.gatt.borrow_mut().insert(board, table);

        // Every open link owns a copy of its peripheral's current values, so a
        // central that already wrote sees the notification's value too.
        let addresses: Vec<[u8; 6]> = self
            .links
            .borrow()
            .iter()
            .filter(|link| link.peripheral == Some(board))
            .map(|link| link.address)
            .collect();
        for address in &addresses {
            let mut current = self.gatt_current(address, Some(board));
            litz_ble::gatt::set_value(&mut current, service, characteristic, value.to_vec());
            self.gatt_overrides.borrow_mut().insert(*address, current);
        }
        let mut links = self.links.borrow_mut();
        for link in links.iter_mut() {
            if link.peripheral != Some(board) {
                continue;
            }
            if link.subscriptions.contains(&(service, characteristic)) {
                link.notifications.push(litz_ble::GattNotification {
                    service,
                    characteristic,
                    value: value.to_vec(),
                });
                // A central that does not poll yet cannot grow the queue.
                if link.notifications.len() > MAX_PENDING_WRITES {
                    link.notifications.remove(0);
                }
            }
        }
        Ok(())
    }

    /// Register `board` as a station on the shared packet medium, or retune
    /// the one it already has. A custom chip uses this to put raw frames on
    /// the same air the nRF24, SX1276 and AT86RF233 parts share.
    pub fn radio_configure(
        &self,
        board: u16,
        network: &str,
        channel: u16,
        address: &[u8],
    ) -> Result<(), String> {
        if network.is_empty() {
            return Err("a radio needs a network name".into());
        }
        if network.len() > 32 {
            return Err("a network name is at most 32 bytes".into());
        }
        if address.len() > 8 {
            return Err("a radio address is at most eight bytes".into());
        }
        let mut ids = self.radio_ids.borrow_mut();
        let mut hub = self.radios.borrow_mut();
        match ids.get(&board) {
            Some(id) => {
                hub.set_network(*id, network.to_string());
                hub.set_channel(*id, channel);
                hub.set_address(*id, address.to_vec());
            }
            None => {
                let id = hub.register(litz_radio::RadioConfig {
                    network: network.to_string(),
                    channel,
                    address: address.to_vec(),
                    power_dbm: 0,
                    range_m: None,
                });
                ids.insert(board, id);
            }
        }
        Ok(())
    }

    /// Put one raw frame on the air at `now`. `airtime_ns` is the caller's
    /// modulation result, as it is for every other part on this medium.
    pub fn radio_send(
        &self,
        board: u16,
        bytes: &[u8],
        to: Option<&[u8]>,
        airtime_ns: u64,
        now: Tick,
    ) -> Result<(), String> {
        if bytes.is_empty() {
            return Err("a radio frame is at least one byte".into());
        }
        if bytes.len() > MAX_RADIO_PAYLOAD {
            return Err(format!(
                "a radio frame is at most {MAX_RADIO_PAYLOAD} bytes"
            ));
        }
        if let Some(to) = to
            && to.len() > 8
        {
            return Err("a radio address is at most eight bytes".into());
        }
        let id =
            *self.radio_ids.borrow().get(&board).ok_or_else(|| {
                "this station has no radio; call radio-configure first".to_string()
            })?;
        self.radios
            .borrow_mut()
            .transmit(id, now, bytes, airtime_ns, to)
            .ok_or_else(|| "the radio is not registered on the medium".to_string())?;
        let end = now.saturating_add(airtime_ns);
        let mut clock = self.radio_clock.borrow_mut();
        *clock = (*clock).max(end);
        Ok(())
    }

    /// The furthest point in simulated time any station has used the medium.
    pub fn radio_clock(&self) -> Tick {
        *self.radio_clock.borrow()
    }

    /// Frames that have finished by `now` and are addressed to `board`,
    /// with the sender's address alongside each one.
    pub fn radio_receive(&self, board: u16, now: Tick) -> Vec<(Vec<u8>, litz_radio::Packet)> {
        let ids = self.radio_ids.borrow();
        let Some(id) = ids.get(&board).copied() else {
            return Vec::new();
        };
        let mut hub = self.radios.borrow_mut();
        let packets = hub.inbox(id, now);
        packets
            .into_iter()
            .map(|packet| {
                let from = hub
                    .config(packet.from)
                    .map(|config| config.address.clone())
                    .unwrap_or_default();
                (from, packet)
            })
            .collect()
    }

    fn link(&self, central: u16, handle: u16) -> Result<([u8; 6], Option<u16>), String> {
        self.links
            .borrow()
            .iter()
            .find(|link| link.handle == handle && link.central == central)
            .map(|link| (link.address, link.peripheral))
            .ok_or_else(|| format!("no connection {handle} on this station"))
    }

    fn gatt_current(
        &self,
        address: &[u8; 6],
        peripheral: Option<u16>,
    ) -> Vec<litz_ble::GattService> {
        if let Some(table) = self.gatt_overrides.borrow().get(address) {
            return table.clone();
        }
        if let Some(owner) = peripheral
            && let Some(table) = self.gatt.borrow().get(&owner)
        {
            return table.clone();
        }
        self.bluetooth
            .beacons
            .iter()
            .find(|beacon| &beacon.address == address)
            .map(|beacon| beacon.gatt.clone())
            .unwrap_or_default()
    }

    fn free_link_handle(&self) -> u16 {
        let mut next = self.next_link.borrow_mut();
        loop {
            let handle = (*next).max(1);
            *next = handle.wrapping_add(1).max(1);
            if !self.links.borrow().iter().any(|link| link.handle == handle) {
                return handle;
            }
        }
    }

    pub fn network(&self) -> Option<&litz_net::NetworkConfig> {
        self.network.as_ref()
    }

    /// Replace the network the board carries, which is how a runner attaches
    /// a capture or a `--wifi` override before any chip is built.
    pub fn set_network(&mut self, network: litz_net::NetworkConfig) {
        self.network = Some(network);
    }

    /// The earliest tick at which a part wants its own clock moved.
    ///
    /// An adapter that only pushes pin changes never needs this; one that
    /// schedules chip work (a QEMU-style backend, or the esp32sim bus) uses it
    /// to know when to call [`Board::advance_to`].
    pub fn next_event(&self) -> Option<Tick> {
        self.parts.iter().filter_map(|part| part.next_event()).min()
    }

    /// Move every part's own clock forward to `now`.
    ///
    /// Advancing can change a part's drive, which changes a net, which can
    /// schedule another event in the same window; the loop repeats until no
    /// part has work due. Every round settles at `now`, so a chip reading its
    /// inputs afterwards sees the final levels rather than an intermediate
    /// one. A part that never stops asking for the past is cut off after
    /// [`MAX_ADVANCE_ROUNDS`] and reported as a board conflict.
    pub fn advance_to(&mut self, now: Tick) {
        for _ in 0..MAX_ADVANCE_ROUNDS {
            let Some(at) = self.next_event().filter(|at| *at <= now) else {
                return;
            };
            // Move only the parts that are due at the earliest tick, then
            // settle there. Doing them one tick at a time is what keeps two
            // events in the same call from collapsing into the later one in a
            // waveform or in a chip's input queue.
            for part in self.parts.iter_mut() {
                while let Some(next) = part.next_event() {
                    if next > at {
                        break;
                    }
                    part.advance(next);
                }
            }
            // A part's own clock is a drive change like any other, so the
            // cached operating point belongs to the old state. A transient run
            // keeps its history: its steps are the caller's to take.
            self.analog_cache.get_mut().take();
            self.settle(at);
        }
        self.conflicts.push(Conflict {
            net: u16::MAX,
            detail: "a part kept scheduling events in the past".into(),
        });
    }

    /// Whether this board participates in analog co-simulation.
    pub fn analog_enabled(&self) -> bool {
        self.analog
    }

    /// Whether analog co-simulation is on. A short alias for
    /// [`Board::analog_enabled`].
    pub fn analog(&self) -> bool {
        self.analog
    }

    /// Turn analog co-simulation on or off.
    ///
    /// Off is the default and the fast path: the digital runtime behaves
    /// exactly as it always has and no netlist is ever built. On makes
    /// [`Board::analog_inputs`] prefer a solved node voltage for an analog
    /// pin, and [`Board::state`] report an LED from its solved current.
    pub fn set_analog(&mut self, enabled: bool) {
        self.analog = enabled;
        // Turning analog off (or on) starts a fresh analog view: a transient
        // history belongs to the old mode and cannot be resumed.
        self.clear_analog();
    }

    /// A board with analog co-simulation already enabled, for a one-expression
    /// construction.
    pub fn with_analog(mut self, enabled: bool) -> Self {
        self.analog = enabled;
        self
    }

    /// Whether a transient is live. False is DC mode, the default.
    pub fn analog_transient_active(&self) -> bool {
        self.analog_transient.is_some()
    }

    /// The simulated time of the live transient, in nanoseconds. `None` in DC
    /// mode.
    pub fn analog_time(&self) -> Option<Tick> {
        self.analog_transient.as_ref().map(|run| run.at_ns)
    }

    /// Advance the analog transient by one step of `dt_ns` nanoseconds and
    /// return the new node voltages.
    ///
    /// The first call starts a zero-state transient (every node at 0 V, every
    /// inductor at 0 A) from the board's current wiring and drives, so a
    /// capacitor charges from discharged rather than from its DC steady state.
    /// Every later call continues the same run; only the step size, a drive,
    /// a press, a control or a configuration change starts a new one. To step
    /// without owning the step-size choice, use [`Board::analog_advance`].
    pub fn analog_transient(&mut self, dt_ns: u64) -> Result<AnalogSolution, String> {
        if dt_ns == 0 {
            return Err("analog: a transient step must be a positive number of nanoseconds".into());
        }
        let restart = match &self.analog_transient {
            Some(run) => run.dt_ns != dt_ns,
            None => true,
        };
        if restart {
            self.start_transient(dt_ns)?;
        }
        self.step_transient(dt_ns)
    }

    /// Advance the analog transient to `to_ns`, stepping in equal `dt` slices,
    /// and return the new node voltages.
    ///
    /// `to_ns` is the chip's time, so an adapter calls this with the tick it
    /// has just reached. The run keeps its own clock, starting at 0 when
    /// transient mode begins; `to_ns` is that clock's time. The step size is
    /// the one chosen by the most recent [`Board::analog_transient`], or
    /// [`ANALOG_DEFAULT_DT_NS`] if none was chosen. The final slice is
    /// shortened so the run lands exactly on `to_ns`. Asking for an earlier
    /// time is an error; asking for the current time is a no-op that returns
    /// the last solution.
    pub fn analog_advance(&mut self, to_ns: Tick) -> Result<AnalogSolution, String> {
        if self.analog_transient.is_none() {
            self.start_transient(ANALOG_DEFAULT_DT_NS)?;
        }
        let at_ns = self
            .analog_transient
            .as_ref()
            .map(|run| run.at_ns)
            .unwrap_or(0);
        if to_ns < at_ns {
            return Err(format!(
                "analog: cannot advance the transient backwards from {at_ns} ns to {to_ns} ns"
            ));
        }
        let mut steps = 0u64;
        while let Some((at_ns, dt_ns)) = self.transient_clock() {
            if at_ns >= to_ns {
                break;
            }
            let step = (to_ns - at_ns).min(dt_ns);
            self.step_transient(step)?;
            steps += 1;
            if steps > MAX_TRANSIENT_STEPS {
                return Err(format!(
                    "analog: advancing to {to_ns} ns needs more than {MAX_TRANSIENT_STEPS} steps; \
                     call analog_transient with a larger step"
                ));
            }
        }
        self.analog_transient
            .as_ref()
            .map(|run| run.solution.clone())
            .ok_or_else(|| "analog: the transient did not start".to_string())
    }

    /// The live transient's time and step size, copied out so a stepping loop
    /// does not hold a borrow of the run it is about to advance.
    fn transient_clock(&self) -> Option<(Tick, u64)> {
        self.analog_transient
            .as_ref()
            .map(|run| (run.at_ns, run.dt_ns))
    }

    /// Build a fresh zero-state transient from the current wiring and drives.
    fn start_transient(&mut self, dt_ns: u64) -> Result<(), String> {
        let circuit = analog::build_operating(
            &self.flat,
            &self.chip_drive,
            &self.parts,
            &self.analog_config,
            true,
        )?;
        let run = litz_analog::Transient::with_zero_state(circuit.netlist())
            .map_err(|e| format!("analog: the transient could not start: {e}"))?;
        let solution = circuit.initial_solution();
        self.analog_transient = Some(TransientRun {
            run,
            circuit,
            dt_ns,
            at_ns: 0,
            solution,
        });
        Ok(())
    }

    /// Step the live transient by `dt_ns`, if there is one.
    fn step_transient(&mut self, dt_ns: u64) -> Result<AnalogSolution, String> {
        let config = self.analog_config;
        let run = self
            .analog_transient
            .as_mut()
            .ok_or_else(|| "analog: no transient is running".to_string())?;
        let solved = run
            .run
            .step(dt_ns as f64 * 1e-9)
            .map_err(|e| format!("analog: transient step failed: {e}"))?;
        run.at_ns = run
            .at_ns
            .checked_add(dt_ns)
            .ok_or_else(|| "analog: transient time overflowed".to_string())?;
        run.solution = run.circuit.solution(&solved, &config);
        Ok(run.solution.clone())
    }

    /// Drop the cached DC solution and any transient history.
    ///
    /// This is the circuit-invalidation rule in one place: a new chip drive, a
    /// press, a control change or a configuration change makes every earlier
    /// voltage stale, and a transient may not splice old state onto new
    /// topology.
    fn clear_analog(&mut self) {
        self.analog_cache.get_mut().take();
        self.analog_transient = None;
    }

    /// The rail and driver values the analog view uses.
    pub fn analog_config(&self) -> AnalogConfig {
        self.analog_config
    }

    /// Replace the analog configuration and drop any cached solution or transient.
    pub fn set_analog_config(&mut self, config: AnalogConfig) {
        self.analog_config = config;
        self.clear_analog();
    }

    /// Solve the board's DC operating point.
    ///
    /// This is always the DC solve, even while a transient is live: use
    /// [`Board::analog_transient`] or [`Board::analog_advance`] to move the
    /// transient and [`Board::node_voltage`] or [`Board::analog_inputs`] to
    /// read it. The solution is cached until a chip drive, a press, a control
    /// change or a configuration change invalidates it, so reading several
    /// pins costs one solve. A board with no analog elements solves to an
    /// empty solution; a node the solver cannot reference is reported as
    /// floating (`None`), never as 0 V.
    pub fn analog_solve(&self) -> Result<AnalogSolution, String> {
        self.analog_dc_solution()
    }

    /// Voltage on one named pin of a named part, solving if necessary.
    /// `None` when the pin is floating or the solve failed.
    pub fn node_voltage(&self, part: &str, pin: &str) -> Option<f64> {
        self.analog_solution().ok()?.node_voltage(part, pin)
    }

    /// Voltage on a chip GPIO of board 0, solving if necessary. `None` when
    /// floating.
    pub fn node_voltage_gpio(&self, gpio: u8) -> Option<f64> {
        self.node_voltage_gpio_on(0, gpio)
    }

    /// Voltage on GPIO `gpio` of board `board`, solving if necessary.
    pub fn node_voltage_gpio_on(&self, board: u16, gpio: u8) -> Option<f64> {
        self.analog_solution()
            .ok()?
            .node_voltage_gpio_on(board, gpio)
    }

    /// The operating point a reader sees.
    ///
    /// In transient mode that is the last transient step; otherwise it is the
    /// cached DC solve. Keeping this separate from [`Board::analog_solve`]
    /// means a test can still ask for the DC point while a transient is live.
    fn analog_solution(&self) -> Result<AnalogSolution, String> {
        if let Some(run) = &self.analog_transient {
            return Ok(run.solution.clone());
        }
        self.analog_dc_solution()
    }

    fn analog_dc_solution(&self) -> Result<AnalogSolution, String> {
        if let Some(cached) = (*self.analog_cache.borrow()).clone() {
            return Ok(cached);
        }
        let solution = analog::solve(
            &self.flat,
            &self.chip_drive,
            &self.parts,
            &self.analog_config,
        )?;
        *self.analog_cache.borrow_mut() = Some(solution.clone());
        Ok(solution)
    }

    /// The tick the board last settled at. A backend that drives a device
    /// byte by byte uses it to stamp the transaction it is about to describe.
    pub fn now(&self) -> Tick {
        self.now
    }

    /// Record the baud a backend configured for a UART. Reconstructed bytes
    /// and the UART decoder use it; without a call the port keeps
    /// [`OBSERVED_UART_BAUD`].
    pub fn set_uart_baud(&mut self, board: u16, port: u8, baud: u32) {
        if baud > 0 {
            self.bus_rates.uart.insert((board, port), baud);
        }
    }

    /// The configured baud for a UART, or the observation default.
    pub fn uart_baud(&self, board: u16, port: u8) -> u32 {
        self.bus_rates
            .uart
            .get(&(board, port))
            .copied()
            .unwrap_or(OBSERVED_UART_BAUD)
    }

    /// Record the SCL rate a backend configured for an I2C controller.
    pub fn set_i2c_hz(&mut self, board: u16, controller: u8, hz: u32) {
        if hz > 0 {
            self.bus_rates.i2c.insert((board, controller), hz);
        }
    }

    /// The configured I2C rate, or the observation default.
    pub fn i2c_hz(&self, board: u16, controller: u8) -> u32 {
        self.bus_rates
            .i2c
            .get(&(board, controller))
            .copied()
            .unwrap_or(OBSERVED_I2C_HZ)
    }

    /// Record the SCK rate a backend configured for an SPI controller.
    pub fn set_spi_hz(&mut self, board: u16, controller: u8, hz: u32) {
        if hz > 0 {
            self.bus_rates.spi.insert((board, controller), hz);
        }
    }

    /// The configured SPI rate, or the observation default.
    pub fn spi_hz(&self, board: u16, controller: u8) -> u32 {
        self.bus_rates
            .spi
            .get(&(board, controller))
            .copied()
            .unwrap_or(OBSERVED_SPI_HZ)
    }

    /// Every bus on the board. A backend reaches a device through
    /// `buses_mut`; a test reads back the same record the runtime uses.
    /// Save the image of every SPI microSD card on this board whose guest
    /// wrote a block. One card writes to `path` itself; several treat `path`
    /// as a directory and write `<controller>-cs<select>.img` in it. Returns
    /// the files written (empty when nothing changed).
    pub fn save_sd_images(&mut self, path: &Path) -> Result<Vec<String>, String> {
        let mut cards: Vec<(String, u64)> = Vec::new();
        for (&(_, controller), spi) in &self.buses.spi {
            for select in spi.chip_selects().collect::<Vec<_>>() {
                if let Some(sd) = spi.device_as::<crate::storage::MicroSd>(select) {
                    cards.push((
                        format!("sd{controller}-cs{select}.img"),
                        sd.written_blocks(),
                    ));
                }
            }
        }
        if cards.is_empty() {
            return Err(format!(
                "{}: this board has no SPI microSD card",
                path.display()
            ));
        }
        let mut written = Vec::new();
        if cards.len() == 1 {
            let mut index = 0;
            for spi in self.buses.spi.values_mut() {
                for select in spi.chip_selects().collect::<Vec<_>>() {
                    let Some(sd) = spi.device_as_mut::<crate::storage::MicroSd>(select) else {
                        continue;
                    };
                    if index == 0
                        && sd
                            .save_changed(path)
                            .map_err(|e| format!("{}: {e}", path.display()))?
                    {
                        written.push(path.display().to_string());
                    }
                    index += 1;
                }
            }
            return Ok(written);
        }
        std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut index = 0;
        for spi in self.buses.spi.values_mut() {
            for select in spi.chip_selects().collect::<Vec<_>>() {
                let Some(sd) = spi.device_as_mut::<crate::storage::MicroSd>(select) else {
                    continue;
                };
                let file = path.join(&cards[index].0);
                if sd
                    .save_changed(&file)
                    .map_err(|e| format!("{}: {e}", file.display()))?
                {
                    written.push(file.display().to_string());
                }
                index += 1;
            }
        }
        Ok(written)
    }

    pub fn buses(&self) -> &Buses {
        &self.buses
    }

    pub fn buses_mut(&mut self) -> &mut Buses {
        &mut self.buses
    }

    /// The current pixels of the display part `part_id`, if it has one.
    ///
    /// `part_id` is the diagram id (`tft`, `oled`, `paper`), not the catalog
    /// type key. The built-in displays with a whole-frame model are covered —
    /// ILI9341, SSD1306, the e-paper panels, a MAX7219 chain and the Nokia
    /// 5110. A pin-level display (an HD44780 character LCD) and an unknown id
    /// both answer `None`.
    ///
    /// The pixels are copied out of the bus device, so the board may keep
    /// running while a caller encodes or compares the image.
    pub fn part_framebuffer(&self, part_id: &str) -> Option<Framebuffer> {
        let part = self.flat.parts.iter().find(|part| part.id == part_id)?;
        let Engine::Builtin(engine) = &part.engine else {
            return None;
        };
        match (engine.as_str(), part.device?) {
            (
                "ili9341",
                Device::Spi {
                    board,
                    controller,
                    chip_select,
                },
            ) => {
                let panel = self
                    .buses
                    .spi
                    .get(&(board, controller))?
                    .device_as::<crate::displays::Ili9341>(chip_select)?;
                let (pixels, width, height) = panel.framebuffer();
                Some(Framebuffer::from_rgb565(pixels, width, height))
            }
            (
                "st7735",
                Device::Spi {
                    board,
                    controller,
                    chip_select,
                },
            ) => {
                let panel = self
                    .buses
                    .spi
                    .get(&(board, controller))?
                    .device_as::<crate::displays::St7735>(chip_select)?;
                let (pixels, width, height) = panel.framebuffer();
                Some(Framebuffer::from_rgb565(pixels, width, height))
            }
            (
                "ssd1306",
                Device::I2c {
                    board,
                    controller,
                    address,
                },
            ) => {
                let panel = self
                    .buses
                    .i2c
                    .get(&(board, controller))?
                    .device_as::<crate::bus_parts::Ssd1306>(address)?;
                let (bytes, width, height) = panel.framebuffer();
                Some(Framebuffer::from_page_bits_lsb_first(bytes, width, height))
            }
            (
                "sh1106",
                Device::I2c {
                    board,
                    controller,
                    address,
                },
            ) => {
                let panel = self
                    .buses
                    .i2c
                    .get(&(board, controller))?
                    .device_as::<crate::bus_parts::Sh1106>(address)?;
                let (bytes, width, height) = panel.framebuffer();
                Some(Framebuffer::from_page_bits_lsb_first(bytes, width, height))
            }
            (
                "epaper",
                Device::Spi {
                    board,
                    controller,
                    chip_select,
                },
            ) => {
                let panel = self
                    .buses
                    .spi
                    .get(&(board, controller))?
                    .device_as::<crate::displays::Epd>(chip_select)?;
                Some(Framebuffer::from_row_bits_msb_first(
                    panel.displayed(),
                    panel.width() as u32,
                    panel.height() as u32,
                ))
            }
            (
                "max7219",
                Device::Spi {
                    board,
                    controller,
                    chip_select,
                },
            ) => {
                let matrix = self
                    .buses
                    .spi
                    .get(&(board, controller))?
                    .device_as::<crate::protocol_displays::Max7219>(chip_select)?;
                let width = matrix.chain() as u32 * 8;
                let mut pixels = vec![0u8; width as usize * 8 * 3];
                for y in 0..8 {
                    for x in 0..width as usize {
                        if matrix.pixel(x, y) {
                            let at = (y * width as usize + x) * 3;
                            pixels[at..at + 3].fill(0xff);
                        }
                    }
                }
                Some(Framebuffer::rgb8(width, 8, pixels))
            }
            (
                "pcd8544",
                Device::Spi {
                    board,
                    controller,
                    chip_select,
                },
            ) => {
                let panel = self
                    .buses
                    .spi
                    .get(&(board, controller))?
                    .device_as::<crate::protocol_displays::Pcd8544>(chip_select)?;
                Some(Framebuffer::from_page_bits_lsb_first(
                    panel.framebuffer(),
                    panel.width() as u32,
                    panel.height() as u32,
                ))
            }
            _ => None,
        }
    }

    /// Put a finger on (or lift it from) the first XPT2046 touch controller on
    /// any SPI controller of any board.
    ///
    /// `x` and `y` are the controller's raw 12-bit codes (`0..=4095`); the
    /// panel state is clamped to that range. The point is what the next SPI
    /// read of the X and Y channels returns, and `down` is the pen state the
    /// controller's `touched` reports. A board with no XPT2046 is an error, so
    /// a scenario cannot silently press a panel that is not there.
    ///
    /// A TFT module's touch controller is a separate chip from its display, so
    /// this never touches the display model.
    pub fn touch_panel(&mut self, x: u16, y: u16, down: bool) -> Result<(), String> {
        for spi in self.buses.spi.values_mut() {
            for chip_select in spi.chip_selects().collect::<Vec<_>>() {
                if let Some(touch) = spi.device_as_mut::<crate::touch::Xpt2046>(chip_select) {
                    touch.touch(x, y, down);
                    return Ok(());
                }
            }
        }
        Err("this board has no XPT2046 touch controller on any SPI bus".to_string())
    }

    /// An I2C register read on `controller` of `board`, the transaction a
    /// driver writes. The transaction is stamped at the board's current tick.
    pub fn i2c_write_read(
        &mut self,
        board: u16,
        controller: u8,
        address: u8,
        write: &[u8],
        len: usize,
    ) -> Option<Vec<u8>> {
        let at = self.now;
        self.i2c_write_read_at(board, controller, address, write, len, at)
    }

    /// An I2C transaction that starts at `at`, which is the tick a probe shows
    /// the first START on. A driver with its own clock passes it; everything
    /// else uses [`Board::i2c_write_read`].
    pub fn i2c_write_read_at(
        &mut self,
        board: u16,
        controller: u8,
        address: u8,
        write: &[u8],
        len: usize,
        at: Tick,
    ) -> Option<Vec<u8>> {
        let wiring = self.flat.buses.i2c.get(&(board, controller)).cloned()?;
        let read = self
            .buses
            .i2c
            .get_mut(&(board, controller))?
            .write_read(address, write, len)?;
        if self.trace.is_some() {
            let hz = self.i2c_hz(board, controller);
            let edges = waveform::i2c(wiring.sda, wiring.scl, at, hz, address, write, &read);
            // Both lines are pulled up while the bus is idle, which is what
            // makes START a falling edge rather than an arbitrary one.
            self.observe(
                board,
                at,
                &[(wiring.sda, Level::High), (wiring.scl, Level::High)],
                &edges,
            );
        }
        Some(read)
    }

    /// A full SPI transaction on one chip select of `board`. An unpopulated
    /// select reads 0xFF, the floating MISO a real controller sees.
    pub fn spi_transfer(
        &mut self,
        board: u16,
        controller: u8,
        chip_select: u8,
        tx: &[u8],
        rx_len: usize,
    ) -> Vec<u8> {
        let at = self.now;
        self.spi_transfer_at(board, controller, chip_select, tx, rx_len, at)
    }

    /// A full SPI transaction that starts at `at`.
    pub fn spi_transfer_at(
        &mut self,
        board: u16,
        controller: u8,
        chip_select: u8,
        tx: &[u8],
        rx_len: usize,
        at: Tick,
    ) -> Vec<u8> {
        let wiring = self.flat.buses.spi.get(&(board, controller)).cloned();
        let rx = match self.buses.spi.get_mut(&(board, controller)) {
            Some(spi) => spi.transfer(chip_select, tx, rx_len),
            None => vec![0xff; rx_len],
        };
        if self.trace.is_some()
            && let Some(wiring) = wiring
        {
            let hz = self.spi_hz(board, controller);
            let edges = waveform::spi(
                wiring.sck,
                wiring.mosi,
                wiring.miso,
                chip_select,
                at,
                hz,
                OBSERVED_SPI_MODE,
                tx,
                &rx,
            );
            let mut seeds = vec![(chip_select, Level::High)];
            // The clock idles at CPOL, and data lines idle low when nobody is
            // clocking them.
            seeds.push((
                wiring.sck,
                if OBSERVED_SPI_MODE & 2 != 0 {
                    Level::High
                } else {
                    Level::Low
                },
            ));
            for pin in [wiring.mosi, wiring.miso].into_iter().flatten() {
                seeds.push((pin, Level::Low));
            }
            self.observe(board, at, &seeds, &edges);
        }
        rx
    }

    /// Bytes the chip on `board` transmitted reach the part on `port`. False
    /// when no part is wired there.
    pub fn uart_receive(&mut self, board: u16, port: u8, at: Tick, bytes: &[u8]) -> bool {
        let wiring = self.flat.buses.uart.get(&(board, port)).cloned();
        let delivered = match self.buses.uart.get_mut(&(board, port)) {
            Some(uart) => uart.receive(port, at, bytes),
            None => false,
        };
        if delivered
            && self.trace.is_some()
            && let Some(wiring) = wiring
        {
            // The chip drives `chip_tx`; the part hears it. The part's own
            // answers are observed when the chip polls them.
            let baud = self.uart_baud(board, port);
            let edges = waveform::uart(wiring.chip_tx, None, at, baud, bytes, &[]);
            self.observe(board, at, &[(wiring.chip_tx, Level::High)], &edges);
        }
        delivered
    }

    /// Bytes the part on `board`'s `port` has for the chip.
    pub fn uart_poll(&mut self, board: u16, port: u8, at: Tick) -> Vec<u8> {
        let wiring = self.flat.buses.uart.get(&(board, port)).cloned();
        let bytes = self
            .buses
            .uart
            .get_mut(&(board, port))
            .map(|uart| uart.poll(port, at))
            .unwrap_or_default();
        if self.trace.is_some()
            && !bytes.is_empty()
            && let Some(wiring) = wiring
        {
            {
                // The part's transmission reaches the chip on `chip_rx`.
                let baud = self.uart_baud(board, port);
                let edges =
                    waveform::uart(wiring.chip_tx, Some(wiring.chip_rx), at, baud, &[], &bytes);
                self.observe(
                    board,
                    at,
                    &[(wiring.chip_tx, Level::High), (wiring.chip_rx, Level::High)],
                    &edges,
                );
            }
        }
        bytes
    }

    /// Record an I2C transaction a backend performed byte by byte.
    ///
    /// `esp32sim` and the AVR core drive a bus device through `I2c`'s
    /// callbacks rather than [`Board::i2c_write_read_at`], so they hand the
    /// completed write and read phases here. `at` is when the START happened.
    pub fn observe_i2c(
        &mut self,
        board: u16,
        controller: u8,
        address: u8,
        write: &[u8],
        read: &[u8],
        at: Tick,
    ) {
        if self.trace.is_none() {
            return;
        }
        let Some(wiring) = self.flat.buses.i2c.get(&(board, controller)).cloned() else {
            return;
        };
        let hz = self.i2c_hz(board, controller);
        let edges = waveform::i2c(wiring.sda, wiring.scl, at, hz, address, write, read);
        self.observe(
            board,
            at,
            &[(wiring.sda, Level::High), (wiring.scl, Level::High)],
            &edges,
        );
    }

    /// File the edges of a bus transaction against the nets their pins belong
    /// to.
    ///
    /// This is the one place a byte-level transfer becomes a waveform. The
    /// edges are held separately from `trace` because they can be stamped
    /// ahead of the tick the host runs at: a UART byte written at `now` takes
    /// ten bit times, so the last edge belongs to a tick the board has not
    /// settled yet. Merging at read time keeps both orders intact.
    fn observe(&mut self, board: u16, at: Tick, idle: &[(u8, Level)], edges: &[waveform::Edge]) {
        for &(pin, level) in idle {
            let endpoint = Endpoint::chip(board, pin);
            let Some(&net) = self.flat.endpoint_net.get(&endpoint) else {
                continue;
            };
            let known = self
                .observed_levels
                .get(&net)
                .copied()
                .unwrap_or(self.net_level[net as usize]);
            if known == Level::HighZ {
                // A bus line no part drives is pulled to its idle level. The
                // probe sees that before the first start bit or START, and a
                // decoder needs it to tell the first edge from a level that
                // was already held.
                self.observed.push((at, net, level));
                self.observed_levels.insert(net, level);
            }
        }
        for edge in edges {
            let endpoint = Endpoint::chip(board, edge.pin);
            if let Some(&net) = self.flat.endpoint_net.get(&endpoint) {
                self.observed_levels.insert(net, edge.level);
                self.observed.push((edge.at, net, edge.level));
            }
        }
    }

    /// A chip pin started or stopped driving. `drive` carries both the level
    /// and whether the pin is an output at all.
    ///
    /// This names GPIO `gpio` of board 0, which is the board every backend
    /// that predates multi-board diagrams drives. A second board's chip uses
    /// [`Board::set_chip_output_on`].
    pub fn set_chip_output(&mut self, gpio: u8, drive: Drive, now: Tick) {
        self.set_chip_output_on(0, gpio, drive, now);
    }

    /// A pin of board `board` started or stopped driving.
    ///
    /// `board` is the index in [`FlatBoard::boards`], the same index
    /// [`Endpoint::ChipOn`] uses. Two boards' GPIO 4 are independent pads on
    /// one shared net list, so a change on one settles the nets and can reach
    /// the other exactly as a wire between their header pins says.
    pub fn set_chip_output_on(&mut self, board: u16, gpio: u8, drive: Drive, now: Tick) {
        if self.chip_drive.get(&(board, gpio)) == Some(&drive) {
            return;
        }
        self.chip_drive.insert((board, gpio), drive);
        self.clear_analog();
        self.settle(now);
    }

    /// Levels board 0's chip should apply to its own input pads. `HighZ` means
    /// the pad is floating and the host drive should be released.
    pub fn take_chip_inputs(&mut self) -> Vec<(u8, Level)> {
        self.take_chip_inputs_on(0)
    }

    /// Levels board `board`'s chip should apply to its own input pads.
    ///
    /// Only that board's pending changes are drained; another board's queue is
    /// left for its own backend.
    pub fn take_chip_inputs_on(&mut self, board: u16) -> Vec<(u8, Level)> {
        let mut mine = Vec::new();
        let mut rest = Vec::with_capacity(self.pending_inputs.len());
        for (at, pin, level) in std::mem::take(&mut self.pending_inputs) {
            if at == board {
                mine.push((pin, level));
            } else {
                rest.push((at, pin, level));
            }
        }
        self.pending_inputs = rest;
        mine
    }

    /// Levels the board currently holds on board 0's chip pins, for attaching
    /// a backend that needs the state before any edge is produced.
    pub fn input_levels(&self) -> Vec<(u8, Level)> {
        self.input_levels_on(0)
    }

    /// Levels board `board` currently holds on its chip pins.
    pub fn input_levels_on(&self, board: u16) -> Vec<(u8, Level)> {
        self.chip_level
            .iter()
            .filter(|((at, _), _)| *at == board)
            .map(|((_, pin), level)| (*pin, *level))
            .collect()
    }

    /// Whether board 0's chip has levels waiting.
    pub fn has_pending_inputs(&self) -> bool {
        self.has_pending_inputs_on(0)
    }

    /// Whether board `board`'s chip has levels waiting.
    pub fn has_pending_inputs_on(&self, board: u16) -> bool {
        self.pending_inputs.iter().any(|(at, _, _)| *at == board)
    }

    /// The level board `board`'s chip sees on `gpio`, if that pad is wired.
    pub fn chip_level_on(&self, board: u16, gpio: u8) -> Option<Level> {
        self.chip_level.get(&(board, gpio)).copied()
    }

    /// How many development boards the diagram places.
    ///
    /// This is the number of indices [`Board::set_chip_output_on`] and
    /// [`Board::take_chip_inputs_on`] accept. A hand-built flat board with no
    /// board records but wired chip pins still counts as one chip, so the
    /// value is never below one when any chip pin exists.
    pub fn chips(&self) -> usize {
        let from_pins = self
            .flat
            .nets
            .iter()
            .flat_map(|members| members.iter())
            .filter_map(|endpoint| endpoint.chip_pin().map(|(board, _)| board as usize + 1))
            .max()
            .unwrap_or(0);
        from_pins.max(self.flat.boards.len())
    }

    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    pub fn state(&self, id: &str) -> Option<PartState> {
        let index = self.flat.parts.iter().position(|p| p.id == id)?;
        let state = self.parts[index].state();
        // With analog on, an LED's brightness is its forward current, not the
        // digital level on its pins: a diode below its forward voltage is
        // dark even when the anode's net is high.
        if self.analog
            && matches!(state, PartState::Led { .. })
            && let Ok(solution) = self.analog_solution()
            && let Some(lit) = solution.is_lit(id)
        {
            return Some(PartState::Led { lit });
        }
        Some(state)
    }

    /// The chip pin a named part is wired to, which is what a test script
    /// addresses: `press btn1` becomes a level on that GPIO.
    ///
    /// The board is dropped, so this keeps its old meaning for board 0. Use
    /// [`Board::named_pin_on`] when the part may sit on another board and the
    /// board index matters.
    pub fn named_pin(&self, id: &str) -> Option<u8> {
        self.named_chip_pin(id).map(|(_, gpio)| gpio)
    }

    /// The board index and chip pin a named part is wired to.
    pub fn named_pin_on(&self, id: &str) -> Option<(u16, u8)> {
        self.named_chip_pin(id)
    }

    fn named_chip_pin(&self, id: &str) -> Option<(u16, u8)> {
        let index = self.flat.parts.iter().position(|p| p.id == id)? as u16;
        for members in &self.flat.nets {
            if members
                .iter()
                .any(|e| matches!(e, Endpoint::Part { part, .. } if *part == index))
                && let Some(chip) = members.iter().find_map(|e| e.chip_pin())
            {
                return Some(chip);
            }
        }
        None
    }

    /// A host pressed or released a named part. Returns false when the part
    /// does not take a press (an LED, a resistor), so a UI can tell.
    pub fn press(&mut self, id: &str, pressed: bool, now: Tick) -> bool {
        let Some(index) = self.flat.parts.iter().position(|p| p.id == id) else {
            return false;
        };
        if !self.parts[index].press(pressed) {
            return false;
        }
        self.clear_analog();
        self.settle(now);
        true
    }

    /// A host moved a continuous control on a named part, in permille. Returns
    /// false when the part has no such control.
    pub fn set_control(&mut self, id: &str, value: u16, now: Tick) -> bool {
        let Some(index) = self.flat.parts.iter().position(|p| p.id == id) else {
            return false;
        };
        if !self.parts[index].set_control(value) {
            return false;
        }
        self.clear_analog();
        self.settle(now);
        true
    }

    /// A host set a named control (`pressed`, `temperature`, `lux`) on a part.
    /// The part reads the text in its own units; the board only settles the
    /// nets afterwards so the new value is visible to the chip at `now`.
    pub fn set_named_control(&mut self, id: &str, name: &str, value: &str, now: Tick) -> bool {
        let Some(index) = self.flat.parts.iter().position(|p| p.id == id) else {
            return false;
        };
        // Bus devices are not `Part`s: an I²C sensor's registers live behind
        // its controller, so its named controls are reached through the same
        // address the firmware talks to.
        let mpu = matches!(
            &self.flat.parts[index].engine,
            Engine::Builtin(name) if name == "mpu6050"
        );
        if mpu
            && let Some(Device::I2c {
                board,
                controller,
                address,
            }) = self.flat.parts[index].device
            && let Some(sensor) = self
                .buses
                .i2c
                .get_mut(&(board, controller))
                .and_then(|i2c| i2c.device_as_mut::<crate::bus_parts::Mpu6050>(address))
            && sensor.set_named_control(name, value)
        {
            self.clear_analog();
            self.settle(now);
            return true;
        }
        if !self.parts[index].set_named_control(name, value) {
            return false;
        }
        self.clear_analog();
        self.settle(now);
        true
    }

    /// Analog voltages board 0 holds on its chip pins, from parts that source
    /// one. A digital-only board returns nothing, and the pads keep whatever
    /// the backend gives them.
    pub fn analog_inputs(&self) -> Vec<(u8, f32)> {
        self.analog_inputs_on(0)
    }

    /// Analog voltages board `board` holds on its chip pins.
    ///
    /// A net is reported only to the board that has a chip pin on it, so one
    /// board's GPIO 4 cannot overwrite another's. A net spanning both boards
    /// reports the same voltage to both, which is what an ADC on either end
    /// would read.
    pub fn analog_inputs_on(&self, board: u16) -> Vec<(u8, f32)> {
        // With analog on, a solved node voltage is the truth and the part's
        // static `analog()` value is only the fallback when the solve failed.
        // With analog off, nothing is solved and the static path is untouched.
        let solved = if self.analog {
            self.analog_solution().ok()
        } else {
            None
        };
        let mut volts: BTreeMap<u8, f32> = BTreeMap::new();
        for members in &self.flat.nets {
            let Some(gpio) = members
                .iter()
                .filter_map(|e| e.chip_pin())
                .find_map(|(at, pin)| (at == board).then_some(pin))
            else {
                continue;
            };
            for &endpoint in members {
                let Endpoint::Part { part, pin } = endpoint else {
                    continue;
                };
                // A pin is analog either because its kind says so (a sensor
                // output) or because the part sources a sampled value (the
                // potentiometer's wiper).
                let analog_kind = self.flat.parts[part as usize]
                    .pin_kinds
                    .get(pin as usize)
                    .is_some_and(|kind| {
                        matches!(
                            kind,
                            PinKind::AnalogIn | PinKind::AnalogOut | PinKind::AnalogIo
                        )
                    });
                let static_value = self.parts[part as usize].analog(PinId(pin));
                if !analog_kind && static_value.is_none() {
                    continue;
                }
                let value = match &solved {
                    Some(solution) => match solution.node_voltage_pin(part, pin) {
                        Some(volts) => volts as f32,
                        // A floating analog node is not a voltage; leave the
                        // pad alone rather than inventing one.
                        None => continue,
                    },
                    None => match static_value {
                        Some(value) => value,
                        None => continue,
                    },
                };
                volts.insert(gpio, value);
            }
        }
        volts.into_iter().collect()
    }

    /// The resolved level on one pin of a named part, whichever net it sits on.
    /// A scenario asks about a pin this way because a part in series with a
    /// resistor is two nets away from the chip and still has a level.
    pub fn part_pin_level(&self, part: &str, pin: &str) -> Option<Level> {
        let index = self.flat.parts.iter().position(|p| p.id == part)? as u16;
        let flat_part = self.flat.parts.get(index as usize)?;
        let pin_index = flat_part.pins.iter().position(|name| name == pin)? as u16;
        for (id, members) in self.flat.nets.iter().enumerate() {
            let here = members.iter().any(
                |e| matches!(e, Endpoint::Part { part: p, pin: q } if *p == index && *q == pin_index),
            );
            if here {
                return Some(self.net_level[id]);
            }
        }
        None
    }

    /// The level the chip sees on the pin a named part is wired to.
    ///
    /// The board is the one the part's net actually reaches, so this is
    /// already board-correct for a part on any board. Use
    /// [`Board::chip_pin_level_on`] to insist on a particular board.
    pub fn chip_pin_level(&self, part: &str) -> Option<Level> {
        let (board, gpio) = self.named_chip_pin(part)?;
        self.chip_level_on(board, gpio)
    }

    /// The level board `board`'s chip sees on the pin a named part is wired
    /// to. `None` when the part is not wired to that board.
    pub fn chip_pin_level_on(&self, board: u16, part: &str) -> Option<Level> {
        let (at, gpio) = self.named_chip_pin(part)?;
        (at == board)
            .then(|| self.chip_level_on(board, gpio))
            .flatten()
    }

    pub fn part_ids(&self) -> impl Iterator<Item = &str> {
        self.flat.parts.iter().map(|p| p.id.as_str())
    }

    /// Begin recording net transitions for [`Board::vcd`]. Time 0 of the
    /// waveform is the moment this is called, so a trace started after boot
    /// shows the board as the chip first sees it.
    pub fn start_trace(&mut self) {
        self.trace_initial = self.net_level.clone();
        self.trace_started = self.now;
        self.trace = Some(Vec::new());
        self.observed.clear();
        self.observed_levels.clear();
        self.analog_trace = Some(Vec::new());
    }

    /// Watch a point in the circuit.
    ///
    /// `point` is either `<board>:<GPIO>` with a board instance id and a GPIO
    /// number (`esp:GPIO8`, `b:4`) or `<part>:<pin>` with a part id and the
    /// pin name the catalog gives it (`led1:A`, `esp/led_pwr:A`). The name is
    /// what the host prints and draws, so it has to be unique.
    ///
    /// A probe records nothing until [`Board::start_trace`]; registering one
    /// after a run has begun starts its channel empty rather than inventing a
    /// past.
    pub fn add_probe(&mut self, name: &str, point: &str) -> Result<(), String> {
        if self.probes.iter().any(|probe| probe.name == name) {
            return Err(format!("probe {name:?} is already defined"));
        }
        let endpoint = self.probe_endpoint(point)?;
        let net = *self
            .flat
            .endpoint_net
            .get(&endpoint)
            .ok_or_else(|| format!("{point:?} is not wired to anything"))?;
        self.probes.push(Probe {
            name: name.to_string(),
            point: point.to_string(),
            net,
        });
        Ok(())
    }

    /// The points this board is watching.
    pub fn probes(&self) -> &[Probe] {
        &self.probes
    }

    /// Every probe's record, with the measurements taken from it.
    ///
    /// The digital part is the net trace and the analog part is the solved
    /// voltage at each settle, so a channel is a step waveform: it holds the
    /// last value between changes rather than sampling a fixed grid.
    pub fn scope(&self) -> Vec<Channel> {
        let recorded = self.recorded();
        probe::channels(
            &self.probes,
            &recorded,
            self.analog_trace.as_deref().unwrap_or(&[]),
            &self.trace_initial,
            self.trace_started,
        )
    }

    /// The net transitions the scope and the VCD read: settled edges and
    /// per-peripheral edges in one time-ordered record.
    fn recorded(&self) -> Vec<(Tick, u16, Level)> {
        let mut all = self.trace.clone().unwrap_or_default();
        all.extend(self.observed.iter().copied());
        all.sort_by_key(|(at, ..)| *at);
        all
    }

    /// Resolve `point` to the endpoint its net is found through.
    fn probe_endpoint(&self, point: &str) -> Result<Endpoint, String> {
        let (head, tail) = point.split_once(':').ok_or_else(|| {
            format!(
                "{point:?} is not a point; write <board>:<GPIO> or <part>:<pin>, \
                 e.g. {}:GPIO8 or led1:A",
                self.flat
                    .board_ids
                    .first()
                    .map(String::as_str)
                    .unwrap_or("esp")
            )
        })?;
        if let Some(board) = self.flat.board_ids.iter().position(|id| id == head) {
            let number = tail.strip_prefix("GPIO").unwrap_or(tail);
            let gpio: u8 = number.parse().map_err(|_| {
                format!("{point:?}: {tail:?} is not a GPIO number on board {head:?}")
            })?;
            return Ok(Endpoint::chip(board as u16, gpio));
        }
        let Some(part) = self.flat.parts.iter().position(|part| part.id == head) else {
            return Err(format!(
                "{point:?}: {head:?} is neither a board ({}) nor a part in diagram.json",
                self.flat.board_ids.join(", ")
            ));
        };
        let flat_part = &self.flat.parts[part];
        let pin = flat_part
            .pins
            .iter()
            .position(|name| name == tail)
            .ok_or_else(|| {
                format!(
                    "{point:?}: part {head:?} has no pin {tail:?}; its pins are {}",
                    flat_part.pins.join(", ")
                )
            })?;
        Ok(Endpoint::Part {
            part: part as u16,
            pin: pin as u16,
        })
    }

    /// Record the solved voltage on every probed net.
    ///
    /// Called after each settle while a trace is running. The solution only
    /// changes when a drive does, so the analog channel is sampled at change
    /// points rather than on a fixed time grid: a scope draws exactly the
    /// states the circuit was in, with no resampling error.
    fn sample_analog(&mut self, now: Tick) {
        if !self.analog || self.probes.is_empty() || self.analog_trace.is_none() {
            return;
        }
        let Ok(solution) = self.analog_solution() else {
            return;
        };
        let samples: Vec<(u16, f64)> = self
            .probes
            .iter()
            .filter_map(|probe| {
                solution
                    .node_voltage_net(probe.net)
                    .map(|volts| (probe.net, volts))
            })
            .collect();
        let Some(trace) = self.analog_trace.as_mut() else {
            return;
        };
        for (net, volts) in samples {
            // A fixed-point settle runs several passes at one tick; the state
            // that matters is the settled one, so a repeated sample replaces
            // the earlier value instead of stacking at the same instant.
            let duplicate = trace
                .iter()
                .rev()
                .take_while(|(at, ..)| *at == now)
                .any(|(_, recorded, _)| *recorded == net);
            if duplicate {
                if let Some(last) = trace
                    .iter_mut()
                    .rev()
                    .find(|(at, recorded, _)| *at == now && *recorded == net)
                {
                    last.2 = volts;
                }
            } else {
                trace.push((now, net, volts));
            }
        }
    }

    /// How many transitions have been recorded, for a caller that wants to
    /// bound a capture.
    pub fn trace_len(&self) -> usize {
        self.trace.as_ref().map_or(0, Vec::len) + self.observed.len()
    }

    /// The recorded waveform as a Value Change Dump, with one channel per net.
    ///
    /// Nanoseconds are the timescale because that is the simulator's unit; a
    /// viewer such as PulseView or GTKWave reads the file directly. Net names
    /// carry their first endpoint (`net3_esp_GPIO8`) so a waveform is readable
    /// without cross-referencing the diagram.
    pub fn vcd(&self) -> String {
        let mut out = String::from("$timescale 1ns $end\n$scope module litz $end\n");
        for id in 0..self.flat.nets.len() {
            out.push_str(&format!("$var wire 1 n{id} {} $end\n", self.net_name(id)));
        }
        out.push_str("$upscope $end\n$enddefinitions $end\n#0\n");
        for (id, level) in self.trace_initial.iter().enumerate() {
            out.push_str(&format!("{}n{id}\n", vcd_value(*level)));
        }
        let mut last = 0;
        for (at, net, level) in self.recorded() {
            if at != last {
                out.push_str(&format!("#{at}\n"));
                last = at;
            }
            out.push_str(&format!("{}n{net}\n", vcd_value(level)));
        }
        out
    }

    fn net_name(&self, id: usize) -> String {
        let mut name = format!("net{id}");
        if let Some(endpoint) = self.flat.nets[id].first() {
            let label = match *endpoint {
                // Board 0 keeps the old `gpio4` name so a single-board VCD is
                // byte-for-byte unchanged; another board is prefixed so the
                // two `gpio4`s in one waveform cannot collide.
                Endpoint::Chip(gpio) => format!("gpio{gpio}"),
                Endpoint::ChipOn { board, pin } => format!("b{board}_gpio{pin}"),
                Endpoint::Part { part, pin } => {
                    let flat = &self.flat.parts[part as usize];
                    format!(
                        "{}_{}",
                        flat.id,
                        flat.pins.get(pin as usize).map_or("?", String::as_str)
                    )
                }
                Endpoint::Rail(rail) => match rail {
                    crate::Rail::Gnd => "gnd".into(),
                    crate::Rail::V33 => "v33".into(),
                    crate::Rail::V5 => "v5".into(),
                    crate::Rail::Vin => "vin".into(),
                },
            };
            name.push('_');
            name.push_str(&label);
        }
        name.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn settle(&mut self, now: Tick) -> bool {
        self.now = now;
        let mut converged = false;
        for _ in 0..MAX_SETTLE_ITERATIONS {
            let mut changed = false;
            {
                let Self {
                    flat,
                    parts,
                    net_level,
                    chip_drive,
                    trace,
                    ..
                } = self;
                for (id, members) in flat.nets.iter().enumerate() {
                    let level = resolve(flat, members, chip_drive, parts).unwrap_or(Level::HighZ);
                    if level != net_level[id] {
                        net_level[id] = level;
                        changed = true;
                        if let Some(trace) = trace {
                            trace.push((now, id as u16, level));
                        }
                        for endpoint in members {
                            if let Endpoint::Part { part, pin } = *endpoint {
                                parts[part as usize].set_pin(PinId(pin), level, now);
                            }
                        }
                    }
                }
            }
            if !changed {
                converged = true;
                break;
            }
        }

        let mut conflicts = Vec::new();
        if !converged {
            conflicts.push(Conflict {
                net: u16::MAX,
                detail: "the board did not settle; a passive loop is feeding itself".into(),
            });
        }
        for (id, members) in self.flat.nets.iter().enumerate() {
            if let Err(detail) = resolve(&self.flat, members, &self.chip_drive, &self.parts) {
                conflicts.push(Conflict {
                    net: id as u16,
                    detail,
                });
            }
        }
        self.conflicts = conflicts;

        let mut pending = std::mem::take(&mut self.pending_inputs);
        for (id, members) in self.flat.nets.iter().enumerate() {
            let level = self.net_level[id];
            for endpoint in members {
                let Some((board, gpio)) = endpoint.chip_pin() else {
                    continue;
                };
                if self.chip_level.get(&(board, gpio)) != Some(&level) {
                    self.chip_level.insert((board, gpio), level);
                    pending.push((board, gpio, level));
                }
            }
        }
        self.pending_inputs = pending;
        let unsettled = !self.pending_inputs.is_empty();
        self.sample_analog(now);
        unsettled
    }
}

/// Build a part's digital behaviour, tolerating an analog-only engine that
/// this build does not implement yet.
///
/// A capacitor, inductor, transistor or op-amp only matters to the analog
/// view; its pins carry no digital drive. If `parts::build` reports that the
/// engine is not in this build, a silent shell keeps the pins as net
/// endpoints so the analog solver can read them. A genuine error from a known
/// engine is still propagated.
fn build_part(
    part: &crate::flatten::FlatPart,
    radios: &RadioHubHandle,
) -> Result<Box<dyn Part>, BuildError> {
    match parts::build(part, radios) {
        Ok(built) => Ok(built),
        Err(error)
            if analog::is_analog_only_engine(part)
                && error.to_string().contains("which this build does not have") =>
        {
            Ok(Box::new(parts::BusShell))
        }
        Err(error) => Err(error),
    }
}

/// The one-character value a VCD uses for a level.
fn vcd_value(level: Level) -> char {
    match level {
        Level::Low => '0',
        Level::High => '1',
        Level::HighZ => 'z',
    }
}

fn resolve(
    flat: &FlatBoard,
    members: &[Endpoint],
    chip_drive: &BTreeMap<(u16, u8), Drive>,
    parts: &[Box<dyn Part>],
) -> Result<Level, String> {
    let mut strong: Option<(Level, Endpoint)> = None;
    let mut weak: Option<(Level, Endpoint)> = None;
    let mut weak_split = false;
    for &endpoint in members {
        let drive = match endpoint {
            Endpoint::Chip(_) | Endpoint::ChipOn { .. } => {
                // `chip_pin` never fails for these two arms; the board index
                // is what keeps two GPIO 4s apart in the drive map.
                let (board, gpio) = endpoint.chip_pin().expect("chip endpoint");
                chip_drive
                    .get(&(board, gpio))
                    .copied()
                    .unwrap_or(Drive::HIGH_Z)
            }
            Endpoint::Part { part, pin } => parts[part as usize].drive(PinId(pin)),
            Endpoint::Rail(rail) => Drive::strong(rail.level()),
        };
        if !drive.is_driving() {
            continue;
        }
        let slot = match drive.strength {
            Strength::Strong => &mut strong,
            Strength::Weak => &mut weak,
        };
        match slot {
            None => *slot = Some((drive.level, endpoint)),
            Some((level, _)) if *level == drive.level => {}
            Some((_, first)) if drive.strength == Strength::Strong => {
                return Err(format!(
                    "{} and {} drive the net to opposite levels",
                    describe(flat, *first),
                    describe(flat, endpoint)
                ));
            }
            // Two resistors pulling apart is a divider, not a fault: legal
            // hardware holding an indeterminate level. Two states cannot say
            // "half way", so the net reads as floating.
            Some(_) => weak_split = true,
        }
    }
    Ok(match strong {
        Some((level, _)) => level,
        None if weak_split => Level::HighZ,
        None => weak.map_or(Level::HighZ, |(level, _)| level),
    })
}

fn describe(flat: &FlatBoard, endpoint: Endpoint) -> String {
    match endpoint {
        // Board 0 keeps the wording every existing conflict message used; a
        // second board is named so a short between two boards is readable.
        Endpoint::Chip(gpio) => format!("chip GPIO{gpio}"),
        Endpoint::ChipOn { board, pin } => format!("chip {board} GPIO{pin}"),
        Endpoint::Rail(rail) => format!("{rail:?}"),
        Endpoint::Part { part, pin } => {
            let p = &flat.parts[part as usize];
            let name = p.pins.get(pin as usize).map(String::as_str).unwrap_or("?");
            format!("{}:{name}", p.id)
        }
    }
}
