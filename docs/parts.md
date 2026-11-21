# Parts

A part bundle is a directory that a project installs into its own `parts/`.
Nothing is fetched and nothing is recompiled: the manifest is data, and a wasm
backend is a file next to it.

## Bundle layout

A bundle holds one manifest and every file the manifest names:

```
my-part/
  part.toml
  mirror.wasm     # only when [simulation].backend = "wasm"
```

`part.toml` is the conventional name. If it is absent, `litz catalog add` takes
the only `<name>.toml` in the directory. A builtin-backed part needs no other
file.

The installed layout is the same, with the manifest and its files copied into
the project:

```
project/
  parts/
    part.toml
    mirror.wasm
  boards/         # board manifests, optional
```

## Discovery

`litz run`, `litz test` and `litz check` load the builtin catalog, then every
`*.toml` in `parts/`, then every `*.toml` in `boards/`, all beside the project
directory they are given. A project manifest whose id matches a builtin entry
replaces that entry for the run. When two project directories define the same
id, the later directory (`boards/`) wins.

The loader reads manifests only. Parsing never executes code.

## Manifest fields

A part manifest is a `[part]` table, pins, optional attributes and buses, and a
`[simulation]` table.

```toml
[part]
id = "vendor-mirror"
name = "Mirror"
version = "0.1.0"
description = "Output follows input"
license = "MIT OR Apache-2.0"

[pins.IN]
kind = "digital-in"
number = 1

[pins.OUT]
kind = "digital-out"
number = 2

[attrs.color]
type = "enum"
values = ["red", "green", "blue"]
default = "red"

[simulation]
backend = "builtin"
engine = "led"
```

`[part]` fields:

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | the catalog key a `diagram.json` names; immutable |
| `name` | yes | the label a picker shows |
| `version` | yes | semver text; informational |
| `description` | no | one line for a picker |
| `license` | no | SPDX expression for the bundle |

Pin fields:

| Field | Required | Meaning |
|---|---|---|
| `kind` | yes | the electrical role (see below) |
| `number` | no | package pin number for display |
| `repeat` | no | how many contacts share the name, default 1 |
| `label` | no | text shown next to the pin |

Pin kinds are `digital-in`, `digital-out`, `digital-io`, `analog-in`,
`analog-out`, `analog-io`, `passive`, `power`, `ground`, `nc`, and the bus
kinds `i2c-sda`, `i2c-scl`, `spi-mosi`, `spi-miso`, `spi-sck`, `spi-cs`,
`uart-tx`, `uart-rx`.

Attribute fields: `type` is `bool`, `int`, `float`, `string` or `enum`;
`values` is required and non-empty for an enum; `default` must match the type
and, for an enum, be one of `values`.

Buses. A part that answers bus transactions declares at most one bus section:

```toml
[buses.i2c0]
address = 0x76        # 7-bit address, 0x08..=0x7f

[buses.spi0]
cs = "CS"             # the spi-cs pin that selects the part

[buses.uart0]
port = 0              # must agree with the name's index
```

Each kind takes only its own field. The bus pins themselves (`i2c-sda`,
`spi-sck`, and so on) are ordinary pins that the diagram wires to the board.

## Backends

| Backend | Required fields | What runs |
|---|---|---|
| `builtin` | `engine` | a model compiled into the Litz runner, selected by name |
| `wasm` | `module`; `abi` optional | a WebAssembly component implementing the part contract |

A builtin part uses an engine name the runner already has; choosing an unknown
name parses but will not simulate. `docs/formats.md` lists the engines.

A wasm part names a module relative to the manifest. The path may not be
absolute and may not contain `..`. `abi` defaults to `litz-part-2`
(`litz-part-1` is still accepted). The module runs sandboxed: no filesystem,
network, clock or randomness, with memory and fuel limits, so a bundle from
someone else cannot reach the host.

## Versioning

The id is the promise. A diagram names an id and expects the same device with
the same pins and behaviour forever. `version` is informational: nothing
resolves or pins a version, and a run does not compare it.

Use semver for the reader's benefit: patch for fixes, minor for additive
changes such as a new optional attribute, major for behaviour changes. Never
reuse an id for a different device and never change the meaning of a pin.

## Licensing

A bundle is distributed by whoever wrote it, not by Litz. Put an SPDX
expression in `[part].license`, keep the bundle's own license file, and check
the license before installing a bundle from a third party. `litz catalog add`
copies files; it does not inspect or enforce licenses.

## Commands

All commands except `list` and `check` act on the project in the current
directory. `--dir` chooses the destination directory, `parts` by default.

### Print the builtin catalog

```sh
litz catalog
```

```
id                                    kind   name                             pins
board-arduino-uno                     board  Arduino Uno                        32
...
litz-led                              part   LED                                  2
```

`litz catalog --json` prints the same entries as a JSON array.

### List a project

```sh
litz catalog list .
```

```
id                    kind   name              pins  source
litz-led              part   LED                  2  builtin
vendor-mirror         part   Mirror               2  parts/
```

The `source` column is `builtin`, `parts/` or `boards/`; rows are sorted by id.

### Check manifests

```sh
litz catalog check .
```

```
2 manifests ok
```

A manifest that does not parse prints one line naming the file and exits 1:

```
parts/bad.toml: missing field `version`
```

### Install a bundle

```sh
litz catalog add ../vendor-mirror
```

```
installed vendor-mirror at /project/parts/part.toml
```

`add` validates the whole bundle before writing anything: the manifest parses,
the id is neither builtin nor already installed, every referenced module is a
relative path inside the bundle, and every referenced file exists. A bad bundle
or an id collision leaves the destination untouched.

### Scaffold a manifest

```sh
litz catalog new vendor-switch --name "Vendor Switch"
```

```
created /project/parts/vendor-switch.toml
```

The file declares two passive pins and leaves the backend commented out.
Uncomment one `[simulation]` block, choose the engine or module, then run
`litz catalog check .`. `new` refuses to overwrite an existing file.

### Remove a part

```sh
litz catalog remove vendor-switch
```

```
removed vendor-switch (/project/parts/vendor-switch.toml)
```

`remove` deletes the manifest and any wasm module it names, unless another
manifest in the same directory names that module too. Builtin ids are refused:
no project file owns them.

## Worked example: a two-pin builtin part

A green LED on the `led` engine. Create the directory and manifest:

```sh
mkdir -p vendor-led
```

```toml
# vendor-led/part.toml
[part]
id = "vendor-led"
name = "Vendor LED"
version = "1.0.0"
description = "5 mm green LED"
license = "MIT OR Apache-2.0"

[pins.A]
kind = "digital-in"
number = 1
label = "anode"

[pins.C]
kind = "ground"
number = 2

[attrs.color]
type = "enum"
values = ["green"]
default = "green"

[simulation]
backend = "builtin"
engine = "led"
```

Install and check:

```sh
litz catalog add vendor-led
litz catalog check .
litz catalog list .
```

```
installed vendor-led at /project/parts/part.toml
1 manifest ok
id          kind  name        pins  source
vendor-led  part  Vendor LED     2  parts/
```

The diagram may now use `"type": "vendor-led"` wherever `litz-led` is allowed.

## Worked example: a wasm part

Start from `templates/part/`, a buildable part whose `OUT` pin mirrors its
`IN` pin. Change the id and name in `part.toml` and in `src/lib.rs`, then
build:

```sh
templates/part/build.sh
```

The script compiles the crate for `wasm32-unknown-unknown` and writes the
wrapped component next to the manifest:

```
templates/part/part.toml
templates/part/part.wasm
```

The manifest it ships is:

```toml
[part]
id = "litz-part-example"
name = "Example mirror"
version = "0.1.0"
license = "MIT OR Apache-2.0"

[pins.IN]
kind = "digital-in"
number = 1

[pins.OUT]
kind = "digital-out"
number = 2

[simulation]
backend = "wasm"
module = "part.wasm"
abi = "litz-part-2"
```

From the project that should receive it:

```sh
litz catalog add path/to/templates/part
litz catalog check .
```

```
installed litz-part-example at /project/parts/part.toml
2 manifests ok
```

`add` copies `part.toml` and `part.wasm` together, so the manifest keeps
resolving its module. The guest itself is ordinary Rust against
`litz-part-sdk`; adjust `src/lib.rs` for real behaviour and rerun `build.sh`
before reinstalling.
