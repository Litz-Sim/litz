# Build recipes — `litz.build.toml`

Litz does not compile firmware. Toolchains change faster than a simulator should, and embedding an Arduino core or an ESP-IDF release would tie every project to Litz's release schedule. A project brings its own recipe instead: `litz.build.toml`, an ordered list of shell steps shaped like a CI job.

`litz build` runs the recipe. `litz test --build` and `litz run --build` run it first and then the command. The recipe is optional: a project that already has a firmware image needs none, `run` and `test` read one only when `--build` is given, and `check` never reads one.

## A recipe

```toml
[build]
# Every step runs here unless it names its own workdir.
workdir = "firmware"
# Fail the build if this file is missing after the last step.
firmware = "../build/blink.bin"

# Environment for every step; a step's own env wins on a collision.
[build.env]
FQBN = "esp32:esp32:esp32c3"

[[step]]
name = "compile"
run = "arduino-cli compile --fqbn $FQBN --output-dir build ."
timeout = "5m"

[[step]]
name = "collect"
run = "cp build/*.bin ../build/blink.bin"
env = { STRIP = "1" }
```

An ESP-IDF project is the same shape:

```toml
[build]
workdir = "."
firmware = "build/hello_world.bin"

[[step]]
name = "idf build"
run = "idf.py build"
timeout = "20m"
```

## Fields

| Key | Where | Meaning |
|---|---|---|
| `workdir` | `[build]`, `[[step]]` | Directory for the step, relative to the project. Defaults to the project directory; a step's value wins. A value containing `..` is refused, and the resolved path must stay inside the project. |
| `env` | `[build]`, `[[step]]` | Environment variables. `[build.env]` is the base; a step's `env` overrides one key. |
| `firmware` | `[build]` | A file that must exist after the last step. Relative to the project directory, not to `workdir`. |
| `name` | `[[step]]` | Label in the transcript. Defaults to the command. |
| `run` | `[[step]]` | The command, run by `sh -c` on Unix and `cmd /C` on Windows. Required and may not be empty. |
| `timeout` | `[[step]]` | Wall-clock limit such as `90s`, `500ms` or `20m`. A bare number is seconds; `ms`, `us`, `s` and `m` are accepted suffixes, and fractional values are allowed. A step without one has no limit of its own. |

A recipe needs at least one `[[step]]`. Steps run in file order; the first failure stops the recipe and is reported with its exit code. The transcript prefixes each step with `[build N/M] <label>`.

## Commands

```sh
litz build [directory]             # run the recipe
litz build [directory] --dry-run   # load the recipe, print the steps and exit
litz test  [directory] --build     # recipe, then the tests
litz run   [directory] --build     # recipe, then the bounded run
```

The directory defaults to the current directory. The recipe is read from `litz.build.toml` in that directory. `--dry-run` still parses and validates the recipe and its timeouts, but runs no command and does not check the `firmware` path. A recipe failure stops `run` or `test` before that command starts.

`litz build` exits 0 when every step succeeded and the `firmware` file exists, 1 when a step failed, the output is missing, or the recipe is missing or invalid, and 2 for an unknown option.

## Toolchain detection

`litz toolchain` reports the firmware toolchains on the host machine. `litz doctor` is an alias. It reads `PATH` and `$IDF_PATH`, runs only version-style probes with a three-second limit each, makes no network calls, and installs nothing. It exits 0 even when tools are missing, so a diagnostic script can read the table without branching. It takes no directory; `--json` prints `{"tools": [...]}`, and an unknown option exits 2.

| Tool | How it is detected |
|---|---|
| `arduino-cli` | on `PATH`; version from `arduino-cli version` |
| `arduino core esp32` | `arduino-cli core list`, reading the `esp32:esp32` row |
| `idf.py` | on `PATH`, or `$IDF_PATH/tools/idf.py`; version from `idf.py --version` |
| `riscv32-esp-elf-gcc` | on `PATH` |
| `xtensa-esp32-elf-gcc` | on `PATH` |
| `xtensa-esp32s3-elf-gcc` | on `PATH` |
| `xtensa-esp32c3-elf-gcc` | on `PATH` |
| `arm-none-eabi-gcc` | on `PATH` |
| `avr-gcc` | on `PATH` |
| `avrdude` | on `PATH` |
| `micropython` | on `PATH` |
| `circuitpython` | on `PATH`; the note points at `tools/fetch-circuitpython.sh` for firmware images |
| `mpremote` | on `PATH`; the note says it is for boards over USB serial |
| `rustc` | on `PATH`; version from `rustc --version` |
| `rust target wasm32-unknown-unknown` | `rustup target list --installed`, else a directory under the `rustc` sysroot |

Each JSON row carries `name` and `found`, plus `path`, `version` and `note` when they are known. A missing tool has `found: false` and no other fields.

`litz editor --vscode [directory] [--force] [--gdb-port N]` is the companion command: it writes `.vscode/launch.json` and `.vscode/tasks.json` so F5 starts `litz run . --gdb <port>` as a background task and attaches a `cppdbg` launch configuration to `127.0.0.1:<port>`. The port defaults to 3333. Without `--force` an existing file is refused; with `--force` the Litz entries are merged and unrelated configurations and tasks are kept.

## Firmware images

A run needs an image for every builtin-chip board. A board whose `[board.simulator]` is a wasm component may omit `firmware`; the guest's own boot code starts whatever it carries in flash. Everything else names `firmware` under `[sim]` for a single-board project or under each `[[board]]` for a multi-board project. `litz build` checks only that the file named by `[build] firmware` exists; it does not validate the image.

The runner classifies an ESP image by its bytes, not by its file name:

- A merged image has the ESP image magic `0xE9` at byte 0 and the partition-table magic `0xAA50` at `0x8000`, or at `0x7000` for a classic ESP32 factory image whose flash base is `0x1000`. A file with an ESP image header at `0x10000` is also merged even if the first test does not match.
- An application image is anything else that is at least four bytes long. A bare application has its image header at byte 0 and nothing at `0x8000`.

A merged image is written in one piece at the chip's bootloader offset (0 on most parts, `0x1000` on the classic ESP32 and `0x2000` on the ESP32-S31) and the mask ROM starts it. It already contains the bootloader, partition table and application, so naming `bootloader` or `partition_table` alongside it is an error. A merged image needs the mask ROM.

An application image alone is written at the application offset (`0x10000` on the ESP parts) and started directly. If separate `bootloader` and `partition_table` files are named, they are written at the chip's bootloader and partition-table offsets and the bootloader finds the application. The two go together or not at all; one without the other is an error, and a named bootloader also requires the mask ROM. A missing mask ROM is reported with the board id.

The mask ROM comes from `rom` in the board's `[[board]]` entry, then `[sim] rom`, then `$LITZ_ROM_DIR` joined with the chip's file name: `esp32_rev300_rom.elf`, `esp32c3_rev3_rom.elf`, `esp32c6_rev0_rom.elf`, `esp32s3_rev0_rom.elf`, `esp32p4_rev0_rom.elf` or `esp32s31_rev0_rom.elf`. Espressif publishes these under Apache-2.0; they are not vendored.

Flash size comes from the board section's `flash_size`, then `[sim] flash_size`, then the board manifest's `flash_size`, then 4 MiB. A board whose MCU is an RP2040 takes a flat dump of QSPI flash starting at the XIP window `0x1000_0000`: the file is written at flash offset 0 with no header, its first 256 bytes are stage 2, and the application's vector table must be the 256 bytes after that at `0x1000_0100`. A Pico SDK `boot2` image is already in that shape; `tools/fetch-micropython.sh` verifies the Pico UF2 and converts it.

`elf` optionally names the firmware's symbol table. The image runs without it, but it is what lets the machine start a Bluetooth controller inside the guest and what gives logs and the debugger names. `litz.lock` records the firmware hash and the catalog entries; a changed image or catalog is refused unless `--update-lock` is passed.

## MicroPython and CircuitPython staging

A project can run a stock Python interpreter instead of its own application. Set `runtime` to `"micropython"` or `"circuitpython"` in `[sim]` or in a `[[board]]` entry, and name the source that should run:

```toml
[sim]
firmware = "../../build/micropython/ESP32_GENERIC_C3-20260824-v1.29.0.bin"
rom = "build/micropython/esp32c3_rev3_rom.elf"
runtime = "micropython"
script = "main.py"
flash_size = "4MB"
```

`script` is one project-relative file. `scripts` is an array of files; each entry is either `path` or `name = path`. A bare path keeps its relative path on the device, so `"lib/util.py"` is written to `/lib/util.py` and its directories are created. A `name = path` entry stages the project file `path` under the device name `name`. Every staged name must be a relative path with no empty segment, `.` or `..`, and two entries may not stage the same name. A single file is limited to 64 KiB, and the staged files together are limited to 64 KiB because they are pasted through the raw REPL.

The runner waits for the friendly prompt `>>> ` on the board's console, bounded by the test's `timeout` or the scenario's per-wait `timeout`, both defaulting to ten seconds of simulated time, never a wall clock. It then sends Ctrl-C, Ctrl-A (`0x01`) to enter the raw REPL, the script bytes, and Ctrl-D (`0x04`) to compile and run them. A prompt that never appears is reported as an error rather than a failed assertion.

One staged file is pasted directly. Several files are pasted as a bootstrap that writes each file with `open(..., 'wb')`, creates parent directories, and then executes the entry with `exec(open(entry).read())`. No `boot.py` is used or needed. For MicroPython the first staged file is the entry; for CircuitPython the entry must be `code.py` or `main.py` and is moved to the front whatever order it was written in. CircuitPython starts its own `code.py` on boot, so the interrupt is what stops that program before the paste; a stock MicroPython boot lands at the prompt with nothing to interrupt.

The runtime decides whether a script is staged. The exact interpreter is then detected from an explicit `runtime = "circuitpython"` or from a firmware file name or firmware bytes that contain `circuitpython` (case-insensitive); otherwise it is MicroPython. `script` and `scripts` are refused with `runtime = "native"`, because a native firmware has no REPL to paste them into. With a Python runtime and no script, the firmware simply runs and `expect`, `fail` and scenario steps read its console as usual.

The firmware image itself must be a MicroPython or CircuitPython build for the board's chip. MicroPython publishes ESP images as merged images, so they need the mask ROM; CircuitPython publishes a raw `.bin` for the ESP family. `tools/fetch-micropython.sh` downloads the pinned MicroPython images and the ESP mask ROMs, and `tools/fetch-circuitpython.sh VERSION CHIP` downloads a CircuitPython image. Both are build steps; the runner never downloads anything.

## What it is not

A step runs with the user's permissions. The recipe is code, like a `Makefile` or a GitHub Actions `run:` line, and Litz does not sandbox it. The path rule only keeps a mis-typed `workdir` from walking out of the project; it does not constrain what the command itself does.

Nothing is downloaded or cached by Litz. If a step needs `arduino-cli`, `idf.py`, `avr-gcc` or `cargo`, that tool must already be on the machine, and the error from the shell is passed through unchanged.

## CI

```yaml
- run: cargo install --path crates/litz-cli
- run: litz toolchain
- run: litz test --build --junit report.xml
```

For a project that keeps its own Litz binary, the same recipe runs unchanged on a developer machine, a container, or a GitHub runner, because it is only a list of shell commands.

`litz build --push` runs the same recipe and then hands the resulting image to a `litz connect-editor` link. It is the local loop: edit in the page, build from a terminal, and the page boots the new image without a file dialog.
