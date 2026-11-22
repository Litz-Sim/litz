//! Fault-path tests for the RP2040 core and machine.
//!
//! Each test builds a small flash image (a `bx lr` stage 2 and an application
//! vector table at `0x1000_0100`), boots the machine the way the ROM does, and
//! then drives the failure the test is about through the public
//! [`Machine`]/[`Core`](litz_rp::Core) surface.

use litz_rp::bus::FLASH_BASE;
use litz_rp::machine::ROM_HANDOFF;
use litz_rp::{Fault, Machine, Stop};

/// Where the application's first instruction sits in the test images.
const APP_ENTRY: u32 = FLASH_BASE + 0x108;
/// The stack the ROM leaves for the application: the top of the first 256 KiB
/// of SRAM.
const STACK_TOP: u32 = 0x2004_1000;
/// An address in none of flash, SRAM, the ROM, the PPB or a peripheral block.
const UNMAPPED: u32 = 0x3000_0000;

/// A flash image whose stage 2 is `bx lr` (so the ROM hands off immediately)
/// and whose application vector table points at `app` at [`APP_ENTRY`].
fn image(app: &[u8]) -> Vec<u8> {
    let mut flash = vec![0xffu8; 0x200];
    flash[0..2].copy_from_slice(&0x4770u16.to_le_bytes()); // bx lr
    flash[0x100..0x104].copy_from_slice(&STACK_TOP.to_le_bytes());
    flash[0x104..0x108].copy_from_slice(&(APP_ENTRY | 1).to_le_bytes());
    flash[0x108..0x108 + app.len()].copy_from_slice(app);
    flash
}

fn booted(app: &[u8]) -> Machine {
    let mut machine = Machine::new();
    machine.load_flash(0, &image(app)).unwrap();
    machine.boot();
    machine
}

/// Boot through stage 2's `bx lr` and stop at the ROM handoff, one step before
/// the application's first instruction.
fn at_handoff(app: &[u8]) -> Machine {
    let mut machine = booted(app);
    assert_eq!(machine.step(), Ok(2), "stage 2 is a single bx lr");
    assert_eq!(machine.pc(), ROM_HANDOFF, "stage 2 returned to the ROM");
    machine
}

#[test]
fn an_undefined_instruction_traps_the_machine_instead_of_running_on() {
    // udf #0, then b . so anything that ignored the fault would loop forever.
    const APP: [u8; 4] = [0x00, 0xde, 0xfe, 0xe7];

    // The decoder reports the undefined halfword at the instruction address.
    let mut machine = at_handoff(&APP);
    assert_eq!(
        machine.step(),
        Err(Fault::Undefined {
            pc: APP_ENTRY,
            opcode: 0xde00
        })
    );

    // A run stops on the trap well before its budget, so the `b .` never ran.
    let mut machine = booted(&APP);
    assert_eq!(machine.run(1_000), Stop::Trap);
    assert!(machine.cycles() < 1_000, "the trap ended the run early");
    let fault = machine.fault().expect("the machine reports a fault");
    assert!(
        fault.contains("undefined instruction"),
        "unexpected fault text: {fault:?}"
    );
}

#[test]
fn an_unimplemented_32_bit_encoding_is_reported_as_undefined() {
    // 0xf7f0/0x8f00 is neither BL, MRS, MSR nor a barrier, so the 32-bit
    // decoder has no case for it. Note that `Fault::Unimplemented` is declared
    // but never constructed: a gap in the 32-bit set surfaces as `Undefined`.
    let mut machine = at_handoff(&[0xf0, 0xf7, 0x00, 0x8f]);

    match machine.step() {
        Err(Fault::Undefined { pc, .. }) => assert_eq!(pc, APP_ENTRY),
        other => panic!("expected an undefined-instruction fault, got {other:?}"),
    }
}

#[test]
fn a_fetch_outside_the_mapped_image_reports_a_bus_fault() {
    let mut machine = at_handoff(&[0xfe, 0xe7]); // b .
    // The core's register file is public; point it at an address the map does
    // not know.
    machine.cpu.r[15] = UNMAPPED;

    assert_eq!(
        machine.step(),
        Err(Fault::Bus {
            addr: UNMAPPED,
            write: false
        })
    );
    assert_eq!(
        machine.pc(),
        UNMAPPED,
        "a failed fetch leaves the PC in place"
    );
}

#[test]
fn a_load_from_an_unmapped_address_reports_a_bus_fault() {
    // ldr r0, [r1]
    let mut machine = at_handoff(&[0x08, 0x68]);
    machine.cpu.r[1] = UNMAPPED;

    assert_eq!(
        machine.step(),
        Err(Fault::Bus {
            addr: UNMAPPED,
            write: false
        })
    );
}

#[test]
fn a_store_to_an_unmapped_address_reports_a_bus_fault() {
    // str r0, [r1]
    let mut machine = at_handoff(&[0x08, 0x60]);
    machine.cpu.r[1] = UNMAPPED;

    assert_eq!(
        machine.step(),
        Err(Fault::Bus {
            addr: UNMAPPED,
            write: true
        })
    );
}

#[test]
fn a_bkpt_halts_the_machine_rather_than_trapping_it() {
    // bkpt #0 is the semihosting break the core turns into `Fault::Halted`.
    let mut machine = booted(&[0x00, 0xbe]);

    assert_eq!(machine.run(1_000), Stop::Halted);
    assert_eq!(machine.fault(), Some("core halted"));
    assert!(machine.cycles() < 1_000, "the halt ended the run early");
}

#[test]
fn a_trapped_machine_makes_no_progress_on_a_later_run() {
    let mut machine = at_handoff(&[0xfe, 0xe7]); // b .
    machine.cpu.r[15] = UNMAPPED;

    let cycles = machine.cycles();
    let pc = machine.pc();
    assert_eq!(machine.run(100), Stop::Trap);
    assert_eq!(
        machine.cycles(),
        cycles,
        "a faulting fetch consumes no cycles"
    );
    assert_eq!(machine.pc(), pc, "the PC stays at the faulting address");
    let fault = machine.fault().map(str::to_string);
    assert!(fault.is_some(), "the trap is reported");

    // Nothing resumes between runs: the same fault returns at the same PC with
    // the clock still stopped.
    assert_eq!(machine.run(100), Stop::Trap);
    assert_eq!(machine.cycles(), cycles);
    assert_eq!(machine.pc(), pc);
    assert_eq!(machine.fault(), fault.as_deref());
}

#[test]
fn two_machines_built_from_the_same_image_print_the_same_console_and_stop_alike() {
    let code = hello_uart();
    let mut first = booted(&code);
    let mut second = booted(&code);

    let first_stop = first.run(500);
    let second_stop = second.run(500);

    assert_eq!(first_stop, second_stop, "the same image stops the same way");
    assert_eq!(first_stop, Stop::Budget, "the image prints and then spins");

    let first_console = first.take_console();
    let second_console = second.take_console();
    assert_eq!(first_console, b"OK".to_vec(), "the guest wrote both bytes");
    assert_eq!(
        first_console, second_console,
        "byte-for-byte console output"
    );
    assert_eq!(
        first.cycles(),
        second.cycles(),
        "no hidden timing divergence"
    );
}

/// Enable UART0's transmitter, write `OK` to its data register, then spin.
///
/// The literal pool follows the code; the `ldr` offsets beside each word are
/// relative to the aligned `PC + 4` that ARMv6-M uses for literal loads.
fn hello_uart() -> Vec<u8> {
    // (offset from APP_ENTRY, halfword).
    let words: [(usize, u16); 8] = [
        (0x00, 0x4903), // ldr r1, [pc, #0x0c]  -> UART0 base
        (0x02, 0x4804), // ldr r0, [pc, #0x10]  -> CR bits
        (0x04, 0x6308), // str r0, [r1, #0x30]  -> UART0 CR
        (0x06, 0x4804), // ldr r0, [pc, #0x10]  -> 'O'
        (0x08, 0x6008), // str r0, [r1]         -> UART0 DR
        (0x0a, 0x4804), // ldr r0, [pc, #0x10]  -> 'K'
        (0x0c, 0x6008), // str r0, [r1]         -> UART0 DR
        (0x0e, 0xe7fe), // b .
    ];
    let mut code = vec![0u8; 0x20];
    for (offset, halfword) in words {
        code[offset..offset + 2].copy_from_slice(&halfword.to_le_bytes());
    }
    // Literal pool at 0x118.
    code[0x10..0x14].copy_from_slice(&0x4003_4000u32.to_le_bytes()); // UART0 base
    code[0x14..0x18].copy_from_slice(&0x0000_0101u32.to_le_bytes()); // UARTEN | TXE
    code[0x18..0x1c].copy_from_slice(&u32::from(b'O').to_le_bytes());
    code[0x1c..0x20].copy_from_slice(&u32::from(b'K').to_le_bytes());
    code
}
