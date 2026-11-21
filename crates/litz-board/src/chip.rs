//! What the runner needs from an MCU, whichever way that MCU is provided.
//!
//! [`Board`] owns the parts and resolves nets; everything above it — tests,
//! the CLI, a UI — talks to a `Chip`. Two implementations exist: the
//! `litz-esp` adapter over esp32sim, and `litz-mcu-wasm`, which loads a
//! WebAssembly component that speaks the `litz:mcu` contract. Neither the
//! board nor the runner knows which of the two it has.
//!
//! The trait is deliberately close to what a chip actually does: load images,
//! boot, run for a while, and exchange pin levels and console bytes. Anything
//! that only makes sense for one backend (a mask ROM, an esp32sim input
//! script) stays out of it.

use crate::Conflict;
use litz_core::{Level, PartState};

/// A piece of non-volatile memory a chip can be asked to load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    /// The mask ROM / firmware burned into the chip.
    Rom,
    /// A second-stage bootloader.
    Bootloader,
    /// A partition table.
    PartitionTable,
    /// The application.
    Firmware,
}

/// Where images live, in the chip's own terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashLayout {
    pub size: u64,
    pub bootloader: u64,
    pub partition_table: u64,
    pub app: u64,
}

/// Why a run came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The cycle budget ran out; more work remains.
    Budget,
    /// Nothing to do until the host changes an input.
    Idle,
    /// The core stopped on purpose.
    Halted,
    /// The core faulted; the detail was reported through the chip's fault
    /// channel.
    Trap,
    /// The chip asked for its own reset.
    Reset,
    /// The run reached a host-requested breakpoint at this address.
    Breakpoint(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunResult {
    pub cycles: u64,
    pub stop: Stop,
}

/// Something the host wants to happen at a simulated time.
///
/// Scheduling lives above the trait so each backend can implement it with the
/// clock it already has: esp32sim renders a script, a wasm chip compares
/// against the time it is asked to run to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// Assert a level on a chip pin (a bare GPIO number, not a part).
    Gpio { at_ns: u64, pin: u8, level: Level },
    /// Deliver bytes to the chip's console (UART0 receive).
    Uart { at_ns: u64, bytes: Vec<u8> },
}

/// The chip's own description.
#[derive(Clone, Debug)]
pub struct ChipInfo {
    pub name: String,
    pub cpu_hz: u64,
    pub flash: FlashLayout,
}

/// An MCU driving a [`crate::Board`].
///
/// Implementations push resolved input levels into the chip and read the drive
/// it holds on each pad back out; the board does the rest.
pub trait Chip {
    fn info(&self) -> ChipInfo;

    fn load_image(&mut self, kind: ImageKind, offset: u64, bytes: &[u8]) -> Result<(), String>;

    /// Make external PSRAM visible to the chip. A board that has none leaves
    /// the default, which reports the absence rather than pretending.
    fn set_psram_size(&mut self, _bytes: usize) -> Result<(), String> {
        Err("this backend has no external PSRAM".into())
    }

    /// Start the firmware and return its entry address.
    fn boot(&mut self) -> Result<u32, String>;

    /// Queue host actions. Replaces anything queued before.
    fn schedule(&mut self, inputs: Vec<Input>) -> Result<(), String>;

    /// Run for at most `budget_cycles` device cycles.
    fn run(&mut self, budget_cycles: u64) -> RunResult;

    /// Simulated seconds since reset.
    fn seconds(&self) -> f64;

    /// Bytes from a host terminal into the chip's console receive path.
    fn write_console(&mut self, bytes: &[u8]);

    /// Console bytes the chip has produced since the last call.
    fn take_console(&mut self) -> Vec<u8>;

    fn state(&self, part: &str) -> Option<PartState>;

    fn press(&mut self, part: &str, pressed: bool) -> bool;

    fn set_control(&mut self, part: &str, value: u16) -> bool;

    /// Set a named control on a part (`pressed`, `temperature`, `lux`). The
    /// default says the backend has no such surface, so a scenario can report
    /// the part rather than silently doing nothing.
    fn set_named_control(&mut self, _part: &str, _name: &str, _value: &str) -> bool {
        false
    }

    fn part_pin_level(&self, part: &str, pin: Option<&str>) -> Option<Level>;

    /// The level this chip sees on one of its own pads, by GPIO number. This
    /// is how a scenario addresses a board pin (`part-id: esp, name: 2`).
    fn gpio_level(&self, _gpio: u8) -> Option<Level> {
        None
    }

    /// Electrical faults the board found, in net order.
    fn conflicts(&self) -> Vec<Conflict>;

    /// Begin recording net transitions for [`Chip::vcd`]. A backend that does
    /// not record leaves the default and returns an empty dump.
    fn start_trace(&mut self) {}

    /// The recorded waveform as a Value Change Dump, empty when nothing was
    /// recorded.
    fn vcd(&self) -> String {
        String::new()
    }

    /// Stop the next run at this program counter, where the backend supports
    /// breakpoints. The stop comes back as [`Stop::Breakpoint`].
    fn break_at(&mut self, _pc: u32) {}

    /// Registers of the stopped core, for a debug print. Empty when the
    /// backend does not expose them.
    fn registers(&self) -> String {
        String::new()
    }

    /// Backend reports collected during the run — coverage, a pc histogram,
    /// a heap summary — one string per observer. Empty when no observer is
    /// enabled or the backend has none.
    fn report(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// Turn on instruction coverage for the next run, where the backend can.
    /// A backend without coverage ignores it.
    fn enable_coverage(&mut self) {}

    /// Why the chip stopped early, when it stopped for its own reasons (a
    /// trap, a fault the guest reported). `None` for a chip that has nothing
    /// to say.
    fn fault(&self) -> Option<String> {
        None
    }

    /// Write the simulated display to `path` as a PNG. A backend whose boards
    /// have no framebuffer says so instead of leaving an empty file.
    fn save_display(&mut self, _path: &std::path::Path) -> Result<(), String> {
        Err("this backend has no display to capture".into())
    }

    /// Write one display part's framebuffer to `path` as a PNG, by the part's
    /// diagram id. A backend without a board or a part with no display says so.
    fn save_part_display(&mut self, _part: &str, _path: &std::path::Path) -> Result<(), String> {
        Err("this backend cannot capture a part".into())
    }

    /// Deliver a touch at panel coordinates. A board with a touch controller
    /// sees the press through that part; a board without one says so.
    fn touch(&mut self, _x: u16, _y: u16, _down: bool) -> Result<(), String> {
        Err("this board has no touch controller".into())
    }

    /// Ask a running firmware for its free heap, by calling its own heap
    /// getter. Backends that cannot call into the guest say so.
    fn sample_heap(&mut self) -> Result<u32, String> {
        Err("this backend cannot sample a guest heap".into())
    }

    /// Record every 802.11 frame the radio puts on the air into a pcap with
    /// link type 105, written as the run goes.
    fn start_air_capture(&mut self, _path: &std::path::Path) -> Result<(), String> {
        Err("this backend has no 802.11 radio".into())
    }

    /// Record the Ethernet frames the station's MAC carries into a pcap with
    /// link type 1, written as the run goes.
    fn start_ethernet_capture(&mut self, _path: &std::path::Path) -> Result<(), String> {
        Err("this backend has no Ethernet transport".into())
    }

    /// Put host sockets behind the Wi-Fi gateway, forwarding these host ports
    /// into the guest (`tcp:[host:]port-[guest:]port`). Returns one line per
    /// forward. A run that uses this is not reproducible off the host.
    fn attach_host_gateway(&mut self, _forwards: &[String]) -> Result<Vec<String>, String> {
        Err("this backend has no Wi-Fi radio".into())
    }

    /// The debugger surface, when the backend has one. The protocol lives in
    /// the CLI; handing it a `ChipDebug` keeps every register and memory
    /// access on the backend that owns it.
    fn debug(&mut self) -> Option<&mut dyn ChipDebug> {
        None
    }
}

/// A GDB server over one chip.
///
/// The trait exists so a boxed [`Chip`] can still hand out its debug surface:
/// the runner knows a session can be served, and the backend knows how to read
/// its own registers and memory.
pub trait ChipDebug {
    /// Serve one client on `127.0.0.1:port`; returns when it disconnects.
    fn serve(&mut self, port: u16) -> Result<(), String>;

    /// The registers of the stopped core, for a run report.
    fn report(&mut self) -> String {
        String::new()
    }
}
/// A boxed chip is still a chip, so callers can hold the backend behind `dyn`
/// without threading a lifetime through every helper.
impl<T: Chip + ?Sized> Chip for Box<T> {
    fn info(&self) -> ChipInfo {
        (**self).info()
    }
    fn load_image(&mut self, kind: ImageKind, offset: u64, bytes: &[u8]) -> Result<(), String> {
        (**self).load_image(kind, offset, bytes)
    }
    fn set_psram_size(&mut self, bytes: usize) -> Result<(), String> {
        (**self).set_psram_size(bytes)
    }
    fn boot(&mut self) -> Result<u32, String> {
        (**self).boot()
    }
    fn schedule(&mut self, inputs: Vec<Input>) -> Result<(), String> {
        (**self).schedule(inputs)
    }
    fn run(&mut self, budget_cycles: u64) -> RunResult {
        (**self).run(budget_cycles)
    }
    fn seconds(&self) -> f64 {
        (**self).seconds()
    }
    fn write_console(&mut self, bytes: &[u8]) {
        (**self).write_console(bytes)
    }
    fn take_console(&mut self) -> Vec<u8> {
        (**self).take_console()
    }
    fn state(&self, part: &str) -> Option<PartState> {
        (**self).state(part)
    }
    fn press(&mut self, part: &str, pressed: bool) -> bool {
        (**self).press(part, pressed)
    }
    fn set_control(&mut self, part: &str, value: u16) -> bool {
        (**self).set_control(part, value)
    }
    fn set_named_control(&mut self, part: &str, name: &str, value: &str) -> bool {
        (**self).set_named_control(part, name, value)
    }
    fn part_pin_level(&self, part: &str, pin: Option<&str>) -> Option<Level> {
        (**self).part_pin_level(part, pin)
    }
    fn gpio_level(&self, gpio: u8) -> Option<Level> {
        (**self).gpio_level(gpio)
    }
    fn conflicts(&self) -> Vec<Conflict> {
        (**self).conflicts()
    }
    fn fault(&self) -> Option<String> {
        (**self).fault()
    }
    fn save_display(&mut self, path: &std::path::Path) -> Result<(), String> {
        (**self).save_display(path)
    }
    fn save_part_display(&mut self, part: &str, path: &std::path::Path) -> Result<(), String> {
        (**self).save_part_display(part, path)
    }
    fn sample_heap(&mut self) -> Result<u32, String> {
        (**self).sample_heap()
    }
    fn touch(&mut self, x: u16, y: u16, down: bool) -> Result<(), String> {
        (**self).touch(x, y, down)
    }
    fn start_air_capture(&mut self, path: &std::path::Path) -> Result<(), String> {
        (**self).start_air_capture(path)
    }
    fn start_ethernet_capture(&mut self, path: &std::path::Path) -> Result<(), String> {
        (**self).start_ethernet_capture(path)
    }
    fn attach_host_gateway(&mut self, forwards: &[String]) -> Result<Vec<String>, String> {
        (**self).attach_host_gateway(forwards)
    }
    fn debug(&mut self) -> Option<&mut dyn ChipDebug> {
        (**self).debug()
    }
    fn start_trace(&mut self) {
        (**self).start_trace()
    }
    fn vcd(&self) -> String {
        (**self).vcd()
    }
    fn break_at(&mut self, pc: u32) {
        (**self).break_at(pc)
    }
    fn registers(&self) -> String {
        (**self).registers()
    }
    fn report(&mut self) -> Vec<String> {
        (**self).report()
    }
    fn enable_coverage(&mut self) {
        (**self).enable_coverage()
    }
}
