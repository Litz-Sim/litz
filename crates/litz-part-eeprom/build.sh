#!/usr/bin/env bash
# Rebuilds the 24C02 reference guest and the component the catalog ships.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build -p litz-part-eeprom --target wasm32-unknown-unknown --release

target_dir=$(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

cargo run -q -p litz-mcu-wasm --features tooling --bin litz-componentize -- \
  "$target_dir/wasm32-unknown-unknown/release/litz_part_eeprom.wasm" \
  crates/litz-catalog/builtin/parts/24c02.wasm
