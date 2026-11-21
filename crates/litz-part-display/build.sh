#!/usr/bin/env bash
# Rebuilds the RGB panel reference guest and the component the catalog ships.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build -p litz-part-display --target wasm32-unknown-unknown --release

target_dir=$(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

cargo run -q -p litz-mcu-wasm --features tooling --bin litz-componentize -- \
  "$target_dir/wasm32-unknown-unknown/release/litz_part_display.wasm" \
  crates/litz-catalog/builtin/parts/display.wasm
