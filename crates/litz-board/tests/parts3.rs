//! Batch 3 on a real board: the analog shelf and the remaining digital
//! devices, wired through the catalog to the ESP32-C3 header.

use litz_board::bus_parts::{Ds1307, LcdI2c, Rc522};
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level, PartState};
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;

fn circuit(parts: &[&str], connections: &[&str]) -> Circuit {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        r#"]}"#,
    ]
    .concat();
    Circuit::from_json(&json).unwrap()
}

fn board(parts: &[&str], connections: &[&str]) -> Board {
    let circuit = circuit(parts, connections);
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).expect("the diagram flattens"))
        .expect("the board builds")
}

fn part(id: &str, type_key: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"{type_key}","attrs":{attrs}}}"#)
}

fn wire(from: &str, to: &str) -> String {
    format!(r#"["{from}","{to}","",[]]"#)
}

fn set(board: &mut Board, gpio: u8, level: Level) {
    board.set_chip_output(gpio, Drive::strong(level), 0);
}

fn set_at(board: &mut Board, gpio: u8, level: Level, at: u64) {
    board.set_chip_output(gpio, Drive::strong(level), at);
}

// ------------------------------------------------------------------ catalog

#[test]
fn every_batch_three_entry_is_in_the_builtin_catalog() {
    let registry = Registry::builtin();
    let ids = [
        // Analog and passive.
        "litz-capacitor-electrolytic",
        "litz-diode",
        "litz-diode-1n4148",
        "litz-diode-1n4007",
        "litz-zener",
        "litz-bjt",
        "litz-mosfet",
        "litz-opamp",
        "litz-voltage-regulator",
        "litz-battery",
        "litz-power-supply",
        "litz-photodiode",
        "litz-optocoupler",
        // Digital.
        "litz-lcd1602-i2c",
        "litz-lcd2004-i2c",
        "litz-7segment-4digit",
        "litz-led-ring",
        "litz-ds1307",
        "litz-rfid-rc522",
        "litz-fingerprint",
    ];
    for id in ids {
        assert!(
            registry.get(id).is_some(),
            "{id} is missing from the builtin catalog"
        );
    }
}

#[test]
fn the_analog_shelf_builds_on_a_board_and_never_drives() {
    let parts = [
        part("cap1", "litz-capacitor-electrolytic", r#"{"value":"47u"}"#),
        part("d1", "litz-diode", "{}"),
        part("d2", "litz-diode-1n4148", "{}"),
        part("d3", "litz-diode-1n4007", "{}"),
        part("z1", "litz-zener", r#"{"vz":5.1}"#),
        part("q1", "litz-bjt", r#"{"polarity":"npn"}"#),
        part("m1", "litz-mosfet", r#"{"channel":"n"}"#),
        part("u1", "litz-opamp", r#"{"variant":"LM358"}"#),
        part("v1", "litz-voltage-regulator", r#"{"variant":"7805"}"#),
        part("b1", "litz-battery", r#"{"voltage":9.0}"#),
        part("p1", "litz-power-supply", r#"{"voltage":5.0}"#),
        part("pd1", "litz-photodiode", "{}"),
        part("o1", "litz-optocoupler", r#"{"variant":"PC817"}"#),
    ];
    let mut all = vec![BOARD];
    all.extend(parts.iter().map(String::as_str));
    let board = board(&all, &[]);
    assert!(board.conflicts().is_empty(), "{:?}", board.conflicts());
    for id in [
        "cap1", "d1", "d2", "d3", "z1", "q1", "m1", "u1", "v1", "b1", "p1", "pd1", "o1",
    ] {
        assert_eq!(
            board.state(id),
            Some(PartState::None),
            "{id} should be an inert shell"
        );
    }
}

// ------------------------------------------------------------------ analog

#[test]
fn a_forward_biased_diode_conducts_and_a_reversed_one_does_not() {
    let mut board = board(
        &[
            BOARD,
            &part("r1", "litz-resistor", r#"{"value":"220"}"#),
            &part("d1", "litz-diode", r#"{"vf":0.7}"#),
        ],
        &[
            &wire("esp:GPIO8", "r1:1"),
            &wire("r1:2", "d1:A"),
            &wire("d1:C", "esp:GND.1"),
        ],
    );
    board.set_analog(true);

    set(&mut board, 8, Level::High);
    let on = board.analog_solve().expect("the forward circuit solves");
    let amps = on.current("d1").expect("the diode carries current");
    assert!(amps > 1e-3, "forward current was {amps} A");

    set(&mut board, 8, Level::Low);
    let off = board.analog_solve().expect("the reverse circuit solves");
    assert!(
        off.current("d1").unwrap().abs() < 1e-6,
        "a reverse diode blocks"
    );
}

#[test]
fn a_battery_lights_an_led_through_a_series_resistor() {
    let mut board = board(
        &[
            BOARD,
            &part("bat", "litz-battery", r#"{"type":"9V","voltage":5.0}"#),
            &part("r1", "litz-resistor", r#"{"value":"220"}"#),
            &part("led1", "litz-led", r#"{"color":"red"}"#),
        ],
        &[
            &wire("bat:+", "r1:1"),
            &wire("r1:2", "led1:A"),
            &wire("led1:C", "bat:-"),
            &wire("bat:-", "esp:GND.1"),
        ],
    );
    board.set_analog(true);

    let solution = board.analog_solve().expect("the battery circuit solves");
    assert_eq!(solution.is_lit("led1"), Some(true));
    assert!(solution.current("led1").unwrap() > 1e-3);
    // The battery is the only source; the + terminal sits at its voltage.
    assert!((solution.node_voltage("bat", "+").unwrap() - 5.0).abs() < 1e-6);
}

// ------------------------------------------------------------------ I2C LCD

/// The PCF8574 backpack wiring: P0 RS, P1 RW, P2 E, P3 backlight, P4..P7 data.
fn lcd_latch(rs: bool, nibble: u8) -> u8 {
    let mut byte = (nibble & 0x0f) << 4;
    if rs {
        byte |= 0x01;
    }
    byte | 0x08
}

fn lcd_pulse(board: &mut Board, rs: bool, nibble: u8) {
    let latch = lcd_latch(rs, nibble);
    assert!(
        board
            .buses_mut()
            .i2c_mut(0, 0)
            .write(0x27, &[latch | 0x04, latch])
    );
}

fn lcd_byte(board: &mut Board, rs: bool, value: u8) {
    lcd_pulse(board, rs, value >> 4);
    lcd_pulse(board, rs, value & 0x0f);
}

fn lcd_init(board: &mut Board) {
    for nibble in [0x3, 0x3, 0x3, 0x2] {
        lcd_pulse(board, false, nibble);
    }
    lcd_byte(board, false, 0x28); // 4-bit, two lines
    lcd_byte(board, false, 0x0c); // display on
    lcd_byte(board, false, 0x06); // entry increment
    lcd_byte(board, false, 0x01); // clear
}

#[test]
fn an_i2c_lcd_writes_text_through_its_backpack() {
    let lcd = part("lcd", "litz-lcd1602-i2c", "{}");
    let mut board = board(
        &[BOARD, &lcd],
        &[
            &wire("esp:GPIO8", "lcd:SDA"),
            &wire("esp:GPIO9", "lcd:SCL"),
            &wire("esp:3V3", "lcd:VCC"),
            &wire("esp:GND.1", "lcd:GND"),
        ],
    );
    lcd_init(&mut board);
    lcd_byte(&mut board, false, 0x80); // DDRAM address 0
    for ch in b"Hi" {
        lcd_byte(&mut board, true, *ch);
    }

    let model = board
        .buses()
        .i2c
        .get(&(0, 0))
        .and_then(|bus| bus.device_as::<LcdI2c>(0x27))
        .expect("the LCD backpack is on i2c0 at 0x27");
    assert_eq!(model.text(), "Hi");
    assert_eq!((model.columns(), model.rows()), (16, 2));
    assert!(model.display_on());
}

#[test]
fn a_20x4_i2c_lcd_reports_four_rows() {
    let lcd = part("lcd", "litz-lcd2004-i2c", "{}");
    let mut board = board(
        &[BOARD, &lcd],
        &[
            &wire("esp:GPIO8", "lcd:SDA"),
            &wire("esp:GPIO9", "lcd:SCL"),
            &wire("esp:3V3", "lcd:VCC"),
            &wire("esp:GND.1", "lcd:GND"),
        ],
    );
    lcd_init(&mut board);
    for (address, ch) in [(0x00u8, b'A'), (0x40, b'B'), (0x14, b'C'), (0x54, b'D')] {
        lcd_byte(&mut board, false, 0x80 | address);
        lcd_byte(&mut board, true, ch);
    }
    let model = board
        .buses()
        .i2c
        .get(&(0, 0))
        .and_then(|bus| bus.device_as::<LcdI2c>(0x27))
        .expect("the LCD backpack is on i2c0 at 0x27");
    assert_eq!((model.columns(), model.rows()), (20, 4));
    assert_eq!(model.text(), "A\nB\nC\nD");
}

// ------------------------------------------------------------------ RTC

#[test]
fn a_ds1307_answers_at_0x68_with_its_bcd_time() {
    let clock = part("rtc", "litz-ds1307", r#"{"time":"2026-11-20T13:45:30"}"#);
    let mut board = board(
        &[BOARD, &clock],
        &[
            &wire("esp:GPIO8", "rtc:SDA"),
            &wire("esp:GPIO9", "rtc:SCL"),
            &wire("esp:3V3", "rtc:VCC"),
            &wire("esp:GND.1", "rtc:GND"),
        ],
    );
    assert_eq!(
        board.i2c_write_read(0, 0, 0x68, &[0x00], 3),
        Some(vec![0x30, 0x45, 0x13]),
        "seconds, minutes and hours in BCD"
    );
    let model = board
        .buses()
        .i2c
        .get(&(0, 0))
        .and_then(|bus| bus.device_as::<Ds1307>(0x68))
        .expect("the DS1307 is on i2c0");
    let time = model.time();
    assert_eq!((time.year, time.month, time.day), (2026, 11, 20));
    assert_eq!((time.hour, time.minute, time.second), (13, 45, 30));
}

// ------------------------------------------------------------------ SPI RFID

#[test]
fn an_rc522_reports_its_version_register_over_spi() {
    let reader = part("rfid", "litz-rfid-rc522", "{}");
    let mut board = board(
        &[BOARD, &reader],
        &[
            &wire("esp:GPIO7", "rfid:SDA"),
            &wire("esp:GPIO8", "rfid:SCK"),
            &wire("esp:GPIO9", "rfid:MOSI"),
            &wire("esp:GPIO10", "rfid:MISO"),
            &wire("esp:GPIO0", "rfid:RST"),
            &wire("esp:3V3", "rfid:VCC"),
            &wire("esp:GND.1", "rfid:GND"),
        ],
    );
    // 0x37 << 1 | 0x80 is a read of VersionReg; the value follows.
    let rx = board.spi_transfer(0, 0, 7, &[0xee, 0x00], 2);
    assert_eq!(rx[1], 0x92);
    let model = board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<Rc522>(7))
        .expect("the RC522 is on spi0 select GPIO7");
    assert_eq!(model.version(), 0x92);
}

// ------------------------------------------------------------------ UART

fn fingerprint_packet(address: u32, package_id: u8, instruction: u8) -> Vec<u8> {
    // Length counts the instruction, the parameter and the two checksum bytes.
    let mut content = vec![instruction, 0x00];
    let length = (content.len() + 2) as u16;
    let mut packet = vec![0xef, 0x01];
    packet.extend_from_slice(&address.to_be_bytes());
    packet.push(package_id);
    packet.extend_from_slice(&length.to_be_bytes());
    packet.append(&mut content);
    let checksum = packet[6..]
        .iter()
        .fold(0u16, |sum, byte| sum.wrapping_add(u16::from(*byte)));
    packet.extend_from_slice(&checksum.to_be_bytes());
    packet
}

#[test]
fn a_fingerprint_acknowledges_a_command_on_uart() {
    let sensor = part("fp", "litz-fingerprint", "{}");
    let mut board = board(
        &[BOARD, &sensor],
        &[
            &wire("esp:GPIO20", "fp:TX"),
            &wire("esp:GPIO21", "fp:RX"),
            &wire("esp:3V3", "fp:VCC"),
            &wire("esp:GND.1", "fp:GND"),
        ],
    );
    let command = fingerprint_packet(0xffff_ffff, 0x01, 0x01); // GenImg
    assert!(board.uart_receive(0, 0, 0, &command));
    let reply = board.uart_poll(0, 0, 1_000);
    assert_eq!(&reply[..2], &[0xef, 0x01], "the ack starts with the header");
    assert_eq!(reply[6], 0x07, "the ack package id");
    assert_eq!(reply[9], 0x00, "the ack instruction");
    let length = usize::from(u16::from_be_bytes([reply[7], reply[8]]));
    assert_eq!(reply.len(), 9 + length);
}

// ------------------------------------------------------------------ displays

#[test]
fn a_4digit_display_keeps_a_mask_per_multiplexed_digit() {
    // DIG1..DIG4 on GPIO0..3, segments A..G on GPIO4..10.
    let display = part("disp", "litz-7segment-4digit", "{}");
    let mut board = board(
        &[BOARD, &display],
        &[
            &wire("esp:GPIO0", "disp:DIG1"),
            &wire("esp:GPIO1", "disp:DIG2"),
            &wire("esp:GPIO2", "disp:DIG3"),
            &wire("esp:GPIO3", "disp:DIG4"),
            &wire("esp:GPIO4", "disp:A"),
            &wire("esp:GPIO5", "disp:B"),
            &wire("esp:GPIO6", "disp:C"),
            &wire("esp:GPIO7", "disp:D"),
            &wire("esp:GPIO8", "disp:E"),
            &wire("esp:GPIO9", "disp:F"),
            &wire("esp:GPIO10", "disp:G"),
        ],
    );

    // Digit 1 shows "1": segments b and c.
    set(&mut board, 5, Level::High);
    set(&mut board, 6, Level::High);
    set(&mut board, 0, Level::Low);
    assert_eq!(
        board.state("disp"),
        Some(PartState::Segments { mask: 0b0000_0110 })
    );

    // Digit 2 shows "3": segments a, b, c, d and g.
    set(&mut board, 0, Level::High);
    for (gpio, lit) in [
        (4, true),
        (5, true),
        (6, true),
        (7, true),
        (8, false),
        (9, false),
        (10, true),
    ] {
        set(&mut board, gpio, if lit { Level::High } else { Level::Low });
    }
    set(&mut board, 1, Level::Low);
    assert_eq!(
        board.state("disp"),
        Some(PartState::Segments { mask: 0x4f })
    );

    // With digit 1 selected again, changing the segments back to "1" shows
    // that digit's mask; the shared segment bus is what multiplexing shares.
    set(&mut board, 1, Level::High);
    for (gpio, lit) in [
        (4, false),
        (5, true),
        (6, true),
        (7, false),
        (8, false),
        (9, false),
        (10, false),
    ] {
        set(&mut board, gpio, if lit { Level::High } else { Level::Low });
    }
    set(&mut board, 0, Level::Low);
    assert_eq!(
        board.state("disp"),
        Some(PartState::Segments { mask: 0b0000_0110 })
    );
}

#[test]
fn an_led_ring_reuses_the_neopixel_chain() {
    let ring = part("ring", "litz-led-ring", r#"{"pixels":12}"#);
    let mut board = board(
        &[BOARD, &ring],
        &[
            &wire("esp:GPIO0", "ring:DIN"),
            &wire("esp:GPIO1", "ring:DOUT"),
            &wire("esp:3V3", "ring:VDD"),
            &wire("esp:GND.1", "ring:VSS"),
        ],
    );
    // Red is G=0, R=255, B=0 in GRB order.
    let word = 0x00ff00u32;
    let mut at = 1_000u64;
    for bit in 0..24 {
        set_at(&mut board, 0, Level::High, at);
        at += if word & (1 << (23 - bit)) != 0 {
            800
        } else {
            350
        };
        set_at(&mut board, 0, Level::Low, at);
        at += 600;
    }
    let reset = board.next_event().expect("the reset gap is scheduled");
    board.advance_to(reset);
    assert_eq!(board.state("ring"), Some(PartState::Led { lit: true }));
}
