#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { encodingOp, instantiate } from "./witchy-runtime.mjs";

const utf8 = new TextEncoder();
const text = (value) => utf8.encode(value);
const decoded = (value) => new TextDecoder().decode(value);

const textCases = [
  [0, text("Hi"), "4869"],
  [1, text("4869"), "Hi"],
  [2, text("Hi"), "SGk="],
  [3, text("SGk="), "Hi"],
  [4, text("4869"), "SGk"],
  [5, text("SGk"), "Hi"],
  [6, text("SGk"), "4869"],
  [7, new Uint8Array([0x48, 0xff, 0x69]), "H\uFFFDi"],
  [8, new Uint8Array([0x00, 0xff]), "00ff"],
  [9, new Uint8Array([0x00, 0xff]), "AP8="],
  [10, new Uint8Array([0x00, 0xff]), "AP8"],
];

for (const [op, input, expected] of textCases) {
  assert.equal(decoded(encodingOp(op, input)), expected, `encoding op ${op}`);
}

for (const [op, input] of [
  [11, text("00ff")],
  [12, text("AP8=")],
  [13, text("AP8")],
]) {
  assert.deepEqual([...encodingOp(op, input)], [0x00, 0xff], `encoding op ${op}`);
}

assert.throws(() => encodingOp(1, text("xyz")), /not valid hex/);
assert.throws(() => encodingOp(11, text("xyz")), /not valid hex/);
assert.throws(() => encodingOp(14, text("Hi")), /unknown encoding op 14/);

const SOURCE = `import bytes
import encoding
import show

fn print_text(console: Console, value: Result(String, encoding.EncodingError)):
    match value:
        Ok(text) -> console.print(text)
        Err(error) -> console.print("ERR: " + encoding.encoding_error_message(error))

fn print_bytes(console: Console, value: Result(Bytes, encoding.EncodingError)):
    match value:
        Ok(data) -> console.print(show.render(bytes.to_list(data)))
        Err(error) -> console.print("ERR: " + encoding.encoding_error_message(error))

fn main(console: Console):
    let hi = bytes.from_string("Hi")
    console.print(encoding.hex_encode("Hi"))
    console.print(encoding.hex_encode_bytes(hi))
    console.print(encoding.base64_encode("Hi"))
    console.print(encoding.base64_encode_bytes(hi))
    console.print(encoding.base64url_encode_bytes(hi))
    console.print(bytes.to_string_lossy(hi))
    print_text(console, encoding.hex_decode("4869"))
    print_bytes(console, encoding.hex_decode_bytes("00ff"))
    print_text(console, encoding.base64_decode("SGk="))
    print_bytes(console, encoding.base64_decode_bytes("AP8="))
    print_text(console, encoding.base64url_decode("SGk"))
    print_bytes(console, encoding.base64url_decode_bytes("AP8"))
    print_text(console, encoding.base64url_to_hex("SGk"))
    print_text(console, encoding.hex_to_base64url("4869"))
`;

const expected = [
  "4869",
  "4869",
  "SGk=",
  "SGk=",
  "SGk",
  "Hi",
  "Hi",
  "[0, 255]",
  "Hi",
  "[0, 255]",
  "Hi",
  "[0, 255]",
  "4869",
  "SGk",
];

const bin = resolve(process.argv[2] || "target/debug/witchy");
const work = mkdtempSync(join(tmpdir(), "witchy-encoding-abi-"));
try {
  const source = join(work, "main.witchy");
  const wasmPath = join(work, "main.wasm");
  writeFileSync(source, SOURCE);
  execFileSync(bin, ["compile", source, "--out", wasmPath], { cwd: work, stdio: "pipe" });

  const nativeLines = execFileSync(bin, [source], { cwd: work, encoding: "utf8" })
    .replace(/\n+$/, "")
    .split("\n");
  assert.deepEqual(nativeLines, expected, "native encoding output");

  const runtime = await instantiate(readFileSync(wasmPath));
  assert.deepEqual(runtime.run(), expected, "browser encoding output");
} finally {
  rmSync(work, { recursive: true, force: true });
}

console.log("ENCODING-ABI OK");
