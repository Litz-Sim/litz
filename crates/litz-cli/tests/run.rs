//! `litz test` against a project that uses none of the downloaded assets: the
//! firmware comes from `litz-fixture`, the button is pressed from the test
//! file, and the assertion is on the circuit rather than on serial text.

use litz_cli::runner::{
    Interactive, Mode, Options, Screenshot, execute, execute_capture, execute_scenario,
    execute_with,
};
use std::path::{Path, PathBuf};

fn project(name: &str, diagram: &str, toml: &str, app: &[u8]) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/litz-cli-tests")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("diagram.json"), diagram).unwrap();
    std::fs::write(dir.join("litz.toml"), toml).unwrap();
    std::fs::write(dir.join("app.bin"), app).unwrap();
    dir
}

const BUTTON: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "btn1", "type": "litz-pushbutton", "attrs": {}}
  ],
  "connections": [
    ["esp:GPIO0", "btn1:1.l", "", []],
    ["btn1:2.l", "esp:GND.1", "", []]
  ]
}"#;

const BLINKY: &str = r#"{
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

#[test]
fn the_led_blinks_with_no_downloaded_assets() {
    let dir = project(
        "blink",
        BLINKY,
        r#"
[sim]
firmware = "app.bin"

[[test]]
name = "blinks"
timeout = "2s"
expect = "litz: blinky"
expect_state = "led1:on"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
    assert!(
        outcome.output.contains("1/1 tests passed"),
        "{}",
        outcome.output
    );
}

#[test]
fn coverage_is_reported_without_changing_the_exit_code() {
    let dir = project(
        "coverage",
        BLINKY,
        r#"
[sim]
firmware = "app.bin"

[[test]]
name = "blinks"
timeout = "2s"
expect = "litz: blinky"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let options = Options {
        coverage: true,
        ..Options::default()
    };
    let outcome = execute_with(&dir, Mode::Test, false, true, &options).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
    assert!(outcome.output.contains("[coverage]"), "{}", outcome.output);
}

#[test]
fn a_test_file_can_press_a_button() {
    let dir = project(
        "press",
        BUTTON,
        r#"
[sim]
firmware = "app.bin"

[[test]]
name = "presses"
timeout = "400ms"
expect = "litz: hello"
expect_state = "btn1:pressed"

[[test.input]]
at = "0s"
pin = "btn1"
level = 0
"#,
        &litz_fixture::hello(),
    );
    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
}

const RV32: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "rv", "type": "board-litz-rv32", "attrs": {}},
    {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
    {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
  ],
  "connections": [
    ["rv:GPIO8", "r1:1", "", []],
    ["r1:2", "led1:A", "", []],
    ["led1:C", "rv:GND.1", "", []]
  ]
}"#;

#[test]
fn a_trap_fails_a_test_without_waiting_for_the_timeout() {
    // One illegal instruction word at the entry point. The reference chip
    // reports it through `host.fault`, and the runner must turn that into a
    // failure now rather than spin until the clock runs out.
    let app = litz_fixture::image(0x4038_0000, &[(0x4038_0000, &[0xff, 0xff, 0xff, 0xff])]);
    let dir = project(
        "trap",
        RV32,
        r#"
[sim]
firmware = "app.bin"

[[test]]
name = "traps"
timeout = "5s"
expect = "never printed"
"#,
        &app,
    );
    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 1, "{}", outcome.output);
    assert!(
        outcome.output.contains("the core trapped"),
        "{}",
        outcome.output
    );
    assert!(!outcome.output.contains("timed out"), "{}", outcome.output);
}

/// The button is held from 0 to 200 ms and released after that, so the same
/// circuit exercises "at an instant" and "for a while".
fn button_timeline(extra: &str) -> String {
    format!(
        r#"[sim]
firmware = "app.bin"

[[test]]
name = "timed"
timeout = "600ms"
expect = "litz: hello"
{extra}

[[test.input]]
at = "0s"
pin = "btn1"
level = 0

[[test.input]]
at = "200ms"
pin = "btn1"
level = 1
"#
    )
}

#[test]
fn state_at_samples_one_instant_instead_of_ever_seen() {
    let pass = project(
        "state-at-pass",
        BUTTON,
        &button_timeline("expect_state = \"btn1:pressed\"\nstate_at = \"100ms\""),
        &litz_fixture::hello(),
    );
    let outcome = execute(&pass, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);

    let fail = project(
        "state-at-fail",
        BUTTON,
        &button_timeline("expect_state = \"btn1:pressed\"\nstate_at = \"400ms\""),
        &litz_fixture::hello(),
    );
    let outcome = execute(&fail, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 1, "{}", outcome.output);
    assert!(
        outcome.output.contains("was not pressed at 0.4s"),
        "{}",
        outcome.output
    );
}

#[test]
fn state_stays_requires_the_state_to_hold() {
    let fail = project(
        "state-stays-fail",
        BUTTON,
        &button_timeline("expect_state = \"btn1:pressed\"\nstate_stays = \"300ms\""),
        &litz_fixture::hello(),
    );
    let outcome = execute(&fail, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 1, "{}", outcome.output);
    assert!(
        outcome.output.contains("did not hold for 0.3s"),
        "{}",
        outcome.output
    );

    let pass = project(
        "state-stays-pass",
        BUTTON,
        &button_timeline("expect_state = \"btn1:pressed\"\nstate_stays = \"100ms\""),
        &litz_fixture::hello(),
    );
    let outcome = execute(&pass, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
}

#[test]
fn an_assertion_on_a_part_that_does_not_exist_is_refused() {
    let dir = project(
        "missing-part",
        BLINKY,
        r#"
[sim]
firmware = "app.bin"

[[test]]
name = "typo"
expect_state = "led2:on"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let err = match execute(&dir, Mode::Test, false, true) {
        Ok(outcome) => panic!("expected an error, got exit {}", outcome.exit_code),
        Err(e) => e,
    };
    assert!(err.contains("no part named \"led2\""), "{err}");
}

#[test]
fn a_state_that_never_happens_fails_the_test() {
    let dir = project(
        "never",
        BLINKY,
        r#"
[sim]
firmware = "app.bin"

[[test]]
name = "stays dark"
timeout = "300ms"
expect_state = "led1:off"
fail = "litz: blinky"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 1, "{}", outcome.output);
    assert!(
        outcome.output.contains("found failure text"),
        "{}",
        outcome.output
    );
}

/// Two boards, each with its own image. Board `a` holds the wire low; board
/// `b` reads it on GPIO0 and lights its LED, so the LED's state is evidence
/// that the two chips and the wire between them are one circuit.
const SIGNAL: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "gen", "type": "litz-signal-generator",
     "attrs": {"wave": "sine", "frequency": "1k", "amplitude": "3.3"}},
    {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
    {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
  ],
  "connections": [
    ["gen:OUT", "r1:1", "", []],
    ["r1:2", "led1:A", "", []],
    ["led1:C", "esp:GND.1", "", []],
    ["gen:GND", "esp:GND.1", "", []]
  ]
}"#;

const TWO_BOARDS: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "a", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "b", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
    {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
  ],
  "connections": [
    ["a:GPIO8", "b:GPIO0", "", []],
    ["b:GPIO8", "r1:1", "", []],
    ["r1:2", "led1:A", "", []],
    ["led1:C", "b:GND.1", "", []]
  ]
}"#;

#[test]
fn each_board_boots_its_own_firmware_and_the_wire_connects_them() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/litz-cli-tests")
        .join("two-boards");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("diagram.json"), TWO_BOARDS).unwrap();
    std::fs::write(
        dir.join("litz.toml"),
        r#"
[[board]]
id = "a"
firmware = "a.bin"

[[board]]
id = "b"
firmware = "b.bin"

[[test]]
name = "b follows a"
timeout = "2s"
expect = ["litz: holding GPIO8 low", "litz: press the button"]
expect_state = "led1:on"
"#,
    )
    .unwrap();
    std::fs::write(dir.join("a.bin"), litz_fixture::hold_low()).unwrap();
    std::fs::write(dir.join("b.bin"), litz_fixture::button_led()).unwrap();

    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
}

#[test]
fn a_diagram_with_two_boards_names_each_one() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/litz-cli-tests")
        .join("two-boards-unnamed");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("diagram.json"), TWO_BOARDS).unwrap();
    std::fs::write(
        dir.join("litz.toml"),
        r#"
[sim]
firmware = "a.bin"
"#,
    )
    .unwrap();
    std::fs::write(dir.join("a.bin"), litz_fixture::hold_low()).unwrap();

    let error = execute(&dir, Mode::Test, false, false)
        .err()
        .expect("two boards need [[board]] entries");
    assert!(error.contains("[[board]]"), "{error}");
}

/// A two-board project whose consoles say which board printed what: `a` holds
/// the wire low, `b` runs the echo image and answers its own UART.
fn two_board_echo(name: &str, toml: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/litz-cli-tests")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("diagram.json"), TWO_BOARDS).unwrap();
    std::fs::write(dir.join("litz.toml"), toml).unwrap();
    std::fs::write(dir.join("a.bin"), litz_fixture::hold_low()).unwrap();
    std::fs::write(dir.join("b.bin"), litz_fixture::echo()).unwrap();
    dir
}

#[test]
fn an_expectation_naming_a_board_reads_only_that_boards_console() {
    let dir = two_board_echo(
        "two-boards-consoles",
        r#"
[[board]]
id = "a"
firmware = "a.bin"

[[board]]
id = "b"
firmware = "b.bin"

[[test]]
name = "each banner belongs to its own board"
timeout = "2s"
expect = ["a@litz: holding GPIO8 low", "b@litz: echo, type a line"]
"#,
    );
    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
}

#[test]
fn a_qualified_expectation_does_not_accept_the_other_boards_console() {
    let dir = two_board_echo(
        "two-boards-wrong-console",
        r#"
[[board]]
id = "a"
firmware = "a.bin"

[[board]]
id = "b"
firmware = "b.bin"

[[test]]
name = "b's banner is not a's"
timeout = "500ms"
expect = "a@litz: echo, type a line"
"#,
    );
    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 1, "{}", outcome.output);
    assert!(
        outcome.output.contains("on board \"a\""),
        "{}",
        outcome.output
    );
}

#[test]
fn a_serial_input_naming_a_board_reaches_that_chip() {
    let dir = two_board_echo(
        "two-boards-input",
        r#"
[[board]]
id = "a"
firmware = "a.bin"

[[board]]
id = "b"
firmware = "b.bin"

[[test]]
name = "b echoes what the test types into b"
timeout = "2s"
expect = ["b@litz: echo", "b@ping"]

[[test.input]]
at = "200ms"
board = "b"
serial = "ping"
"#,
    );
    let outcome = execute(&dir, Mode::Test, false, true).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
}

#[test]
fn an_input_in_a_multi_board_project_names_its_board() {
    let dir = two_board_echo(
        "two-boards-input-unnamed",
        r#"
[[board]]
id = "a"
firmware = "a.bin"

[[board]]
id = "b"
firmware = "b.bin"

[[test]]
name = "an unnamed input has no board to land on"
timeout = "500ms"

[[test.input]]
at = "100ms"
pin = "9"
level = 0
"#,
    );
    let error = execute(&dir, Mode::Test, false, false)
        .err()
        .expect("a two-board project cannot default an input's board");
    assert!(error.contains("needs `board = \"...\"`"), "{error}");
}

#[test]
fn console_and_screenshot_flags_name_their_board() {
    let dir = two_board_echo(
        "two-boards-console-flags",
        r#"
[[board]]
id = "a"
firmware = "a.bin"

[[board]]
id = "b"
firmware = "b.bin"

[[test]]
name = "boot"
timeout = "1s"
"#,
    );

    // A board that is not in the diagram is refused before the run starts.
    let bad = Options {
        screenshot: Some(Screenshot {
            board: Some("zz".into()),
            path: dir.join("shot.png"),
        }),
        ..Options::default()
    };
    let error = execute_capture(&dir, Some("100ms"), false, true, &bad)
        .err()
        .expect("a screenshot cannot address a board that is not there");
    assert!(error.contains("--screenshot names board \"zz\""), "{error}");

    // A named board with no panel is a note, not a failure, and the terminal
    // is attached to the board the flag names.
    let named = Options {
        interactive: Some(Interactive {
            board: Some("b".into()),
        }),
        screenshot: Some(Screenshot {
            board: Some("a".into()),
            path: dir.join("shot.png"),
        }),
        ..Options::default()
    };
    let captured = execute_capture(&dir, Some("100ms"), false, true, &named).unwrap();
    assert_eq!(captured.exit_code, 42, "{}", captured.output);
}

#[test]
fn a_probe_records_a_wire_and_the_scope_writes_a_csv() {
    let dir = project(
        "probe-scope",
        BLINKY,
        r#"
[sim]
firmware = "app.bin"

[[probe]]
name = "led"
point = "esp:GPIO8"

[[test]]
name = "blinks"
timeout = "2s"
expect_state = "led1:on"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let csv_path = dir.join("scope.csv");
    let options = Options {
        scope: Some(csv_path.clone()),
        ..Options::default()
    };
    // A bounded run, not a test: the blink period is longer than the instant a
    // passing `expect_state` stops at, and the record has to span an edge.
    let outcome = execute_capture(&dir, Some("300ms"), false, true, &options).unwrap();
    assert_eq!(outcome.exit_code, 42, "{}", outcome.output);
    let csv = std::fs::read_to_string(&csv_path).unwrap();
    assert!(csv.starts_with("time_ns,led\n"), "{csv}");
    assert!(csv.lines().count() > 2, "{csv}");
    assert!(
        outcome.output.contains("litz: led:"),
        "the report names the channel: {}",
        outcome.output
    );
}

#[test]
fn a_probe_given_on_the_command_line_is_its_own_channel() {
    let dir = project(
        "probe-flag",
        BLINKY,
        r#"
[sim]
firmware = "app.bin"

[[test]]
name = "blinks"
timeout = "2s"
expect_state = "led1:on"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let csv_path = dir.join("scope-flag.csv");
    let options = Options {
        probes: vec!["led1:A".into()],
        scope: Some(csv_path.clone()),
        ..Options::default()
    };
    let outcome = execute_with(&dir, Mode::Test, false, true, &options).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
    let csv = std::fs::read_to_string(&csv_path).unwrap();
    assert!(csv.starts_with("time_ns,led1:A\n"), "{csv}");
}

#[test]
fn a_probe_point_that_is_not_in_the_diagram_is_refused() {
    let dir = project(
        "probe-bad",
        BLINKY,
        r#"
[sim]
firmware = "app.bin"

[[probe]]
point = "nope:A"

[[test]]
name = "blinks"
timeout = "1s"
expect_state = "led1:on"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let error = execute(&dir, Mode::Test, false, false)
        .err()
        .expect("a probe on a part that does not exist is a configuration error");
    assert!(error.contains("neither a board"), "{error}");
}

#[test]
fn a_sine_generator_reaches_the_scope_in_volts() {
    let dir = project(
        "signal-sine",
        SIGNAL,
        r#"
[sim]
firmware = "app.bin"

[[probe]]
name = "sig"
point = "gen:OUT"
"#,
        &litz_fixture::hello(),
    );
    let csv_path = dir.join("scope.csv");
    let options = Options {
        analog: true,
        scope: Some(csv_path.clone()),
        ..Options::default()
    };
    let outcome = execute_capture(&dir, Some("5ms"), false, true, &options).unwrap();
    assert_eq!(outcome.exit_code, 42, "{}", outcome.output);
    let csv = std::fs::read_to_string(&csv_path).unwrap();
    let mut lines = csv.lines();
    assert_eq!(
        lines.next(),
        Some("time_ns,sig,sig_volts"),
        "an analog channel gets its own volts column"
    );
    let volts: Vec<f64> = lines
        .filter_map(|line| line.rsplit(',').next()?.parse().ok())
        .collect();
    assert!(volts.len() > 20, "the sine was sampled: {}", volts.len());
    let low = volts.iter().copied().fold(f64::INFINITY, f64::min);
    let high = volts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    assert!(low < 0.3, "the wave reaches its bottom: {low}");
    assert!(high > 3.0, "the wave reaches its top: {high}");
}

#[test]
fn a_decode_reads_frames_off_a_probed_channel() {
    let dir = project(
        "signal-decode",
        SIGNAL,
        r#"
[sim]
firmware = "app.bin"

[[probe]]
name = "sig"
point = "gen:OUT"

[[decode]]
kind = "uart"
rx = "sig"
"#,
        &litz_fixture::hello(),
    );
    let outcome = execute_capture(&dir, Some("20ms"), false, true, &Options::default()).unwrap();
    assert_eq!(outcome.exit_code, 42, "{}", outcome.output);
    assert!(
        outcome.output.contains("litz: uart sig:"),
        "a decoder names its frames: {}",
        outcome.output
    );
}

#[test]
fn a_decode_naming_an_unprobed_channel_is_refused() {
    let dir = project(
        "decode-missing",
        SIGNAL,
        r#"
[sim]
firmware = "app.bin"

[[probe]]
name = "sig"
point = "gen:OUT"

[[decode]]
kind = "uart"
rx = "missing"
"#,
        &litz_fixture::hello(),
    );
    let error = execute(&dir, Mode::Run, false, false)
        .err()
        .expect("a decoder needs a channel a probe records");
    assert!(error.contains("missing"), "{error}");
}

#[test]
fn a_packet_capture_needs_a_simulated_network() {
    let dir = project(
        "pcap-none",
        BLINKY,
        r#"
[sim]
firmware = "app.bin"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let options = Options {
        pcap: Some(dir.join("run.pcap")),
        ..Options::default()
    };
    let error = execute_with(&dir, Mode::Run, false, true, &options)
        .err()
        .expect("a board with no network has nothing to capture");
    assert!(
        error.contains("--pcap needs a simulated network"),
        "{error}"
    );
}

#[test]
fn a_packet_capture_is_written_when_the_circuit_has_a_network() {
    let diagram = r#"{
      "version": 1,
      "parts": [
        {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
        {"id": "ap", "type": "litz-wifi-router", "attrs": {"echo": "7"}}
      ],
      "connections": []
    }"#;
    let dir = project(
        "pcap-net",
        diagram,
        r#"
[sim]
firmware = "app.bin"

[[test]]
name = "boots"
timeout = "200ms"
expect = "litz: blinky"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let path = dir.join("run.pcap");
    let options = Options {
        pcap: Some(path.clone()),
        ..Options::default()
    };
    execute_with(&dir, Mode::Test, false, true, &options).expect("the run completes");
    let bytes = std::fs::read(&path).expect("the capture is written");
    assert_eq!(&bytes[..4], &0xa1b2_3c4du32.to_le_bytes());
    assert!(bytes.len() >= 24, "the pcap header");
}

#[test]
fn a_scenario_switches_a_board_off_and_on_again() {
    // The board's own switch starts off, so its core never boots until the
    // scenario throws it; `set-control` on a board id is that switch.
    let diagram = BLINKY.replace("\"attrs\": {}", "\"attrs\": {\"power\": false}");
    let dir = project(
        "power-cycle",
        &diagram,
        r#"
[sim]
firmware = "app.bin"
"#,
        &litz_fixture::blinky(16_000_000),
    );
    let scenario = dir.join("scenario.yaml");
    std::fs::write(
        &scenario,
        r#"
name: power cycle
steps:
  - delay: 5ms
  - set-control: {part-id: "esp", name: "power", value: "on"}
  - wait-serial: "litz: blinky"
  - set-control: {part-id: "esp", name: "power", value: "off"}
  - delay: 5ms
  - expect-pin: {part-id: "led1", name: "A", value: 0}
  - set-control: {part-id: "esp", name: "power", value: "on"}
  - wait-pin: {part-id: "led1", name: "A", value: 1, timeout: "500ms"}
"#,
    )
    .unwrap();
    let outcome = execute_scenario(&dir, &scenario, false, false).unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output);
    assert_eq!(
        outcome.output.matches("litz: blinky").count(),
        2,
        "power-on boots the firmware again: {}",
        outcome.output
    );
}
