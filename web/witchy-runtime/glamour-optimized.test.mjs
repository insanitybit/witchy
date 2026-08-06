#!/usr/bin/env node

import assert from "node:assert/strict";
import { mountOptimized } from "./glamour-optimized.mjs";
import {
  FrameKind,
  GLAMOUR_HEADER_BYTES,
  Op,
  encodeOperation,
  encodeOutputFrame,
} from "./glamour-protocol.mjs";
import { FakeElement, fakeDocument as document } from "./glamour-test-dom.mjs";

const APP_ID = 7;
const BUILD_ID = 0x0102_0304_0506_0708n;
const encoder = new TextEncoder();

function frameWithText(sequence, value, extraOperations = []) {
  const payload = encoder.encode(value);
  const text = encodeOperation(Op.SetText, [11, 0, payload.byteLength]);
  const operations = [text, ...extraOperations];
  const payloadOffset =
    GLAMOUR_HEADER_BYTES +
    operations.reduce((total, operation) => total + operation.byteLength, 0);
  new DataView(text.buffer).setUint32(12, payloadOffset, true);
  return encodeOutputFrame({
    appId: APP_ID,
    buildId: BUILD_ID,
    sequence,
    operations,
    payloads: [payload],
  });
}

function frameWithTextAndCustomProperty(sequence, textValue, propertyValue, sink = 27, minor = 2) {
  const textPayload = encoder.encode(textValue);
  const propertyPayload = encoder.encode(propertyValue);
  const text = encodeOperation(Op.SetText, [11, 0, textPayload.byteLength]);
  const property = encodeOperation(Op.SetCustomProperty, [10, sink, 0, propertyPayload.byteLength]);
  const payloadOffset = GLAMOUR_HEADER_BYTES + text.byteLength + property.byteLength;
  new DataView(text.buffer).setUint32(12, payloadOffset, true);
  new DataView(property.buffer).setUint32(16, payloadOffset + textPayload.byteLength, true);
  return encodeOutputFrame({
    appId: APP_ID,
    buildId: BUILD_ID,
    sequence,
    minor,
    operations: [text, property],
    payloads: [textPayload, propertyPayload],
  });
}

const initialPayload = encoder.encode("0");
const mount = encodeOperation(Op.Mount, [3, 1, 0, 0, 1, 1, 0, initialPayload.byteLength]);
const eventPlan = encodeOperation(Op.SetEventPlan, [10, 29, 31]);
const initialPayloadOffset =
  GLAMOUR_HEADER_BYTES + mount.byteLength + eventPlan.byteLength;
new DataView(mount.buffer).setUint32(32, initialPayloadOffset, true);
const initialFrame = encodeOutputFrame({
  kind: FrameKind.Mount,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 0n,
  operations: [mount, eventPlan],
  payloads: [initialPayload],
});
const updateFrame = frameWithTextAndCustomProperty(1n, "1", "12px");
const invalidFrame = frameWithText(
  2n,
  "2",
  [encodeOperation(Op.SetProperty, [10, 999, initialPayloadOffset, 0])],
);
const startFrame = encodeOutputFrame({
  kind: FrameKind.Start,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 0n,
});

function fakeRuntime(outputs, protocolMinor = 0) {
  const memory = { buffer: new ArrayBuffer(64 * 1024) };
  const inputPointer = 1024;
  const outputPointer = 8192;
  let outputLength = 0;
  let dispatches = 0;
  let disposals = 0;
  let resumes = 0;
  const inputs = [];
  const stage = (frame) => {
    new Uint8Array(memory.buffer).set(frame, outputPointer);
    outputLength = frame.byteLength;
    return outputPointer;
  };
  return {
    memory,
    stage,
    get dispatches() {
      return dispatches;
    },
    get disposals() {
      return disposals;
    },
    get resumes() {
      return resumes;
    },
    get inputs() {
      return inputs;
    },
    instance: {
      exports: {
        __glamour_protocol_version: () => (1 << 16) | protocolMinor,
        __glamour_input_reserve: (length) => {
          assert.ok(length >= GLAMOUR_HEADER_BYTES);
          return inputPointer;
        },
        __glamour_init: () => stage(outputs[0]),
        __glamour_resume: (pointer, length) => {
          const view = new DataView(memory.buffer, pointer, length);
          assert.equal(view.getUint8(8), FrameKind.Start);
          resumes += 1;
          outputLength = 0;
          return 0;
        },
        __glamour_dispatch: (pointer, length) => {
          const view = new DataView(memory.buffer, pointer, length);
          const kind = view.getUint8(8);
          inputs.push(new Uint8Array(memory.buffer).slice(pointer, pointer + length));
          if (kind === FrameKind.Event) {
            assert.equal(view.getUint32(GLAMOUR_HEADER_BYTES + 8, true), 31);
          } else {
            assert.ok(
              kind === FrameKind.ActionInput || kind === FrameKind.ActionCompletion,
              `unexpected input frame kind ${kind}`,
            );
          }
          dispatches += 1;
          return stage(outputs[dispatches]);
        },
        __glamour_output_length: () => outputLength,
        __glamour_output_release: () => {
          outputLength = 0;
        },
        __glamour_dispose: () => {
          disposals += 1;
        },
      },
    },
  };
}

function installDevelopmentMetadata(
  runtime,
  fields = [3],
  protocolMinor = 2,
  snapshotFormat = 1,
  names = fields.map(() => null),
) {
  assert.equal(names.length, fields.length);
  const memory = new Uint8Array(runtime.memory.buffer);
  const pointer = 128;
  const encodedNames = names.map((name) => encoder.encode(name || ""));
  const metadata = new Uint8Array(
    80 + fields.length + encodedNames.reduce((size, name) => size + 2 + name.length, 0),
  );
  metadata.set(encoder.encode("WGDM"), 0);
  const view = new DataView(metadata.buffer);
  view.setUint16(4, 2, true);
  view.setUint16(6, snapshotFormat, true);
  view.setUint16(8, 1, true);
  view.setUint16(10, protocolMinor, true);
  view.setUint32(12, fields.length, true);
  metadata.fill(0x11, 16, 48);
  metadata.fill(0x22, 48, 80);
  metadata.set(fields, 80);
  let cursor = 80 + fields.length;
  for (const name of encodedNames) {
    view.setUint16(cursor, name.length, true);
    cursor += 2;
    metadata.set(name, cursor);
    cursor += name.length;
  }
  new DataView(runtime.memory.buffer).setUint32(pointer, metadata.byteLength, true);
  memory.set(metadata, pointer + 4);
  const changesPointer = pointer + metadata.byteLength + 4;
  runtime.instance.exports.__glamour_dev_metadata = () => pointer;
  runtime.instance.exports.__glamour_dev_changes = () => changesPointer;
  runtime.instance.exports.__glamour_dev_changes_length = () => fields.length;
  memory.fill(1, changesPointer, changesPointer + fields.length);
  return { memory, metadata, pointer };
}

const runtime = fakeRuntime([initialFrame, updateFrame, invalidFrame], 2);
const manifest = {
  appId: APP_ID,
  buildId: BUILD_ID,
  registryId: 1,
  templates: new Map([
    [
      3,
      {
        root: {
          kind: "element",
          tag: "button",
          node: 10,
          children: [{ kind: "text", node: 11, text: "" }],
        },
        slots: new Map([[1, { kind: "text", node: 11 }]]),
      },
    ],
  ]),
  nodes: new Map([
    [10, { template: 3 }],
    [11, { template: 3 }],
  ]),
  regions: new Map(),
  properties: new Map([[17, "value"]]),
  attributes: new Map(),
  aria: new Map(),
  customProperties: new Map([
    [27, { name: "--glamour-gap", category: "length" }],
    [28, { name: "--glamour-position", category: "percentage" }],
    [30, { name: "--glamour-turn", category: "angle" }],
    [32, { name: "--glamour-delay", category: "time" }],
  ]),
  eventClasses: new Map([[29, { name: "click", capture: false }]]),
  eventPlans: new Map([
    [
      31,
      {
        eventClass: 29,
        instance: 1,
        preventDefault: true,
        stopPropagation: false,
      },
    ],
  ]),
};

const freshPayload = encoder.encode("0");
const freshAttributePayload = encoder.encode("button");
const freshMount = encodeOperation(Op.Mount, [
  3, 1, 0, 0, 2,
  1, 0, freshPayload.byteLength,
  2, 0, freshAttributePayload.byteLength,
]);
const freshPayloadOffset = GLAMOUR_HEADER_BYTES + freshMount.byteLength;
new DataView(freshMount.buffer).setUint32(32, freshPayloadOffset, true);
new DataView(freshMount.buffer).setUint32(44, freshPayloadOffset + freshPayload.byteLength, true);
const freshFrame = encodeOutputFrame({
  kind: FrameKind.Mount,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 0n,
  operations: [freshMount],
  payloads: [freshPayload, freshAttributePayload],
});
const freshManifest = {
  ...manifest,
  templates: new Map([[
    3,
    {
      ...manifest.templates.get(3),
      slots: new Map([
        [1, { kind: "text", node: 11 }],
        [2, { kind: "attribute", node: 10, sink: 18 }],
      ]),
      events: [{ node: 10, eventClass: 29, eventPlan: 31 }],
    },
  ]]),
  attributes: [{ id: 18, name: "type" }],
};
const freshRuntime = fakeRuntime([freshFrame, updateFrame], 2);
const freshRoot = new FakeElement("root");
const fallback = document.createElement("p");
fallback.appendChild(document.createTextNode("inert fallback"));
freshRoot.appendChild(fallback);

let forgedOwnerInstantiated = false;
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("root"), {
    document,
    manifest: {
      ...manifest,
      ownerInstances: new Map([[1, { declaration: 73, kind: "root" }]]),
      eventPlans: new Map([[
        31,
        {
          ...manifest.eventPlans.get(31),
          ownerScope: 74,
        },
      ]]),
    },
    startFrame,
    instantiate: async () => {
      forgedOwnerInstantiated = true;
      return fakeRuntime([initialFrame], 2);
    },
  }),
  /event plan does not belong to its authenticated owner instance/,
);
assert.equal(forgedOwnerInstantiated, false);

const freshApp = await mountOptimized(new Uint8Array(), freshRoot, {
  document,
  manifest: freshManifest,
  startFrame,
  replaceRoot: true,
  instantiate: async () => freshRuntime,
});
assert.equal(freshRoot.childNodes.length, 1);
assert.equal(freshRoot.childNodes[0].tag, "button");
assert.equal(freshRoot.childNodes[0].attributes.get("type"), "button");
assert.equal(fallback.parentNode, null, "fresh mount replaces the inert fallback at commit");
freshRoot.dispatchEvent({
  type: "click",
  target: freshRoot.childNodes[0],
  composedPath: () => [freshRoot.childNodes[0], freshRoot],
  preventDefault() {},
});
assert.equal(freshRuntime.dispatches, 1, "template events become live with the fresh mount");
freshApp.dispose();

const invalidFreshProperty = encodeOperation(Op.SetProperty, [10, 999, 0, 0]);
const invalidFreshMount = freshMount.slice();
new DataView(invalidFreshMount.buffer).setUint32(
  32,
  GLAMOUR_HEADER_BYTES + invalidFreshMount.byteLength + invalidFreshProperty.byteLength,
  true,
);
new DataView(invalidFreshMount.buffer).setUint32(
  44,
  GLAMOUR_HEADER_BYTES + invalidFreshMount.byteLength + invalidFreshProperty.byteLength + freshPayload.byteLength,
  true,
);
const invalidFreshFrame = encodeOutputFrame({
  kind: FrameKind.Mount,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 0n,
  operations: [invalidFreshMount, invalidFreshProperty],
  payloads: [freshPayload, freshAttributePayload],
});
const invalidFreshRoot = new FakeElement("root");
const preservedFallback = document.createElement("p");
preservedFallback.appendChild(document.createTextNode("preserved fallback"));
invalidFreshRoot.appendChild(preservedFallback);
await assert.rejects(
  mountOptimized(new Uint8Array(), invalidFreshRoot, {
    document,
    manifest: freshManifest,
    startFrame,
    replaceRoot: true,
    instantiate: async () => fakeRuntime([invalidFreshFrame], 2),
  }),
  /unknown property 999/,
);
assert.equal(invalidFreshRoot.childNodes[0], preservedFallback, "rejected fresh output leaves fallback untouched");

const root = new FakeElement("root");
const app = await mountOptimized(new Uint8Array(), root, {
  document,
  manifest,
  startFrame,
  instantiate: async () => runtime,
});
assert.equal(root.childNodes.length, 1);
const button = root.childNodes[0];
assert.equal(button.textContent, "0");
assert.equal(app.listenerCount, 1);
assert.equal(app.inspectDevelopment, undefined, "ordinary mounts expose no inspection bridge");
assert.equal(root.listeners.get("click").size, 1, "one delegated root listener is installed");
assert.equal(button.listeners.size, 0, "static nodes have no per-node listeners");

let prevented = 0;
button.parentNode.dispatchEvent({
  type: "click",
  target: button,
  composedPath: () => [button, root],
  preventDefault: () => {
    prevented += 1;
  },
});
assert.equal(button.textContent, "1");
assert.equal(button.style.getPropertyValue("--glamour-gap"), "12px");
assert.equal(runtime.dispatches, 1);
assert.equal(prevented, 1);
assert.equal(root.listeners.get("click").size, 1, "dispatch does not add another listener");

assert.throws(
  () =>
    root.dispatchEvent({
      type: "click",
      target: button,
      composedPath: () => [button, root],
      preventDefault() {},
    }),
  /unknown property 999/,
);
assert.equal(button.textContent, "1", "a malformed later operation prevents the earlier text patch");
assert.equal(app.disposed, true);
assert.equal(root.listeners.get("click").size, 0);
assert.equal(runtime.disposals, 1);

const invalidCustomRuntime = fakeRuntime([
  initialFrame,
  frameWithTextAndCustomProperty(1n, "unsafe", "url(https://evil.test/x)"),
], 2);
const invalidCustomRoot = new FakeElement("root");
const invalidCustomApp = await mountOptimized(new Uint8Array(), invalidCustomRoot, {
  document,
  manifest,
  startFrame,
  instantiate: async () => invalidCustomRuntime,
});
const invalidCustomButton = invalidCustomRoot.childNodes[0];
assert.throws(
  () => invalidCustomRoot.dispatchEvent({
    type: "click",
    target: invalidCustomButton,
    composedPath: () => [invalidCustomButton, invalidCustomRoot],
    preventDefault() {},
  }),
  /invalid length value/,
);
assert.equal(invalidCustomButton.textContent, "0", "invalid CSS keeps the preceding text patch inert");
assert.equal(invalidCustomButton.style.getPropertyValue("--glamour-gap"), "");
invalidCustomApp.dispose();

const extendedCssRuntime = fakeRuntime([
  initialFrame,
  frameWithTextAndCustomProperty(1n, "1", "25%", 28, 3),
  frameWithTextAndCustomProperty(2n, "2", "90deg", 30, 3),
  frameWithTextAndCustomProperty(3n, "3", "250ms", 32, 3),
], 3);
const extendedCssRoot = new FakeElement("root");
const extendedCssApp = await mountOptimized(new Uint8Array(), extendedCssRoot, {
  document,
  manifest,
  startFrame,
  instantiate: async () => extendedCssRuntime,
});
const extendedCssButton = extendedCssRoot.childNodes[0];
for (let index = 0; index < 3; index += 1) {
  extendedCssRoot.dispatchEvent({
    type: "click",
    target: extendedCssButton,
    composedPath: () => [extendedCssButton, extendedCssRoot],
    preventDefault() {},
  });
}
assert.equal(extendedCssButton.style.getPropertyValue("--glamour-position"), "25%");
assert.equal(extendedCssButton.style.getPropertyValue("--glamour-turn"), "90deg");
assert.equal(extendedCssButton.style.getPropertyValue("--glamour-delay"), "250ms");
extendedCssApp.dispose();

const invalidAngleRuntime = fakeRuntime([
  initialFrame,
  frameWithTextAndCustomProperty(1n, "unsafe", "360001deg", 30, 3),
], 3);
const invalidAngleRoot = new FakeElement("root");
const invalidAngleApp = await mountOptimized(new Uint8Array(), invalidAngleRoot, {
  document,
  manifest,
  startFrame,
  instantiate: async () => invalidAngleRuntime,
});
const invalidAngleButton = invalidAngleRoot.childNodes[0];
assert.throws(
  () => invalidAngleRoot.dispatchEvent({
    type: "click",
    target: invalidAngleButton,
    composedPath: () => [invalidAngleButton, invalidAngleRoot],
    preventDefault() {},
  }),
  /invalid angle value/,
);
assert.equal(invalidAngleButton.textContent, "0", "invalid angle keeps the frame atomic");
assert.equal(invalidAngleButton.style.getPropertyValue("--glamour-turn"), "");
assert.equal(invalidAngleApp.disposed, true);

const actionRuntime = fakeRuntime([
  initialFrame,
  frameWithText(1n, "submitting"),
  frameWithText(2n, "complete"),
], 4);
const actionRoot = new FakeElement("root");
const actionId = `glamour-form1-${"a".repeat(64)}`;
const actionManifest = {
  ...manifest,
  actions: [{
    id: actionId,
    method: "POST",
    action: "/signup",
    inputSchema: 47,
    resultSchema: 53,
    fields: [
      { name: "email", label: "Email", kind: "email", required: true },
      { name: "password", label: "Password", kind: "secret", required: true },
    ],
  }],
};
class ActionFormData {
  constructor(form) {
    this.form = form;
  }

  entries() {
    return this.form.entries[Symbol.iterator]();
  }
}
const actionApp = await mountOptimized(new Uint8Array(), actionRoot, {
  document,
  manifest: actionManifest,
  startFrame,
  FormData: ActionFormData,
  baseUrl: "https://witchy.example/",
  formFetch: async () => ({ ok: true, status: 204 }),
  instantiate: async () => actionRuntime,
});
const actionForm = new FakeElement("form");
actionForm.entries = [["email", "ada@example.test"], ["password", "s3cret"]];
actionForm.setAttribute("data-glamour-form", actionId);
actionForm.setAttribute("method", "POST");
actionForm.setAttribute("action", "/signup");
actionRoot.appendChild(actionForm);
const actionEvent = {
  target: actionForm,
  defaultPrevented: false,
  composedPath: () => [actionForm, actionRoot],
  preventDefault() {
    this.defaultPrevented = true;
  },
};
await [...actionRoot.listeners.get("submit")][0](actionEvent);
assert.equal(actionEvent.defaultPrevented, true);
assert.equal(actionRuntime.inputs.length, 2);
assert.equal(actionRuntime.inputs[0][8], FrameKind.ActionInput);
assert.equal(actionRuntime.inputs[1][8], FrameKind.ActionCompletion);
assert.equal(
  new DataView(actionRuntime.inputs[0].buffer).getUint32(GLAMOUR_HEADER_BYTES + 8, true),
  47,
);
assert.equal(
  new DataView(actionRuntime.inputs[1].buffer).getUint32(GLAMOUR_HEADER_BYTES + 8, true),
  53,
);
assert.doesNotMatch(new TextDecoder().decode(actionRuntime.inputs[0]), /s3cret/);
assert.equal(actionRoot.childNodes[0].textContent, "complete");
actionApp.dispose();

const resumedRoot = new FakeElement("root");
const resumedButton = new FakeElement("button");
resumedButton.appendChild(document.createTextNode("0"));
resumedRoot.appendChild(resumedButton);
const resumedRuntime = fakeRuntime([null, updateFrame], 2);
installDevelopmentMetadata(resumedRuntime);
const resumedManifest = {
  ...manifest,
  features: { mode: "development" },
  resume: {
    version: 1,
    sequence: 1,
    inputSequence: 0,
    nodes: [
      { id: 10, path: [0] },
      { id: 11, path: [0, 0] },
    ],
    regions: [],
    events: [{ node: 10, eventClass: 29, eventPlan: 31 }],
    subscriptions: [],
  },
};
const resumedApp = await mountOptimized(new Uint8Array(), resumedRoot, {
  document,
  manifest: resumedManifest,
  startFrame,
  resume: true,
  instantiate: async () => resumedRuntime,
});
assert.equal(resumedRuntime.resumes, 1);
assert.equal(resumedRoot.childNodes.length, 1, "resume adopts rather than remounts DOM");
assert.equal(resumedRoot.childNodes[0], resumedButton);
resumedRoot.dispatchEvent({
  type: "click",
  target: resumedButton,
  composedPath: () => [resumedButton, resumedRoot],
  preventDefault() {},
});
assert.equal(resumedButton.textContent, "1");
assert.equal(resumedButton.style.getPropertyValue("--glamour-gap"), "12px");
assert.equal(resumedRuntime.dispatches, 1);
assert.equal(resumedApp.inspectDevelopment().application.activation, "resume");
resumedApp.dispose();

const resumedLeaveBranch = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 2n,
  operations: [encodeOperation(Op.LeaveBranch, [15])],
});
const resumedEnterBranch = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 1n,
  operations: [encodeOperation(Op.EnterBranch, [15, 7, 15])],
});
const resumedBranchRoot = new FakeElement("root");
const resumedBranchParent = new FakeElement("div");
const resumedBranchButton = new FakeElement("button");
resumedBranchButton.appendChild(document.createTextNode("branch"));
const resumedBranchTail = document.createTextNode("tail");
resumedBranchParent.appendChild(resumedBranchTail);
resumedBranchRoot.appendChild(resumedBranchParent);
const resumedBranchRuntime = fakeRuntime([null, resumedEnterBranch, resumedLeaveBranch]);
const resumedBranchManifest = {
  ...manifest,
  templates: new Map([
    [
      7,
      {
        root: {
          kind: "element",
          tag: "button",
          node: 70,
          children: [{ kind: "text", node: 71, text: "branch" }],
        },
        regions: {},
        events: [],
      },
    ],
  ]),
  nodes: new Map([70, 71, 80, 81].map((id) => [id, {}])),
  regions: new Map([[15, { kind: "branch", parent: 80, nodes: [70, 71], template: 7 }]]),
  resume: {
    version: 1,
    sequence: 1,
    inputSequence: 0,
    nodes: [
      { id: 80, path: [0] },
      { id: 81, path: [0, 0] },
    ],
    regions: [{
      id: 15,
      parent: 80,
      keys: [],
      before: [81],
      child: null,
    }],
    events: [{ node: 80, eventClass: 29, eventPlan: 31 }],
    subscriptions: [],
  },
};
const resumedBranchApp = await mountOptimized(new Uint8Array(), resumedBranchRoot, {
  document,
  manifest: resumedBranchManifest,
  startFrame,
  resume: true,
  instantiate: async () => resumedBranchRuntime,
});
resumedBranchRoot.dispatchEvent({
  type: "click",
  target: resumedBranchParent,
  composedPath: () => [resumedBranchParent, resumedBranchRoot],
  preventDefault() {},
});
assert.equal(resumedBranchParent.childNodes.length, 2, "inactive branch enters on demand");
assert.equal(resumedBranchParent.childNodes[0].textContent, "branch");
assert.notEqual(resumedBranchParent.childNodes[0], resumedBranchButton);
assert.equal(resumedBranchParent.childNodes[1], resumedBranchTail);
resumedBranchRoot.dispatchEvent({
  type: "click",
  target: resumedBranchParent,
  composedPath: () => [resumedBranchParent, resumedBranchRoot],
  preventDefault() {},
});
assert.deepEqual(
  resumedBranchParent.childNodes,
  [resumedBranchTail],
  "entered branch leaves without moving its following sibling",
);
resumedBranchApp.dispose();

let malformedResumeInstantiated = false;
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("root"), {
    document,
    manifest: resumedManifest,
    startFrame,
    resume: true,
    instantiate: async () => {
      malformedResumeInstantiated = true;
      return fakeRuntime([]);
    },
  }),
  /does not match the existing DOM/,
);
assert.equal(
  malformedResumeInstantiated,
  false,
  "existing DOM identity is checked before application code is instantiated",
);

let invalidCustomRegistryInstantiated = false;
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("root"), {
    document,
    manifest: {
      ...manifest,
      customProperties: new Map([[27, { name: "color", category: "length" }]]),
    },
    startFrame,
    instantiate: async () => {
      invalidCustomRegistryInstantiated = true;
      return fakeRuntime([initialFrame]);
    },
  }),
  /invalid identity, name, or category/,
);
assert.equal(invalidCustomRegistryInstantiated, false);

let crossedEventRegistryInstantiated = false;
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("root"), {
    document,
    manifest: { ...manifest, registryId: 2 },
    startFrame,
    instantiate: async () => {
      crossedEventRegistryInstantiated = true;
      return fakeRuntime([initialFrame]);
    },
  }),
  /event plan does not belong to the manifest registry/,
);
assert.equal(
  crossedEventRegistryInstantiated,
  false,
  "cross-registry event plans are rejected before application code is instantiated",
);

const unsafeRuntime = fakeRuntime([initialFrame]);
const unsafeManifest = {
  ...manifest,
  templates: new Map([
    [3, { root: { kind: "element", tag: "script", node: 10, children: [] } }],
  ]),
};
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("root"), {
    document,
    manifest: unsafeManifest,
    startFrame,
    instantiate: async () => unsafeRuntime,
  }),
  /invalid element name/,
);
assert.equal(unsafeRuntime.disposals, 1);

const keyedMount = encodeOutputFrame({
  kind: FrameKind.Mount,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 0n,
  operations: [
    encodeOperation(Op.Mount, [3, 1, 0, 0, 0]),
    encodeOperation(Op.ListInsert, [13, 1, 0, 4, 101, 0]),
    encodeOperation(Op.ListInsert, [13, 2, 0, 5, 102, 0]),
    encodeOperation(Op.ListInsert, [13, 3, 0, 6, 103, 0]),
    encodeOperation(Op.SetEventPlan, [20, 29, 31]),
  ],
});
const keyedMove = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 1n,
  operations: [
    encodeOperation(Op.ListMove, [13, 3, 1]),
    encodeOperation(Op.ListMove, [14, 12, 11]),
  ],
});
const keyedRemove = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 2n,
  operations: [encodeOperation(Op.ListRemove, [13, 1])],
});
const keyedInsertionSlots = [
  [7, encoder.encode("again")],
  [8, encoder.encode("hot")],
  [9, encoder.encode("prop")],
  [10, encoder.encode("title")],
  [11, encoder.encode("label")],
  [12, encoder.encode("1")],
  [13, encoder.encode("18px")],
];
const keyedInsertionPayloadOffset = GLAMOUR_HEADER_BYTES + 32 + keyedInsertionSlots.length * 12;
let keyedInsertionCursor = keyedInsertionPayloadOffset;
const keyedInsertionSlotWords = keyedInsertionSlots.flatMap(([slot, value]) => {
  const words = [slot, keyedInsertionCursor, value.byteLength];
  keyedInsertionCursor += value.byteLength;
  return words;
});
const keyedReinsert = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 3n,
  minor: 1,
  operations: [
    encodeOperation(Op.ListInsert, [
      13,
      1,
      2,
      4,
      104,
      keyedInsertionSlots.length,
      ...keyedInsertionSlotWords,
    ]),
  ],
  payloads: keyedInsertionSlots.map(([, value]) => value),
});
const keyedRuntime = fakeRuntime([keyedMount, keyedMove, keyedRemove, keyedReinsert], 1);
const keyedManifest = {
  ...manifest,
  templates: new Map([
    [
      3,
      {
        root: { kind: "element", tag: "ul", node: 20, children: [] },
        regions: { 13: 20 },
      },
    ],
    [
      4,
      {
        root: {
          kind: "element",
          tag: "li",
          node: 41,
          children: [{
            kind: "element",
            tag: "ul",
            node: 42,
            children: [
              { kind: "element", tag: "li", node: 43, children: [{ kind: "text", node: 44, text: "1" }] },
              { kind: "element", tag: "li", node: 45, children: [{ kind: "text", node: 46, text: "2" }] },
            ],
          }],
        },
        regions: {
          14: {
            parent: 42,
            kind: "list",
            keys: [
              { key: 11, root: 43, nodes: [43, 44] },
              { key: 12, root: 45, nodes: [45, 46] },
            ],
          },
        },
        slots: new Map([
          [7, { kind: "text", node: 44 }],
          [8, { kind: "class", node: 41 }],
          [9, { kind: "property", node: 41, sink: 17 }],
          [10, { kind: "attribute", node: 41, sink: 19 }],
          [11, { kind: "aria", node: 41, sink: 23 }],
          [12, { kind: "boolean", node: 41, sink: 20 }],
          [13, { kind: "custom-property", node: 41, sink: 27 }],
        ]),
        events: [{ node: 43, eventClass: 29, eventPlan: 31 }],
      },
    ],
    [
      5,
      {
        root: {
          kind: "element",
          tag: "li",
          node: 51,
          children: [{ kind: "text", node: 52, text: "2" }],
        },
      },
    ],
    [
      6,
      {
        root: {
          kind: "element",
          tag: "li",
          node: 61,
          children: [{ kind: "text", node: 62, text: "3" }],
        },
      },
    ],
  ]),
  nodes: new Map([20, 41, 42, 43, 44, 45, 46, 51, 52, 61, 62].map((id) => [id, {}])),
  regions: new Map([[13, { template: 3, kind: "list" }], [14, { template: 4, kind: "list" }]]),
  attributes: new Map([[19, "title"], [20, "hidden"]]),
  aria: new Map([[23, "aria-label"]]),
};
const keyedRoot = new FakeElement("root");
const keyedApp = await mountOptimized(new Uint8Array(), keyedRoot, {
  document,
  manifest: keyedManifest,
  startFrame,
  instantiate: async () => keyedRuntime,
});
const list = keyedRoot.childNodes[0];
const [one, two, three] = list.childNodes;
const nestedList = one.childNodes[0];
const nestedOne = nestedList.childNodes[0];
one.scrollTop = 17;
two.selectionStart = 1;
three.__imeComposition = "active";
assert.deepEqual(list.childNodes.map((item) => item.textContent), ["12", "2", "3"]);
keyedRoot.dispatchEvent({
  type: "click",
  target: nestedOne,
  composedPath: () => [nestedOne, nestedList, one, list, keyedRoot],
  preventDefault() {},
});
assert.deepEqual(list.childNodes.map((item) => item.textContent), ["3", "21", "2"]);
assert.equal(list.childNodes[0], three);
assert.equal(list.childNodes[1], one);
assert.equal(list.childNodes[2], two);
assert.equal(one.scrollTop, 17);
assert.equal(two.selectionStart, 1);
assert.equal(three.__imeComposition, "active");
assert.equal(keyedMove[16], 2, "the reorder frame contains one move per keyed region");
keyedRoot.dispatchEvent({
  type: "click",
  target: list,
  composedPath: () => [list, keyedRoot],
  preventDefault() {},
});
assert.deepEqual(list.childNodes.map((item) => item.textContent), ["3", "2"]);
assert.equal(one.parentNode, null);
keyedRoot.dispatchEvent({
  type: "click",
  target: list,
  composedPath: () => [list, keyedRoot],
  preventDefault() {},
});
assert.deepEqual(list.childNodes.map((item) => item.textContent), ["3", "again2", "2"]);
const reinserted = list.childNodes[1];
assert.equal(reinserted.attributes.get("class"), "hot");
assert.equal(reinserted.value, "prop");
assert.equal(reinserted.attributes.get("title"), "title");
assert.equal(reinserted.attributes.get("aria-label"), "label");
assert.equal(reinserted.attributes.get("hidden"), "");
assert.equal(reinserted.style.getPropertyValue("--glamour-gap"), "18px");
keyedApp.dispose();

const structuralMount = encodeOutputFrame({
  kind: FrameKind.Mount,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 0n,
  operations: [
    encodeOperation(Op.Mount, [9, 201, 0, 0, 0]),
  ],
});
const branchSlotValue = encoder.encode("branch live");
const enterBranch = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 1n,
  minor: 1,
  operations: [
    encodeOperation(Op.EnterBranch, [
      15,
      7,
      202,
      1,
      8,
      GLAMOUR_HEADER_BYTES + 36,
      branchSlotValue.byteLength,
    ]),
  ],
  payloads: [branchSlotValue],
});
const childSlotValue = encoder.encode("child live");
const mountChild = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 2n,
  minor: 1,
  operations: [
    encodeOperation(Op.MountChild, [
      16,
      8,
      203,
      1,
      9,
      GLAMOUR_HEADER_BYTES + 36,
      childSlotValue.byteLength,
    ]),
  ],
  payloads: [childSlotValue],
});
const unmountChild = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 3n,
  operations: [encodeOperation(Op.UnmountChild, [16])],
});
const leaveBranch = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 4n,
  operations: [encodeOperation(Op.LeaveBranch, [15])],
});
const structuralRuntime = fakeRuntime([
  structuralMount,
  enterBranch,
  mountChild,
  unmountChild,
  leaveBranch,
], 1);
const structuralManifest = {
  ...manifest,
  templates: new Map([
    [
      9,
      {
        root: {
          kind: "element",
          tag: "div",
          node: 80,
          children: [{ kind: "text", node: 81, text: "tail" }],
        },
        regions: {
          15: { parent: 80, kind: "branch", keys: [], before: [81], template: 7 },
          16: { parent: 80, kind: "child", keys: [], before: [81], template: 8 },
        },
        events: [{ node: 80, eventClass: 29, eventPlan: 31 }],
      },
    ],
    [
      7,
      {
        root: { kind: "element", tag: "button", node: 70, children: [{ kind: "text", node: 71, text: "branch" }] },
        slots: new Map([[8, { kind: "text", node: 71 }]]),
        events: [{ node: 70, eventClass: 29, eventPlan: 31 }],
      },
    ],
    [
      8,
      {
        root: { kind: "element", tag: "span", node: 72, children: [{ kind: "text", node: 73, text: "child" }] },
        slots: new Map([[9, { kind: "text", node: 73 }]]),
      },
    ],
  ]),
  nodes: new Map([70, 71, 72, 73, 80, 81].map((id) => [id, {}])),
  regions: new Map([[15, { kind: "branch", template: 7 }], [16, { kind: "child", template: 8 }]]),
};
const structuralRoot = new FakeElement("root");
const structuralFallback = document.createElement("p");
structuralFallback.appendChild(document.createTextNode("structural fallback"));
structuralRoot.appendChild(structuralFallback);
const structuralApp = await mountOptimized(new Uint8Array(), structuralRoot, {
  document,
  manifest: structuralManifest,
  startFrame,
  replaceRoot: true,
  instantiate: async () => structuralRuntime,
});
assert.equal(structuralFallback.parentNode, null, "fresh structural mount replaces fallback once");
const structuralParent = structuralRoot.childNodes[0];
const tailNode = structuralParent.childNodes[0];
assert.equal(tailNode.textContent, "tail");
structuralRoot.dispatchEvent({
  type: "click",
  target: structuralParent,
  composedPath: () => [structuralParent, structuralRoot],
  preventDefault() {},
});
const branchNode = structuralParent.childNodes[0];
assert.equal(branchNode.textContent, "branch live");
assert.deepEqual(structuralParent.childNodes, [branchNode, tailNode]);
structuralRoot.dispatchEvent({
  type: "click",
  target: branchNode,
  composedPath: () => [branchNode, structuralRoot],
  preventDefault() {},
});
const childNode = structuralParent.childNodes[1];
assert.equal(childNode.textContent, "child live");
assert.deepEqual(structuralParent.childNodes, [branchNode, childNode, tailNode]);
structuralRoot.dispatchEvent({
  type: "click",
  target: branchNode,
  composedPath: () => [branchNode, structuralParent, structuralRoot],
  preventDefault() {},
});
assert.deepEqual(structuralParent.childNodes, [branchNode, tailNode]);
structuralRoot.dispatchEvent({
  type: "click",
  target: branchNode,
  composedPath: () => [branchNode, structuralParent, structuralRoot],
  preventDefault() {},
});
assert.equal(structuralParent.childNodes.length, 1);
assert.equal(structuralParent.childNodes[0], tailNode);
const dispatchesAfterLeave = structuralRuntime.dispatches;
structuralRoot.dispatchEvent({
  type: "click",
  target: branchNode,
  composedPath: () => [branchNode, structuralRoot],
  preventDefault() {},
});
assert.equal(
  structuralRuntime.dispatches,
  dispatchesAfterLeave,
  "leaving a fresh structural region removes its dormant template event",
);
structuralApp.dispose();

const invalidOrderRuntime = fakeRuntime([structuralMount]);
const invalidOrderManifest = {
  ...structuralManifest,
  templates: new Map([
    ...structuralManifest.templates,
    [
      9,
      {
        root: {
          kind: "element",
          tag: "div",
          node: 80,
          children: [{ kind: "text", node: 81, text: "tail" }],
        },
        regions: {
          15: { parent: 80, kind: "branch", keys: [], before: [70], template: 7 },
          16: { parent: 80, kind: "child", keys: [], before: [81], template: 8 },
        },
      },
    ],
  ]),
};
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("root"), {
    document,
    manifest: invalidOrderManifest,
    startFrame,
    instantiate: async () => invalidOrderRuntime,
  }),
  /invalid insertion order/,
);
assert.equal(invalidOrderRuntime.disposals, 1);

const wrongStructuralTemplate = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 1n,
  operations: [encodeOperation(Op.EnterBranch, [15, 8, 204])],
});
const wrongTemplateRuntime = fakeRuntime([structuralMount, wrongStructuralTemplate]);
const wrongTemplateRoot = new FakeElement("root");
const wrongTemplateApp = await mountOptimized(new Uint8Array(), wrongTemplateRoot, {
  document,
  manifest: structuralManifest,
  startFrame,
  instantiate: async () => wrongTemplateRuntime,
});
const wrongTemplateParent = wrongTemplateRoot.childNodes[0];
assert.throws(
  () => wrongTemplateRoot.dispatchEvent({
    type: "click",
    target: wrongTemplateParent,
    composedPath: () => [wrongTemplateParent, wrongTemplateRoot],
    preventDefault() {},
  }),
  /incompatible region or template/,
);
assert.equal(wrongTemplateParent.textContent, "tail");
wrongTemplateApp.dispose();

const missingStructuralSlot = encodeOutputFrame({
  kind: FrameKind.Patch,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 1n,
  minor: 1,
  operations: [encodeOperation(Op.EnterBranch, [15, 7, 205, 0])],
});
const missingSlotRuntime = fakeRuntime([structuralMount, missingStructuralSlot], 1);
const missingSlotRoot = new FakeElement("root");
const missingSlotApp = await mountOptimized(new Uint8Array(), missingSlotRoot, {
  document,
  manifest: structuralManifest,
  startFrame,
  instantiate: async () => missingSlotRuntime,
});
const missingSlotParent = missingSlotRoot.childNodes[0];
assert.throws(
  () => missingSlotRoot.dispatchEvent({
    type: "click",
    target: missingSlotParent,
    composedPath: () => [missingSlotParent, missingSlotRoot],
    preventDefault() {},
  }),
  /exactly cover the authenticated slot table/,
);
assert.equal(missingSlotParent.textContent, "tail");
missingSlotApp.dispose();

const oldProtocolRuntime = fakeRuntime([structuralMount, enterBranch]);
const oldProtocolRoot = new FakeElement("root");
const oldProtocolApp = await mountOptimized(new Uint8Array(), oldProtocolRoot, {
  document,
  manifest: structuralManifest,
  startFrame,
  instantiate: async () => oldProtocolRuntime,
});
const oldProtocolParent = oldProtocolRoot.childNodes[0];
assert.throws(
  () => oldProtocolRoot.dispatchEvent({
    type: "click",
    target: oldProtocolParent,
    composedPath: () => [oldProtocolParent, oldProtocolRoot],
    preventDefault() {},
  }),
  /newer than its declared protocol version/,
);
assert.equal(oldProtocolParent.textContent, "tail");
oldProtocolApp.dispose();

function dynamicInsertFrame(sequence, key, beforeKey, value) {
  const keyBytes = encoder.encode(key);
  const beforeBytes = encoder.encode(beforeKey);
  const valueBytes = encoder.encode(value);
  const payloadOffset = GLAMOUR_HEADER_BYTES + 48;
  return encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: APP_ID,
    buildId: BUILD_ID,
    sequence,
    minor: 3,
    operations: [encodeOperation(Op.ListInsertDynamic, [
      33,
      payloadOffset,
      keyBytes.byteLength,
      payloadOffset + keyBytes.byteLength,
      beforeBytes.byteLength,
      31,
      1,
      41,
      payloadOffset + keyBytes.byteLength + beforeBytes.byteLength,
      valueBytes.byteLength,
    ])],
    payloads: [keyBytes, beforeBytes, valueBytes],
  });
}

function dynamicKeyFrame(sequence, tag, key, beforeKey = "") {
  const keyBytes = encoder.encode(key);
  const beforeBytes = encoder.encode(beforeKey);
  const operationBytes = tag === Op.ListMoveDynamic ? 28 : 20;
  const payloadOffset = GLAMOUR_HEADER_BYTES + operationBytes;
  const words = [33, payloadOffset, keyBytes.byteLength];
  if (tag === Op.ListMoveDynamic) {
    words.push(payloadOffset + keyBytes.byteLength, beforeBytes.byteLength);
  }
  return encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: APP_ID,
    buildId: BUILD_ID,
    sequence,
    minor: 3,
    operations: [encodeOperation(tag, words)],
    payloads: [keyBytes, beforeBytes],
  });
}

function dynamicUpdateFrame(sequence, updates) {
  const operationBytes = updates.length * 36;
  let payloadCursor = GLAMOUR_HEADER_BYTES + operationBytes;
  const payloads = [];
  const operations = updates.map(([key, value]) => {
    const keyBytes = encoder.encode(key);
    const valueBytes = encoder.encode(value);
    const keyOffset = payloadCursor;
    payloadCursor += keyBytes.byteLength;
    const valueOffset = payloadCursor;
    payloadCursor += valueBytes.byteLength;
    payloads.push(keyBytes, valueBytes);
    return encodeOperation(Op.UpdateDynamicSlots, [
      33,
      keyOffset,
      keyBytes.byteLength,
      1,
      41,
      valueOffset,
      valueBytes.byteLength,
    ]);
  });
  return encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: APP_ID,
    buildId: BUILD_ID,
    sequence,
    minor: 3,
    operations,
    payloads,
  });
}

const dynamicMount = encodeOutputFrame({
  kind: FrameKind.Mount,
  appId: APP_ID,
  buildId: BUILD_ID,
  sequence: 0n,
  operations: [
    encodeOperation(Op.Mount, [30, 301, 0, 0, 0]),
    encodeOperation(Op.SetEventPlan, [300, 29, 31]),
  ],
});
const dynamicManifest = {
  ...manifest,
  templates: new Map([
    [
      30,
      {
        root: { kind: "element", tag: "ul", node: 300, children: [] },
        regions: {
          33: { parent: 300, kind: "list", keys: [], dynamicTemplate: 31 },
        },
      },
    ],
    [
      31,
      {
        root: {
          kind: "element",
          tag: "li",
          node: 310,
          children: [{ kind: "text", node: 311, text: "" }],
        },
        slots: new Map([[41, { kind: "text", node: 311 }]]),
        regions: {},
        events: [],
      },
    ],
  ]),
  nodes: new Map([[300, { template: 30 }]]),
  regions: new Map([[33, { kind: "list", parent: 300, dynamicTemplate: 31 }]]),
};
const duplicateDynamicUpdate = dynamicUpdateFrame(6n, [["alpha", "A3"], ["alpha", "A4"]]);
const dynamicRuntime = fakeRuntime([
  dynamicMount,
  dynamicInsertFrame(1n, "alpha", "", "A"),
  dynamicInsertFrame(2n, "beta", "alpha", "B"),
  dynamicKeyFrame(3n, Op.ListMoveDynamic, "alpha", "beta"),
  dynamicUpdateFrame(4n, [["alpha", "A2"]]),
  dynamicKeyFrame(5n, Op.ListRemoveDynamic, "beta"),
  duplicateDynamicUpdate,
], 3);
const dynamicRoot = new FakeElement("root");
const dynamicApp = await mountOptimized(new Uint8Array(), dynamicRoot, {
  document,
  manifest: dynamicManifest,
  startFrame,
  instantiate: async () => dynamicRuntime,
});
const dynamicList = dynamicRoot.childNodes[0];
const dispatchDynamic = () => dynamicRoot.dispatchEvent({
  type: "click",
  target: dynamicList,
  composedPath: () => [dynamicList, dynamicRoot],
  preventDefault() {},
});
dispatchDynamic();
const alpha = dynamicList.childNodes[0];
assert.equal(alpha.textContent, "A");
dispatchDynamic();
const beta = dynamicList.childNodes[0];
assert.deepEqual(dynamicList.childNodes.map((item) => item.textContent), ["B", "A"]);
assert.notEqual(alpha, beta, "dynamic entries clone one authenticated template independently");
dispatchDynamic();
assert.deepEqual(dynamicList.childNodes, [alpha, beta]);
dispatchDynamic();
assert.equal(alpha.textContent, "A2");
dispatchDynamic();
assert.deepEqual(dynamicList.childNodes, [alpha]);
assert.throws(dispatchDynamic, /incompatible or repeated dynamic entry/);
assert.equal(alpha.textContent, "A2", "a rejected frame leaves an earlier slot update inert");
assert.equal(dynamicApp.disposed, true);

const resumedDynamicRoot = new FakeElement("root");
const resumedDynamicList = new FakeElement("ul");
const resumedDynamicItem = new FakeElement("li");
resumedDynamicItem.appendChild(document.createTextNode("server"));
resumedDynamicList.appendChild(resumedDynamicItem);
resumedDynamicRoot.appendChild(resumedDynamicList);
const resumedDynamicRuntime = fakeRuntime([
  null,
  dynamicUpdateFrame(1n, [["alpha", "client"]]),
], 3);
const resumedDynamicManifest = {
  ...dynamicManifest,
  nodes: new Map([[300, {}], [320, {}], [321, {}]]),
  resume: {
    version: 1,
    sequence: 1,
    inputSequence: 0,
    nodes: [
      { id: 300, path: [0] },
      { id: 320, path: [0, 0] },
      { id: 321, path: [0, 0, 0] },
    ],
    regions: [{
      id: 33,
      parent: 300,
      keys: [{ key: 1, source: "alpha", root: 320, nodes: [320, 321] }],
      before: [],
      child: null,
    }],
    events: [{ node: 300, eventClass: 29, eventPlan: 31 }],
    subscriptions: [],
  },
};
const resumedDynamicApp = await mountOptimized(new Uint8Array(), resumedDynamicRoot, {
  document,
  manifest: resumedDynamicManifest,
  startFrame,
  resume: true,
  instantiate: async () => resumedDynamicRuntime,
});
resumedDynamicRoot.dispatchEvent({
  type: "click",
  target: resumedDynamicList,
  composedPath: () => [resumedDynamicList, resumedDynamicRoot],
  preventDefault() {},
});
assert.equal(resumedDynamicItem.textContent, "client");
assert.equal(resumedDynamicRoot.childNodes[0], resumedDynamicList);
resumedDynamicApp.dispose();

const wrongDynamicShapeRoot = new FakeElement("root");
const wrongDynamicShapeList = new FakeElement("ul");
const wrongDynamicShapeItem = new FakeElement("span");
wrongDynamicShapeItem.appendChild(document.createTextNode("server"));
wrongDynamicShapeList.appendChild(wrongDynamicShapeItem);
wrongDynamicShapeRoot.appendChild(wrongDynamicShapeList);
let wrongDynamicShapeInstantiated = false;
await assert.rejects(
  mountOptimized(new Uint8Array(), wrongDynamicShapeRoot, {
    document,
    manifest: resumedDynamicManifest,
    startFrame,
    resume: true,
    instantiate: async () => {
      wrongDynamicShapeInstantiated = true;
      return fakeRuntime([]);
    },
  }),
  /does not match its authenticated template/,
);
assert.equal(wrongDynamicShapeInstantiated, false);

const developmentRuntime = fakeRuntime([
  initialFrame,
  ...Array.from({ length: 130 }, (_, index) =>
    frameWithText(BigInt(index + 1), String(index + 1)),
  ),
], 2);
const developmentExports = developmentRuntime.instance.exports;
const {
  memory: developmentMemory,
  pointer: metadataPointer,
} = installDevelopmentMetadata(developmentRuntime, [3], 2, 1, ["active"]);
const developmentSnapshot = new Uint8Array(48);
developmentSnapshot.set(encoder.encode("WGST"), 0);
const snapshotView = new DataView(developmentSnapshot.buffer);
snapshotView.setUint16(4, 1, true);
snapshotView.setUint16(6, 1, true);
developmentSnapshot.fill(0x11, 8, 40);
snapshotView.setUint32(40, 1, true);
const snapshotPointer = 512;
developmentMemory.set(developmentSnapshot, snapshotPointer);
developmentExports.__glamour_dev_snapshot = () => snapshotPointer;
developmentExports.__glamour_dev_snapshot_length = () => developmentSnapshot.byteLength;
developmentExports.__glamour_dev_restore = () => {
  const restored = developmentMemory.slice(1024, 1024 + developmentSnapshot.byteLength);
  assert.deepEqual(restored, developmentSnapshot);
  return developmentRuntime.stage(initialFrame);
};
const detachedRoot = new FakeElement("detached");
const developmentManifest = {
  ...manifest,
  buildIdentity: "build-private",
  features: { mode: "development" },
  effectDescriptors: new Map([
    [37, {
      handler: "request",
      resultSchema: 43,
      ownerScope: 49,
      semantic: "resource",
      privateData: "omitted",
    }],
  ]),
  subscriptionDescriptors: new Map([
    [41, {
      handler: "interval",
      resultSchema: 47,
      ownerScope: 51,
      semantic: "not-an-authenticated-semantic",
      privateData: "omitted",
    }],
  ]),
};
const freshDevelopmentRuntime = fakeRuntime([initialFrame], 2);
installDevelopmentMetadata(freshDevelopmentRuntime);
const freshDevelopment = await mountOptimized(
  new Uint8Array(),
  new FakeElement("fresh"),
  {
    document,
    manifest: developmentManifest,
    startFrame,
    instantiate: async () => freshDevelopmentRuntime,
  },
);
assert.equal(freshDevelopment.inspectDevelopment().application.activation, "fresh");
freshDevelopment.dispose();
const aggregateDevelopmentRuntime = fakeRuntime([initialFrame], 2);
installDevelopmentMetadata(aggregateDevelopmentRuntime, [4], 2, 0);
const aggregateDevelopment = await mountOptimized(
  new Uint8Array(),
  new FakeElement("aggregate"),
  {
    document,
    manifest: developmentManifest,
    startFrame,
    instantiate: async () => aggregateDevelopmentRuntime,
  },
);
assert.deepEqual(aggregateDevelopment.inspectDevelopment().model.fields, [
  { index: 0, kind: "Aggregate", value: "<redacted>" },
]);
assert.equal(aggregateDevelopment.developmentMetadata.snapshotFormat, 0);
assert.throws(() => aggregateDevelopment.snapshot(), /development snapshot is unavailable/);
aggregateDevelopment.dispose();
const aggregateSnapshotRuntime = fakeRuntime([initialFrame], 2);
const { memory: aggregateSnapshotMemory } = installDevelopmentMetadata(
  aggregateSnapshotRuntime,
  [4],
  2,
  2,
);
const aggregateSnapshot = new Uint8Array(46);
aggregateSnapshot.set(encoder.encode("WGST"), 0);
const aggregateSnapshotView = new DataView(aggregateSnapshot.buffer);
aggregateSnapshotView.setUint16(4, 2, true);
aggregateSnapshotView.setUint16(6, 1, true);
aggregateSnapshot.fill(0x11, 8, 40);
aggregateSnapshotView.setUint32(40, 2, true);
aggregateSnapshot.set(encoder.encode("{}"), 44);
const aggregateSnapshotPointer = 640;
aggregateSnapshotMemory.set(aggregateSnapshot, aggregateSnapshotPointer);
aggregateSnapshotRuntime.instance.exports.__glamour_dev_snapshot = () =>
  aggregateSnapshotPointer;
aggregateSnapshotRuntime.instance.exports.__glamour_dev_snapshot_length = () =>
  aggregateSnapshot.byteLength;
const aggregateSnapshotApplication = await mountOptimized(
  new Uint8Array(),
  new FakeElement("aggregate-snapshot"),
  {
    document,
    manifest: {
      ...developmentManifest,
      development: { maxSnapshotBytes: 1024 * 1024 },
    },
    startFrame,
    instantiate: async () => aggregateSnapshotRuntime,
  },
);
assert.deepEqual(aggregateSnapshotApplication.snapshot(), aggregateSnapshot);
aggregateSnapshotApplication.dispose();
const invalidAggregateSnapshotRuntime = fakeRuntime([initialFrame], 2);
installDevelopmentMetadata(invalidAggregateSnapshotRuntime, [4], 2, 1);
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("invalid-aggregate"), {
    document,
    manifest: developmentManifest,
    startFrame,
    instantiate: async () => invalidAggregateSnapshotRuntime,
  }),
  /snapshot metadata contains an aggregate field/,
);
const candidate = await mountOptimized(new Uint8Array(), detachedRoot, {
  document,
  manifest: developmentManifest,
  restoreSnapshot: developmentSnapshot,
  deferActivation: true,
  instantiate: async () => developmentRuntime,
});
assert.equal(candidate.developmentMetadata.modelSchema, "11".repeat(32));
assert.equal(candidate.developmentMetadata.authorizationSchema, "22".repeat(32));
assert.deepEqual(candidate.snapshot(), developmentSnapshot);
const inspection = candidate.inspectDevelopment();
assert.equal(inspection.schema, "witchy.glamour.devtools.v1");
assert.equal(inspection.buildIdentity, "build-private");
assert.equal(inspection.model.schema, "11".repeat(32));
assert.deepEqual(inspection.model.fields, [
  { index: 0, name: "active", kind: "Bool", value: "<redacted>" },
]);
assert.deepEqual(inspection.application, {
  kind: "application",
  activation: "hot-swap",
  liveNodes: 2,
  liveRegions: 0,
  rootListeners: 1,
  islands: [],
});
assert.ok(Object.isFrozen(inspection.application));
assert.ok(Object.isFrozen(inspection.application.islands));
assert.deepEqual(inspection.descriptors.effects, [
  { id: 37, handler: "request", resultSchema: 43, ownerScope: 49, semantic: "resource" },
]);
assert.deepEqual(inspection.descriptors.subscriptions, [
  { id: 41, handler: "interval", resultSchema: 47, ownerScope: 51 },
]);
assert.ok(!JSON.stringify(inspection).includes("privateData"));
assert.equal(inspection.timeline.length, 1);
assert.equal(inspection.timeline[0].sequence, "0");
assert.deepEqual(inspection.timeline[0].modelChanges, [0]);
assert.ok(Object.isFrozen(inspection.timeline[0].modelChanges));
assert.ok(inspection.timeline[0].timing.totalMs >= 0);
assert.ok(
  inspection.timeline[0].operations.every(
    (operation) => !("value" in operation) && !("request" in operation) && !("key" in operation),
  ),
);
assert.ok(Object.isFrozen(inspection));
assert.ok(Object.isFrozen(inspection.timeline));
const activatedRoot = new FakeElement("root");
candidate.activate(activatedRoot);
assert.equal(detachedRoot.childNodes.length, 0);
assert.equal(activatedRoot.childNodes[0].textContent, "0");
assert.equal(detachedRoot.listeners.get("click").size, 0);
assert.equal(activatedRoot.listeners.get("click").size, 1);
for (let index = 0; index < 130; index += 1) {
  candidate.dispatch({
    plan: 31,
    node: 10,
    name: "click",
    value: "",
    checked: false,
    key: "",
    composing: false,
    userActivation: true,
  });
}
const cappedInspection = candidate.inspectDevelopment();
assert.equal(cappedInspection.timeline.length, 128);
assert.equal(cappedInspection.timeline[0].sequence, "3");
assert.equal(cappedInspection.timeline.at(-1).sequence, "130");
assert.equal(activatedRoot.childNodes[0].textContent, "130");
assert.deepEqual(candidate.inspectDevelopment().application, {
  kind: "application",
  activation: "hot-swap",
  liveNodes: 2,
  liveRegions: 0,
  rootListeners: 1,
  islands: [],
});
candidate.dispose();

console.log("GLAMOUR-OPTIMIZED OK");
