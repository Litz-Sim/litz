# {{id}}

A community MCU board scaffold. Its MCU is the WebAssembly component in
`simulator/`, built against `litz:mcu@0.2.0` (`abi = "litz-mcu-2"`).
`board.toml` is the catalog entry, `{{id}}.onboard.json` is the board's
internal schematic, and `wit/` is the contract copy the guest compiles
against.

`litz catalog new board` created this directory. Edit `board.toml` for the
header pins and `simulator/src/lib.rs` for the chip, then build:

```sh
./simulator/build.sh        # writes {{id}}.wasm next to board.toml
```

`build.sh` needs `litz-componentize` on PATH. From a Litz checkout:

```sh
cargo install --path crates/litz-mcu-wasm --features tooling
```

It already sits in this project's `boards/`, so `litz run` and `litz test`
find it. To install the built bundle into another project, copy this directory
there or run, from that project's root:

```sh
litz catalog add <path-to-this-directory> --dir boards
```

`catalog add` copies the manifest, the schematic and the built module; the
simulator source stays here. The board id to use in `diagram.json` is
`{{id}}`:

```json
{ "id": "mcu", "type": "{{id}}" }
```

The guest is a tiny register machine: 4 KiB of RAM, a 64-byte program, and
one byte-instruction per cycle. `simulator/src/lib.rs` documents the opcodes
and the shape of the `litz:mcu` exports; replace the machine, not the glue.

## What the guest can reach

The 0.2 host interface is the whole point of a custom chip. A program can:

| Opcode | Host call | What it does |
|---|---|---|
| `4 0` | `wifi-scan` | list the access points and nearby networks |
| `4 1 controller address register` | `i2c-transfer` | read one register from an I2C part |
| `4 2` | `ble-scan` | list BLE advertisers |
| `4 3 pin` | `analog-read` | print the solved voltage on a pad |
| `4 4 port length bytes…` | `uart-write` | send bytes on a UART port |
| `4 5 length ssid…` | `wifi-join` | join the circuit's access point |
| `4 6 port-hi port-lo length bytes…` | `tcp-connect` / `tcp-write` / `tcp-read` | talk to a simulated service |
| `4 7 length data… response-length response…` | `ble-advertise` | put an advertisement on the air |
| `4 8 length address…` | `ble-stop-advertising` | withdraw one advertisement |
| `4 9 services…` | `ble-set-gatt` | register the GATT table this station serves |
| `4 10 length address…` | `ble-connect` | connect to an advertiser (empty means the first one) |
| `4 11 handle` | `ble-discover` | list a connection's services and characteristics |
| `4 12 handle svc-hi svc-lo char-hi char-lo` | `ble-read` | print a characteristic's bytes |
| `4 13 handle svc-hi svc-lo char-hi char-lo length bytes…` | `ble-write` | write a characteristic |
| `4 14 handle svc-hi svc-lo char-hi char-lo enabled` | `ble-subscribe` | follow a characteristic's notifications |
| `4 15 handle` | `ble-notifications` | print notifications received |
| `4 16` | `ble-take-writes` | print writes centrals made to this station |
| `4 17 svc-hi svc-lo char-hi char-lo length bytes…` | `ble-notify` | notify every subscriber |
| `4 18 handle` | `ble-disconnect` | close a connection |
| `4 19 length network channel-hi channel-lo length address…` | `radio-configure` | join the shared packet medium |
| `4 20 length bytes… length address… airtime(4)` | `radio-send` | put a raw frame on the air |
| `4 21` | `radio-receive` | print frames that finished and reached this station |

`spi-transfer`, `wifi-resolve`, `udp-*` and `wifi-leave` have the same shape
in `wit/litz-mcu.wit`; call them from your own machine when a program needs
them. The same file's `debug` interface is what a debugger uses: `pc`,
`registers`, `read`, `disassemble`, `set-breakpoints` and `step`. Nothing reaches a real file, clock or socket: the router the circuit
places answers every network call, and the bus devices the diagram wires
answer every transaction. A BLE connection is plain simulated state: the
advertiser's table answers discovery, reads and writes, and a peripheral that
calls `ble-notify` reaches every subscribed central on the same circuit.
