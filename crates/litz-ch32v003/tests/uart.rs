//! USART1 against the console: transmit, and receive with a scheduled line.

mod common;

use common::*;
use litz_board::{Chip, ImageKind, Input};
use litz_ch32v003::Ch32v003;

const USART: u32 = 0x4001_3800;

fn boot(chip: &mut Ch32v003, program: &[u32]) {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, program);
    chip.load_image(ImageKind::Firmware, 0, &image.bytes())
        .unwrap();
    assert_eq!(chip.boot().unwrap(), 0x10);
}

#[test]
fn a_transmit_program_reaches_the_console() {
    let [hi1, lo1] = li32(1, USART);
    let [hi2, lo2] = li32(2, 0x2008); // UE | TE
    let mut program = vec![hi1, lo1, hi2, lo2, sw(2, 1, 0x0c)];
    for byte in b"Hi!" {
        program.push(addi(3, 0, i32::from(*byte)));
        program.push(sw(3, 1, 0x04));
    }
    program.push(SELF);

    let mut chip = Ch32v003::new();
    boot(&mut chip, &program);
    assert_eq!(chip.info().name, "ch32v003");
    assert_eq!(chip.info().cpu_hz, 48_000_000);

    let result = chip.run(200);
    assert_eq!(result.stop, litz_board::Stop::Budget, "{:?}", chip.fault());
    assert_eq!(chip.take_console().as_slice(), b"Hi!");
    assert!(chip.take_console().is_empty(), "the console drains");
}

#[test]
fn a_scheduled_line_is_received_and_echoed() {
    let [hi1, lo1] = li32(1, USART);
    let [hi2, lo2] = li32(2, 0x200c); // UE | TE | RE
    let mut program = vec![hi1, lo1, hi2, lo2, sw(2, 1, 0x0c)];

    // loop: lw x3, STATR ; andi x4, x3, RXNE ; beq x4, x0, loop
    //       lw x5, DATAR ; sw x5, DATAR ; jal x0, loop
    program.push(lw(3, 1, 0x00));
    program.push(andi(4, 3, 0x20));
    program.push(beq(4, 0, -8));
    program.push(lw(5, 1, 0x04));
    program.push(sw(5, 1, 0x04));
    program.push(jal(0, -20));

    let mut chip = Ch32v003::new();
    boot(&mut chip, &program);
    chip.schedule(vec![Input::Uart {
        at_ns: 100_000,
        bytes: b"hi".to_vec(),
    }])
    .unwrap();

    let result = chip.run(20_000);
    assert_eq!(result.stop, litz_board::Stop::Budget, "{:?}", chip.fault());
    assert_eq!(chip.take_console().as_slice(), b"hi");
    assert!(chip.fault().is_none());
}

#[test]
fn a_receiver_that_is_off_drops_host_bytes() {
    // The line is pushed before the firmware enables the receiver, so the
    // model must not invent a byte the pad never carried while listening.
    let [hi1, lo1] = li32(1, USART);
    let [hi2, lo2] = li32(2, 0x200c);
    let mut program = vec![hi1, lo1, hi2, lo2, sw(2, 1, 0x0c)];
    program.push(lw(3, 1, 0x00));
    program.push(SELF);

    let mut chip = Ch32v003::new();
    boot(&mut chip, &program);
    chip.write_console(b"x");
    chip.run(50);
    // STATR's RXNE was not set by the dropped byte; x3 holds the status word.
    let file = chip.debug_register_file().unwrap();
    assert_eq!(file[3] & 0x20, 0, "RXNE stayed clear");
}
