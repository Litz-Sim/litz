#!/usr/bin/env bash
# Rebuilds the template's wasm part and wraps it in the component part.toml
# names.
set -euo pipefail
cd "$(dirname "$0")"

# The manifest is stored with an .in suffix so the CLI tarball includes it;
# Cargo treats any subdirectory holding a Cargo.toml as a nested package.
cp Cargo.toml.in Cargo.toml

cargo build --manifest-path Cargo.toml --target wasm32-unknown-unknown --release

target_dir=$(cargo metadata --manifest-path Cargo.toml --format-version 1 --no-deps |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

cargo run -q --manifest-path ../../../../Cargo.toml -p litz-mcu-wasm --features tooling --bin litz-componentize -- \
  "$target_dir/wasm32-unknown-unknown/release/litz_part_example.wasm" \
  part.wasm
