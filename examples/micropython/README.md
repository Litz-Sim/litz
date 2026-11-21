# MicroPython on an ESP32-C3

Stock MicroPython, exactly as micropython.org publishes it, booted through the
mask ROM. The runner waits for the REPL prompt and pastes `main.py` with
MicroPython's raw-paste protocol, so the Python source needs no filesystem
image — the project is a diagram, a `litz.toml` and a `.py` file.

`diagram.json` is the ESP32-C3-DevKitM-1 with an LED on GPIO8. `main.py` prints
`litz: micropython`, blinks GPIO8 three times through `machine.Pin`, and prints
`+`/`-` for each edge.

## Run it

From the repository root:

```sh
tools/fetch-micropython.sh
LITZ_ROM_DIR=build/micropython cargo run -p litz-cli -- run examples/micropython
LITZ_ROM_DIR=build/micropython cargo run -p litz-cli -- test examples/micropython
```

The first command fetches the official MicroPython v1.29.0 image for the
ESP32-C3 and the C3 mask ROM into `build/micropython/` (git-ignored), checking
each file against a SHA-256 pinned in the script. The same script also fetches
the classic ESP32, ESP32-S3 and ESP32-C6 images. `litz` itself never
downloads anything.

The parenthesised note in the two `litz` commands is important: the C3 image is
a merged bootloader/partition-table/application file, so the mask ROM has to
run it. `LITZ_ROM_DIR` points at the directory holding
`esp32c3_rev3_rom.elf`; `[sim] rom` in `litz.toml` is the per-project
alternative.

`test` exits 0 when `litz: micropython` appears and `led1` has been lit. The
`+`/`-` lines in the transcript mark each blink.

## Licence

The image is upstream MicroPython, MIT-licensed; it also links ESP-IDF and
Espressif's Wi-Fi/PHY libraries (see the notices published with the release).
The mask ROM ELF is Espressif's, Apache-2.0, from
[`espressif/esp-rom-elfs`](https://github.com/espressif/esp-rom-elfs). Neither
is vendored in this repository; `tools/fetch-micropython.sh` fetches them by
pinned URL and hash.