//! The debug surface of an esp32sim-backed chip.
//!
//! Two callers want the same facts: a person reading a failed CI run, and a GDB
//! client driving the chip. This module is the one place that reaches the
//! machine for them, so `litz-cli` never names an esp32sim type. The register
//! dump is the human-readable view and the RV32 core's public register fields
//! are the machine-readable one, memory is `Machine::peek`, and breakpoints are
//! esp32sim's own `Stop::Breakpoint`.
//!
//! The `GdbTarget` trait is deliberately narrow: it is what `litz-cli`'s GDB
//! stub needs and nothing more. A backend that cannot answer one of these
//! questions returns an empty value rather than inventing a number.
#![cfg(any(
    feature = "esp32",
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32s3",
    feature = "esp32p4",
    feature = "esp32s31"
))]

use crate::backend::EspChip;
use esp_soc::{Soc, Stop};

/// Why a debugging run returned, in terms the CLI and a GDB client share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugStop {
    /// The step budget was spent and the core is still executing.
    Running,
    /// One instruction retired (single-step).
    Stepped,
    /// The core halted itself, for example in `wfi`.
    Halted,
    /// A configured address was reached.
    Breakpoint(u32),
    /// The core trapped: `ebreak` with no handler, or an unimplemented instruction.
    Trapped,
    /// The firmware asked for a chip reset.
    Reset,
    /// Any other stop the backend reported.
    Stopped,
}

impl From<Stop> for DebugStop {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::MaxInsns => DebugStop::Running,
            Stop::Halted => DebugStop::Halted,
            Stop::Breakpoint(pc) => DebugStop::Breakpoint(pc),
            Stop::SwReset => DebugStop::Reset,
            Stop::Unimplemented(..) | Stop::Ebreak(_) => DebugStop::Trapped,
            _ => DebugStop::Stopped,
        }
    }
}

/// A chip whose core is RV32, and so has the register layout the GDB stub
/// advertises.
///
/// The Xtensa parts (ESP32, S3) need a different target description and a
/// windowed-register mapping; leaving them out of `GdbTarget` keeps the stub
/// from claiming a layout they do not have.
pub trait Rv32Chip: Soc + Sized {
    /// The RV32 integer registers (x0..x31) followed by pc.
    ///
    /// The concrete cores keep their register file in public fields; this
    /// method is where the per-chip type is known, so the generic session can
    /// read it without naming an esp32sim core type.
    fn register_file(machine: &esp_soc::Machine<Self>) -> Option<[u32; 33]>;

    /// The core's integer registers and program counter, mutably, for a host
    /// call into guest code (reading a heap counter without firmware help).
    fn call_state(machine: &mut esp_soc::Machine<Self>) -> Option<(&mut [u32; 32], &mut u32)>;
}

#[cfg(any(
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32p4",
    feature = "esp32s31"
))]
macro_rules! rv32_registers {
    ($chip:ty) => {
        impl Rv32Chip for $chip {
            fn register_file(machine: &esp_soc::Machine<Self>) -> Option<[u32; 33]> {
                let core = machine.cores.first()?;
                let mut file = [0u32; 33];
                file[..32].copy_from_slice(&core.x);
                file[32] = core.pc;
                Some(file)
            }

            fn call_state(
                machine: &mut esp_soc::Machine<Self>,
            ) -> Option<(&mut [u32; 32], &mut u32)> {
                let core = machine.cores.first_mut()?;
                Some((&mut core.x, &mut core.pc))
            }
        }
    };
}

#[cfg(feature = "esp32c3")]
rv32_registers!(esp32c3::C3);
#[cfg(feature = "esp32c6")]
rv32_registers!(esp32c6::C6);
#[cfg(feature = "esp32p4")]
rv32_registers!(esp32p4::P4);
#[cfg(feature = "esp32s31")]
rv32_registers!(esp32s31::S31);

/// The accessors a remote debugger needs from a chip.
///
/// `litz-cli`'s GDB stub is generic over this trait, so the CLI stays free of
/// esp32sim types. Implementations return `None` for facts the backend cannot
/// answer; a debugger is told the register is unavailable rather than being
/// handed a made-up zero.
pub trait GdbTarget {
    /// The program counter, if the backend can name it.
    fn pc(&self) -> Option<u32>;

    /// The 32 integer registers followed by the program counter, if the
    /// backend exposes the full register file.
    fn register_file(&self) -> Option<[u32; 33]> {
        None
    }

    /// Bytes readable at `addr`, stopping at the first inaccessible word.
    ///
    /// A short result is the GDB protocol's way of saying the range is only
    /// partly readable; an empty result means the first byte is not.
    fn read_memory(&mut self, addr: u32, len: usize) -> Vec<u8>;

    /// Stop the next run at `addr`.
    fn arm_breakpoint(&mut self, addr: u32);

    /// Execute at most `steps` instructions and report why execution ended.
    fn run_batch(&mut self, steps: u64) -> DebugStop;

    /// Execute exactly one instruction.
    fn step_once(&mut self) -> DebugStop;
}

impl<S: Rv32Chip> GdbTarget for EspChip<S> {
    fn pc(&self) -> Option<u32> {
        S::register_file(self.machine_ref()).map(|file| file[32])
    }

    fn register_file(&self) -> Option<[u32; 33]> {
        S::register_file(self.machine_ref())
    }

    fn read_memory(&mut self, addr: u32, len: usize) -> Vec<u8> {
        if len == 0 {
            return Vec::new();
        }
        // `peek` reads whole words and prints one line per word. Reading the
        // aligned word before `addr` costs nothing and lets a byte-addressed
        // GDB request share the same path.
        let start = addr & !3;
        let skip = (addr - start) as usize;
        let words = (skip + len).div_ceil(4);
        let text = self.machine_mut().peek(start, words);
        let mut bytes = Vec::with_capacity(words * 4);
        for line in text.lines() {
            let Some((_, word)) = line.split_once(": ") else {
                break;
            };
            let Ok(word) = u32::from_str_radix(word.trim(), 16) else {
                break;
            };
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        if bytes.len() < skip {
            return Vec::new();
        }
        let end = (skip + len).min(bytes.len());
        bytes[skip..end].to_vec()
    }

    fn arm_breakpoint(&mut self, addr: u32) {
        // esp32sim's observer stops before the instruction executes, which is
        // what GDB expects for a software breakpoint.
        self.break_at(addr);
    }

    fn run_batch(&mut self, steps: u64) -> DebugStop {
        EspChip::run(self, steps).into()
    }

    fn step_once(&mut self) -> DebugStop {
        match EspChip::run(self, 1) {
            // One instruction retired without a trap: that is a completed
            // single-step, not a still-running core.
            Stop::MaxInsns => DebugStop::Stepped,
            other => other.into(),
        }
    }
}

/// A human-readable debug report: observer reports, the register dump, and the
/// last stop the session observed.
///
/// The session wraps the chip because `EspChip` has no room for a "last stop"
/// field that every normal run would have to carry; a debugging session is the
/// only place that fact matters.
pub struct DebugSession<'a, S: Soc> {
    chip: &'a mut EspChip<S>,
    last_stop: Option<DebugStop>,
}

impl<'a, S: Soc> DebugSession<'a, S> {
    pub fn new(chip: &'a mut EspChip<S>) -> Self {
        Self {
            chip,
            last_stop: None,
        }
    }

    /// The enabled observers' end-of-run lines, the register dump, and why the
    /// last `run` returned. Each part is already formatted by the backend, so
    /// a caller can print the whole block unchanged.
    pub fn report(&mut self) -> String {
        let mut out = String::new();
        for line in self.chip.reports() {
            out.push_str(&line);
            if !line.ends_with('\n') {
                out.push('\n');
            }
        }
        out.push_str(&self.chip.registers());
        out.push_str(&format!("last stop: {}\n", self.stop_label()));
        out
    }

    /// Words starting at `addr`, one per line, as the backend printed them.
    ///
    /// esp32sim's `peek` is text, not bytes; a debugger that needs bytes goes
    /// through `GdbTarget::read_memory` above, which parses the same text.
    pub fn peek(&mut self, addr: u32, words: usize) -> String {
        self.chip.machine_mut().peek(addr, words)
    }

    /// Disassemble `count` instructions at `addr`, with esp32sim's symbols.
    pub fn disasm(&mut self, addr: u32, count: usize) -> String {
        self.chip.disassemble(addr, count)
    }

    /// Advance the chip and remember why it stopped.
    pub fn run(&mut self, steps: u64) -> DebugStop {
        let stop = EspChip::run(self.chip, steps).into();
        self.last_stop = Some(stop);
        stop
    }

    /// Arm a breakpoint without running.
    pub fn break_at(&mut self, addr: u32) {
        self.chip.break_at(addr);
    }

    fn stop_label(&self) -> String {
        match self.last_stop {
            None => "not run".into(),
            Some(DebugStop::Running) => "still running (step budget spent)".into(),
            Some(DebugStop::Stepped) => "single step".into(),
            Some(DebugStop::Halted) => "halted".into(),
            Some(DebugStop::Breakpoint(pc)) => format!("breakpoint at {pc:#010x}"),
            Some(DebugStop::Trapped) => "trap".into(),
            Some(DebugStop::Reset) => "chip reset".into(),
            Some(DebugStop::Stopped) => "stopped".into(),
        }
    }
}

/// The Xtensa half of [`EspChip::call_guest`]: one core's whole architectural
/// state is saved, a `call8` is synthesized, and execution stops at a sentinel
/// return address.
///
/// Unlike RV32 this cannot be written once over `S: Soc`: a windowed call reads
/// and rewrites the register file, so the implementation is generated per chip,
/// where the concrete core type is known.
#[cfg(any(feature = "esp32", feature = "esp32s3"))]
pub(crate) trait XtensaChip: Soc + Sized {
    /// Call `address` with no arguments and return the callee's `a2`.
    fn call_guest(chip: &mut EspChip<Self>, address: u32) -> Result<u32, String>;
}

/// Generates the per-chip windowed call. The body mirrors what executing
/// `call8 address` does: set CALLINC for the callee's `entry`, write the return
/// address into the caller's `a8` (the callee's `a0` after rotation), and jump.
#[cfg(any(feature = "esp32", feature = "esp32s3"))]
macro_rules! xtensa_call_guest {
    ($chip:ty) => {
        impl XtensaChip for $chip {
            fn call_guest(chip: &mut EspChip<Self>, address: u32) -> Result<u32, String> {
                // Keep the sentinel in the code region: `retw` restores the top
                // two address bits from its own PC, so an address outside that
                // region would not come back as this value.
                const SENTINEL: u32 = 0x4000_0000;
                const CALLINC: u32 = 2 << 16;
                const CALLINC_MASK: u32 = 3 << 16;
                const WOE: u32 = 1 << 18;
                const RETURN_TAG: u32 = 2 << 30;
                const BUDGET: u64 = 2_000_000;
                let machine = chip.machine_mut();
                let Some(core) = machine.cores.first_mut() else {
                    return Err("this chip has no core to call into".into());
                };
                // A call rotates the register window, allocates a frame and
                // rewrites windowstart, so restore the whole core rather than
                // reason about which fields the callee may touch.
                let saved = core.clone();
                // The call is the host's, so it also supplies the caller
                // context a real `call8` requires: windowed mode, and a core
                // that is not parked in `waiti`.
                core.ps = (core.ps & !CALLINC_MASK) | CALLINC | WOE;
                core.waiting = false;
                core.set_ar(8, RETURN_TAG | (SENTINEL & 0x3fff_ffff));
                core.pc = address;
                let mark = machine.observers.len();
                machine.add_observer(Box::new(esp_soc::observers::Breakpoints {
                    pcs: vec![SENTINEL],
                }));
                let stop = machine.run(BUDGET);
                machine.observers.truncate(mark);
                // `call8` rotates the window by eight registers, so the callee's
                // `a2` — where a windowed function leaves its result — is the
                // caller's `a10` once `retw` has restored the base.
                let value = machine.cores.first().map(|core| core.get_ar(10));
                if let Some(core) = machine.cores.first_mut() {
                    *core = saved;
                }
                match stop {
                    Stop::Breakpoint(pc) if pc == SENTINEL => {
                        value.ok_or_else(|| "the core disappeared during the call".to_string())
                    }
                    other => Err(format!(
                        "the call at 0x{address:08x} did not return: {other:?}"
                    )),
                }
            }
        }
    };
}

#[cfg(feature = "esp32")]
xtensa_call_guest!(esp32::Esp32);
#[cfg(feature = "esp32s3")]
xtensa_call_guest!(esp32s3::S3);

/// The RV32 half of [`EspChip::call_guest`]: save the register file, jump,
/// and stop when the sentinel return address dispatches again.
#[cfg(any(
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32p4",
    feature = "esp32s31"
))]
pub(crate) fn rv32_call_guest<S: Rv32Chip>(
    chip: &mut EspChip<S>,
    address: u32,
) -> Result<u32, String> {
    const BUDGET: u64 = 2_000_000;
    let machine = chip.machine_mut();
    let (saved, sentinel) = match S::call_state(machine) {
        Some((registers, pc)) => (registers.to_owned(), *pc),
        None => return Err("this chip has no RV32 core to call into".into()),
    };
    {
        let (registers, pc) = S::call_state(machine).expect("checked above");
        registers[1] = sentinel; // ra: return to the interrupted instruction
        *pc = address;
    }
    let mark = machine.observers.len();
    machine.add_observer(Box::new(esp_soc::observers::Breakpoints {
        pcs: vec![sentinel],
    }));
    let stop = machine.run(BUDGET);
    machine.observers.truncate(mark);
    let value = S::call_state(machine).map(|(registers, _)| registers[10]);
    if let Some((registers, pc)) = S::call_state(machine) {
        registers.copy_from_slice(&saved);
        *pc = sentinel;
    }
    match stop {
        esp_soc::soc::Stop::Breakpoint(_) => {
            value.ok_or_else(|| "the core disappeared during the call".to_string())
        }
        other => Err(format!(
            "the call at 0x{address:08x} did not return: {other:?}"
        )),
    }
}
