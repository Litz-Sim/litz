//! Analog co-simulation over real board circuits.
//!
//! Each test is named for the circuit it builds: a divider, a GPIO driving an
//! LED, a pot on an ADC pin, a floating node, and the analog-off path that must
//! stay exactly as it was.

use litz_board::flatten::{Endpoint, Engine, FlatBuses, FlatPart, Rail};
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level, PartState};
use litz_diagram::{Circuit, PinKind};
use std::collections::BTreeMap;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;

fn board(parts: &[&str], connections: &[&str]) -> Board {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        r#"]}"#,
    ]
    .concat();
    let circuit = Circuit::from_json(&json).unwrap();
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap()
}

fn adc(board: &Board, gpio: u8) -> Option<f32> {
    board
        .analog_inputs()
        .into_iter()
        .find(|&(pin, _)| pin == gpio)
        .map(|(_, volts)| volts)
}

#[test]
fn a_two_resistor_divider_solves_to_its_midpoint() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"r2","type":"litz-resistor","attrs":{"value":"10k"}}"#,
        ],
        &[
            r#"["esp:3V3","r1:1","",[]]"#,
            r#"["r1:2","esp:GPIO0","",[]]"#,
            r#"["esp:GPIO0","r2:1","",[]]"#,
            r#"["r2:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let solution = board.analog_solve().expect("divider solves");
    let midpoint = solution
        .node_voltage_gpio(0)
        .expect("the tapped net is solved");
    assert!((midpoint - 1.65).abs() < 1e-6, "midpoint was {midpoint}");
    assert!((solution.node_voltage("r1", "2").unwrap() - 1.65).abs() < 1e-6);
    assert!((solution.node_voltage("r2", "1").unwrap() - 1.65).abs() < 1e-6);
    assert_eq!(solution.node_voltage("r1", "1").unwrap(), 3.3);
    assert_eq!(solution.node_voltage("r2", "2").unwrap(), 0.0);
}

#[test]
fn a_bench_supply_holds_its_voltage_and_reads_out_its_current() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"pwr","type":"litz-power-supply","attrs":{"voltage":5.0}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"100"}}"#,
        ],
        &[
            r#"["pwr:+","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
            r#"["pwr:-","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let solution = board.analog_solve().expect("the supply circuit solves");
    assert_eq!(solution.node_voltage("pwr", "+"), Some(5.0));
    let delivered = -solution
        .current("pwr")
        .expect("the supply's current is readable");
    assert!(
        (delivered - 0.05).abs() < 1e-6,
        "the supply delivered {delivered} A"
    );
}

#[test]
fn a_supply_past_its_current_limit_lets_its_voltage_sag() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"pwr","type":"litz-power-supply","attrs":{"voltage":5.0,"current_limit":0.01}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"100"}}"#,
        ],
        &[
            r#"["pwr:+","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
            r#"["pwr:-","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let solution = board.analog_solve().expect("the limited circuit solves");
    // The load would draw 50 mA at 5 V; the supply holds 10 mA instead, so
    // 100 Ω carries 10 mA and the terminal sits at 1 V.
    let positive = solution.node_voltage("pwr", "+").expect("the rail solves");
    assert!((positive - 1.0).abs() < 1e-6, "terminal was {positive} V");
    let delivered = -solution
        .current("pwr")
        .expect("the supply's current is readable");
    assert!(
        (delivered - 0.01).abs() < 1e-6,
        "the supply delivered {delivered} A"
    );
}

#[test]
fn a_bench_supply_follows_its_host_controls() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"pwr","type":"litz-power-supply","attrs":{"voltage":5.0}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"100"}}"#,
        ],
        &[
            r#"["pwr:+","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
            r#"["pwr:-","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    assert!(board.set_named_control("pwr", "voltage", "3.3", 0));
    let solution = board.analog_solve().expect("the circuit solves at 3.3 V");
    assert!((solution.node_voltage("pwr", "+").unwrap() - 3.3).abs() < 1e-6);

    // 3.3 V into 100 Ω is 33 mA; a 10 mA limit pulls the terminal to 1 V.
    assert!(board.set_named_control("pwr", "current", "0.01", 0));
    let solution = board.analog_solve().expect("the limited circuit solves");
    assert!((solution.node_voltage("pwr", "+").unwrap() - 1.0).abs() < 1e-6);

    assert!(board.set_named_control("pwr", "current", "0", 0));
    let solution = board.analog_solve().expect("the limit is off again");
    assert!((solution.node_voltage("pwr", "+").unwrap() - 3.3).abs() < 1e-6);

    assert!(!board.set_named_control("pwr", "temperature", "20", 0));
}

#[test]
fn a_gpio_driving_a_resistor_and_led_sets_its_current_and_lights_it() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"220"}}"#,
            r#"{"id":"led1","type":"litz-led","attrs":{"color":"red"}}"#,
        ],
        &[
            r#"["esp:GPIO8","r1:1","",[]]"#,
            r#"["r1:2","led1:A","",[]]"#,
            r#"["led1:C","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    board.set_chip_output(8, Drive::strong(Level::High), 0);
    let solution = board.analog_solve().expect("the LED circuit solves");
    let amps = solution.current("led1").expect("the LED carries current");
    // 3.3 V through 50 Ω (driver), 220 Ω (series) and the junction.
    assert!((0.0045..0.0065).contains(&amps), "LED current was {amps} A");
    assert_eq!(solution.is_lit("led1"), Some(true));
    assert_eq!(board.state("led1"), Some(PartState::Led { lit: true }));

    board.set_chip_output(8, Drive::strong(Level::Low), 0);
    let solution = board.analog_solve().expect("the low drive solves");
    assert!(solution.current("led1").unwrap().abs() < 1e-9);
    assert_eq!(solution.is_lit("led1"), Some(false));
    assert_eq!(board.state("led1"), Some(PartState::Led { lit: false }));
}

#[test]
fn a_potentiometer_wiper_tracks_set_control_and_reaches_the_adc() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"pot1","type":"litz-potentiometer","attrs":{"value":"50"}}"#,
        ],
        &[
            r#"["esp:3V3","pot1:1","",[]]"#,
            r#"["pot1:2","esp:GPIO0","",[]]"#,
            r#"["pot1:3","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    // Half travel: the wiper is the 3.3 V / 2 divider point.
    let volts = adc(&board, 0).expect("the wiper reaches the ADC");
    assert!((volts - 1.65).abs() < 0.01, "wiper was {volts}");
    let solution = board.analog_solve().unwrap();
    assert!((solution.node_voltage_gpio(0).unwrap() - 1.65).abs() < 0.01);
    assert!((solution.node_voltage("pot1", "2").unwrap() - 1.65).abs() < 0.01);

    // Travel runs from pin 1 to pin 3: 0 parks the wiper on 3V3.
    assert!(board.set_control("pot1", 0, 0));
    let volts = adc(&board, 0).expect("still solved");
    assert!((volts - 3.3).abs() < 0.01, "wiper was {volts}");
    assert!(
        (board
            .analog_solve()
            .unwrap()
            .node_voltage("pot1", "2")
            .unwrap()
            - 3.3)
            .abs()
            < 0.01
    );

    assert!(board.set_control("pot1", 1000, 0));
    let volts = adc(&board, 0).expect("still solved");
    assert!(volts.abs() < 0.01, "wiper was {volts}");

    assert!(board.set_control("pot1", 250, 0));
    let volts = adc(&board, 0).expect("still solved");
    assert!((volts - 2.475).abs() < 0.01, "wiper was {volts}");
}

#[test]
fn a_floating_node_is_high_z_not_zero_volts() {
    // One end of the resistor reaches a GPIO that is an input; the other end
    // reaches nothing. There is no path to a rail, so the node has no voltage
    // at all — the ADC must see a floating pad, not 0 V.
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10k"}}"#,
        ],
        &[r#"["esp:GPIO9","r1:1","",[]]"#],
    );
    board.set_analog(true);

    let solution = board
        .analog_solve()
        .expect("a floating node does not fail the solve");
    assert_eq!(solution.node_voltage_gpio(9), None);
    assert_eq!(solution.node_voltage("r1", "1"), None);
    assert_eq!(solution.node_is_high_z("r1", "1"), Some(true));

    // The digital view agrees: an input with nothing driving it is HighZ.
    assert!(
        board
            .take_chip_inputs()
            .iter()
            .any(|&(pin, level)| pin == 9 && level == Level::HighZ)
    );
}

/// Analog is opt-in. With it off, the digital runtime and the sampled analog
/// path are the code that ran before co-simulation existed: exact levels,
/// exact states, and the potentiometer's static `analog()` value.
#[test]
fn analog_off_keeps_the_digital_results_unchanged() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"pot1","type":"litz-potentiometer","attrs":{"value":"50"}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"220"}}"#,
            r#"{"id":"led1","type":"litz-led","attrs":{"color":"green"}}"#,
        ],
        &[
            r#"["esp:3V3","pot1:1","",[]]"#,
            r#"["pot1:2","esp:GPIO0","",[]]"#,
            r#"["pot1:3","esp:GND.1","",[]]"#,
            r#"["esp:GPIO8","r1:1","",[]]"#,
            r#"["r1:2","led1:A","",[]]"#,
            r#"["led1:C","esp:GND.1","",[]]"#,
        ],
    );
    assert!(!board.analog_enabled(), "analog is off by default");

    // The static wiper value, computed in f32 exactly as before.
    assert_eq!(board.analog_inputs(), vec![(0, 1.65_f32)]);
    assert_eq!(board.state("pot1"), Some(PartState::Knob { permille: 500 }));

    board.set_chip_output(8, Drive::strong(Level::High), 0);
    assert_eq!(board.state("led1"), Some(PartState::Led { lit: true }));
    assert_eq!(board.part_pin_level("led1", "A"), Some(Level::High));
    assert_eq!(board.part_pin_level("led1", "C"), Some(Level::Low));

    // Solving explicitly is observational: it must not perturb any digital
    // result, and the sampled value must still be the static one.
    let solution = board.analog_solve().unwrap();
    assert_eq!(
        solution.node_voltage_gpio(0).map(|v| (v * 1000.0).round()),
        Some(1650.0)
    );

    assert_eq!(board.state("led1"), Some(PartState::Led { lit: true }));
    assert_eq!(board.analog_inputs(), vec![(0, 1.65_f32)]);
    assert_eq!(board.part_pin_level("pot1", "2"), Some(Level::HighZ));
    assert!(board.conflicts().is_empty());
}
// ---------------------------------------------------------------- transient

/// Build one leaf part for a synthetic board.
///
/// The analog views in these tests do not need a catalog manifest, only the
/// engine name, the pin names and the attributes. This also pins the names the
/// analog mapper reads (`capacitor`/`inductor` with `value`, `bjt` with
/// `beta`/`vbe`, and so on) without waiting for the catalog to gain a part.
fn synth_part(
    id: &str,
    engine: &str,
    pins: &[(&str, PinKind)],
    attrs: &[(&str, &str)],
) -> FlatPart {
    FlatPart {
        id: id.to_string(),
        type_key: format!("litz-{engine}"),
        engine: Engine::Builtin(engine.to_string()),
        pins: pins.iter().map(|(name, _)| (*name).to_string()).collect(),
        pin_kinds: pins.iter().map(|(_, kind)| *kind).collect(),
        attrs: attrs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        buses: Vec::new(),
        device: None,
    }
}

fn endpoint(spec: &str, parts: &[FlatPart]) -> Endpoint {
    match spec {
        "$gnd" => Endpoint::Rail(Rail::Gnd),
        "$v33" => Endpoint::Rail(Rail::V33),
        "$v5" => Endpoint::Rail(Rail::V5),
        "$vin" => Endpoint::Rail(Rail::Vin),
        _ if spec.starts_with("gpio") => Endpoint::Chip(spec[4..].parse().unwrap()),
        _ => {
            let (id, name) = spec.split_once(':').unwrap();
            let part = parts.iter().position(|part| part.id == id).unwrap() as u16;
            let pin = parts[part as usize]
                .pins
                .iter()
                .position(|pin| pin == name)
                .unwrap() as u16;
            Endpoint::Part { part, pin }
        }
    }
}

/// Assemble a flat board from parts and explicit nets. Each net is a list of
/// `part:pin`, `gpioN` or `$rail` endpoints.
fn synthetic(parts: Vec<FlatPart>, nets: &[&[&str]]) -> Board {
    let nets: Vec<Vec<Endpoint>> = nets
        .iter()
        .map(|net| net.iter().map(|spec| endpoint(spec, &parts)).collect())
        .collect();
    let mut endpoint_net = BTreeMap::new();
    for (index, members) in nets.iter().enumerate() {
        for endpoint in members {
            endpoint_net.insert(*endpoint, index as u16);
        }
    }
    let chip_pins = endpoint_net
        .keys()
        .filter_map(|endpoint| match endpoint {
            Endpoint::Chip(gpio) => Some(*gpio),
            _ => None,
        })
        .collect();
    let flat = FlatBoard {
        parts,
        nets,
        endpoint_net,
        chip_pins,
        buses: FlatBuses::default(),
        boards: Vec::new(),
        board_ids: Vec::new(),
        chip_pins_of: BTreeMap::new(),
        board_power: Vec::new(),
    };
    Board::new(flat).unwrap()
}

/// GPIO4 -> 9.95 kΩ -> node -> 100 nF -> ground. With the 50 Ω driver that is
/// `tau = 10 kΩ · 100 nF = 1 ms`.
fn rc_board() -> Board {
    synthetic(
        vec![
            synth_part(
                "r1",
                "resistor",
                &[("1", PinKind::Passive), ("2", PinKind::Passive)],
                &[("value", "9.95k")],
            ),
            synth_part(
                "c1",
                "capacitor",
                &[("1", PinKind::Passive), ("2", PinKind::Passive)],
                &[("value", "100n")],
            ),
        ],
        &[
            &["gpio4", "r1:1"],
            &["r1:2", "c1:1", "gpio0"],
            &["c1:2", "$gnd"],
        ],
    )
}

#[test]
fn an_rc_low_pass_follows_the_input_step() {
    let mut board = rc_board();
    board.set_analog(true);
    board.set_chip_output(4, Drive::strong(Level::High), 0);

    // tau = 1 ms, so 10 µs is one hundredth of a time constant: backward Euler
    // is within 0.3% of the analytic 63.2% at one tau.
    let dt_ns = 10_000;
    let steps = 100;
    let first = board
        .analog_transient(dt_ns)
        .expect("the first RC step solves")
        .node_voltage_gpio(0)
        .expect("solved");
    for _ in 1..steps {
        board.analog_transient(dt_ns).expect("the RC step solves");
    }
    assert!(board.analog_transient_active());
    assert_eq!(board.analog_time(), Some(steps * dt_ns));

    let at_tau = board
        .node_voltage_gpio(0)
        .expect("the capacitor node is solved");
    let expected = 3.3 * (1.0 - (-1.0f64).exp());
    let error = (at_tau - expected).abs() / expected;
    assert!(
        error < 0.02,
        "RC output at tau was {at_tau:.6} V, expected {expected:.6} V ({:.2}% error)",
        error * 100.0
    );
    // The response is a charge curve, not an instant jump.
    assert!(first > 0.0 && first < at_tau && at_tau < 3.3);
}

#[test]
fn a_capacitor_holds_its_voltage_between_steps() {
    let mut board = rc_board();
    board.set_analog(true);
    board.set_chip_output(4, Drive::strong(Level::High), 0);

    let dt_ns = 10_000;
    for _ in 0..steps_to_tau() {
        board.analog_transient(dt_ns).unwrap();
    }
    let before = board.node_voltage_gpio(0).expect("solved");

    // A further step continues the same run: the state is not reset to zero
    // and the node does not jump; it only creeps along the curve.
    board.analog_transient(dt_ns).unwrap();
    let after = board.node_voltage_gpio(0).expect("solved");

    assert!(
        after > before,
        "the capacitor kept charging: {before} -> {after}"
    );
    assert!(
        (after - before).abs() < 0.01 * before,
        "between steps the capacitor held its charge: {before} -> {after}"
    );
    assert!(after < 3.3);
}

fn steps_to_tau() -> usize {
    100
}

#[test]
fn a_closed_switch_pulls_the_node_low() {
    let mut board = synthetic(
        vec![
            synth_part(
                "r1",
                "resistor",
                &[("1", PinKind::Passive), ("2", PinKind::Passive)],
                &[("value", "10k")],
            ),
            synth_part(
                "btn1",
                "pushbutton",
                &[
                    ("1.l", PinKind::Passive),
                    ("1.r", PinKind::Passive),
                    ("2.l", PinKind::Passive),
                    ("2.r", PinKind::Passive),
                ],
                &[],
            ),
        ],
        &[
            &["$v33", "r1:1"],
            &["r1:2", "gpio0", "btn1:1.l"],
            &["btn1:2.l", "$gnd"],
        ],
    );
    board.set_analog(true);

    // Open is 10 MΩ against the 10 kΩ pull-up: essentially the rail.
    let open = board
        .analog_solve()
        .unwrap()
        .node_voltage_gpio(0)
        .expect("solved");
    assert!(
        open > 3.0,
        "an open switch floats the node high, got {open} V"
    );

    assert!(board.press("btn1", true, 0));
    assert_eq!(
        board.state("btn1"),
        Some(PartState::Button { pressed: true })
    );
    let closed = board
        .analog_solve()
        .unwrap()
        .node_voltage_gpio(0)
        .expect("solved");
    assert!(
        closed < 0.01,
        "a closed switch (0.1 Ω) pulls the node low, got {closed} V"
    );

    assert!(board.press("btn1", false, 0));
    let open_again = board
        .analog_solve()
        .unwrap()
        .node_voltage_gpio(0)
        .expect("solved");
    assert!(
        open_again > 3.0,
        "released it returns high, got {open_again} V"
    );
}

#[test]
fn an_ntc_reading_follows_its_control() {
    let mut board = synthetic(
        vec![synth_part(
            "ntc1",
            "ntc-temperature-sensor",
            &[
                ("VCC", PinKind::Power),
                ("GND", PinKind::Ground),
                ("AO", PinKind::AnalogOut),
                ("DO", PinKind::DigitalOut),
            ],
            &[("temperature", "25")],
        )],
        &[
            &["$v33", "ntc1:VCC"],
            &["ntc1:GND", "$gnd"],
            &["ntc1:AO", "gpio0"],
        ],
    );
    board.set_analog(true);

    // The control spans −20..100 °C; the output is that fraction of 3.3 V.
    assert!(board.set_control("ntc1", 500, 0));
    let mid = board
        .node_voltage_gpio(0)
        .expect("the NTC output is solved");
    assert!((mid - 1.65).abs() < 0.02, "at 40 °C the output was {mid} V");
    let adc = board
        .analog_inputs()
        .into_iter()
        .find(|&(pin, _)| pin == 0)
        .map(|(_, v)| v);
    assert!(
        (adc.unwrap() - 1.65).abs() < 0.02,
        "the ADC reads the solved divider"
    );

    assert!(board.set_control("ntc1", 1000, 0));
    let hot = board.node_voltage_gpio(0).expect("solved");
    assert!((hot - 3.3).abs() < 0.02, "at 100 °C the output was {hot} V");

    assert!(board.set_control("ntc1", 0, 0));
    let cold = board.node_voltage_gpio(0).expect("solved");
    assert!(cold.abs() < 0.02, "at −20 °C the output was {cold} V");
}

#[test]
fn a_bjt_switch_drives_a_load() {
    let mut board = synthetic(
        vec![
            synth_part(
                "rb",
                "resistor",
                &[("1", PinKind::Passive), ("2", PinKind::Passive)],
                &[("value", "10k")],
            ),
            synth_part(
                "rl",
                "resistor",
                &[("1", PinKind::Passive), ("2", PinKind::Passive)],
                &[("value", "1k")],
            ),
            synth_part(
                "q1",
                "bjt",
                &[
                    ("C", PinKind::Passive),
                    ("B", PinKind::Passive),
                    ("E", PinKind::Passive),
                ],
                &[("beta", "100"), ("vbe", "0.7")],
            ),
        ],
        &[
            &["gpio4", "rb:1"],
            &["rb:2", "q1:B"],
            &["$v33", "rl:1"],
            &["rl:2", "q1:C", "gpio0"],
            &["q1:E", "$gnd"],
        ],
    );
    board.set_analog(true);

    board.set_chip_output(4, Drive::strong(Level::High), 0);
    let on = board.analog_solve().unwrap();
    let collector = on.node_voltage_gpio(0).expect("the collector is solved");
    assert!(
        collector < 0.5,
        "a saturated BJT pulls its collector low, got {collector} V"
    );
    assert!(
        on.current("q1").unwrap() > 1e-3,
        "the load current flows through the BJT"
    );

    board.set_chip_output(4, Drive::strong(Level::Low), 0);
    let off = board
        .analog_solve()
        .unwrap()
        .node_voltage_gpio(0)
        .expect("solved");
    assert!(
        off > 3.0,
        "with the base low the collector returns high, got {off} V"
    );
}

#[test]
fn turning_analog_off_restores_the_digital_path() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"pot1","type":"litz-potentiometer","attrs":{"value":"50"}}"#,
        ],
        &[
            r#"["esp:3V3","pot1:1","",[]]"#,
            r#"["pot1:2","esp:GPIO0","",[]]"#,
            r#"["pot1:3","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);
    assert!(
        (adc(&board, 0).unwrap() - 1.65).abs() < 0.01,
        "solved wiper"
    );

    // Enter transient mode; the ADC now reads the stepped solution.
    board.analog_transient(1_000).unwrap();
    assert!(board.analog_transient_active());
    assert!(
        (adc(&board, 0).unwrap() - 1.65).abs() < 0.01,
        "stepped wiper"
    );

    board.set_analog(false);
    assert!(!board.analog_enabled());
    assert!(
        !board.analog_transient_active(),
        "off drops the transient history"
    );
    assert_eq!(board.analog_time(), None);

    // The digital path is exactly what it was before co-simulation: the
    // static `analog()` value and the unresolved level on the wiper net.
    assert_eq!(board.analog_inputs(), vec![(0, 1.65_f32)]);
    assert_eq!(board.state("pot1"), Some(PartState::Knob { permille: 500 }));
    assert_eq!(board.part_pin_level("pot1", "2"), Some(Level::HighZ));
    assert!(board.conflicts().is_empty());
}

#[test]
fn analog_advance_lands_on_the_chip_time() {
    let mut board = rc_board();
    board.set_analog(true);
    board.set_chip_output(4, Drive::strong(Level::High), 0);

    // One millisecond at the default 1 µs step is one time constant.
    let solution = board.analog_advance(1_000_000).expect("the advance solves");
    assert_eq!(board.analog_time(), Some(1_000_000));
    let at_tau = solution.node_voltage_gpio(0).expect("solved");
    let expected = 3.3 * (1.0 - (-1.0f64).exp());
    assert!(
        (at_tau - expected).abs() / expected < 0.02,
        "advance reached {at_tau} V, expected about {expected} V"
    );

    // Asking for the current time changes nothing, and the final partial
    // slice lands exactly on the requested tick.
    let same = board.analog_advance(1_000_000).unwrap();
    assert_eq!(same.node_voltage_gpio(0), Some(at_tau));
    board.analog_advance(1_000_500).unwrap();
    assert_eq!(board.analog_time(), Some(1_000_500));
    assert!(
        board.analog_advance(1_000_000).is_err(),
        "time cannot run backwards"
    );
}

#[test]
fn a_drive_change_restarts_the_transient() {
    let mut board = rc_board();
    board.set_analog(true);
    board.set_chip_output(4, Drive::strong(Level::High), 0);

    board.analog_transient(10_000).unwrap();
    assert!(board.analog_transient_active());

    // The invalidation rule in action: a new drive discards the run, so the
    // next step starts from a zero state rather than splicing the old charge
    // onto the new excitation.
    board.set_chip_output(4, Drive::strong(Level::Low), 0);
    assert!(!board.analog_transient_active());
    assert_eq!(board.analog_time(), None);

    let fresh = board.analog_transient(10_000).unwrap();
    let volts = fresh.node_voltage_gpio(0).expect("solved");
    assert!(
        volts.abs() < 0.01,
        "a fresh transient starts discharged, got {volts} V"
    );
}
