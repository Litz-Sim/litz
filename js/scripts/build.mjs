// Build the WebAssembly module this package ships.
//
// The source of truth is `crates/litz-wasm`; this script compiles it for
// `wasm32-unknown-unknown`, copies the result next to `index.js` and checks the
// two properties the loader relies on: no imports, and every export present.

import { execFileSync } from "node:child_process";
import { copyFileSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageDir = path.resolve(here, "..");
const repoRoot = path.resolve(packageDir, "..");
const target = path.join(packageDir, "litz_wasm.wasm");

function run(command, args) {
  execFileSync(command, args, { cwd: repoRoot, stdio: "inherit" });
}

run("cargo", ["build", "--release", "-p", "litz-wasm", "--target", "wasm32-unknown-unknown"]);

const metadata = JSON.parse(
  execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
    cwd: repoRoot,
    encoding: "utf8",
  }),
);
const built = path.join(
  metadata.target_directory,
  "wasm32-unknown-unknown",
  "release",
  "litz_wasm.wasm",
);
copyFileSync(built, target);

const module = new WebAssembly.Module(readFileSync(target));
const imports = WebAssembly.Module.imports(module);
if (imports.length > 0) {
  throw new Error(
    `the module imports ${imports.map((entry) => entry.name).join(", ")}; ` +
      "the loader instantiates it with an empty import object",
  );
}
const names = new Set(WebAssembly.Module.exports(module).map((entry) => entry.name));
const required = readFileSync(path.join(packageDir, "index.js"), "utf8").match(
  /const REQUIRED_EXPORTS = \[([\s\S]*?)\];/,
);
for (const [, name] of required ? required[1].matchAll(/"([^"]+)"/g) : []) {
  if (!names.has(name)) throw new Error(`the module does not export ${name}`);
}

console.log(`built ${path.relative(repoRoot, target)} (${statSync(target).size} bytes)`);
