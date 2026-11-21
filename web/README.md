# Litz web front end

A single-page simulator pane: it loads the Litz WebAssembly module, boots a
firmware image against a `diagram.json`, draws the schematic, and streams the
serial output. Plain HTML, CSS and one ES module. No package manager, bundler,
framework or CDN.

## Serve it

From the repository root:

```sh
cargo run -p litz-cli -- serve          # http://127.0.0.1:8000/
```

`litz serve` answers out of `web/` with the content types a browser needs
(`application/wasm` above all) and refuses paths that climb out of the
directory. Any static server works as well — `python3 -m http.server` from this
directory, for instance — as long as it serves `.wasm` as a byte stream. Opening `index.html` directly from disk does
not work: `fetch` and WebAssembly need an HTTP origin.

## What the page expects

At serve time, next to this README:

| Path | Needed for |
|---|---|
| `litz_wasm.wasm` | the simulator |
| `demo/diagram.json` | the **Blink** demo |
| `demo/blinky.bin` | its application image |
| `demo/button/diagram.json` | the **Button** demo |
| `demo/button/button.bin` | its application image |

The demo image is app-only: no mask ROM, bootloader or partition table is
supplied or needed. If either demo file is missing, the page shows file inputs
for a `diagram.json`, an application image, and optional mask ROM, bootloader
and partition-table images.

Build both from the repository root. The script also generates the demo
firmware and copies the module out of whatever `CARGO_TARGET_DIR` is in force:

```sh
./web/build.sh
```

The module exports a plain C ABI, documented in `crates/litz-wasm/src/lib.rs`:
`litz_alloc`, `litz_free`, `litz_new`, `litz_drop`, `litz_last_error`,
`litz_run`, `litz_ms`, `litz_conflict_count`, `litz_conflict`,
`litz_serial_take`, `litz_serial_write`, `litz_part_state`, `litz_part_value`,
`litz_press`, `litz_set_control`, `litz_part_count`, `litz_part_id`, the
`litz_trace_*` exports, and `memory`.

`web/build.sh` also regenerates `demo/blinky.bin` from `litz-fixture`, so the
firmware cannot drift from its source. Neither generated file is committed.

## Controls

- **Speed** runs 100k, 400k or 1.6M simulator steps per animation frame.
- **Pause / Run** stops and resumes stepping without dropping the session.
- **Reset** drops the handle and boots the same diagram and images again.
- **Demo** switches between Blink (firmware toggles GPIO8) and Button (the LED
  follows a pushbutton on GPIO0). Starting a demo drops the running session.
- Click and hold a `litz-pushbutton` to press it; releasing, leaving the part,
  or cancelling the pointer releases it. The Button demo is the one where this
  visibly changes the circuit.
- **Clear** empties the serial pane only.

The serial pane is two-way: a typed line is sent through
`litz_serial_write` into UART0 receive, so an echo or a REPL answers it.

## Workbench

`web/workbench/` is the circuit editor and simulator: a component picker, a
wiring canvas on a 20 px grid, an inspector, a project panel, a firmware panel,
a diagnostics list and a serial monitor. It edits a `.litz` project in memory,
saves and opens it (File System Access API where the browser has it, download
and file input otherwise), imports or exports `diagram.json`, and
runs firmware against the diagram.

**Run `./web/build.sh` before using the workbench simulator.** It builds
`web/litz_wasm.wasm` and `web/workbench/litz_wasm.wasm` and regenerates the
demo firmware. The workbench page asks for `../litz_wasm.wasm`; since
`litz serve web/workbench` uses that directory as its server root (and refuses
paths that climb out of it), the build places a copy inside it too.

```sh
cargo run -p litz-cli -- serve web/workbench   # http://127.0.0.1:8000/
```

Workbench controls:

- **Run / Pause** starts and stops stepping. **Reset** drops the session and
  boots the same diagrams and images again.
- **Speed** steps 100k, 400k or 1.6M simulator steps per animation frame.
- **Firmware → Image kind** selects application-only, application plus
  bootloader and partition table, or ROM plus bootloader and partition table.
  The optional bootloader and partition table must be supplied together.
- **Application** accepts a raw `.bin` or an Intel HEX `.hex`. HEX is detected
  from the content and converted to bytes in the page; a `.hex` file that fails
  validation is reported rather than treated as binary. **ROM**,
  **Bootloader** and **Partition table** accept the same formats.
- **Load & Run** builds a session. If the circuit has no board or a required
  image is missing, the firmware panel and the status line list what is
  missing.
- The status line shows simulated milliseconds, the electrical fault count and
  whether the session is running or paused.
- The serial monitor appends UART0 output and accepts typed lines, which are
  sent through `litz_serial_write`.
- While a session is loaded, click and hold a pushbutton to press it, click a
  slide switch to throw it, and drag a potentiometer or rotary encoder to move
  its control. LEDs and buttons on the canvas show the state the simulator
  reports.

The picker reads `web/workbench/catalog.json`. Regenerate it after changing a
builtin manifest:

```sh
cargo run -p litz-cli -- catalog --json > web/workbench/catalog.json
```

The document model, wire router and catalog index are plain ES modules with no
DOM dependency, so they run under Node:

```sh
cd web/workbench && node --test test/*.test.js
```

## Schematic notes

Parts are laid out from `left`/`top` when present, otherwise automatically: a
`board-*` part goes on the left and the remaining parts form a column to its
right. Only pins named by connections are drawn. Connections to `$rail` and
`$host` endpoints are shown as dashed external boxes on the far right. Unknown
part types get a generic 60x40 box with one pin marker per connected endpoint.