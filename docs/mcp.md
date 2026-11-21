# Litz over MCP

`litz mcp` is a Model Context Protocol server for Litz. It speaks JSON-RPC 2.0 over stdin/stdout, one message per line, and is written in Rust: no Node, no Python and no cloud compile. The tools call the same crates as the `litz` command, and test and run execution goes through the same runner, so a circuit the server checks is the circuit the CLI runs.

```sh
cargo build --release -p litz-cli
./target/release/litz mcp
```

The process reads requests until EOF and exits 0 on a clean shutdown. It writes nothing to stdout except protocol messages; diagnostics go to stderr. `litz mcp --help` prints the existing usage text, and any other argument exits 2.

## Protocol

The server implements `initialize`, `notifications/initialized`, `notifications/cancelled`, `initialized`, `ping`, `tools/list` and `tools/call`. The current revision is `2025-06-18`; `initialize` answers with the revision the client requests, and uses `2025-06-18` when the request names none. It advertises the `tools` capability with `listChanged: false` and identifies itself as `litz-mcp` with the crate version. Notifications produce no response.

The server exposes tools only. There are no resources and no prompts; `resources/list` and `prompts/list` are answered as unknown methods.

A successful `tools/call` carries its result twice: as a text content block and as `structuredContent`. A tool that fails while doing its work returns `isError: true` with the message as the text content and no structured result. An unknown tool, a missing tool name, or an `arguments` value that is not an object is a JSON-RPC invalid-params error.

## Paths

Every tool takes `root`, the project directory. The root must exist and is canonicalised. Every other path is a string relative to that root; absolute paths, drive prefixes and `..` are refused before the filesystem is touched, and an existing path is canonicalised so a symlink cannot widen the sandbox. A path to write must have its parent inside the root, and an existing target is canonicalised for the same reason.

Before `litz_test` or `litz_run` starts the runner, the server also checks the paths `litz.toml` itself names for `[project] diagram`, `[sim] firmware`, `bootloader`, `partition_table`, `rom` and `script`. A manifest cannot reach outside the project through one of those keys.

## Tools

| Tool | Arguments | Returns |
|---|---|---|
| `litz_check` | `root`, `diagram?`, `strict?` | `passed`, `diagram`, `diagnostics[]` (`severity`, `code`, `message`) |
| `litz_test` | `root`, `scenario?`, `strict?` | `status`, `exitCode`, `simTimeMs`, `tests[]`, `output`; each test has `name`, `status`, `exitCode`, `simTimeMs`, `output` |
| `litz_run` | `root`, `timeout?`, `strict?` | `status`, `exitCode`, `simTimeMs`, `serial`, `partStates`, `faults[]` |
| `litz_circuit_create` | `root`, `parts`, `connections?`, `path?`, `force?` | `written`, `replaced`, `partCount`, `connectionCount`, `diagnostics[]` |
| `litz_circuit_update` | `root`, `parts?`, `connections?`, `path?` | `written`, `partCount`, `connectionCount`, `diagnostics[]` |
| `litz_code_generate` | `root`, `language` (`c` or `python`), `firmware?`, `force?` | `language`, `written[]`, `skipped[]` |
| `litz_import_diagram` | `root`, `source`, `output?` | `source`, `output`, `partCount`, `connectionCount`, `unknownParts[]`, `unknownAttributes[]`, `diagnostics[]` |

A diagram path that is omitted means the diagram named by `[project] diagram` in `litz.toml`, or `diagram.json` when the manifest does not name one. `litz_check`, `litz_circuit_create` and `litz_circuit_update` all resolve the default this way.

`litz_check` validates the diagram against the builtin catalog plus the project's `parts/` and `boards/`, and returns each diagnostic. `passed` is false when a diagnostic is an error, or when `strict` is true and any warning is present. It loads no firmware.

`litz_test` runs the `[[test]]` entries from `litz.toml`, or the YAML file named by `scenario`, through the existing runner. `status` is `passed` or `failed`, and the per-test entries carry the same simulator time and transcript the CLI reports.

`litz_run` boots the same project for a bounded amount of simulated time (default `10s`). `status` is `ok` when every board halted, `timeout` when the limit was reached without a fault, and `failed` otherwise; `exitCode` is the runner's own code (0, 42 or 1). The serial transcript is returned in `serial` instead of streamed. `partStates` is keyed by part id, and each value carries a `kind` plus the fields for that state, such as `{"kind": "led", "lit": true}` or `{"kind": "segments", "mask": 127}`. `faults` lists electrical conflicts.

`litz_circuit_create` writes a diagram from catalog ids. It refuses a duplicate part id, a `$`-prefixed id, an unknown catalog `type`, a malformed connection, or a result with validation errors. It refuses to replace an existing file unless `force` is true. The written document has `version: 1`.

`litz_circuit_update` merges into the existing JSON value, so unknown top-level and per-part fields survive. A matching part id updates that part and its `attrs` merge by key. A connection already present between the same two pins, in either direction, is not added twice. A merged document with structural errors is refused and the file is left unchanged.

`litz_code_generate` writes a starter `main.c` or `main.py` and a matching `litz.toml`. `language` defaults to `c`; the handler also accepts `micropython` as a synonym for `python`. When `firmware` is omitted, the manifest names `firmware/main.bin` for C and `firmware/micropython.bin` for Python. An existing source or manifest file is skipped unless `force` is true. Nothing is compiled and nothing is sent to a network service.

`litz_import_diagram` reads a diagram and reports every part whose `type` the catalog does not know under `unknownParts`, as `{id, type}`, and every part attribute the catalog does not define under `unknownAttributes`, as `{part, attribute}`. It writes a pretty-printed copy only when `output` is given.

A part in `litz_circuit_create` is an object with `id` and a catalog `type`:

```json
{
  "id": "led1",
  "type": "litz-led",
  "attrs": {"color": "green"}
}
```

A connection is either an object or the four-element array the file format uses:

```json
{"from": "esp:GPIO8", "to": "r1:1", "color": "green"}
```

```json
["r1:2", "led1:A", "green", []]
```

## Client configuration

An MCP client that launches servers from a JSON block can use this shape; the surrounding file name and key depend on the client.

```json
{
  "mcpServers": {
    "litz": {
      "command": "/absolute/path/to/litz_core/target/release/litz",
      "args": ["mcp"]
    }
  }
}
```

Build the release binary first. The server has no network dependency and starts no child process; it is the same binary as the CLI.

## Examples

Check the bundled Blinky example:

```json
{
  "name": "litz_check",
  "arguments": {
    "root": "/absolute/path/to/litz_core/examples/blinky"
  }
}
```

Run its tests and read the merged run report:

```json
{
  "name": "litz_test",
  "arguments": {
    "root": "/absolute/path/to/litz_core/examples/blinky"
  }
}
```

Create a two-part circuit:

```json
{
  "name": "litz_circuit_create",
  "arguments": {
    "root": "/absolute/path/to/my-project",
    "parts": [
      {"id": "esp", "type": "board-esp32-c3-devkitm-1"},
      {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
    ],
    "connections": [
      {"from": "esp:GPIO8", "to": "led1:A", "color": "green"},
      {"from": "led1:C", "to": "esp:GND.1", "color": "black"}
    ]
  }
}
```

The server has no compile step. `litz_code_generate` writes starter sources and a manifest; producing the firmware image is still the project's own build.
