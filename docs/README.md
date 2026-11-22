# Litz documentation

Litz is a deterministic electronics and firmware simulator. A `diagram.json` describes the parts and wiring, a project file names the firmware image, and Litz boots that image on an emulated microcontroller while resolving pins, buses and analog nets against the circuit. Time is simulated nanoseconds, so the same project produces the same console, pin levels and timing on every machine.

```sh
cargo install litz-cli
litz test examples/blinky
```

## Where to go

| Page | What it covers |
|---|---|
| [Getting started](getting-started.md) | A first project, from a diagram to a passing test |
| [CLI reference](cli.md) | Every subcommand and flag of the `litz` binary |
| [Rust reference](rust.md) | The crates, `Part` and `Chip` traits, and the runner |
| [JavaScript reference](javascript.md) | `@litz-sim/litz` in the browser and under Node |
| [Building firmware](build.md) | Toolchains, recipes and staging MicroPython or CircuitPython |
| [Parts and boards](parts.md) | The catalog, part manifests, buses and analog parts |
| [Writing a chip](chips.md) | A wasm MCU: the board bundle, both ABIs and the host calls |
| [MCP server](mcp.md) | Driving a simulation from an editor over the Model Context Protocol |
| [Architecture](architecture.md) | How the crates fit together |
| [File formats](formats.md) | `diagram.json`, `litz.toml`, `litz.lock`, probes, decoders and outputs |

## Source and releases

- [GitHub](https://github.com/Litz-Sim/litz)
- [crates.io](https://crates.io/crates/litz)
- [npm](https://www.npmjs.com/package/@litz-sim/litz)