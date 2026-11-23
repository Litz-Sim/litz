# YM2610

A custom part defined in Verilog: the Yamaha YM2610 (OPNB) from the Neo Geo, compiled to WebAssembly with Verilator and wrapped in the `litz:part` contract.

The RTL is [jotego/jt12](https://github.com/jotego/jt12)'s `jt10.v`, which is GPL-3.0. **This example is therefore GPL-3.0-or-later** (`third_party/jt12/COPYING`); the rest of the repository stays Apache-2.0. It is not part of any published crate or the npm package.

```sh
cargo run -p litz-cli -- test examples/ym2610
```

The firmware is bare-metal RP2040 C. It writes the shared bus (`D0`–`D7`, `A0`, `A1`, `/CS`, `/WR`): two FM voices (A4 and E5) and SSG channel A at about 292 Hz, waits 200 ms, releases them, and prints what it did. The test waits for `done` and for the buzzer on `AUDIO_L` to sound.

## The part

| File | Holds |
|---|---|
| `part/files.f` | The RTL file list from `jt10.qip` |
| `part/model.cpp` | A C ABI over the Verilator model: reset, register writes, a cycle target and the two audio samples |
| `part/glue.c` | The `litz:part@0.2.0` glue: pins to bus writes, simulated time to model cycles, analog volts from the samples |
| `part/runtime_stubs.cpp` | The parts of Verilator's hosted runtime a wasm component cannot have: no thread pool, no stdio, no environment |
| `part/build.sh` | wit-bindgen C bindings, Verilator, wasi-sdk, then `litz-componentize` |
| `parts/ym2610.toml` + `parts/ym2610.wasm` | The committed component and its pin table |

The model runs at 8 MHz and produces an audio frame every 144 clocks. The part publishes the level of each channel every 8 frames (144 µs), often enough for a probe or a passive buzzer and well below the 55.5 kHz sample rate; a whole second of simulated audio would be 8 million clocks, which is more work than the host's per-call fuel budget allows, so the cadence is also what keeps one `advance` bounded. `analog()` reports the sample as a voltage around 1.65 V.

The mix has a large DC component because the SSG is summed unsigned. A real YM2610 drives a coupling capacitor, so the part tracks a slow average of the two channels and publishes whether each sample is above it; the result follows the waveform's zero crossings rather than a fixed offset.

ADPCM-A and ADPCM-B are present in the RTL but the part leaves their ROM data pins unconnected, so only FM and SSG make sound in this example.

## Rebuilding

`part/build.sh` needs `verilator` and an unpacked wasi-sdk (`WASI_SDK=/path/to/wasi-sdk-34.0`); `firmware/build.sh` needs `arm-none-eabi-gcc`. Both the component and the firmware image are committed, so running the example needs neither.

The component has no WASI imports: the Verilator runtime's file, clock and thread paths are compiled but never taken, and the stub definitions in `runtime_stubs.cpp` keep the core module's import list empty so it can be wrapped as a component at all.
