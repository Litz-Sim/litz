// Firmware images for the workbench: raw `.bin` bytes and Intel HEX text.
// Pure and DOM-free so `node --test` can exercise it. The parser is small on
// purpose: it validates every record and assembles one contiguous image from
// the lowest to the highest address it saw, filling gaps with 0xFF.

const MAX_IMAGE_BYTES = 16 * 1024 * 1024;
const HEX_DIGITS = /^[0-9A-Fa-f]+$/;

function asText(source) {
  if (typeof source === "string") return source;
  if (source instanceof ArrayBuffer) source = new Uint8Array(source);
  if (ArrayBuffer.isView(source)) {
    let text = "";
    const bytes = new Uint8Array(source.buffer, source.byteOffset, source.byteLength);
    for (const byte of bytes) text += String.fromCharCode(byte);
    return text;
  }
  throw new TypeError("firmware source must be a string, ArrayBuffer or typed array");
}

function asBytes(source) {
  if (source instanceof Uint8Array) return source;
  if (source instanceof ArrayBuffer) return new Uint8Array(source);
  if (ArrayBuffer.isView(source)) {
    return new Uint8Array(source.buffer, source.byteOffset, source.byteLength);
  }
  if (Array.isArray(source)) return Uint8Array.from(source);
  throw new TypeError("firmware source must be an ArrayBuffer, typed array or byte array");
}

/** True when the first non-blank line looks like an Intel HEX record. */
export function looksLikeIntelHex(source) {
  let text;
  try {
    text = asText(source);
  } catch {
    return false;
  }
  for (const raw of text.replace(/^﻿/, "").split(/\r?\n/)) {
    const line = raw.trim();
    if (line === "") continue;
    return line.startsWith(":");
  }
  return false;
}

function parseLine(text, number) {
  if (!text.startsWith(":")) {
    throw new Error(`line ${number}: an Intel HEX record starts with ":"`);
  }
  const digits = text.slice(1);
  if (digits.length < 10) {
    throw new Error(`line ${number}: record is too short`);
  }
  if (digits.length % 2 !== 0) {
    throw new Error(`line ${number}: record has an odd number of hex digits`);
  }
  if (!HEX_DIGITS.test(digits)) {
    throw new Error(`line ${number}: record contains a non-hex character`);
  }
  const bytes = new Uint8Array(digits.length / 2);
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = Number.parseInt(digits.slice(i * 2, i * 2 + 2), 16);
  }
  const count = bytes[0];
  if (bytes.length !== count + 5) {
    throw new Error(
      `line ${number}: byte count ${count} does not match ${bytes.length - 5} data bytes`,
    );
  }
  let sum = 0;
  for (const byte of bytes) sum += byte;
  if ((sum & 0xff) !== 0) {
    throw new Error(`line ${number}: checksum does not add up to zero`);
  }
  return {
    type: bytes[3],
    address: (bytes[1] << 8) | bytes[2],
    data: bytes.subarray(4, 4 + count),
  };
}

/**
 * Parse Intel HEX text (or bytes holding that text).
 *
 * Returns `{bytes, minAddress, maxAddress, startAddress}`. `startAddress` is
 * the execution address from a type 03 or 05 record, or `null`. Overlapping
 * data, a missing end-of-file record, an unsupported record type and images
 * larger than 16 MiB are refused.
 */
export function parseIntelHex(source) {
  const text = asText(source).replace(/^﻿/, "");
  const records = text.split(/\r?\n/);
  const chunks = [];
  let base = 0;
  let minAddress = Infinity;
  let maxAddress = -Infinity;
  let startAddress = null;
  let sawEof = false;

  records.forEach((raw, index) => {
    const line = raw.trim();
    if (line === "") return;
    const number = index + 1;
    if (sawEof) throw new Error(`line ${number}: record follows the end-of-file record`);

    const { type, address, data } = parseLine(line, number);

    if (type === 0x00) {
      if (data.length === 0) return;
      const absolute = base + address;
      if (!Number.isSafeInteger(absolute) || absolute > 0xffffffff) {
        throw new Error(`line ${number}: address overflows 32 bits`);
      }
      chunks.push({ address: absolute, data: new Uint8Array(data) });
      minAddress = Math.min(minAddress, absolute);
      maxAddress = Math.max(maxAddress, absolute + data.length);
      return;
    }

    if (type === 0x01) {
      if (data.length !== 0) throw new Error(`line ${number}: end-of-file record carries data`);
      sawEof = true;
      return;
    }

    if (type === 0x02) {
      if (data.length !== 2) throw new Error(`line ${number}: extended segment address needs 2 bytes`);
      base = ((data[0] << 8) | data[1]) << 4;
      return;
    }

    if (type === 0x04) {
      if (data.length !== 2) throw new Error(`line ${number}: extended linear address needs 2 bytes`);
      base = ((data[0] << 8) | data[1]) * 0x10000;
      return;
    }

    if (type === 0x03) {
      if (data.length !== 4) throw new Error(`line ${number}: start segment address needs 4 bytes`);
      startAddress =
        (((data[0] << 8) | data[1]) << 4) + ((data[2] << 8) | data[3]);
      return;
    }

    if (type === 0x05) {
      if (data.length !== 4) throw new Error(`line ${number}: start linear address needs 4 bytes`);
      startAddress = ((data[0] << 24) | (data[1] << 16) | (data[2] << 8) | data[3]) >>> 0;
      return;
    }

    throw new Error(`line ${number}: unsupported record type 0x${type.toString(16)}`);
  });

  if (!sawEof) throw new Error("the image has no end-of-file record");
  if (chunks.length === 0) throw new Error("the image has no data records");

  const length = maxAddress - minAddress;
  if (length > MAX_IMAGE_BYTES) {
    throw new Error(`the image spans ${length} bytes, more than the ${MAX_IMAGE_BYTES} byte limit`);
  }

  const bytes = new Uint8Array(length);
  bytes.fill(0xff);
  const written = new Uint8Array(length);
  for (const chunk of chunks) {
    const offset = chunk.address - minAddress;
    for (let i = 0; i < chunk.data.length; i += 1) {
      if (written[offset + i]) {
        throw new Error(`data at 0x${(chunk.address + i).toString(16)} is written twice`);
      }
      bytes[offset + i] = chunk.data[i];
    }
    written.fill(1, offset, offset + chunk.data.length);
  }

  return { bytes, minAddress, maxAddress, startAddress };
}

/**
 * Turn a chosen file into the bytes the simulator takes.
 *
 * Intel HEX is recognised from its content; a `.hex` name also forces a parse
 * so a corrupt hex file is an error rather than a binary blob. Everything
 * else is a raw application image.
 */
export function firmwareImage(name, source) {
  const filename = String(name ?? "");
  if (typeof source === "string") {
    if (looksLikeIntelHex(source) || /\.hex$/i.test(filename)) {
      const parsed = parseIntelHex(source);
      return {
        kind: "hex",
        bytes: parsed.bytes,
        minAddress: parsed.minAddress,
        maxAddress: parsed.maxAddress,
        startAddress: parsed.startAddress,
      };
    }
    return { kind: "bin", bytes: new TextEncoder().encode(source) };
  }

  const bytes = asBytes(source);
  if (looksLikeIntelHex(bytes) || /\.hex$/i.test(filename)) {
    const parsed = parseIntelHex(bytes);
    return {
      kind: "hex",
      bytes: parsed.bytes,
      minAddress: parsed.minAddress,
      maxAddress: parsed.maxAddress,
      startAddress: parsed.startAddress,
    };
  }
  return { kind: "bin", bytes: new Uint8Array(bytes) };
}