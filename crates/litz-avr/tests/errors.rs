//! Fault-path and determinism tests for the AVR core.
//!
//! These exercise the paths a broken or hostile firmware takes: an opcode the
//! decoder does not implement, a flash read past the end of the image, and a
//! data access outside the data space. Each must stop with a named [`Fault`]
//! rather than hang or invent behaviour. The last test pins reset determinism:
//! the same image run twice, with a reset in between, produces the same
//! registers and console output.

use litz_avr::cpu::Fault;
use litz_avr::{Machine, Stop};

/// Put 16-bit instruction words at a word address in flash.
fn load_words(machine: &mut Machine, word: usize, program: &[u16]) {
    let mut bytes = Vec::with_capacity(program.len() * 2);
    for instruction in program {
        bytes.extend_from_slice(&instruction.to_le_bytes());
    }
    machine.load_bin(&bytes, (word * 2) as u64).unwrap();
}

/// `ldi Rd, K` for `d` in 16..=31.
fn ldi(d: u8, k: u8) -> u16 {
    debug_assert!((16..=31).contains(&d));
    0xe000 | ((u16::from(k) & 0x00f0) << 4) | ((u16::from(d) - 16) << 4) | (u16::from(k) & 0x000f)
}

/// `sts k, Rr`, the two-word absolute store.
fn sts(r: u8, k: u16) -> [u16; 2] {
    [0x9200 | (u16::from(r) << 4), k]
}

/// `lpm Rd, Z`, load program memory through the Z pair.
fn lpm_z(d: u8) -> u16 {
    0x9004 | (u16::from(d) << 4)
}

#[test]
fn an_unimplemented_opcode_traps_with_unknown_opcode_and_its_byte_address() {
    let mut machine = Machine::new();
    // 0xffff matches no decoder arm, so it is rejected as an unknown opcode.
    load_words(&mut machine, 8, &[0xffff]);
    machine.boot();
    machine.cpu_mut().set_pc(8);

    let fault = machine.step().expect_err("0xffff is not an instruction");

    match fault {
        Fault::UnknownOpcode { addr, opcode } => {
            assert_eq!(opcode, 0xffff);
            assert_eq!(addr, 0x10, "word 8 is byte address 0x10");
        }
        other => panic!("wrong fault: {other:?}"),
    }

    assert_eq!(
        machine.run(1),
        Stop::Trap,
        "a trapped core stops instead of retiring further instructions"
    );
    assert_eq!(
        machine.fault(),
        Some(fault),
        "the machine reports the same unknown-opcode fault the step raised"
    );
}

#[test]
fn a_flash_read_past_the_end_traps_with_flash_out_of_bounds() {
    let mut machine = Machine::new();
    // A program fetch cannot leave flash: `set_pc` masks the counter into the
    // 32 KiB image, and `fetch_word` masks again. The reachable way to read
    // outside flash is a byte read through Z. Point Z at 0x8000, the first
    // address past the 32 KiB flash, then `lpm r16, Z`.
    let program = [ldi(30, 0x00), ldi(31, 0x80), lpm_z(16)];
    load_words(&mut machine, 0, &program);
    machine.boot();

    assert_eq!(machine.run(3), Stop::Trap);

    match machine.fault() {
        Some(Fault::FlashOutOfBounds { addr }) => {
            assert_eq!(addr, 0x8000, "Z pointed one byte past the 32 KiB flash");
        }
        other => panic!("wrong fault: {other:?}"),
    }
}

#[test]
fn a_store_above_sram_traps_with_data_out_of_bounds() {
    let mut machine = Machine::new();
    // ldi r16, 0x42 ; ldi r26, 0x00 ; ldi r27, 0x09 ; st X, r16
    // SRAM ends at 0x08ff, so X = 0x0900 is outside the data space.
    let program = [ldi(16, 0x42), ldi(26, 0x00), ldi(27, 0x09), 0x930c];
    load_words(&mut machine, 0, &program);
    machine.boot();

    assert_eq!(machine.run(4), Stop::Trap);

    match machine.fault() {
        Some(Fault::DataOutOfBounds { addr }) => {
            assert_eq!(addr, 0x0900, "X points one byte past the last SRAM byte");
        }
        other => panic!("wrong fault: {other:?}"),
    }
}

#[test]
fn a_trapped_machine_stays_trapped_and_does_not_execute_again() {
    let mut machine = Machine::new();
    // Same out-of-range store: trap on the fourth instruction.
    let program = [ldi(16, 0x42), ldi(26, 0x00), ldi(27, 0x09), 0x930c];
    load_words(&mut machine, 0, &program);
    machine.boot();

    assert_eq!(machine.run(4), Stop::Trap);
    let fault = machine.fault().expect("the store trapped");
    let pc = machine.pc();
    let cycles = machine.cycles();

    assert_eq!(
        machine.run(100),
        Stop::Trap,
        "running a trapped machine again returns the trap immediately"
    );
    assert_eq!(machine.fault(), Some(fault), "the fault is retained");
    assert_eq!(
        machine.step(),
        Ok(0),
        "a halted core retires no further instructions"
    );
    assert_eq!(machine.pc(), pc, "the program counter did not advance");
    assert_eq!(machine.cycles(), cycles, "no further cycles were retired");
}

#[test]
fn resetting_and_rerunning_the_same_image_gives_the_same_state() {
    let mut machine = Machine::new();
    // ldi r16, 'A' ; sts UDR0, r16 ; ldi r17, 1 ; add r16, r17 ; rjmp .
    let mut program = vec![ldi(16, b'A')];
    program.extend_from_slice(&sts(16, 0x00c6));
    program.extend_from_slice(&[ldi(17, 0x01), 0x0f01, 0xcfff]);
    load_words(&mut machine, 0, &program);
    machine.boot();

    assert_eq!(machine.run(6), Stop::Budget);
    let first_regs = *machine.cpu().regs();
    let first_console = machine.take_console();
    assert_eq!(first_regs[16], 0x42, "the add retired before the budget");
    assert_eq!(first_console, b"A".to_vec(), "the sketch wrote its byte");

    // A reset clears core and SRAM state; flash survives, so the same image
    // can be booted and run again.
    machine.reset();
    load_words(&mut machine, 0, &program);
    machine.boot();

    assert_eq!(machine.run(6), Stop::Budget);
    assert_eq!(
        *machine.cpu().regs(),
        first_regs,
        "the same image leaves the registers in the same state"
    );
    assert_eq!(
        machine.take_console(),
        first_console,
        "the same image produces the same console output"
    );
    assert_eq!(machine.fault(), None, "a clean program does not trap");
}
