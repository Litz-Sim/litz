# Litz over MCP

`litz mcp` is a Model Context Protocol server for Litz. It speaks JSON-RPC 2.0
over stdin/stdout, one message per line, and is written in Rust: no Node, no
Python and no cloud compile. The tools call the same crates as the `litz`
command, so a circuit the server checks is the circuit the CLI runs.

```sh
cargo build --release -p litz-cli
./target/release/litz mcp
```

The process reads requests until EOF and exits 0 on a clean shutdown. It
writes nothing to stdout except protocol messages; diagnostics go to stderr.
`litz mcp --help` prints the existing usage text, and any other argument exits
2.

## Tools

Every tool takes `root`, the project directory. Every other path is a string
relative to that root; absolute paths, `..`, and symlinks that point outside
the root are refused.

| Tool | Arguments | Returns |
|---|---|---|
| `litz_check` | `root`, `diagram?`, `strict?` | `passed`, `diagram`, `diagnostics[]` (`severity`, `code`, `message`) |
| `litz_test` | `root`, `scenario?`, `strict?` | `status`, `exitCode`, `simTimeMs`, `tests[]` with serial `output`, `output` |
| `litz_run` | `root`, `timeout?`, `strict?` | `status`, `exitCode`, `simTimeMs`, `serial`, `partStates`, `faults[]` |
| `litz_circuit_create` | `root`, `parts`, `connections?`, `path?`, `force?` | `written`, `replaced`, `partCount`, `connectionCount`, `diagnostics[]` |
| `litz_circuit_update` | `root`, `parts?`, `connections?`, `path?` | `written`, `partCount`, `connectionCount`, `diagnostics[]` |
| `litz_code_generate` | `root`, `language` (`c` or `python`), `firmware?`, `force?` | `language`, `written[]`, `skipped[]` |
| `litz_import_diagram` | `root`, `source`, `output?` | `unknownParts[]`, `unknownAttributes[]`, `diagnostics[]`, `output` |

`litz_test` runs the `[[test]]` entries from `litz.toml`, or the YAML file named
by `scenario`, through the existing runner. `litz_run` boots the same project
for a bounded amount of simulated time: exit 0 means every board halted, exit 42 means the time limit was
reached without a fault, and the serial transcript is returned instead of
streamed. `litz_circuit_create` refuses a diagram that would not validate;
`litz_circuit_update` merges into the existing JSON value, so unknown top-level
and per-part fields survive. `litz_import_diagram` keeps every part it does not
recognise and lists it under `unknownParts` rather than dropping it.

A part in `litz_circuit_create` is an object with `id` and a catalog `type`:

```json
{
  "id": "led1",
  "type": "litz-led",
  "attrs": {"color": "green"}
}
```

A connection is either an object or the four-element array the file format
uses:

```json
{"from": "esp:GPIO8", "to": "r1:1", "color": "green"}
```

```json
["r1:2", "led1:A", "green", []]
```

## Editor configuration

Build the release binary first, then point the editor at it. The same shape
works for Claude Code (`.mcp.json` in a project, or `claude mcp add`), Claude
Desktop (`claude_desktop_config.json`) and Cursor (`.cursor/mcp.json`):

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

For Claude Code, the command-line equivalent is:

```sh
claude mcp add litz -- /absolute/path/to/litz_core/target/release/litz mcp
```

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

Run its tests and read the serial transcript:

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

The server has no compile step. `litz_code_generate` writes a starter
`main.c` or `main.py` and a matching `litz.toml`; producing the firmware image
is still the project's own build.