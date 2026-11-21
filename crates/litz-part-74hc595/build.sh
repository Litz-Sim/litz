#!/usr/bin/env bash
# Rebuilds the 74HC595 reference part and the component the catalog ships.
#
# Two steps, both plain `cargo`: the guest is an ordinary cdylib for
# wasm32-unknown-unknown, and `litz-componentize` wraps that core module in the
# `litz:part` component type. No cargo-component install is needed.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build -p litz-part-74hc595 --target wasm32-unknown-unknown --release

target_dir=$(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

cargo run -q -p litz-mcu-wasm --features tooling --bin litz-componentize -- \
  "$target_dir/wasm32-unknown-unknown/release/litz_part_74hc595.wasm" \
  crates/litz-catalog/builtin/parts/74hc595.wasm