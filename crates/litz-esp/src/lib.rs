//! ESP32-family chips.
//!
//! The adapter is generic over esp32sim's `Soc`: one `EspChip<S>` drives any chip
//! that crate family models, so adding a chip is a type alias, a bus factory and
//! a board manifest rather than a second copy of this file. ESP32-C3 is wired
//! today; the Xtensa parts (ESP32, S3) and the remaining RISC-V parts (C6, P4,
//! S31) follow the same shape.
//!
//! The mask ROM is not vendored here. Espressif publishes the ROM ELFs under
//! Apache-2.0 at `espressif/esp-rom-elfs`; a chip loads one at runtime from a
//! path or from an embedded blob the caller supplies, so the crate stays free
//! of binaries and the licence question stays where Espressif put it.
// The esp32sim backend is a `std` crate, so the no_std promise only holds
// when that backend is off.
#![cfg_attr(
    not(any(
        feature = "esp32",
        feature = "esp32c3",
        feature = "esp32c6",
        feature = "esp32s3",
        feature = "esp32p4",
        feature = "esp32s31"
    )),
    no_std
)]
#![forbid(unsafe_code)]

extern crate alloc;

use litz_mcu::MemoryRegion;

/// The simulated network behind an access point: DHCP/DNS/ICMP from esp32sim,
/// TCP and UDP from Litz's own service table.
#[cfg(any(
    feature = "esp32",
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32s3",
    feature = "esp32p4",
    feature = "esp32s31"
))]
pub mod router;

// The debug surface, the display capture and the GDB stub are siblings of
// `backend`: each needs the machine, which only the backend may hold, and each
// gates itself on the chips it can serve.
pub mod debug;
pub mod gdb;
pub mod screenshot;

pub use litz_net::{NetworkConfig, Service};
#[cfg(any(
    feature = "esp32",
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32s3",
    feature = "esp32p4",
    feature = "esp32s31"
))]
pub use router::EspRouter;

/// Which chip a [`Config`] describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    /// RV32IMC, 160 MHz, PLIC, no atomics.
    Esp32C3,
    /// RV32IMAC, 160 MHz, PLIC.
    Esp32C6,
    /// RV32IMAC, 96 MHz, PLIC, no radio.
    Esp32H2,
}

/// External flash layout, in the order the ROM expects to find images.
#[derive(Clone, Copy, Debug)]
pub struct FlashLayout {
    pub size: u32,
    /// Offset of the second-stage bootloader.
    pub bootloader_offset: u32,
    /// Offset of the partition table.
    pub partition_table_offset: u32,
    /// Offset of the application image.
    pub app_offset: u32,
}

impl Chip {
    pub const fn memory_map(self) -> &'static [MemoryRegion] {
        // Addresses are from the ESP32-C3/C6 technical reference manuals.
        match self {
            Chip::Esp32C3 | Chip::Esp32C6 | Chip::Esp32H2 => &PLIC_AND_RV_MAP,
        }
    }

    pub const fn flash_layout(self) -> FlashLayout {
        FlashLayout {
            size: 4 * 1024 * 1024,
            bootloader_offset: 0x0,
            partition_table_offset: 0x8000,
            app_offset: 0x1_0000,
        }
    }
}

/// Adapter over `esp32sim`'s ESP32-family machines.
///
/// This exists to answer one question with a build rather than an opinion:
/// can a mature, MIT-licensed ESP32 emulator be driven as a library from
/// outside its own repo? Everything above this module — diagrams, parts, the
/// CI runner — only needs the [`litz_board::Chip`] calls, so the backend can be
/// swapped for a vendored copy or an in-tree core later without touching them.
#[cfg(any(
    feature = "esp32",
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32s3",
    feature = "esp32p4",
    feature = "esp32s31"
))]
pub mod backend {
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use esp_periph::i2c::I2cDevice as EspI2cDevice;
    use esp_soc::ble::peer::Nearby;
    use esp_soc::board::{BoardEdge, BoardModel, SpiPins};
    use esp_soc::uart::{UartInput, UartRoute};
    use esp_soc::{Machine, Soc, SocBus};
    use litz_board::{Board as LitzBoard, Conflict};
    use litz_core::{Drive, Level, PartState};

    /// Builds a chip's bus with the Litz board already on it.
    ///
    /// Each esp32sim crate has its own `SocBus::new` signature — the S3 takes a
    /// PSRAM size, the P4 a wafer revision, the S31 nothing — so the factory is
    /// the one per-chip detail, and everything above it is generic. The cell
    /// carries the GP-SPI clock the guest programmed, from an MMIO observer to
    /// the board model that reconstructs the wires.
    pub type BusFactory<S> = fn(
        [u8; 6],
        usize,
        Option<Rc<RefCell<LitzBoard>>>,
        u16,
        Rc<Cell<[u32; 2]>>,
    ) -> <S as Soc>::Bus;

    /// An esp32sim machine attached to a Litz board.
    pub struct EspChip<S: Soc> {
        machine: Machine<S>,
        board: Option<Rc<RefCell<LitzBoard>>>,
        /// Kept so `with_board` can rebuild the machine; the bus owns the board
        /// model and esp32sim has no setter after construction.
        mac: [u8; 6],
        flash: usize,
        bus_for: BusFactory<S>,
        /// A bootloader image was placed, so the machine starts from it
        /// rather than from a direct `boot_app`.
        bootloader_loaded: bool,
        /// The analog values last written to the pads, so a run that changes
        /// nothing does not reprogram the ADC every step.
        analog: Vec<(u8, f32)>,
        /// The firmware's ELF, when the project supplies one. esp32sim's HCI
        /// Bluetooth controller is started by patching the guest's controller
        /// task, so it needs the symbol table; the same symbols name addresses
        /// in logs, disassembly and the debugger.
        elf: Option<Vec<u8>>,
        /// Serves a GDB client, when this chip's core is one the stub can
        /// describe. The per-chip constructor installs it; the Xtensa parts
        /// leave it empty, because their windowed register file needs a
        /// different target description rather than a made-up one.
        gdb: Option<GdbServeFn<S>>,
        /// Calls a guest function on a core this adapter can drive, installed
        /// by that chip's constructor: the flat RV32 register file, or a
        /// windowed Xtensa `call8` with a saved core around it.
        guest_call: Option<GuestCall<S>>,
        /// Which development-board instance on the diagram these pads belong
        /// to. A diagram with one board is board 0.
        board_index: u16,
        /// The chip's simulated time, shared with the network adapter so a
        /// packet capture can timestamp the frames it records.
        net_clock: Rc<Cell<u64>>,
        /// The controller is on and its scan list came from the board, so a
        /// runtime advertiser has to be mirrored on the next step.
        ble_attached: bool,
        /// The board's BLE generation the controller's scan list reflects.
        ble_seen: u64,
        /// The controller's scan list as this adapter last gave it, so a peer
        /// added while the simulation runs can be appended to it.
        ble_peers: Vec<litz_ble::Beacon>,
        /// Reads and writes the USB Serial/JTAG console on chips that have one.
        /// A firmware built for USB CDC (CircuitPython, TinyUSB, HWCDC) prints
        /// there and not on UART0, so the console has to follow both.
        /// The GP-SPI clock registers a firmware write feeds to the board model.
        spi_clock: Option<SpiClockSpec>,
        /// The latest programmed rate per GP-SPI host (2 and 3), written by an
        /// MMIO observer and read when a transfer reaches the board.
        spi_rates: Rc<Cell<[u32; 2]>>,
    }

    /// Starts a GDB session over one chip.
    pub type GdbServeFn<S> = fn(&mut EspChip<S>, u16) -> Result<(), String>;

    /// Calls a guest function with no arguments and returns its result: `a0` on
    /// RV32, and the windowed callee's `a2` on Xtensa (the caller's `a10` once
    /// `retw` restores the register base).
    pub type GuestCall<S> = fn(&mut EspChip<S>, u32) -> Result<u32, String>;

    /// The entry a RISC-V chip's constructor installs. The stub is only compiled for the
    /// RISC-V parts, so an Xtensa-only feature set must not reference it.
    #[cfg(any(
        feature = "esp32c3",
        feature = "esp32c6",
        feature = "esp32p4",
        feature = "esp32s31"
    ))]
    fn serve_gdb<S: crate::debug::Rv32Chip>(
        chip: &mut EspChip<S>,
        port: u16,
    ) -> Result<(), String> {
        crate::gdb::serve(chip, port)
    }

    impl<S: Soc> EspChip<S> {
        /// A chip with no board on it. Public to the crate's own constructors;
        /// callers use the per-chip `new`.
        fn bare(mac: [u8; 6], flash: usize, bus_for: BusFactory<S>) -> Self {
            let spi_rates = Rc::new(Cell::new([0; 2]));
            let machine = Self::build(mac, flash, bus_for, None, 0, Rc::clone(&spi_rates));
            Self {
                machine,
                board: None,
                mac,
                flash,
                bus_for,
                bootloader_loaded: false,
                analog: Vec::new(),
                elf: None,
                gdb: None,
                guest_call: None,
                board_index: 0,
                net_clock: Rc::new(Cell::new(0)),
                ble_attached: false,
                ble_seen: 0,
                ble_peers: Vec::new(),
                spi_clock: None,
                spi_rates,
            }
        }

        fn build(
            mac: [u8; 6],
            flash: usize,
            bus_for: BusFactory<S>,
            board: Option<Rc<RefCell<LitzBoard>>>,
            index: u16,
            spi_rates: Rc<Cell<[u32; 2]>>,
        ) -> Machine<S> {
            let mut machine = Machine::new(mac, bus_for(mac, flash, board, index, spi_rates));
            // The scheduling batching knobs default from `ESP32SIM_VQ` /
            // `ESP32SIM_BB` at construction; pin them to the plain one-quantum
            // schedule, which is the bit-exact reference the batching is
            // defined against. A simulator whose behaviour depends on the CI
            // runner's environment cannot be a test gate, so the machine is
            // built directly rather than through `esp32c3::machine`.
            machine.vq_max = 1;
            machine.bb_max = 1;
            // The console prints to stdout unless captured; a library caller
            // wants the bytes, not the side effect.
            machine.console.capture = true;
            machine
        }

        /// Attach a flattened board. Rebuilds the machine, so it must be called
        /// before images are loaded (which is how every caller uses it).
        pub fn with_board(self, board: LitzBoard) -> Self {
            let shared = Rc::new(RefCell::new(board));
            self.with_shared_board(shared, 0)
        }

        /// Attach a board another chip already shares, and say which board
        /// instance these pads belong to.
        ///
        /// A diagram with more than one MCU has one [`LitzBoard`] and one chip
        /// per development-board instance; the board is the same object, and
        /// the index keeps two boards' GPIO 4 apart. The machine is rebuilt
        /// here, which is why images are loaded after this call.
        pub fn with_shared_board(mut self, board: Rc<RefCell<LitzBoard>>, index: u16) -> Self {
            self.machine = Self::build(
                self.mac,
                self.flash,
                self.bus_for,
                Some(Rc::clone(&board)),
                index,
                Rc::clone(&self.spi_rates),
            );
            self.board = Some(board);
            self.board_index = index;
            // The rebuild replaces the machine, so the symbols and the clock
            // observer go back on it. The bytes parsed when they were accepted,
            // and parsing is pure.
            if let Some(bytes) = &self.elf {
                let _ = self.machine.add_symbols(bytes);
            }
            self.install_spi_observer();
            self
        }

        /// Watch the GP-SPI clock registers, when this chip has them and a
        /// board is attached.
        ///
        /// A firmware write is seen by an MMIO observer and stored in
        /// `spi_rates`; the board model reports it at the transfer itself, so
        /// the reconstructed wires use the divider the driver programmed
        /// rather than the board's default. A chip with no board reconstructs
        /// nothing, so it does not pay for an MMIO log. Installing twice is a
        /// no-op.
        fn install_spi_observer(&mut self) {
            if self.board.is_none() {
                return;
            }
            let Some(spec) = self.spi_clock else {
                return;
            };
            if self.machine.has_observer("litz-spi-clock") {
                return;
            }
            self.machine.add_observer(Box::new(SpiClockObserver {
                rates: Rc::clone(&self.spi_rates),
                spec,
            }));
        }

        /// The machine, for the sibling modules that capture a display or
        /// answer a debugger. Every esp32sim type still stays in this crate.
        pub(crate) fn machine_ref(&self) -> &Machine<S> {
            &self.machine
        }

        pub(crate) fn machine_mut(&mut self) -> &mut Machine<S> {
            &mut self.machine
        }

        /// Serve one GDB client on `127.0.0.1:port`, returning when it
        /// disconnects.
        pub fn gdb_serve(&mut self, port: u16) -> Result<(), String> {
            match self.gdb {
                Some(serve) => serve(self, port),
                None => Err(format!(
                    "{} has no GDB target: only the RISC-V chips describe a register file",
                    S::NAME
                )),
            }
        }

        /// Give the machine the firmware's symbol table.
        ///
        /// A project that has the ELF next to its image should always pass it:
        /// the Bluetooth controller esp32sim runs is installed by patching the
        /// guest's own controller task, which is only reachable by symbol, and
        /// every log line, disassembly and breakpoint address benefits from
        /// names. Call it after [`EspChip::with_board`], which rebuilds the
        /// machine and would otherwise drop the symbols.
        pub fn with_elf(mut self, bytes: &[u8]) -> Result<Self, String> {
            self.machine.add_symbols(bytes)?;
            self.elf = Some(bytes.to_vec());
            Ok(self)
        }

        /// Whether a second-stage bootloader was loaded. With one, the boot
        /// sequence is the bootloader's job and `boot` has nothing to start.
        pub fn has_bootloader(&self) -> bool {
            self.bootloader_loaded
        }

        pub fn board(&self) -> Option<&Rc<RefCell<LitzBoard>>> {
            self.board.as_ref()
        }

        /// The esp32sim board callbacks, which route pins, SPI, UART and I2C
        /// to the Litz board. A test can call the same entry points the chip
        /// does without hand-programming a peripheral.
        pub fn board_model(&mut self) -> &mut dyn BoardModel {
            self.machine.bus.board()
        }

        pub fn state(&self, part: &str) -> Option<PartState> {
            self.board.as_ref()?.borrow().state(part)
        }

        /// Press or release a named part. The board resolves it, so a LED
        /// wired through the switch lights up as well; the chip only sees the
        /// pin move if the circuit routes the switch to one.
        pub fn press(&mut self, part: &str, pressed: bool) -> bool {
            let Some(board) = self.board.as_ref() else {
                return false;
            };
            let now = (self.machine.seconds() * 1e9) as u64;
            board.borrow_mut().press(part, pressed, now)
        }

        /// Move a continuous control (a potentiometer's travel, in permille). The
        /// board recomputes the wiper voltage; the pad is reprogrammed on the next
        /// run, which is the first moment the firmware could read it anyway.
        /// Make external PSRAM visible to the chip, in bytes.
        pub fn set_psram_size(&mut self, bytes: usize) -> Result<(), String> {
            self.machine.bus.set_psram_size(bytes)
        }

        pub fn set_control(&mut self, part: &str, permille: u16) -> bool {
            let Some(board) = self.board.as_ref() else {
                return false;
            };
            let now = (self.machine.seconds() * 1e9) as u64;
            board.borrow_mut().set_control(part, permille, now)
        }

        /// Set a named control on a part, in the unit the scenario wrote.
        pub fn set_named_control(&mut self, part: &str, name: &str, value: &str) -> bool {
            let Some(board) = self.board.as_ref() else {
                return false;
            };
            let now = (self.machine.seconds() * 1e9) as u64;
            board.borrow_mut().set_named_control(part, name, value, now)
        }

        /// Bytes from the host into UART0's receive FIFO, as a terminal on the
        /// chip's RX pin would send them.
        pub fn write_serial(&mut self, bytes: &[u8]) {
            self.machine.bus.uart_input(0, bytes);
            *self.machine.bus.irq_dirty() = true;
        }

        /// Analog voltages the board holds on chip pins, for tests and a UI.
        pub fn analog_inputs(&self) -> Vec<(u8, f32)> {
            self.board
                .as_ref()
                .map(|b| b.borrow().analog_inputs_on(self.board_index))
                .unwrap_or_default()
        }

        /// Push the board's analog levels to the pads that changed since the
        /// last push.
        ///
        /// `Board::analog_inputs` is the one preference point: when the board's
        /// analog co-simulation is on it returns the solved node voltage for an
        /// analog pin, and when it is off it returns the part's static
        /// `Part::analog` value exactly as before. The adapter therefore has no
        /// second, chip-specific path to keep in sync.
        fn publish_analog(&mut self) {
            let Some(board) = self.board.as_ref() else {
                return;
            };
            let now = board.borrow().analog_inputs();
            for &(gpio, volts) in &now {
                let unchanged = self
                    .analog
                    .iter()
                    .any(|&(pin, v)| pin == gpio && v == volts);
                if !unchanged {
                    self.machine
                        .bus
                        .analog_set(gpio, esp_periph::AnalogSource::Const(volts));
                }
            }
            self.analog = now;
        }

        /// The level on one pin of a named part. With no pin name, the level
        /// the chip sees on the pin the part is wired to.
        pub fn part_pin_level(&self, part: &str, pin: Option<&str>) -> Option<Level> {
            let board = self.board.as_ref()?.borrow();
            match pin {
                Some(pin) => board.part_pin_level(part, pin),
                None => board.chip_pin_level_on(self.board_index, part),
            }
        }

        /// The level on one of this chip's own pads, by GPIO number.
        pub fn gpio_level(&self, gpio: u8) -> Option<Level> {
            let board = self.board.as_ref()?.borrow();
            board.chip_level_on(self.board_index, gpio)
        }

        pub fn conflicts(&self) -> Vec<Conflict> {
            self.board
                .as_ref()
                .map(|b| b.borrow().conflicts().to_vec())
                .unwrap_or_default()
        }

        /// Load the mask ROM ELF (Apache-2.0, from `espressif/esp-rom-elfs`).
        pub fn load_rom(&mut self, rom_elf: &[u8]) -> Result<(), String> {
            self.machine.load_rom(rom_elf)
        }

        /// Write bytes at a flash offset (bootloader at 0x0, partition table at
        /// 0x8000, application at 0x10000, matching the ESP-IDF layout).
        pub fn write_flash(&mut self, offset: usize, data: &[u8]) -> Result<(), String> {
            self.machine.write_flash(offset, data)
        }

        /// Start the application at `app_off` the way the second-stage
        /// bootloader would, for images that ship without a bootloader.
        pub fn boot_app(&mut self, app_off: usize) -> Result<u32, String> {
            self.machine.boot_app(app_off)
        }

        /// Schedule host actions (pressing a named pin, writing to a UART) at
        /// simulated times. Times are seconds; the format is esp32sim's.
        pub fn load_script(&mut self, text: &str) -> Result<(), String> {
            self.machine.load_script(text)
        }

        /// Load an ELF's symbols so a fault or a trace names a function instead of
        /// an address. The ROM ELF and the firmware ELF both work.
        pub fn add_symbols(&mut self, elf: &[u8]) -> Result<(), String> {
            self.machine.add_symbols(elf)
        }

        /// Stop the next run when the program counter reaches `pc`.
        pub fn break_at(&mut self, pc: u32) {
            self.machine
                .add_observer(Box::new(esp_soc::observers::Breakpoints { pcs: vec![pc] }));
        }

        /// Registers of every core, as the debugger prints them.
        pub fn registers(&self) -> String {
            self.machine.dump_regs()
        }

        /// Record which code blocks execute, so a test can show what it
        /// actually covered. Enabling twice is a no-op.
        pub fn enable_coverage(&mut self) {
            if !self.machine.has_observer("coverage") {
                self.machine
                    .add_observer(Box::new(esp_soc::observers::Coverage::new(None)));
            }
        }

        /// The enabled observers' end-of-run reports, one line each.
        pub fn reports(&mut self) -> Vec<String> {
            self.machine.reports().lines().map(str::to_string).collect()
        }

        /// Disassemble `count` instructions at `addr`.
        /// The address the firmware's symbol table gives `name`, when the
        /// project supplied an ELF.
        pub fn symbol_addr(&self, name: &str) -> Option<u32> {
            self.machine.sym_addr(name)
        }

        /// Call a guest function with no arguments and return its result: `a0`
        /// on RV32, and the windowed callee's `a2` on Xtensa (read from the
        /// caller's `a10` after `retw` rotates the register window back).
        ///
        /// The call is transparent to the firmware: the core state and program
        /// counter are saved and restored, and the function returns to the
        /// instruction that was about to execute. The call is bounded, so a
        /// function that never returns comes back as an error instead of hanging
        /// the run.
        pub fn call_guest(&mut self, address: u32) -> Result<u32, String> {
            match self.guest_call {
                Some(call) => call(self, address),
                None => Err(format!(
                    "{} does not install a guest-call trampoline",
                    S::NAME
                )),
            }
        }

        /// The free heap the running firmware reports, by calling its own
        /// `esp_get_free_heap_size` (or another IDF getter).
        ///
        /// This is a real sample: the function reads the guest's allocator.
        /// It needs the ELF's symbols and a stopped or idle core.
        pub fn sample_heap(&mut self) -> Result<u32, String> {
            let names = [
                "esp_get_free_heap_size",
                "heap_caps_get_free_size",
                "xPortGetFreeHeapSize",
            ];
            let (name, address) = names
                .iter()
                .find_map(|name| self.symbol_addr(name).map(|address| (*name, address)))
                .ok_or_else(|| {
                    "no heap getter symbol in the ELF (esp_get_free_heap_size, \
                     heap_caps_get_free_size or xPortGetFreeHeapSize); supply `elf` in the project"
                        .to_string()
                })?;
            self.call_guest(address)
                .map_err(|error| format!("{name}: {error}"))
        }

        pub fn disassemble(&mut self, addr: u32, count: usize) -> String {
            self.machine.disasm(addr, count)
        }

        /// Put a virtual access point on the air the chip's radio hears.
        ///
        /// `spec` is esp32sim's AP syntax (`ssid=...,psk=...,channel=...`). The
        /// network behind it is the deterministic virtual one (DHCP, ARP, DNS,
        /// NTP); no host socket is involved, so a test that uses Wi-Fi stays
        /// repeatable. NAT to the host is deliberately not wired here.
        pub fn attach_wifi(&mut self, spec: &str) -> Result<(), String> {
            let ap = esp_soc::wifi::ApConfig::parse(spec)?;
            self.machine.bus.attach_wifi(ap, None)
        }

        /// Put a whole simulated network on the air: the joinable access points,
        /// the networks a scan should see, and a router that answers TCP and UDP
        /// without a host socket.
        ///
        /// The router is installed as esp32sim's NAT transport, so the guest's
        /// DHCP lease, gateway, DNS server and default route are the ones a real
        /// access point hands out; only the far side of the gateway is Litz's,
        /// and that side is `litz-net`, not anything ESP-specific.
        pub fn attach_network(&mut self, config: &crate::NetworkConfig) -> Result<(), String> {
            let specs: Vec<String> = config.access_points.iter().map(ap_spec).collect();
            let aps = esp_soc::wifi::ApConfig::parse_all(&specs)?;
            let nat = esp_soc::nat::Nat::with_transport(
                false,
                Box::new(crate::EspRouter::with_clock(
                    config,
                    Rc::clone(&self.net_clock),
                )),
            );
            self.machine.bus.attach_wifi(aps[0].clone(), Some(nat))?;
            for ap in &aps[1..] {
                self.machine.bus.add_wifi_ap(ap.clone())?;
            }
            if !config.nearby.is_empty() {
                let specs: Vec<String> = config.nearby.iter().map(nearby_spec).collect();
                let beacons = esp_soc::wifi::BeaconAp::parse_all(&specs)?;
                let left_out = self.machine.bus.wifi_nearby(beacons)?;
                if let Some((beacon, why)) = left_out.first() {
                    return Err(format!(
                        "the network '{}' cannot be beside the access point: it shares {why}",
                        beacon.ssid
                    ));
                }
            }
            Ok(())
        }

        /// Replace the simulated far side of the Wi-Fi gateway with host
        /// sockets, so the guest reaches the real internet through the host.
        ///
        /// The access point and the DHCP lease stay the simulated ones; only
        /// what is behind the gateway changes. This is opt-in and has no
        /// deterministic bytes: a run that uses it depends on the host network.
        #[cfg(not(target_arch = "wasm32"))]
        pub fn attach_host_nat(&mut self, local_ntp: bool) -> Result<(), String> {
            self.machine
                .bus
                .attach_nat(Some(esp_soc::nat::Nat::new(false)), local_ntp)
        }

        /// Put host sockets behind the gateway and forward these host ports
        /// into the guest: `tcp:[host:]port-[guest:]port`, the same syntax
        /// esp32sim uses. Returns one line per forward describing where it
        /// listens.
        #[cfg(not(target_arch = "wasm32"))]
        pub fn attach_host_forwards(
            &mut self,
            forwards: &[String],
            local_ntp: bool,
        ) -> Result<Vec<String>, String> {
            let mut transport = esp_soc::nat::StdTransport::new();
            let mut lines = Vec::new();
            for spec in forwards {
                let forward = esp_soc::nat::HostFwdSpec::parse(spec)?;
                let at = transport
                    .listen(&forward)
                    .map_err(|e| format!("host forward {spec:?}: {e}"))?;
                lines.push(format!("{spec} -> {at}"));
            }
            let mut nat = esp_soc::nat::Nat::with_transport(false, Box::new(transport));
            nat.hostfwd = forwards.len() as u64;
            self.machine.bus.attach_nat(Some(nat), local_ntp)?;
            Ok(lines)
        }

        /// Record the Ethernet frames the station's MAC carries, both ways,
        /// for a pcap. Independent of the virtual network's own capture.
        pub fn set_net_tap(&mut self, tap: esp_soc::netcap::NetTap) -> Result<(), String> {
            self.machine.bus.set_net_tap(tap)
        }

        /// Record every 802.11 frame the air carries into a pcap with link
        /// type 105, written as the run goes.
        pub fn start_air_capture(&mut self, path: &std::path::Path) -> Result<(), String> {
            let file =
                std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let capture =
                std::sync::Arc::new(std::sync::Mutex::new(esp_soc::netcap::AirCapture::default()));
            capture
                .lock()
                .map_err(|_| "the air capture lock is poisoned".to_string())?
                .set_pcap(Box::new(file))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            self.machine
                .bus
                .set_air_tap(Some(capture))
                .map_err(|e| format!("802.11 capture: {e}"))
        }

        /// Record the Ethernet frames the station's MAC actually carried into
        /// a pcap with link type 1, written as the run goes.
        pub fn start_ethernet_capture(&mut self, path: &std::path::Path) -> Result<(), String> {
            let file =
                std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let capture =
                std::sync::Arc::new(std::sync::Mutex::new(esp_soc::netcap::NetCapture::default()));
            capture
                .lock()
                .map_err(|_| "the Ethernet capture lock is poisoned".to_string())?
                .set_pcap(Box::new(file))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            self.machine
                .bus
                .set_net_tap(capture)
                .map_err(|e| format!("Ethernet capture: {e}"))
        }

        /// Replace the simulated network with a raw Ethernet seam: the guest's
        /// frames come out of [`take_ethernet_frames`](Self::take_ethernet_frames)
        /// and the host's go back in through
        /// [`receive_ethernet_frame`](Self::receive_ethernet_frame).
        pub fn set_ethernet_relay(&mut self, enabled: bool) -> Result<(), String> {
            self.machine.bus.set_ethernet_relay(enabled)
        }

        /// Complete Ethernet frames (no FCS) in transmit order since the last
        /// call. Empty outside relay mode.
        pub fn take_ethernet_frames(&mut self) -> Vec<Vec<u8>> {
            self.machine.bus.take_ethernet_frames()
        }

        /// Queue one 14..=1518-byte Ethernet frame (no FCS) for the guest.
        pub fn receive_ethernet_frame(&mut self, frame: &[u8]) -> Result<(), String> {
            self.machine.bus.receive_ethernet_frame(frame)
        }

        /// Put the circuit's Bluetooth advertisers on the air the chip's LE
        /// controller hears.
        ///
        /// The ESP32-C3's link controller is derived from its mask ROM, so it
        /// needs no ELF and can be installed here with `enable_ble_full(false)`.
        /// The S3, C6 and classic ESP32 controllers are package images carried
        /// inside the firmware ELF; Litz has no ELF plumbing for them, so those
        /// chips answer with an error naming themselves rather than start a
        /// controller that cannot scan.
        ///
        /// The beacons are translated straight into esp32sim's `Nearby` structs:
        /// the part attributes have already been validated by the board, so the
        /// string syntax `--ble-nearby` uses would only parse them a second time.
        pub fn attach_ble(&mut self, config: &litz_ble::BleConfig) -> Result<(), String> {
            if config.is_empty() {
                return Ok(());
            }
            // The list the controller hears is the circuit's, and it is not
            // fixed for the run: a runtime advertiser changes it. Remember the
            // generation the controller was given so a step can notice.
            self.ble_seen = self
                .board
                .as_ref()
                .map(|board| board.borrow().ble_generation())
                .unwrap_or(0);
            self.ble_attached = false;
            // The advertisers reach a scanning guest through esp32sim's HCI
            // controller, which is started by patching the guest's own
            // controller task — an entry the machine can only find by symbol.
            // The ROM-derived controller (`enable_ble_full`) is a different
            // object and deliberately refuses the connection: sending the
            // beacons to it would look like success and put nothing on the
            // air, so the missing ELF is reported instead.
            let Some(bytes) = self.elf.clone() else {
                return Err(format!(
                    "{} needs the firmware ELF to scan for a beacon: set [sim] elf, \
                     because the Bluetooth controller is started through the guest's \
                     symbol table",
                    S::NAME
                ));
            };
            let elf = esp_soc::elf::parse(&bytes)?;
            self.machine
                .bus
                .enable_ble(&elf)
                .map_err(|why| format!("{} cannot carry BLE beacons: {why}", S::NAME))?;
            let beacons = nearby_list(&config.beacons);
            self.machine.bus.ble_nearby(beacons)?;
            self.ble_peers = config.beacons.clone();
            self.ble_attached = true;
            Ok(())
        }

        /// Replace the devices the controller's scan reports while it runs.
        ///
        /// esp32sim hands the list to the controller, and a scan in progress
        /// hears the new one at once, so this is the runtime half of
        /// [`EspChip::attach_ble`]. Litz calls it from `run` whenever a custom
        /// chip starts or stops advertising, which is what makes a runtime
        /// advertiser visible to a native ESP32 rather than only to another
        /// wasm chip.
        pub fn set_ble_nearby(&mut self, config: &litz_ble::BleConfig) -> Result<(), String> {
            if !self.ble_attached {
                return Err("BLE is not enabled on this chip".into());
            }
            self.machine.bus.ble_nearby(nearby_list(&config.beacons))?;
            self.ble_peers = config.beacons.clone();
            self.ble_seen = self
                .board
                .as_ref()
                .map(|board| board.borrow().ble_generation())
                .unwrap_or(0);
            Ok(())
        }

        /// Add one advertiser to the controller's scan list while it runs.
        ///
        /// esp32sim replaces its peer list wholesale, and a scan in progress
        /// reports the new list at once, so the list this adapter was given is
        /// the thing to append to. The method is inherent rather than on
        /// [`litz_board::Chip`]: that trait is the runner's seam and has no
        /// per-advertiser BLE entry point.
        pub fn add_ble_peer(&mut self, beacon: &litz_ble::Beacon) -> Result<(), String> {
            if !self.ble_attached {
                return Err("BLE is not enabled on this chip".into());
            }
            self.ble_peers.push(beacon.clone());
            let peers = nearby_list(&self.ble_peers);
            if let Err(error) = self.machine.bus.ble_nearby(peers) {
                self.ble_peers.pop();
                return Err(error);
            }
            self.ble_seen = self
                .board
                .as_ref()
                .map(|board| board.borrow().ble_generation())
                .unwrap_or(0);
            Ok(())
        }

        /// Drive the virtual central the controller's peer link exposes:
        /// `connect`, `discover`, `read <handle>`, `write <handle> <hex>`,
        /// `disconnect` or `central-stop`.
        ///
        /// The commands are the ones a phone would send to the guest's
        /// peripheral, so a test can exercise the guest's GATT server and its
        /// connection events without a second chip. The full link controller
        /// (C3) models the 30 ms connection interval, channel hopping and ATT
        /// transactions; the ROM-derived HCI controller answers with the
        /// smaller command set and says so.
        pub fn ble_command(&mut self, command: &str) -> Result<(), String> {
            self.machine.bus.ble_command(command)
        }

        /// How many virtual-central commands have not completed yet.
        pub fn ble_pending_commands(&self) -> usize {
            self.machine.bus.ble_pending_commands()
        }

        /// The oldest kept `[ble]` line, without its prefix.
        pub fn ble_log(&mut self) -> Option<String> {
            self.machine.bus.ble_log_take()
        }

        /// Start the full C3 link controller, which models connection events,
        /// the 30 ms interval and gap timing rather than only advertising and
        /// scanning. `observe` keeps its `[ble-*]` lines for [`EspChip::ble_log`].
        pub fn enable_full_ble(&mut self, observe: bool) -> Result<(), String> {
            self.machine.bus.enable_ble_full(observe)
        }

        /// Simulated seconds since reset.
        pub fn seconds(&self) -> f64 {
            self.machine.seconds()
        }

        /// Run at most `steps` scheduling steps; the stop condition says why it ended.
        pub fn run(&mut self, steps: u64) -> esp_soc::Stop {
            // An ADC conversion inside the run must see the voltage the knob
            // holds now, so the pads are refreshed before any instruction runs.
            self.publish_analog();
            // A capture timestamps network answers with the board's clock.
            if let Some(board) = &self.board {
                let now = board.borrow().now();
                self.net_clock.set(now);
                // A wasm chip on the same board may have started or stopped
                // advertising since the last step. Mirror the air only when it
                // changed: the list reaches a scan in progress at once.
                if self.ble_attached {
                    let (generation, air) = {
                        let board = board.borrow();
                        (board.ble_generation(), board.ble_air(self.board_index))
                    };
                    if generation != self.ble_seen {
                        self.ble_seen = generation;
                        let _ = self.machine.bus.ble_nearby(nearby_list(&air));
                        self.ble_peers = air;
                    }
                }
            }
            self.machine.run(steps)
        }

        pub fn uart0(&self) -> &[u8] {
            &self.machine.console.uart0
        }

        pub fn take_uart0(&mut self) -> Vec<u8> {
            core::mem::take(&mut self.machine.console.uart0)
        }
    }

    /// esp32sim's access-point syntax, built from the chip-independent
    /// description so the parsing, validation and BSSID derivation stay in one
    /// place (esp32sim's own).
    fn ap_spec(ap: &litz_net::AccessPoint) -> String {
        let mut spec = format!("ssid={},channel={}", ap.ssid, ap.channel);
        if let Some(psk) = &ap.psk {
            spec.push_str(&format!(",psk={psk}"));
        }
        if let Some(bssid) = ap.bssid {
            spec.push_str(&format!(
                ",bssid={}",
                bssid
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(":")
            ));
        }
        if let Some(epoch) = ap.epoch_s {
            spec.push_str(&format!(",epoch={epoch}"));
        }
        spec
    }

    /// The controller's view of a set of advertisers, including the GATT
    /// table a central discovers after connecting.
    fn nearby_list(beacons: &[litz_ble::Beacon]) -> Vec<Nearby> {
        beacons
            .iter()
            .map(|beacon| Nearby {
                addr: beacon.address,
                addr_type: beacon.address_type(),
                rssi: beacon.rssi,
                adv: beacon.adv.clone(),
                scan_response: beacon.scan_response.clone(),
                gatt: beacon
                    .gatt
                    .iter()
                    .map(|service| esp_soc::ble::peer::GattService {
                        uuid: service.uuid,
                        characteristics: service
                            .characteristics
                            .iter()
                            .map(|characteristic| esp_soc::ble::peer::GattCharacteristic {
                                uuid: characteristic.uuid,
                                flags: characteristic.flags,
                                value: characteristic.value.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect()
    }

    fn nearby_spec(network: &litz_net::NearbyNetwork) -> String {
        format!(
            "ssid={},channel={},rssi={},secure={}",
            network.ssid,
            network.channel,
            network.rssi,
            u8::from(network.secure)
        )
    }

    /// The ESP32-C3. Kept as a named alias so callers keep writing `C3`.
    #[cfg(feature = "esp32c3")]
    pub type C3 = EspChip<esp32c3::C3>;

    #[cfg(feature = "esp32c3")]
    impl EspChip<esp32c3::C3> {
        pub fn new(mac: [u8; 6], flash_size: usize) -> Self {
            let mut chip = Self::bare(mac, flash_size, c3_bus);
            chip.gdb = Some(serve_gdb::<esp32c3::C3>);
            chip.guest_call = Some(crate::debug::rv32_call_guest::<esp32c3::C3>);
            chip
        }
    }

    #[cfg(feature = "esp32c3")]
    fn c3_bus(
        mac: [u8; 6],
        flash: usize,
        board: Option<Rc<RefCell<LitzBoard>>>,
        index: u16,
        spi_rates: Rc<Cell<[u32; 2]>>,
    ) -> esp32c3::bus::SocBus {
        const CPU_HZ: u64 = esp32c3::periph::CPU_HZ;
        let mut bus = esp32c3::bus::SocBus::new(flash, mac);
        if let Some(board) = board {
            bus.board = Box::new(LitzModel::new(board, CPU_HZ, index, spi_rates));
            // The bus caches which callbacks the board wants and reconnects the
            // board's current input levels; both happen here.
            bus.attach_board_devices();
        }
        bus
    }

    /// The ESP32-C6: the same adapter over the second RISC-V chip.
    #[cfg(feature = "esp32c6")]
    pub type C6 = EspChip<esp32c6::C6>;

    #[cfg(feature = "esp32c6")]
    impl EspChip<esp32c6::C6> {
        pub fn new(mac: [u8; 6], flash_size: usize) -> Self {
            let mut chip = Self::bare(mac, flash_size, c6_bus);
            chip.gdb = Some(serve_gdb::<esp32c6::C6>);
            chip.guest_call = Some(crate::debug::rv32_call_guest::<esp32c6::C6>);
            chip
        }
    }

    #[cfg(feature = "esp32c6")]
    fn c6_bus(
        mac: [u8; 6],
        flash: usize,
        board: Option<Rc<RefCell<LitzBoard>>>,
        index: u16,
        spi_rates: Rc<Cell<[u32; 2]>>,
    ) -> esp32c6::bus::SocBus {
        const CPU_HZ: u64 = esp32c6::periph::CPU_HZ;
        let mut bus = esp32c6::bus::SocBus::new(flash, mac);
        if let Some(board) = board {
            bus.board = Box::new(LitzModel::new(board, CPU_HZ, index, spi_rates));
            bus.attach_board_devices();
        }
        bus
    }

    /// The classic ESP32 (Xtensa LX6, two cores).
    #[cfg(feature = "esp32")]
    pub type Esp32 = EspChip<esp32::Esp32>;

    #[cfg(feature = "esp32")]
    impl EspChip<esp32::Esp32> {
        pub fn new(mac: [u8; 6], flash_size: usize) -> Self {
            let mut chip = Self::bare(mac, flash_size, esp32_bus);
            chip.guest_call = Some(crate::debug::XtensaChip::call_guest);
            chip.spi_clock = Some(ESP32_SPI_CLOCKS);
            chip
        }
    }

    #[cfg(feature = "esp32")]
    fn esp32_bus(
        mac: [u8; 6],
        flash: usize,
        board: Option<Rc<RefCell<LitzBoard>>>,
        index: u16,
        spi_rates: Rc<Cell<[u32; 2]>>,
    ) -> esp32::bus::SocBus {
        const CPU_HZ: u64 = esp32::periph::CPU_HZ;
        let mut bus = esp32::bus::SocBus::new(flash, mac);
        if let Some(board) = board {
            bus.board = Box::new(LitzModel::new(board, CPU_HZ, index, spi_rates));
            bus.attach_board_devices();
        }
        bus
    }

    /// PSRAM the S3 adapter hands the bus. A board manifest does not carry a
    /// PSRAM size yet; the DevKitC-1 has 2 MiB and the chips tolerate more.
    #[cfg(feature = "esp32s3")]
    const S3_PSRAM: usize = 2 << 20;

    /// The ESP32-S3 (Xtensa LX7).
    #[cfg(feature = "esp32s3")]
    pub type S3 = EspChip<esp32s3::S3>;

    #[cfg(feature = "esp32s3")]
    impl EspChip<esp32s3::S3> {
        pub fn new(mac: [u8; 6], flash_size: usize) -> Self {
            let mut chip = Self::bare(mac, flash_size, s3_bus);
            chip.guest_call = Some(crate::debug::XtensaChip::call_guest);
            chip.spi_clock = Some(S3_SPI_CLOCKS);
            chip
        }
    }

    #[cfg(feature = "esp32s3")]
    fn s3_bus(
        mac: [u8; 6],
        flash: usize,
        board: Option<Rc<RefCell<LitzBoard>>>,
        index: u16,
        spi_rates: Rc<Cell<[u32; 2]>>,
    ) -> esp32s3::bus::SocBus {
        const CPU_HZ: u64 = esp32s3::periph::CPU_HZ;
        let mut bus = esp32s3::bus::SocBus::new(flash, S3_PSRAM, mac);
        if let Some(board) = board {
            bus.board = Box::new(LitzModel::new(board, CPU_HZ, index, spi_rates));
            bus.attach_board_devices();
        }
        bus
    }

    /// The ESP32-P4 in its pre-rev-3 wafer, which is what `esp32p4::machine`
    /// builds. A rev-3 board can add a sibling alias over `P4Rev3`.
    #[cfg(feature = "esp32p4")]
    pub type P4 = EspChip<esp32p4::P4>;

    #[cfg(feature = "esp32p4")]
    impl EspChip<esp32p4::P4> {
        pub fn new(mac: [u8; 6], flash_size: usize) -> Self {
            let mut chip = Self::bare(mac, flash_size, p4_bus);
            chip.gdb = Some(serve_gdb::<esp32p4::P4>);
            chip.guest_call = Some(crate::debug::rv32_call_guest::<esp32p4::P4>);
            chip
        }
    }

    #[cfg(feature = "esp32p4")]
    fn p4_bus(
        mac: [u8; 6],
        flash: usize,
        board: Option<Rc<RefCell<LitzBoard>>>,
        index: u16,
        spi_rates: Rc<Cell<[u32; 2]>>,
    ) -> esp32p4::bus::SocBus {
        const CPU_HZ: u64 = esp32p4::periph::CPU_HZ;
        let wafer = <esp32p4::RevLt3 as esp32p4::Revision>::WAFER;
        let mut bus = esp32p4::bus::SocBus::new(flash, mac, wafer);
        if let Some(board) = board {
            bus.board = Box::new(LitzModel::new(board, CPU_HZ, index, spi_rates));
            bus.attach_board_devices();
        }
        bus
    }

    /// The ESP32-S31.
    #[cfg(feature = "esp32s31")]
    pub type S31 = EspChip<esp32s31::S31>;

    #[cfg(feature = "esp32s31")]
    impl EspChip<esp32s31::S31> {
        pub fn new(mac: [u8; 6], flash_size: usize) -> Self {
            let mut chip = Self::bare(mac, flash_size, s31_bus);
            chip.gdb = Some(serve_gdb::<esp32s31::S31>);
            chip.guest_call = Some(crate::debug::rv32_call_guest::<esp32s31::S31>);
            chip
        }
    }

    #[cfg(feature = "esp32s31")]
    fn s31_bus(
        mac: [u8; 6],
        flash: usize,
        board: Option<Rc<RefCell<LitzBoard>>>,
        index: u16,
        spi_rates: Rc<Cell<[u32; 2]>>,
    ) -> esp32s31::SocBus {
        const CPU_HZ: u64 = esp32s31::periph::CPU_HZ;
        let mut bus = esp32s31::SocBus::new(flash, mac);
        if let Some(board) = board {
            bus.board = Box::new(LitzModel::new(board, CPU_HZ, index, spi_rates));
            bus.attach_board_devices();
        }
        bus
    }

    /// One I2C target, as esp32sim's controller sees it.
    ///
    /// The device itself stays in `Board::buses`; this handle holds the shared
    /// board and an address, so handing it to esp32sim does not move the
    /// device out of the board and the board stays usable by tests and the
    /// host. Every callback borrows the board for the length of one byte.
    struct BoardI2cDevice {
        board: Rc<RefCell<LitzBoard>>,
        /// The board instance whose controller this is; two boards' `i2c0`
        /// are different buses.
        board_index: u16,
        controller: u8,
        address: u8,
        /// The phases of the transaction in flight, so the pins can be
        /// reconstructed when the STOP arrives. `esp32sim` reports one byte
        /// at a time and has no timestamp of its own.
        written: Vec<u8>,
        received: Vec<u8>,
        reading: bool,
        open: bool,
        /// Where the last observed transaction ended, so back-to-back
        /// transactions do not stack at one tick when the board never settled
        /// a pin between them.
        last_at: u64,
        /// When the firmware's controller started this transaction. The
        /// elapsed time is the configured SCL rate, which is what the
        /// reconstructed edges use.
        started_at: u64,
    }

    impl EspI2cDevice for BoardI2cDevice {
        fn address(&self, _configured: u8) -> u8 {
            self.address
        }

        fn start(&mut self, read: bool) -> bool {
            if !self.open {
                self.written.clear();
                self.received.clear();
                self.open = true;
                self.started_at = self.board.borrow().now();
            }
            self.reading = read;
            let mut board = self.board.borrow_mut();
            let Some(bus) = board
                .buses_mut()
                .i2c
                .get_mut(&(self.board_index, self.controller))
            else {
                return false;
            };
            let Some(device) = bus.device_mut(self.address) else {
                return false;
            };
            device.connect(read);
            true
        }

        fn write(&mut self, byte: u8) -> bool {
            if !self.reading {
                self.written.push(byte);
            }
            let mut board = self.board.borrow_mut();
            let Some(bus) = board
                .buses_mut()
                .i2c
                .get_mut(&(self.board_index, self.controller))
            else {
                return false;
            };
            let Some(device) = bus.device_mut(self.address) else {
                return false;
            };
            device.write(byte)
        }

        fn read(&mut self) -> u8 {
            let byte = {
                let mut board = self.board.borrow_mut();
                let Some(bus) = board
                    .buses_mut()
                    .i2c
                    .get_mut(&(self.board_index, self.controller))
                else {
                    return 0xff;
                };
                let Some(device) = bus.device_mut(self.address) else {
                    return 0xff;
                };
                device.read()
            };
            self.received.push(byte);
            byte
        }

        fn stop(&mut self) {
            {
                let mut board = self.board.borrow_mut();
                let Some(bus) = board
                    .buses_mut()
                    .i2c
                    .get_mut(&(self.board_index, self.controller))
                else {
                    return;
                };
                if let Some(device) = bus.device_mut(self.address) {
                    device.stop();
                }
            }
            if self.open {
                let mut board = self.board.borrow_mut();
                let end = board.now();
                let at = self.started_at.max(self.last_at);
                let phases = 1
                    + self.written.len()
                    + if self.received.is_empty() {
                        0
                    } else {
                        1 + self.received.len()
                    };
                // The controller clocks each phase at the divider the firmware
                // programmed, so the transaction's own duration is the SCL
                // rate. Falling back to the last known rate keeps a
                // zero-duration model (a callback-only device) usable.
                let elapsed = end.saturating_sub(self.started_at);
                let hz = if elapsed > 0 {
                    (phases as u128 * 9 * 1_000_000_000 / u128::from(elapsed))
                        .clamp(1_000, 5_000_000) as u32
                } else {
                    board.i2c_hz(self.board_index, self.controller)
                };
                board.set_i2c_hz(self.board_index, self.controller, hz);
                board.observe_i2c(
                    self.board_index,
                    self.controller,
                    self.address,
                    &self.written,
                    &self.received,
                    at,
                );
                let period = 1_000_000_000 / u64::from(hz);
                self.last_at = (at + phases as u64 * 9 * period).max(end);
            }
            self.open = false;
            self.reading = false;
        }
    }

    /// Where one chip keeps its GP-SPI clock dividers.
    ///
    /// The two Xtensa parts encode the same `APB / ((pre+1) * (n+1))` divider
    /// at the same bit positions, but the register offset and the width of the
    /// pre-divider differ: the classic ESP32 has 0x18 and 13 bits, the S3 0xC
    /// and 4 bits.
    #[derive(Clone, Copy)]
    struct SpiClockSpec {
        /// `(clock register, pre-divider shift, pre-divider mask)` for hosts 2 and 3.
        ports: [(u32, u32, u32); 2],
    }

    #[cfg(feature = "esp32")]
    const ESP32_SPI_CLOCKS: SpiClockSpec = SpiClockSpec {
        ports: [(0x3ff6_4018, 18, 0x1fff), (0x3ff6_5018, 18, 0x1fff)],
    };

    #[cfg(feature = "esp32s3")]
    const S3_SPI_CLOCKS: SpiClockSpec = SpiClockSpec {
        ports: [(0x6002_400c, 18, 0xf), (0x6002_500c, 18, 0xf)],
    };

    /// Turns a firmware's GP-SPI clock-register write into the rate the board
    /// model reports for the next transfer.
    ///
    /// esp32sim's `spi_transfer` callback carries bytes, not a clock, so the
    /// programmed divider has to travel beside it. Watching MMIO is exact and
    /// cheap: only a register write ever changes the rate.
    struct SpiClockObserver {
        rates: Rc<Cell<[u32; 2]>>,
        spec: SpiClockSpec,
    }

    impl<S: Soc> esp_soc::Observer<S> for SpiClockObserver {
        fn name(&self) -> &'static str {
            "litz-spi-clock"
        }

        fn wants(&self) -> esp_soc::Wants {
            esp_soc::Wants::MMIO
        }

        fn on_mmio(&mut self, _cx: &esp_soc::Ctx, _pc: u32, addr: u32, value: u32, write: bool) {
            if !write {
                return;
            }
            let Some(slot) = self.spec.ports.iter().position(|&(reg, ..)| reg == addr) else {
                return;
            };
            let (_, shift, mask) = self.spec.ports[slot];
            // SPI_CLK_EQU_SYSCLK bypasses both dividers; otherwise CLKCNT_N is
            // bits 17:12 and CLKDIV_PRE sits above it.
            let divider = if value & (1 << 31) != 0 {
                1
            } else {
                (((value >> shift) & mask) + 1) * (((value >> 12) & 0x3f) + 1)
            };
            let mut rates = self.rates.get();
            rates[slot] = (esp_periph::APB_HZ / u64::from(divider)) as u32;
            self.rates.set(rates);
        }
    }

    /// `esp_soc::BoardModel` over a flattened Litz board.
    ///
    /// The bus reports the GPIO output latch and its enable mask; only enabled
    /// bits are strong drivers, everything else is high-impedance. Edges the
    /// board resolves back onto a chip pin leave through `take_edges`.
    struct LitzModel {
        board: Rc<RefCell<LitzBoard>>,
        /// The board instance whose pads this bus drives.
        index: u16,
        enabled: u64,
        output: u64,
        cycle: u64,
        releases: Vec<u8>,
        /// The chip's clock, to turn esp32sim's bus cycles into the nanoseconds
        /// the board's parts and the waveform measure time in.
        cpu_hz: u64,
        /// Each UART port's receive pin and baud, learned from the route on
        /// the chip's last transmitted byte. A board peer answers on the pin
        /// the chip listens to, and that is what an input has to name.
        uart_routes: BTreeMap<u8, (u8, u32)>,
        /// The GP-SPI rates the firmware programmed, shared with the MMIO
        /// observer. A chip without GP-SPI registers leaves it all zero.
        spi_rates: Rc<Cell<[u32; 2]>>,
    }

    impl LitzModel {
        fn new(
            board: Rc<RefCell<LitzBoard>>,
            cpu_hz: u64,
            index: u16,
            spi_rates: Rc<Cell<[u32; 2]>>,
        ) -> Self {
            Self {
                board,
                index,
                enabled: 0,
                output: 0,
                cycle: 0,
                releases: Vec::new(),
                cpu_hz,
                uart_routes: BTreeMap::new(),
                spi_rates,
            }
        }

        /// The rate the guest programmed for GP-SPI `host`, if it wrote one.
        fn spi_rate(&self, host: u8) -> Option<u32> {
            let slot = usize::from(host.checked_sub(2)?);
            let hz = *self.spi_rates.get().get(slot)?;
            (hz > 0).then_some(hz)
        }

        fn ns(&self, cycle: u64) -> u64 {
            (u128::from(cycle) * 1_000_000_000 / u128::from(self.cpu_hz)) as u64
        }

        /// The first bus cycle at or after `ns`, so a deadline is never
        /// rounded down into the past.
        fn cycles_for(&self, ns: u64) -> u64 {
            let cycles = u128::from(ns) * u128::from(self.cpu_hz);
            (cycles.div_ceil(1_000_000_000)) as u64
        }
    }

    /// SSD1306 GDDRAM is page-major, LSB-first within a byte; a host PNG
    /// wants one RGB565 word per pixel.
    fn expand_monochrome(bytes: &[u8], width: u32, height: u32) -> Vec<u16> {
        let mut pixels = vec![0u16; (width * height) as usize];
        for page in 0..height.div_ceil(8) {
            for column in 0..width {
                let Some(byte) = bytes.get((page * width + column) as usize) else {
                    continue;
                };
                for bit in 0..8u32 {
                    let y = page * 8 + bit;
                    if y >= height {
                        break;
                    }
                    if byte & (1 << bit) != 0 {
                        pixels[(y * width + column) as usize] = 0xffff;
                    }
                }
            }
        }
        pixels
    }

    /// The board fabric's frame type is the same data as the chip's wire frame, with the
    /// board's own fixed-size array and frame kind. Convert once at the boundary.
    fn board_can_frame(frame: &esp_soc::board::CanFrame) -> litz_board::bus::CanFrame {
        // The chip carries a classic CAN wire frame: eight data bytes at most.
        let mut data = [0u8; litz_board::bus::CAN_MAX_DATA];
        let len = usize::from(frame.len).min(frame.data.len());
        data[..len].copy_from_slice(&frame.data[..len]);
        litz_board::bus::CanFrame {
            id: frame.id,
            extended: frame.extended,
            rtr: frame.rtr,
            len: frame.len,
            data,
            kind: if frame.rtr {
                litz_board::bus::CanFrameKind::Remote
            } else {
                litz_board::bus::CanFrameKind::Classic
            },
        }
    }

    impl BoardModel for LitzModel {
        fn name(&self) -> &'static str {
            "litz"
        }

        fn gpio_output_at(
            &mut self,
            cycle: u64,
            changes: &[(u8, bool)],
            enabled: u64,
            output: u64,
        ) {
            // The callback's cycle is relative to the current quantum on some chips;
            // `advance_to` carries the absolute bus cycle. Take whichever is later
            // so a toggle is never dated before the run reached it.
            self.cycle = self.cycle.max(cycle);
            let now = self.ns(self.cycle);
            if std::env::var("LITZ_TRACE_DEBUG").is_ok() {
                eprintln!("DBG cycle={} now={}", self.cycle, now);
            }
            // An enable-only transition is not in `changes`, and it matters:
            // releasing an output must stop driving the net.
            let mut mask = (enabled ^ self.enabled) | (output ^ self.output);
            for &(pin, _) in changes {
                mask |= 1u64 << pin;
            }
            let mut board = self.board.borrow_mut();
            while mask != 0 {
                let pin = mask.trailing_zeros() as u8;
                mask &= mask - 1;
                let bit = 1u64 << pin;
                let drive = if enabled & bit != 0 {
                    if output & bit != 0 {
                        Drive::strong(Level::High)
                    } else {
                        Drive::strong(Level::Low)
                    }
                } else {
                    Drive::HIGH_Z
                };
                board.set_chip_output_on(self.index, pin, drive, now);
            }
            self.enabled = enabled;
            self.output = output;
        }

        /// The first display on this board's buses, as RGB565. A colour TFT
        /// hands over its own buffer; a monochrome OLED is expanded one bit
        /// per pixel. The change counter comes from the panel, so a host can
        /// skip a frame that did not move.
        fn display(&self) -> Option<(u32, u32, Vec<u16>, u64)> {
            let board = self.board.borrow();
            let buses = board.buses();
            for (&(owner, _), i2c) in &buses.i2c {
                if owner != self.index {
                    continue;
                }
                for address in i2c.addresses() {
                    if let Some(oled) = i2c.device_as::<litz_board::Ssd1306>(address) {
                        let (bytes, width, height) = oled.framebuffer();
                        return Some((
                            width,
                            height,
                            expand_monochrome(bytes, width, height),
                            oled.frame_version(),
                        ));
                    }
                }
            }
            for (&(owner, _), spi) in &buses.spi {
                if owner != self.index {
                    continue;
                }
                for select in spi.chip_selects() {
                    if let Some(tft) = spi.device_as::<litz_board::Ili9341>(select) {
                        let (pixels, width, height) = tft.framebuffer();
                        return Some((width, height, pixels.to_vec(), tft.frame_version()));
                    }
                }
            }
            None
        }

        /// Host touch from the browser or a scenario reaches the board's
        /// controller part; a board without one ignores it, exactly as a
        /// finger on a panel that is not there would.
        fn touch(&mut self, x: u16, y: u16, down: bool) {
            let _ = self.board.borrow_mut().touch_panel(x, y, down);
        }

        fn display_version(&self) -> u64 {
            let board = self.board.borrow();
            let buses = board.buses();
            for (&(owner, _), i2c) in &buses.i2c {
                if owner != self.index {
                    continue;
                }
                for address in i2c.addresses() {
                    if let Some(oled) = i2c.device_as::<litz_board::Ssd1306>(address) {
                        return oled.frame_version();
                    }
                }
            }
            for (&(owner, _), spi) in &buses.spi {
                if owner != self.index {
                    continue;
                }
                for select in spi.chip_selects() {
                    if let Some(tft) = spi.device_as::<litz_board::Ili9341>(select) {
                        return tft.frame_version();
                    }
                }
            }
            0
        }

        fn advance_to(&mut self, cycle: u64) {
            self.cycle = cycle;
            // A part that schedules its own edges (a rangefinder's ECHO, a
            // sensor's frame) has to be moved on the chip's clock, not the
            // board's wall clock. The board settles after every event, so the
            // edges this produces are already waiting in `take_edges`.
            let now = self.ns(cycle);
            self.board.borrow_mut().advance_to(now);
        }

        fn next_deadline(&self) -> Option<u64> {
            // Pending inputs must reach the chip now, not at its next poll. A
            // part's next event is nanoseconds; the bus counts cycles.
            let board = self.board.borrow();
            let now = board.has_pending_inputs_on(self.index).then_some(0);
            let event = board.next_event().map(|at| self.cycles_for(at));
            match (now, event) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }

        fn take_edges(&mut self) -> Vec<BoardEdge> {
            let cycle = self.cycle;
            let drained = self.board.borrow_mut().take_chip_inputs_on(self.index);
            // One board attach can queue a pin twice: the first settle leaves
            // it floating and the next drive, in the same batch, holds it. The
            // last level is the one the pad has, so a pin that ends driven is
            // not also released — releasing it after `set_input` would put the
            // pad back on its pull and lose the level that just arrived.
            let mut levels: BTreeMap<u8, Level> = BTreeMap::new();
            for (pin, level) in drained {
                levels.insert(pin, level);
            }
            let mut edges = Vec::new();
            for (pin, level) in levels {
                match level {
                    Level::High => edges.push(BoardEdge {
                        cycle,
                        pin,
                        level: true,
                    }),
                    Level::Low => edges.push(BoardEdge {
                        cycle,
                        pin,
                        level: false,
                    }),
                    Level::HighZ => self.releases.push(pin),
                }
            }
            edges
        }

        fn released_inputs(&mut self) -> Vec<u8> {
            core::mem::take(&mut self.releases)
        }

        fn input_levels(&self) -> Vec<(u8, bool)> {
            self.board
                .borrow()
                .input_levels_on(self.index)
                .into_iter()
                .filter_map(|(pin, level)| match level {
                    Level::High => Some((pin, true)),
                    Level::Low => Some((pin, false)),
                    Level::HighZ => None,
                })
                .collect()
        }

        fn named_pin(&self, name: &str) -> Option<u8> {
            let board = self.board.borrow();
            let (index, pin) = board.named_pin_on(name)?;
            (index == self.index).then_some(pin)
        }

        /// Every I2C target the board holds, as `(controller, address, device)`.
        ///
        /// The device boxes share the board rather than taking a device out of
        /// it, so `Board` remains the owner and stays usable after attach.
        fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn EspI2cDevice>)> {
            let board = self.board.borrow();
            let mut devices: Vec<(u8, u8)> = Vec::new();
            for (&(owner, controller), bus) in &board.buses().i2c {
                if owner != self.index {
                    continue;
                }
                for address in bus.addresses() {
                    devices.push((controller, address));
                }
            }
            drop(board);
            devices
                .into_iter()
                .map(|(controller, address)| {
                    let device: Box<dyn EspI2cDevice> = Box::new(BoardI2cDevice {
                        board: Rc::clone(&self.board),
                        board_index: self.index,
                        controller,
                        address,
                        written: Vec::new(),
                        received: Vec::new(),
                        reading: false,
                        open: false,
                        last_at: 0,
                        started_at: 0,
                    });
                    (controller, address, device)
                })
                .collect()
        }

        fn uses_spi_pins(&self) -> bool {
            self.board
                .borrow()
                .buses()
                .spi
                .keys()
                .any(|&(owner, _)| owner == self.index)
        }

        /// A transaction with no pin route: use the only device on the chip's
        /// controller, or the board's only device when the controller numbering
        /// differs between the board and the chip (a board names `spi0`, the
        /// C3 calls its GP-SPI host 2). No device or an ambiguous one floats.
        fn spi_transfer(&mut self, host: u8, tx: &[u8], rx_len: usize) -> Vec<u8> {
            let rate = self.spi_rate(host);
            let mut board = self.board.borrow_mut();
            let selected = board
                .buses()
                .spi_sole(self.index, host)
                .or_else(|| board.buses().spi_only_device(self.index));
            match selected {
                Some((controller, chip_select)) => {
                    // The waveform is reconstructed here, so the rate has to
                    // reach the board before the transaction does.
                    if let Some(hz) = rate {
                        board.set_spi_hz(self.index, controller, hz);
                    }
                    board.spi_transfer(self.index, controller, chip_select, tx, rx_len)
                }
                None => vec![0xff; rx_len],
            }
        }

        /// A routed transaction: the asserted chip-select lines select the
        /// device, exactly as they do on the board.
        fn spi_transfer_pins(
            &mut self,
            host: u8,
            pins: SpiPins,
            tx: &[u8],
            rx_len: usize,
        ) -> Vec<u8> {
            let rate = self.spi_rate(host);
            let mut board = self.board.borrow_mut();
            match board.buses().spi_selected(self.index, pins.cs) {
                Some((controller, chip_select)) => {
                    if let Some(hz) = rate {
                        board.set_spi_hz(self.index, controller, hz);
                    }
                    board.spi_transfer(self.index, controller, chip_select, tx, rx_len)
                }
                None => vec![0xff; rx_len],
            }
        }

        fn uses_uart_pins(&self) -> bool {
            self.board
                .borrow()
                .buses()
                .uart
                .keys()
                .any(|&(owner, _)| owner == self.index)
        }

        /// A byte the chip transmitted reaches the peer on `route.port`. The
        /// route names the chip's receive pin, so remember it for `uart_rx`.
        fn uart_tx(&mut self, cycle: u64, route: UartRoute, byte: u8) {
            let now = self.ns(cycle);
            let port = route.port as u8;
            let baud = route
                .baud
                .unwrap_or(litz_board::runtime::OBSERVED_UART_BAUD);
            let mut board = self.board.borrow_mut();
            board.set_uart_baud(self.index, port, baud);
            board.uart_receive(self.index, port, now, &[byte]);
            drop(board);
            if let Some(pin) = route.rx_pin {
                self.uart_routes
                    .insert(port, (pin, route.baud.unwrap_or(115_200)));
            }
        }

        /// Bytes the board's UART peers have ready, one input per port. The
        /// chip's last route supplies the pin and baud; before the chip has
        /// transmitted, the board's resolved wiring names the receive pin and
        /// the usual 115 200 baud is assumed. A port with neither is skipped.
        fn uart_rx(&mut self, cycle: u64) -> Vec<UartInput> {
            let now = self.ns(cycle);
            let mut board = self.board.borrow_mut();
            for (&port, &(_, baud)) in &self.uart_routes {
                board.set_uart_baud(self.index, port, baud);
            }
            let ports: Vec<(u8, Option<(u8, u32)>)> = board
                .buses()
                .uart
                .keys()
                .filter(|(owner, _)| *owner == self.index)
                .map(|&(_, port)| {
                    let wired = board
                        .flat()
                        .buses
                        .uart
                        .get(&(self.index, port))
                        .map(|bus| (bus.chip_rx, 115_200));
                    (port, self.uart_routes.get(&port).copied().or(wired))
                })
                .collect();
            let mut inputs = Vec::new();
            for (port, route) in ports {
                let Some((pin, baud)) = route else { continue };
                let data = board.uart_poll(self.index, port, now);
                if !data.is_empty() {
                    inputs.push(UartInput::new(pin, baud, data));
                }
            }
            inputs
        }

        /// The chip completed a TWAI transmission: put it on the board's shared CAN wire.
        /// The node is the board index, so two MCUs on one diagram are distinct endpoints.
        fn can_tx(&mut self, _host: u8, frame: &[u8]) {
            let Some(frame) = esp_soc::board::CanFrame::decode(frame) else {
                return;
            };
            let mut board = self.board.borrow_mut();
            let can = board.buses_mut().can_mut();
            can.attach(self.index);
            let _ = can.transmit(self.index, board_can_frame(&frame));
        }

        /// The chip's TWAI receive buffer is empty: hand it the next frame addressed to this
        /// board's node. Attaching lazily keeps a CAN-free diagram unchanged; nothing is
        /// queued for a board that never drives TWAI.
        fn can_take(&mut self, _host: u8) -> Option<Vec<u8>> {
            let mut board = self.board.borrow_mut();
            let can = board.buses_mut().can_mut();
            can.attach(self.index);
            let frame = can.take(self.index)?;
            // The chip's wire frame holds only the eight classic CAN bytes.
            let mut data = [0u8; esp_soc::board::CAN_MAX_DATA];
            let len = usize::from(frame.len).min(data.len());
            data[..len].copy_from_slice(&frame.data[..len]);
            Some(
                esp_soc::board::CanFrame {
                    id: frame.id,
                    extended: frame.extended,
                    rtr: frame.rtr || frame.kind == litz_board::bus::CanFrameKind::Remote,
                    len: frame.len,
                    data,
                }
                .encode()
                .to_vec(),
            )
        }
    }

    /// The [`litz_board::Chip`] view of this adapter: what a runner needs, with
    /// nothing esp32sim-specific in the signature.
    impl<S: Soc> litz_board::Chip for EspChip<S> {
        fn info(&self) -> litz_board::ChipInfo {
            litz_board::ChipInfo {
                name: S::NAME.into(),
                cpu_hz: S::CPU_HZ,
                flash: litz_board::FlashLayout {
                    size: self.flash as u64,
                    bootloader: S::BOOTLOADER_OFFSET as u64,
                    partition_table: 0x8000,
                    app: 0x1_0000,
                },
            }
        }

        fn load_image(
            &mut self,
            kind: litz_board::ImageKind,
            offset: u64,
            bytes: &[u8],
        ) -> Result<(), String> {
            match kind {
                litz_board::ImageKind::Rom => self.load_rom(bytes),
                litz_board::ImageKind::Bootloader => {
                    self.bootloader_loaded = true;
                    self.write_flash(offset as usize, bytes)
                }
                _ => self.write_flash(offset as usize, bytes),
            }
        }

        fn set_psram_size(&mut self, bytes: usize) -> Result<(), String> {
            EspChip::set_psram_size(self, bytes)
        }

        fn boot(&mut self) -> Result<u32, String> {
            if self.bootloader_loaded {
                // The bootloader finds the application itself; starting the app
                // directly would skip it.
                return Ok(0);
            }
            let app = self.info().flash.app as usize;
            self.boot_app(app)
        }

        fn schedule(&mut self, inputs: Vec<litz_board::Input>) -> Result<(), String> {
            let mut text = String::new();
            for input in inputs {
                match input {
                    litz_board::Input::Gpio { at_ns, pin, level } => {
                        let seconds = at_ns as f64 / 1e9;
                        let bit = u8::from(level == Level::High);
                        text.push_str(&format!("{seconds:.6} gpio {pin} {bit}\n"));
                    }
                    litz_board::Input::Uart { at_ns, bytes } => {
                        let seconds = at_ns as f64 / 1e9;
                        let text_in = String::from_utf8_lossy(&bytes);
                        // The script format appends the newline itself.
                        text.push_str(&format!(
                            "{seconds:.6} uart0 {}\n",
                            text_in.trim_end_matches('\n')
                        ));
                    }
                }
            }
            if text.is_empty() {
                Ok(())
            } else {
                self.load_script(&text)
            }
        }

        fn run(&mut self, budget_cycles: u64) -> litz_board::RunResult {
            let stop = match EspChip::run(self, budget_cycles) {
                esp_soc::Stop::MaxInsns => litz_board::Stop::Budget,
                esp_soc::Stop::Halted => litz_board::Stop::Halted,
                esp_soc::Stop::SwReset => litz_board::Stop::Reset,
                esp_soc::Stop::Breakpoint(pc) => litz_board::Stop::Breakpoint(pc),
                _ => litz_board::Stop::Idle,
            };
            litz_board::RunResult {
                cycles: budget_cycles,
                stop,
            }
        }

        fn seconds(&self) -> f64 {
            EspChip::seconds(self)
        }

        fn write_console(&mut self, bytes: &[u8]) {
            // Both interpreters the runner speaks to read their console from
            // UART0. Mirroring the bytes onto the USB block as well splits one
            // paste across two input paths and the guest sees it twice.
            self.write_serial(bytes);
        }

        fn take_console(&mut self) -> Vec<u8> {
            let mut bytes = self.take_uart0();
            // The machine's own drain already moved USB output into this
            // backlog; a chip without a USB peripheral leaves it empty. A
            // build that logs to both consoles sends the same bytes down each,
            // so an identical copy is the same console message, not a second
            // one.
            let usb = core::mem::take(&mut self.machine.console.usb);
            if usb != bytes {
                bytes.extend(usb);
            }
            bytes
        }

        fn state(&self, part: &str) -> Option<PartState> {
            EspChip::state(self, part)
        }

        fn press(&mut self, part: &str, pressed: bool) -> bool {
            EspChip::press(self, part, pressed)
        }

        fn set_control(&mut self, part: &str, value: u16) -> bool {
            EspChip::set_control(self, part, value)
        }

        fn set_named_control(&mut self, part: &str, name: &str, value: &str) -> bool {
            EspChip::set_named_control(self, part, name, value)
        }

        fn part_pin_level(&self, part: &str, pin: Option<&str>) -> Option<Level> {
            EspChip::part_pin_level(self, part, pin)
        }

        fn gpio_level(&self, gpio: u8) -> Option<Level> {
            EspChip::gpio_level(self, gpio)
        }

        fn conflicts(&self) -> Vec<Conflict> {
            EspChip::conflicts(self)
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
            EspChip::break_at(self, pc);
        }

        fn registers(&self) -> String {
            self.machine.dump_regs()
        }

        fn report(&mut self) -> Vec<String> {
            EspChip::reports(self)
        }

        fn enable_coverage(&mut self) {
            EspChip::enable_coverage(self);
        }

        fn save_display(&mut self, path: &std::path::Path) -> Result<(), String> {
            crate::screenshot::capture(self, path)
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

        fn sample_heap(&mut self) -> Result<u32, String> {
            EspChip::sample_heap(self)
        }

        fn touch(&mut self, x: u16, y: u16, down: bool) -> Result<(), String> {
            let Some(board) = &self.board else {
                return Err("this chip is not attached to a board".into());
            };
            board.borrow_mut().touch_panel(x, y, down)
        }

        fn start_air_capture(&mut self, path: &std::path::Path) -> Result<(), String> {
            EspChip::start_air_capture(self, path)
        }

        fn start_ethernet_capture(&mut self, path: &std::path::Path) -> Result<(), String> {
            EspChip::start_ethernet_capture(self, path)
        }

        fn attach_host_gateway(&mut self, forwards: &[String]) -> Result<Vec<String>, String> {
            #[cfg(not(target_arch = "wasm32"))]
            {
                if forwards.is_empty() {
                    EspChip::attach_host_nat(self, false).map(|()| Vec::new())
                } else {
                    EspChip::attach_host_forwards(self, forwards, false)
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = forwards;
                Err("the browser build has no host sockets".into())
            }
        }

        fn debug(&mut self) -> Option<&mut dyn litz_board::ChipDebug> {
            Some(self)
        }
    }

    impl<S: Soc> litz_board::ChipDebug for EspChip<S> {
        fn serve(&mut self, port: u16) -> Result<(), String> {
            self.gdb_serve(port)
        }

        fn report(&mut self) -> String {
            self.machine.dump_regs()
        }
    }

    #[cfg(all(test, feature = "esp32c3"))]
    mod ble_peers {
        use super::*;

        fn beacon() -> litz_ble::Beacon {
            litz_ble::Beacon {
                name: Some("late".into()),
                services: Vec::new(),
                address: [0x04, 0x03, 0x02, 0x01, 0xaa, 0xc4],
                rssi: -42,
                adv: vec![2, 1, 6, 6, 9, b'l', b'a', b't', b'e'],
                scan_response: Vec::new(),
                gatt: Vec::new(),
            }
        }

        /// A scan enabled before the peer arrives hears it on the next report.
        /// The controller is stood up just far enough for `ble_nearby` to
        /// accept a list: firmware assets are not needed to test the list.
        #[test]
        fn a_peer_added_at_runtime_reaches_a_live_scan() {
            let mut chip = C3::new([0x24, 0x0a, 0xc4, 0x00, 0x01, 0x10], 4 << 20);
            assert_eq!(
                chip.add_ble_peer(&beacon()).unwrap_err(),
                "BLE is not enabled on this chip"
            );
            chip.machine_mut().bus.ble.hooks.push(0);
            chip.machine_mut().bus.ble.lines.keep();
            chip.ble_attached = true;

            // LE Set Scan Enable: enabled, passive.
            chip.machine_mut()
                .bus
                .ble
                .session
                .send(&[1, 0x0c, 0x20, 2, 1, 0]);
            chip.machine_mut().bus.ble.session.drain_log();

            chip.add_ble_peer(&beacon())
                .expect("the scan accepts the list");
            assert_eq!(chip.ble_peers.len(), 1);
            let line = chip.ble_log().expect("the controller logged a report");
            assert!(line.contains("scan report"), "{line}");
            assert!(line.contains("c4:aa:01:02:03:04"), "{line}");
        }
    }
}

const PLIC_AND_RV_MAP: [MemoryRegion; 6] = [
    MemoryRegion::new("irom", 0x4200_0000, 0x0040_0000),
    MemoryRegion::new("drom", 0x3C00_0000, 0x0040_0000),
    MemoryRegion::new("dram", 0x3FC8_0000, 0x0004_0000),
    MemoryRegion::new("irom-dram", 0x4037_C000, 0x0000_8000),
    MemoryRegion::new("peripherals", 0x6000_0000, 0x0010_0000),
    MemoryRegion::new("rtc", 0x5000_0000, 0x0001_0000),
];
