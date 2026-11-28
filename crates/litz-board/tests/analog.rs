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

#[test]
fn a_multimeter_reads_the_voltage_across_its_leads() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"1k"}}"#,
            r#"{"id":"m1","type":"litz-multimeter","attrs":{"mode":"voltage"}}"#,
        ],
        &[
            r#"["esp:3V3","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
            r#"["m1:+","r1:1","",[]]"#,
            r#"["m1:-","r1:2","",[]]"#,
        ],
    );
    board.set_analog(true);

    let positive = board.node_voltage("m1", "+").expect("the lead is solved");
    let negative = board.node_voltage("m1", "-").expect("the lead is solved");
    assert!(
        (positive - negative - 3.3).abs() < 0.01,
        "the meter reads {} V across the rail",
        positive - negative
    );
    // A voltmeter stands across the circuit without loading it.
    let through = board.node_current("m1").expect("the meter has a current");
    assert!(
        through.abs() < 1e-6,
        "the meter drew {through} A from a 1k load"
    );
}

#[test]
fn a_multimeter_in_series_reads_the_current() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"1k"}}"#,
            r#"{"id":"m1","type":"litz-multimeter","attrs":{"mode":"current"}}"#,
        ],
        &[
            r#"["esp:3V3","r1:1","",[]]"#,
            r#"["r1:2","m1:+","",[]]"#,
            r#"["m1:-","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let amps = board.node_current("m1").expect("the meter is in the path");
    let expected = 3.3 / 1000.01;
    assert!(
        (amps - expected).abs() < 1e-4,
        "the meter read {amps} A, expected about {expected} A"
    );
}

// ------------------------------------------------------------ regulators

/// A 7805 from a 12 V supply holds 5 V into a 100 Ω load, and the source
/// reports the current it delivers.
#[test]
fn a_7805_regulates_twelve_volts_down_to_five() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"ps","type":"litz-power-supply","attrs":{"voltage":12.0}}"#,
            r#"{"id":"u1","type":"litz-voltage-regulator","attrs":{"variant":"7805"}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"100"}}"#,
        ],
        &[
            r#"["ps:+","u1:VI","",[]]"#,
            r#"["ps:-","esp:GND.1","",[]]"#,
            r#"["u1:GND","esp:GND.1","",[]]"#,
            r#"["u1:VO","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let out = board
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!((out - 5.0).abs() < 1e-6, "the regulator held {out} V");
    let amps = board
        .node_current("u1")
        .expect("the pass element has a current");
    assert!(
        (amps + 0.05).abs() < 1e-6,
        "the regulator delivered {} A into 100 ohms",
        -amps
    );
}

/// With only 6 V in, a 7805 cannot hold 5 V: the output sits one dropout
/// below the input instead of floating to 5 V that no pass element can make.
#[test]
fn a_7805_in_dropout_follows_its_input() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"ps","type":"litz-power-supply","attrs":{"voltage":6.0}}"#,
            r#"{"id":"u1","type":"litz-voltage-regulator","attrs":{"variant":"7805"}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"100"}}"#,
        ],
        &[
            r#"["ps:+","u1:VI","",[]]"#,
            r#"["ps:-","esp:GND.1","",[]]"#,
            r#"["u1:GND","esp:GND.1","",[]]"#,
            r#"["u1:VO","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let out = board
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!(
        (out - 4.0).abs() < 0.05,
        "a 7805 with 6 V in should drop out to about 4 V, held {out} V"
    );
    assert!(
        out < 4.9,
        "the regulator claimed {out} V out of a 6 V input"
    );
}

/// An LM317 follows its dial, and a host can move the dial while the board
/// runs.
#[test]
fn an_lm317_regulates_to_its_setting_and_can_be_redialled() {
    let mut under_test = board(
        &[
            BOARD,
            r#"{"id":"ps","type":"litz-power-supply","attrs":{"voltage":12.0}}"#,
            r#"{"id":"u1","type":"litz-voltage-regulator","attrs":{"variant":"LM317","voltage":7.5}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"100"}}"#,
        ],
        &[
            r#"["ps:+","u1:VI","",[]]"#,
            r#"["ps:-","esp:GND.1","",[]]"#,
            r#"["u1:GND","esp:GND.1","",[]]"#,
            r#"["u1:VO","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
        ],
    );
    under_test.set_analog(true);

    let out = under_test
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!((out - 7.5).abs() < 1e-6, "the LM317 held {out} V");

    assert!(under_test.set_named_control("u1", "voltage", "9.0", 0));
    let redialled = under_test
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!(
        (redialled - 9.0).abs() < 1e-6,
        "after dialling 9 V the regulator held {redialled} V"
    );
    // The fixed variants do not have that dial.
    let mut fixed = board(
        &[
            BOARD,
            r#"{"id":"ps","type":"litz-power-supply","attrs":{"voltage":12.0}}"#,
            r#"{"id":"u1","type":"litz-voltage-regulator","attrs":{"variant":"7805"}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"100"}}"#,
        ],
        &[
            r#"["ps:+","u1:VI","",[]]"#,
            r#"["ps:-","esp:GND.1","",[]]"#,
            r#"["u1:GND","esp:GND.1","",[]]"#,
            r#"["u1:VO","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
        ],
    );
    assert!(!fixed.set_named_control("u1", "voltage", "9.0", 0));
}

/// A 5.1 V zener across a 10 V source behind 1 kΩ sets the node at its
/// breakdown instead of leaving it at the supply.
#[test]
fn a_zener_sets_a_reference_on_the_board() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"ps","type":"litz-power-supply","attrs":{"voltage":10.0}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"1k"}}"#,
            r#"{"id":"z1","type":"litz-zener","attrs":{"vz":5.1}}"#,
        ],
        &[
            r#"["ps:+","r1:1","",[]]"#,
            r#"["ps:-","esp:GND.1","",[]]"#,
            r#"["r1:2","z1:C","",[]]"#,
            r#"["z1:A","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let clamped = board.node_voltage("z1", "C").expect("the node is solved");
    assert!(
        (5.0..5.2).contains(&clamped),
        "the zener held {clamped} V, expected about 5.1 V"
    );
    let through = board.node_current("z1").expect("the zener carries current");
    assert!(
        (-5.0e-3..-4.0e-3).contains(&through),
        "the zener carried {through:e} A"
    );
}

// ------------------------------------------------------------ DAC and ADC

/// The DAC's output pin is a source in the analog view at the code the
/// firmware wrote, scaled to the rail its VCC is on.
#[test]
fn a_dac_on_the_bus_sources_its_code() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"dac","type":"litz-mcp4725","attrs":{"value":0}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"1k"}}"#,
        ],
        &[
            r#"["esp:3V3","dac:VCC","",[]]"#,
            r#"["esp:GND.1","dac:GND","",[]]"#,
            r#"["esp:GPIO8","dac:SDA","",[]]"#,
            r#"["esp:GPIO9","dac:SCL","",[]]"#,
            r#"["dac:VOUT","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    // Fast write of 2048: half of the 12-bit range.
    assert_eq!(
        board.i2c_write_read(0, 0, 0x60, &[0x08, 0x00], 0),
        Some(vec![])
    );
    let out = board
        .node_voltage("dac", "VOUT")
        .expect("the output is solved");
    assert!(
        (out - 1.65).abs() < 0.01,
        "code 2048 on a 3.3 V reference gave {out} V"
    );
    // Halving the code halves the output.
    assert_eq!(
        board.i2c_write_read(0, 0, 0x60, &[0x04, 0x00], 0),
        Some(vec![])
    );
    let out = board
        .node_voltage("dac", "VOUT")
        .expect("the output is solved");
    assert!(
        (out - 0.825).abs() < 0.01,
        "code 1024 on a 3.3 V reference gave {out} V"
    );
}

/// An ADS1115 conversion reads the voltage on its channel pin, not a manifest
/// number: the same diagram that would report `a0` reports the divider.
#[test]
fn an_ads1115_converts_the_voltage_its_input_pin_carries() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"r2","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"adc","type":"litz-ads1115","attrs":{}}"#,
        ],
        &[
            r#"["esp:3V3","r1:1","",[]]"#,
            r#"["r1:2","r2:1","",[]]"#,
            r#"["r2:2","esp:GND.1","",[]]"#,
            r#"["r1:2","adc:A0","",[]]"#,
            r#"["esp:3V3","adc:VDD","",[]]"#,
            r#"["esp:GND.1","adc:GND","",[]]"#,
            r#"["esp:GPIO8","adc:SDA","",[]]"#,
            r#"["esp:GPIO9","adc:SCL","",[]]"#,
        ],
    );
    board.set_analog(true);

    // AIN0 single-ended, ±4.096 V, single shot, 128 SPS.
    board.i2c_write_read(0, 0, 0x48, &[0x01, 0xC3, 0x83], 0);
    let raw = board
        .i2c_write_read(0, 0, 0x48, &[0x00], 2)
        .expect("the ADC answers");
    let code = i16::from_be_bytes([raw[0], raw[1]]);
    let expected = (32767.0_f64 * 1.65 / 4.096).round() as i16;
    assert!(
        (code - expected).abs() <= 1,
        "the ADC converted {code}, expected {expected}"
    );
}

/// A DAC trimming a regulator's ADJ pin and an ADC watching the output: the
/// code the firmware latches moves the output, and the conversion that follows
/// reads the moved output rather than the one from before the write.
#[test]
fn a_dac_trims_the_regulator_and_the_adc_reads_the_output() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"ps","type":"litz-power-supply","attrs":{"voltage":9}}"#,
            r#"{"id":"dac","type":"litz-mcp4725","attrs":{}}"#,
            r#"{"id":"reg","type":"litz-voltage-regulator","attrs":{"variant":"LM317"}}"#,
            r#"{"id":"adc","type":"litz-ads1115","attrs":{}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"r2","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"load","type":"litz-resistor","attrs":{"value":"220"}}"#,
        ],
        &[
            r#"["ps:+","reg:VI","",[]]"#,
            r#"["ps:-","esp:GND.1","",[]]"#,
            r#"["reg:GND","esp:GND.1","",[]]"#,
            r#"["dac:VOUT","reg:ADJ","",[]]"#,
            r#"["dac:VCC","esp:3V3","",[]]"#,
            r#"["dac:GND","esp:GND.1","",[]]"#,
            r#"["dac:SDA","esp:GPIO8","",[]]"#,
            r#"["dac:SCL","esp:GPIO9","",[]]"#,
            r#"["adc:VDD","esp:3V3","",[]]"#,
            r#"["adc:GND","esp:GND.1","",[]]"#,
            r#"["adc:SDA","esp:GPIO8","",[]]"#,
            r#"["adc:SCL","esp:GPIO9","",[]]"#,
            r#"["reg:VO","r1:1","",[]]"#,
            r#"["r1:2","adc:A0","",[]]"#,
            r#"["adc:A0","r2:1","",[]]"#,
            r#"["r2:2","esp:GND.1","",[]]"#,
            r#"["reg:VO","load:1","",[]]"#,
            r#"["load:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    // With no code written the ADJ pin is pulled to ground and the regulator
    // sits at its 1.25 V floor.
    let idle = board.node_voltage("reg", "VO").expect("the output solves");
    assert!((idle - 1.25).abs() < 0.01, "the idle output is {idle} V");

    // Fast write of 0x0fc0 = 4032, which is 3.249 V against the DAC's 3.3 V
    // rail, so the output goes to 4.499 V.
    board.i2c_write_read(0, 0, 0x60, &[0x0f, 0xc0], 0);
    let out = board.node_voltage("reg", "VO").expect("the output solves");
    assert!((out - 4.499).abs() < 0.02, "the trimmed output is {out} V");

    // AIN0 single-ended, ±4.096 V, single shot, 128 SPS: the divider halves
    // the output, which is 2.2496 V on the channel.
    board.i2c_write_read(0, 0, 0x48, &[0x01, 0xC3, 0x83], 0);
    let raw = board
        .i2c_write_read(0, 0, 0x48, &[0x00], 2)
        .expect("the ADC answers");
    let code = i16::from_be_bytes([raw[0], raw[1]]);
    let expected = (32767.0_f64 * (out / 2.0) / 4.096).round() as i16;
    assert!(
        (code - expected).abs() <= 1,
        "the ADC converted {code}, expected {expected}"
    );
}

/// The SPI counterpart: an MCP3008 conversion reads its `CHn` pin.
#[test]
fn an_mcp3008_converts_the_voltage_its_channel_pin_carries() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"r2","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"adc","type":"litz-mcp3008","attrs":{}}"#,
        ],
        &[
            r#"["esp:GPIO7","adc:CS","",[]]"#,
            r#"["esp:GPIO8","adc:CLK","",[]]"#,
            r#"["esp:GPIO9","adc:DOUT","",[]]"#,
            r#"["esp:GPIO10","adc:DIN","",[]]"#,
            r#"["esp:3V3","adc:VDD","",[]]"#,
            r#"["esp:3V3","adc:VREF","",[]]"#,
            r#"["esp:GND.1","adc:DGND","",[]]"#,
            r#"["esp:3V3","r1:1","",[]]"#,
            r#"["r1:2","r2:1","",[]]"#,
            r#"["r2:2","esp:GND.1","",[]]"#,
            r#"["r1:2","adc:CH1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let rx = board.spi_transfer(0, 0, 7, &[0x01, 0x90, 0x00], 3);
    let raw = (u16::from(rx[1] & 0x03) << 8) | u16::from(rx[2]);
    assert_eq!(raw, 512, "the divider midpoint on a 3.3 V reference");
}

// ------------------------------------------------------- PWM and feedback

/// A pad a backend reports as a running PWM holds its average voltage in the
/// analog view, so a filter or a load that integrates the pin sees the duty's
/// worth of the rail instead of whichever half-cycle the sample fell in.
#[test]
fn a_pwm_pad_supplies_its_average_voltage() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"l1","type":"litz-inductor","attrs":{"value":"100uH"}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"1k"}}"#,
        ],
        &[
            r#"["esp:GPIO0","l1:1","",[]]"#,
            r#"["l1:2","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    board.set_chip_pwm(0, Some(0.3), 0);
    let low = board.node_voltage_gpio(0).expect("the pad is solved");
    // 30% of 3.3 V behind the 50 Ω driver into 1 kΩ.
    let expected = 0.3 * 3.3 * 1_000.0 / 1_050.0;
    assert!(
        (low - expected).abs() < 0.01,
        "30% PWM gave {low} V, expected {expected}"
    );

    board.set_chip_pwm(0, Some(0.6), 0);
    let high = board.node_voltage_gpio(0).expect("the pad is solved");
    assert!(
        (high - 2.0 * low).abs() < 1e-6,
        "60% PWM gave {high} V against {low} V at 30%"
    );

    // The peripheral stopped: the pad is only its drive, and a pin the chip
    // is not driving has no source.
    board.set_chip_pwm(0, None, 0);
    assert_eq!(
        board.node_voltage_gpio(0),
        Some(0.0),
        "with the PWM stopped the load holds 0 V"
    );
}

/// An LM317 with its adjust pin on a divider regulates 1.25 V across the
/// upper resistor, so the output follows the divider and not the dial.
#[test]
fn an_lm317_with_a_wired_adjust_pin_follows_its_divider() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"ps","type":"litz-power-supply","attrs":{"voltage":12.0}}"#,
            r#"{"id":"u1","type":"litz-voltage-regulator","attrs":{"variant":"LM317","voltage":20.0}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"240"}}"#,
            r#"{"id":"r2","type":"litz-resistor","attrs":{"value":"720"}}"#,
        ],
        &[
            r#"["ps:+","u1:VI","",[]]"#,
            r#"["ps:-","esp:GND.1","",[]]"#,
            r#"["u1:GND","esp:GND.1","",[]]"#,
            r#"["u1:VO","r1:1","",[]]"#,
            r#"["r1:2","u1:ADJ","",[]]"#,
            r#"["u1:ADJ","r2:1","",[]]"#,
            r#"["r2:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let out = board
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!(
        (out - 5.0).abs() < 1e-4,
        "1.25 V across 240 Ω with 720 Ω below gave {out} V, expected 5 V"
    );
    let adjust = board
        .node_voltage("u1", "ADJ")
        .expect("the adjust pin is solved");
    assert!(
        (adjust - 3.75).abs() < 1e-4,
        "the adjust pin held {adjust} V, expected 3.75 V"
    );
}

/// The PCF8591 reads its `AINn` pins through the same solve and sources
/// `AOUT` from the byte the firmware wrote.
#[test]
fn a_pcf8591_reads_its_inputs_and_sources_its_dac() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"r2","type":"litz-resistor","attrs":{"value":"10k"}}"#,
            r#"{"id":"r3","type":"litz-resistor","attrs":{"value":"1k"}}"#,
            r#"{"id":"u1","type":"litz-pcf8591","attrs":{}}"#,
        ],
        &[
            r#"["esp:3V3","r1:1","",[]]"#,
            r#"["r1:2","r2:1","",[]]"#,
            r#"["r2:2","esp:GND.1","",[]]"#,
            r#"["r1:2","u1:AIN0","",[]]"#,
            r#"["esp:3V3","u1:VCC","",[]]"#,
            r#"["esp:GND.1","u1:GND","",[]]"#,
            r#"["esp:GPIO8","u1:SDA","",[]]"#,
            r#"["esp:GPIO9","u1:SCL","",[]]"#,
            r#"["u1:AOUT","r3:1","",[]]"#,
            r#"["r3:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    // Select channel 0; the first conversion returned is the previous latch.
    board.i2c_write_read(0, 0, 0x48, &[0x00], 1);
    let raw = board
        .i2c_write_read(0, 0, 0x48, &[0x00], 1)
        .expect("the converter answers");
    let expected = (1.65_f64 / 3.3 * 255.0).round() as u8;
    assert!(
        raw[0].abs_diff(expected) <= 1,
        "AIN0 converted {}, expected {expected}",
        raw[0]
    );

    // The second byte of a write is the DAC register.
    board.i2c_write_read(0, 0, 0x48, &[0x00, 0xff], 0);
    let out = board
        .node_voltage("u1", "AOUT")
        .expect("the output is solved");
    assert!(
        (out - 3.3).abs() < 0.01,
        "code 255 on a 3.3 V reference gave {out} V"
    );
}

/// A buck module regulates down to its setpoint, but cannot make more than
/// its input; a boost is the mirror, never below its input.
#[test]
fn a_dc_dc_module_respects_its_topology() {
    let circuit = |topology: &str, vin: f64, setpoint: f64, load: &str| {
        board(
            &[
                BOARD,
                &format!(r#"{{"id":"ps","type":"litz-power-supply","attrs":{{"voltage":{vin}}}}}"#),
                &format!(
                    r#"{{"id":"u1","type":"litz-dc-dc","attrs":{{"topology":"{topology}","voltage":{setpoint}}}}}"#
                ),
                &format!(r#"{{"id":"r1","type":"litz-resistor","attrs":{{"value":"{load}"}}}}"#),
            ],
            &[
                r#"["ps:+","u1:VI","",[]]"#,
                r#"["ps:-","esp:GND.1","",[]]"#,
                r#"["u1:GND","esp:GND.1","",[]]"#,
                r#"["u1:VO","r1:1","",[]]"#,
                r#"["r1:2","esp:GND.1","",[]]"#,
            ],
        )
    };

    let mut buck = circuit("buck", 12.0, 5.0, "100");
    buck.set_analog(true);
    let out = buck.node_voltage("u1", "VO").expect("the output is solved");
    assert!((out - 5.0).abs() < 1e-6, "a 12→5 buck gave {out} V");

    // Asked for more than it has, a buck passes its input.
    let mut starved = circuit("buck", 12.0, 20.0, "100");
    starved.set_analog(true);
    let out = starved
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!((out - 12.0).abs() < 1e-6, "a starved buck gave {out} V");

    let mut boost = circuit("boost", 5.0, 12.0, "1k");
    boost.set_analog(true);
    let out = boost
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!((out - 12.0).abs() < 1e-6, "a 5→12 boost gave {out} V");

    // Asked for less than its input, a boost passes the input through.
    let mut below = circuit("boost", 5.0, 3.0, "1k");
    below.set_analog(true);
    let out = below
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!(
        (out - 5.0).abs() < 1e-6,
        "a boost below its input gave {out} V"
    );
}

/// A module with a 0.2 A cap delivers that much into a load that would draw
/// more, so its output sags instead of the source inventing current.
#[test]
fn a_dc_dc_module_folds_at_its_current_limit() {
    let mut board = board(
        &[
            BOARD,
            r#"{"id":"ps","type":"litz-power-supply","attrs":{"voltage":12.0}}"#,
            r#"{"id":"u1","type":"litz-dc-dc","attrs":{"topology":"buck","voltage":5.0,"max_amps":0.2}}"#,
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10"}}"#,
        ],
        &[
            r#"["ps:+","u1:VI","",[]]"#,
            r#"["ps:-","esp:GND.1","",[]]"#,
            r#"["u1:GND","esp:GND.1","",[]]"#,
            r#"["u1:VO","r1:1","",[]]"#,
            r#"["r1:2","esp:GND.1","",[]]"#,
        ],
    );
    board.set_analog(true);

    let out = board
        .node_voltage("u1", "VO")
        .expect("the output is solved");
    assert!(
        (out - 2.0).abs() < 1e-3,
        "0.2 A into 10 Ω gave {out} V, expected 2 V"
    );
    let current = board
        .node_current("u1")
        .expect("the converter carries current");
    assert!(
        (current.abs() - 0.2).abs() < 1e-6,
        "the converter delivered {current} A"
    );
}
