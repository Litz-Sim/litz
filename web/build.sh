#!/usr/bin/env bash
# Builds everything the page loads: the wasm module and the demo firmware.
# The firmware is generated, never committed, so it cannot drift from the
# source in crates/litz-fixture.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p litz-wasm --target wasm32-unknown-unknown
cargo run -q --release -p litz-fixture -- blinky web/demo/blinky.bin --period-ms 120
cargo run -q --release -p litz-fixture -- button web/demo/button/button.bin
cargo run -q --release -p litz-fixture -- hello web/demo/switch/hello.bin
cargo run -q --release -p litz-fixture -- pot web/demo/pot/pot.bin

target_dir=$(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
cp "$target_dir/wasm32-unknown-unknown/release/litz_wasm.wasm" web/litz_wasm.wasm
# `litz serve web/workbench` makes that directory the server root, and the
# server refuses paths that climb out of it. The workbench page asks for
# ../litz_wasm.wasm, which a root-level page resolves to /litz_wasm.wasm, so
# keep a copy beside it as well.
cp "$target_dir/wasm32-unknown-unknown/release/litz_wasm.wasm" web/workbench/litz_wasm.wasm

echo "built web/litz_wasm.wasm ($(wc -c < web/litz_wasm.wasm) bytes) and the demo firmware"
echo "copied it to web/workbench/litz_wasm.wasm for the standalone workbench server"
