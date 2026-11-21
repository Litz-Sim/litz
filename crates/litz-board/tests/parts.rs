//! The expanded builtin catalog on a real board: a manifest puts each part on
//! pins or a bus, the diagram wires it, and the runtime reports what it does.

use litz_board::{Board, DateTime, Ds3231, FlatBoard, Mcp3008, Mpu6050, Pcf8574, Ssd1306};
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

#[test]
fn an_ssd1306_on_i2c_accepts_a_page_write_and_reports_pixels() {
    let oled = part("oled", "litz-ssd1306", "{}");
    let mut board = board(
        &[BOARD, &oled],
        &[
            &wire("esp:GPIO8", "oled:SDA"),
            &wire("esp:GPIO9", "oled:SCL"),
            &wire("esp:3V3", "oled:VCC"),
            &wire("esp:GND.1", "oled:GND"),
        ],
    );

    // 0x00 is the command stream: page 0, column 0. 0x40 is the data stream.
    assert!(
        board
            .buses_mut()
            .i2c_mut(0, 0)
            .write(0x3c, &[0x00, 0xb0, 0x00, 0x10])
    );
    assert!(board.buses_mut().i2c_mut(0, 0).write(0x3c, &[0x40, 0xaa]));

    let model = board
        .buses()
        .i2c
        .get(&(0, 0))
        .and_then(|bus| bus.device_as::<Ssd1306>(0x3c))
        .expect("the SSD1306 model is on i2c0");
    assert_eq!(model.framebuffer_byte(0, 0), Some(0xaa));
    assert!(model.pixel(0, 1));
    assert_eq!(model.lit_pixels(), 4);
}

#[test]
fn a_ds3231_answers_at_0x68_with_its_bcd_time() {
    let clock = part("rtc", "litz-ds3231", r#"{"time":"2026-11-20T13:45:30"}"#);
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
        .and_then(|bus| bus.device_as::<Ds3231>(0x68))
        .expect("the DS3231 model is on i2c0");
    assert_eq!(
        model.time(),
        DateTime {
            year: 2026,
            month: 11,
            day: 20,
            weekday: 1,
            hour: 13,
            minute: 45,
            second: 30
        }
    );
}

#[test]
fn a_pcf8574_latches_a_byte_on_i2c() {
    let expander = part("port", "litz-pcf8574", "{}");
    let mut board = board(
        &[BOARD, &expander],
        &[
            &wire("esp:GPIO8", "port:SDA"),
            &wire("esp:GPIO9", "port:SCL"),
            &wire("esp:3V3", "port:VCC"),
            &wire("esp:GND.1", "port:GND"),
        ],
    );
    assert!(board.buses_mut().i2c_mut(0, 0).write(0x20, &[0x5a]));
    assert_eq!(board.i2c_write_read(0, 0, 0x20, &[], 1), Some(vec![0x5a]));
    let model = board
        .buses()
        .i2c
        .get(&(0, 0))
        .and_then(|bus| bus.device_as::<Pcf8574>(0x20))
        .expect("the PCF8574 model is on i2c0");
    assert_eq!(model.latched(), 0x5a);
}

#[test]
fn an_mpu6050_answers_who_am_i_on_i2c() {
    let imu = part("imu", "litz-mpu6050", r#"{"az":1.0,"gz":90.0}"#);
    let mut board = board(
        &[BOARD, &imu],
        &[
            &wire("esp:GPIO8", "imu:SDA"),
            &wire("esp:GPIO9", "imu:SCL"),
            &wire("esp:3V3", "imu:VCC"),
            &wire("esp:GND.1", "imu:GND"),
        ],
    );
    assert_eq!(
        board.i2c_write_read(0, 0, 0x68, &[0x75], 1),
        Some(vec![0x68])
    );
    let model = board
        .buses()
        .i2c
        .get(&(0, 0))
        .and_then(|bus| bus.device_as::<Mpu6050>(0x68))
        .expect("the MPU6050 model is on i2c0");
    assert!((model.accel_g()[2] - 1.0).abs() < 0.001);
    assert!((model.gyro_dps()[2] - 90.0).abs() < 0.5);
}

#[test]
fn an_mcp3008_reads_the_selected_channel_over_spi() {
    let adc = part("adc", "litz-mcp3008", r#"{"v1":1.65}"#);
    let mut board = board(
        &[BOARD, &adc],
        &[
            &wire("esp:GPIO7", "adc:CS"),
            &wire("esp:GPIO8", "adc:CLK"),
            &wire("esp:GPIO9", "adc:DOUT"),
            &wire("esp:GPIO10", "adc:DIN"),
            &wire("esp:3V3", "adc:VDD"),
            &wire("esp:3V3", "adc:VREF"),
            &wire("esp:GND.1", "adc:DGND"),
        ],
    );
    let rx = board.spi_transfer(0, 0, 7, &[0x01, 0x90, 0x00], 3);
    let raw = (u16::from(rx[1] & 0x03) << 8) | u16::from(rx[2]);
    assert_eq!(raw, 512, "channel 1 sits at half of the 3.3 V reference");
    let model = board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<Mcp3008>(7))
        .expect("the MCP3008 model is on spi0");
    assert_eq!(model.selected_channel(), 1);
}

#[test]
fn a_dht22_takes_its_temperature_from_a_control() {
    let sensor = part("dht", "litz-dht22", r#"{"temperature":25,"humidity":50}"#);
    let mut board = board(
        &[BOARD, &sensor],
        &[
            &wire("esp:GPIO0", "dht:SDA"),
            &wire("esp:3V3", "dht:VCC"),
            &wire("esp:GND.1", "dht:GND"),
        ],
    );
    assert_eq!(board.state("dht"), Some(PartState::Knob { permille: 541 }));
    assert!(board.set_control("dht", 1000, 0));
    assert_eq!(board.state("dht"), Some(PartState::Knob { permille: 1000 }));
    assert!(board.set_control("dht", 0, 0));
    assert_eq!(board.state("dht"), Some(PartState::Knob { permille: 0 }));
}

#[test]
fn an_hc_sr04_takes_its_distance_from_a_control() {
    let sensor = part("range", "litz-hc-sr04", "{}");
    let mut board = board(
        &[BOARD, &sensor],
        &[
            &wire("esp:GPIO0", "range:TRIG"),
            &wire("esp:GPIO1", "range:ECHO"),
            &wire("esp:3V3", "range:VCC"),
            &wire("esp:GND.1", "range:GND"),
        ],
    );
    assert_eq!(
        board.state("range"),
        Some(PartState::Knob { permille: 246 })
    );
    assert!(board.set_control("range", 1000, 0));
    assert_eq!(
        board.state("range"),
        Some(PartState::Knob { permille: 1000 })
    );
}

#[test]
fn a_pir_sensor_drives_its_output_to_the_chip() {
    let pir = part("pir", "litz-pir-motion-sensor", "{}");
    let mut board = board(
        &[BOARD, &pir],
        &[
            &wire("esp:GPIO1", "pir:OUT"),
            &wire("esp:3V3", "pir:VCC"),
            &wire("esp:GND.1", "pir:GND"),
        ],
    );
    assert_eq!(board.chip_pin_level("pir"), Some(Level::Low));
    assert!(board.set_control("pir", 900, 0));
    assert_eq!(board.chip_pin_level("pir"), Some(Level::High));
    assert_eq!(board.state("pir"), Some(PartState::Switch { on: true }));
}

#[test]
fn a_photoresistor_sets_the_analog_voltage() {
    let ldr = part("ldr", "litz-photoresistor-sensor", r#"{"light":500}"#);
    let mut board = board(
        &[BOARD, &ldr],
        &[
            &wire("esp:GPIO0", "ldr:AO"),
            &wire("esp:GPIO1", "ldr:DO"),
            &wire("esp:3V3", "ldr:VCC"),
            &wire("esp:GND.1", "ldr:GND"),
        ],
    );
    let volts = |board: &Board| {
        board
            .analog_inputs()
            .into_iter()
            .find(|&(pin, _)| pin == 0)
            .map(|(_, volts)| volts)
    };
    let value = volts(&board).expect("the light sensor sources a voltage");
    assert!((value - 1.65).abs() < 0.01, "{value}");
    assert_eq!(board.part_pin_level("ldr", "DO"), Some(Level::High));

    assert!(board.set_control("ldr", 100, 0));
    let value = volts(&board).expect("the light sensor sources a voltage");
    assert!((value - 0.33).abs() < 0.01, "{value}");
}

#[test]
fn an_ntc_sensor_sets_the_analog_voltage() {
    let ntc = part(
        "ntc",
        "litz-ntc-temperature-sensor",
        r#"{"temperature":25}"#,
    );
    let mut board = board(
        &[BOARD, &ntc],
        &[
            &wire("esp:GPIO0", "ntc:AO"),
            &wire("esp:GPIO1", "ntc:DO"),
            &wire("esp:3V3", "ntc:VCC"),
            &wire("esp:GND.1", "ntc:GND"),
        ],
    );
    let volts = |board: &Board| {
        board
            .analog_inputs()
            .into_iter()
            .find(|&(pin, _)| pin == 0)
            .map(|(_, volts)| volts)
    };
    let value = volts(&board).expect("the thermistor sources a voltage");
    assert!((value - 1.2375).abs() < 0.01, "{value}");

    assert!(board.set_control("ntc", 1000, 0));
    let value = volts(&board).expect("the thermistor sources a voltage");
    assert!((value - 3.3).abs() < 0.01, "{value}");
    assert_eq!(board.state("ntc"), Some(PartState::Knob { permille: 1000 }));
}

#[test]
fn an_rgb_led_lights_when_a_channel_is_driven() {
    let led = part("rgb", "litz-rgb-led", "{}");
    let mut board = board(
        &[BOARD, &led],
        &[
            &wire("esp:GPIO0", "rgb:R"),
            &wire("esp:GPIO1", "rgb:G"),
            &wire("esp:GPIO2", "rgb:B"),
            &wire("esp:GND.1", "rgb:COM"),
        ],
    );
    assert_eq!(board.state("rgb"), Some(PartState::Led { lit: false }));
    set(&mut board, 2, Level::High);
    assert_eq!(board.state("rgb"), Some(PartState::Led { lit: true }));
}

#[test]
fn a_led_bar_graph_counts_the_lit_segments() {
    let bar = part("bar", "litz-led-bar-graph", "{}");
    let mut board = board(
        &[BOARD, &bar],
        &[
            &wire("esp:GPIO0", "bar:A1"),
            &wire("esp:GPIO1", "bar:A2"),
            &wire("esp:GPIO2", "bar:A3"),
            &wire("esp:GND.1", "bar:C"),
        ],
    );
    assert_eq!(board.state("bar"), Some(PartState::Knob { permille: 0 }));
    set(&mut board, 0, Level::High);
    set(&mut board, 2, Level::High);
    assert_eq!(board.state("bar"), Some(PartState::Knob { permille: 200 }));
}

#[test]
fn a_relay_switches_the_common_between_nc_and_no() {
    let relay = part("relay", "litz-relay", "{}");
    let mut board = board(
        &[BOARD, &relay],
        &[
            &wire("esp:GPIO0", "relay:IN"),
            &wire("esp:GPIO1", "relay:COM"),
            &wire("esp:3V3", "relay:NC"),
            &wire("esp:GND.1", "relay:NO"),
            &wire("esp:3V3", "relay:VCC"),
            &wire("esp:GND.1", "relay:GND"),
        ],
    );
    assert_eq!(
        board.part_pin_level("relay", "COM"),
        Some(Level::High),
        "COM rests on NC"
    );
    assert_eq!(board.state("relay"), Some(PartState::Switch { on: false }));

    set(&mut board, 0, Level::High);
    assert_eq!(
        board.part_pin_level("relay", "COM"),
        Some(Level::Low),
        "COM follows NO"
    );
    assert_eq!(board.state("relay"), Some(PartState::Switch { on: true }));
}

#[test]
fn an_l293d_drives_its_motor_outputs() {
    let driver = part("motor", "litz-l293d", "{}");
    let mut board = board(
        &[BOARD, &driver],
        &[
            &wire("esp:GPIO0", "motor:IN1"),
            &wire("esp:GPIO1", "motor:IN2"),
            &wire("esp:GPIO2", "motor:EN"),
            &wire("esp:GPIO3", "motor:OUT1"),
            &wire("esp:GPIO4", "motor:OUT2"),
            &wire("esp:3V3", "motor:VCC1"),
            &wire("esp:5V", "motor:VCC2"),
            &wire("esp:GND.1", "motor:GND"),
        ],
    );
    set(&mut board, 2, Level::High);
    set(&mut board, 0, Level::High);
    assert_eq!(board.part_pin_level("motor", "OUT1"), Some(Level::High));
    assert_eq!(board.part_pin_level("motor", "OUT2"), Some(Level::Low));
    assert_eq!(board.state("motor"), Some(PartState::Switch { on: true }));

    set(&mut board, 0, Level::Low);
    assert_eq!(board.part_pin_level("motor", "OUT1"), Some(Level::Low));
    assert_eq!(board.part_pin_level("motor", "OUT2"), Some(Level::Low));
}

#[test]
fn a_74hc165_shifts_its_parallel_byte_onto_q7() {
    let shift = part("shift", "litz-74hc165", "{}");
    let mut board = board(
        &[BOARD, &shift],
        &[
            &wire("esp:3V3", "shift:D0"),
            &wire("esp:3V3", "shift:D7"),
            &wire("esp:GPIO2", "shift:PL"),
            &wire("esp:GPIO3", "shift:CP"),
            &wire("esp:GPIO4", "shift:CE"),
            &wire("esp:GPIO5", "shift:Q7"),
            &wire("esp:GND.1", "shift:DS"),
            &wire("esp:3V3", "shift:VCC"),
            &wire("esp:GND.1", "shift:GND"),
        ],
    );
    set(&mut board, 2, Level::Low);
    set(&mut board, 2, Level::High);
    set(&mut board, 4, Level::Low);
    assert_eq!(
        board.part_pin_level("shift", "Q7"),
        Some(Level::High),
        "D7 loads into Q7"
    );

    set(&mut board, 3, Level::High);
    assert_eq!(
        board.part_pin_level("shift", "Q7"),
        Some(Level::Low),
        "D6 shifts out"
    );
}

#[test]
fn a_74hc138_selects_one_active_low_output() {
    let decoder = part("decoder", "litz-74hc138", "{}");
    let mut board = board(
        &[BOARD, &decoder],
        &[
            &wire("esp:GPIO0", "decoder:A0"),
            &wire("esp:GPIO1", "decoder:A1"),
            &wire("esp:GPIO2", "decoder:A2"),
            &wire("esp:GPIO3", "decoder:E1"),
            &wire("esp:GPIO4", "decoder:E2"),
            &wire("esp:GPIO5", "decoder:E3"),
            &wire("esp:GPIO6", "decoder:Y5"),
            &wire("esp:GPIO7", "decoder:Y0"),
            &wire("esp:3V3", "decoder:VCC"),
            &wire("esp:GND.1", "decoder:GND"),
        ],
    );
    set(&mut board, 3, Level::Low);
    set(&mut board, 4, Level::Low);
    set(&mut board, 5, Level::High);
    set(&mut board, 0, Level::High);
    set(&mut board, 2, Level::High);
    assert_eq!(board.part_pin_level("decoder", "Y5"), Some(Level::Low));
    assert_eq!(board.part_pin_level("decoder", "Y0"), Some(Level::High));

    set(&mut board, 5, Level::Low);
    assert_eq!(
        board.part_pin_level("decoder", "Y5"),
        Some(Level::High),
        "disabled asserts none"
    );
}
