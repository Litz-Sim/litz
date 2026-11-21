#!/usr/bin/env bash
# Builds the guest for wasm32-unknown-unknown and wraps the core module in the
# component named by `board.toml`. `litz-componentize` comes with Litz (it is
# the `litz-componentize` binary in the `litz-mcu-wasm` crate); no
# cargo-component install is needed.
set -euo pipefail
cd "$(dirname "$0")"

# A per-project target directory keeps the artifact path predictable even
# when the author has a global `build.target-dir` configured.
if ! command -v litz-componentize >/dev/null 2>&1; then
  echo "litz-componentize is not on PATH; from a Litz checkout run:" >&2
  echo "  cargo install --path crates/litz-mcu-wasm --features tooling" >&2
  exit 1
fi

cargo build --target wasm32-unknown-unknown --release --target-dir target

litz-componentize \
  target/wasm32-unknown-unknown/release/{{crate_snake}}.wasm \
  ../{{id}}.wasm
