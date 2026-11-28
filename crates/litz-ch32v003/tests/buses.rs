//! I2C1 and SPI1: one transaction against a real board part, and one against
//! a transfer callback so the controller's register handshake is covered
//! without a device.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::*;
use litz_board::{Board, Chip, FlatBoard, ImageKind};
use litz_catalog::Registry;
use litz_ch32v003::{Ch32v003, Machine, Stop};
use litz_diagram::Circuit;

const SRAM: u32 = 0x2000_0000;
const I2C1: u32 = 0x4000_5400;
const SPI1: u32 = 0x4001_3000;

fn flat_board(diagram: &str) -> Board {
    let circuit = Circuit::from_json(diagram).unwrap();
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap()
}

fn boot_chip(chip: &mut Ch32v003, program: &[u32]) {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, program);
    chip.load_image(ImageKind::Firmware, 0, &image.bytes())
        .unwrap();
    chip.boot().unwrap();
}

fn boot_machine(machine: &mut Machine, program: &[u32]) {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, program);
    machine.load_flash(0, &image.bytes()).unwrap();
    machine.boot();
}

#[test]
fn an_i2c_register_read_answers_from_the_boards_rtc() {
    let diagram = [
        r#"{"version":1,"parts":[{"id":"xiao","type":"board-seeed-xiao-rp2040","attrs":{}},"#,
        r#"{"id":"rtc","type":"litz-ds3231","attrs":{"time":"2026-11-20T13:45:30"}}],"#,
        r#""connections":["#,
        r#"["xiao:3V3","rtc:VCC","",[]],"#,
        r#"["xiao:GND","rtc:GND","",[]],"#,
        r#"["xiao:D4","rtc:SDA","",[]],"#,
        r#"["xiao:D5","rtc:SCL","",[]]]}"#,
    ]
    .concat();
    let mut chip = Ch32v003::new().with_board(flat_board(&diagram));

    let [b1, b2] = li32(1, I2C1);
    let [p1, p2] = li32(2, 0x401); // PE | ACK
    let [s1, s2] = li32(2, 0x501); // PE | ACK | START
    let [r1, r2] = li32(2, 0x601); // PE | ACK | STOP
    let [m1, m2] = li32(6, SRAM);
    boot_chip(
        &mut chip,
        &[
            b1,
            b2,
            p1,
            p2,
            sw(2, 1, 0x00), // enable
            s1,
            s2,
            sw(2, 1, 0x00), // START
            addi(3, 0, 0x68),
            slli(3, 3, 1),  // address 0x68, write
            sw(3, 1, 0x08), // OADDR1
            sw(0, 1, 0x10), // register pointer: seconds
            s1,
            s2,
            sw(2, 1, 0x00),   // repeated START
            addi(3, 0, 0xd1), // address 0x68, read
            sw(3, 1, 0x08),
            lw(5, 1, 0x10), // DATAR
            m1,
            m2,
            sw(5, 6, 0),
            r1,
            r2,
            sw(2, 1, 0x00), // STOP
            SELF,
        ],
    );

    let result = chip.run(200);
    assert_eq!(result.stop, litz_board::Stop::Budget, "{:?}", chip.fault());
    assert_eq!(chip.machine().sram()[0], 0x30, "13:45:30 is BCD 0x30");
    assert!(chip.conflicts().is_empty(), "{:?}", chip.conflicts());
}

#[test]
fn an_spi_frame_reaches_the_transfer_callback() {
    let mut machine = Machine::new();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    machine
        .periph_mut()
        .spi
        .set_transfer(Box::new(move |chip_select, tx| {
            sink.borrow_mut().push((chip_select, tx.to_vec()));
            vec![0xa5]
        }));

    let [b1, b2] = li32(1, SPI1);
    let [m1, m2] = li32(5, SRAM);
    boot_machine(
        &mut machine,
        &[
            b1,
            b2,
            addi(2, 0, 0x44), // SPE | MSTR
            sw(2, 1, 0x00),
            addi(3, 0, 0x5a),
            sw(3, 1, 0x0c), // DATAR: transfer
            lw(4, 1, 0x0c), // DATAR: the answer
            m1,
            m2,
            sw(4, 5, 0),
            SELF,
        ],
    );

    assert_eq!(machine.run(100), Stop::Budget, "{:?}", machine.fault());
    assert_eq!(
        u32::from_le_bytes([
            machine.sram()[0],
            machine.sram()[1],
            machine.sram()[2],
            machine.sram()[3]
        ]),
        0xa5
    );
    assert_eq!(*seen.borrow(), vec![(0u8, vec![0x5a])]);
}
