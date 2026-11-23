#!/usr/bin/env bash
# Rebuilds the SN76489 guest and wraps it in the component part.toml names.
#
# Two steps, both plain `cargo`: the guest is a cdylib for
# wasm32-unknown-unknown, and `litz-componentize` wraps that core module in the
# `litz:part` component type. Run it from anywhere.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --manifest-path Cargo.toml --target wasm32-unknown-unknown --release

target_dir=$(cargo metadata --manifest-path Cargo.toml --format-version 1 --no-deps |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

cargo run -q --manifest-path ../../../Cargo.toml -p litz-mcu-wasm --features tooling --bin litz-componentize -- \
  "$target_dir/wasm32-unknown-unknown/release/sn76489_part.wasm" \
  ../parts/sn76489.wasm
