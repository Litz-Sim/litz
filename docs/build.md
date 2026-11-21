# Build recipes — `litz.build.toml`

Litz does not compile firmware. Toolchains change faster than a simulator should, and embedding an Arduino core or an ESP-IDF release would tie every project to Litz's release schedule. A project brings its own recipe instead: `litz.build.toml`, an ordered list of shell steps shaped like a CI job.

`litz build` runs the recipe. `litz test --build` runs it first and then the tests. The recipe is optional: a project that already has a firmware image needs none.

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
| `workdir` | `[build]`, `[[step]]` | Directory for the step, relative to the project. A step's value wins. `..` is refused, and the resolved path must stay inside the project. |
| `env` | `[build]`, `[[step]]` | Environment variables. `[build.env]` is the base; a step's `env` overrides one key. |
| `firmware` | `[build]` | A file that must exist after the last step. Relative to the project. |
| `name` | `[[step]]` | Label in the transcript. Defaults to the command. |
| `run` | `[[step]]` | The command, run by `sh -c` on Unix and `cmd /C` on Windows. Required. |
| `timeout` | `[[step]]` | Wall-clock limit such as `90s` or `20m`. A step without one has no limit of its own. |

A recipe needs at least one `[[step]]`. Steps run in file order; the first failure stops the recipe and is reported with its exit code.

## Commands

```sh
litz build [directory]             # run the recipe
litz build [directory] --dry-run   # print the steps and exit
litz test  [directory] --build     # recipe, then tests
litz run   [directory] --build     # recipe, then the bounded run
```

`litz build` exits 0 when every step succeeded and the `firmware` file exists, 1 when a step failed or the output is missing, and 2 for an unknown option. A missing `litz.build.toml` is an error naming the file.

## What it is not

A step runs with the user's permissions. The recipe is code, like a `Makefile` or a GitHub Actions `run:` line, and Litz does not sandbox it. The path rule only keeps a mis-typed `workdir` from walking out of the project; it does not constrain what the command itself does.

Nothing is downloaded or cached by Litz. If a step needs `arduino-cli`, `idf.py`, `avr-gcc` or `cargo`, that tool must already be on the machine, and the error from the shell is passed through unchanged.

## CI

```yaml
- run: cargo install --path crates/litz-cli
- run: litz test --build --junit report.xml
```

For a project that keeps its own Litz binary, the same recipe runs unchanged on a developer machine, a container, or a GitHub runner, because it is only a list of shell commands.
