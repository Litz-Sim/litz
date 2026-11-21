//! Batch 2 on a real board: displays, input devices, addressable outputs,
//! sensors, logic chips and the SD card, wired through the catalog to the
//! ESP32-C3 header.

use litz_board::{Board, Epd, FlatBoard, Ili9341, MicroSd};
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

fn set(board: &mut Board, gpio: u8, level: Level, at: u64) {
    board.set_chip_output(gpio, Drive::strong(level), at);
}

fn pin(board: &Board, part: &str, name: &str) -> Option<Level> {
    board.part_pin_level(part, name)
}

fn spi(board: &mut Board, select: u8, tx: &[u8]) -> Vec<u8> {
    board.spi_transfer(0, 0, select, tx, tx.len())
}

// ------------------------------------------------------------------ displays

#[test]
fn an_lcd1602_wires_its_parallel_bus_without_a_conflict() {
    let lcd = part("lcd", "litz-lcd1602", "{}");
    let mut board = board(
        &[BOARD, &lcd],
        &[
            &wire("esp:GPIO0", "lcd:RS"),
            &wire("esp:GPIO1", "lcd:E"),
            &wire("esp:GPIO2", "lcd:D4"),
            &wire("esp:GPIO3", "lcd:D5"),
            &wire("esp:GPIO4", "lcd:D6"),
            &wire("esp:GPIO5", "lcd:D7"),
            &wire("esp:GND.1", "lcd:RW"),
            &wire("esp:3V3", "lcd:VDD"),
            &wire("esp:GND.1", "lcd:VSS"),
        ],
    );
    // The 4-bit wake-up then an entry-mode command.
    for (at, nibble) in [0x3, 0x3, 0x3, 0x2].into_iter().enumerate() {
        set(&mut board, 0, Level::Low, at as u64 * 10);
        for bit in 0..4 {
            set(
                &mut board,
                2 + bit,
                if nibble & (1 << bit) != 0 {
                    Level::High
                } else {
                    Level::Low
                },
                at as u64 * 10,
            );
        }
        set(&mut board, 1, Level::High, at as u64 * 10 + 1);
        set(&mut board, 1, Level::Low, at as u64 * 10 + 2);
    }
    assert!(board.conflicts().is_empty(), "{:?}", board.conflicts());
}

#[test]
fn an_ili9341_draws_a_window_over_spi() {
    let display = part("tft", "litz-ili9341", "{}");
    let mut board = board(
        &[BOARD, &display],
        &[
            &wire("esp:GPIO7", "tft:CS"),
            &wire("esp:GPIO8", "tft:SCK"),
            &wire("esp:GPIO9", "tft:MOSI"),
            &wire("esp:GPIO10", "tft:MISO"),
            &wire("esp:GPIO18", "tft:DC"),
            &wire("esp:GPIO19", "tft:RESET"),
            &wire("esp:3V3", "tft:VCC"),
            &wire("esp:GND.1", "tft:GND"),
        ],
    );
    spi(&mut board, 7, &[0x00, 0x2a, 0x00, 0x05, 0x00, 0x06]);
    spi(&mut board, 7, &[0x00, 0x2b, 0x00, 0x09, 0x00, 0x09]);
    spi(&mut board, 7, &[0x00, 0x2c]);
    spi(&mut board, 7, &[0x40, 0xf8, 0x00, 0x07, 0xe0]);
    let model = board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<Ili9341>(7))
        .expect("the ILI9341 is on spi0 select GPIO7");
    assert_eq!(model.pixel(5, 9), Some(0xf800));
    assert_eq!(model.pixel(6, 9), Some(0x07e0));
}

#[test]
fn an_epaper_panel_refreshes_from_its_spi_frame() {
    let panel = part("paper", "litz-epaper-1in54", "{}");
    let mut board = board(
        &[BOARD, &panel],
        &[
            &wire("esp:GPIO7", "paper:CS"),
            &wire("esp:GPIO8", "paper:CLK"),
            &wire("esp:GPIO9", "paper:DIN"),
            &wire("esp:3V3", "paper:VCC"),
            &wire("esp:GND.1", "paper:GND"),
        ],
    );
    spi(&mut board, 7, &[0x00, 0x10]);
    spi(&mut board, 7, &[0x40, 0x80]);
    spi(&mut board, 7, &[0x00, 0x11, 0x12]);
    let model = board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<Epd>(7))
        .expect("the e-paper is on spi0 select GPIO7");
    assert!(model.pixel(0, 0));
    assert!(!model.pixel(1, 0));
    assert_eq!((model.width(), model.height()), (200, 200));
}

#[test]
fn a_microsd_answers_cmd0_and_cmd8_on_spi() {
    let card = part("card", "litz-microsd-card", "{}");
    let mut board = board(
        &[BOARD, &card],
        &[
            &wire("esp:GPIO7", "card:CS"),
            &wire("esp:GPIO8", "card:SCK"),
            &wire("esp:GPIO9", "card:DI"),
            &wire("esp:GPIO10", "card:DO"),
            &wire("esp:3V3", "card:VCC"),
            &wire("esp:GND.1", "card:GND"),
        ],
    );
    let command = |board: &mut Board, index: u8| {
        let tx = [
            0xff,
            0x40 | index,
            0,
            0,
            0,
            0,
            0x95,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
        ];
        spi(board, 7, &tx)[6..].to_vec()
    };
    assert_eq!(
        command(&mut board, 0)[0],
        0x01,
        "the card enters idle state"
    );
    assert_eq!(
        &command(&mut board, 8)[..5],
        &[0x01, 0x00, 0x00, 0x01, 0xaa]
    );
    let model = board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<MicroSd>(7))
        .expect("the card is on spi0 select GPIO7");
    assert!(model.image().is_empty(), "a card without an image is blank");
}

// ------------------------------------------------------------------ input

#[test]
fn a_dip_switch_bridges_a_closed_position() {
    let dip = part("dip", "litz-dip-switch-8", "{}");
    let mut board = board(
        &[BOARD, &dip],
        &[&wire("esp:GPIO0", "dip:1a"), &wire("esp:GPIO1", "dip:1b")],
    );
    assert!(
        board.set_control("dip", 1, 0),
        "the switch mask is a control"
    );
    set(&mut board, 0, Level::High, 0);
    assert_eq!(pin(&board, "dip", "1b"), Some(Level::High));
}

#[test]
fn a_rotary_encoder_press_and_turn_reach_the_chip() {
    let encoder = part("enc", "litz-rotary-encoder", "{}");
    let mut board = board(
        &[BOARD, &encoder],
        &[
            &wire("esp:GPIO0", "enc:CLK"),
            &wire("esp:GPIO1", "enc:DT"),
            &wire("esp:GPIO2", "enc:SW"),
        ],
    );
    assert!(board.press("enc", true, 0));
    assert_eq!(
        pin(&board, "enc", "SW"),
        Some(Level::Low),
        "SW is active low"
    );
    assert!(board.set_control("enc", 1000, 0));
    assert_eq!(board.state("enc"), Some(PartState::Knob { permille: 1000 }));
}

#[test]
fn an_analog_joystick_centres_at_half_the_rail() {
    let stick = part("stick", "litz-analog-joystick", "{}");
    let mut board = board(
        &[BOARD, &stick],
        &[
            &wire("esp:GPIO0", "stick:VERT"),
            &wire("esp:GPIO1", "stick:HORZ"),
            &wire("esp:GPIO2", "stick:SEL"),
            &wire("esp:3V3", "stick:VCC"),
            &wire("esp:GND.1", "stick:GND"),
        ],
    );
    let volts = |board: &Board| board.analog_inputs();
    let values = volts(&board);
    assert!(
        values
            .iter()
            .any(|&(pin, v)| pin == 0 && (v - 1.65).abs() < 0.01),
        "{values:?}"
    );
    assert!(
        values
            .iter()
            .any(|&(pin, v)| pin == 1 && (v - 1.65).abs() < 0.01),
        "{values:?}"
    );

    assert!(board.set_control("stick", 1000, 0));
    let values = volts(&board);
    assert!(
        values
            .iter()
            .any(|&(pin, v)| pin == 1 && (v - 3.3).abs() < 0.01),
        "{values:?}"
    );
}

#[test]
fn a_membrane_keypad_joins_its_row_and_column() {
    let keypad = part("keys", "litz-membrane-keypad", "{}");
    let mut board = board(
        &[BOARD, &keypad],
        &[&wire("esp:GPIO0", "keys:R1"), &wire("esp:GPIO1", "keys:C1")],
    );
    assert!(board.press("keys", true, 0), "the first key is pressed");
    set(&mut board, 0, Level::Low, 0);
    assert_eq!(pin(&board, "keys", "C1"), Some(Level::Low));
    assert!(board.press("keys", false, 0));
    assert_eq!(pin(&board, "keys", "C1"), Some(Level::HighZ));
}

#[test]
fn a_tilt_switch_closes_when_tilted() {
    let tilt = part("tilt", "litz-tilt-switch", "{}");
    let mut board = board(
        &[BOARD, &tilt],
        &[&wire("esp:GPIO0", "tilt:1"), &wire("esp:GPIO1", "tilt:2")],
    );
    assert_eq!(
        pin(&board, "tilt", "2"),
        Some(Level::HighZ),
        "upright is open"
    );
    assert!(board.set_control("tilt", 800, 0));
    set(&mut board, 0, Level::High, 0);
    assert_eq!(pin(&board, "tilt", "2"), Some(Level::High));
}

#[test]
fn a_slide_potentiometer_sets_its_wiper_voltage() {
    let pot = part("slide", "litz-slide-potentiometer", r#"{"value":"512"}"#);
    let board = board(
        &[BOARD, &pot],
        &[
            &wire("esp:GPIO0", "slide:2"),
            &wire("esp:3V3", "slide:1"),
            &wire("esp:GND.1", "slide:3"),
        ],
    );
    let value = board
        .analog_inputs()
        .into_iter()
        .find(|&(pin, _)| pin == 0)
        .map(|(_, volts)| volts)
        .expect("the wiper sources a voltage");
    assert!((value - 1.65).abs() < 0.02, "{value}");
}

#[test]
fn a_pushbutton_6mm_is_a_momentary_contact() {
    let button = part("b6", "litz-pushbutton-6mm", "{}");
    let mut board = board(
        &[BOARD, &button],
        &[&wire("esp:GPIO0", "b6:1.l"), &wire("esp:GPIO1", "b6:2.l")],
    );
    set(&mut board, 0, Level::High, 0);
    assert_eq!(pin(&board, "b6", "2.l"), Some(Level::HighZ));
    assert!(board.press("b6", true, 0));
    assert_eq!(pin(&board, "b6", "2.l"), Some(Level::High));
}

// ------------------------------------------------------------------ output

#[test]
fn a_neopixel_latches_a_frame_on_the_board_clock() {
    let pixel = part("pixel", "litz-neopixel", "{}");
    let mut board = board(
        &[BOARD, &pixel],
        &[
            &wire("esp:GPIO0", "pixel:DIN"),
            &wire("esp:GPIO1", "pixel:DOUT"),
            &wire("esp:3V3", "pixel:VDD"),
            &wire("esp:GND.1", "pixel:VSS"),
        ],
    );
    // Red is G=0, R=255, B=0 in GRB order.
    let word = 0x00ff00u32;
    let mut at = 1_000u64;
    for bit in 0..24 {
        set(&mut board, 0, Level::High, at);
        at += if word & (1 << (23 - bit)) != 0 {
            800
        } else {
            350
        };
        set(&mut board, 0, Level::Low, at);
        at += 600;
    }
    let reset = board.next_event().expect("the reset gap is scheduled");
    board.advance_to(reset);
    assert_eq!(board.state("pixel"), Some(PartState::Led { lit: true }));
}

#[test]
fn a_stepper_turns_from_the_coil_phase() {
    let motor = part("motor", "litz-stepper-motor", "{}");
    let mut board = board(
        &[BOARD, &motor],
        &[
            &wire("esp:GPIO0", "motor:A+"),
            &wire("esp:GPIO1", "motor:A-"),
            &wire("esp:GPIO2", "motor:B+"),
            &wire("esp:GPIO3", "motor:B-"),
        ],
    );
    let sequence = [
        (true, false, false, false),
        (false, false, true, false),
        (false, true, false, false),
        (false, false, false, true),
    ];
    for (at, (a_plus, a_minus, b_plus, b_minus)) in sequence.into_iter().enumerate() {
        let at = at as u64;
        set(
            &mut board,
            0,
            if a_plus { Level::High } else { Level::Low },
            at,
        );
        set(
            &mut board,
            1,
            if a_minus { Level::High } else { Level::Low },
            at,
        );
        set(
            &mut board,
            2,
            if b_plus { Level::High } else { Level::Low },
            at,
        );
        set(
            &mut board,
            3,
            if b_minus { Level::High } else { Level::Low },
            at,
        );
    }
    // Six half-steps is 15 permille of the 400-half-step revolution.
    assert_eq!(board.state("motor"), Some(PartState::Knob { permille: 15 }));
}

// ------------------------------------------------------------------ sensors

#[test]
fn a_heartbeat_sensor_pulses_on_the_board_clock() {
    let heart = part("heart", "litz-heart-beat-sensor", r#"{"bpm":72}"#);
    let mut board = board(
        &[BOARD, &heart],
        &[
            &wire("esp:GPIO0", "heart:OUT"),
            &wire("esp:3V3", "heart:VCC"),
            &wire("esp:GND.1", "heart:GND"),
        ],
    );
    board.advance_to(0);
    assert_eq!(board.chip_pin_level("heart"), Some(Level::High));
    board.advance_to(100_000_000);
    assert_eq!(board.chip_pin_level("heart"), Some(Level::Low));
    board.advance_to(1_000_000_000);
    let inputs = board.take_chip_inputs();
    assert!(inputs.contains(&(0, Level::High)), "{inputs:?}");
}

#[test]
fn an_hx711_shifts_a_weight_out_on_two_wires() {
    let scale = part("scale", "litz-hx711", r#"{"weight":2500,"capacity":5000}"#);
    let mut board = board(
        &[BOARD, &scale],
        &[
            &wire("esp:GPIO0", "scale:DT"),
            &wire("esp:GPIO1", "scale:SCK"),
            &wire("esp:5V", "scale:VCC"),
            &wire("esp:GND.1", "scale:GND"),
        ],
    );
    assert_eq!(
        pin(&board, "scale", "DT"),
        Some(Level::Low),
        "a conversion is ready"
    );
    let mut raw = 0i32;
    for _ in 0..24 {
        set(&mut board, 1, Level::High, 0);
        if pin(&board, "scale", "DT") == Some(Level::High) {
            raw = raw << 1 | 1;
        } else {
            raw <<= 1;
        }
        set(&mut board, 1, Level::Low, 0);
    }
    assert_eq!(raw, 1050, "half of the 5 kg range is 1050 counts");
}

#[test]
fn a_gps_sends_nmea_on_its_uart() {
    let gps = part("gps", "litz-gps-neo6m", "{}");
    let mut board = board(
        &[BOARD, &gps],
        &[
            &wire("esp:GPIO20", "gps:TX"),
            &wire("esp:GPIO21", "gps:RX"),
            &wire("esp:3V3", "gps:VCC"),
            &wire("esp:GND.1", "gps:GND"),
        ],
    );
    let sentences = String::from_utf8(board.uart_poll(0, 0, 0)).expect("NMEA is ASCII");
    assert!(sentences.contains("$GPGGA,"), "{sentences}");
    assert!(sentences.contains("$GPRMC,"), "{sentences}");
}

#[test]
fn a_flame_and_gas_sensor_source_their_analog_outputs() {
    let flame = part("flame", "litz-flame-sensor", r#"{"intensity":800}"#);
    let gas = part("gas", "litz-gas-sensor", r#"{"ppm":5000}"#);
    let board = board(
        &[BOARD, &flame, &gas],
        &[
            &wire("esp:GPIO0", "flame:AO"),
            &wire("esp:GPIO1", "gas:AOUT"),
            &wire("esp:3V3", "flame:VCC"),
            &wire("esp:GND.1", "flame:GND"),
            &wire("esp:5V", "gas:VCC"),
            &wire("esp:GND.1", "gas:GND"),
        ],
    );
    let values = board.analog_inputs();
    assert!(
        values
            .iter()
            .any(|&(pin, v)| pin == 0 && (v - 2.64).abs() < 0.02),
        "{values:?}"
    );
    assert!(
        values
            .iter()
            .any(|&(pin, v)| pin == 1 && (v - 2.5).abs() < 0.02),
        "{values:?}"
    );
}

// ------------------------------------------------------------------ logic

#[test]
fn a_discrete_and_gate_drives_its_output() {
    let gate = part("g", "litz-and", "{}");
    let mut board = board(
        &[BOARD, &gate],
        &[
            &wire("esp:GPIO0", "g:A"),
            &wire("esp:GPIO1", "g:B"),
            &wire("esp:GPIO2", "g:Y"),
        ],
    );
    assert_eq!(pin(&board, "g", "Y"), Some(Level::HighZ));
    set(&mut board, 0, Level::High, 0);
    set(&mut board, 1, Level::High, 0);
    assert_eq!(pin(&board, "g", "Y"), Some(Level::High));
    set(&mut board, 1, Level::Low, 0);
    assert_eq!(pin(&board, "g", "Y"), Some(Level::Low));
}

#[test]
fn a_74hc00_quad_nand_inverts_a_conjunction() {
    let gate = part("g", "litz-74hc00", "{}");
    let mut board = board(
        &[BOARD, &gate],
        &[
            &wire("esp:GPIO0", "g:1A"),
            &wire("esp:GPIO1", "g:1B"),
            &wire("esp:GPIO2", "g:1Y"),
        ],
    );
    set(&mut board, 0, Level::High, 0);
    set(&mut board, 1, Level::High, 0);
    assert_eq!(pin(&board, "g", "1Y"), Some(Level::Low));
    set(&mut board, 0, Level::Low, 0);
    assert_eq!(pin(&board, "g", "1Y"), Some(Level::High));
}

#[test]
fn a_74hc164_shifts_bits_to_its_outputs() {
    let shift = part("shift", "litz-74hc164", "{}");
    let mut board = board(
        &[BOARD, &shift],
        &[
            &wire("esp:3V3", "shift:A"),
            &wire("esp:3V3", "shift:B"),
            &wire("esp:GPIO2", "shift:CLK"),
            &wire("esp:GPIO3", "shift:CLR"),
            &wire("esp:GPIO4", "shift:Q0"),
            &wire("esp:3V3", "shift:VCC"),
            &wire("esp:GND.1", "shift:GND"),
        ],
    );
    set(&mut board, 3, Level::High, 0);
    assert_eq!(pin(&board, "shift", "Q0"), Some(Level::Low));
    set(&mut board, 2, Level::High, 1);
    assert_eq!(
        pin(&board, "shift", "Q0"),
        Some(Level::High),
        "the first bit lands in Q0"
    );
}

#[test]
fn a_74hc573_is_transparent_then_holds_its_latch() {
    let latch = part("latch", "litz-74hc573", "{}");
    let mut board = board(
        &[BOARD, &latch],
        &[
            &wire("esp:GPIO0", "latch:D0"),
            &wire("esp:GPIO1", "latch:LE"),
            &wire("esp:GPIO2", "latch:OE"),
            &wire("esp:GPIO3", "latch:Q0"),
            &wire("esp:3V3", "latch:VCC"),
            &wire("esp:GND.1", "latch:GND"),
        ],
    );
    set(&mut board, 2, Level::Low, 0);
    set(&mut board, 0, Level::High, 1);
    set(&mut board, 1, Level::High, 2);
    assert_eq!(pin(&board, "latch", "Q0"), Some(Level::High), "transparent");
    set(&mut board, 1, Level::Low, 3);
    set(&mut board, 0, Level::Low, 4);
    assert_eq!(pin(&board, "latch", "Q0"), Some(Level::High), "latched");
}

#[test]
fn a_4017_walks_one_hot_outputs() {
    let counter = part("counter", "litz-4017", "{}");
    let mut board = board(
        &[BOARD, &counter],
        &[
            &wire("esp:GPIO0", "counter:CLK"),
            &wire("esp:GPIO1", "counter:RST"),
            &wire("esp:GPIO2", "counter:CLKINH"),
            &wire("esp:GPIO3", "counter:Q0"),
            &wire("esp:GPIO4", "counter:Q1"),
            &wire("esp:3V3", "counter:VDD"),
            &wire("esp:GND.1", "counter:VSS"),
        ],
    );
    set(&mut board, 1, Level::Low, 0);
    set(&mut board, 2, Level::Low, 0);
    assert_eq!(pin(&board, "counter", "Q0"), Some(Level::High));
    set(&mut board, 0, Level::High, 1);
    set(&mut board, 0, Level::Low, 2);
    assert_eq!(pin(&board, "counter", "Q1"), Some(Level::High));
    assert_eq!(pin(&board, "counter", "Q0"), Some(Level::Low));
}

#[test]
fn a_4051_passes_the_selected_channel() {
    let mux = part("mux", "litz-4051", "{}");
    let mut board = board(
        &[BOARD, &mux],
        &[
            &wire("esp:GPIO0", "mux:A"),
            &wire("esp:GPIO1", "mux:INH"),
            &wire("esp:GPIO2", "mux:X"),
            &wire("esp:GPIO3", "mux:X1"),
            &wire("esp:3V3", "mux:VDD"),
            &wire("esp:GND.1", "mux:VSS"),
        ],
    );
    set(&mut board, 1, Level::Low, 0);
    set(&mut board, 0, Level::High, 1);
    set(&mut board, 3, Level::High, 2);
    assert_eq!(pin(&board, "mux", "X"), Some(Level::High));
}

#[test]
fn a_4094_latches_shifted_bits() {
    let shift = part("shift", "litz-4094", "{}");
    let mut board = board(
        &[BOARD, &shift],
        &[
            &wire("esp:GPIO0", "shift:DATA"),
            &wire("esp:GPIO1", "shift:CLK"),
            &wire("esp:GPIO2", "shift:STROBE"),
            &wire("esp:GPIO3", "shift:OE"),
            &wire("esp:GPIO4", "shift:Q0"),
            &wire("esp:3V3", "shift:VDD"),
            &wire("esp:GND.1", "shift:VSS"),
        ],
    );
    set(&mut board, 3, Level::High, 0);
    set(&mut board, 0, Level::High, 1);
    set(&mut board, 1, Level::High, 2);
    assert_eq!(
        pin(&board, "shift", "Q0"),
        Some(Level::Low),
        "the latch is still empty"
    );
    set(&mut board, 2, Level::High, 3);
    assert_eq!(pin(&board, "shift", "Q0"), Some(Level::High));
}

// ------------------------------------------------------------------ catalog

#[test]
fn every_batch_two_entry_is_in_the_builtin_catalog() {
    let registry = Registry::builtin();
    let ids = [
        "litz-lcd1602",
        "litz-lcd2004",
        "litz-ili9341",
        "litz-epaper-1in54",
        "litz-epaper-2in13",
        "litz-dip-switch-8",
        "litz-rotary-encoder",
        "litz-analog-joystick",
        "litz-membrane-keypad",
        "litz-pushbutton-6mm",
        "litz-slide-potentiometer",
        "litz-tilt-switch",
        "litz-neopixel",
        "litz-neopixel-matrix",
        "litz-stepper-motor",
        "litz-ir-receiver",
        "litz-flame-sensor",
        "litz-gas-sensor",
        "litz-heart-beat-sensor",
        "litz-hx711",
        "litz-gps-neo6m",
        "litz-74hc164",
        "litz-74hc166",
        "litz-74hc573",
        "litz-4017",
        "litz-4051",
        "litz-4094",
        "litz-74hc00",
        "litz-74hc02",
        "litz-74hc04",
        "litz-74hc08",
        "litz-74hc14",
        "litz-74hc32",
        "litz-74hc86",
        "litz-and",
        "litz-nand",
        "litz-or",
        "litz-nor",
        "litz-xor",
        "litz-xnor",
        "litz-not",
        "litz-buffer",
        "litz-microsd-card",
    ];
    for id in ids {
        assert!(
            registry.get(id).is_some(),
            "{id} is missing from the builtin catalog"
        );
    }
}

#[test]
fn a_pm_sensor_measures_on_its_uart_interval() {
    let pm = part("pm", "litz-pms5003", r#"{"pm25":35,"interval":1000}"#);
    let mut board = board(
        &[BOARD, &pm],
        &[
            &wire("esp:GPIO20", "pm:TX"),
            &wire("esp:GPIO21", "pm:RX"),
            &wire("esp:3V3", "pm:VCC"),
            &wire("esp:GND.1", "pm:GND"),
        ],
    );
    assert!(
        board.uart_poll(0, 0, 0).is_empty(),
        "the first measurement is one interval away"
    );
    let frame = board.uart_poll(0, 0, 1_000_000_000);
    assert_eq!(frame.len(), 32, "PMS5003 frame");
    assert_eq!(&frame[..4], &[0x42, 0x4d, 0x00, 0x1c]);
    assert!(
        frame.windows(2).any(|pair| pair == [0x00, 35]),
        "the PM2.5 value is in the payload"
    );
    let checksum: u32 = frame[..30].iter().map(|byte| u32::from(*byte)).sum();
    assert_eq!(
        u16::from_be_bytes([frame[30], frame[31]]),
        checksum as u16,
        "the frame's checksum"
    );
    assert!(
        board.uart_poll(0, 0, 1_500_000_000).is_empty(),
        "half an interval later there is no second frame"
    );
}
