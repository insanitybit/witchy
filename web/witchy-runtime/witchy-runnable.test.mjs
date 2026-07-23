#!/usr/bin/env node
// RFC-0041 Phase 2: the runnable cell, end to end and headless. Builds a fake DOM with a
// `<pre><code class="language-witchy">…</code></pre>` block (exactly what `markdown.to_vnode`
// now emits), enhances it with `enhanceRunnableCells`, and DRIVES Run — which compiles+runs
// the block's source with the SAME `witchy-host.js` engine + `web/witchy.wasm` the validated
// playground uses. Asserts the output equals the program's, that a compile error surfaces as
// an error cell, and that enhancement is idempotent. No browser needed: `witchy-host.js` runs
// under Node/V8 (that is what `pg_validate.mjs` uses).
//
// Usage:  WITCHY_WASM_PATH=target/wasm32-unknown-unknown/debug/witchy.wasm \
//           node web/witchy-runtime/witchy-runnable.test.mjs

import { enhanceRunnableCells } from "../witchy-runnable.js";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const COMPILER_WASM = process.env.WITCHY_WASM_PATH || resolve(REPO, "web/witchy.wasm");

// A minimal DOM, matching the glamour drivers' FakeElement shape.
class FakeNode {
  constructor() { this.childNodes = []; this.parentNode = null; }
  appendChild(c) { if (c.parentNode) c.parentNode.removeChild(c); c.parentNode = this; this.childNodes.push(c); return c; }
  removeChild(c) { const i = this.childNodes.indexOf(c); if (i >= 0) this.childNodes.splice(i, 1); c.parentNode = null; return c; }
  replaceChild(n, p) { const i = this.childNodes.indexOf(p); if (i < 0) throw new Error("replaceChild"); this.childNodes[i] = n; n.parentNode = this; p.parentNode = null; return p; }
}
class FakeText extends FakeNode {
  constructor(t) { super(); this._t = t; }
  get textContent() { return this._t; }
  set textContent(v) { this._t = v; this.childNodes = []; }
}
class FakeElement extends FakeNode {
  constructor(tag) { super(); this.el = tag; this.attributes = new Map(); this.listeners = new Map(); }
  setAttribute(n, v) { this.attributes.set(n, String(v)); }
  getAttribute(n) { return this.attributes.has(n) ? this.attributes.get(n) : null; }
  addEventListener(e, fn) { if (!this.listeners.has(e)) this.listeners.set(e, new Set()); this.listeners.get(e).add(fn); }
  dispatchEvent(ev) { const s = this.listeners.get(ev.type); if (s) for (const fn of [...s]) fn(ev); return true; }
  get textContent() { let o = ""; for (const c of this.childNodes) o += c.textContent; return o; }
  set textContent(v) { this.childNodes = []; this.appendChild(new FakeText(v)); }
}
const doc = { createElement: (t) => new FakeElement(t), createTextNode: (t) => new FakeText(t) };
const qsa = (node, tag, acc = []) => {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) qsa(c, tag, acc);
  return acc;
};

// Build `<div class=markdown><pre><code class="language-witchy">SOURCE</code></pre></div>`.
function pageWith(source) {
  const div = new FakeElement("div");
  div.setAttribute("class", "markdown");
  const pre = new FakeElement("pre");
  const code = new FakeElement("code");
  code.setAttribute("class", "language-witchy");
  code.appendChild(new FakeText(source));
  pre.appendChild(code);
  div.appendChild(pre);
  return div;
}

// The compiler: instantiate `web/witchy.wasm` (no imports) — the very module the playground
// and pg_validate use. Lazily, exactly as a browser would fetch it on first Run.
const loadCompiler = async () => {
  const bytes = readFileSync(COMPILER_WASM);
  const { instance } = await WebAssembly.instantiate(bytes, {});
  return instance.exports;
};

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

try {
  // 1. A runnable cell: enhance, then Run, and the real output shows.
  const root = pageWith('fn main(console: Console):\n    console.print("hello from a runnable cell")');
  const cells = enhanceRunnableCells(root, { document: doc, loadCompiler });
  ok(cells.length === 1, "one runnable cell is found and enhanced");
  ok(qsa(root, "button").some((b) => (b.getAttribute("class") || "").includes("witchy-run")), "a Run button is added");
  ok(qsa(root, "div").some((d) => (d.getAttribute("class") || "") === "witchy-cell"), "the cell is wrapped");
  ok(qsa(root, "button").some((b) => (b.getAttribute("class") || "").includes("witchy-copy")), "a Copy button is added to the cell");
  ok(qsa(root, "textarea").length === 1 && cells[0].editor.value.includes("hello from a runnable cell"), "the cell is EDITABLE (a textarea seeded with the source)");

  await cells[0].run();
  ok(cells[0].output.textContent === "hello from a runnable cell", "Run compiles + runs the code and shows its output");
  ok((cells[0].output.getAttribute("class") || "").includes("ok"), "a successful run is marked ok");
  ok(cells[0].statsOutput.textContent === "", "ordinary cells do not show optimization counters");

  // Editing the textarea and re-running runs the READER'S edited source, not the seed.
  cells[0].editor.value = 'fn main(console: Console):\n    console.print("the reader edited this")';
  await cells[0].run();
  ok(cells[0].output.textContent === "the reader edited this", "Run executes the reader's EDITED source");

  // 2. Browser argv is explicit page-supplied launch input.
  const argv = pageWith(`fn main(console: Console, args: List(String)):
    console.print("\${args.length()}")
    for arg in args:
        console.print(arg)`);
  const argvCells = enhanceRunnableCells(argv, {
    document: doc,
    loadCompiler,
    runOptions: { args: ["one", "héllo"] },
  });
  await argvCells[0].run();
  ok(argvCells[0].output.textContent === "2\none\nhéllo", "a runnable cell receives ordered UTF-8 argv");

  // 3. The book host receives explicit page-supplied SecretStore grants.
  const secrets = pageWith(`import crypto
import secretstore

fn main(console: Console, secrets: SecretStore):
    let signing = secrets.require("signing")
    console.print("signature length \${crypto.sign(signing, "release v1.2.3").length()}")
    console.print("token: \${crypto.reveal(secrets.require("api-token"))}")`);
  const secretCells = enhanceRunnableCells(secrets, {
    document: doc,
    loadCompiler,
    runOptions: {
      capabilities: {
        secrets: {
          signing: { value: "0".repeat(64), useOnly: true },
          "api-token": "sk-live-abc",
        },
      },
    },
  });
  await secretCells[0].run();
  ok(
    secretCells[0].output.textContent === "signature length 128\ntoken: sk-live-abc",
    "a runnable cell signs with an opaque secret and reveals only the value secret",
  );

  // 4. The book host delegates VM examples to fresh sequential instances.
  const vm = pageWith(`import vm
import bytes

fn step(state: Bytes, request: Bytes) -> Bytes:
    bytes.concat(state, request)

fn main(console: Console):
    let responses = vm.serve(
        bytes.from_string(""),
        [bytes.from_string("a"), bytes.from_string("b")],
        step,
    )
    for response in responses:
        console.print(bytes.to_string(response))`);
  const vmWorkers = [];
  const vmCells = enhanceRunnableCells(vm, {
    document: doc,
    loadCompiler,
    runOptions: {
      capabilities: { vm: true },
      onVmSpawn: (instance) => vmWorkers.push(instance),
    },
  });
  await vmCells[0].run();
  ok(vmCells[0].output.textContent === "a\nab", "a runnable cell executes vm.serve sequentially");
  ok(vmWorkers.length === 1, "the runnable VM cell uses one fresh worker instance");

  // 5. Idempotent: re-enhancing the same root finds nothing new.
  const again = enhanceRunnableCells(root, { document: doc, loadCompiler });
  ok(again.length === 0, "re-enhancing is idempotent (no double cells)");

  // 6. A compile error surfaces as an error cell, not a thrown exception.
  const bad = pageWith("fn main(console: Console):\n    console.print(nope)");
  const badCells = enhanceRunnableCells(bad, { document: doc, loadCompiler });
  await badCells[0].run();
  ok((badCells[0].output.getAttribute("class") || "").includes("err"), "a compile error marks the cell err");
  ok(badCells[0].output.textContent.length > 0, "the error message is shown");

  // 7. RFC-0089: the browser reads the same compiled-Wasm resource counters as
  // native `witchy stats`. A 50,000-transition FIP kernel performs only its four
  // fixed setup allocations and no per-transition reuse/free/rewind work.
  const fip = pageWith(`mode opt

type State:
    count: Int

fn run(own state: unique State, n: Int) -> unique State:
    if n == 0:
        return state
    state.count = state.count + 1
    run(state, n - 1)

fn main(console: Console):
    let done = run(State(0), 50000)
    console.print("\${done.count}")`);
  const fipCells = enhanceRunnableCells(fip, { document: doc, loadCompiler });
  await fipCells[0].run();
  ok(fipCells[0].output.textContent === "50000", "the browser completes 50,000 FIP transitions");
  const proof = fipCells[0].statsOutput.textContent;
  ok(proof.includes("rc_alloc_calls 4"), "the browser reports four fixed RC allocations");
  ok(proof.includes("bump_alloc_calls 4"), "the browser reports four fixed bump allocations");
  ok(proof.includes("rc_reuse_calls 0"), "FIP depth adds no free-list reuse");
  ok(proof.includes("rc_free_calls 0"), "FIP depth adds no frees");
  ok(proof.includes("region_rewind_calls 0"), "FIP depth adds no region rewinds");

  // 8. RFC-0098: browser-visible counters compare a shallow projection with a
  // projection-heavy loop. Sixty-three additional exact target constructions
  // add at most sixty-three allocator calls; output proves the richer source is
  // still intact. This is an operation-count bound, never a timing assertion.
  const projectionKernel = (iterations) => `mode opt

type Summary = .{id: Int, label: String}
type Detailed = .{..Summary, revision: Int}

fn main(console: Console):
    let row: Detailed = .{id: 7, label: "ready", revision: 3}
    var i = 0
    var total = 0
    while i < ${iterations}:
        let summary: Summary = row
        total = total + summary.id
        i = i + 1
    console.print("\${total} \${row.revision}")`;
  const counterValue = (proofText, name) => {
    const line = proofText.split("\n").find((entry) => entry.startsWith(`${name} `));
    if (!line) throw new Error(`missing browser counter ${name}: ${proofText}`);
    return BigInt(line.slice(name.length + 1));
  };
  const shallowProjection = enhanceRunnableCells(pageWith(projectionKernel(1)), { document: doc, loadCompiler })[0];
  const heavyProjection = enhanceRunnableCells(pageWith(projectionKernel(64)), { document: doc, loadCompiler })[0];
  await shallowProjection.run();
  await heavyProjection.run();
  ok(shallowProjection.output.textContent === "7 3", "the shallow projection preserves its richer source");
  ok(heavyProjection.output.textContent === "448 3", "the projection-heavy loop preserves semantics");
  const shallowProof = shallowProjection.statsOutput.textContent;
  const heavyProof = heavyProjection.statsOutput.textContent;
  ok(
    counterValue(heavyProof, "rc_alloc_calls") - counterValue(shallowProof, "rc_alloc_calls") <= 63n,
    "63 additional projections add at most 63 RC allocations",
  );
  ok(
    counterValue(heavyProof, "bump_alloc_calls") - counterValue(shallowProof, "bump_alloc_calls") <= 63n,
    "63 additional projections add at most 63 bump allocations",
  );
  ok(
    counterValue(heavyProof, "region_rewind_calls") - counterValue(shallowProof, "region_rewind_calls") === 63n,
    "the browser observes one closed loop region per additional projection",
  );

  // 9. A non-witchy code block is left alone.
  const other = new FakeElement("div");
  const pre = new FakeElement("pre");
  const code = new FakeElement("code");
  code.setAttribute("class", "language-sh");
  code.appendChild(new FakeText("echo hi"));
  pre.appendChild(code); other.appendChild(pre);
  ok(enhanceRunnableCells(other, { document: doc, loadCompiler }).length === 0, "a non-witchy fence is not enhanced");
} catch (e) {
  console.error("harness threw:", e);
  failures++;
}

if (failures > 0) {
  console.error(`\nWITCHY-RUNNABLE FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nWITCHY-RUNNABLE OK");
process.exit(0);
