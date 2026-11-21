#!/usr/bin/env bash
# Rebuilds the reference chip and the component the catalog ships.
#
# Two steps, both plain `cargo`: the guest is an ordinary cdylib for
# wasm32-unknown-unknown, and `litz-componentize` wraps that core module in the
# `litz:mcu` component type. No cargo-component install is needed.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build -p litz-chip-rv32 --target wasm32-unknown-unknown --release

target_dir=$(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

cargo run -q -p litz-mcu-wasm --features tooling --bin litz-componentize -- \
  "$target_dir/wasm32-unknown-unknown/release/litz_chip_rv32.wasm" \
  crates/litz-catalog/builtin/boards/litz-rv32.wasm
