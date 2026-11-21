//! End-to-end MicroPython: the official ESP32-C3 image boots from the mask
//! ROM, the runner waits for the REPL prompt, and a script pasted over the
//! console drives an LED on the diagram.
//!
//! The images are downloads, so this test is ignored by default and
//! `cargo test --workspace` stays asset-free:
//!
//! ```sh
//! tools/fetch-micropython.sh
//! LITZ_MICROPYTHON=build/micropython \
//!   cargo test -p litz-cli --test micropython -- --ignored
//! ```

use litz_cli::runner::{Mode, execute};
use std::path::{Path, PathBuf};

fn asset_dir(var: &str) -> PathBuf {
    let p = PathBuf::from(std::env::var(var).unwrap_or_else(|_| panic!("{var} is not set")));
    if p.is_absolute() {
        p
    } else {
        // Cargo runs tests with the crate directory as the working directory;
        // relative asset paths in the command line are meant from the repo root.
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(p)
    }
}

const DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
    {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
  ],
  "connections": [
    ["esp:GPIO8", "r1:1", "", []],
    ["r1:2", "led1:A", "", []],
    ["led1:C", "esp:GND.1", "", []]
  ]
}"#;

const SCRIPT: &str = r#"import machine
print("litz: micropython")
led = machine.Pin(8, machine.Pin.OUT)
led.value(1)
"#;

#[test]
#[ignore = "needs LITZ_MICROPYTHON=build/micropython (tools/fetch-micropython.sh)"]
fn micropython_runs_a_pasted_script() {
    let assets = asset_dir("LITZ_MICROPYTHON");
    let firmware = assets.join("ESP32_GENERIC_C3-20260824-v1.29.0.bin");
    let rom = assets.join("esp32c3_rev3_rom.elf");

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/litz-cli-tests/micropython");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("diagram.json"), DIAGRAM).unwrap();
    std::fs::write(dir.join("main.py"), SCRIPT).unwrap();
    std::fs::write(
        dir.join("litz.toml"),
        format!(
            r#"
[sim]
firmware = "{}"
rom = "{}"
runtime = "micropython"
script = "main.py"

[[test]]
name = "pasted script runs"
timeout = "20s"
expect = "litz: micropython"
expect_state = "led1:on"
"#,
            firmware.display(),
            rom.display()
        ),
    )
    .unwrap();

    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
    assert!(
        outcome.output.contains("litz: micropython"),
        "{}",
        outcome.output
    );
}
