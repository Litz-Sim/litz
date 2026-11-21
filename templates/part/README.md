# Example Litz part

A minimal wasm-backed part with two pins: `OUT` follows the level on `IN`.
The guest is `src/lib.rs`; the catalog entry is `part.toml`.

From the repository root, run:

```sh
templates/part/build.sh
cargo run -p litz-cli --bin litz -- catalog add templates/part
cargo run -p litz-cli --bin litz -- catalog list .
```

The first command writes `part.wasm` next to `part.toml`, the second copies
both into `parts/`, and the third confirms the catalog entry with source
`parts/`. To install into another project, run the same commands from that
project and give `catalog add` the template's path.
