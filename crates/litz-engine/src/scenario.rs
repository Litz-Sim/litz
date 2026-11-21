//! Automation scenarios (`scenario.yaml`), as run by `litz-cli --scenario`.
//!
//! Supported steps:
//! - `delay`: a duration string such as `500ms` or `2s`.
//! - `wait-serial`: text to wait for; a scalar or `{text, timeout}`.
//! - `write-serial`: text, a list of byte values, or `{value: ...}`.
//! - `wait-pin`: `{part-id, name?, value, timeout?}`; blocks until the pin
//!   reaches the level or the timeout expires.
//! - `expect-pin`: `{part-id, value}`; the pin name (`name`, or the older
//!   `pin`) is accepted, and `expected` is accepted for `value`.
//! - `set-control`: `{part-id, name|control, value}`.
//! - `take-screenshot`: a path, `{path, board, expected}`, or Wokwi's
//!   `{part-id, save-to, compare-with}`. A PNG of that part (or of the board's
//!   display) is written at that point in the run; a reference image is
//!   compared pixel by pixel, with `tolerance` per channel.
//! - `touch`: `{x, y, down}` presses or releases; without `down` it is Wokwi's
//!   tap, releasing after `duration` (default 50 ms). `touch-press`,
//!   `touch-move` and `touch-release` are the low-level form. Coordinates are
//!   panel pixels.
//!
//! Any unknown step key is rejected when the scenario loads, so a scenario
//! cannot silently half-run. Durations are relative to the previous step.

use serde::Deserialize;
use serde_yaml_ng as serde_yaml;
use serde_yaml_ng::{Mapping, Value};
use std::path::{Path, PathBuf};

const SUPPORTED: &str = "delay, wait-serial, write-serial, wait-pin, expect-pin, set-control, \
                         take-screenshot, touch, touch-press, touch-move, touch-release";

/// A parsed automation scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    pub name: Option<String>,
    pub version: Option<u32>,
    pub steps: Vec<Step>,
}

/// One scenario step, carrying what the runner needs to execute it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Delay {
        ns: u64,
    },
    WaitSerial {
        text: String,
        timeout: Option<u64>,
    },
    WriteSerial {
        text: String,
    },
    WaitPin {
        part: String,
        pin: Option<String>,
        value: bool,
        timeout: Option<u64>,
    },
    ExpectPin {
        part: String,
        pin: Option<String>,
        value: bool,
    },
    SetControl {
        part: String,
        control: String,
        value: String,
    },
    TakeScreenshot {
        part: Option<String>,
        path: Option<PathBuf>,
        board: Option<String>,
        expected: Option<PathBuf>,
        tolerance: u8,
    },
    Touch {
        x: u16,
        y: u16,
        down: bool,
        board: Option<String>,
        part: Option<String>,
    },
    TouchMove {
        x: u16,
        y: u16,
        board: Option<String>,
        part: Option<String>,
    },
    TouchTap {
        x: u16,
        y: u16,
        duration: u64,
        wait: bool,
        board: Option<String>,
        part: Option<String>,
    },
    TouchRelease {
        x: Option<u16>,
        y: Option<u16>,
        board: Option<String>,
        part: Option<String>,
    },
}

impl Scenario {
    /// Read and parse a scenario file, naming the file in any error.
    pub fn load(path: &Path) -> Result<Scenario, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parse scenario text. Used by `load`, and directly by the tests.
    pub fn parse(text: &str) -> Result<Scenario, String> {
        let raw: Raw = serde_yaml::from_str(text).map_err(yaml_message)?;
        let steps = raw
            .steps
            .iter()
            .enumerate()
            .map(|(index, value)| parse_step(value).map_err(|e| format!("step {}: {e}", index + 1)))
            .collect::<Result<_, _>>()?;
        Ok(Scenario {
            name: raw.name,
            version: raw.version,
            steps,
        })
    }
}

#[derive(Deserialize)]
struct Raw {
    name: Option<String>,
    version: Option<u32>,
    #[serde(default)]
    steps: Vec<Value>,
}

fn yaml_message(error: serde_yaml::Error) -> String {
    let line = error
        .location()
        .map(|l| format!("line {}: ", l.line()))
        .unwrap_or_default();
    format!("{line}{error}")
}

/// One entry of `steps`: a mapping with exactly one key, which names the step.
fn parse_step(value: &Value) -> Result<Step, String> {
    let map = value
        .as_mapping()
        .ok_or("a step must be a mapping with one key")?;
    if map.len() != 1 {
        let keys: Vec<&str> = map.iter().map(|(k, _)| k.as_str().unwrap_or("?")).collect();
        return Err(format!(
            "a step must have exactly one key, found: {}",
            keys.join(", ")
        ));
    }
    let (key, body) = map.iter().next().expect("checked above");
    let key = key.as_str().ok_or("the step key must be text")?;
    match key {
        "delay" => {
            let text = scalar(body, key)?;
            let ns = parse_duration(&text).map_err(|e| format!("delay: {e}"))?;
            Ok(Step::Delay { ns })
        }
        "wait-serial" => parse_wait_serial(body),
        "write-serial" => Ok(Step::WriteSerial {
            text: write_text(body)?,
        }),
        "wait-pin" => parse_wait_pin(body),
        "expect-pin" => parse_expect_pin(body),
        "set-control" => parse_set_control(body),
        "take-screenshot" => parse_take_screenshot(body),
        "touch" => parse_touch(body),
        "touch-press" => {
            let point = touch_point(body, "touch-press", true)?;
            Ok(Step::Touch {
                x: point.x,
                y: point.y,
                down: true,
                board: point.board,
                part: point.part,
            })
        }
        "touch-move" => parse_touch_move(body),
        "touch-release" => parse_touch_release(body),
        other => Err(format!("unknown step {other:?}; supported: {SUPPORTED}")),
    }
}

fn parse_wait_serial(value: &Value) -> Result<Step, String> {
    let Some(map) = value.as_mapping() else {
        return Ok(Step::WaitSerial {
            text: scalar(value, "wait-serial")?,
            timeout: None,
        });
    };
    let text = scalar(
        field(map, "text").ok_or("wait-serial needs a text field")?,
        "text",
    )?;
    let timeout = field(map, "timeout")
        .map(|v| parse_duration(&scalar(v, "wait-serial timeout")?))
        .transpose()?;
    Ok(Step::WaitSerial { text, timeout })
}

/// `write-serial` takes text, or the bytes of the text as a list of numbers.
fn write_text(value: &Value) -> Result<String, String> {
    // Wokwi documents both `write-serial: text` and `write-serial: {value: text}`.
    let value = value
        .as_mapping()
        .and_then(|map| field(map, "value"))
        .unwrap_or(value);
    let Value::Sequence(items) = value else {
        return scalar(value, "write-serial");
    };
    let bytes: Result<Vec<u8>, String> = items
        .iter()
        .map(|item| {
            let number = item
                .as_u64()
                .ok_or("write-serial bytes must be 0 through 255")?;
            u8::try_from(number).map_err(|_| format!("{number} is not a byte"))
        })
        .collect();
    Ok(String::from_utf8_lossy(&bytes?).into_owned())
}

fn parse_expect_pin(value: &Value) -> Result<Step, String> {
    let map = value
        .as_mapping()
        .ok_or("expect-pin needs a mapping with part-id and value")?;
    let part = part_id(map, "expect-pin")?;
    // `name` is the pin to look at, when the part is not directly on a chip
    // pin; without it the level is the one on the pin the part is wired to.
    let pin = field(map, "name")
        .map(|v| scalar(v, "pin name"))
        .transpose()?;
    let level = field(map, "value")
        .or_else(|| field(map, "expected"))
        .ok_or("expect-pin needs a value (0 or 1)")?;
    Ok(Step::ExpectPin {
        part,
        pin,
        value: pin_level(level)?,
    })
}

fn parse_set_control(value: &Value) -> Result<Step, String> {
    let map = value
        .as_mapping()
        .ok_or("set-control needs part-id, a control and a value")?;
    let part = part_id(map, "set-control")?;
    let control = field(map, "name").or_else(|| field(map, "control"));
    let control = scalar(control.ok_or("set-control needs a control")?, "control")?;
    let value = field(map, "value").ok_or("set-control needs a value")?;
    Ok(Step::SetControl {
        part,
        control,
        value: scalar(value, "set-control value")?,
    })
}

/// A touch point, shared by the tap and low-level touch steps.
struct TouchPoint {
    x: u16,
    y: u16,
    board: Option<String>,
    part: Option<String>,
}

fn touch_point(
    value: &Value,
    step: &str,
    coordinates_required: bool,
) -> Result<TouchPoint, String> {
    if let Some(text) = value.as_str() {
        let (x, y) = text
            .split_once(',')
            .ok_or_else(|| format!("{step} needs x,y or {{x, y}}"))?;
        return Ok(TouchPoint {
            x: x.trim()
                .parse::<u16>()
                .map_err(|_| format!("{step} x is not a whole number"))?,
            y: y.trim()
                .parse::<u16>()
                .map_err(|_| format!("{step} y is not a whole number"))?,
            board: None,
            part: None,
        });
    }
    let map = value
        .as_mapping()
        .ok_or_else(|| format!("{step} needs x,y or {{x, y}}"))?;
    let number = |key: &str| -> Result<Option<u16>, String> {
        match field(map, key) {
            Some(value) => scalar(value, key)?
                .trim()
                .parse::<u16>()
                .map(Some)
                .map_err(|_| format!("{step} {key} is not a whole number")),
            None if coordinates_required => Err(format!("{step} needs {key}")),
            None => Ok(None),
        }
    };
    let x = number("x")?;
    let y = number("y")?;
    let board = field(map, "board")
        .map(|value| scalar(value, "board"))
        .transpose()?;
    let part = field(map, "part-id")
        .or_else(|| field(map, "part"))
        .map(|value| scalar(value, "part-id"))
        .transpose()?;
    match (x, y) {
        (Some(x), Some(y)) => Ok(TouchPoint { x, y, board, part }),
        (None, None) if !coordinates_required => Ok(TouchPoint {
            x: 0,
            y: 0,
            board,
            part,
        }),
        _ => Err(format!("{step} needs both x and y")),
    }
}

/// `touch: {x, y, down}` keeps the original press/release form; without
/// `down`, Wokwi's meaning applies: a tap that releases after `duration`.
fn parse_touch(value: &Value) -> Result<Step, String> {
    let down = value
        .as_mapping()
        .and_then(|map| field(map, "down"))
        .map(|value| -> Result<bool, String> {
            let text = scalar(value, "down")?;
            Ok(text.trim() != "0" && text.trim() != "false")
        })
        .transpose()?;
    if let Some(down) = down {
        let point = touch_point(value, "touch", true)?;
        return Ok(Step::Touch {
            x: point.x,
            y: point.y,
            down,
            board: point.board,
            part: point.part,
        });
    }
    let point = touch_point(value, "touch", true)?;
    let duration = value
        .as_mapping()
        .and_then(|map| field(map, "duration"))
        .map(|value| parse_duration(&scalar(value, "touch duration")?))
        .transpose()?
        .unwrap_or(50_000_000);
    let wait = value
        .as_mapping()
        .and_then(|map| field(map, "wait"))
        .map(|value| -> Result<bool, String> {
            let text = scalar(value, "wait")?;
            Ok(text.trim() != "0" && text.trim() != "false")
        })
        .transpose()?
        .unwrap_or(false);
    Ok(Step::TouchTap {
        x: point.x,
        y: point.y,
        duration,
        wait,
        board: point.board,
        part: point.part,
    })
}

fn parse_touch_move(value: &Value) -> Result<Step, String> {
    let point = touch_point(value, "touch-move", true)?;
    Ok(Step::TouchMove {
        x: point.x,
        y: point.y,
        board: point.board,
        part: point.part,
    })
}

fn parse_touch_release(value: &Value) -> Result<Step, String> {
    let point = touch_point(value, "touch-release", false)?;
    let has_point = value.as_str().is_some()
        || value
            .as_mapping()
            .is_some_and(|map| field(map, "x").is_some() && field(map, "y").is_some());
    Ok(Step::TouchRelease {
        x: has_point.then_some(point.x),
        y: has_point.then_some(point.y),
        board: point.board,
        part: point.part,
    })
}

/// Wokwi's `{part-id, save-to, compare-with}` and the older
/// `{path, board, expected}` both name the same three things.
fn parse_take_screenshot(value: &Value) -> Result<Step, String> {
    if let Some(text) = value.as_str() {
        return Ok(Step::TakeScreenshot {
            part: None,
            path: Some(PathBuf::from(text)),
            board: None,
            expected: None,
            tolerance: 0,
        });
    }
    let map = value
        .as_mapping()
        .ok_or("take-screenshot needs a path or {part-id, save-to, compare-with}")?;
    let path = field(map, "path")
        .or_else(|| field(map, "save-to"))
        .map(|value| scalar(value, "path").map(PathBuf::from))
        .transpose()?;
    let expected = field(map, "expected")
        .or_else(|| field(map, "compare-with"))
        .map(|value| scalar(value, "expected").map(PathBuf::from))
        .transpose()?;
    if path.is_none() && expected.is_none() {
        return Err("take-screenshot needs save-to, compare-with, or both".into());
    }
    let part = field(map, "part-id")
        .or_else(|| field(map, "part"))
        .map(|value| scalar(value, "part-id"))
        .transpose()?;
    let board = field(map, "board")
        .map(|value| scalar(value, "board"))
        .transpose()?;
    let tolerance = match field(map, "tolerance") {
        Some(value) => scalar(value, "tolerance")?
            .trim()
            .parse::<u8>()
            .map_err(|_| "tolerance is not 0 through 255".to_string())?,
        None => 0,
    };
    Ok(Step::TakeScreenshot {
        part,
        path,
        board,
        expected,
        tolerance,
    })
}

fn parse_wait_pin(value: &Value) -> Result<Step, String> {
    let map = value
        .as_mapping()
        .ok_or("wait-pin needs {part-id, value}")?;
    let part = part_id(map, "wait-pin")?;
    let pin = field(map, "name")
        .map(|v| scalar(v, "pin name"))
        .transpose()?;
    let level = field(map, "value")
        .or_else(|| field(map, "expected"))
        .ok_or("wait-pin needs a value (0 or 1)")?;
    let timeout = field(map, "timeout")
        .map(|v| parse_duration(&scalar(v, "wait-pin timeout")?))
        .transpose()?;
    Ok(Step::WaitPin {
        part,
        pin,
        value: pin_level(level)?,
        timeout,
    })
}

/// `part-id`, or a bare `pin` when it is the only identifier present, as in
/// simplified examples such as `{pin: led1, value: 1}`.
fn part_id(map: &Mapping, step: &str) -> Result<String, String> {
    let part = field(map, "part-id")
        .or_else(|| field(map, "pin"))
        .ok_or_else(|| format!("{step} needs a part-id"))?;
    scalar(part, "part-id")
}

/// A pin level written as a boolean, a number, or text such as `high`.
fn pin_level(value: &Value) -> Result<bool, String> {
    match value {
        Value::Bool(level) => Ok(*level),
        Value::Number(number) => match number.as_i64() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(format!("pin level {number} is not 0 or 1")),
        },
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "low" => Ok(false),
            "1" | "true" | "high" => Ok(true),
            _ => Err(format!("pin level {text:?} is not 0, 1, low or high")),
        },
        _ => Err("the pin level must be 0 or 1".into()),
    }
}

/// A scalar as text; numbers keep their YAML spelling.
fn scalar(value: &Value, what: &str) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(flag) => Ok(flag.to_string()),
        _ => Err(format!(
            "{what} expects a single value, not a list or mapping"
        )),
    }
}

fn field<'a>(map: &'a Mapping, name: &str) -> Option<&'a Value> {
    map.iter()
        .find(|(key, _)| key.as_str() == Some(name))
        .map(|(_, value)| value)
}

/// `500ms`, `2s`, `10s`, `1m`, or a bare number of seconds. Mirrors
/// `config::parse_duration`, kept local so it can later become a shared helper.
fn parse_duration(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (number, scale) = if let Some(v) = text.strip_suffix("ms") {
        (v, 1_000_000u64)
    } else if let Some(v) = text.strip_suffix("us") {
        (v, 1_000)
    } else if let Some(v) = text.strip_suffix('s') {
        (v, 1_000_000_000)
    } else if let Some(v) = text.strip_suffix('m') {
        (v, 60_000_000_000)
    } else {
        (text, 1_000_000_000)
    };
    let value: f64 = number
        .trim()
        .parse()
        .map_err(|_| format!("{text:?} is not a duration"))?;
    if value < 0.0 {
        return Err(format!("{text:?} is negative"));
    }
    Ok((value * scale as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENARIO_EXAMPLE: &str = r#"name: Blink
version: 1
author: Someone
steps:
  - wait-serial: "Ready"
  - delay: 500ms
  - write-serial: "on\n"
  - wait-serial: "LED on"
  - expect-pin: { pin: "led1", value: 1 }
  - set-control: { part-id: "pot1", control: "value", value: "512" }
"#;

    #[test]
    fn all_supported_steps_parse() {
        let text = r#"name: Everything
version: 1
steps: [{delay: 500ms}, {wait-serial: "Ready"}, {write-serial: "on\n"}, {expect-pin: {part-id: led1, name: anode, value: 1}}, {set-control: {part-id: pot1, name: value, value: 512}}]
"#;
        let s = Scenario::parse(text).unwrap();
        assert_eq!(
            (s.name.as_deref(), s.version),
            (Some("Everything"), Some(1))
        );
        assert_eq!(s.steps.len(), 5);
        assert_eq!(s.steps[0], Step::Delay { ns: 500_000_000 });
        assert_eq!(
            s.steps[1],
            Step::WaitSerial {
                text: "Ready".into(),
                timeout: None
            }
        );
        assert_eq!(
            s.steps[2],
            Step::WriteSerial {
                text: "on\n".into()
            }
        );
        assert_eq!(
            s.steps[3],
            Step::ExpectPin {
                part: "led1".into(),
                pin: Some("anode".into()),
                value: true
            }
        );
        assert_eq!(
            s.steps[4],
            Step::SetControl {
                part: "pot1".into(),
                control: "value".into(),
                value: "512".into()
            }
        );
    }

    #[test]
    fn a_screenshot_step_takes_a_path_or_a_mapping() {
        let s = Scenario::parse(
            "steps:\n  - take-screenshot: shot.png\n  - take-screenshot: {path: later.png, board: esp, expected: golden.png}\n",
        )
        .unwrap();
        assert_eq!(
            s.steps[0],
            Step::TakeScreenshot {
                part: None,
                path: Some(PathBuf::from("shot.png")),
                board: None,
                expected: None,
                tolerance: 0
            }
        );
        assert_eq!(
            s.steps[1],
            Step::TakeScreenshot {
                part: None,
                path: Some(PathBuf::from("later.png")),
                board: Some("esp".into()),
                expected: Some(PathBuf::from("golden.png")),
                tolerance: 0
            }
        );
    }

    #[test]
    fn the_wokwi_screenshot_fields_are_accepted() {
        let s = Scenario::parse(
            "steps:\n  - take-screenshot: {part-id: oled1, save-to: shots/oled.png, compare-with: ref/oled.png, tolerance: 4}\n  - take-screenshot: {part-id: oled1, compare-with: ref/oled.png}\n",
        )
        .unwrap();
        assert_eq!(
            s.steps[0],
            Step::TakeScreenshot {
                part: Some("oled1".into()),
                path: Some(PathBuf::from("shots/oled.png")),
                board: None,
                expected: Some(PathBuf::from("ref/oled.png")),
                tolerance: 4
            }
        );
        assert_eq!(
            s.steps[1],
            Step::TakeScreenshot {
                part: Some("oled1".into()),
                path: None,
                board: None,
                expected: Some(PathBuf::from("ref/oled.png")),
                tolerance: 0
            }
        );
    }

    #[test]
    fn a_touch_step_names_the_point_and_the_pen_state() {
        let s = Scenario::parse(
            "steps:\n  - touch: {x: 10, y: 20, down: true}\n  - touch-press: 30,40\n  - touch-release: {x: 30, y: 40}\n",
        )
        .unwrap();
        assert_eq!(
            s.steps[0],
            Step::Touch {
                x: 10,
                y: 20,
                down: true,
                board: None,
                part: None
            }
        );
        assert_eq!(
            s.steps[1],
            Step::Touch {
                x: 30,
                y: 40,
                down: true,
                board: None,
                part: None
            }
        );
        assert_eq!(
            s.steps[2],
            Step::TouchRelease {
                x: Some(30),
                y: Some(40),
                board: None,
                part: None
            }
        );
    }

    #[test]
    fn a_wokwi_tap_and_a_move_parse() {
        let s = Scenario::parse(
            "steps:\n  - touch: {part-id: box, x: 120, y: 160, duration: 100ms}\n  - touch-press: {x: 1, y: 2}\n  - touch-move: {x: 3, y: 4}\n  - touch-release: {part-id: box}\n",
        )
        .unwrap();
        assert_eq!(
            s.steps[0],
            Step::TouchTap {
                x: 120,
                y: 160,
                duration: 100_000_000,
                wait: false,
                board: None,
                part: Some("box".into())
            }
        );
        assert_eq!(
            s.steps[1],
            Step::Touch {
                x: 1,
                y: 2,
                down: true,
                board: None,
                part: None
            }
        );
        assert_eq!(
            s.steps[2],
            Step::TouchMove {
                x: 3,
                y: 4,
                board: None,
                part: None
            }
        );
        assert_eq!(
            s.steps[3],
            Step::TouchRelease {
                x: None,
                y: None,
                board: None,
                part: Some("box".into())
            }
        );
    }

    #[test]
    fn wait_pin_parses_with_a_timeout() {
        let s = Scenario::parse(
            "steps:\n  - wait-pin: {part-id: led1, name: anode, value: high, timeout: 2s}\n",
        )
        .unwrap();
        assert_eq!(
            s.steps[0],
            Step::WaitPin {
                part: "led1".into(),
                pin: Some("anode".into()),
                value: true,
                timeout: Some(2_000_000_000)
            }
        );
    }

    #[test]
    fn an_unknown_step_names_the_step_and_the_known_ones() {
        let error = Scenario::parse("steps:\n  - move-serial: { part-id: rgb }\n").unwrap_err();
        assert!(error.contains("move-serial"), "{error}");
        assert!(
            error.contains("delay") && error.contains("set-control"),
            "{error}"
        );
    }

    #[test]
    fn a_bad_duration_is_an_error() {
        let error = Scenario::parse("steps:\n  - delay: soon\n").unwrap_err();
        assert!(error.contains("delay"), "{error}");
        assert!(error.contains("soon"), "{error}");
    }

    #[test]
    fn the_scenario_example_parses() {
        let s = Scenario::parse(SCENARIO_EXAMPLE).unwrap();
        assert_eq!(s.steps.len(), 6);
        assert_eq!(
            s.steps[0],
            Step::WaitSerial {
                text: "Ready".into(),
                timeout: None
            }
        );
        assert_eq!(s.steps[1], Step::Delay { ns: 500_000_000 });
        assert_eq!(
            s.steps[2],
            Step::WriteSerial {
                text: "on\n".into()
            }
        );
        assert_eq!(
            s.steps[4],
            Step::ExpectPin {
                part: "led1".into(),
                pin: None,
                value: true
            }
        );
        assert_eq!(
            s.steps[5],
            Step::SetControl {
                part: "pot1".into(),
                control: "value".into(),
                value: "512".into()
            }
        );
    }
}
