//! Two identical runs must agree: no wall clock, no threads, no randomness.

mod common;

use common::*;
use litz_board::{Chip, ImageKind};
use litz_ch32v003::{CPU_HZ, Ch32v003};

fn program() -> Image {
    let [u1, u2] = li32(1, 0x4001_3800); // USART1
    let [c1, c2] = li32(2, 0x2008); // UE | TE
    let [t1, t2] = li32(4, 0x4000_0000); // TIM2
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(
        0x10,
        &[
            u1,
            u2,
            c1,
            c2,
            sw(2, 1, 0x0c),
            t1,
            t2,
            addi(3, 0, 99),
            sw(3, 4, 0x2c), // ARR
            addi(3, 0, 1),
            sw(3, 4, 0x00), // CEN
        ],
    );
    let mut at = 0x3c;
    for byte in b"det" {
        image.word(at, addi(5, 0, i32::from(*byte)));
        image.word(at + 4, sw(5, 1, 0x04));
        at += 8;
    }
    image.word(at, addi(6, 6, 1));
    image.word(at + 4, SELF);
    image
}

fn run_once(image: &Image) -> Ch32v003 {
    let mut chip = Ch32v003::new();
    chip.load_image(ImageKind::Firmware, 0, &image.bytes())
        .unwrap();
    chip.boot().unwrap();
    let result = chip.run(1_000);
    assert_eq!(result.stop, litz_board::Stop::Budget, "{:?}", chip.fault());
    chip
}

#[test]
fn two_identical_runs_agree() {
    let image = program();
    let mut first = run_once(&image);
    let mut second = run_once(&image);

    assert_eq!(first.take_console().as_slice(), b"det");
    assert_eq!(second.take_console().as_slice(), b"det");
    assert_eq!(
        first.debug_register_file(),
        second.debug_register_file(),
        "register files diverge"
    );
    assert_eq!(first.debug_pc(), second.debug_pc());
    assert_eq!(
        first.machine().cycles(),
        second.machine().cycles(),
        "the clock diverges"
    );
    assert_eq!(first.seconds(), second.seconds());
    assert!((first.seconds() - first.machine().cycles() as f64 / CPU_HZ as f64).abs() < 1e-12);
    assert!(first.fault().is_none());
}
