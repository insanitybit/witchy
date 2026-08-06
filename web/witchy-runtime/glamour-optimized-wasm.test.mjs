#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mountOptimized } from "./glamour-optimized.mjs";
import {
  optimizedCounterManifest,
  optimizedCounterResumeFrame,
  optimizedCounterResumeManifest,
  optimizedCounterStartFrame,
} from "./glamour-optimized-counter-fixture.mjs";
import { FakeElement, fakeDocument } from "./glamour-test-dom.mjs";
import { instantiate } from "./witchy-runtime.mjs";

const BIN = resolve(process.argv[2] || "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const PROJECT = join(REPO, "projects/glamour/examples/optimized_counter");
const work = mkdtempSync(join(tmpdir(), "glamour-optimized-wasm-"));

class ActionFormData {
  constructor(form) {
    this.form = form;
  }

  entries() {
    return this.form.entries[Symbol.iterator]();
  }
}

try {
  const wasmPath = join(work, "optimized-counter.wasm");
  execFileSync(
    BIN,
    ["compile", "src/optimized_counter.witchy", "--out", wasmPath],
    { cwd: PROJECT },
  );
  const wasmBytes = readFileSync(wasmPath);
  const manifest = {
    ...optimizedCounterManifest(),
    actions: [{
      id: "glamour-form1-00260bc33b35b90bb6dce5d11da82aa2f1fc273d789f581676bbb5254880aba2",
      method: "POST",
      action: "/signup",
      inputSchema: 2_859_606_054,
      resultSchema: 4_195_218_877,
      fields: [
        { name: "email", label: "Email", kind: "email", required: true },
        { name: "password", label: "Password", kind: "secret", required: true },
        { name: "updates", label: "Updates", kind: "checkbox", required: false },
      ],
    }],
  };
  const startFrame = optimizedCounterStartFrame();
  let runtime;
  const requests = [];
  const root = new FakeElement("root");
  const app = await mountOptimized(wasmBytes, root, {
    document: fakeDocument,
    manifest,
    startFrame,
    FormData: ActionFormData,
    baseUrl: "https://witchy.example/",
    formFetch: async (url, init) => {
      requests.push({ url, init });
      return { ok: true, status: 204 };
    },
    instantiateOptions: { userCaps: [["optimized-counter"]] },
    instantiate: async (bytes, options) => {
      runtime = await instantiate(bytes, options);
      return runtime;
    },
  });
  const list = root.childNodes[0];
  const countNode = list.childNodes[0];
  const [one, two, three] = list.childNodes.slice(1);
  assert.equal(countNode.textContent, "0");
  assert.deepEqual(list.childNodes.slice(1).map((item) => item.textContent), ["1", "2", "3"]);
  assert.equal(list.listeners.size, 0);
  assert.equal(root.listeners.get("click").size, 1);
  assert.equal(app.getModel, undefined, "the optimized host exposes no model snapshot");
  one.scrollTop = 17;
  two.selectionStart = 1;
  three.__imeComposition = "active";

  let prevented = 0;
  for (let count = 1; count <= 1000; count += 1) {
    root.dispatchEvent({
      type: "click",
      target: list,
      composedPath: () => [list, root],
      preventDefault: () => {
        prevented += 1;
      },
    });
    assert.equal(countNode.textContent, String(count));
    assert.deepEqual(
      list.childNodes.slice(1),
      count % 2 === 1 ? [three, one, two] : [one, two, three],
      "the Wasm-computed one-move plan preserves keyed node identity",
    );
  }
  assert.equal(prevented, 1000);
  assert.equal(root.listeners.get("click").size, 1);
  assert.equal(one.scrollTop, 17);
  assert.equal(two.selectionStart, 1);
  assert.equal(three.__imeComposition, "active");
  const pages = runtime.memory.buffer.byteLength / (64 * 1024);
  assert.ok(pages <= 8, `the bounded counter trace stays within eight Wasm pages (got ${pages})`);

  const form = new FakeElement("form");
  form.entries = [
    ["email", "ada@example.test"],
    ["password", "s3cret"],
    ["updates", "true"],
  ];
  form.setAttribute(
    "data-glamour-form",
    "glamour-form1-00260bc33b35b90bb6dce5d11da82aa2f1fc273d789f581676bbb5254880aba2",
  );
  form.setAttribute("method", "POST");
  form.setAttribute("action", "/signup");
  root.appendChild(form);
  const submit = {
    target: form,
    defaultPrevented: false,
    composedPath: () => [form, root],
    preventDefault() {
      this.defaultPrevented = true;
    },
  };
  await [...root.listeners.get("submit")][0](submit);
  assert.equal(submit.defaultPrevented, true);
  assert.equal(countNode.textContent, "1110");
  assert.equal(requests.length, 1);
  assert.match(requests[0].init.body.toString(), /password=s3cret/);

  app.dispose();
  assert.equal(root.listeners.get("click").size, 0);

  const resumedRoot = new FakeElement("root");
  const resumedList = new FakeElement("ul");
  const resumedCount = fakeDocument.createTextNode("5");
  const item = (value) => {
    const element = new FakeElement("li");
    element.appendChild(fakeDocument.createTextNode(value));
    return element;
  };
  const resumedThree = item("3");
  const resumedOne = item("1");
  const resumedTwo = item("2");
  resumedList.appendChild(resumedCount);
  resumedList.appendChild(resumedThree);
  resumedList.appendChild(resumedOne);
  resumedList.appendChild(resumedTwo);
  resumedRoot.appendChild(resumedList);
  const resumedManifest = optimizedCounterResumeManifest(5);
  const resumedApp = await mountOptimized(wasmBytes, resumedRoot, {
    document: fakeDocument,
    manifest: resumedManifest,
    startFrame: optimizedCounterResumeFrame(5),
    resume: true,
    instantiateOptions: { userCaps: [["optimized-counter-resume"]] },
  });
  assert.equal(resumedRoot.childNodes[0], resumedList);
  resumedApp.dispatch({
    plan: 31,
    node: 20,
    name: "click",
    value: "",
    checked: false,
    key: "",
    composing: false,
    userActivation: true,
  });
  assert.equal(resumedCount.textContent, "6");
  assert.deepEqual(
    resumedList.childNodes.slice(1),
    [resumedOne, resumedTwo, resumedThree],
    "the resumed patch preserves and reorders the server-rendered keyed nodes",
  );
  resumedApp.dispose();
  console.log("GLAMOUR-OPTIMIZED-WASM OK");
} finally {
  rmSync(work, { recursive: true, force: true });
}
