# Examples

Several projects, deliberately different in what they need.

`blinky` runs a firmware this repository generates. `cargo test`-style, no assets:

```sh
cargo run -p litz-cli -- test examples/blinky     # LED on GPIO8 blinks, exit 0
cargo run -p litz-cli -- run examples/blinky      # stream the serial output
```

Its `firmware/blinky.bin` is 272 bytes of hand-assembled RV32IMC built by `litz-fixture`: it configures GPIO8, toggles it and marks each edge on UART0. It needs no ESP-IDF toolchain, no RISC-V toolchain and no mask ROM, and it boots straight from an app image. Regenerate it with:

```sh
cargo run -p litz-fixture -- blinky examples/blinky/firmware/blinky.bin --period-ms 100
```

`rv32` is the same firmware on a different MCU: `board-litz-rv32` names a WebAssembly component instead of a builtin chip, and `crates/litz-chip-rv32` provides one. Nothing in the diagram, the firmware or the test changes — that is the point of the contract.

```sh
cargo run -p litz-cli -- test examples/rv32
```

`button` holds a pushbutton on the `litz.toml` input schedule and expects the LED and the serial line to follow:

```sh
cargo run -p litz-cli -- test examples/button
```

`two-boards` is a diagram with two ESP32-C3 boards and no parts between them on the wire that matters: board `a` runs a firmware that holds GPIO8 low, board `b` runs the button firmware and reads that pin on GPIO0. The LED on b's GPIO8 lights because the other chip holds the wire. Each board names its own image under `[[board]]`, which is how a diagram with more than one MCU boots:

```sh
cargo run -p litz-cli -- test examples/two-boards
```

`signal-generator` is a circuit with no firmware driving anything: a `litz-signal-generator` part holds a 5 Hz square on its output, the LED follows it, and the `[[probe]]` on `gen:OUT` makes the run report the measured frequency, duty and pulse width:

```sh
cargo run -p litz-cli -- test examples/signal-generator
```

`echo` is the console in the other direction: `write-serial` feeds UART0 RX at 50 ms and the test expects the firmware to send the bytes back.

```sh
cargo run -p litz-cli -- test examples/echo
```

`pot` is a potentiometer on GPIO0 and an LED on GPIO8, with a scenario that sets the knob and expects the pin to follow:

```sh
cargo run -p litz-cli -- run examples/pot --scenario examples/pot/scenario.yaml
```

`uno-blink` runs six bytes of hand-assembled AVR on an Arduino Uno board: `sbi DDRB,5`, `sbi PORTB,5`, `rjmp .`. It exercises the in-house ATmega328P core and the board's onboard D13 LED with no Arduino runtime and no `avr-gcc`.

```sh
cargo run -p litz-cli -- test examples/uno-blink
```

`pico-blink` is the same idea on the RP2040: 405 bytes of hand-assembled Thumb-1, laid out the way a Pico boots — a 256-byte stage 2 that the ROM copies into SRAM, then an application with its own vector table that prints on UART0 (GP0) and blinks GP15. Regenerate it with:

```sh
cargo run -p litz-fixture -- pico examples/pico-blink/firmware/pico-blink.bin --gpio 15
cargo run -p litz-cli -- test examples/pico-blink
```

`pico-c-systick` is the same board running C the way ordinary developers compile it: `main.c` and `vectors.S` are built by `arm-none-eabi-gcc -mcpu=cortex-m0plus -O2`, the SysTick handler lights GP15 and prints a count, and the division in the printout pulls in libgcc. The image is committed, so the test needs no ARM toolchain; `build.sh` regenerates it.

```sh
cargo run -p litz-cli -- test examples/pico-c-systick
```

`pico-c-i2c` puts an LM75 on the Pico's I2C0 (GP4/GP5, address 0x48) and reads its temperature from compiled C through the DW_apb controller, so a catalog part is reached over a bus rather than by a pin wiggle:

```sh
cargo run -p litz-cli -- test examples/pico-c-i2c
```

`micropython-esp32`, `micropython-s3`, `micropython-c3` and `micropython-c6` boot pinned MicroPython images, paste `main.py` over the REPL and assert on the LED. The images and the mask ROMs they need are not vendored; fetch them once:

```sh
./tools/fetch-micropython.sh
cargo run -p litz-cli -- test examples/micropython-esp32
```

`circuitpython` runs the official CircuitPython build for the ESP32-C3-DevKitM-1. It is a different interpreter from MicroPython, not a flag on the same one: it runs its own `code.py` on boot, waits at "Press any key to enter the REPL", brings the Bluetooth controller up before that, and prints over the chip's USB serial/JTAG console. Fetch it once, then run it:

```sh
./tools/fetch-circuitpython.sh 10.3.2 esp32c3
cargo run -p litz-cli -- test examples/circuitpython
```

`hello-c3` runs a real ESP-IDF `hello_world`, which is the interesting case: an unmodified vendor binary going through the mask ROM and the second-stage bootloader. That one needs two things this repository does not carry.

**A mask ROM.** Espressif publishes the C3 ROM under Apache-2.0 in `espressif/esp-rom-elfs`. Point `LITZ_ROM_DIR` at a directory holding `esp32c3_rev3_rom.elf`:

```sh
export LITZ_ROM_DIR=/path/to/roms
```

**Firmware.** Build it with ESP-IDF, or take any image you already have, and drop the three files into the project's `firmware/` directory (git-ignored):

| File | Comes from |
|---|---|
| `firmware/bootloader.bin` | the ESP-IDF build's `build/bootloader/bootloader.bin` |
| `firmware/partition-table.bin` | `build/partition_table/partition-table.bin` |
| `firmware/hello_world.bin` | `build/hello_world.bin`, under the name `litz.toml` gives |

Then:

```sh
cargo run -p litz-cli -- test examples/hello-c3
```

## MicroPython examples

The `mp-*` projects each carry a `main.py` and run it on the ESP32-C3 through the pinned MicroPython image, so they need no compiler. Run `./tools/fetch-micropython.sh` once, then any of:

```sh
cargo run -p litz-cli -- test examples/mp-blink
```

| Project | What it proves |
|---|---|
| `mp-blink` | `machine.Pin` toggles GPIO8; the LED state follows. |
| `mp-button` | A held pushbutton on GPIO0 lights the LED. |
| `mp-adc` | `machine.ADC` reads the potentiometer at mid scale. |
| `mp-servo` | `machine.PWM` at 50 Hz, 1.5 ms pulse centres a servo. |
| `mp-sevenseg` | Ten GPIOs drive a seven-segment display to `1` and `7`. |
| `mp-i2c-oled` | A minimal SSD1306 driver initialises the panel at 0x3c. |
| `mp-i2c-sensor` | An I2C register read returns the BMP280 chip id. |
| `mp-traffic` | Three lamps run a timed red/amber/green cycle. |
| `mp-uart-echo` | `machine.UART` returns bytes written to UART0. |

`run` boots the firmware and streams its serial output, exiting 42 at the time limit; `test` runs the `[[test]]` entries and exits 0 or 1; `check` validates the diagram without running anything; `serve` serves the browser simulator (`web/`).
