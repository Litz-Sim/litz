//! Traps through the PFIC, `mret`, and the wait-for-interrupt path.

mod common;

use common::*;
use litz_ch32v003::{Machine, Stop};

const SRAM: u32 = 0x2000_0000;
const PFIC_ISR: u32 = 0xE000_E200;
const PFIC_IENR: u32 = 0xE000_E100;

/// Word offset in flash of the vector table entry for source `irq`.
fn vector(irq: u16) -> usize {
    4 * (usize::from(irq) + 1)
}

fn boot(image: Image) -> Machine {
    let mut machine = Machine::new();
    machine.load_flash(0, &image.bytes()).unwrap();
    machine.boot();
    machine
}

fn two(rd: u32, value: u32) -> [u32; 2] {
    li32(rd, value)
}

#[test]
fn a_pending_line_is_delivered_and_mret_returns() {
    let mut image = Image::new();
    image.word(0, 0x80);
    // USART1's source is 17; its handler is entry 18. The reset code starts
    // past the vector table so it cannot overwrite the entry it relies on.
    image.word(vector(17), 0x180);

    let [hi1, lo1] = two(1, PFIC_ISR);
    let [hi6, lo6] = two(6, SRAM);
    image.place(
        0x80,
        &[
            hi1,
            lo1, // x1 = PFIC IPSR
            hi6,
            lo6,           // x6 = SRAM
            addi(2, 0, 1), // x2 = 1 << 17
            slli(2, 2, 17),
            sw(2, 1, 0),        // raise source 17
            addi(1, 1, -0x100), // PFIC IENR
            sw(2, 1, 0),        // enable it
            addi(3, 0, 5),      // a loop that must finish before MIE
            addi(3, 3, -1),
            bne(3, 0, -4),
            addi(3, 0, 8),
            csrrs(0, 0x300, 3), // mstatus.MIE = 1
            jal(0, 0),
        ],
    );
    image.place(
        0x180,
        &[
            sw(3, 6, 0),        // the interrupted counter belongs to the loop
            csrrs(5, 0x342, 0), // mcause
            sw(5, 6, 4),
            addi(10, 10, 1),
            MRET,
        ],
    );

    let mut machine = boot(image);
    let stop = machine.run(300);
    assert_eq!(stop, Stop::Budget, "{:?}", machine.fault());
    assert_eq!(machine.cpu.reg(10), 1, "handler ran exactly once");
    assert_ne!(machine.cpu.mstatus & 0x8, 0, "MIE restored by mret");
    assert_eq!(machine.cpu.pc(), 0xb8, "returned to the interrupted loop");
    let sram = machine.sram();
    assert_eq!(
        u32::from_le_bytes([sram[0], sram[1], sram[2], sram[3]]),
        8,
        "the masked loop ran to completion before the handler"
    );
    assert_eq!(
        u32::from_le_bytes([sram[4], sram[5], sram[6], sram[7]]),
        0x8000_0011,
        "mcause names the machine external interrupt for source 17"
    );
}

#[test]
fn a_timer_update_reaches_its_pfic_source() {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.word(vector(22), 0x100);

    let [hi1, lo1] = two(1, 0x4000_0000); // TIM2
    let [hi9, lo9] = two(9, PFIC_IENR);
    image.place(
        0x10,
        &[
            hi1,
            lo1,
            addi(2, 0, 49), // ARR: update every 50 timer clocks
            sw(2, 1, 0x2c),
            addi(2, 0, 1),
            sw(2, 1, 0x0c), // DIER.UIE
            sw(2, 1, 0x00), // CTLR1.CEN
            add(8, 1, 0),   // handler keeps the timer base in x8
            hi9,
            lo9,
            addi(2, 0, 1),
            slli(2, 2, 22), // x2 = 1 << 22
            sw(2, 9, 0),    // enable TIM2 in the PFIC
            addi(3, 0, 8),
            csrrs(0, 0x300, 3),
            jal(0, 0),
        ],
    );
    image.place(
        0x100,
        &[
            addi(10, 10, 1),
            sw(0, 8, 0x10), // clear TIM2's update flag
            MRET,
        ],
    );

    let mut machine = boot(image);
    let stop = machine.run(400);
    assert_eq!(stop, Stop::Budget, "{:?}", machine.fault());
    assert!(
        (1..=10).contains(&machine.cpu.reg(10)),
        "the handler ran an update-driven number of times, got {}",
        machine.cpu.reg(10)
    );
}

#[test]
fn wfi_idles_until_an_event_and_wakes_on_the_timer() {
    // Nothing configured: the core idles instead of spinning out the budget.
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, &[WFI, SELF]);
    let mut machine = boot(image);
    assert_eq!(machine.run(100), Stop::Idle);
    assert!(machine.cpu.waiting);

    // `wfe` parks the same way: the part has no event register to wait on.
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, &[WFE, SELF]);
    let mut machine = boot(image);
    assert_eq!(machine.run(100), Stop::Idle);
    assert!(machine.cpu.waiting);

    // A running timer gives the run loop a moment to skip ahead to, and the
    // sleeping core resumes after it.
    let [hi1, lo1] = two(1, 0x4000_0000);
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(
        0x10,
        &[
            hi1,
            lo1,
            addi(2, 0, 9),
            sw(2, 1, 0x2c), // ARR
            addi(2, 0, 1),
            sw(2, 1, 0x00), // CEN
            WFI,
            addi(10, 0, 1),
            jal(0, 0),
        ],
    );
    let mut machine = boot(image);
    let stop = machine.run(500);
    assert_eq!(stop, Stop::Budget, "{:?}", machine.fault());
    assert_eq!(machine.cpu.reg(10), 1, "the instruction after wfi ran");
}
