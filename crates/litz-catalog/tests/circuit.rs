//! A diagram checked against the builtin catalog: the two crates have to agree
//! on pin names, kinds and attribute types before anything can run.

use litz_catalog::Registry;
use litz_diagram::{Circuit, Severity};

const BLINKY: &str = r#"{
  "version": 1,
  "author": "Litz",
  "editor": "litz",
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
    {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
  ],
  "connections": [
    ["esp:GPIO8", "r1:1", "green", []],
    ["r1:2", "led1:A", "green", []],
    ["led1:C", "esp:GND.1", "black", []],
    ["esp:TX", "$serialMonitor:RX", "", []]
  ]
}"#;

fn errors(circuit: &Circuit) -> Vec<String> {
    let registry = Registry::builtin();
    circuit
        .validate(&registry)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

#[test]
fn builtin_catalog_accepts_a_diagram() {
    let circuit = Circuit::from_json(BLINKY).unwrap();
    assert_eq!(errors(&circuit), Vec::<String>::new());
}

#[test]
fn unknown_pin_is_reported_with_its_name_and_the_nearest_pin() {
    let circuit = Circuit::from_json(&BLINKY.replace("esp:GPIO8", "esp:GPIO99")).unwrap();
    let e = errors(&circuit).join("\n");
    assert!(e.contains("unknown-pin"), "{e}");
    assert!(e.contains("GPIO99"), "{e}");
    assert!(e.contains("did you mean"), "{e}");
}

#[test]
fn unknown_part_type_is_reported_with_the_nearest_one() {
    let circuit = Circuit::from_json(&BLINKY.replace("litz-resistor", "litz-resister")).unwrap();
    let e = errors(&circuit).join("\n");
    assert!(e.contains("unknown-part"), "{e}");
    assert!(e.contains(r#"did you mean "litz-resistor"?"#), "{e}");
}

#[test]
fn an_unrelated_part_type_gets_no_guess() {
    let circuit = Circuit::from_json(&BLINKY.replace("litz-resistor", "qqqqqqqq")).unwrap();
    let e = errors(&circuit).join("\n");
    assert!(e.contains("unknown-part"), "{e}");
    assert!(!e.contains("did you mean"), "{e}");
}

#[test]
fn enum_attribute_outside_its_values_is_reported_with_the_choices() {
    let circuit = Circuit::from_json(&BLINKY.replace("\"green\"", "\"chartreuse\"")).unwrap();
    let e = errors(&circuit).join("\n");
    assert!(e.contains("attr-type"), "{e}");
    assert!(e.contains("\"green\""), "{e}");
}

#[test]
fn gnd_repeat_limits_the_index() {
    let circuit = Circuit::from_json(&BLINKY.replace("esp:GND.1", "esp:GND.3")).unwrap();
    let e = errors(&circuit).join("\n");
    assert!(e.contains("pin-index"), "{e}");
}
