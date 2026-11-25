// Exercise the browser worker's section checks without executing guest code.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createContext, runInContext } from "node:vm";

const worker = readFileSync(new URL("../../../grimoire/glamour-worker.mjs", import.meta.url), "utf8");
const header = [0, 97, 115, 109, 1, 0, 0, 0];

function inspect(sections) {
    const context = createContext({ self: {}, input: [...header, ...sections] });
    runInContext(worker, context);
    return runInContext("checkMemory(Uint8Array.from(input).buffer)", context);
}

function memory(...bytes) {
    return [5, bytes.length, ...bytes];
}

test("admits memoryless and bounded private-memory envelopes", () => {
    assert.equal(inspect([]), false);
    assert.equal(inspect(memory(1, 1, 2, 2)), true);
    assert.equal(inspect(memory(2, 1, 3, 3, 1, 1, 1)), true);
});

test("refuses any declaration exceeding the fixed memory grant", () => {
    assert.throws(() => inspect(memory(1, 0, 2)), /private memory32/);
    assert.throws(() => inspect(memory(1, 3, 2, 2)), /private memory32/);
    assert.throws(() => inspect(memory(1, 1, 2, 3)), /fixed size/);
    assert.throws(() => inspect(memory(1, 1, 5, 5)), /memory budget/);
    assert.throws(() => inspect(memory(2, 1, 4, 4, 1, 1, 1)), /memory budget/);
});

test("refuses table and start sections and malformed envelope lengths", () => {
    assert.throws(() => inspect([4, 1, 0]), /invalid filter section/);
    assert.throws(() => inspect([8, 1, 0]), /invalid filter section/);
    assert.throws(() => inspect([5, 9, 1, 1, 2, 2]), /truncated/);
    assert.throws(() => inspect([5, 2, 1, 1, 10, 0]), /invalid filter section integer/);
    assert.throws(() => inspect([5, 128, 128, 128, 128, 16]), /invalid filter section integer/);
});

test("the worker declines an over-budget declaration before calling the engine", () => {
    let compiles = 0;
    let closed = false;
    const messages = [];
    const context = createContext({
        input: [...header, ...memory(1, 1, 5, 5)],
        self: {
            postMessage(message) { messages.push(message.failed); },
            close() { closed = true; },
        },
        WebAssembly: {
            Module: class { constructor() { compiles++; } },
        },
    });
    runInContext(worker, context);
    runInContext("self.onmessage({ data: { module: Uint8Array.from(input).buffer } })", context);
    assert.equal(compiles, 0);
    assert.deepEqual(messages, [true]);
    assert.equal(closed, true);
});
