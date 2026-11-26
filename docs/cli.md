# CLI reference

The `litz` binary runs a firmware image against a diagram, tests it, validates the diagram, builds firmware with a project recipe, serves a directory over HTTP, manages the catalog, inspects an ELF, writes editor files and serves the Model Context Protocol. The command is implemented in `crates/litz-cli/src` and the run loop in `crates/litz-engine/src/runner.rs`. This page is the complete reference for the arguments the binary parses.

## Synopsis

```text
litz <command> [directory] [options]
litz catalog <subcommand> [arguments] [options]
litz board check [directory]
litz heap FIRMWARE.elf
litz toolchain [--json]        (alias: litz doctor)
litz editor --vscode [directory] [--force] [--gdb-port N]
litz connect-editor [directory] [--port N]
litz mcp
litz --version
```

`<command>` is `run`, `test`, `check`, `lint`, `build` or `serve`. The directory is one positional argument and defaults to `.`; options may appear before or after it. A token that starts with `-` and is not a known option is a usage error, and so is a missing value for an option that requires one. Both exit 2. The commands are listed in the table below; each has its own section after the shared options.

| Command | What it does |
|---|---|
| `litz run` | Boots the firmware and streams its serial output. |
| `litz test` | Runs the `[[test]]` entries in `litz.toml`. |
| `litz check` (`lint`) | Validates `diagram.json` against the catalog without booting. |
| `litz build` | Runs the project's `litz.build.toml` recipe. |
| `litz connect-editor` | Opens the loopback link the editor builds through. |
| `litz serve` | Serves a directory over HTTP on loopback. |
| `litz catalog` | Prints or manages the builtin and project catalog. |
| `litz board check` | Parses and instantiates a board bundle. |
| `litz heap` | Reads static RAM and heap bounds from an ELF. |
| `litz toolchain` (`doctor`) | Reports the installed firmware toolchains. |
| `litz editor` | Writes VS Code launch and task files. |
| `litz mcp` | Serves the Model Context Protocol over stdio. |

## Global options

| Option | Meaning |
|---|---|
| `-h`, `--help` | Prints usage and exits 0. Each subcommand has its own usage text. |
| `-V`, `--version` | Prints `litz <version>` and exits 0. |

## Exit codes

Every command uses the same four numbers. A command that finishes normally returns 0; a failed assertion, invalid diagram, electrical fault or I/O error returns 1; a malformed invocation returns 2; and only `run` uses 42.

| Code | Meaning |
|---|---|
| 0 | Success: tests passed, the diagram is valid, every board halted, or the command completed. |
| 1 | Failure: an assertion failed, the diagram is invalid, an electrical fault or trap occurred, or a file could not be read or written. |
| 2 | Usage error: unknown option, missing option value, or a wrong number of arguments. |
| 42 | `run` reached its simulated time limit without every board halting and without a fault. A `test` timeout is a failure (1), not 42. |

## run

`litz run [directory] [options]` boots every development board in the diagram, streams the merged serial output to stdout as it arrives, and exits when every board halts, the simulated time limit is reached, or the circuit reports a fault. The default limit is ten seconds of simulated time; a project's test can set its own with `timeout`, but `run` itself uses the default. The exit code is 0 when every board halted, 42 at the time limit, and 1 on a fault, a trap or an invalid diagram.

```sh
litz run examples/blinky
litz run examples/blinky --vcd blink.vcd --probe led1:A
```

`run` reads `[sim]` or the `[[board]]` entries for the firmware, the runtime and the image layout. A plain application image boots directly; a merged image or a bootloader needs a mask ROM named by `rom`, `[sim] rom` or `LITZ_ROM_DIR`.

## test

`litz test [directory] [options]` runs every `[[test]]` entry in the project's `litz.toml`. A test passes when every `expect` string appeared on the console, no `fail` string appeared, and every `expect_state` reached its state; `state_at` samples one instant and `state_stays` requires the state to hold for a duration. A project with no `[[test]]` entries runs one implicit test named `run` with the default timeout. The exit code is 0 when all tests passed and 1 when any failed.

```sh
litz test examples/blinky
litz test examples/blinky --json --junit report.xml
litz test examples/pot --scenario examples/pot/scenario.yaml
```

`--scenario FILE` replaces the `[[test]]` entries with a YAML automation script and can also be given to `run`. `--build` runs `litz.build.toml` before the tests. The scenario step vocabulary (`delay`, `wait-serial`, `write-serial`, `wait-pin`, `expect-pin`, `set-control`, `take-screenshot`, `touch` and its variants) is defined by `crates/litz-engine/src/scenario.rs`; `examples/pot/scenario.yaml` is a short working example.

## check and lint

`litz check [directory]` and its alias `litz lint [directory]` validate `diagram.json` against the builtin catalog plus the project's `parts/` and `boards/`. They read only the diagram: `litz.toml` and the firmware are not required. The exit code is 0 when the diagram is valid and 1 when any diagnostic is an error. Validation warnings do not fail the check unless `--strict` is given.

```sh
litz check examples/blinky
litz lint examples/blinky --strict
```

## Options for run, test and check

These options are parsed for `run`, `test`, `check` and `lint`. Options that only affect a booted run have no effect under `check`, which returns before the firmware starts.

### Output and validation

| Option | Meaning |
|---|---|
| `--json` | Prints one JSON record instead of the transcript. |
| `--junit FILE` | Also writes a JUnit XML report to `FILE`. |
| `--strict` | Treats validation warnings as errors. |
| `--warnings-as-errors` | The same as `--strict`. |
| `--update-lock` | Rewrites `litz.lock` instead of checking it. |
| `--scenario FILE` | Runs the YAML automation script instead of the `[[test]]` entries. |
| `--build` | Runs `litz.build.toml` first; consulted for `run` and `test`. |

A missing `litz.lock` is written automatically. Without `--update-lock`, an existing lock that disagrees with the catalog or the firmware is refused rather than silently run. `--scenario` replaces the mode's assertions entirely and produces the scenario's own 0 or 1 result.

### Recording and instruments

| Option | Meaning |
|---|---|
| `--vcd FILE` | Writes every net transition to `FILE` as a VCD. |
| `--pcap FILE` | Writes the simulated network's traffic as a pcap. |
| `--pcap-air FILE` | Writes every 802.11 frame as a link-type-105 pcap. |
| `--pcap-eth FILE` | Writes the station's Ethernet frames as a link-type-1 pcap. |
| `--probe [NAME=]POINT` | Watches a point for one run; repeatable. |
| `--scope FILE` | Writes every probe's record as a CSV when the run ends. |
| `--sr FILE` | Writes every probe as a sigrok session archive for PulseView. |
| `--sr-rate HZ` | Samplerate for the sigrok session; defaults to 2 000 000. |
| `--trigger SPEC` | Starts the scope window at a logic-analyzer trigger. |
| `--decode SPEC` | Reads a protocol off probe channels; repeatable. |
| `--sd-export PATH` | Saves the image of every microSD card the guest wrote. |
| `--flash-export FILE` | Saves the MCU's own flash as the firmware left it. |

A probe point is `<board>:<pin>` (`esp:GPIO8`, `esp:GP0`, `esp:3V3`, `esp:VSYS`, `esp:GND`) or `<part>:<pin>` (`led1:A`). Any header pin is a point, including the supply pins, and an unwired pad reads high impedance rather than being refused; a part that is not in the diagram and a pin the part does not have are still refused before the firmware boots. `NAME=` names the channel; a bare point keeps its own text as the name. The number of channels is not limited. `--scope` writes `time_ns` followed by one column per channel, one row per time any channel changes, with values held between changes and levels written `1`, `0` or `z`; a channel with analog samples also gets a `<name>_volts` column. `--trigger` takes `CHANNEL=high|low[,pre=DURATION][,post=DURATION]`, where the durations use the same syntax as a scenario delay (`250us`, `2ms`) and an absent `post` runs the window to the end of the record.

`--decode` takes `uart:rx=CHANNEL[,baud=N]`, `i2c:sda=CHANNEL,scl=CHANNEL` or `spi:sck=CHANNEL,mosi=CHANNEL,cs=CHANNEL[,miso=CHANNEL][,mode=N]`, optionally with a `name=` entry. Every channel must already be a probe channel, because a decoder reads the probe record and nothing else. Decoding is a report and never changes the run.

`--sr` writes the same record as a sigrok session, the `.sr` archive PulseView and `sigrok-cli` open. It samples every probe at `--sr-rate` and holds each channel at its last transition between samples; a high-impedance line is recorded low. The trigger window from `--trigger` applies the same way as it does to `--scope`.

`--pcap` needs a simulated network: a `litz-wifi-router` part in the diagram or a `[sim.wifi]` table. The file has nanosecond timestamps and is byte-identical across runs of the same firmware. `--pcap-air` and `--pcap-eth` attach to the first Wi-Fi chip. `--sd-export` writes to the path directly for one card and treats it as a directory for several; storage parts with a `save` attribute write their own image at the end of the run without it. `--flash-export` writes the whole internal array (the RP2040's 2 MiB XIP array, the ESP32 family's flash), so a firmware that stored settings in flash hands them to the next run.

### Simulation and radio

| Option | Meaning |
|---|---|
| `--analog` | Solves the analog network while the firmware runs. |
| `--coverage` | Prints the backend's instruction-coverage report after the run. |
| `--wifi SPEC` | Gives the ESP32-family radio an access point for this run. |
| `--net` | Puts host sockets behind the Wi-Fi gateway. |
| `--hostfwd SPEC` | Forwards a host port into the guest; implies `--net`. |

`--analog` turns on MNA co-simulation for one invocation, without editing `[sim] analog`. `--coverage` is a report, not an assertion: it does not change the exit code, and a backend without coverage ignores it. `--wifi` uses `ssid=NAME[,psk=PASS][,channel=N][,bssid=AA:BB:CC:DD:EE:FF][,epoch=SECONDS]`; `pass`, `password`, `chan` and `ch` are accepted as aliases, and the flag overrides `[sim.wifi]`. A radio only exists on the ESP32-family chips, so an `atmega328p` board refuses the option. `--hostfwd` takes `tcp:[host:]port-[guest:]port`. `--net` is non-deterministic by nature and is intended for interactive work, not CI.

### Interaction and debugging

| Option | Meaning |
|---|---|
| `--interactive[=BOARD]` | Forwards lines typed on stdin to that board's console. |
| `--screenshot [BOARD@]FILE` | Writes a board's display as a PNG. |
| `--gdb [BOARD@]PORT` | Serves the GDB remote protocol on `127.0.0.1:PORT`. |
| `--break ADDR` | Stops the first run at a program counter. |

`--interactive` without a board targets the first board. The board must exist in the diagram; a name that does not is refused before boot. `--screenshot` captures the display at the end of a `run` and when a test fails; without a board it captures the first board with a framebuffer, and a circuit with none is a note on stderr that leaves the exit code alone. A prefix is read as a board only when it has no path separator, so `shots/a@2x.png` is a path while `b@shot.png` names board `b`.

`--gdb` serves one board's chip once it is booted and returns when the client disconnects, after which the run continues; a session owns its chip's clock while attached. A board name picks the chip, and without one the first backend with a debug target is served. The RISC-V adapters have a GDB target; the Xtensa and AVR backends and a WebAssembly MCU refuse. `--break` takes a hexadecimal address (`0x40380000`) or a decimal one and stops the first run at that program counter; combine it with `--vcd` to inspect the trace.

### Heap sampling

| Option | Meaning |
|---|---|
| `--heap` | Asks the firmware for its free heap at the end of the run and prints it. |
| `--heap-interval MS` | Samples the free heap every `MS` of simulated time. |
| `--heap-min-free N` | Fails the run when the free heap drops below `N` bytes. |
| `--heap-csv FILE` | Appends `part,time_s,free,min_free` samples to `FILE`. |

Sampling uses the firmware's own getter, so the numbers are the ones the firmware sees. The default interval is 1000 ms. A bare `--heap` is informational: a firmware that cannot answer produces a note, not a failure. `--heap-min-free` is a gate, and a firmware that cannot answer then fails the run. `--heap-interval`, `--heap-min-free` and `--heap-csv` each turn sampling on.

## build

`litz build [directory] [--dry-run] [--push [--board ID]]` runs the project's `litz.build.toml` recipe. The recipe is an ordered list of shell steps with an optional working directory, environment and timeout; it is not a sandbox and runs with the user's permissions. The exit code is 0 when every step succeeded and the declared `firmware` file exists, 1 when a step failed, the output is missing, or the recipe is missing or invalid, and 2 for a usage error.

| Option | Meaning |
|---|---|
| `--dry-run` | Loads and validates the recipe, prints each step and exits without running anything. |
| `--push` | After a successful build, hands the image to the link `litz connect-editor` opened. |
| `--board ID` | Names the `[[board]]` whose image is pushed; defaults to the recipe's `firmware`. |

`--dry-run` still parses the recipe and its timeouts but runs no command and does not check the `firmware` path. The recipe format is documented in [Building firmware](build.md). `litz test --build` and `litz run --build` run the same recipe first.

## connect-editor

`litz connect-editor [directory] [--port N]` opens a loopback link for the Litz editor page. It prints an attach URL and a pairing code:

```text
litz: editor link ws://127.0.0.1:4317/ws
litz: pairing code B82E-4414-23F7-48D9
```

Paste the URL with the code appended as its fragment, `ws://127.0.0.1:4317/ws#B82E-4414-23F7-48D9`, into the editor's **Connect to CLI** dialog. The fragment never reaches the daemon, so the code is not in any server log. The link stays open until Ctrl-C.

The page sends one of two messages: `build` runs this project's recipe and streams its transcript line by line, and then announces the image it produced by SHA-256; the page fetches those bytes from `/artifact/<sha256>`. Nothing else is accepted, and no message carries a command string: the link is not a shell. Every route needs the code, and the listener binds `127.0.0.1` only.

The code is also written to `.litz/editor.json` (mode 0600) so that, from another terminal, `litz build --push` can hand a freshly built image to the connected page without going through the recipe a second time. Without a running link the build still succeeds and prints that it was not pushed.

| Option | Meaning |
|---|---|
| `--port N` | Port to listen on; defaults to 4317. |

The link is a development convenience on the same machine. Moving a project between devices is a storage question, not this one.

## serve

`litz serve [directory] [--port N]` serves one directory over HTTP. It binds to `127.0.0.1` only, answers GET requests, serves `index.html` for a bare path, and refuses a request that escapes the directory. It is a plain static file server with no simulator behind it, intended for a page that loads the WebAssembly build.

| Option | Meaning |
|---|---|
| `--port N` | Port to listen on; defaults to 8000. Port 0 asks the system to choose one. |

The command runs until interrupted. It prints the URL when the listener is bound and returns 0 on a clean stop, or 1 when the port cannot be bound.

## catalog

`litz catalog` is a command group. With no subcommand it prints the builtin catalog as a table; with `--json` it prints the builtin catalog as a JSON array. `list` and `check` take a project directory; `add`, `new` and `remove` act on the project in the current directory.

| Subcommand | What it does |
|---|---|
| `litz catalog` | Prints the builtin catalog. |
| `litz catalog list [directory]` | Prints the builtin catalog plus the project's `parts/` and `boards/`, with a source column. |
| `litz catalog check [directory]` | Parses every manifest in `parts/` and `boards/` and reports the bad ones. |
| `litz catalog add BUNDLE` | Installs a bundle into the project catalog. |
| `litz catalog new ID` | Writes a starter part manifest. |
| `litz catalog new board ID` | Writes a starter board bundle. |
| `litz catalog remove ID` | Removes an installed manifest and the module only it names. |

| Option | Meaning |
|---|---|
| `--json` | Prints the builtin catalog as JSON. It applies to the form with no subcommand. |
| `--dir DIR`, `--dir=DIR` | Catalog subdirectory or install destination; defaults to `parts`, or `boards` for `new board`. |
| `--name NAME`, `--name=NAME` | Display name for `new` and `new board`. |
| `-h`, `--help` | Prints catalog usage and exits 0. |

`add` reads the whole bundle and checks it before writing any byte, refuses an id that is already builtin and an id already installed, and places a board under `DIR/<id>` so its manifest, module and schematic stay together. `new ID` writes `DIR/<id>.toml`; `new board ID` writes a complete bundle under `DIR/<id>/` and adds a `board-` prefix when the id lacks one. `remove` refuses a builtin id and keeps a module another manifest still names. The exit code is 0 on success, 1 when a manifest cannot be read or written, and 2 for a usage error or an unknown subcommand.

## board check

`litz board check [directory]` checks a board bundle on its own, without a project. It parses `board.toml`, resolves the onboard schematic and the simulator module beside it, and instantiates the WebAssembly component under the ABI the manifest names. The directory defaults to `.`. The exit code is 0 when the bundle parses and instantiates, 1 when it does not, and 2 when the subcommand is missing or the arguments are wrong.

```sh
litz catalog new board example      # writes boards/board-example/
litz board check boards/board-example
```

A bundle is written by `litz catalog new board`; its `simulator/build.sh` builds the component the check loads.

## heap

`litz heap FIRMWARE.elf` reads the image's section table and symbol table directly, without running anything. It reports the allocated RAM sections, their total, and the heap bounds a linker script left as `_heap_start`/`_heap_end` (or the double-underscore and unprefixed spellings). The exit code is 0 on success, 1 when the file cannot be read or is not an ELF image, and 2 when the argument count is wrong.

```sh
litz heap firmware/app.elf
```

| Option | Meaning |
|---|---|
| `-h`, `--help` | Prints usage and exits 0. |

Runtime free heap is a different facility: sample it with `litz run --heap`. This subcommand reads only the image, so its result does not depend on booting the firmware.

## toolchain and doctor

`litz toolchain [--json]` and its alias `litz doctor [--json]` report the firmware toolchains on this machine, one line per tool: `arduino-cli` and its ESP32 core, `idf.py` (from `PATH` or `$IDF_PATH`), the ESP32 cross compilers, `arm-none-eabi-gcc`, `avr-gcc`, `avrdude`, `micropython`, `circuitpython`, `mpremote`, `rustc` and the `wasm32-unknown-unknown` target. Detection is read-only: it reads `PATH` and `$IDF_PATH`, runs version probes with a short timeout, makes no network calls and installs nothing.

| Option | Meaning |
|---|---|
| `--json` | Prints `{"tools": [...]}` instead of the table. |
| `-h`, `--help` | Prints usage and exits 0. |

The command prints a report and exits 0 even when tools are missing. A bad option or a positional directory exits 2.

## editor

`litz editor --vscode [directory] [--force] [--gdb-port N]` writes `.vscode/launch.json` and `.vscode/tasks.json`. The launch configuration is a `cppdbg` session pointed at `127.0.0.1:PORT`, and the task runs `litz run . --gdb PORT` in the background so the firmware boots, the GDB stub starts and the serial output stays in the task's terminal.

| Option | Meaning |
|---|---|
| `--vscode` | Selects VS Code, the only editor target; required. |
| `--force` | Merges the Litz entries into existing files and keeps unrelated configurations and tasks. |
| `--gdb-port N`, `--gdb-port=N` | Port for the GDB stub; defaults to 3333. |
| `-h`, `--help` | Prints usage and exits 0. |

The directory defaults to `.`; a second directory is a usage error, as is a missing `--vscode` or a non-numeric port. Without `--force`, an existing `launch.json` or `tasks.json` is refused and nothing is written. The command exits 0 when the files are written, 1 when a file cannot be written, and 2 for a usage error.

## mcp

`litz mcp` serves the Model Context Protocol over stdin and stdout as JSON-RPC 2.0, one message per line. It takes no arguments; `litz mcp --help` prints the CLI usage, and any other argument exits 2. The process reads requests until EOF and exits 0 on a clean shutdown, or 1 on an I/O failure. It writes nothing to stdout except protocol messages; diagnostics go to stderr.

The server implements `initialize`, `notifications/initialized`, `ping`, `tools/list` and `tools/call`. Its tools are `litz_check`, `litz_test`, `litz_run`, `litz_circuit_create`, `litz_circuit_update`, `litz_code_generate` and `litz_import_diagram`. The protocol details and tool arguments are in [MCP server](mcp.md).

## Machine-readable output

`--json` prints one object derived from the same result the exit code comes from. For `test` it carries one entry per test; for `run` and `check` the `tests` array is empty.

| Field | Meaning |
|---|---|
| `status` | `passed` or `failed`. |
| `reason` | `ok`, `failed`, `all tests passed`, `electrical fault`, `assertion failed`, `failure text matched` or `timed out`. |
| `exit_code` | The process exit code. |
| `sim_time_ms` | Simulated milliseconds the run reached. |
| `tests` | One object per test with `name`, `status`, `exit_code`, `sim_time_ms` and `output`. |

`--junit FILE` writes `<testsuites>` with one `<testcase>` per `[[test]]`, a `<failure message="exit N">` for each failure and the transcript as `<system-out>`. The suite and class names are `litz`. An XML-illegal control character from a serial log is replaced rather than encoded.

## Instrumentation and the run record

A VCD captures net transitions rather than samples, so a value holds between changes and no point is recorded that the circuit was never in. A probe channel is the same step waveform, and its analog column appears only when `--analog` gives the solve a voltage. The probe report printed after a run gives frequency from rising edges, duty, edge count, pulse range and, when present, the voltage range. Protocol decoding reads that record and adds no load to the circuit.

Electrical faults are reported as `litz: electrical fault on net N: <detail>` after the serial output and fail the run whether or not a test was asserted. A core trap is a stop, not a slow boot, and is reported instead of waiting for the timeout. Probe summaries, part-state notes and decode frames are appended to the transcript with the `litz:` prefix; coverage lines are appended as the backend prints them, so a CI consumer can separate them from firmware output.
