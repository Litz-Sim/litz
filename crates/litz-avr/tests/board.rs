//! An ATmega328P driving a flattened Litz board.
//!
//! The board in these tests is the ESP32-S3 DevKitC manifest used purely as a
//! numbered header: its `GPIO0..GPIO13` and `GPIO14..GPIO19` rows are the
//! Uno's `D0..D13` and `A0..A5`. No Uno manifest exists in the catalog yet, and
//! the adapter only cares about the numeric header pin anyway.
//!
//! Every firmware is hand-assembled words, the same way the peripheral tests
//! build theirs, so the crate needs no toolchain to prove the bridge.

use std::cell::RefCell;
use std::rc::Rc;

use litz_avr::Atmega328p;
use litz_avr::peripherals::addr;
use litz_board::{Board, Chip, FlatBoard, I2cDevice, ImageKind};
use litz_catalog::Registry;
use litz_core::{Drive, Level, PartState};
use litz_diagram::Circuit;

// -- a tiny assembler ------------------------------------------------------

/// `ldi Rd, K` for `d` in 16..=31.
fn ldi(d: u8, k: u8) -> u16 {
    debug_assert!((16..=31).contains(&d));
    0xe000 | ((u16::from(k) & 0x00f0) << 4) | ((u16::from(d) - 16) << 4) | (u16::from(k) & 0x000f)
}

/// `in Rd, A`, the low-I/O load.
fn in_from(d: u8, io: u16) -> u16 {
    debug_assert!(io < 0x40);
    0xb000 | ((io & 0x30) << 5) | (u16::from(d) << 4) | (io & 0x0f)
}

/// `sbi A, b`, set a bit in the low I/O space. `address` is the data-space
/// address the peripheral table uses.
fn sbi(address: u16, bit: u8) -> u16 {
    0x9a00 | (((address - 0x20) & 0x1f) << 3) | u16::from(bit)
}

/// `sbrc Rr, b`, skip the next word when the bit is clear.
fn sbrc(r: u8, bit: u8) -> u16 {
    0xfc00 | (u16::from(r) << 4) | u16::from(bit)
}

/// `sbrs Rr, b`, skip the next word when the bit is set.
fn sbrs(r: u8, bit: u8) -> u16 {
    0xfe00 | (u16::from(r) << 4) | u16::from(bit)
}

/// `rjmp` with a word offset from the following instruction.
fn rjmp(offset: i16) -> u16 {
    0xc000 | ((offset as u16) & 0x0fff)
}

const RJMP_SELF: u16 = 0xcfff;

/// `lds Rd, k`, the two-word absolute load.
fn lds(d: u8, k: u16) -> [u16; 2] {
    [0x9000 | (u16::from(d) << 4), k]
}

/// `sts k, Rr`, the two-word absolute store.
fn sts(r: u8, k: u16) -> [u16; 2] {
    [0x9200 | (u16::from(r) << 4), k]
}

// -- fixtures --------------------------------------------------------------

fn flat_board(diagram: &str) -> Board {
    let circuit = Circuit::from_json(diagram).unwrap();
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap()
}

fn chip_on(diagram: &str) -> Atmega328p {
    Atmega328p::new().with_board(flat_board(diagram))
}

/// Load words at the reset vector and boot.
fn boot(chip: &mut Atmega328p, program: &[u16]) {
    let mut image = Vec::with_capacity(program.len() * 2);
    for word in program {
        image.extend_from_slice(&word.to_le_bytes());
    }
    chip.load_image(ImageKind::Firmware, 0, &image).unwrap();
    chip.boot().unwrap();
}

fn word_at(chip: &Atmega328p, offset: usize) -> u16 {
    let sram = chip.machine().sram();
    u16::from_le_bytes([sram[offset], sram[offset + 1]])
}

/// A numbered-header development board plus the parts a test wires to it.
fn diagram(connections: &[&str], parts: &[&str]) -> String {
    [
        r#"{"version":1,"parts":[{"id":"uno","type":"board-esp32-s3-devkitc-1","attrs":{}}"#,
        if parts.is_empty() { "" } else { "," },
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        "]}",
    ]
    .concat()
}

// -- tests -----------------------------------------------------------------

#[test]
fn a_blink_program_lights_the_boards_led() {
    // D13 (PB5) -> 220 ohm -> LED -> GND, the Uno's onboard LED circuit.
    let mut chip = chip_on(&diagram(
        &[
            r#"["uno:GPIO13","r1:1","",[]]"#,
            r#"["r1:2","led1:A","",[]]"#,
            r#"["led1:C","uno:GND.1","",[]]"#,
        ],
        &[
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"220"}}"#,
            r#"{"id":"led1","type":"litz-led","attrs":{"color":"green"}}"#,
        ],
    ));

    // sbi DDRB,5 ; sbi PORTB,5 ; rjmp .
    boot(
        &mut chip,
        &[sbi(addr::DDRB, 5), sbi(addr::PORTB, 5), RJMP_SELF],
    );
    assert_eq!(chip.state("led1"), Some(PartState::Led { lit: false }));

    let result = chip.run(100);
    assert_eq!(result.stop, litz_board::Stop::Budget);

    assert_eq!(
        chip.state("led1"),
        Some(PartState::Led { lit: true }),
        "PB5/D13 drives the LED high through its resistor"
    );
    assert!(chip.conflicts().is_empty(), "{:?}", chip.conflicts());
}

#[test]
fn a_button_on_d2_reaches_pinb() {
    // D2 (PD2) is pulled up by r1 and shorted to ground when btn1 is pressed.
    let mut chip = chip_on(&diagram(
        &[
            r#"["uno:3V3","r1:1","",[]]"#,
            r#"["r1:2","uno:GPIO2","",[]]"#,
            r#"["uno:GPIO2","btn1:1.l","",[]]"#,
            r#"["btn1:2.l","uno:GND.1","",[]]"#,
        ],
        &[
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10000"}}"#,
            r#"{"id":"btn1","type":"litz-pushbutton","attrs":{}}"#,
        ],
    ));

    // poll: in r16, PIND ; sbrc r16,2 ; rjmp poll
    //       ldi r17,1 ; sts 0x0100,r17 ; rjmp .
    let program = [
        in_from(16, addr::PIND - 0x20),
        sbrc(16, 2),
        rjmp(-3),
        ldi(17, 1),
        sts(17, 0x0100)[0],
        sts(17, 0x0100)[1],
        RJMP_SELF,
    ];
    boot(&mut chip, &program);

    // The pull-up holds D2 high, so the loop never falls through the branch.
    chip.run(200);
    assert_eq!(word_at(&chip, 0), 0, "D2 reads high before the press");

    assert!(chip.press("btn1", true), "the button takes a press");
    chip.run(400);
    assert_eq!(
        word_at(&chip, 0),
        1,
        "PD2 went low and the firmware stored its marker"
    );
    assert_eq!(
        chip.state("btn1"),
        Some(PartState::Button { pressed: true })
    );
}

#[test]
fn an_analog_pin_reads_the_boards_solved_voltage() {
    // A0 is GPIO14. The knob is at 100 %, so its wiper is the 3.3 V rail.
    let mut chip = chip_on(&diagram(
        &[
            r#"["uno:GND.1","pot1:1","",[]]"#,
            r#"["pot1:2","uno:GPIO14","",[]]"#,
            r#"["pot1:3","uno:3V3","",[]]"#,
        ],
        &[r#"{"id":"pot1","type":"litz-potentiometer","attrs":{"value":"100"}}"#],
    ));
    // The board solves the divider; the adapter publishes it as microvolts.
    chip.board().unwrap().borrow_mut().set_analog(true);
    chip.machine_mut().set_analog_reference(3_300_000);

    // ADMUX = AVcc | ADC0 ; ADCSRA = ADEN | ADSC | /128 ; poll ; read.
    let mut program = vec![ldi(16, 0x40)];
    program.extend_from_slice(&sts(16, addr::ADMUX));
    program.push(ldi(16, 0xc7));
    program.extend_from_slice(&sts(16, addr::ADCSRA));
    let poll = program.len();
    program.extend_from_slice(&lds(17, addr::ADCSRA));
    program.push(sbrc(17, 6));
    program.push(rjmp((poll as i16) - (program.len() as i16 + 1)));
    program.extend_from_slice(&lds(18, addr::ADCL));
    program.extend_from_slice(&lds(19, addr::ADCH));
    program.extend_from_slice(&sts(18, 0x0100));
    program.extend_from_slice(&sts(19, 0x0101));
    program.push(RJMP_SELF);
    boot(&mut chip, &program);

    chip.run(5_000);

    assert_eq!(
        word_at(&chip, 0),
        1023,
        "3.3 V at the A0 pad is full scale against a 3.3 V reference"
    );
}

/// An I²C target that records the bytes a controller writes.
struct Recorder {
    seen: Rc<RefCell<Vec<u8>>>,
}

impl I2cDevice for Recorder {
    fn connect(&mut self, _read: bool) {}

    fn read(&mut self) -> u8 {
        0xff
    }

    fn write(&mut self, byte: u8) -> bool {
        self.seen.borrow_mut().push(byte);
        true
    }

    fn stop(&mut self) {}
}

#[test]
fn an_i2c_write_from_firmware_reaches_a_board_device() {
    let mut chip = chip_on(&diagram(&[], &[]));
    let seen = Rc::new(RefCell::new(Vec::new()));
    chip.board()
        .unwrap()
        .borrow_mut()
        .buses_mut()
        .i2c_mut(0, 0)
        .attach(
            0x42,
            Box::new(Recorder {
                seen: Rc::clone(&seen),
            }),
        )
        .unwrap();

    // Wire write to 0x42: START, SLA+W, 'h', 'i', STOP.
    let mut program = Vec::new();
    program.push(ldi(16, 0xa4)); // TWINT | TWSTA | TWEN
    program.extend_from_slice(&sts(16, addr::TWCR));
    let poll = program.len();
    program.extend_from_slice(&lds(17, addr::TWCR));
    program.push(sbrs(17, 7));
    program.push(rjmp((poll as i16) - (program.len() as i16 + 1)));
    program.push(ldi(16, 0x84)); // SLA+W, address 0x42
    program.extend_from_slice(&sts(16, addr::TWDR));
    program.push(ldi(16, 0x84)); // TWINT | TWEN
    program.extend_from_slice(&sts(16, addr::TWCR));
    for &byte in b"hi" {
        program.push(ldi(16, byte));
        program.extend_from_slice(&sts(16, addr::TWDR));
        program.push(ldi(16, 0x84));
        program.extend_from_slice(&sts(16, addr::TWCR));
    }
    program.push(ldi(16, 0x94)); // TWINT | TWSTO | TWEN
    program.extend_from_slice(&sts(16, addr::TWCR));
    program.push(RJMP_SELF);
    boot(&mut chip, &program);

    chip.run(400);

    assert_eq!(
        seen.borrow().as_slice(),
        b"hi",
        "the board's device at 0x42 received the bytes the firmware sent"
    );
}

#[test]
fn the_twi_clock_the_sketch_programmed_reaches_the_board() {
    let mut chip = chip_on(&diagram(&[], &[]));
    chip.board()
        .unwrap()
        .borrow_mut()
        .buses_mut()
        .i2c_mut(0, 0)
        .attach(0x42, Box::new(NullDevice))
        .unwrap();

    // TWBR = 12, TWPS = 0: 16 MHz / (16 + 2 * 12) = 400 kHz.
    let mut program = Vec::new();
    program.push(ldi(16, 12));
    program.extend_from_slice(&sts(16, addr::TWBR));
    program.push(ldi(16, 0));
    program.extend_from_slice(&sts(16, addr::TWSR));
    program.push(ldi(16, 0xa4)); // TWINT | TWSTA | TWEN
    program.extend_from_slice(&sts(16, addr::TWCR));
    let poll = program.len();
    program.extend_from_slice(&lds(17, addr::TWCR));
    program.push(sbrs(17, 7));
    program.push(rjmp((poll as i16) - (program.len() as i16 + 1)));
    program.push(ldi(16, 0x84)); // SLA+W, address 0x42
    program.extend_from_slice(&sts(16, addr::TWDR));
    program.push(ldi(16, 0x84)); // TWINT | TWEN
    program.extend_from_slice(&sts(16, addr::TWCR));
    program.push(ldi(16, b'x'));
    program.extend_from_slice(&sts(16, addr::TWDR));
    program.push(ldi(16, 0x84));
    program.extend_from_slice(&sts(16, addr::TWCR));
    program.push(ldi(16, 0x94)); // TWINT | TWSTO | TWEN
    program.extend_from_slice(&sts(16, addr::TWCR));
    program.push(RJMP_SELF);
    boot(&mut chip, &program);

    chip.run(400);

    assert_eq!(
        chip.board().unwrap().borrow().i2c_hz(0, 0),
        400_000,
        "the board records the SCL rate TWBR asked for"
    );
}

/// A device that accepts writes and answers every read with zero.
struct NullDevice;

impl I2cDevice for NullDevice {
    fn connect(&mut self, _selected: bool) {}

    fn read(&mut self) -> u8 {
        0
    }

    fn write(&mut self, _byte: u8) -> bool {
        true
    }

    fn stop(&mut self) {}
}

#[test]
fn the_spi_clock_the_sketch_programmed_reaches_the_board() {
    let mut chip = chip_on(&diagram(&[], &[]));

    // SPCR = SPE | MSTR | SPR1:0 = 0 and SPSR.SPI2X = 1: 16 MHz / 2.
    let mut program = Vec::new();
    program.push(ldi(16, 0x50));
    program.extend_from_slice(&sts(16, addr::SPCR));
    program.push(ldi(16, 0x01));
    program.extend_from_slice(&sts(16, addr::SPSR));
    program.push(ldi(16, 0x5a));
    program.extend_from_slice(&sts(16, addr::SPDR));
    program.push(RJMP_SELF);
    boot(&mut chip, &program);

    chip.run(400);

    assert_eq!(
        chip.board().unwrap().borrow().spi_hz(0, 0),
        8_000_000,
        "the board records the SCK rate SPCR/SPSR asked for"
    );
}

#[test]
fn a_sensor_event_on_the_board_reaches_the_cpu() {
    // TRIG on D4, ECHO on D13 (PB5). The HC-SR04 raises ECHO 50 us after the
    // trigger falls, on the board's own clock.
    let mut chip = chip_on(&diagram(
        &[
            r#"["uno:GPIO4","sonar:TRIG","",[]]"#,
            r#"["uno:GPIO13","sonar:ECHO","",[]]"#,
            r#"["sonar:VCC","uno:3V3","",[]]"#,
            r#"["sonar:GND","uno:GND.1","",[]]"#,
        ],
        &[r#"{"id":"sonar","type":"litz-hc-sr04","attrs":{"distance":100.0}}"#],
    ));
    {
        let shared = chip.board().unwrap().clone();
        let mut board = shared.borrow_mut();
        board.set_chip_output(4, Drive::strong(Level::High), 0);
        board.set_chip_output(4, Drive::strong(Level::Low), 20_000);
        assert_eq!(
            board.next_event(),
            Some(70_000),
            "echo is 50 us after the fall"
        );
    }

    // poll: in r16, PINB ; sbrs r16,5 ; rjmp poll
    //       ldi r17,1 ; sts 0x0100,r17 ; rjmp .
    let program = [
        in_from(16, addr::PINB - 0x20),
        sbrs(16, 5),
        rjmp(-3),
        ldi(17, 1),
        sts(17, 0x0100)[0],
        sts(17, 0x0100)[1],
        RJMP_SELF,
    ];
    boot(&mut chip, &program);

    chip.run(10_000);

    assert_eq!(
        word_at(&chip, 0),
        1,
        "the board advanced to its 70 us event and PB5 went high before the budget ended"
    );
}
