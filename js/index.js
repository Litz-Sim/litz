// Litz as a headless WebAssembly library.
//
// The module next to this file is built from `crates/litz-wasm`, which exposes
// a C ABI over linear memory: pointers are byte offsets, strings are UTF-8 and
// every call is synchronous. The wrapper turns that into objects — a session,
// its serial line, its parts, its display — and lives under `src/`; this file
// is the public entry point.
//
// A session owns one board, its flash and its peripherals. `session.run(steps)`
// advances it and returns the simulated clock; the host decides how much time
// to simulate per call.

export { DEFAULT_STEPS } from "./src/constants.js";
export { LitzError } from "./src/errors.js";
export { Litz, loadLitz, Session, wasmUrl } from "./src/litz.js";
export { chipFromJco, chipImports, partFromJco, partImports } from "./src/jco.js";
