#!/usr/bin/env bash
# Rebuilds the runaway test fixture used by litz-part-wasm's sandbox test.
# The artifact is not a catalog part; it lives beside the test that loads it.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build -p litz-part-runaway --target wasm32-unknown-unknown --release

target_dir=$(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

cargo run -q -p litz-mcu-wasm --features tooling --bin litz-componentize -- \
  "$target_dir/wasm32-unknown-unknown/release/litz_part_runaway.wasm" \
  crates/litz-part-wasm/tests/fixtures/runaway.wasm
