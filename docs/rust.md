# Using Litz from Rust

Litz is a Rust workspace before it is a CLI or a JavaScript package. The same crates that boot `examples/blinky` in the terminal can be linked into a test harness, an editor, a service or another simulator. This page covers the two entry points — an in-memory boot and a project runner — and the extension points for parts and chips. The JavaScript equivalent is in the [JavaScript reference](javascript.md).

## Which crate

| Crate | What it gives you |
|---|---|
| `litz-core` | The vocabulary: `Bus`, `Mcu`, `Part`, `Level`, `Drive`, `PartState`, `Tick`. No chip, no board, no files. |
| `litz-diagram` | `Circuit`: parses and validates `diagram.json`. |
| `litz-catalog` | `Registry`: builtin and project manifests, board expansion. |
| `litz-board` | `FlatBoard`, `Board`, the builtin parts, buses, probes, the analog solver, and the `Chip` trait. |
| `litz-engine` | Project loading, `machine::boot`, the runner and assertions, decoders and scenarios. This is the crate a host normally depends on. |
| `litz-esp`, `litz-avr`, `litz-rp` | The builtin chip backends: ESP32 family, ATmega328P, RP2040. |
| `litz-mcu-wasm` | `WasmChip`: a `litz:mcu` component hosted by wasmtime. Native only. |
| `litz-part-wasm` | `WasmPart`: a `litz:part` component hosted by wasmtime. Native only. |
| `litz-net`, `litz-ble`, `litz-radio` | The Wi-Fi/802.11 network, Bluetooth and packet-radio models the board attaches to its parts. |
| `litz-wasm` | The C ABI the `@litz-sim/litz` package loads. Depend on it only from a wasm host. |

A host that only needs to run a circuit usually depends on `litz-engine`; `litz_board::probe` is re-exported there so a second dependency is unnecessary.

```toml
[dependencies]
litz-engine = "0.1"
```

The version tracks the workspace. The `0.x` line makes no compatibility promise between minor versions.

## Boot a diagram in memory

`litz_engine::machine::boot` takes the `diagram.json` text and the images already in memory. It builds the board, selects the chip the diagram's board names, writes the images at the chip's own offsets and resets the core.

```rust
use litz_core::PartState;
use litz_engine::machine::{boot, BootOptions, Images};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let diagram = std::fs::read_to_string("examples/blinky/diagram.json")?;
    let app = std::fs::read("examples/blinky/firmware/blinky.bin")?;

    let mut session = boot(
        &diagram,
        Images {
            rom: &[],
            bootloader: &[],
            partition_table: &[],
            app: &app,
        },
        &BootOptions::default(),
    )?;

    let mut console = String::new();
    for _ in 0..500 {
        session.chip.run(100_000);
        console.push_str(&String::from_utf8_lossy(&session.chip.take_console()));
        if session.board.borrow().state("led1") == Some(PartState::Led { lit: true }) {
            break;
        }
    }

    println!("{console}");
    assert_eq!(session.chip.fault(), None);
    Ok(())
}
```

`Booted` has three fields: `chip` (a `Box<dyn litz_board::Chip>`), `board` (`Rc<RefCell<litz_board::Board>>`) and `circuit` (the parsed diagram). The board is shared with the chip, so the host reads part state from the same instance the chip drives.

A merged ESP image is recognised and written at the bootloader offset; a bare application goes at the chip's application offset. `rom`, `bootloader` and `partition_table` are optional, but the latter two go together because the bootloader finds the application through the table. `BootOptions` carries `flash_size`, `mac`, `analog` and `manifests`:

| Field | Meaning |
|---|---|
| `flash_size` | Bytes, or `None` for the board manifest's value and then 4 MiB. |
| `mac` | The station MAC. The default is fixed so two runs match. |
| `analog` | Solve the analog network instead of treating every net as digital. |
| `manifests` | `Vec<ManifestText>` of `parts/` or `boards/` TOML the host supplies, with the board schematic text when the manifest names one. A browser or an embedded host has no directory to load them from. |

`machine::boot` runs one board. A diagram with several development boards boots through the project runner, which gives each board its own image. `machine::boot_c3` is the C3-specific entry point kept for callers that already know the chip.

## Read and drive the circuit

Everything a UI or a test asks about the circuit is on `Board`:

| Call | Effect |
|---|---|
| `state(id)` | `Option<PartState>`: `Led { lit }`, `Button { pressed }`, `Switch { on }`, `Segments { mask }`, `Tone { sounding, hz }`, `Servo { degrees }`, `Knob { permille }`, `Radio { .. }` or `None`. |
| `press(id, pressed, now)` | Press or release a button; returns whether the part has that input. |
| `set_control(id, permille, now)` | Move a knob or a joystick axis; returns whether the part has that control. |
| `set_named_control(id, name, value, now)` | A named control such as `temperature` or `lux`. |
| `part_pin_level(id, pin)` / `chip_level_on(board, gpio)` | The resolved level on a part pin or a chip pad. |
| `input_levels()` / `take_chip_inputs()` | The levels waiting for the chip. |
| `conflicts()` | Electrical conflicts: two strong drivers disagreeing on one net. |
| `part_framebuffer(id)` / `touch_panel(x, y, down)` | One part's framebuffer and a touch at panel pixels. |
| `save_sd_images(path)` | Write every microSD card the guest wrote. |

The same surface is reachable through the chip where a backend forwards it (`Chip::state`, `Chip::press`, `Chip::gpio_level`, `Chip::display`, `Chip::touch`), so the runner and a host do not need to know which side owns the board.

Time is nanoseconds (`litz_core::Tick`), never a float. `Board::now()` is the last tick the board settled at, and every part call takes the tick it happens at.

## Run a project directory

A project directory is the same layout the CLI reads: `diagram.json`, `litz.toml`, `firmware/`, optional `parts/`, `boards/`, `litz.build.toml` and `scenario.yaml`. `litz_engine::runner` loads and runs it.

```rust
use std::path::Path;
use litz_engine::runner::{execute, Mode};

let outcome = execute(Path::new("examples/blinky"), Mode::Test, false, false)?;
print!("{}", outcome.output);
for test in &outcome.tests {
    println!("{}: {}", test.name, if test.passed { "pass" } else { "fail" });
}
assert_eq!(outcome.exit_code, 0);
```

`Mode` is `Run`, `Test` or `Check`. The boolean arguments are `strict` (promote validation warnings to errors) and `update_lock` (rewrite `litz.lock`). `Outcome` carries `exit_code`, the transcript, `sim_ms` and one `TestOutcome` per `[[test]]`. Exit codes match the CLI: `0` when every board halts or every test passes, `42` at the time limit of a run, `1` on a fault or a failed assertion.

`execute_with` adds the recording options in `runner::Options`: VCD, a breakpoint, analog, probes, scope CSV, a logic-analyzer trigger, decoders, a screenshot, GDB, pcaps, heap sampling and SD export. `Options::default()` is the plain run.

`execute_capture` is the same run without writing to stdout: it collects the console, the non-`None` part states and the board's faults, which is what a service or an MCP host wants. `execute_scenario` runs a `scenario.yaml` instead of the `[[test]]` entries.

## Probes, decoders and waveforms

A probe records the settled level of one net. Register it before the trace starts, then read every channel after the run:

```rust
use litz_board::probe;

let mut board = session.board.borrow_mut();
board.add_probe("pwm", "esp:GPIO8")?;
board.start_trace();
drop(board);

session.chip.run(10_000_000);

for channel in session.board.borrow().scope() {
    println!("{}", probe::summary(&channel));
}
```

A `Channel` holds the probe, its digital `Series` (level changes, not samples) and its analog `Series` (solved volts at each settle). `probe::measure` computes edges, high and low time, pulse widths, frequency and duty from a series, and `probe::triggered` applies a `Trigger`.

`litz_engine::decode::frames` reads a `DecodeSection` (`uart`, `i2c` or `spi`) off the channels, and `decode::check` verifies that its points exist before the run.

## Analog

With `BootOptions { analog: true, .. }` or `[sim] analog = true`, `Board` solves the circuit's node voltages alongside the digital levels. `analog_solve()` returns the current solution, `node_voltage(part, pin)` and `node_voltage_gpio(gpio)` read one node, and `analog_advance(to_ns)` steps the solver. An ADC in the chip reads the solved voltage on its channel instead of a digital level. The solver's timestep and options are in `AnalogConfig`.

## Custom parts

A native part implements `litz_core::Part`: it sees pin levels and time, never chip registers.

```rust
use litz_core::{Drive, Level, Part, PinId, Tick};

struct PullUp {
    pin: PinId,
}

impl Part for PullUp {
    fn drive(&self, pin: PinId) -> Drive {
        if pin == self.pin {
            Drive::weak(Level::High)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}
}
```

The board builds parts from the catalog manifest's engine name in `litz_board::parts::build`; the builtin engines (`led`, `resistor`, `pushbutton`, `pwm`, the sensors, the displays) are compiled in. Adding a native engine to that match is a change to `litz-board` itself, which is appropriate for a fork or an application that vendors the workspace.

For a part that ships independently, implement the `litz:part@0.2.0` contract as a WebAssembly component and let `litz-part-wasm` host it:

```rust
let part = litz_part_wasm::WasmPart::from_bytes(&wasm, "my-sensor")?;
```

A project names such a component in `part.toml`; the board loads it through the same `parts::build` path, and a `[[probe]]` or a test cannot tell it from a builtin engine. The guest has no WASI, clock, filesystem or randomness, its memory is capped and every call has a fuel budget. `WasmPart::fault` reports a trap after the fact.

`litz-part-sdk` generates the guest bindings; `crates/litz-part-74hc595`, `litz-part-eeprom` and `litz-part-display` are complete reference guests.

## Custom chips

A chip implements `litz_board::Chip`. The required methods are `info`, `load_image`, `boot`, `schedule`, `run`, `seconds`, `write_console`, `take_console`, `state`, `press`, `set_control`, `part_pin_level` and `conflicts`; recording, breakpoints, displays, touch, heap sampling, pcaps, the host gateway and GDB are default methods a backend opts into.

The contract is deliberately small: the chip drives the board with `Board::set_chip_output(gpio, drive, now)`, reads `Board::take_chip_inputs()`, and advances the shared board's clock with `Board::advance_to(now)` from inside `run`. `Chip::run` returns a `RunResult { cycles, stop }` where `Stop` is `Budget`, `Idle`, `Halted`, `Trap`, `Reset` or `Breakpoint(pc)`.

`crates/litz-chip-rv32` is a compact reference: RV32IMC plus enough of the ESP32-C3 memory map to run the repository's firmware. It is also the guest used by the `litz:mcu` contract.

A custom chip can instead be a WebAssembly component. `litz-mcu-wasm` loads both `litz:mcu@0.1.0` and `0.2.0` with wasmtime:

```rust
use std::cell::RefCell;
use std::rc::Rc;

let board = Rc::new(RefCell::new(board));
let mut chip = litz_mcu_wasm::WasmChip::from_bytes_with_abi(&wasm, "my-chip", "litz-mcu-2")?
    .with_shared_board(Rc::clone(&board), 0);
```

The manifest's `abi` field selects the contract; a 0.1 module stays loadable. Every 0.2 call — analog reads, I2C/SPI/UART transactions, Wi-Fi scan, join and TCP/UDP, BLE advertising, GATT and the raw radio — is answered by the same board state the builtin adapters use. The [file formats](formats.md) reference has the WIT contract and the manifest fields.

## WebAssembly and platform notes

`litz-core`, `litz-board`, `litz-engine` and the chip adapters compile for `wasm32-unknown-unknown`; that is how the browser package is built. Two differences:

- Xtensa execution (classic ESP32 and ESP32-S3) needs a host JIT and is not compiled into the zero-import browser module. The RISC-V ESP parts, the ATmega328P and the RP2040 are.
- `litz-mcu-wasm` and `litz-part-wasm` are native only: a browser has no wasmtime. On wasm32 the JavaScript host instantiates a jco-transpiled component and `litz-board::host::HostChip`/`HostPart` forward the calls through `litz_host`.

The analog solver, the SD model and the displays are available on both sides because the board and its parts compile on both targets.

## Determinism

Two runs of one firmware with the same inputs produce the same console bytes, pin transitions and part states on every host. There is no wall clock, no unseeded randomness and no floating point in the scheduler. Host actions take a simulated tick, so the moment a press or a touch lands is part of the input rather than a race. Anything that deliberately leaves the model — `--host-network`, the host gateway, a real serial port — says so in its options and is off by default.

## Where to look next

- [Architecture](architecture.md) explains how the crates fit together and where a new backend belongs.
- [File formats](formats.md) documents `diagram.json`, `litz.toml`, probes, decoders and the analog solver.
- [Parts and boards](parts.md) covers catalog manifests, installing a bundle and the wasm part contract.
- [JavaScript reference](javascript.md) is the same library across the wasm boundary.
