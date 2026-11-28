//! The debug surface: disassembly, registers, breakpoints, memory.

mod common;

use common::*;
use litz_board::{Chip, ImageKind};
use litz_ch32v003::Ch32v003;

fn chip_with(image: &Image) -> Ch32v003 {
    let mut chip = Ch32v003::new();
    chip.load_image(ImageKind::Firmware, 0, &image.bytes())
        .unwrap();
    chip.boot().unwrap();
    chip
}

#[test]
fn the_disassembler_prints_the_program_it_is_given() {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(
        0x10,
        &[
            addi(1, 0, 5),
            lui(2, 0x20000),
            sw(2, 0, 4),
            lw(3, 1, 0),
            jal(0, 4),
            beq(1, 2, -4),
            csrrw(0, 0x300, 1),
            MRET,
            mul(1, 2, 3),
        ],
    );
    image.half(0x34, c_li(8, 5));
    image.half(0x36, c_j(4));

    let mut chip = chip_with(&image);
    let text = chip.debug_disassemble(0x10, 11);
    for expected in [
        "addi x1, x0, 0x5",
        "lui x2, 0x20000",
        "sw x2, 0x4(x0)",
        "lw x3, 0x0(x1)",
        "jal x0, 0x4",
        "beq x1, x2, -0x4",
        "csrrw x0, 0x300, x1",
        "mret",
        ".insn 0x023100b3",
        "c.li x8, 5",
        "c.j 0x4",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}

#[test]
fn the_register_file_keeps_rv32e_at_sixteen_words() {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, &[addi(1, 0, 5), SELF]);
    let mut chip = chip_with(&image);
    assert!(chip.debug_available());
    assert_eq!(chip.debug_pc(), Some(0x10));

    assert_eq!(chip.debug_step(), Some(litz_board::Stop::Idle));
    let file = chip.debug_register_file().unwrap();
    assert_eq!(file.len(), 33);
    assert_eq!(file[1], 5);
    assert_eq!(file[32], 0x14, "the pc is the last word");
    for index in 16..32 {
        assert_eq!(file[index], 0, "x{index} does not exist on RV32E");
    }
    assert_eq!(chip.debug_pc(), Some(0x14));
}

#[test]
fn breakpoints_stop_the_run_before_the_instruction() {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, &[addi(1, 0, 1), addi(2, 0, 2), SELF]);
    let mut chip = chip_with(&image);
    chip.debug_set_breakpoints(&[0x14]);

    let result = chip.run(100);
    assert_eq!(result.stop, litz_board::Stop::Breakpoint(0x14));
    assert_eq!(chip.debug_pc(), Some(0x14));
    assert_eq!(chip.debug_register_file().unwrap()[1], 1);
    assert_eq!(chip.debug_register_file().unwrap()[2], 0);
}

#[test]
fn memory_reads_and_the_flash_image_agree() {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, &[addi(1, 0, 5), SELF]);
    let mut chip = chip_with(&image);

    let flash = chip.flash_image().unwrap();
    assert_eq!(flash.len(), 16 * 1024);
    assert_eq!(chip.debug_read_memory(0x10, 4), &flash[0x10..0x14][..]);
    assert!(chip.debug_read_memory(0x9000_0000, 4).is_empty());
}

#[test]
fn only_a_firmware_image_loads_and_an_illegal_word_traps() {
    let mut chip = Ch32v003::new();
    let error = chip.load_image(ImageKind::Rom, 0, &[0u8; 4]).unwrap_err();
    assert!(error.contains("firmware"), "{error}");

    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, &[mul(1, 2, 3)]);
    let mut chip = chip_with(&image);
    assert_eq!(chip.run(20).stop, litz_board::Stop::Trap);
    let fault = chip.fault().unwrap();
    assert!(fault.contains("illegal instruction"), "{fault}");
}
