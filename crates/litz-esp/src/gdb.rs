//! A GDB remote-serial-protocol stub over the ESP32 debug surface.
//!
//! The simulator is the target: a GDB client connects to a TCP port, reads the
//! program counter and memory, sets breakpoints, and resumes the same chip the
//! runner booted. `gdbstub` speaks the protocol; this module only maps its
//! `Target` operations onto [`crate::debug::GdbTarget`], so nothing here
//! names an esp32sim type.
//!
//! Register reads go through [`Rv32Chip::register_file`](crate::debug::Rv32Chip),
//! which every RISC-V adapter answers from the core's own fields; the Xtensa
//! parts have no target and say so. Memory is read through `Machine::peek`.
//! Writes are refused: the adapter has no public write path.
#![cfg(any(
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32p4",
    feature = "esp32s31"
))]

use crate::debug::{DebugStop, GdbTarget};
use gdbstub::arch::{Arch, RegId, Registers};
use gdbstub::common::Signal;
use gdbstub::conn::ConnectionExt;
use gdbstub::stub::run_blocking::{BlockingEventLoop, Event, WaitForStopReasonError};
use gdbstub::stub::{GdbStub, SingleThreadStopReason};
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::base::single_register_access::{
    SingleRegisterAccess, SingleRegisterAccessOps,
};
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadResume, SingleThreadResumeOps, SingleThreadSingleStep,
    SingleThreadSingleStepOps,
};
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, SwBreakpoint, SwBreakpointOps,
};
use gdbstub::target::{Target, TargetError, TargetResult};
use std::marker::PhantomData;
use std::net::{TcpListener, TcpStream};
use std::num::NonZeroUsize;

/// How many instructions one scheduling batch runs between checks for a GDB
/// interrupt. Large enough that `continue` is fast, small enough that a Ctrl-C
/// is seen promptly.
const RUN_BATCH: u64 = 1_000_000;

/// The RISC-V RV32 register layout GDB expects: x0..x31 then pc.
const TARGET_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
  <architecture>riscv:rv32</architecture>
  <feature name="org.gnu.gdb.riscv.cpu">
    <reg name="zero" bitsize="32" type="int"/>
    <reg name="ra" bitsize="32" type="int"/>
    <reg name="sp" bitsize="32" type="data_ptr"/>
    <reg name="gp" bitsize="32" type="data_ptr"/>
    <reg name="tp" bitsize="32" type="data_ptr"/>
    <reg name="t0" bitsize="32" type="int"/>
    <reg name="t1" bitsize="32" type="int"/>
    <reg name="t2" bitsize="32" type="int"/>
    <reg name="fp" bitsize="32" type="data_ptr"/>
    <reg name="s1" bitsize="32" type="int"/>
    <reg name="a0" bitsize="32" type="int"/>
    <reg name="a1" bitsize="32" type="int"/>
    <reg name="a2" bitsize="32" type="int"/>
    <reg name="a3" bitsize="32" type="int"/>
    <reg name="a4" bitsize="32" type="int"/>
    <reg name="a5" bitsize="32" type="int"/>
    <reg name="a6" bitsize="32" type="int"/>
    <reg name="a7" bitsize="32" type="int"/>
    <reg name="s2" bitsize="32" type="int"/>
    <reg name="s3" bitsize="32" type="int"/>
    <reg name="s4" bitsize="32" type="int"/>
    <reg name="s5" bitsize="32" type="int"/>
    <reg name="s6" bitsize="32" type="int"/>
    <reg name="s7" bitsize="32" type="int"/>
    <reg name="s8" bitsize="32" type="int"/>
    <reg name="s9" bitsize="32" type="int"/>
    <reg name="s10" bitsize="32" type="int"/>
    <reg name="s11" bitsize="32" type="int"/>
    <reg name="t3" bitsize="32" type="int"/>
    <reg name="t4" bitsize="32" type="int"/>
    <reg name="t5" bitsize="32" type="int"/>
    <reg name="t6" bitsize="32" type="int"/>
    <reg name="pc" bitsize="32" type="code_ptr"/>
  </feature>
</target>
"#;

/// The zero-variant marker type an `Arch` implementation is required to be.
enum Riscv32 {}

/// The register file as the backend can report it.
///
/// `None` means the value is genuinely unavailable. `Registers::gdb_serialize`
/// turns each missing byte into the protocol's "unavailable" marker, so GDB
/// shows `??` instead of a plausible-looking number.
#[derive(Clone, Debug, Default, PartialEq)]
struct RiscvRegisters {
    x: Option<[u32; 32]>,
    pc: Option<u32>,
}

impl Registers for RiscvRegisters {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.pc.unwrap_or(0)
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        for index in 0..32 {
            match (&self.x, index) {
                (Some(values), _) => {
                    for byte in values[index].to_le_bytes() {
                        write_byte(Some(byte));
                    }
                }
                // x0 is hard-wired to zero; that is a fact, not a guess.
                (None, 0) => {
                    for byte in 0u32.to_le_bytes() {
                        write_byte(Some(byte));
                    }
                }
                (None, _) => {
                    for _ in 0..4 {
                        write_byte(None);
                    }
                }
            }
        }
        match self.pc {
            Some(pc) => {
                for byte in pc.to_le_bytes() {
                    write_byte(Some(byte));
                }
            }
            None => {
                for _ in 0..4 {
                    write_byte(None);
                }
            }
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() != 33 * 4 {
            return Err(());
        }
        let mut values = [0u32; 32];
        for (index, value) in values.iter_mut().enumerate() {
            let start = index * 4;
            *value = u32::from_le_bytes(bytes[start..start + 4].try_into().map_err(|_| ())?);
        }
        let pc = u32::from_le_bytes(bytes[128..132].try_into().map_err(|_| ())?);
        self.x = Some(values);
        self.pc = Some(pc);
        Ok(())
    }
}

/// The GDB register number: x0..x31, then pc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RiscvRegId {
    X(u8),
    Pc,
}

impl RegId for RiscvRegId {
    fn from_raw_id(id: usize) -> Option<(Self, Option<NonZeroUsize>)> {
        let size = NonZeroUsize::new(4);
        match id {
            0..=31 => Some((RiscvRegId::X(id as u8), size)),
            32 => Some((RiscvRegId::Pc, size)),
            _ => None,
        }
    }

    fn to_raw_id(&self) -> Option<usize> {
        Some(match self {
            RiscvRegId::X(index) => *index as usize,
            RiscvRegId::Pc => 32,
        })
    }
}

impl Arch for Riscv32 {
    type Usize = u32;
    type Registers = RiscvRegisters;
    type BreakpointKind = usize;
    type RegId = RiscvRegId;

    fn target_description_xml() -> Option<&'static str> {
        Some(TARGET_XML)
    }
}

/// The state one GDB connection drives.
struct Stub<'a, T: GdbTarget> {
    target: &'a mut T,
    /// A resume or step has been requested and not yet reported as stopped.
    running: bool,
    /// The pending request executes one instruction rather than continuing.
    single_step: bool,
}

impl<'a, T: GdbTarget> Stub<'a, T> {
    fn new(target: &'a mut T) -> Self {
        Self {
            target,
            running: false,
            single_step: false,
        }
    }

    /// Run the pending request until it stops, or return `None` while the core
    /// is still executing so the caller can look for a GDB interrupt.
    fn advance(&mut self) -> Option<SingleThreadStopReason<u32>> {
        if !self.running {
            return Some(SingleThreadStopReason::Signal(Signal::SIGTRAP));
        }
        let stop = if self.single_step {
            self.running = false;
            self.target.step_once()
        } else {
            self.target.run_batch(RUN_BATCH)
        };
        match stop {
            DebugStop::Running => None,
            DebugStop::Stepped => {
                self.running = false;
                Some(SingleThreadStopReason::DoneStep)
            }
            DebugStop::Breakpoint(_) => {
                self.running = false;
                Some(SingleThreadStopReason::SwBreak(()))
            }
            DebugStop::Halted | DebugStop::Trapped | DebugStop::Reset | DebugStop::Stopped => {
                self.running = false;
                Some(SingleThreadStopReason::Signal(Signal::SIGTRAP))
            }
        }
    }
}

impl<'a, T: GdbTarget> Target for Stub<'a, T> {
    type Arch = Riscv32;
    type Error = String;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        BaseOps::SingleThread(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }
}

impl<'a, T: GdbTarget> SingleThreadBase for Stub<'a, T> {
    fn read_registers(&mut self, regs: &mut RiscvRegisters) -> TargetResult<(), Self> {
        match self.target.register_file() {
            Some(file) => {
                let mut x = [0u32; 32];
                x.copy_from_slice(&file[..32]);
                regs.x = Some(x);
                regs.pc = Some(file[32]);
            }
            None => {
                regs.x = None;
                regs.pc = self.target.pc();
            }
        }
        Ok(())
    }

    fn write_registers(&mut self, _regs: &RiscvRegisters) -> TargetResult<(), Self> {
        // Writing a register needs a public core accessor the adapter does not
        // have; refusing is better than silently discarding the write.
        Err(TargetError::NonFatal)
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        if data.is_empty() {
            return Ok(0);
        }
        let bytes = self.target.read_memory(start_addr, data.len());
        if bytes.is_empty() {
            return Err(TargetError::NonFatal);
        }
        let count = bytes.len().min(data.len());
        data[..count].copy_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn write_addrs(&mut self, _start_addr: u32, _data: &[u8]) -> TargetResult<(), Self> {
        // Memory writes would need a public bus write; `peek` is read-only.
        Err(TargetError::NonFatal)
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl<'a, T: GdbTarget> SingleThreadResume for Stub<'a, T> {
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.running = true;
        self.single_step = false;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl<'a, T: GdbTarget> SingleThreadSingleStep for Stub<'a, T> {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.running = true;
        self.single_step = true;
        Ok(())
    }
}

impl<'a, T: GdbTarget> SingleRegisterAccess<()> for Stub<'a, T> {
    fn read_register(
        &mut self,
        _tid: (),
        reg_id: RiscvRegId,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let value = match reg_id {
            RiscvRegId::Pc => match self.target.pc() {
                Some(pc) => Some(pc),
                None => return Ok(0),
            },
            // x0 is architecturally zero even though the rest of the file is
            // not exposed.
            RiscvRegId::X(0) => Some(0),
            RiscvRegId::X(_) => return Ok(0),
        };
        let Some(value) = value else {
            return Ok(0);
        };
        if buf.len() < 4 {
            return Err(TargetError::NonFatal);
        }
        buf[..4].copy_from_slice(&value.to_le_bytes());
        Ok(4)
    }

    fn write_register(
        &mut self,
        _tid: (),
        _reg_id: RiscvRegId,
        _val: &[u8],
    ) -> TargetResult<(), Self> {
        Err(TargetError::NonFatal)
    }
}

impl<'a, T: GdbTarget> Breakpoints for Stub<'a, T> {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }
}

impl<'a, T: GdbTarget> SwBreakpoint for Stub<'a, T> {
    fn add_sw_breakpoint(
        &mut self,
        addr: u32,
        _kind: <Self::Arch as Arch>::BreakpointKind,
    ) -> TargetResult<bool, Self> {
        self.target.arm_breakpoint(addr);
        Ok(true)
    }

    fn remove_sw_breakpoint(
        &mut self,
        _addr: u32,
        _kind: <Self::Arch as Arch>::BreakpointKind,
    ) -> TargetResult<bool, Self> {
        // esp32sim's breakpoint observer cannot be disarmed once added.
        // Reporting failure is truthful; the breakpoint stays armed.
        Ok(false)
    }
}

/// The blocking event loop `gdbstub` uses while the target is running.
///
/// It runs one batch at a time and checks the connection between batches, so a
/// Ctrl-C is seen even when the firmware never reaches a breakpoint.
struct EventLoop<'a, T: GdbTarget>(PhantomData<&'a mut T>);

impl<'a, T: GdbTarget> BlockingEventLoop for EventLoop<'a, T> {
    type Target = Stub<'a, T>;
    type Connection = TcpStream;
    type StopReason = SingleThreadStopReason<u32>;

    fn wait_for_stop_reason(
        target: &mut Self::Target,
        conn: &mut Self::Connection,
    ) -> Result<Event<Self::StopReason>, WaitForStopReasonError<String, std::io::Error>> {
        loop {
            if let Some(reason) = target.advance() {
                return Ok(Event::TargetStopped(reason));
            }
            match ConnectionExt::peek(conn) {
                Ok(Some(byte)) => return Ok(Event::IncomingData(byte)),
                Ok(None) => {}
                Err(error) => return Err(WaitForStopReasonError::Connection(error)),
            }
        }
    }

    fn on_interrupt(target: &mut Self::Target) -> Result<Option<Self::StopReason>, String> {
        target.running = false;
        target.single_step = false;
        Ok(Some(SingleThreadStopReason::Signal(Signal::SIGINT)))
    }
}

/// Serve one GDB session for `target` on `127.0.0.1:port`.
///
/// The call blocks until the client disconnects, so the caller decides when the
/// chip is handed over. Port 0 asks the host for a free port; the chosen
/// address is printed to stderr.
pub fn serve<T: GdbTarget>(target: &mut T, port: u16) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
    let address = listener
        .local_addr()
        .map_err(|e| format!("cannot read the listener address: {e}"))?;
    eprintln!("litz: gdb waiting on {address}");
    let (stream, _) = listener
        .accept()
        .map_err(|e| format!("cannot accept a GDB connection: {e}"))?;

    let mut stub = Stub::new(target);
    let gdb: GdbStub<'_, Stub<'_, T>, TcpStream> = GdbStub::new(stream);
    gdb.run_blocking::<EventLoop<'_, T>>(&mut stub)
        .map(|_| ())
        .map_err(|e| format!("gdb session failed: {e}"))
}
