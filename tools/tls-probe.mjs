#!/usr/bin/env node
// Runs the browser module's TLS handshake on a host with no imports.
//
// The page calls `WebAssembly.instantiate(bytes, {})`, so the module may not
// import anything; that is also why the TLS cryptography has to be pure Rust.
// Build the probe with:
//
//   cargo build -p litz-wasm --target wasm32-unknown-unknown --release \
//     --features tls-probe --target-dir build/tls-probe
//   node tools/tls-probe.mjs \
//     build/tls-probe/wasm32-unknown-unknown/release/litz_wasm.wasm
//
// A non-zero exit means either an import appeared or the handshake failed; on
// failure the module's own message is printed.

import { readFileSync } from "node:fs";

const path = process.argv[2];
if (!path) {
  console.error("usage: tools/tls-probe.mjs PATH_TO_litz_wasm.wasm");
  process.exit(2);
}

const module = new WebAssembly.Module(readFileSync(path));
const imports = WebAssembly.Module.imports(module);
if (imports.length > 0) {
  console.error("the module must import nothing, found:", imports);
  process.exit(1);
}

const instance = new WebAssembly.Instance(module, {});
const bytes = instance.exports.litz_tls_probe();
if (bytes === 0) {
  const ptr = instance.exports.litz_alloc(512);
  const len = instance.exports.litz_last_error(ptr, 512);
  const text = new TextDecoder().decode(
    new Uint8Array(instance.exports.memory.buffer, ptr, len),
  );
  console.error(`TLS probe failed: ${text || "no message"}`);
  process.exit(1);
}

console.log(`TLS probe: ${bytes} bytes crossed the wire with no imports`);
