# SN76489

A custom part: the SN76489 programmable sound generator from the Master System and Mega Drive, implemented as a WebAssembly component and driven by a Pico.

```sh
cargo run -p litz-cli -- test examples/sn76489
cargo run -p litz-cli -- test examples/sn76489 --scope build/audio.csv
```

The firmware is bare-metal RP2040 C. It silences all four channels, plays a note on channel 0, walks through three more dividers, then plays a white-noise burst, printing every divider and its frequency on UART0. The test waits for `done` and for the buzzer to report a tone.

## The part

`parts/sn76489.toml` names the pins and the component; `parts/sn76489.wasm` is the committed binary. The source is in `part/`:

| File | Holds |
|---|---|
| `part/src/psg.rs` | Registers, tone dividers, the 15-bit noise shift register and the mixer, with unit tests that run on the host |
| `part/src/component.rs` | The `litz:part@0.2.0` glue: pin writes, scheduled events and the reported drive |
| `part/build.sh` | Builds the guest for `wasm32-unknown-unknown` and wraps it with `litz-componentize` |

`AUDIO` is a digital output thresholded at half the current mix level, and `analog()` reports the mix in volts, so a probe or a scope sees the waveform while a passive buzzer only needs the edges.

## The circuit

| Pico | SN76489 |
|---|---|
| GP2–GP9 | D0–D7 |
| GP10 | /WE |
| GND | /CE and GND |
| 3V3 | VCC |

`psg:AUDIO` drives the buzzer; a probe on the same point makes the run report the mixed frequency, duty and pulse width. Each tone update is the chip's two writes: a latch byte carrying the low four bits, then a data byte carrying the high six.

## Rebuilding

`part/build.sh` needs the `wasm32-unknown-unknown` target; `firmware/build.sh` needs `arm-none-eabi-gcc` and the repository's `litz-fixture` for the 256-byte stage 2. Both binaries are committed, so running the example needs neither.
