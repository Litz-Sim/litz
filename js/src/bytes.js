// Buffers and the byte boundary: host values in, typed arrays out.

import { DECODER } from "./constants.js";
import { LitzError } from "./errors.js";

export function hold(io, bytes) {
  if (bytes.length === 0) return { ptr: 0, len: 0 };
  const ptr = io.exports.litz_alloc(bytes.length);
  if (ptr === 0) throw new LitzError(`could not allocate ${bytes.length} bytes in the module`);
  io.view(ptr, bytes.length).set(bytes);
  return { ptr, len: bytes.length };
}

export function bytesOf(value, label) {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  throw new LitzError(`\`${label}\` must be a Uint8Array`);
}

export function optionalBytes(value, label) {
  if (value === null || value === undefined) return new Uint8Array(0);
  return bytesOf(value, label);
}

export function decodeChunks(chunks) {
  if (chunks.length === 0) return "";
  if (chunks.length === 1) return DECODER.decode(chunks[0]);
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const joined = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) {
    joined.set(chunk, at);
    at += chunk.length;
  }
  return DECODER.decode(joined);
}

export async function toBytes(source) {
  if (source instanceof ArrayBuffer) return new Uint8Array(source.slice(0));
  if (ArrayBuffer.isView(source)) {
    return new Uint8Array(
      source.buffer.slice(source.byteOffset, source.byteOffset + source.byteLength),
    );
  }
  if (typeof Response !== "undefined" && source instanceof Response) {
    if (!source.ok) throw new LitzError(`fetching the wasm module failed: ${source.status}`);
    return new Uint8Array(await source.arrayBuffer());
  }
  const url = source instanceof URL ? source : new URL(String(source), import.meta.url);
  if (url.protocol === "file:") {
    const { readFile } = await import("node:fs/promises");
    try {
      return new Uint8Array(await readFile(url));
    } catch (error) {
      throw new LitzError(
        `could not read the wasm module at ${url.href}; run \`npm run build\` in the package, or pass \`wasm\` explicitly`,
        { cause: error },
      );
    }
  }
  if (typeof fetch !== "function") {
    throw new LitzError("this host has no fetch; pass `wasm` as bytes or a URL");
  }
  const response = await fetch(url);
  if (!response.ok) {
    throw new LitzError(`fetching ${url.href} failed: ${response.status}`);
  }
  return new Uint8Array(await response.arrayBuffer());
}

