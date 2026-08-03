#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { instantiate } from "./witchy-runtime.mjs";

const BIN = resolve(process.argv[2] || "target/debug/witchy");
const work = mkdtempSync(join(tmpdir(), "glamour-stateful-abi-"));
const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

try {
  const source = join(work, "stateful.witchy");
  const wasmPath = join(work, "stateful.wasm");
  writeFileSync(
    source,
    `import bytes

grantable capability UiRoot:
    policy: String

type BrowserState:
    Active(String, Int)
    Dormant(String)

@browser
pub fn glamour_init(root: UiRoot, input: Bytes) -> BrowserState:
    match root:
        UiRoot(_) -> Active(bytes.to_string(input), bytes.length(input))

@browser
pub fn glamour_dispatch(state: BrowserState, input: Bytes) -> BrowserState:
    match state:
        Active(label, count) -> Active(label, count + bytes.length(input))
        Dormant(label) -> Active(label, bytes.length(input))

@browser
pub fn glamour_emit(state: BrowserState) -> Bytes:
    match state:
        Active(label, count) -> bytes.from_string("\${label}:\${count}")
        Dormant(label) -> bytes.from_string(label)

@browser
pub fn glamour_release(own state: BrowserState):
    match state:
        Active(_, _) -> Nil
        Dormant(_) -> Nil
`,
  );
  execFileSync(BIN, ["compile", source, "--out", wasmPath], { cwd: work });
  const runtime = await instantiate(readFileSync(wasmPath), {
    userCaps: [["stateful"]],
  });
  const { instance, memory } = runtime;
  const exports = instance.exports;
  assert.equal(exports.__glamour_protocol_version(), (1 << 16) | 4);
  assert.equal(exports.glamour_init, undefined);
  assert.equal(exports.glamour_dispatch, undefined);
  assert.equal(exports.__glamour_state, undefined);

  const stage = (text) => {
    const bytes = encoder.encode(text);
    const pointer = exports.__glamour_input_reserve(bytes.byteLength);
    new Uint8Array(memory.buffer).set(bytes, pointer);
    return { pointer, length: bytes.byteLength };
  };
  const readOutput = (pointer) => {
    const length = exports.__glamour_output_length();
    return decoder.decode(new Uint8Array(memory.buffer).slice(pointer, pointer + length));
  };

  const initial = stage("abc");
  const initialOutput = exports.__glamour_init(initial.pointer, initial.length);
  assert.equal(readOutput(initialOutput), "abc:3");
  assert.throws(
    () => {
      const event = stage("x");
      exports.__glamour_dispatch(event.pointer, event.length);
    },
    WebAssembly.RuntimeError,
    "a borrowed output blocks the next dispatch",
  );
  exports.__glamour_output_release();

  const event = stage("xy");
  const eventOutput = exports.__glamour_dispatch(event.pointer, event.length);
  assert.equal(readOutput(eventOutput), "abc:5", "the model persisted inside the Wasm instance");
  exports.__glamour_output_release();
  exports.__glamour_output_release();

  for (let index = 0; index < 1000; index += 1) {
    const idle = stage("");
    const output = exports.__glamour_dispatch(idle.pointer, idle.length);
    assert.equal(readOutput(output), "abc:5");
    exports.__glamour_output_release();
  }
  const pages = memory.buffer.byteLength / (64 * 1024);
  assert.ok(pages <= 8, `aggregate state stays within eight Wasm pages (got ${pages})`);

  const duplicateInit = stage("");
  assert.throws(
    () => exports.__glamour_init(duplicateInit.pointer, duplicateInit.length),
    WebAssembly.RuntimeError,
    "one Wasm instance owns exactly one application",
  );

  exports.__glamour_dispose();
  exports.__glamour_dispose();
  const late = stage("late");
  assert.throws(
    () => exports.__glamour_dispatch(late.pointer, late.length),
    WebAssembly.RuntimeError,
    "dispatch after dispose fails closed",
  );

  const resumedRuntime = await instantiate(readFileSync(wasmPath), {
    userCaps: [["stateful-resume"]],
  });
  const resumedExports = resumedRuntime.instance.exports;
  const resumedState = encoder.encode("resume");
  const resumedPointer = resumedExports.__glamour_input_reserve(
    resumedState.byteLength,
  );
  new Uint8Array(resumedRuntime.memory.buffer).set(resumedState, resumedPointer);
  assert.equal(
    resumedExports.__glamour_resume(resumedPointer, resumedState.byteLength),
    0,
  );
  assert.equal(
    resumedExports.__glamour_output_length(),
    0,
    "resume restores Wasm-owned state without replaying the initial render",
  );
  const resumedEvent = encoder.encode("xy");
  const resumedEventPointer = resumedExports.__glamour_input_reserve(
    resumedEvent.byteLength,
  );
  new Uint8Array(resumedRuntime.memory.buffer).set(
    resumedEvent,
    resumedEventPointer,
  );
  const resumedOutput = resumedExports.__glamour_dispatch(
    resumedEventPointer,
    resumedEvent.byteLength,
  );
  assert.equal(
    decoder.decode(
      new Uint8Array(resumedRuntime.memory.buffer).slice(
        resumedOutput,
        resumedOutput + resumedExports.__glamour_output_length(),
      ),
    ),
    "resume:8",
  );
  resumedExports.__glamour_output_release();
  resumedExports.__glamour_dispose();

  console.log("GLAMOUR-STATEFUL-ABI OK");
} finally {
  rmSync(work, { recursive: true, force: true });
}
