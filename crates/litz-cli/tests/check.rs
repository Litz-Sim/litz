//! `litz check` on diagrams that must be rejected, and one that must not.
//!
//! These need no ROM and no firmware: validation happens before anything is
//! loaded, which is the point of having `check` at all.

use litz_cli::runner::{Mode, execute};
use std::path::{Path, PathBuf};

/// Scratch space inside `target/`, so nothing lands in the source tree or in
/// the system temp directory.
fn project(name: &str, diagram: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/litz-cli-tests")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("diagram.json"), diagram).unwrap();
    dir
}

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;
const RESISTOR: &str = r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"220"}}"#;
const LED: &str = r#"{"id":"led1","type":"litz-led","attrs":{"color":"green"}}"#;

fn diagram(connections: &[&str]) -> String {
    format!(
        r#"{{"version":1,"parts":[{BOARD},{RESISTOR},{LED}],"connections":[{}]}}"#,
        connections.join(",")
    )
}

fn check(dir: &Path) -> (i32, String) {
    let outcome = execute(dir, Mode::Check, false, true).unwrap();
    (outcome.exit_code, outcome.output)
}

#[test]
fn a_sound_diagram_passes() {
    let dir = project(
        "sound",
        &diagram(&[
            r#"["esp:GPIO8","r1:1","",[]]"#,
            r#"["r1:2","led1:A","",[]]"#,
            r#"["led1:C","esp:GND.1","",[]]"#,
        ]),
    );
    let (code, output) = check(&dir);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("diagram is valid"), "{output}");
}

#[test]
fn two_rails_on_one_wire_are_a_short() {
    let dir = project("rail-short", &diagram(&[r#"["$gnd","$v33","",[]]"#]));
    let (code, output) = check(&dir);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("rail-short"), "{output}");
    // One diagnosis, not also "Ground cannot meet Power".
    assert_eq!(output.lines().count(), 1, "{output}");
}

#[test]
fn a_pin_the_board_does_not_have_is_an_error() {
    let dir = project("unknown-pin", &diagram(&[r#"["esp:GPIO99","r1:1","",[]]"#]));
    let (code, output) = check(&dir);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("unknown-pin"), "{output}");
}

#[test]
fn a_misspelled_part_type_is_an_error() {
    let dir = project(
        "unknown-part",
        r#"{"version":1,"parts":[{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}},{"id":"r1","type":"litz-resister","attrs":{}}],"connections":[]}"#,
    );
    let (code, output) = check(&dir);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("unknown-part"), "{output}");
    assert!(
        output.contains(r#"did you mean "litz-resistor"?"#),
        "{output}"
    );
}
