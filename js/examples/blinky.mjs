// Boot the repository's blinky example and print its console.
//
// Run from the package directory:
//
//   npm run example
//
// The blinky firmware toggles GPIO8 and writes a line per edge to UART0.

import { readFile } from "node:fs/promises";
import { loadLitz } from "@litz-sim/litz";

const project = new URL("../../examples/blinky/", import.meta.url);

const litz = await loadLitz();
const session = litz.createSession({
  diagram: await readFile(new URL("diagram.json", project), "utf8"),
  app: new Uint8Array(await readFile(new URL("firmware/blinky.bin", project))),
});

// A scheduling step is not a fixed span of time, so advance by simulated
// milliseconds instead and let the wrapper run as many steps as it takes.
session.advance(300);

process.stdout.write(session.serial.take());
console.log(`${session.chip().name} at ${session.ms} ms, stop=${session.stop}`);

const led = session.part("led1");
console.log(`led1: ${led?.state ?? "unknown"}`);

session.close();
litz.close();
