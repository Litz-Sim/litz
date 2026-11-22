//! The contract every chip backend has to pass.
//!
//! A chip in Litz is any type that implements [`litz_board::Chip`]: a builtin
//! backend such as `litz-esp`, `litz-avr` or `litz-rp`, or a WebAssembly
//! component loaded by `litz-mcu-wasm` or the JavaScript host. The trait is
//! small on purpose, and this crate is the executable form of the part of it
//! a host depends on: an image loads, the chip boots, it runs, it writes a
//! console, it drives a pin, and two identical instances produce the same
//! result.
//!
//! The checks are not a substitute for a backend's own tests. They are the
//! floor: a chip that cannot pass these cannot be a peer of the builtin ones,
//! and a community chip can point at this crate to show that it clears that
//! floor.
//!
//! ```ignore
//! use litz_chip_conformance::{check, Fixture};
//!
//! // `C3`, `MAC`, `FLASH` and `DIAGRAM` come from the backend's own test.
//! let firmware = std::fs::read("bm.bin").unwrap();
//! let fixture = Fixture::new("esp32c3", DIAGRAM, &firmware)
//!     .console("litz: blinky")
//!     .gpio(8);
//! let report = check(
//!     |board| {
//!         let chip = C3::new(MAC, FLASH).with_shared_board(board, 0);
//!         let chip: Box<dyn litz_board::Chip> = Box::new(chip);
//!         Ok(chip)
//!     },
//!     &fixture,
//! );
//! assert!(report.is_ok(), "{report}");
//! ```
#![forbid(unsafe_code)]

use litz_board::{Board, Chip, FlatBoard, ImageKind, Stop};
use litz_catalog::Registry;
use litz_core::{Level, PartState};
use litz_diagram::Circuit;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// One runnable example for one chip.
///
/// `diagram` is a complete `diagram.json` whose board part is the chip under
/// test. The suite builds a fresh board from it for every instance, so the
/// chip is exercised the way a project uses it, not on a bare trait object.
pub struct Fixture<'a> {
    /// The name `Chip::info()` must report.
    pub name: &'a str,
    /// A diagram naming the board the chip belongs to.
    pub diagram: &'a str,
    /// A firmware image the chip boots and runs.
    pub firmware: &'a [u8],
    /// Text the firmware must have written to the console by the end of the
    /// run. `None` checks only that the run completes.
    pub console: Option<&'a str>,
    /// A GPIO the firmware drives. Its level must be readable after the run.
    pub gpio: u8,
    /// A part id and the state it must have after the run.
    pub part: Option<(&'a str, PartState)>,
    /// Device cycles granted to each run.
    pub budget_cycles: u64,
}

impl<'a> Fixture<'a> {
    /// A fixture with no console, pin or part expectation.
    pub fn new(name: &'a str, diagram: &'a str, firmware: &'a [u8]) -> Self {
        Self {
            name,
            diagram,
            firmware,
            console: None,
            gpio: 0,
            part: None,
            budget_cycles: 2_000_000,
        }
    }

    /// Require this text on the console.
    #[must_use]
    pub fn console(mut self, text: &'a str) -> Self {
        self.console = Some(text);
        self
    }

    /// Require this GPIO to have a readable level after the run.
    #[must_use]
    pub fn gpio(mut self, gpio: u8) -> Self {
        self.gpio = gpio;
        self
    }

    /// Require this part to hold this state after the run.
    #[must_use]
    pub fn part(mut self, id: &'a str, state: PartState) -> Self {
        self.part = Some((id, state));
        self
    }

    /// Set the cycle budget for each run.
    #[must_use]
    pub fn budget(mut self, cycles: u64) -> Self {
        self.budget_cycles = cycles;
        self
    }
}

/// One finding: which check failed and what it saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub check: &'static str,
    pub message: String,
}

/// What the checks found.
#[derive(Clone, Debug, Default)]
pub struct Report {
    findings: Vec<Finding>,
}

impl Report {
    /// Whether every check passed.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.findings.is_empty()
    }

    /// The findings, in the order the checks ran.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    fn fail(&mut self, check: &'static str, message: impl Into<String>) {
        self.findings.push(Finding {
            check,
            message: message.into(),
        });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.findings.is_empty() {
            return f.write_str("every chip check passed");
        }
        for (index, finding) in self.findings.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{}: {}", finding.check, finding.message)?;
        }
        Ok(())
    }
}

/// Run the contract checks against one chip.
///
/// `make` receives a fresh shared board and returns the chip attached to it.
/// The suite calls it twice: once for the checks, once more to prove that the
/// same image and the same inputs produce the same console, pin level and
/// stop reason.
#[must_use]
pub fn check<F>(make: F, fixture: &Fixture<'_>) -> Report
where
    F: Fn(Rc<RefCell<Board>>) -> Result<Box<dyn Chip>, String>,
{
    let mut report = Report::default();
    let Some(first) = run_once(&make, fixture, &mut report) else {
        return report;
    };
    let Some(second) = run_once(&make, fixture, &mut report) else {
        return report;
    };
    if first.console != second.console {
        report.fail(
            "determinism",
            format!(
                "two identical runs wrote different consoles: {:?} and {:?}",
                String::from_utf8_lossy(&first.console),
                String::from_utf8_lossy(&second.console)
            ),
        );
    }
    if first.gpio != second.gpio {
        report.fail(
            "determinism",
            format!(
                "two identical runs left GPIO {} at {:?} and {:?}",
                fixture.gpio, first.gpio, second.gpio
            ),
        );
    }
    if first.stop != second.stop {
        report.fail(
            "determinism",
            format!(
                "two identical runs stopped differently: {:?} and {:?}",
                first.stop, second.stop
            ),
        );
    }
    if first.cycles != second.cycles {
        report.fail(
            "determinism",
            format!(
                "two identical runs took {} and {} cycles",
                first.cycles, second.cycles
            ),
        );
    }
    if first.state != second.state {
        report.fail(
            "determinism",
            format!(
                "two identical runs left the part in {:?} and {:?}",
                first.state, second.state
            ),
        );
    }
    report
}

/// What one run observed, for the determinism comparison.
struct Observation {
    console: Vec<u8>,
    gpio: Option<Level>,
    stop: Stop,
    cycles: u64,
    state: Option<PartState>,
}

fn run_once<F>(make: &F, fixture: &Fixture<'_>, report: &mut Report) -> Option<Observation>
where
    F: Fn(Rc<RefCell<Board>>) -> Result<Box<dyn Chip>, String>,
{
    let circuit = match Circuit::from_json(fixture.diagram) {
        Ok(circuit) => circuit,
        Err(error) => {
            report.fail("diagram", format!("the diagram does not parse: {error}"));
            return None;
        }
    };
    let registry = Registry::builtin();
    let flat = match FlatBoard::build(&circuit, &registry) {
        Ok(flat) => flat,
        Err(error) => {
            report.fail("diagram", format!("the board does not build: {error}"));
            return None;
        }
    };
    let board = match Board::new(flat) {
        Ok(board) => Rc::new(RefCell::new(board)),
        Err(error) => {
            report.fail("diagram", format!("the board does not start: {error}"));
            return None;
        }
    };
    let mut chip = match make(Rc::clone(&board)) {
        Ok(chip) => chip,
        Err(error) => {
            report.fail("construct", error);
            return None;
        }
    };

    let info = chip.info();
    if info.name != fixture.name {
        report.fail(
            "info",
            format!(
                "the chip reports itself as {:?}, the fixture expects {:?}",
                info.name, fixture.name
            ),
        );
    }
    if info.cpu_hz == 0 {
        report.fail("info", "the chip reports a zero clock");
    }
    if info.flash.size == 0 {
        report.fail("info", "the chip reports no flash");
    }

    if let Err(error) = chip.load_image(ImageKind::Firmware, info.flash.app, fixture.firmware) {
        report.fail("load", error);
        return None;
    }
    if let Err(error) = chip.boot() {
        report.fail("boot", error);
        return None;
    }

    let result = chip.run(fixture.budget_cycles);
    let console = chip.take_console();
    if let Some(expected) = fixture.console
        && !String::from_utf8_lossy(&console).contains(expected)
    {
        report.fail(
            "console",
            format!(
                "the console does not contain {expected:?}: {:?}",
                String::from_utf8_lossy(&console)
            ),
        );
    }
    if result.stop == Stop::Trap
        && let Some(fault) = chip.fault()
    {
        report.fail("run", format!("the chip trapped: {fault}"));
    }
    let gpio = chip.gpio_level(fixture.gpio);
    if gpio.is_none() {
        report.fail(
            "gpio",
            format!("GPIO {} has no readable level after the run", fixture.gpio),
        );
    }
    let state = fixture.part.and_then(|(id, _)| board.borrow().state(id));
    if let Some((id, expected)) = fixture.part
        && state != Some(expected)
    {
        report.fail(
            "part",
            format!("part {id:?} is {state:?}, the fixture expects {expected:?}"),
        );
    }

    Some(Observation {
        console,
        gpio,
        stop: result.stop,
        cycles: result.cycles,
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use litz_board::{ChipInfo, Conflict, FlashLayout, Input, RunResult};

    const DIAGRAM: &str = r#"{"version":1,"parts":[{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}],"connections":[]}"#;

    /// A chip whose run the test controls, so the checks themselves can be
    /// tested without a real backend.
    struct Fake {
        console: Vec<u8>,
        gpio: Option<Level>,
        trap: bool,
    }

    impl Chip for Fake {
        fn info(&self) -> ChipInfo {
            ChipInfo {
                name: "fake".into(),
                cpu_hz: 1_000_000,
                flash: FlashLayout {
                    size: 4 << 20,
                    bootloader: 0,
                    partition_table: 0,
                    app: 0x1_0000,
                },
            }
        }

        fn load_image(
            &mut self,
            _kind: ImageKind,
            _offset: u64,
            _bytes: &[u8],
        ) -> Result<(), String> {
            Ok(())
        }

        fn boot(&mut self) -> Result<u32, String> {
            Ok(0x1_0000)
        }

        fn schedule(&mut self, _inputs: Vec<Input>) -> Result<(), String> {
            Ok(())
        }

        fn run(&mut self, budget_cycles: u64) -> RunResult {
            RunResult {
                cycles: budget_cycles,
                stop: if self.trap { Stop::Trap } else { Stop::Budget },
            }
        }

        fn seconds(&self) -> f64 {
            0.0
        }

        fn write_console(&mut self, _bytes: &[u8]) {}

        fn take_console(&mut self) -> Vec<u8> {
            std::mem::take(&mut self.console)
        }

        fn state(&self, _part: &str) -> Option<PartState> {
            None
        }

        fn press(&mut self, _part: &str, _pressed: bool) -> bool {
            false
        }

        fn set_control(&mut self, _part: &str, _value: u16) -> bool {
            false
        }

        fn part_pin_level(&self, _part: &str, _pin: Option<&str>) -> Option<Level> {
            None
        }

        fn gpio_level(&self, _gpio: u8) -> Option<Level> {
            self.gpio
        }

        fn conflicts(&self) -> Vec<Conflict> {
            Vec::new()
        }

        fn fault(&self) -> Option<String> {
            self.trap.then(|| "fake trap".to_string())
        }
    }

    fn run(console: &[u8], gpio: Option<Level>) -> Report {
        let fake_console = console.to_vec();
        let fixture = Fixture::new("fake", DIAGRAM, &[]).console("litz: fake");
        check(
            move |_board| {
                Ok(Box::new(Fake {
                    console: fake_console.clone(),
                    gpio,
                    trap: false,
                }) as Box<dyn Chip>)
            },
            &fixture,
        )
    }

    #[test]
    fn a_chip_that_boots_writes_the_text_and_drives_a_pin_passes() {
        let report = run(b"litz: fake\r\n", Some(Level::High));
        assert!(report.is_ok(), "{report}");
    }

    #[test]
    fn a_missing_console_line_is_reported_by_name() {
        let report = run(b"something else", Some(Level::High));
        assert!(!report.is_ok());
        assert_eq!(report.findings()[0].check, "console");
    }

    #[test]
    fn an_unreadable_pin_is_reported_by_number() {
        let report = run(b"litz: fake\r\n", None);
        assert!(!report.is_ok());
        assert_eq!(report.findings()[0].check, "gpio");
    }

    #[test]
    fn a_run_that_traps_is_a_failure_even_with_the_right_console() {
        let fixture = Fixture::new("fake", DIAGRAM, &[]).console("litz: fake");
        let report = check(
            |_board| {
                Ok(Box::new(Fake {
                    console: b"litz: fake".to_vec(),
                    gpio: Some(Level::High),
                    trap: true,
                }) as Box<dyn Chip>)
            },
            &fixture,
        );
        assert!(!report.is_ok());
        assert!(report.findings().iter().any(|f| f.check == "run"));
    }
}
