//! Loading a project, booting it and deciding pass or fail.

use crate::config::{
    BoardSection, Config, DecodeSection, Runtime, TestSpec, WifiSpec, parse_duration, parse_size,
};
use crate::image::{self, FirmwareLayout};
use crate::lock::Lock;
use crate::scenario::{Scenario, Step};
use litz_board::chip::{Chip, ImageKind, Input, Stop};
use litz_board::probe;
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Level, PartState};
use litz_diagram::{Circuit, Diagnostic, Severity};
use litz_net::{AccessPoint, NearbyNetwork, NetworkConfig, Service};

use crate::machine::{Attachments, builtin_chip};

// A wasm guest chip needs wasmtime, which cannot itself be built for a wasm32
// host. The browser module uses `machine::boot_c3` only, so the runner's wasm
// chip backend is compiled out there rather than dragging wasmtime along.
#[cfg(not(target_family = "wasm"))]
use litz_mcu_wasm::WasmChip;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const DEFAULT_TIMEOUT: &str = "10s";
const DEFAULT_MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56];
const EXIT_TIMEOUT: i32 = 42;
/// MicroPython's friendly-REPL prompt. It is the only sign the interpreter is
/// ready to accept a paste; matching a banner would tie the runner to one
/// build's wording.
const REPL_PROMPT: &str = ">>> ";
/// How often a CircuitPython board that has not answered yet is sent a key.
/// Long enough that one press survives a `code.py` that is still starting,
/// short enough that a boot which already finished does not wait.
const CIRCUITPYTHON_NUDGE: u64 = 200_000_000;
/// A paste is assembled in RAM before it is sent, so it is capped at the size
/// a REPL session can reasonably swallow. A project that needs more should
/// ship a filesystem image instead.
const MAX_SCRIPT: usize = 64 * 1024;
/// Simulated time granted after a paste is queued before an `expect` that
/// already matched the boot output may end the run. It gives the UART queue
/// time to drain and the interpreter time to start the script, so a test that
/// asserts on the banner cannot pass without the script ever running.
const PASTE_GRACE_NS: u64 = 200_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Run,
    Test,
    Check,
}

/// What the caller wants recorded about a run, beyond pass or fail.
#[derive(Clone, Default)]
pub struct Options {
    /// Write every net transition to this file as a VCD.
    pub vcd: Option<PathBuf>,
    /// Stop the first run at this program counter and report the registers.
    pub break_at: Option<u32>,
    /// Solve the circuit's analog network alongside the digital run. Off by
    /// default; `[sim] analog = true` turns it on for the project.
    pub analog: bool,
    /// Override the project's Wi-Fi access point for this run.
    pub wifi: Option<String>,
    /// Turn on instruction coverage and print the backend's report after the
    /// run. A backend without coverage ignores it.
    pub coverage: bool,
    /// Forward lines typed on stdin to one board's console, so a REPL or a
    /// firmware that asks a question can be answered from the terminal.
    pub interactive: Option<Interactive>,
    /// Write a simulated display here as a PNG: at the end of a `run`, and
    /// when a test fails.
    pub screenshot: Option<Screenshot>,
    /// Points to watch, in addition to the project's `[[probe]]` entries.
    /// Each is its own channel name.
    pub probes: Vec<String>,
    /// Write every probe's record here as a CSV at the end of the run.
    pub scope: Option<PathBuf>,
    /// Write every probe as a sigrok session archive (`.sr`) for PulseView
    /// or `sigrok-cli`. The samples come from the same record the scope
    /// writes, held at each channel's last transition.
    pub sr: Option<PathBuf>,
    /// The samplerate for the sigrok archive, in Hz. `None` uses
    /// [`litz_board::sigrok::DEFAULT_RATE_HZ`].
    pub sr_rate: Option<u64>,
    /// Start the scope's window at a logic-analyzer trigger instead of at
    /// time zero. The trigger names a probe the run already watches.
    pub trigger: Option<litz_board::probe::Trigger>,
    /// Protocols to read off the probed channels, in addition to the project's
    /// `[[decode]]` entries.
    pub decodes: Vec<DecodeSection>,
    /// Serve the GDB remote protocol on one board's chip once it is booted.
    pub gdb: Option<GdbRequest>,
    /// Write the simulated network's traffic here as a pcap at the end of the
    /// run, for Wireshark.
    pub pcap: Option<PathBuf>,
    /// Write every 802.11 frame the radio carries into a link-type-105 pcap
    /// as the run goes.
    pub pcap_air: Option<PathBuf>,
    /// Write the Ethernet frames the station's MAC carries into a
    /// link-type-1 pcap as the run goes.
    pub pcap_eth: Option<PathBuf>,
    /// Put host sockets behind the Wi-Fi gateway. Non-deterministic by
    /// nature; the forwarded ports are `tcp:[host:]port-[guest:]port`.
    pub host_network: bool,
    pub host_forwards: Vec<String>,
    /// Ask the firmware for its free heap at the end of the run and print it.
    /// A firmware that cannot answer makes the run fail, so CI can gate on it.
    pub heap_sample: bool,
    /// Sample the free heap every this many nanoseconds of simulated time.
    /// `None` keeps the end-of-run sample that `heap_sample` asks for.
    pub heap_interval: Option<u64>,
    /// Fail the run when the free heap drops below this many bytes.
    pub heap_min_free: Option<u32>,
    /// Append every heap sample to this CSV as it is taken.
    pub heap_csv: Option<PathBuf>,
    /// Save the image of every SPI microSD card the guest wrote, after the
    /// run. One card writes to the path; several treat it as a directory.
    pub sd_export: Option<PathBuf>,
    /// Save the MCU's own flash after the run, so a firmware that wrote a
    /// config sector or a filesystem hands it to the next run.
    pub flash_export: Option<PathBuf>,
}

/// `--interactive[=BOARD]`: whose console receives what is typed.
#[derive(Clone, Debug, Default)]
pub struct Interactive {
    /// The board whose console stdin reaches; `None` means the first board.
    pub board: Option<String>,
}

/// `--screenshot [<board>@]FILE`: which display to capture, and where.
#[derive(Clone, Debug)]
pub struct Screenshot {
    /// The board whose display to capture; `None` means the first board that
    /// has a framebuffer.
    pub board: Option<String>,
    pub path: PathBuf,
}

/// Heap samples taken while the run proceeds.
///
/// The firmware's own getter is the source, so the numbers are the ones the
/// firmware sees. Sampling happens at chunk boundaries, which is where the
/// core is between instructions; a sample never changes the program's state
/// beyond the simulated time the call itself spends.
struct HeapLog {
    label: String,
    interval_ns: u64,
    next_ns: u64,
    limit: Option<u32>,
    csv: Option<(PathBuf, std::io::BufWriter<std::fs::File>)>,
    samples: Vec<(u64, u32)>,
    lowest: Option<u32>,
    failure: Option<String>,
    last_error: Option<String>,
}

impl HeapLog {
    fn from_options(options: &Options, label: String) -> Result<Option<Self>, String> {
        let enabled = options.heap_sample
            || options.heap_interval.is_some()
            || options.heap_min_free.is_some()
            || options.heap_csv.is_some();
        if !enabled {
            return Ok(None);
        }
        let interval_ns = options.heap_interval.unwrap_or(1_000_000_000).max(1);
        let csv = options
            .heap_csv
            .as_ref()
            .map(|path| {
                let file =
                    std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
                let mut writer = std::io::BufWriter::new(file);
                writer
                    .write_all(b"part,time_s,free,min_free\n")
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Ok::<(PathBuf, std::io::BufWriter<std::fs::File>), String>((path.clone(), writer))
            })
            .transpose()?;
        Ok(Some(Self {
            label,
            interval_ns,
            next_ns: 0,
            limit: options.heap_min_free,
            csv,
            samples: Vec::new(),
            lowest: None,
            failure: None,
            last_error: None,
        }))
    }

    /// Take a sample when the interval has elapsed. Sampling more than once
    /// per chunk is pointless: the shape is bounded by the chunk size, not by
    /// how many times the loop asks.
    fn sample(&mut self, mcu: &mut Fleet, now_ns: u64) {
        if now_ns < self.next_ns || self.failure.is_some() {
            return;
        }
        while self.next_ns <= now_ns {
            self.next_ns = self.next_ns.saturating_add(self.interval_ns);
        }
        self.record(mcu, now_ns);
    }

    /// Record one sample now, regardless of the interval, for the run's end.
    fn record(&mut self, mcu: &mut Fleet, now_ns: u64) {
        let free = match mcu.sample_heap() {
            Ok(free) => free,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        self.samples.push((now_ns, free));
        self.lowest = Some(self.lowest.map_or(free, |lowest| lowest.min(free)));
        let lowest = self.lowest.unwrap_or(free);
        if let Some((path, writer)) = &mut self.csv {
            let line = format!(
                "{},{:.3},{},{}\n",
                csv_field(&self.label),
                now_ns as f64 / 1e9,
                free,
                lowest
            );
            if let Err(error) = writer.write_all(line.as_bytes()) {
                self.failure = Some(format!("{}: {error}", path.display()));
            }
        }
        if let Some(limit) = self.limit
            && free < limit
        {
            self.failure = Some(format!(
                "free heap {free} dropped below {limit} bytes at {:.3}s",
                now_ns as f64 / 1e9
            ));
        }
    }

    /// The report lines, the failure that should turn the run red, and the
    /// samples themselves.
    fn finish(mut self, mcu: &mut Fleet, now_ns: u64) -> (Vec<String>, Option<String>) {
        // The end of the run is a sample point in its own right: a leak that
        // only shows after the last interval must not be missed.
        if !self.samples.iter().any(|(at, _)| *at == now_ns) {
            self.record(mcu, now_ns);
        }
        let mut lines = Vec::new();
        if self.samples.is_empty() {
            let why = self
                .last_error
                .unwrap_or_else(|| "the firmware never answered".into());
            let message = format!("no samples: {why}");
            // A bare `--heap` is informational, as Wokwi's is; a minimum is a
            // gate, and a gate that cannot read the heap must fail.
            return if self.limit.is_some() {
                (Vec::new(), Some(message))
            } else {
                (vec![message], None)
            };
        }
        let first = self.samples[0];
        let last = *self.samples.last().expect("checked non-empty");
        let mut last_free = None;
        for (at, free) in &self.samples {
            if last_free == Some(*free) {
                continue;
            }
            last_free = Some(*free);
            lines.push(format!(
                "[heap {}] {:.3}s free={free} min={}",
                self.label,
                *at as f64 / 1e9,
                self.lowest.unwrap_or(*free)
            ));
        }
        let lost = first.1.saturating_sub(last.1);
        let seconds = (last.0.saturating_sub(first.0)) as f64 / 1e9;
        let rate = if seconds > 0.0 {
            lost as f64 / seconds
        } else {
            0.0
        };
        lines.push(format!(
            "Heap: {} samples, free {} -> {}, lowest {}, {:.0} bytes/s lost",
            self.samples.len(),
            first.1,
            last.1,
            self.lowest.unwrap_or(last.1),
            rate
        ));
        (lines, self.failure)
    }
}

/// A CSV field is quoted only when it contains a separator or a quote; board
/// ids are simple, but a project may name one anything.
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

impl HeapLog {
    /// The failure so far, for the loop to stop early when the limit is hit.
    fn failed(&self) -> bool {
        self.failure.is_some()
    }
}

/// Which board to debug, and where to listen for the client.
///
/// A GDB session owns the chip's clock for as long as the client is attached,
/// so one session is served at a time: `board` says which chip, and its
/// absence picks the first board whose backend has a debug target.
#[derive(Clone, Debug)]
pub struct GdbRequest {
    pub board: Option<String>,
    pub port: u16,
}

pub struct Outcome {
    pub exit_code: i32,
    pub output: String,
    /// Simulated milliseconds the run reached, for a report that wants to say
    /// how much of the timeline a passing test actually looked at.
    pub sim_ms: u32,
    /// One entry per `[[test]]`; empty for `run` and `check`.
    pub tests: Vec<TestOutcome>,
}

pub struct TestOutcome {
    pub name: String,
    pub passed: bool,
    pub exit_code: i32,
    pub output: String,
    pub sim_ms: u32,
}

/// A run whose console was captured instead of streamed.
///
/// `run` writes bytes to stdout as they arrive because a person is watching a
/// terminal. A host that owns stdout — the MCP server is one — cannot allow
/// that, so this path collects the same serial text and the part states the
/// UI would read back.
pub struct CaptureOutcome {
    pub exit_code: i32,
    pub output: String,
    pub sim_ms: u32,
    /// Parts whose state is not [`litz_core::PartState::None`], by id.
    pub part_states: BTreeMap<String, PartState>,
    /// Electrical faults the board found, in net order.
    pub faults: Vec<String>,
}

struct Prepared {
    config: Config,
    dir: PathBuf,
    registry: Registry,
    circuit: Circuit,
    /// One entry per development board in diagram order; each boots its own
    /// image onto its own chip pins.
    boards: Vec<BoardPlan>,
}

/// Everything one development board boots and asserts against.
struct BoardPlan {
    /// The board instance's id in `diagram.json`.
    id: String,
    /// The board's own manifest: which MCU it names, and where its files are.
    manifest: litz_catalog::PartManifest,
    flash_size: usize,
    rom: Vec<u8>,
    firmware: Vec<u8>,
    /// The firmware's symbol table, when the project names one.
    elf: Option<Vec<u8>>,
    /// Whether `firmware` holds a whole flash image or only an application.
    layout: FirmwareLayout,
    bootloader: Option<Vec<u8>>,
    partition_table: Option<Vec<u8>>,
    /// The runtime this board's console speaks, and the Python pasted into it.
    /// The first staged file is the entry the REPL executes; the rest are
    /// written to the device first so the entry can import them.
    runtime: Runtime,
    /// Which Python the console speaks, resolved from the runtime and the
    /// image name: CircuitPython boots `code.py`, MicroPython `main.py`.
    python: crate::python::PythonRuntime,
    scripts: Vec<StagedScript>,
    mac: [u8; 6],
}

/// One Python file a MicroPython project stages on the device before it runs.
struct StagedScript {
    /// The path on the device, relative to the REPL's working directory.
    name: String,
    bytes: Vec<u8>,
}

pub fn execute(dir: &Path, mode: Mode, strict: bool, update_lock: bool) -> Result<Outcome, String> {
    execute_with(dir, mode, strict, update_lock, &Options::default())
}

/// `execute` with recording options: a VCD file and a breakpoint.
pub fn execute_with(
    dir: &Path,
    mode: Mode,
    strict: bool,
    update_lock: bool,
    options: &Options,
) -> Result<Outcome, String> {
    let config = match Config::load_project(dir) {
        Ok(config) => config,
        Err(e) if mode == Mode::Check => {
            let _ = e;
            Config::default()
        }
        Err(e) => return Err(e),
    };

    let mut registry = Registry::builtin();
    for sub in ["parts", "boards"] {
        let path = dir.join(sub);
        if path.is_dir() {
            registry.load_dir(&path).map_err(|e| e.to_string())?;
        }
    }

    let diagram_path = dir.join(config.project.diagram());
    let text = std::fs::read_to_string(&diagram_path)
        .map_err(|e| format!("{}: {e}", diagram_path.display()))?;
    let circuit =
        Circuit::from_json(&text).map_err(|e| format!("{}: {e}", diagram_path.display()))?;

    let diagnostics = circuit.validate(&registry);
    if blocks(&diagnostics, strict) {
        return Ok(Outcome {
            exit_code: 1,
            output: render(&diagnostics),
            sim_ms: 0,
            tests: Vec::new(),
        });
    }

    if mode == Mode::Check {
        let mut output = render(&diagnostics);
        if output.is_empty() {
            output.push_str("diagram is valid\n");
        }
        return Ok(Outcome {
            exit_code: 0,
            output,
            sim_ms: 0,
            tests: Vec::new(),
        });
    }

    let prepared = prepare(config, dir, registry, circuit)?;
    enforce_lock(&prepared, update_lock)?;

    match mode {
        Mode::Run => run_once(&prepared, None, strict, options),
        Mode::Test => {
            let specs = if prepared.config.test.is_empty() {
                vec![TestSpec {
                    name: "run".into(),
                    timeout: None,
                    expect: None,
                    fail: None,
                    input: Vec::new(),
                    expect_state: None,
                    state_at: None,
                    state_stays: None,
                }]
            } else {
                prepared.config.test.clone()
            };
            let mut output = String::new();
            let mut tests = Vec::new();
            let mut failed = 0;
            let mut sim_ms = 0;
            for spec in &specs {
                let outcome = run_once(&prepared, Some(spec), strict, options)?;
                output.push_str(&format!("== {} ==\n", spec.name));
                output.push_str(&outcome.output);
                if !outcome.output.ends_with('\n') {
                    output.push('\n');
                }
                if outcome.exit_code != 0 {
                    failed += 1;
                }
                sim_ms = sim_ms.max(outcome.sim_ms);
                tests.push(TestOutcome {
                    name: spec.name.clone(),
                    passed: outcome.exit_code == 0,
                    exit_code: outcome.exit_code,
                    output: outcome.output,
                    sim_ms: outcome.sim_ms,
                });
            }
            Ok(Outcome {
                exit_code: if failed == 0 { 0 } else { 1 },
                output: format!(
                    "{output}{}/{} tests passed\n",
                    specs.len() - failed,
                    specs.len()
                ),
                sim_ms,
                tests,
            })
        }
        Mode::Check => unreachable!(),
    }
}

/// Boot a project for bounded simulated time, capturing everything.
///
/// This is `run` without the terminal: the serial stream is returned rather
/// than printed, and the part states are read back when the time limit is
/// reached. A normal bounded run still exits 42, matching the CLI.
pub fn execute_capture(
    dir: &Path,
    timeout: Option<&str>,
    strict: bool,
    update_lock: bool,
    options: &Options,
) -> Result<CaptureOutcome, String> {
    let config = Config::load_project(dir)?;
    let mut registry = Registry::builtin();
    for sub in ["parts", "boards"] {
        let path = dir.join(sub);
        if path.is_dir() {
            registry.load_dir(&path).map_err(|e| e.to_string())?;
        }
    }

    let diagram_path = dir.join(config.project.diagram());
    let text = std::fs::read_to_string(&diagram_path)
        .map_err(|e| format!("{}: {e}", diagram_path.display()))?;
    let circuit =
        Circuit::from_json(&text).map_err(|e| format!("{}: {e}", diagram_path.display()))?;

    let diagnostics = circuit.validate(&registry);
    if blocks(&diagnostics, strict) {
        return Ok(CaptureOutcome {
            exit_code: 1,
            output: render(&diagnostics),
            sim_ms: 0,
            part_states: BTreeMap::new(),
            faults: Vec::new(),
        });
    }

    let prepared = prepare(config, dir, registry, circuit)?;
    enforce_lock(&prepared, update_lock)?;
    run_capture(&prepared, timeout, options)
}

/// Run a scenario (`--scenario`) instead of the `[[test]]` entries.
///
/// The steps share the boot path with a test and differ only in who decides
/// when to look at the output: a test polls for its own expectations, a
/// scenario says what it is waiting for.
pub fn execute_scenario(
    dir: &Path,
    file: &Path,
    strict: bool,
    update_lock: bool,
) -> Result<Outcome, String> {
    execute_scenario_with(dir, file, strict, update_lock, &Options::default())
}

/// `execute_scenario` with recording options.
pub fn execute_scenario_with(
    dir: &Path,
    file: &Path,
    strict: bool,
    update_lock: bool,
    options: &Options,
) -> Result<Outcome, String> {
    let config = Config::load_project(dir)?;
    let mut registry = Registry::builtin();
    for sub in ["parts", "boards"] {
        let path = dir.join(sub);
        if path.is_dir() {
            registry.load_dir(&path).map_err(|e| e.to_string())?;
        }
    }
    let diagram_path = dir.join(config.project.diagram());
    let text = std::fs::read_to_string(&diagram_path)
        .map_err(|e| format!("{}: {e}", diagram_path.display()))?;
    let circuit =
        Circuit::from_json(&text).map_err(|e| format!("{}: {e}", diagram_path.display()))?;

    let diagnostics = circuit.validate(&registry);
    if blocks(&diagnostics, strict) {
        return Ok(Outcome {
            exit_code: 1,
            output: render(&diagnostics),
            sim_ms: 0,
            tests: Vec::new(),
        });
    }

    let prepared = prepare(config, dir, registry, circuit)?;
    enforce_lock(&prepared, update_lock)?;
    let scenario = Scenario::load(file)?;
    run_scenario(&prepared, &scenario, options)
}

/// How long `wait-serial` waits when the scenario does not say.
const DEFAULT_WAIT: u64 = 10_000_000_000;

fn run_scenario(
    prepared: &Prepared,
    scenario: &Scenario,
    options: &Options,
) -> Result<Outcome, String> {
    let Booted {
        fleet: mut mcu,
        board,
        capture,
        ..
    } = boot(prepared, options)?;
    start_recording(&mut mcu, options, prepared);
    let mut heap = HeapLog::from_options(
        options,
        prepared
            .boards
            .first()
            .map(|plan| plan.id.clone())
            .unwrap_or_else(|| "esp".into()),
    )?;
    let mut serial = String::new();
    let mut notes = Vec::new();
    let mut failed = false;
    // A tap with `wait: false` releases in the background, so the next step
    // does not jump the simulated clock by the hold time.
    let mut last_touch: Vec<Option<(u16, u16)>> = vec![None; prepared.boards.len()];
    let mut pending_touch: Option<(u64, u16, u16, Option<usize>)> = None;

    // A scenario observes the same console a test does, so MicroPython starts
    // the same way: wait for the prompt, then paste. A scenario has no
    // `[[test]]` timeout, so the default wait bounds the boot.
    for (index, plan) in prepared.boards.iter().enumerate() {
        if !pastes_script(plan) {
            continue;
        }
        // CircuitPython runs `code.py` on boot, so its console is busy until
        // Ctrl-C reaches it; MicroPython has nothing to interrupt.
        if !plan.python.interrupt_sequence().is_empty() {
            mcu.write_console_on(index, plan.python.interrupt_sequence());
            advance(&mut mcu, &mut serial);
        }
        let deadline = now_ns(&mcu).saturating_add(DEFAULT_WAIT);
        while !mcu.console_text(index).contains(REPL_PROMPT) {
            if now_ns(&mcu) >= deadline || !advance(&mut mcu, &mut serial) {
                break;
            }
        }
        if !mcu.console_text(index).contains(REPL_PROMPT) {
            return Err(format!(
                "board {:?}: MicroPython REPL prompt did not appear within {DEFAULT_WAIT} ns of \
                 simulated time; check that its firmware is a MicroPython or CircuitPython build",
                plan.id
            ));
        }
        paste_script(&mut mcu, index, &plan.scripts, plan.python);
    }

    for (index, step) in scenario.steps.iter().enumerate() {
        let number = index + 1;
        {
            let now = now_ns(&mcu);
            service_touch_release(&mut mcu, &mut pending_touch, now);
        }
        match step {
            Step::Delay { ns } => {
                let target = now_ns(&mcu).saturating_add(*ns);
                while now_ns(&mcu) < target && advance(&mut mcu, &mut serial) {
                    {
                        let now = now_ns(&mcu);
                        service_touch_release(&mut mcu, &mut pending_touch, now);
                    }
                    if let Some(log) = &mut heap {
                        {
                            let now = now_ns(&mcu);
                            log.sample(&mut mcu, now);
                        }
                        if log.failed() {
                            break;
                        }
                    }
                }
            }
            Step::WaitSerial { text, timeout } => {
                let deadline = now_ns(&mcu).saturating_add(timeout.unwrap_or(DEFAULT_WAIT));
                while !serial.contains(text.as_str()) {
                    if now_ns(&mcu) >= deadline {
                        failed = true;
                        notes.push(format!("step {number}: never saw {text:?}"));
                        break;
                    }
                    if !advance(&mut mcu, &mut serial) {
                        break;
                    }
                    {
                        let now = now_ns(&mcu);
                        service_touch_release(&mut mcu, &mut pending_touch, now);
                    }
                    if let Some(log) = &mut heap {
                        {
                            let now = now_ns(&mcu);
                            log.sample(&mut mcu, now);
                        }
                        if log.failed() {
                            break;
                        }
                    }
                }
            }
            Step::WriteSerial { text } => {
                // `write-serial` sends the text as written; it is the
                // author's job to include the newline the firmware expects.
                mcu.write_console_on(0, text.as_bytes());
                advance(&mut mcu, &mut serial);
            }
            Step::WaitPin {
                part,
                pin,
                value,
                timeout,
            } => {
                let deadline = now_ns(&mcu).saturating_add(timeout.unwrap_or(DEFAULT_WAIT));
                loop {
                    let level = pin_level(prepared, &mcu, part, pin.as_deref());
                    if level.is_some_and(|level| (level == Level::High) == *value) {
                        break;
                    }
                    if now_ns(&mcu) >= deadline {
                        failed = true;
                        notes.push(format!(
                            "step {number}: {part} never reached {} within {} ns",
                            if *value { "high" } else { "low" },
                            timeout.unwrap_or(DEFAULT_WAIT)
                        ));
                        break;
                    }
                    if !advance(&mut mcu, &mut serial) {
                        break;
                    }
                    {
                        let now = now_ns(&mcu);
                        service_touch_release(&mut mcu, &mut pending_touch, now);
                    }
                }
            }
            Step::ExpectPin { part, pin, value } => {
                match pin_level(prepared, &mcu, part, pin.as_deref()) {
                    Some(level) => {
                        let high = level == Level::High;
                        if high != *value {
                            failed = true;
                            notes.push(format!(
                                "step {number}: {part} is {} but the scenario expects {}",
                                if high { "high" } else { "low" },
                                if *value { "high" } else { "low" }
                            ));
                        }
                    }
                    None => {
                        failed = true;
                        notes.push(format!(
                            "step {number}: no part {part:?} is wired to a chip pin"
                        ));
                    }
                }
            }
            Step::SetControl {
                part,
                control,
                value,
            } => {
                if mcu.set_named_control(part, control, value) {
                    // The part understood the control's own name and unit.
                } else {
                    match control.as_str() {
                        // The potentiometer's control is named `value` and
                        // measured in percent of travel.
                        "value" | "position" => match value.trim().parse::<f64>() {
                            Ok(percent) if (0.0..=100.0).contains(&percent) => {
                                let permille = (percent * 10.0).round() as u16;
                                if !mcu.set_control(part, permille) {
                                    failed = true;
                                    notes.push(format!(
                                        "step {number}: part {part:?} has no continuous control"
                                    ));
                                }
                            }
                            _ => {
                                failed = true;
                                notes.push(format!(
                                "step {number}: set-control value {value:?} is not 0 through 100"
                            ));
                            }
                        },
                        other => {
                            failed = true;
                            notes.push(format!(
                                "step {number}: part {part:?} has no control {other:?}"
                            ));
                        }
                    }
                }
            }
            Step::TakeScreenshot {
                part,
                path,
                board: which,
                expected,
                tolerance,
            } => {
                let index = match which {
                    Some(id) => Some(find_board(&prepared.boards, id, "take-screenshot")?),
                    None => None,
                };
                // A capture with only `compare-with` still needs a file to
                // write; it lands beside the reference so a failing CI job can
                // show both pictures.
                let path = match (path, expected) {
                    (Some(path), _) => path.clone(),
                    (None, Some(expected)) => expected.with_extension("actual.png"),
                    (None, None) => unreachable!("the parser requires one of them"),
                };
                let capture = match part {
                    Some(part) if prepared.boards.iter().any(|plan| plan.id == *part) => {
                        let board = find_board(&prepared.boards, part, "take-screenshot")?;
                        mcu.save_display(Some(board), &path)
                    }
                    Some(part) => mcu.save_part_display(part, &path),
                    None => mcu.save_display(index, &path),
                };
                match capture {
                    Ok(()) => {
                        if let Some(expected) = expected {
                            match compare_images(&path, expected, *tolerance) {
                                Ok(()) => {}
                                Err(error) => {
                                    failed = true;
                                    notes.push(format!("step {number}: {error}"));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        failed = true;
                        notes.push(format!("step {number}: {error}"));
                    }
                }
            }
            Step::Touch {
                x,
                y,
                down,
                board: which,
                part,
            } => {
                let index = touch_board(prepared, which, part, "touch")?;
                if let Err(error) = mcu.touch(index, *x, *y, *down) {
                    failed = true;
                    notes.push(format!("step {number}: {error}"));
                } else if *down {
                    let slot = index.unwrap_or(0);
                    if let Some(slot) = last_touch.get_mut(slot) {
                        *slot = Some((*x, *y));
                    }
                }
            }
            Step::TouchMove {
                x,
                y,
                board: which,
                part,
            } => {
                let index = touch_board(prepared, which, part, "touch-move")?;
                if let Err(error) = mcu.touch(index, *x, *y, true) {
                    failed = true;
                    notes.push(format!("step {number}: {error}"));
                } else {
                    let slot = index.unwrap_or(0);
                    if let Some(slot) = last_touch.get_mut(slot) {
                        *slot = Some((*x, *y));
                    }
                }
            }
            Step::TouchTap {
                x,
                y,
                duration,
                wait,
                board: which,
                part,
            } => {
                let index = touch_board(prepared, which, part, "touch")?;
                if let Err(error) = mcu.touch(index, *x, *y, true) {
                    failed = true;
                    notes.push(format!("step {number}: {error}"));
                } else {
                    let slot = index.unwrap_or(0);
                    if let Some(slot) = last_touch.get_mut(slot) {
                        *slot = Some((*x, *y));
                    }
                    let release_at = now_ns(&mcu).saturating_add(*duration);
                    if *wait {
                        while now_ns(&mcu) < release_at && advance(&mut mcu, &mut serial) {
                            {
                                let now = now_ns(&mcu);
                                service_touch_release(&mut mcu, &mut pending_touch, now);
                            }
                        }
                        let _ = mcu.touch(index, *x, *y, false);
                    } else {
                        pending_touch = Some((release_at, *x, *y, index));
                    }
                }
            }
            Step::TouchRelease {
                x,
                y,
                board: which,
                part,
            } => {
                let index = touch_board(prepared, which, part, "touch-release")?;
                let previous = last_touch.get(index.unwrap_or(0)).copied().flatten();
                let (x, y) = match (x, y) {
                    (Some(x), Some(y)) => (*x, *y),
                    _ => previous.unwrap_or((0, 0)),
                };
                pending_touch = None;
                if let Err(error) = mcu.touch(index, x, y, false) {
                    failed = true;
                    notes.push(format!("step {number}: {error}"));
                }
            }
        }
        if let Some(log) = &mut heap {
            {
                let now = now_ns(&mcu);
                log.sample(&mut mcu, now);
            }
        }
        let conflicts = mcu.conflicts();
        if !conflicts.is_empty() {
            for conflict in &conflicts {
                notes.push(format!(
                    "electrical fault on net {}: {}",
                    conflict.net, conflict.detail
                ));
            }
            failed = true;
            break;
        }
    }

    let mut output = serial;
    if !output.ends_with('\n') && !output.is_empty() {
        output.push('\n');
    }
    for note in notes {
        output.push_str(&format!("litz: {note}\n"));
    }
    for line in probe_report(&board) {
        output.push_str(&format!("litz: {line}\n"));
    }
    for line in decode_report(&board, prepared, options)? {
        output.push_str(&format!("litz: {line}\n"));
    }
    finish_recording(&mcu, &board, options, capture.as_ref())?;
    if let Some(log) = heap {
        let end = now_ns(&mcu);
        let (lines, failure) = log.finish(&mut mcu, end);
        for line in lines {
            output.push_str(&format!("litz: {line}\n"));
        }
        if let Some(failure) = failure {
            failed = true;
            output.push_str(&format!("litz: heap: {failure}\n"));
        }
    }
    Ok(Outcome {
        exit_code: if failed { 1 } else { 0 },
        output,
        sim_ms: (mcu.seconds() * 1000.0) as u32,
        tests: Vec::new(),
    })
}

/// A scenario's pin reference, resolved to a level: `part-id` naming a
/// development board with a GPIO number (`esp`/`2`), an explicit pin name
/// (`name`), `part:pin` in the part id, or the chip pin the part is on.
fn pin_level(prepared: &Prepared, mcu: &Fleet, target: &str, pin: Option<&str>) -> Option<Level> {
    if let Some(index) = prepared.boards.iter().position(|plan| plan.id == target)
        && let Some(gpio) = pin.and_then(parse_gpio)
    {
        return mcu.gpio_level(index, gpio);
    }
    match (target.split_once(':'), pin) {
        (Some((part, pin)), _) => mcu.part_pin_level(part, Some(pin)),
        (None, Some(pin)) => mcu.part_pin_level(target, Some(pin)),
        (None, None) => mcu.part_pin_level(target, None),
    }
}

/// `2`, `GPIO2` or `D2` as a chip pad number.
fn parse_gpio(text: &str) -> Option<u8> {
    let text = text.trim();
    let text = text
        .strip_prefix("GPIO")
        .or_else(|| text.strip_prefix("gpio"))
        .or_else(|| text.strip_prefix('D'))
        .or_else(|| text.strip_prefix('d'))
        .unwrap_or(text);
    text.parse().ok()
}

fn now_ns(fleet: &Fleet) -> u64 {
    fleet.frontier_ns()
}

/// Release a tap whose hold time has elapsed, so `wait: false` does not stop
/// the scenario for the duration of the touch.
fn service_touch_release(
    mcu: &mut Fleet,
    pending: &mut Option<(u64, u16, u16, Option<usize>)>,
    now_ns: u64,
) {
    if let Some((at, x, y, index)) = *pending
        && now_ns >= at
    {
        let _ = mcu.touch(index, x, y, false);
        *pending = None;
    }
}

/// Compare a captured image with a golden one, per pixel, so a report names a
/// coordinate rather than a byte offset inside a compressed stream. The
/// tolerance is per colour channel: 0 demands the same pixels exactly.
fn compare_images(actual: &Path, expected: &Path, tolerance: u8) -> Result<(), String> {
    let actual_image = decode_png(actual)?;
    let expected_image = decode_png(expected)?;
    if actual_image.0 != expected_image.0 || actual_image.1 != expected_image.1 {
        return Err(format!(
            "screenshot {} is {}x{} but {} is {}x{}",
            actual.display(),
            actual_image.0,
            actual_image.1,
            expected.display(),
            expected_image.0,
            expected_image.1
        ));
    }
    let (width, _, actual_pixels) = actual_image;
    let (_, _, expected_pixels) = expected_image;
    let mut mismatched = 0usize;
    let mut first = None;
    for (index, (a, b)) in actual_pixels.iter().zip(&expected_pixels).enumerate() {
        let differs = a.iter().zip(b).any(|(a, b)| a.abs_diff(*b) > tolerance);
        if differs {
            mismatched += 1;
            first.get_or_insert(index);
        }
    }
    if let Some(index) = first {
        let x = index as u32 % width;
        let y = index as u32 / width;
        return Err(format!(
            "screenshot {} differs from {} at ({x}, {y}) in {width}x{} pixels; \
             {mismatched} pixels differ",
            actual.display(),
            expected.display(),
            actual_pixels.len() as u32 / width.max(1)
        ));
    }
    Ok(())
}

/// Decode a PNG to RGBA8. A reference image may use any colour type; the
/// comparison runs on the same pixels a display would show.
fn decode_png(path: &Path) -> Result<(u32, u32, Vec<[u8; 4]>), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| format!("{}: the image is too large to decode", path.display()))?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let pixels = match info.color_type {
        png::ColorType::Rgba => buffer.as_chunks::<4>().0.to_vec(),
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buffer.iter().map(|p| [*p, *p, *p, 255]).collect(),
        png::ColorType::Indexed => {
            return Err(format!(
                "{}: an indexed PNG did not expand to RGB",
                path.display()
            ));
        }
    };
    Ok((info.width, info.height, pixels))
}

/// Arm the breakpoint and the waveform before the first instruction runs, so
/// time 0 of the trace is the board as the chip first sees it.
fn start_recording(mcu: &mut Fleet, options: &Options, prepared: &Prepared) {
    if let Some(pc) = options.break_at {
        mcu.break_at(pc);
    }
    // A probe's record is the net trace, so asking for one starts the same
    // recording a VCD does.
    let wants_trace = options.vcd.is_some()
        || options.scope.is_some()
        || options.sr.is_some()
        || !prepared.config.probes.is_empty()
        || !prepared.config.decodes.is_empty()
        || !options.probes.is_empty()
        || !options.decodes.is_empty();
    if wants_trace {
        mcu.start_trace();
    }
}

fn finish_recording(
    mcu: &Fleet,
    board: &std::rc::Rc<std::cell::RefCell<Board>>,
    options: &Options,
    capture: Option<&std::rc::Rc<std::cell::RefCell<litz_net::capture::Capture>>>,
) -> Result<(), String> {
    if let (Some(path), Some(capture)) = (&options.pcap, capture) {
        std::fs::write(path, capture.borrow().to_pcap())
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(path) = &options.vcd {
        std::fs::write(path, mcu.vcd()).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(path) = &options.sd_export {
        board.borrow_mut().save_sd_images(path)?;
    }
    board.borrow_mut().save_storage_images()?;
    if let Some(path) = &options.flash_export {
        let image = mcu.flash_image().ok_or_else(|| {
            format!(
                "{}: this board's chip has no readable flash",
                path.display()
            )
        })?;
        std::fs::write(path, image).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(path) = &options.scope {
        let channels = board.borrow().scope();
        let csv = match &options.trigger {
            Some(trigger) => {
                let clipped = probe::triggered(&channels, trigger)?;
                probe::to_csv(&clipped)
            }
            None => probe::to_csv(&channels),
        };
        std::fs::write(path, csv).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(path) = &options.sr {
        let channels = board.borrow().scope();
        let clipped = match &options.trigger {
            Some(trigger) => probe::triggered(&channels, trigger)?,
            None => channels,
        };
        let rate = options
            .sr_rate
            .unwrap_or(litz_board::sigrok::DEFAULT_RATE_HZ);
        let archive = litz_board::sigrok::to_sr(&clipped, rate)?;
        std::fs::write(path, archive).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// One summary line per probe, for a run that wants to say what the wires did.
fn probe_report(board: &std::rc::Rc<std::cell::RefCell<Board>>) -> Vec<String> {
    board.borrow().scope().iter().map(probe::summary).collect()
}

/// One line per frame of every configured decoder, project entries first.
fn decode_report(
    board: &std::rc::Rc<std::cell::RefCell<Board>>,
    prepared: &Prepared,
    options: &Options,
) -> Result<Vec<String>, String> {
    let channels = board.borrow().scope();
    crate::decode::report(
        prepared.config.decodes.iter().chain(options.decodes.iter()),
        &channels,
    )
}

/// One chunk of simulated time. False when the board reported a fault.
fn advance(mcu: &mut Fleet, serial: &mut String) -> bool {
    let _ = mcu.run(1_000_000);
    let chunk = mcu.take_console();
    if !chunk.is_empty() {
        serial.push_str(&String::from_utf8_lossy(&chunk));
    }
    mcu.conflicts().is_empty()
}

/// Whether the project asks for a script to be pasted into a Python REPL.
/// `runtime = "micropython"` with no `script` still means "run this firmware",
/// just without the handshake.
fn pastes_script(plan: &BoardPlan) -> bool {
    matches!(plan.runtime, Runtime::Micropython | Runtime::CircuitPython)
        && !plan.scripts.is_empty()
}

/// Read one project file as a Python source to stage.
fn read_staged(dir: &Path, rel: &str) -> Result<StagedScript, String> {
    let path = dir.join(rel);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() > MAX_SCRIPT {
        return Err(format!(
            "{} is {} bytes; a MicroPython raw paste is limited to {MAX_SCRIPT} bytes",
            path.display(),
            bytes.len()
        ));
    }
    Ok(StagedScript {
        name: staged_name(rel)?,
        bytes,
    })
}

/// A stage path must stay inside the REPL's working directory, so a project
/// file cannot write over `/boot.py` or escape with `..`.
fn staged_name(name: &str) -> Result<String, String> {
    let name = name.trim().replace('\\', "/");
    let invalid = name.is_empty()
        || name.starts_with('/')
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if invalid {
        return Err(format!(
            "staged script {name:?} must be a relative path inside the project, \
             like \"main.py\" or \"lib/util.py\""
        ));
    }
    Ok(name)
}

/// Interrupt whatever is running, then send MicroPython's raw-paste sequence:
/// Ctrl-C (0x03), Ctrl-A (0x01) to enter the raw REPL, the script bytes, and
/// Ctrl-D (0x04) to compile and run them. The chip queues the bytes and paces
/// them at the UART's line rate, so the interpreter sees them in this order
/// however small its receive FIFO is.
fn paste_script(
    mcu: &mut Fleet,
    index: usize,
    scripts: &[StagedScript],
    python: crate::python::PythonRuntime,
) {
    // One file is the script itself, exactly as before; several become a
    // bootstrap that writes each file and then executes the entry.
    let body = if scripts.len() == 1 {
        scripts[0].bytes.clone()
    } else {
        let pairs: Vec<(&str, &[u8])> = scripts
            .iter()
            .map(|script| (script.name.as_str(), script.bytes.as_slice()))
            .collect();
        crate::python::stage_program(&scripts[0].name, &pairs).into_bytes()
    };
    let mut bytes = Vec::with_capacity(body.len() + 4);
    // Ctrl-C stops whatever CircuitPython started on boot; MicroPython lands
    // at a prompt already, but the byte is harmless there.
    let interrupt = python.interrupt_sequence();
    if interrupt.is_empty() {
        bytes.push(0x03);
    } else {
        bytes.extend_from_slice(interrupt);
    }
    bytes.extend_from_slice(python.raw_paste_begin());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(python.raw_paste_end());
    // The host side of the UART is a queue; a classic ESP32 with a small
    // receive FIFO drops a burst that is written all at once. Feeding it in
    // small slices and letting the chip run between them keeps the byte
    // stream intact without a wall-clock sleep.
    for chunk in bytes.chunks(16) {
        mcu.write_console_to(index, chunk);
        mcu.run_one(index, 1_000_000);
    }
}

fn prepare(
    config: Config,
    dir: &Path,
    registry: Registry,
    circuit: Circuit,
) -> Result<Prepared, String> {
    // Every development board on the diagram is its own chip with its own
    // image; the diagram is what says how many there are.
    let instances: Vec<litz_diagram::PartInstance> = circuit
        .parts
        .iter()
        .filter(|part| {
            registry
                .get(&part.type_key)
                .and_then(|manifest| manifest.board.as_ref())
                .is_some()
        })
        .cloned()
        .collect();
    if instances.is_empty() {
        return Err("the diagram has no development board".into());
    }

    // `[[board]]` names each instance. A project with one board keeps using
    // `[sim]`; the moment a second board appears there is no sensible default
    // for which image goes where, so both boards must be named.
    let sections: Vec<BoardSection> = if config.boards.is_empty() {
        if instances.len() != 1 {
            return Err(format!(
                "the diagram has {} development boards; give each one a [[board]] entry with its id and firmware",
                instances.len()
            ));
        }
        vec![BoardSection {
            id: instances[0].id.clone(),
            firmware: config.sim.firmware.clone(),
            bootloader: config.sim.bootloader.clone(),
            partition_table: config.sim.partition_table.clone(),
            rom: config.sim.rom.clone(),
            elf: config.sim.elf.clone(),
            runtime: Some(config.sim.runtime),
            script: config.sim.script.clone(),
            scripts: config.sim.scripts.clone(),
            flash_size: config.sim.flash_size.clone(),
            mac: config.sim.mac.clone(),
        }]
    } else {
        if config.sim.firmware.is_some() {
            return Err(
                "a project with [[board]] entries names firmware per board; remove [sim] firmware"
                    .into(),
            );
        }
        let mut sections = Vec::with_capacity(instances.len());
        for instance in &instances {
            let mut found: Vec<&BoardSection> = config
                .boards
                .iter()
                .filter(|section| section.id == instance.id)
                .collect();
            match found.len() {
                1 => sections.push(found.remove(0).clone()),
                0 => {
                    return Err(format!(
                        "board {:?} in diagram.json has no [[board]] entry; add `id = {:?}` to one",
                        instance.id, instance.id
                    ));
                }
                _ => {
                    return Err(format!("[[board]] names {:?} more than once", instance.id));
                }
            }
        }
        if let Some(unknown) = config
            .boards
            .iter()
            .find(|section| !instances.iter().any(|instance| instance.id == section.id))
        {
            return Err(format!(
                "[[board]] id {:?} is not a development board in diagram.json",
                unknown.id
            ));
        }
        sections
    };

    let mut boards = Vec::with_capacity(sections.len());
    for (section, instance) in sections.iter().zip(&instances) {
        boards.push(prepare_board(&config, dir, &registry, instance, section)?);
    }
    Ok(Prepared {
        config,
        dir: dir.to_path_buf(),
        registry,
        circuit,
        boards,
    })
}

/// Read one board's images and work out how they boot.
fn prepare_board(
    config: &Config,
    dir: &Path,
    registry: &Registry,
    instance: &litz_diagram::PartInstance,
    section: &BoardSection,
) -> Result<BoardPlan, String> {
    let manifest = registry
        .get(&instance.type_key)
        .expect("the caller filtered on boards")
        .clone();
    let info = manifest
        .board
        .as_ref()
        .expect("the caller filtered on boards");
    let chip = info.chip.as_deref();
    // A builtin chip is one the adapters carry; anything else must be a
    // `[board.simulator]` module, which brings its own boot code.
    if let Some(name) = chip
        && !is_builtin_chip(name)
    {
        return Err(format!(
            "board {:?}: chip {name:?} is not built in; name a wasm component under \
             [board.simulator] instead",
            instance.id
        ));
    }

    let flash_size = section
        .flash_size
        .as_deref()
        .or(config.sim.flash_size.as_deref())
        .map(parse_size)
        .transpose()?
        .or_else(|| info.flash_size.as_deref().and_then(|s| parse_size(s).ok()))
        .unwrap_or(4 * 1024 * 1024);

    // A wasm simulator carries its own boot code and may ship a program in
    // flash, so a project can run it with no firmware at all. A built-in chip
    // needs an image: there is nothing in the adapter to execute otherwise.
    let (firmware, layout) = match section.firmware.as_deref() {
        Some(rel) => {
            let path = dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let layout = image::detect(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            (bytes, layout)
        }
        None if info.simulator.is_some() => (Vec::new(), FirmwareLayout::Application),
        None => {
            return Err(format!(
                "board {:?} has no firmware; give it `firmware = \"...\"`",
                instance.id
            ));
        }
    };

    let bootloader_rel = section
        .bootloader
        .as_deref()
        .or(config.sim.bootloader.as_deref());
    let ptable_rel = section
        .partition_table
        .as_deref()
        .or(config.sim.partition_table.as_deref());
    // A merged image already carries the bootloader and partition table; the
    // separate files would be written a second time, at different offsets.
    if layout == FirmwareLayout::Merged && (bootloader_rel.is_some() || ptable_rel.is_some()) {
        return Err(format!(
            "board {:?}: the firmware is a merged image (bootloader at 0x0, \
             partition table at 0x8000, application at 0x10000); remove \
             `bootloader` and `partition_table`",
            instance.id
        ));
    }
    let bootloader = read_optional(dir, bootloader_rel)?;
    let partition_table = read_optional(dir, ptable_rel)?;

    let elf_rel = section.elf.as_deref().or(config.sim.elf.as_deref());
    let elf = match elf_rel {
        Some(rel) => {
            let path = dir.join(rel);
            Some(std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?)
        }
        None => None,
    };

    let runtime = section.runtime.unwrap_or(config.sim.runtime);
    let script_rel = section.script.as_deref().or(config.sim.script.as_deref());
    let staged = if section.scripts.is_empty() {
        &config.sim.scripts
    } else {
        &section.scripts
    };
    let scripts = match runtime {
        Runtime::Micropython | Runtime::CircuitPython => {
            let mut scripts = Vec::new();
            if let Some(rel) = script_rel {
                scripts.push(read_staged(dir, rel)?);
            }
            for spec in staged {
                // `name = path` stages a project file under a different name;
                // a bare path keeps its own relative path on the device.
                let (name, rel) = match spec.split_once('=') {
                    Some((name, path)) => (name.trim(), path.trim()),
                    None => (spec.trim(), spec.trim()),
                };
                if scripts.iter().any(|script| script.name == name) {
                    return Err(format!(
                        "board {:?}: two scripts would be staged as {name:?}",
                        instance.id
                    ));
                }
                let mut script = read_staged(dir, rel)?;
                script.name = staged_name(name)?;
                scripts.push(script);
            }
            let total: usize = scripts.iter().map(|script| script.bytes.len()).sum();
            if total > MAX_SCRIPT {
                return Err(format!(
                    "board {:?}: the staged scripts total {total} bytes; a Python raw \
                     paste is limited to {MAX_SCRIPT} bytes",
                    instance.id
                ));
            }
            // CircuitPython boots `code.py` (or `main.py`); whichever the
            // project staged as its entry runs first, whatever order it was
            // written in.
            if runtime == Runtime::CircuitPython {
                if let Some(entry) = scripts
                    .iter()
                    .position(|script| matches!(script.name.as_str(), "code.py" | "main.py"))
                {
                    let entry = scripts.remove(entry);
                    scripts.insert(0, entry);
                } else {
                    return Err(format!(
                        "board {:?}: CircuitPython needs code.py or main.py among its scripts",
                        instance.id
                    ));
                }
            }
            scripts
        }
        Runtime::Native => {
            if script_rel.is_some() || !staged.is_empty() {
                return Err(format!(
                    "board {:?}: script/scripts need runtime = \"micropython\" or \
                     \"circuitpython\"; a native firmware has no REPL to paste them into",
                    instance.id
                ));
            }
            Vec::new()
        }
    };

    // A plain application image boots from a direct jump and never touches the
    // mask ROM. A merged image does: its first bytes are the bootloader, which
    // only the ROM knows how to start.
    let needs_rom = layout == FirmwareLayout::Merged || bootloader.is_some();
    let rom_rel = section.rom.as_deref().or(config.sim.rom.as_deref());
    let rom_path = rom_rel.map(PathBuf::from).or_else(|| {
        chip.and_then(rom_name)
            .and_then(|file| std::env::var_os("LITZ_ROM_DIR").map(|d| PathBuf::from(d).join(file)))
    });
    let rom = match rom_path {
        Some(path) => std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?,
        None if needs_rom => {
            return Err(format!(
                "board {:?}: {}; set `rom` in its [[board]] entry or in [sim], or LITZ_ROM_DIR",
                instance.id,
                if layout == FirmwareLayout::Merged {
                    "a merged image runs from the mask ROM"
                } else {
                    "a bootloader needs the mask ROM"
                }
            ));
        }
        None => Vec::new(),
    };

    // The image name is the cheapest hint; a stock image whose file is named
    // generically still announces itself in its banner bytes.
    let python = crate::python::detect(runtime, section.firmware.as_deref().unwrap_or(""));
    let python = if python == crate::python::PythonRuntime::MicroPython {
        crate::python::detect_bytes(runtime, &firmware)
    } else {
        python
    };
    Ok(BoardPlan {
        id: instance.id.clone(),
        manifest,
        flash_size,
        rom,
        firmware,
        elf,
        layout,
        bootloader,
        partition_table,
        runtime,
        python,
        scripts,
        mac: parse_mac(section.mac.as_deref().or(config.sim.mac.as_deref())),
    })
}

/// Check `litz.lock` against what this run would use, or write it when there
/// is none yet. A changed catalog or firmware is refused: the whole point is
/// that a green run can be repeated, and silently running a different
/// simulation would make the record a lie.
fn enforce_lock(prepared: &Prepared, update: bool) -> Result<(), String> {
    // One board records the same four hashes as before. Several boards have no
    // single `[firmware]` to describe them, so their images are folded into the
    // firmware hash in board order, each with its id and length; the run is
    // still repeatable when every byte that booted is in that hash.
    let (firmware, rom, bootloader, ptable) = if prepared.boards.len() == 1 {
        let plan = &prepared.boards[0];
        (
            Some(plan.firmware.clone()),
            (!plan.rom.is_empty()).then(|| plan.rom.clone()),
            plan.bootloader.clone(),
            plan.partition_table.clone(),
        )
    } else {
        let mut folded = Vec::new();
        for plan in &prepared.boards {
            folded.extend_from_slice(plan.id.as_bytes());
            folded.push(0);
            for image in [&plan.firmware, &plan.rom] {
                folded.extend_from_slice(&(image.len() as u64).to_le_bytes());
                folded.extend_from_slice(image);
            }
        }
        (Some(folded), None, None, None)
    };
    // Only the manifests this diagram actually expands go in the lock: an
    // entry the circuit never names cannot change the result, and hashing it
    // would rewrite every lock in the world when an unrelated part is added.
    let flat =
        FlatBoard::build(&prepared.circuit, &prepared.registry).map_err(|e| e.to_string())?;
    // The board itself is consumed by expansion, so its own manifest has to
    // come from the diagram rather than from the flattened part list.
    let used: std::collections::BTreeSet<&str> = prepared
        .circuit
        .parts
        .iter()
        .map(|part| part.type_key.as_str())
        .chain(flat.parts.iter().map(|part| part.type_key.as_str()))
        .collect();
    let manifests: Vec<_> = prepared
        .registry
        .manifest_texts()
        .into_iter()
        .filter(|(key, _, _)| used.contains(key.as_str()))
        .collect();
    let actual = Lock::from_run(
        &manifests,
        firmware.as_deref(),
        rom.as_deref(),
        bootloader.as_deref(),
        ptable.as_deref(),
    );
    let path = prepared.dir.join("litz.lock");
    if update || !path.exists() {
        actual.write(&path)?;
        eprintln!("litz: wrote {}", path.display());
        return Ok(());
    }
    let installed = Lock::load(&path)?;
    installed.check(&actual)
}

/// The ROM ELF esp32sim expects for a builtin chip; `None` when the name is
/// not one of the adapters this build carries.
/// Chips an in-tree adapter can build, whether or not they need a mask ROM.
pub(crate) fn is_builtin_chip(chip: &str) -> bool {
    matches!(
        chip,
        "esp32"
            | "esp32c3"
            | "esp32c6"
            | "esp32s3"
            | "esp32p4"
            | "esp32s31"
            | "atmega328p"
            | "atmega2560"
            | "atmega32u4"
            | "attiny85"
            | "rp2040"
            | "stc89c52"
            | "ch32v003"
    )
}

fn rom_name(chip: &str) -> Option<&'static str> {
    Some(match chip {
        "esp32" => "esp32_rev300_rom.elf",
        "esp32c3" => "esp32c3_rev3_rom.elf",
        "esp32c6" => "esp32c6_rev0_rom.elf",
        "esp32s3" => "esp32s3_rev0_rom.elf",
        "esp32p4" => "esp32p4_rev0_rom.elf",
        "esp32s31" => "esp32s31_rev0_rom.elf",
        _ => return None,
    })
}

/// The radio network for this run: `--wifi` wins over `[sim.wifi]`, and a
/// project with neither gets no network at all.
///
/// The router's default services are HTTP on port 80 and a TCP echo on 7, so a
/// firmware that joins the access point has something to talk to without any
/// further configuration; a project that lists `[[sim.wifi.service]]` entries
/// replaces the defaults, and `[sim.wifi] services = false` leaves a router
/// that refuses every connection.
fn network_config(options: &Options, config: &Config) -> Result<Option<NetworkConfig>, String> {
    let spec = match (&options.wifi, &config.sim.wifi) {
        (Some(spec), _) => WifiSpec::Spec(spec.clone()),
        (None, Some(spec)) => spec.clone(),
        (None, None) => return Ok(None),
    };

    let mut network = match &spec {
        WifiSpec::Spec(raw) => NetworkConfig {
            access_points: vec![access_point_from_spec(raw)?],
            ..NetworkConfig::default()
        },
        WifiSpec::Table(table) => {
            let mut network = NetworkConfig {
                access_points: vec![access_point(
                    &table.ssid,
                    table.psk.as_deref(),
                    table.channel,
                    table.bssid.as_deref().map(parse_bssid).transpose()?,
                    table.epoch,
                )],
                nearby: table
                    .nearby
                    .iter()
                    .map(|nearby| nearby_from_spec(nearby))
                    .collect::<Result<Vec<_>, String>>()?,
                resolve_unknown: table.resolve_unknown,
                ..NetworkConfig::default()
            };
            for extra in &table.extra_aps {
                network.access_points.push(access_point(
                    &extra.ssid,
                    extra.psk.as_deref(),
                    extra.channel,
                    extra.bssid.as_deref().map(parse_bssid).transpose()?,
                    extra.epoch,
                ));
            }
            for (name, address) in &table.names {
                let address: std::net::Ipv4Addr = address.parse().map_err(|_| {
                    format!("[sim.wifi] names.{name}: {address:?} is not an IPv4 address")
                })?;
                network.names.insert(name.clone(), address.octets());
            }
            network
        }
    };

    match &spec {
        WifiSpec::Spec(_) => {
            network = network.with_default_services();
        }
        WifiSpec::Table(table) if table.services.is_empty() => {
            if table.services_default {
                network = network.with_default_services();
            }
        }
        WifiSpec::Table(table) => {
            for service in &table.services {
                let inner = if service.mqtt {
                    Service::mqtt(service.port)
                } else if service.websocket {
                    Service::websocket(service.port)
                } else if service.echo {
                    if service.udp {
                        Service::UdpEcho { port: service.port }
                    } else {
                        Service::TcpEcho { port: service.port }
                    }
                } else {
                    let reply = service.reply.as_ref().ok_or_else(|| {
                        format!(
                            "[sim.wifi.service] port {}: give `reply = \"...\"`, `echo = true`, \
                             `websocket = true` or `mqtt = true`",
                            service.port
                        )
                    })?;
                    Service::Tcp {
                        port: service.port,
                        reply: reply.as_bytes().to_vec(),
                        after_request: service.wait,
                    }
                };
                network = network.with_service(if service.tls {
                    Service::tls(service.port, inner)
                } else {
                    inner
                });
            }
        }
    }
    Ok(Some(network))
}

fn access_point(
    ssid: &str,
    psk: Option<&str>,
    channel: Option<u8>,
    bssid: Option<[u8; 6]>,
    epoch: Option<u64>,
) -> AccessPoint {
    AccessPoint {
        ssid: ssid.to_string(),
        psk: psk.map(str::to_string),
        channel: channel.unwrap_or(6),
        bssid,
        epoch_s: epoch,
    }
}

/// An access point in the short `--wifi` syntax: `ssid=NAME[,psk=PASS]
/// [,channel=N][,bssid=..][,epoch=S]`.
fn access_point_from_spec(spec: &str) -> Result<AccessPoint, String> {
    let mut ssid = None;
    let mut psk = None;
    let mut channel = None;
    let mut bssid = None;
    let mut epoch = None;
    for entry in spec.split(',').filter(|entry| !entry.is_empty()) {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| format!("invalid Wi-Fi option {entry:?}: expected key=value"))?;
        match key {
            "ssid" => ssid = Some(value.to_string()),
            "psk" | "password" | "pass" => psk = Some(value.to_string()),
            "channel" | "chan" | "ch" => {
                channel = Some(value.parse::<u8>().map_err(|_| {
                    format!("invalid Wi-Fi channel {value:?}: expected 1 through 14")
                })?)
            }
            "bssid" => bssid = Some(parse_bssid(value)?),
            "epoch" => {
                epoch = Some(value.parse::<u64>().map_err(|_| {
                    format!("invalid Wi-Fi epoch {value:?}: expected seconds since 1970")
                })?)
            }
            other => return Err(format!("unknown Wi-Fi option {other:?}")),
        }
    }
    let ssid = ssid.ok_or_else(|| format!("invalid Wi-Fi network {spec:?}: no ssid"))?;
    Ok(access_point(&ssid, psk.as_deref(), channel, bssid, epoch))
}

fn nearby_from_spec(spec: &str) -> Result<NearbyNetwork, String> {
    let mut network = NearbyNetwork::new("");
    for entry in spec.split(',').filter(|entry| !entry.is_empty()) {
        let (key, value) = entry.split_once('=').ok_or_else(|| {
            format!("invalid nearby-network option {entry:?}: expected key=value")
        })?;
        match key {
            "ssid" => network.ssid = value.to_string(),
            "channel" | "chan" | "ch" => {
                network.channel = value.parse().map_err(|_| {
                    format!("invalid nearby-network channel {value:?}: expected 1 through 14")
                })?
            }
            "rssi" => {
                network.rssi = value.parse().map_err(|_| {
                    format!("invalid nearby-network rssi {value:?}: expected -128 through 127 dBm")
                })?
            }
            "secure" => {
                network.secure = match value {
                    "1" | "true" | "yes" => true,
                    "0" | "false" | "no" => false,
                    _ => {
                        return Err(format!(
                            "invalid nearby-network secure {value:?}: expected 0 or 1"
                        ));
                    }
                }
            }
            other => return Err(format!("unknown nearby-network option {other:?}")),
        }
    }
    if network.ssid.is_empty() {
        return Err(format!("invalid nearby network {spec:?}: no ssid"));
    }
    Ok(network)
}

fn parse_bssid(text: &str) -> Result<[u8; 6], String> {
    let octets: Vec<u8> = text
        .split(':')
        .map(|octet| {
            u8::from_str_radix(octet, 16)
                .map_err(|_| format!("invalid BSSID {text:?}: expected six hex octets"))
        })
        .collect::<Result<_, _>>()?;
    octets
        .try_into()
        .map_err(|_| format!("invalid BSSID {text:?}: expected six hex octets"))
}

/// Build the boards, load each image and start every chip. A test and a
/// scenario differ only in what they do once it is running.
///
/// The backend is chosen by each board's manifest: a builtin `chip` is an
/// adapter this build carries, a `[board.simulator]` module is a wasm
/// component. Everything after this point is written against [`Fleet`], so the
/// backends are interchangeable and a diagram may run more than one.
fn boot(prepared: &Prepared, options: &Options) -> Result<Booted, String> {
    let flat =
        FlatBoard::build(&prepared.circuit, &prepared.registry).map_err(|e| e.to_string())?;
    let parts: Vec<String> = flat.parts.iter().map(|p| p.id.clone()).collect();
    let mut board = Board::new(flat)
        .map_err(|e| e.to_string())?
        .with_analog(options.analog || prepared.config.sim.analog);
    // Probes are resolved before any time passes: a point that is not in the
    // diagram is a configuration error, not a channel that silently stays
    // empty. The project lists them, and a run may add more.
    for probe in &prepared.config.probes {
        let name = probe.name.as_deref().unwrap_or(probe.point.as_str());
        board.add_probe(name, &probe.point)?;
    }
    for spec in &options.probes {
        // `--probe sck=esp:GPIO2` names the channel and the point; a bare
        // point keeps its own name, as the project's probe entries do.
        let (name, point) = match spec.split_once('=') {
            Some((name, point)) => (name.trim(), point.trim()),
            None => (spec.as_str(), spec.as_str()),
        };
        board.add_probe(name, point)?;
    }
    // A decoder reads channels, so every name it mentions must already be a
    // probe. Checking here keeps a typo from producing an empty report after
    // a full run.
    for spec in prepared.config.decodes.iter().chain(options.decodes.iter()) {
        crate::decode::check(spec, board.probes())?;
    }
    // A `litz-wifi-router` part and a `litz-ble-beacon` part belong to the
    // board, so every chip on it hears the same network and the same
    // advertisers; `[sim.wifi]` or `--wifi` replaces the network for one run.
    let mut network =
        network_config(options, &prepared.config)?.or_else(|| board.network().cloned());
    let capture = if options.pcap.is_some() {
        let Some(config) = network.as_mut() else {
            return Err(
                "--pcap needs a simulated network: add a `litz-wifi-router` part or [sim.wifi]"
                    .into(),
            );
        };
        let capture = std::rc::Rc::new(std::cell::RefCell::new(litz_net::capture::Capture::new([
            10, 0, 2, 15,
        ])));
        config.capture = Some(std::rc::Rc::clone(&capture));
        Some(capture)
    } else {
        None
    };
    if let Some(config) = &network {
        board.set_network(config.clone());
    }
    let bluetooth = board.bluetooth().cloned();
    let shared = std::rc::Rc::new(std::cell::RefCell::new(board));

    let mut chips = Vec::with_capacity(prepared.boards.len());
    for (index, plan) in prepared.boards.iter().enumerate() {
        let chip = build_chip(
            prepared,
            plan,
            std::rc::Rc::clone(&shared),
            index as u16,
            network.as_ref(),
            bluetooth.as_ref(),
        )?;
        chips.push(boot_chip(chip, plan)?);
    }

    // The radio seams belong to the station, so they are asked of the first
    // board; a diagram with several Wi-Fi chips records the first one.
    if let Some(path) = &options.pcap_air
        && let Some(chip) = chips.first_mut()
    {
        chip.start_air_capture(path)?;
    }
    if let Some(path) = &options.pcap_eth
        && let Some(chip) = chips.first_mut()
    {
        chip.start_ethernet_capture(path)?;
    }
    if options.host_network || !options.host_forwards.is_empty() {
        let chip = chips
            .first_mut()
            .ok_or_else(|| "--net needs a board".to_string())?;
        chip.attach_host_gateway(&options.host_forwards)?;
    }

    // A debugger attaches to a booted chip, so flash and symbols are already
    // in place when it reads memory or sets a breakpoint. A session owns its
    // chip's clock until the client disconnects, so one board is served; the
    // request says which, or names the first board that has a debug target.
    if let Some(request) = &options.gdb {
        let index = match &request.board {
            Some(id) => find_board(&prepared.boards, id, "--gdb")?,
            None => chips
                .iter_mut()
                .position(|chip| chip.debug().is_some())
                .ok_or_else(|| {
                    "--gdb needs a backend with a GDB target; a wasm MCU and the AVR core have none"
                        .to_string()
                })?,
        };
        let plan = &prepared.boards[index];
        let chip = &mut chips[index];
        let debug = chip.debug().ok_or_else(|| {
            format!(
                "--gdb: board {:?} has no GDB target; a wasm MCU and the AVR core have none",
                plan.id
            )
        })?;
        eprintln!("litz: gdb serves board {:?}", plan.id);
        debug.serve(request.port)?;
    }
    Ok(Booted {
        fleet: Fleet::new(chips),
        parts,
        board: shared,
        capture,
    })
}

/// A booted circuit: the chips that run it, the part ids a report names, and
/// the board they all share. The board handle is what answers probe questions
/// once the chips own the run loop.
struct Booted {
    fleet: Fleet,
    parts: Vec<String>,
    board: std::rc::Rc<std::cell::RefCell<Board>>,
    /// The packet capture the run filled, when `--pcap` asked for one.
    capture: Option<std::rc::Rc<std::cell::RefCell<litz_net::capture::Capture>>>,
}

/// The board a flag names, by its `diagram.json` instance id. The error lists
/// the ids that exist so a typo is answered in the same line it is reported.
fn find_board(boards: &[BoardPlan], id: &str, flag: &str) -> Result<usize, String> {
    boards.iter().position(|plan| plan.id == id).ok_or_else(|| {
        format!(
            "{flag} names board {id:?}; diagram.json has {}",
            boards
                .iter()
                .map(|plan| plan.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Which board a touch targets. Wokwi names the board in `part-id`; the older
/// `board` key still wins when both are present. A part id that is not a board
/// is ignored, so a single-board project can write either.
fn touch_board(
    prepared: &Prepared,
    board: &Option<String>,
    part: &Option<String>,
    flag: &str,
) -> Result<Option<usize>, String> {
    if let Some(id) = board {
        return find_board(&prepared.boards, id, flag).map(Some);
    }
    if let Some(part) = part
        && let Some(index) = prepared.boards.iter().position(|plan| plan.id == *part)
    {
        return Ok(Some(index));
    }
    Ok(None)
}

/// One board's chip, attached to the shared board at its own index.
fn build_chip(
    prepared: &Prepared,
    plan: &BoardPlan,
    board: std::rc::Rc<std::cell::RefCell<Board>>,
    index: u16,
    network: Option<&NetworkConfig>,
    bluetooth: Option<&litz_ble::BleConfig>,
) -> Result<Box<dyn Chip>, String> {
    let info = plan
        .manifest
        .board
        .as_ref()
        .expect("prepare checked it is a board");
    #[cfg(not(target_family = "wasm"))]
    if let Some(simulator) = &info.simulator {
        let module = module_bytes(prepared, &plan.manifest, &simulator.module)?;
        return Ok(Box::new(
            WasmChip::from_bytes_with_abi(&module, &simulator.module, &simulator.abi)?
                .with_shared_board(board, index),
        ));
    }
    #[cfg(target_family = "wasm")]
    if info.simulator.is_some() {
        let _ = prepared;
        return Err(
            "a wasm guest chip cannot run inside the browser build; only the builtin chips \
             are compiled there"
                .into(),
        );
    }
    let name = info.chip.as_deref().unwrap_or("esp32c3");
    let mut chip = builtin_chip(
        name,
        plan.mac,
        plan.flash_size,
        board,
        index,
        Attachments {
            network,
            bluetooth,
            elf: plan.elf.as_deref(),
            ble_controller: plan.python == crate::python::PythonRuntime::CircuitPython,
        },
    )?;
    // A board whose manifest declares PSRAM gets the amount the chip's own
    // SocBus supports; the flag is the board's, the size is the chip's.
    if info.psram {
        chip.set_psram_size(4 << 20)?;
    }
    Ok(chip)
}

/// Load the plan's images and start the chip.
fn boot_chip(mut mcu: Box<dyn Chip>, plan: &BoardPlan) -> Result<Box<dyn Chip>, String> {
    if !plan.rom.is_empty() {
        mcu.load_image(ImageKind::Rom, 0, &plan.rom)?;
    }
    if plan.firmware.is_empty() && plan.bootloader.is_none() {
        // A board with no image boots whatever its own simulator carries.
        mcu.boot()?;
        return Ok(mcu);
    }
    let flash = mcu.info().flash;
    match plan.layout {
        // The whole file goes in at the bootloader offset and `Chip::boot`
        // lets the ROM start it; the bootloader then finds the partition table
        // and application inside the same bytes. `flash.bootloader` is 0 on
        // the RISC-V parts whose official images use this layout.
        FirmwareLayout::Merged => {
            mcu.load_image(ImageKind::Bootloader, flash.bootloader, &plan.firmware)?;
        }
        FirmwareLayout::Application => match (&plan.bootloader, &plan.partition_table) {
            (Some(bootloader), Some(ptable)) => {
                mcu.load_image(ImageKind::Bootloader, flash.bootloader, bootloader)?;
                mcu.load_image(ImageKind::PartitionTable, flash.partition_table, ptable)?;
                mcu.load_image(ImageKind::Firmware, flash.app, &plan.firmware)?;
            }
            (None, None) => {
                mcu.load_image(ImageKind::Firmware, flash.app, &plan.firmware)?;
            }
            // A bootloader finds the application through the partition table,
            // so one without the other is a manifest mistake, not a fallback.
            _ => {
                return Err(format!(
                    "board {:?}: bootloader and partition_table go together; give both or neither",
                    plan.id
                ));
            }
        },
    }
    mcu.boot()?;
    Ok(mcu)
}

/// The chips one diagram runs, in board order.
///
/// A single-board project is a fleet of one, so the run loop has one shape.
/// Board-wide questions — part state, conflicts, a pressed part — are answered
/// by the first chip, which sees the same board as the rest. Chip questions —
/// run, console, time — are asked of every chip, and their consoles are merged
/// in board order at each chunk boundary.
struct Fleet {
    chips: Vec<Box<dyn Chip>>,
    /// Each chip's console since the run began, so a MicroPython prompt can be
    /// found on the board that printed it.
    console: Vec<String>,
    /// How far each console has been handed out.
    read: Vec<usize>,
    /// How each chip last stopped, which is what tells a finished board from a
    /// slow one when the frontier is computed.
    stops: Vec<Stop>,
}

impl Fleet {
    fn new(chips: Vec<Box<dyn Chip>>) -> Self {
        let count = chips.len();
        Self {
            chips,
            console: vec![String::new(); count],
            read: vec![0; count],
            stops: vec![Stop::Budget; count],
        }
    }

    /// Advance every chip by one scheduling batch. The returned stop is the
    /// most significant one any chip reported, so a trap on either board ends
    /// the run.
    fn run(&mut self, steps: u64) -> litz_board::chip::RunResult {
        let mut cycles = 0;
        let mut stop = Stop::Budget;
        for (index, chip) in self.chips.iter_mut().enumerate() {
            let result = chip.run(steps);
            cycles = cycles.max(result.cycles);
            self.stops[index] = result.stop;
            let chunk = chip.take_console();
            if !chunk.is_empty() {
                self.console[index].push_str(&String::from_utf8_lossy(&chunk));
            }
            if stop_rank(result.stop) > stop_rank(stop) {
                stop = result.stop;
            }
        }
        litz_board::chip::RunResult { cycles, stop }
    }

    /// One chip's next batch, for the paste handshake, which talks to the
    /// board whose REPL is ready and lets the others keep their own pace.
    fn run_one(&mut self, index: usize, steps: u64) {
        let result = self.chips[index].run(steps);
        self.stops[index] = result.stop;
        let chunk = self.chips[index].take_console();
        if !chunk.is_empty() {
            self.console[index].push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// Console bytes produced since the last call, merged in board order.
    /// Deliver a touch to one board (or the first), through its controller.
    fn touch(&mut self, board: Option<usize>, x: u16, y: u16, down: bool) -> Result<(), String> {
        let index = board.unwrap_or(0);
        match self.chips.get_mut(index) {
            Some(chip) => chip.touch(x, y, down),
            None => Err(format!("the diagram has no board {index}")),
        }
    }

    /// The first chip's flash, as the run left it.
    fn flash_image(&self) -> Option<Vec<u8>> {
        self.chips.first()?.flash_image()
    }

    /// Ask the first chip for the firmware's free heap.
    fn sample_heap(&mut self) -> Result<u32, String> {
        match self.chips.first_mut() {
            Some(chip) => chip.sample_heap(),
            None => Err("the diagram has no chip to sample".into()),
        }
    }

    fn take_console(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, text) in self.console.iter().enumerate() {
            if self.read[index] < text.len() {
                out.extend_from_slice(&text.as_bytes()[self.read[index]..]);
                self.read[index] = text.len();
            }
        }
        out
    }

    /// One board's console so far.
    fn console_text(&self, index: usize) -> &str {
        &self.console[index]
    }

    fn write_console_to(&mut self, index: usize, bytes: &[u8]) {
        self.chips[index].write_console(bytes);
    }

    /// The time every running chip has reached. A board that halted or
    /// trapped is finished and does not hold the others back.
    /// Whether every board has stopped for good. A halted core makes no more
    /// simulated time, so waiting for the timeout after this would wait for
    /// ever on a clock that never moves.
    fn finished(&self) -> bool {
        self.stops
            .iter()
            .all(|stop| matches!(stop, Stop::Halted | Stop::Reset | Stop::Trap))
    }

    fn frontier_ns(&self) -> u64 {
        let mut frontier: Option<u64> = None;
        let mut latest = 0;
        for (index, chip) in self.chips.iter().enumerate() {
            let now = (chip.seconds() * 1e9) as u64;
            latest = latest.max(now);
            if matches!(self.stops[index], Stop::Halted | Stop::Reset | Stop::Trap) {
                continue;
            }
            frontier = Some(frontier.map_or(now, |front| front.min(now)));
        }
        frontier.unwrap_or(latest)
    }

    fn seconds(&self) -> f64 {
        self.chips
            .iter()
            .map(|chip| chip.seconds())
            .fold(0.0, f64::max)
    }

    /// Queue host actions on one board's chip. `board` is the index in
    /// [`Prepared::boards`], the same order the chips were built in.
    fn schedule(&mut self, board: usize, inputs: Vec<Input>) -> Result<(), String> {
        match self.chips.get_mut(board) {
            Some(chip) => chip.schedule(inputs),
            None => Err(format!("there is no board {board} to schedule input on")),
        }
    }

    fn state(&self, part: &str) -> Option<PartState> {
        self.chips.iter().find_map(|chip| chip.state(part))
    }

    fn press(&mut self, part: &str, pressed: bool) -> bool {
        self.chips.iter_mut().any(|chip| chip.press(part, pressed))
    }

    fn set_control(&mut self, part: &str, value: u16) -> bool {
        // Part ids are unique to the circuit, so only the board that holds the
        // part answers; the rest report that they have no such control.
        self.chips
            .iter_mut()
            .any(|chip| chip.set_control(part, value))
    }

    fn set_named_control(&mut self, part: &str, name: &str, value: &str) -> bool {
        // A board instance is a control target of its own: `set-control` on a
        // board id throws that board's power switch, which is how a scenario
        // lets one board power another.
        self.chips
            .iter_mut()
            .any(|chip| chip.set_named_control(part, name, value))
    }

    fn part_pin_level(&self, part: &str, pin: Option<&str>) -> Option<Level> {
        self.chips
            .iter()
            .find_map(|chip| chip.part_pin_level(part, pin))
    }

    fn gpio_level(&self, board: usize, gpio: u8) -> Option<Level> {
        self.chips.get(board)?.gpio_level(gpio)
    }

    fn conflicts(&self) -> Vec<litz_board::Conflict> {
        self.chips
            .first()
            .map(|chip| chip.conflicts())
            .unwrap_or_default()
    }

    fn fault(&self) -> Option<String> {
        self.chips.iter().find_map(|chip| chip.fault())
    }

    /// Host bytes into one board's console. Board 0 when no board was asked
    /// for.
    fn write_console_on(&mut self, board: usize, bytes: &[u8]) {
        if let Some(chip) = self.chips.get_mut(board) {
            chip.write_console(bytes);
        }
    }

    /// Capture `board`'s display, or — when `board` is `None` — the first
    /// board that has one. A chip without a framebuffer says so, and the
    /// search moves on rather than failing a run because the first board has
    /// no panel.
    fn save_display(&mut self, board: Option<usize>, path: &Path) -> Result<(), String> {
        let Some(board) = board else {
            let mut refusal = None;
            for chip in &mut self.chips {
                match chip.save_display(path) {
                    Ok(()) => return Ok(()),
                    Err(e) => refusal.get_or_insert(e),
                };
            }
            return Err(refusal.unwrap_or_else(|| "the diagram has no chip to capture".into()));
        };
        match self.chips.get_mut(board) {
            Some(chip) => chip.save_display(path),
            None => Err(format!("there is no board {board} to capture")),
        }
    }

    /// Capture one display part by id. The first chip that knows the part
    /// wins, which is the board the diagram wired it to.
    fn save_part_display(&mut self, part: &str, path: &Path) -> Result<(), String> {
        let mut refusal = None;
        for chip in &mut self.chips {
            match chip.save_part_display(part, path) {
                Ok(()) => return Ok(()),
                Err(error) => refusal.get_or_insert(error),
            };
        }
        Err(refusal.unwrap_or_else(|| format!("no part {part:?} has a framebuffer")))
    }

    fn report(&mut self) -> Vec<String> {
        self.chips
            .iter_mut()
            .flat_map(|chip| chip.report())
            .collect()
    }

    fn enable_coverage(&mut self) {
        for chip in &mut self.chips {
            chip.enable_coverage();
        }
    }

    fn start_trace(&mut self) {
        if let Some(chip) = self.chips.first_mut() {
            chip.start_trace();
        }
    }

    fn break_at(&mut self, pc: u32) {
        for chip in &mut self.chips {
            chip.break_at(pc);
        }
    }

    fn vcd(&self) -> String {
        self.chips
            .first()
            .map(|chip| chip.vcd())
            .unwrap_or_default()
    }
}

/// How decisive a stop is when several chips report in one batch.
fn stop_rank(stop: Stop) -> u8 {
    match stop {
        Stop::Trap => 5,
        Stop::Breakpoint(_) => 4,
        Stop::Reset => 3,
        Stop::Halted => 2,
        Stop::Idle => 1,
        Stop::Budget => 0,
    }
}

/// The chip module a board names, from the binary for a builtin board or from
/// disk next to the manifest for one loaded from a directory.
#[cfg(not(target_family = "wasm"))]
fn module_bytes(
    prepared: &Prepared,
    manifest: &litz_catalog::PartManifest,
    module: &str,
) -> Result<Vec<u8>, String> {
    if let Some(bytes) = prepared.registry.builtin_module(&manifest.id) {
        return Ok(bytes.to_vec());
    }
    let base = manifest
        .source
        .as_deref()
        .and_then(|path| path.parent())
        .ok_or_else(|| {
            format!(
                "board {:?} names a module but has no directory",
                manifest.id
            )
        })?;
    let path = base.join(module);
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

fn run_once(
    prepared: &Prepared,
    spec: Option<&TestSpec>,
    _strict: bool,
    options: &Options,
) -> Result<Outcome, String> {
    let data = run_inner(prepared, spec, spec.is_none(), false, None, options)?;
    Ok(Outcome {
        exit_code: data.exit_code,
        output: data.output,
        sim_ms: data.sim_ms,
        tests: Vec::new(),
    })
}

fn run_capture(
    prepared: &Prepared,
    timeout: Option<&str>,
    options: &Options,
) -> Result<CaptureOutcome, String> {
    let data = run_inner(prepared, None, false, true, timeout, options)?;
    Ok(CaptureOutcome {
        exit_code: data.exit_code,
        output: data.output,
        sim_ms: data.sim_ms,
        part_states: data.part_states,
        faults: data.faults,
    })
}

/// Everything a run produces, before it is shaped for a terminal or a host.
struct RunData {
    exit_code: i32,
    output: String,
    sim_ms: u32,
    part_states: BTreeMap<String, PartState>,
    faults: Vec<String>,
}

fn run_inner(
    prepared: &Prepared,
    spec: Option<&TestSpec>,
    stream: bool,
    capture_states: bool,
    timeout_override: Option<&str>,
    options: &Options,
) -> Result<RunData, String> {
    let Booted {
        fleet: mut mcu,
        parts,
        board,
        capture,
    } = boot(prepared, options)?;
    start_recording(&mut mcu, options, prepared);
    if options.coverage {
        mcu.enable_coverage();
    }
    // Which chip the terminal and the screenshot address is settled before any
    // time passes: a board name that is not in the diagram is a configuration
    // error, not a note at the end of a run.
    let console_board = match options
        .interactive
        .as_ref()
        .and_then(|i| i.board.as_deref())
    {
        Some(id) => find_board(&prepared.boards, id, "--interactive")?,
        None => 0,
    };
    let capture_board = match options
        .screenshot
        .as_ref()
        .and_then(|capture| capture.board.as_deref())
    {
        Some(id) => Some(find_board(&prepared.boards, id, "--screenshot")?),
        None => None,
    };
    // `[[test.input]] pin` is a part id when the diagram has such a part and a
    // pin number otherwise. Finding out before the machine exists keeps the
    // two meanings from colliding at run time.
    let presses = spec.map(|s| press_schedule(s, &parts)).unwrap_or_default();

    let inputs = spec
        .map(|s| inputs_for(s, &parts, &prepared.boards))
        .transpose()?
        .unwrap_or_default();
    for (board, inputs) in inputs {
        mcu.schedule(board, inputs)?;
    }

    let timeout = match timeout_override {
        Some(text) => parse_duration(text)?,
        None => spec
            .and_then(|s| s.timeout.as_deref())
            .map(parse_duration)
            .transpose()?
            .unwrap_or_else(|| parse_duration(DEFAULT_TIMEOUT).expect("default timeout")),
    };
    let expects = spec
        .map(|s| consoled(s.expect(), &prepared.boards))
        .unwrap_or_default();
    let fails = spec
        .map(|s| consoled(s.fail(), &prepared.boards))
        .unwrap_or_default();
    let mut states: Vec<Wanted> = spec
        .map(TestSpec::expect_state)
        .unwrap_or_default()
        .into_iter()
        .map(wanted)
        .collect::<Result<_, _>>()?;
    let state_at = spec
        .and_then(|s| s.state_at.as_deref())
        .map(parse_duration)
        .transpose()?;
    let state_stays = spec
        .and_then(|s| s.state_stays.as_deref())
        .map(parse_duration)
        .transpose()?;
    for want in &mut states {
        want.at = state_at;
        want.stays = state_stays;
        if mcu.state(&want.part).is_none() {
            let known: Vec<&str> = diagram_part_ids(prepared).collect();
            return Err(format!(
                "no part named {:?} in diagram.json ({}); a part inside a board is \
                 reached as <board>/<id>",
                want.part,
                known.join(", ")
            ));
        }
    }

    let mut heap = HeapLog::from_options(
        options,
        prepared
            .boards
            .first()
            .map(|plan| plan.id.clone())
            .unwrap_or_else(|| "esp".into()),
    )?;
    // CircuitPython starts `code.py` itself; the interrupt is queued before
    // the first chunk so the prompt can appear.
    for (index, plan) in prepared.boards.iter().enumerate() {
        if pastes_script(plan) && !plan.python.interrupt_sequence().is_empty() {
            mcu.write_console_on(index, plan.python.interrupt_sequence());
        }
    }
    let mut serial = String::new();
    let mut timed_out = false;
    let mut due = 0;
    let mut saw_fail = None;
    // Interactive input arrives on a thread because the run loop owns the
    // simulated clock: blocking on a terminal read would stop time. A line is
    // forwarded at the next chunk boundary, which is where every other host
    // input lands too.
    let (stdin_tx, stdin_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    if options.interactive.is_some() {
        std::thread::spawn(move || {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            let mut line = String::new();
            loop {
                line.clear();
                match lock.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if stdin_tx.send(line.as_bytes().to_vec()).is_err() => break,
                    Ok(_) => {}
                }
            }
        });
    }
    let mut saw_trap: Option<String> = None;
    // A MicroPython run is not ready for its expectations until the prompt has
    // appeared and the script has been pasted. `pasted` gates both the
    // handshake and the "expectations met" exit.
    let mut pasted: Vec<bool> = prepared
        .boards
        .iter()
        .map(|plan| !pastes_script(plan))
        .collect();
    let mut pasted_at = vec![0u64; prepared.boards.len()];
    // When the host last nudged a CircuitPython board: it runs `code.py` by
    // itself and then waits in "Press any key to enter the REPL", so the
    // friendly prompt only appears after a key arrives. `pasted` still gates
    // the handshake; this only decides when to knock.
    let mut nudged_at = vec![0u64; prepared.boards.len()];
    loop {
        // A fault present before the chip runs (two rails meeting on a net, a
        // pin tied to ground while the board is already driving) is reported
        // without spending a chunk of simulated time on a doomed boot.
        if !mcu.conflicts().is_empty() {
            break;
        }
        // Host input lands at chunk boundaries, so it is applied within one
        // million-step chunk of the time it asked for — far below the
        // resolution any assertion can see.
        let now_ns = mcu.frontier_ns();
        while due < presses.len() && presses[due].0 <= now_ns {
            let (_, part, pressed) = &presses[due];
            mcu.press(part, *pressed);
            due += 1;
        }
        while let Ok(bytes) = stdin_rx.try_recv() {
            mcu.write_console_on(console_board, &bytes);
        }
        let step = mcu.run(1_000_000);
        // A trap is a real stop, not a slow boot: without this the run would
        // keep spinning a dead core until the timeout and report only a
        // missing expectation. `fault()` carries the backend's detail when it
        // has one.
        if step.stop == Stop::Trap {
            saw_trap = Some(mcu.fault().unwrap_or_else(|| "the core trapped".into()));
            break;
        }
        let chunk = mcu.take_console();
        if !chunk.is_empty() {
            let text = String::from_utf8_lossy(&chunk);
            if stream {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            serial.push_str(&text);
        }
        if !mcu.conflicts().is_empty() {
            break;
        }
        if let Some(log) = &mut heap {
            let now = mcu.frontier_ns();
            log.sample(&mut mcu, now);
            if log.failed() {
                break;
            }
        }
        if mcu.finished() {
            break;
        }
        if let Some(hit) = fails.iter().find(|f| seen(&mcu, &serial, f)) {
            saw_fail = Some(hit.clone());
            break;
        }
        // Each board keeps its own handshake: two boards boot at their own
        // pace, and the prompt belongs to the console that printed it.
        let now = mcu.frontier_ns();
        for (index, plan) in prepared.boards.iter().enumerate() {
            if pasted[index] {
                continue;
            }
            if mcu.console_text(index).contains(REPL_PROMPT) {
                paste_script(&mut mcu, index, &plan.scripts, plan.python);
                pasted[index] = true;
                pasted_at[index] = now;
            } else if plan.python == crate::python::PythonRuntime::CircuitPython
                && !mcu.console_text(index).is_empty()
                && now.saturating_sub(nudged_at[index]) >= CIRCUITPYTHON_NUDGE
            {
                // Ctrl-C: it interrupts a running code.py, and at the
                // "press any key" idle it is the key that opens the REPL.
                mcu.write_console_on(index, plan.python.interrupt_sequence());
                nudged_at[index] = now;
            } else if now >= timeout {
                return Err(format!(
                    "board {:?}: MicroPython REPL prompt never appeared within {timeout} ns of \
                     simulated time; check that its firmware is a MicroPython or CircuitPython build",
                    plan.id
                ));
            }
        }
        for want in &mut states {
            if let Some(actual) = mcu.state(&want.part) {
                observe(want, actual, now);
            }
        }
        let paste_settled = prepared.boards.iter().enumerate().all(|(index, plan)| {
            !pastes_script(plan)
                || (pasted[index] && now >= pasted_at[index].saturating_add(PASTE_GRACE_NS))
        });
        let satisfied = pasted.iter().all(|pasted| *pasted)
            && paste_settled
            && expects.iter().all(|e| seen(&mcu, &serial, e))
            && states.iter().all(|w| w.seen);
        if satisfied && (!expects.is_empty() || !states.is_empty()) {
            timed_out = false;
            break;
        }
        if mcu.frontier_ns() >= timeout {
            timed_out = true;
            break;
        }
    }

    let conflicts = mcu.conflicts();
    let missing: Vec<&Consoled> = expects.iter().filter(|e| !seen(&mcu, &serial, e)).collect();
    let missing_states: Vec<&Wanted> = states.iter().filter(|w| !w.seen).collect();
    let passed = conflicts.is_empty()
        && saw_fail.is_none()
        && saw_trap.is_none()
        && missing.is_empty()
        && missing_states.is_empty();

    let mut notes = Vec::new();
    for conflict in &conflicts {
        notes.push(format!(
            "electrical fault on net {}: {}",
            conflict.net, conflict.detail
        ));
    }
    if let Some(hit) = &saw_fail {
        let text = hit.text;
        match hit.board.and_then(|index| prepared.boards.get(index)) {
            Some(plan) => notes.push(format!(
                "found failure text {text:?} on board {:?}",
                plan.id
            )),
            None => notes.push(format!("found failure text {text:?}")),
        }
    }
    if let Some(detail) = &saw_trap {
        notes.push(format!("the core trapped: {detail}"));
    }
    for miss in &missing {
        match miss.board.and_then(|index| prepared.boards.get(index)) {
            Some(plan) => notes.push(format!("never saw {:?} on board {:?}", miss.text, plan.id)),
            None => notes.push(format!("never saw {:?}", miss.text)),
        }
    }
    for want in missing_states {
        if want.broken {
            let stays = want.stays.map(seconds).unwrap_or_default();
            notes.push(format!(
                "{} {} did not hold for {stays}",
                want.part, want.state
            ));
        } else if let Some(at) = want.at {
            notes.push(format!(
                "{} was not {} at {}",
                want.part,
                want.state,
                seconds(at)
            ));
        } else {
            notes.push(format!("never saw {} {}", want.part, want.state));
        }
    }
    if timed_out && spec.is_some() {
        notes.push(format!("timed out after {timeout} ns of simulated time"));
    }

    let mut exit_code = if spec.is_none() {
        if !conflicts.is_empty() || saw_trap.is_some() {
            1
        } else if !timed_out && mcu.finished() {
            // Every core halted on its own before the clock ran out: the
            // program finished rather than the run being cut short.
            0
        } else {
            EXIT_TIMEOUT
        }
    } else if passed {
        0
    } else {
        1
    };

    let mut part_states = BTreeMap::new();
    if capture_states {
        for part in &parts {
            if let Some(state) = mcu.state(part)
                && state != PartState::None
            {
                part_states.insert(part.clone(), state);
            }
        }
    }
    let faults: Vec<String> = conflicts
        .iter()
        .map(|conflict| format!("net {}: {}", conflict.net, conflict.detail))
        .collect();

    let mut output = if stream { String::new() } else { serial };
    if !output.ends_with('\n') && !output.is_empty() {
        output.push('\n');
    }
    for note in notes {
        output.push_str(&format!("litz: {note}\n"));
    }
    for line in probe_report(&board) {
        output.push_str(&format!("litz: {line}\n"));
    }
    for line in decode_report(&board, prepared, options)? {
        output.push_str(&format!("litz: {line}\n"));
    }
    // Coverage is a report, not an assertion: it never changes the exit code,
    // so a CI step can collect it without turning a pass into a failure.
    for line in mcu.report() {
        output.push_str(&line);
        if !line.ends_with('\n') {
            output.push('\n');
        }
    }
    let sim_ms = (mcu.seconds() * 1000.0) as u32;
    finish_recording(&mcu, &board, options, capture.as_ref())?;
    // A failing test is the moment a picture of the display earns its keep; a
    // plain `run` captures unconditionally because the caller asked for it.
    // A capture that cannot be taken is a note, not a failure: a board with no
    // display should not turn a green run red.
    if let Some(capture) = &options.screenshot
        && (spec.is_none() || exit_code != 0)
        && let Err(e) = mcu.save_display(capture_board, &capture.path)
    {
        eprintln!("litz: {e}");
    }
    if let Some(log) = heap {
        let end = mcu.frontier_ns();
        let (lines, failure) = log.finish(&mut mcu, end);
        for line in lines {
            output.push_str(&format!("litz: {line}\n"));
        }
        if let Some(failure) = failure {
            exit_code = 1;
            output.push_str(&format!("litz: heap: {failure}\n"));
        }
    }
    Ok(RunData {
        exit_code,
        output,
        sim_ms,
        part_states,
        faults,
    })
}

/// A file named in `litz.toml`, read relative to the project directory.
fn read_optional(dir: &Path, rel: Option<&str>) -> Result<Option<Vec<u8>>, String> {
    rel.map(|rel| {
        let path = dir.join(rel);
        std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
    })
    .transpose()
}

/// One `expect_state` entry: a part, the state it must reach, and whether it
/// has been seen there yet.
///
/// The wanted state stays text until it is compared, because one word can mean
/// the same thing for several parts — `on` is an LED being lit, a switch being
/// thrown and a buzzer sounding — and the part decides which reading applies.
struct Wanted {
    part: String,
    state: String,
    seen: bool,
    /// Sample the state at this simulated time instead of "at some point".
    at: Option<u64>,
    /// Once it matches, it must keep matching this long.
    stays: Option<u64>,
    /// The sample at `at` has been taken.
    sampled: bool,
    /// A held state started matching.
    started: bool,
    /// Simulated time the hold started.
    since: u64,
    /// The hold was broken before it completed.
    broken: bool,
}

/// `<part>:<state>`, the inverse of what this prints in the failure note.
fn wanted(text: &str) -> Result<Wanted, String> {
    let (part, state) = text
        .split_once(':')
        .ok_or_else(|| format!("{text:?} is not <part>:<state>"))?;
    let known = matches!(state, "on" | "off" | "down" | "pressed" | "up" | "released")
        || state.parse::<u16>().is_ok()
        || state
            .strip_prefix("0x")
            .is_some_and(|hex| u8::from_str_radix(hex, 16).is_ok());
    if !known {
        return Err(format!(
            "{state:?} is not a state; write on, off, down, up, a servo angle in degrees, \
             or a segment mask as 0x7f"
        ));
    }
    Ok(Wanted {
        part: part.to_string(),
        state: state.to_string(),
        seen: false,
        at: None,
        stays: None,
        sampled: false,
        started: false,
        since: 0,
        broken: false,
    })
}

/// Fold one observation of a part's state into what the test still needs.
///
/// Without modifiers this is "was ever seen". `state_at` samples instead of
/// accumulating, so a state that was true earlier and false at the requested
/// instant does not pass. `state_stays` extends a match into an interval: the
/// part must keep matching for the whole duration, and a single false
/// observation breaks it.
fn observe(want: &mut Wanted, actual: PartState, now: u64) {
    let matches = state_matches(actual, &want.state);
    match (want.at, want.stays) {
        (None, None) => {
            if matches {
                want.seen = true;
            }
        }
        (Some(at), None) => {
            if !want.sampled && now >= at {
                want.sampled = true;
                want.seen = matches;
            }
        }
        (None, Some(stays)) => {
            if want.broken || want.seen {
                return;
            }
            if matches {
                if !want.started {
                    want.started = true;
                    want.since = now;
                }
                if now.saturating_sub(want.since) >= stays {
                    want.seen = true;
                }
            } else if want.started {
                want.broken = true;
            }
        }
        (Some(at), Some(stays)) => {
            if want.broken || want.seen || now < at {
                return;
            }
            if !want.started {
                want.started = true;
                want.since = now;
            }
            if !matches {
                want.broken = true;
            } else if now.saturating_sub(want.since) >= stays {
                want.seen = true;
            }
        }
    }
}

/// `1.5s` for a note, without trailing zeros.
fn seconds(ns: u64) -> String {
    format!("{}s", ns as f64 / 1e9)
}

/// Whether a part's state is the one the test asked for.
fn state_matches(actual: PartState, want: &str) -> bool {
    match want {
        "on" => matches!(
            actual,
            PartState::Led { lit: true }
                | PartState::Switch { on: true }
                | PartState::Tone { sounding: true, .. }
        ),
        "off" => matches!(
            actual,
            PartState::Led { lit: false }
                | PartState::Switch { on: false }
                | PartState::Tone {
                    sounding: false,
                    ..
                }
        ),
        "down" | "pressed" => actual == PartState::Button { pressed: true },
        "up" | "released" => actual == PartState::Button { pressed: false },
        digits => {
            if let Some(hex) = digits.strip_prefix("0x") {
                let Ok(mask) = u8::from_str_radix(hex, 16) else {
                    return false;
                };
                return actual == PartState::Segments { mask };
            }
            match (digits.parse::<u16>(), actual) {
                (Ok(degrees), PartState::Servo { degrees: actual }) => degrees == actual,
                // A knob is written in percent, the unit `set-control`
                // and the diagram's `value` attribute use.
                (Ok(percent), PartState::Knob { permille }) => percent * 10 == permille,
                _ => false,
            }
        }
    }
}

fn diagram_part_ids(prepared: &Prepared) -> impl Iterator<Item = &str> {
    prepared.circuit.parts.iter().map(|p| p.id.as_str())
}

/// Presses of named parts, in the order they must happen.
fn press_schedule(spec: &TestSpec, parts: &[String]) -> Vec<(u64, String, bool)> {
    let mut presses: Vec<(u64, String, bool)> = Vec::new();
    for input in &spec.input {
        let Some(pin) = &input.pin else { continue };
        if !parts.iter().any(|id| id == pin) {
            continue;
        }
        let Ok(at) = parse_duration(&input.at) else {
            continue;
        };
        // A button wired to ground is pulled down while it is held, which is
        // the level a test writes: 0 presses, 1 releases.
        presses.push((at, pin.clone(), input.level.unwrap_or(1) == 0));
    }
    presses.sort_by_key(|(at, _, _)| *at);
    presses
}

/// Test inputs as backend-independent scheduled events, grouped by board.
///
/// A part id in `pin` is pressed through the board rather than asserted onto a
/// chip pin, so it is left to `press_schedule`; only bare pin numbers become
/// GPIO events. Each group is scheduled on the chip of the board it names; the
/// returned order is board order, so a run queues them deterministically.
fn inputs_for(
    spec: &TestSpec,
    parts: &[String],
    boards: &[BoardPlan],
) -> Result<Vec<(usize, Vec<Input>)>, String> {
    let mut by_board: BTreeMap<usize, Vec<Input>> = BTreeMap::new();
    for input in &spec.input {
        let at_ns = parse_duration(&input.at)?;
        let board = input_board(spec, input, boards)?;
        if let Some(serial) = &input.serial {
            if serial.contains('\n') {
                return Err(format!(
                    "test {:?}: serial input must be one line",
                    spec.name
                ));
            }
            by_board.entry(board).or_default().push(Input::Uart {
                at_ns,
                bytes: format!("{serial}\n").into_bytes(),
            });
        }
        if let Some(pin) = &input.pin {
            if parts.iter().any(|id| id == pin) {
                continue;
            }
            let Ok(number) = pin.parse::<u8>() else {
                return Err(format!(
                    "test {:?}: {pin:?} is neither a part in the diagram nor a GPIO number",
                    spec.name
                ));
            };
            let level = if input.level.unwrap_or(1) == 0 {
                Level::Low
            } else {
                Level::High
            };
            by_board.entry(board).or_default().push(Input::Gpio {
                at_ns,
                pin: number,
                level,
            });
        }
    }
    let mut groups: Vec<(usize, Vec<Input>)> = by_board.into_iter().collect();
    for (_, inputs) in &mut groups {
        inputs.sort_by_key(|input| match input {
            Input::Gpio { at_ns, .. } | Input::Uart { at_ns, .. } => *at_ns,
        });
    }
    Ok(groups)
}

/// The board an input lands on. One board needs no name; a diagram with more
/// than one cannot guess, because the same GPIO number exists on each.
fn input_board(
    spec: &TestSpec,
    input: &crate::config::Input,
    boards: &[BoardPlan],
) -> Result<usize, String> {
    match &input.board {
        Some(id) => boards
            .iter()
            .position(|plan| &plan.id == id)
            .ok_or_else(|| {
                format!(
                    "test {:?}: board {id:?} is not a development board in diagram.json ({})",
                    spec.name,
                    boards
                        .iter()
                        .map(|plan| plan.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
        None if boards.len() == 1 => Ok(0),
        None => Err(format!(
            "test {:?}: the input at {} needs `board = \"...\"`; this diagram has {} boards ({})",
            spec.name,
            input.at,
            boards.len(),
            boards
                .iter()
                .map(|plan| plan.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// One `expect` or `fail` entry: the text, and the board whose console must
/// show it.
///
/// A leading `<board>@` narrows the match to that board's console. The prefix
/// is only read as a board when it names one, so any other text — `@`
/// included — is matched against the merged console of every board.
#[derive(Clone)]
struct Consoled<'a> {
    board: Option<usize>,
    text: &'a str,
}

fn consoled<'a>(entries: Vec<&'a str>, boards: &[BoardPlan]) -> Vec<Consoled<'a>> {
    entries
        .into_iter()
        .map(|entry| match entry.split_once('@') {
            Some((prefix, rest)) => match boards.iter().position(|plan| plan.id == prefix) {
                Some(board) => Consoled {
                    board: Some(board),
                    text: rest,
                },
                None => Consoled {
                    board: None,
                    text: entry,
                },
            },
            None => Consoled {
                board: None,
                text: entry,
            },
        })
        .collect()
}

/// Whether the console this entry names has shown its text. An entry without
/// a board reads the merged stream, exactly as a single-board project always
/// did.
fn seen(fleet: &Fleet, merged: &str, entry: &Consoled) -> bool {
    match entry.board {
        Some(index) => fleet.console_text(index).contains(entry.text),
        None => merged.contains(entry.text),
    }
}

fn parse_mac(text: Option<&str>) -> [u8; 6] {
    let Some(text) = text else {
        return DEFAULT_MAC;
    };
    let bytes: Vec<u8> = text
        .split([':', '-'])
        .filter_map(|b| u8::from_str_radix(b, 16).ok())
        .collect();
    if bytes.len() == 6 {
        [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]]
    } else {
        DEFAULT_MAC
    }
}

fn blocks(diagnostics: &[Diagnostic], strict: bool) -> bool {
    diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error || (strict && d.severity == Severity::Warning))
}

fn render(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| {
            format!(
                "{}[{}]: {}\n",
                if d.severity == Severity::Error {
                    "error"
                } else {
                    "warning"
                },
                d.code,
                d.message
            )
        })
        .collect()
}

/// `--trigger CHANNEL=high|low[,pre=DURATION][,post=DURATION]`. Durations use
/// the same syntax as a scenario delay (`250us`, `2ms`); with no `post`, the
/// window runs to the end of the record.
pub fn parse_trigger(spec: &str) -> Result<litz_board::probe::Trigger, String> {
    use litz_core::Level;
    let (channel, rest) = spec
        .split_once('=')
        .ok_or_else(|| format!("trigger {spec:?} needs CHANNEL=high|low"))?;
    let mut parts = rest.split(',');
    let level = match parts.next().unwrap_or("").trim() {
        "high" | "1" => Level::High,
        "low" | "0" => Level::Low,
        other => {
            return Err(format!("trigger level {other:?} is not high or low"));
        }
    };
    let mut pre = 0;
    let mut post = 0;
    for option in parts {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| format!("trigger option {option:?} needs key=value"))?;
        match key.trim() {
            "pre" => pre = crate::config::parse_duration(value.trim())?,
            "post" => post = crate::config::parse_duration(value.trim())?,
            other => {
                return Err(format!("trigger option {other:?} is not pre or post"));
            }
        }
    }
    let channel = channel.trim();
    if channel.is_empty() {
        return Err("trigger needs a probe name".into());
    }
    Ok(litz_board::probe::Trigger {
        channel: channel.to_string(),
        level,
        pre_ticks: pre,
        post_ticks: post,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged(name: &str, source: &str) -> StagedScript {
        StagedScript {
            name: name.into(),
            bytes: source.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_multi_file_paste_writes_every_module_then_runs_the_entry() {
        let scripts = [
            staged("main.py", "import util\nprint(util.N)\n"),
            staged("lib/util.py", "N = 42\n"),
        ];
        let pairs: Vec<(&str, &[u8])> = scripts
            .iter()
            .map(|script| (script.name.as_str(), script.bytes.as_slice()))
            .collect();
        let text = crate::python::stage_program("main.py", &pairs);
        assert!(text.contains("_litz_stage('main.py', (b'import util\\nprint(util.N)\\n'))"));
        assert!(text.contains("_litz_stage('lib/util.py', (b'N = 42\\n'))"));
        assert!(text.ends_with("exec(open('main.py').read())\n"));
        assert!(text.contains("os.mkdir(cur)"), "directories are created");
    }

    #[test]
    fn a_staged_path_may_not_escape_the_device_directory() {
        assert_eq!(staged_name("lib/util.py").unwrap(), "lib/util.py");
        for bad in ["/boot.py", "../main.py", "lib//x.py", "", "lib/./x.py"] {
            assert!(staged_name(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_trigger_spec_names_a_channel_a_level_and_a_window() {
        let trigger = parse_trigger("sck=high,pre=250us,post=2ms").unwrap();
        assert_eq!(trigger.channel, "sck");
        assert_eq!(trigger.level, litz_core::Level::High);
        assert_eq!(trigger.pre_ticks, 250_000);
        assert_eq!(trigger.post_ticks, 2_000_000);
        let plain = parse_trigger("tx=low").unwrap();
        assert_eq!(plain.level, litz_core::Level::Low);
        assert_eq!((plain.pre_ticks, plain.post_ticks), (0, 0));
        assert!(parse_trigger("sck").is_err());
        assert!(parse_trigger("sck=sideways").is_err());
        assert!(parse_trigger("sck=high,speed=1").is_err());
    }
}
