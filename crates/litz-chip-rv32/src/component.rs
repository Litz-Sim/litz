//! The component glue: the `litz:mcu` exports, and the only part of this
//! crate that is wasm-specific.
//!
//! It is compiled only for `wasm32-unknown-unknown`: the generated exports
//! refer to host symbols that exist inside a component and nowhere else, and
//! the machine underneath is testable without them.

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "simulator",
    });
}

use crate::machine::{DRAM_BASE, DRAM_LEN, Machine, RAM_BASE, RAM_LEN};
use bindings::exports::litz::mcu::chip::Guest;
use bindings::litz::mcu::types::{
    ChipInfo, FlashLayout, Image, Level, MemoryRegion, PinInput, PinOutput, RunResult, Stop,
};
use std::cell::RefCell;

/// The rate the host should count `run-result.cycles` at.
const CPU_HZ: u64 = 160_000_000;

thread_local! {
    /// One machine per component instance. `thread_local` rather than a
    /// `static mut` because the crate forbids unsafe and a component has one
    /// thread; a second instance in the same thread would get its own, which
    /// is what a host running two chips would want.
    static MACHINE: RefCell<Machine> = RefCell::new(Machine::new());
}

struct Chip;

fn with_machine<T>(f: impl FnOnce(&mut Machine) -> T) -> T {
    MACHINE.with(|machine| f(&mut machine.borrow_mut()))
}

fn flush_console(machine: &mut Machine) {
    if !machine.console.is_empty() {
        let bytes = std::mem::take(&mut machine.console);
        bindings::litz::mcu::host::console(&bytes);
    }
}

fn level_of(level: Level) -> bool {
    matches!(level, Level::High)
}

impl Guest for Chip {
    fn info() -> ChipInfo {
        ChipInfo {
            name: "litz-rv32".into(),
            description: "Reference RV32IMC chip: ESP32-C3 memory map, GPIO and UART0".into(),
            cpu_hz: CPU_HZ,
            memory: vec![
                MemoryRegion {
                    name: "iram".into(),
                    start: RAM_BASE,
                    len: RAM_LEN,
                },
                MemoryRegion {
                    name: "dram".into(),
                    start: DRAM_BASE,
                    len: DRAM_LEN,
                },
                MemoryRegion {
                    name: "gpio".into(),
                    start: 0x6000_4000,
                    len: 0x1000,
                },
                MemoryRegion {
                    name: "uart0".into(),
                    start: 0x6000_0000,
                    len: 0x1000,
                },
            ],
            flash: FlashLayout {
                size: 4 << 20,
                bootloader: 0x0,
                partition_table: 0x8000,
                app: 0x1_0000,
            },
        }
    }

    fn load_image(kind: Image, _offset: u64, bytes: Vec<u8>) -> Result<(), String> {
        match kind {
            // The reference has no separate ROM or partition table: the
            // application image is the whole input, and the boot path parses
            // it directly. Anything else is accepted and ignored so a
            // `litz.toml` written for a real ESP board still loads.
            Image::Firmware => with_machine(|machine| {
                machine.reset();
                machine.load(&bytes).map(|_| ())
            }),
            _ => Ok(()),
        }
    }

    fn reset(_mac: Vec<u8>) {
        with_machine(|machine| {
            machine.reset();
            flush_console(machine);
        });
    }

    fn boot() -> Result<u32, String> {
        with_machine(|machine| machine.boot()).ok_or_else(|| "no firmware image was loaded".into())
    }

    fn set_inputs(inputs: Vec<PinInput>) {
        with_machine(|machine| {
            for input in inputs {
                machine.set_input(input.pin, level_of(input.level));
            }
        });
    }

    fn run(budget_cycles: u64) -> RunResult {
        let (cycles, fault) = with_machine(|machine| {
            let cycles = machine.run(budget_cycles);
            flush_console(machine);
            (cycles, machine.halted.clone())
        });
        match fault {
            Some(detail) => {
                bindings::litz::mcu::host::fault(&detail);
                RunResult {
                    cycles,
                    stop: Stop::Trap,
                }
            }
            None => RunResult {
                cycles,
                stop: Stop::Budget,
            },
        }
    }

    fn take_outputs() -> Vec<PinOutput> {
        with_machine(|machine| {
            machine
                .take_outputs()
                .into_iter()
                .map(|(pin, high, enabled)| PinOutput {
                    pin,
                    level: if high { Level::High } else { Level::Low },
                    enabled,
                })
                .collect()
        })
    }

    fn write_console(bytes: Vec<u8>) {
        with_machine(|machine| machine.push_console(&bytes));
    }

    fn read(addr: u32, len: u32) -> Result<Vec<u8>, String> {
        with_machine(|machine| {
            let mut out = Vec::with_capacity(len as usize);
            for i in 0..len {
                use crate::cpu::Memory;
                let at = addr.wrapping_add(i);
                out.push(machine.read8(at).map_err(|trap| trap.describe(at))?);
            }
            Ok(out)
        })
    }
}

bindings::export!(Chip with_types_in bindings);
