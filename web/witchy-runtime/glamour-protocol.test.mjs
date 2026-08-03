#!/usr/bin/env node

import assert from "node:assert/strict";
import { encodeCompletionResult } from "./glamour-completion-codecs.mjs";
import {
  ActionCompletionStatus,
  ActionFieldKind,
  CompletionSource,
  CompletionStatus,
  EffectOp,
  FrameKind,
  GLAMOUR_HEADER_BYTES,
  Op,
  createOutputValidator,
  decodeOutputFrame,
  encodeActivationFrame,
  encodeActionCompletionFrame,
  encodeActionInputFrame,
  encodeEffectCompletionFrame,
  encodeOperation,
  encodeOutputFrame,
} from "./glamour-protocol.mjs";

const encoder = new TextEncoder();
const manifest = Object.freeze({
  appId: 7,
  buildId: 0x0102_0304_0506_0708n,
  templates: [3],
  nodes: [11],
  regions: [13],
  properties: [17],
  attributes: [19],
  aria: [23],
  customProperties: [27],
  eventClasses: [29],
  eventPlans: [31],
  effectDescriptors: [37],
  subscriptionDescriptors: [41],
  limits: {
    maxFrameBytes: 4096,
    maxOperations: 16,
    maxPayloadBytes: 1024,
    maxStringBytes: 128,
    maxSlots: 8,
  },
});

const activation = encodeActivationFrame({
  appId: manifest.appId,
  buildId: manifest.buildId,
  sequence: 9n,
});
const activationView = new DataView(activation.buffer);
assert.equal(activation.byteLength, GLAMOUR_HEADER_BYTES);
assert.equal(activationView.getUint8(8), FrameKind.Activation);
assert.equal(activationView.getUint32(16, true), 0);
assert.equal(activationView.getBigUint64(32, true), 9n);

function patchWithText(value, sequence = 0n) {
  const payload = encoder.encode(value);
  const payloadOffset = GLAMOUR_HEADER_BYTES + 20;
  return encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: manifest.appId,
    buildId: manifest.buildId,
    sequence,
    operations: [encodeOperation(Op.SetText, [11, payloadOffset, payload.byteLength])],
    payloads: [payload],
  });
}

const frame = decodeOutputFrame(patchWithText("hello"), manifest, 0n);
assert.equal(frame.kind, FrameKind.Patch);
assert.equal(frame.operations.length, 1);
assert.deepEqual(
  {
    kind: frame.operations[0].kind,
    node: frame.operations[0].node,
    value: frame.operations[0].value,
  },
  { kind: "set_text", node: 11, value: "hello" },
);
assert.ok(Object.isFrozen(frame));
assert.ok(Object.isFrozen(frame.operations));
assert.ok(Object.isFrozen(frame.operations[0]));

const customPropertyValue = encoder.encode("12px");
const customPropertyOffset = GLAMOUR_HEADER_BYTES + 24;
const customPropertyOperation = encodeOperation(Op.SetCustomProperty, [
  11,
  27,
  customPropertyOffset,
  customPropertyValue.byteLength,
]);
const customPropertyFrame = decodeOutputFrame(
  encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: manifest.appId,
    buildId: manifest.buildId,
    minor: 2,
    operations: [customPropertyOperation],
    payloads: [customPropertyValue],
  }),
  manifest,
  0n,
);
assert.deepEqual(
  {
    kind: customPropertyFrame.operations[0].kind,
    node: customPropertyFrame.operations[0].node,
    sink: customPropertyFrame.operations[0].sink,
    value: customPropertyFrame.operations[0].value,
  },
  { kind: "set_custom_property", node: 11, sink: 27, value: "12px" },
);
assert.throws(
  () => decodeOutputFrame(
    encodeOutputFrame({
      kind: FrameKind.Patch,
      appId: manifest.appId,
      buildId: manifest.buildId,
      minor: 1,
      operations: [customPropertyOperation],
      payloads: [customPropertyValue],
    }),
    manifest,
    0n,
  ),
  /requires protocol minor 2/,
);

const effectPayload = encoder.encode("request");
const effectPayloadOffset = GLAMOUR_HEADER_BYTES + 28 + 24;
const effects = encodeOutputFrame({
  kind: FrameKind.Effects,
  appId: manifest.appId,
  buildId: manifest.buildId,
  sequence: 0n,
  operations: [
    encodeOperation(EffectOp.Start, [
      101,
      77,
      37,
      effectPayloadOffset,
      effectPayload.byteLength,
    ]),
    encodeOperation(EffectOp.SyncSubscription, [
      201,
      41,
      effectPayloadOffset,
      effectPayload.byteLength,
    ]),
  ],
  payloads: [effectPayload],
});
const effectFrame = decodeOutputFrame(effects, manifest, 0n);
assert.deepEqual(
  effectFrame.operations.map((operation) => operation.kind),
  ["start_effect", "sync_subscription"],
);
assert.equal(effectFrame.operations[0].request, "request");
assert.equal(effectFrame.operations[1].request, "request");

const insertionValue = encoder.encode("inserted");
const insertionPayloadOffset = GLAMOUR_HEADER_BYTES + 44;
const insertionOperation = encodeOperation(Op.ListInsert, [
  13,
  1,
  0,
  3,
  101,
  1,
  7,
  insertionPayloadOffset,
  insertionValue.byteLength,
]);
const insertionFrame = decodeOutputFrame(
  encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: manifest.appId,
    buildId: manifest.buildId,
    minor: 1,
    operations: [insertionOperation],
    payloads: [insertionValue],
  }),
  manifest,
  0n,
);
assert.equal(insertionFrame.minor, 1);
assert.deepEqual(
  insertionFrame.operations[0].slots.map(({ slot, value }) => ({ slot, value })),
  [{ slot: 7, value: "inserted" }],
);
assert.throws(
  () =>
    decodeOutputFrame(
      encodeOutputFrame({
        kind: FrameKind.Patch,
        appId: manifest.appId,
        buildId: manifest.buildId,
        operations: [insertionOperation],
        payloads: [insertionValue],
      }),
      manifest,
      0n,
    ),
  /slot payloads require protocol minor 1/,
);

const dynamicKey = encoder.encode("alpha");
const dynamicBeforeKey = encoder.encode("beta");
const dynamicValue = encoder.encode("dynamic");
const dynamicInsertPayloadOffset = GLAMOUR_HEADER_BYTES + 48;
const dynamicInsertOperation = encodeOperation(Op.ListInsertDynamic, [
  13,
  dynamicInsertPayloadOffset,
  dynamicKey.byteLength,
  dynamicInsertPayloadOffset + dynamicKey.byteLength,
  dynamicBeforeKey.byteLength,
  3,
  1,
  7,
  dynamicInsertPayloadOffset + dynamicKey.byteLength + dynamicBeforeKey.byteLength,
  dynamicValue.byteLength,
]);
const dynamicInsertFrame = decodeOutputFrame(
  encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: manifest.appId,
    buildId: manifest.buildId,
    minor: 3,
    operations: [dynamicInsertOperation],
    payloads: [dynamicKey, dynamicBeforeKey, dynamicValue],
  }),
  manifest,
  0n,
);
assert.deepEqual(
  {
    kind: dynamicInsertFrame.operations[0].kind,
    region: dynamicInsertFrame.operations[0].region,
    key: dynamicInsertFrame.operations[0].key,
    beforeKey: dynamicInsertFrame.operations[0].beforeKey,
    template: dynamicInsertFrame.operations[0].template,
    slots: dynamicInsertFrame.operations[0].slots.map(({ slot, value }) => ({ slot, value })),
  },
  {
    kind: "list_insert_dynamic",
    region: 13,
    key: "alpha",
    beforeKey: "beta",
    template: 3,
    slots: [{ slot: 7, value: "dynamic" }],
  },
);
const dynamicCases = [
  {
    tag: Op.ListMoveDynamic,
    words: [13, GLAMOUR_HEADER_BYTES + 28, 5, GLAMOUR_HEADER_BYTES + 33, 4],
    payloads: [dynamicKey, dynamicBeforeKey],
    kind: "list_move_dynamic",
  },
  {
    tag: Op.ListRemoveDynamic,
    words: [13, GLAMOUR_HEADER_BYTES + 20, 5],
    payloads: [dynamicKey],
    kind: "list_remove_dynamic",
  },
  {
    tag: Op.UpdateDynamicSlots,
    words: [
      13,
      GLAMOUR_HEADER_BYTES + 36,
      5,
      1,
      7,
      GLAMOUR_HEADER_BYTES + 41,
      dynamicValue.byteLength,
    ],
    payloads: [dynamicKey, dynamicValue],
    kind: "update_dynamic_slots",
  },
];
for (const candidate of dynamicCases) {
  const decoded = decodeOutputFrame(
    encodeOutputFrame({
      kind: FrameKind.Patch,
      appId: manifest.appId,
      buildId: manifest.buildId,
      minor: 3,
      operations: [encodeOperation(candidate.tag, candidate.words)],
      payloads: candidate.payloads,
    }),
    manifest,
    0n,
  );
  assert.equal(decoded.operations[0].kind, candidate.kind);
  assert.equal(decoded.operations[0].key, "alpha");
}
assert.throws(
  () => decodeOutputFrame(
    encodeOutputFrame({
      kind: FrameKind.Patch,
      appId: manifest.appId,
      buildId: manifest.buildId,
      minor: 2,
      operations: [dynamicInsertOperation],
      payloads: [dynamicKey, dynamicBeforeKey, dynamicValue],
    }),
    manifest,
    0n,
  ),
  /requires protocol minor 3/,
);
const duplicateSlotOffset = GLAMOUR_HEADER_BYTES + 56;
assert.throws(
  () =>
    decodeOutputFrame(
      encodeOutputFrame({
        kind: FrameKind.Patch,
        appId: manifest.appId,
        buildId: manifest.buildId,
        minor: 1,
        operations: [
          encodeOperation(Op.ListInsert, [
            13,
            1,
            0,
            3,
            101,
            2,
            7,
            duplicateSlotOffset,
            insertionValue.byteLength,
            7,
            duplicateSlotOffset,
            insertionValue.byteLength,
          ]),
        ],
        payloads: [insertionValue],
      }),
      manifest,
      0n,
    ),
  /invalid or duplicate slot/,
);
assert.throws(
  () =>
    decodeOutputFrame(
      encodeOutputFrame({
        kind: FrameKind.Effects,
        appId: manifest.appId,
        buildId: manifest.buildId,
        operations: [
          encodeOperation(Op.SetText, [11, GLAMOUR_HEADER_BYTES + 20, 0]),
        ],
      }),
      manifest,
      0n,
    ),
  /effects frame contains a DOM operation/,
);
const completion = encodeEffectCompletionFrame({
  appId: manifest.appId,
  buildId: manifest.buildId,
  sequence: 9n,
  source: CompletionSource.Effect,
  instance: 101,
  generation: 3,
  descriptor: 37,
  resultSchema: 43,
  status: CompletionStatus.Ok,
  value: "done",
});
const completionView = new DataView(completion.buffer);
assert.equal(completionView.getUint8(8), FrameKind.EffectCompletion);
assert.equal(completionView.getBigUint64(32, true), 9n);
assert.equal(completionView.getUint32(GLAMOUR_HEADER_BYTES + 8, true), CompletionSource.Effect);
assert.equal(completionView.getUint32(GLAMOUR_HEADER_BYTES + 12, true), 101);
assert.equal(completionView.getUint32(GLAMOUR_HEADER_BYTES + 16, true), 3);
assert.equal(completionView.getUint32(GLAMOUR_HEADER_BYTES + 20, true), 37);
assert.equal(completionView.getUint32(GLAMOUR_HEADER_BYTES + 24, true), 43);
assert.equal(completionView.getUint32(GLAMOUR_HEADER_BYTES + 28, true), CompletionStatus.Ok);
assert.equal(
  new TextDecoder().decode(completion.subarray(completionView.getUint32(80, true))),
  "done",
);
const httpResult = encodeCompletionResult({
  descriptor: { semantic: "http" },
  status: CompletionStatus.Ok,
  value: { status: 204, body: "done" },
  maxBytes: 1024,
});
assert.equal(httpResult.status, CompletionStatus.Ok);
assert.deepEqual(
  [...httpResult.payload],
  [1, 0, 0, 0, 204, 0, 0, 0, 4, 0, 0, 0, 100, 111, 110, 101],
);
const binaryCompletion = encodeEffectCompletionFrame({
  appId: manifest.appId,
  buildId: manifest.buildId,
  sequence: 10n,
  source: CompletionSource.Effect,
  instance: 102,
  generation: 4,
  descriptor: 37,
  resultSchema: 43,
  status: httpResult.status,
  payload: httpResult.payload,
});
assert.deepEqual([...binaryCompletion.subarray(88)], [...httpResult.payload]);
const navigationFailure = encodeCompletionResult({
  descriptor: { semantic: "navigation" },
  status: CompletionStatus.Error,
  value: "denied",
  maxBytes: 1024,
});
assert.deepEqual(
  [...navigationFailure.payload],
  [2, 0, 0, 0, 6, 0, 0, 0, 100, 101, 110, 105, 101, 100],
);
const hostPortWire = JSON.stringify({ $variant: "IslandCaptureNode", $values: [0, []] });
const hostPortResult = encodeCompletionResult({
  descriptor: { semantic: "host-port" },
  status: CompletionStatus.Ok,
  value: hostPortWire,
  maxBytes: 512,
});
assert.equal(new TextDecoder().decode(hostPortResult.payload), hostPortWire);
const hostPortFailure = encodeCompletionResult({
  descriptor: { semantic: "host-port" },
  status: CompletionStatus.Error,
  value: "credential exchange unavailable",
  maxBytes: 512,
});
assert.equal(new TextDecoder().decode(hostPortFailure.payload), "credential exchange unavailable");
const storageMissing = encodeCompletionResult({
  descriptor: { semantic: "storage-get" },
  status: CompletionStatus.Ok,
  value: { kind: "missing" },
  maxBytes: 1024,
});
assert.deepEqual([...storageMissing.payload], [1, 0, 0, 0]);
const storageValue = encodeCompletionResult({
  descriptor: { semantic: "storage-get" },
  status: CompletionStatus.Ok,
  value: { kind: "value", value: "dark" },
  maxBytes: 1024,
});
assert.deepEqual(
  [...storageValue.payload],
  [2, 0, 0, 0, 4, 0, 0, 0, 100, 97, 114, 107],
);
const storageStored = encodeCompletionResult({
  descriptor: { semantic: "storage-set" },
  status: CompletionStatus.Ok,
  value: undefined,
  maxBytes: 1024,
});
assert.deepEqual([...storageStored.payload], [3, 0, 0, 0]);
const storageRemoved = encodeCompletionResult({
  descriptor: { semantic: "storage-remove" },
  status: CompletionStatus.Ok,
  value: undefined,
  maxBytes: 1024,
});
assert.deepEqual([...storageRemoved.payload], [4, 0, 0, 0]);
const storageFailure = encodeCompletionResult({
  descriptor: { semantic: "storage-get" },
  status: CompletionStatus.Error,
  value: "denied",
  maxBytes: 1024,
});
assert.deepEqual(
  [...storageFailure.payload],
  [5, 0, 0, 0, 6, 0, 0, 0, 100, 101, 110, 105, 101, 100],
);
assert.throws(
  () => encodeCompletionResult({
    descriptor: { semantic: "unknown" },
    status: CompletionStatus.Ok,
    value: "",
    maxBytes: 1024,
  }),
  /has no production codec/,
);

const actionInput = encodeActionInputFrame({
  appId: manifest.appId,
  buildId: manifest.buildId,
  sequence: 10n,
  inputSchema: 47,
  generation: 2,
  fields: [
    { ordinal: 0, kind: ActionFieldKind.Email, value: "ada@example.test" },
    { ordinal: 2, kind: ActionFieldKind.Checkbox, value: "true" },
  ],
});
const actionInputView = new DataView(actionInput.buffer);
assert.equal(actionInputView.getUint8(8), FrameKind.ActionInput);
assert.equal(actionInputView.getUint16(6, true), 4);
assert.equal(actionInputView.getBigUint64(32, true), 10n);
assert.equal(actionInputView.getUint32(GLAMOUR_HEADER_BYTES + 8, true), 47);
assert.equal(actionInputView.getUint32(GLAMOUR_HEADER_BYTES + 12, true), 2);
assert.equal(actionInputView.getUint32(GLAMOUR_HEADER_BYTES + 16, true), 2);
assert.equal(actionInputView.getUint16(GLAMOUR_HEADER_BYTES + 24, true), 0);
assert.equal(actionInputView.getUint8(GLAMOUR_HEADER_BYTES + 26), ActionFieldKind.Email);
assert.equal(actionInputView.getUint16(GLAMOUR_HEADER_BYTES + 40, true), 2);
assert.equal(actionInputView.getUint8(GLAMOUR_HEADER_BYTES + 42), ActionFieldKind.Checkbox);
const firstActionValue = actionInputView.getUint32(GLAMOUR_HEADER_BYTES + 28, true);
assert.equal(
  new TextDecoder().decode(actionInput.subarray(firstActionValue, firstActionValue + 16)),
  "ada@example.test",
);
assert.throws(
  () => encodeActionInputFrame({
    appId: manifest.appId,
    buildId: manifest.buildId,
    sequence: 0n,
    inputSchema: 47,
    generation: 2,
    fields: [
      { ordinal: 0, kind: ActionFieldKind.Text, value: "a" },
      { ordinal: 0, kind: ActionFieldKind.Text, value: "b" },
    ],
  }),
  /action input field is invalid/,
);
assert.throws(
  () => encodeActionInputFrame({
    appId: manifest.appId,
    buildId: manifest.buildId,
    sequence: 0n,
    inputSchema: 47,
    generation: 2,
    fields: [{ ordinal: 0, kind: ActionFieldKind.Text, value: "x".repeat(64 * 1024) }],
  }),
  /action input exceeds the byte limit/,
);

const actionCompletion = encodeActionCompletionFrame({
  appId: manifest.appId,
  buildId: manifest.buildId,
  sequence: 11n,
  resultSchema: 53,
  generation: 2,
  status: ActionCompletionStatus.Succeeded,
  httpStatus: 204,
});
const actionCompletionView = new DataView(actionCompletion.buffer);
assert.equal(actionCompletionView.getUint8(8), FrameKind.ActionCompletion);
assert.equal(actionCompletionView.getUint32(GLAMOUR_HEADER_BYTES + 8, true), 53);
assert.equal(actionCompletionView.getUint32(GLAMOUR_HEADER_BYTES + 12, true), 2);
assert.equal(
  actionCompletionView.getUint32(GLAMOUR_HEADER_BYTES + 16, true),
  ActionCompletionStatus.Succeeded,
);
assert.equal(actionCompletionView.getUint32(GLAMOUR_HEADER_BYTES + 20, true), 204);
assert.throws(
  () => encodeActionCompletionFrame({
    appId: manifest.appId,
    buildId: manifest.buildId,
    sequence: 0n,
    resultSchema: 53,
    generation: 2,
    status: ActionCompletionStatus.Succeeded,
    httpStatus: 42,
  }),
  /action completion is invalid/,
);

const validator = createOutputValidator(manifest);
const first = validator.validate(patchWithText("one", 0n));
let applied = "";
validator.accept(first, (accepted) => {
  applied = accepted.operations[0].value;
});
assert.equal(applied, "one");
assert.equal(validator.nextSequence, 1n);
assert.throws(() => validator.validate(patchWithText("stale", 0n)), /sequence/);

const second = validator.validate(patchWithText("two", 1n));
assert.throws(() => validator.accept(second, () => {
  throw new Error("DOM failed");
}), /DOM failed/);
assert.equal(validator.nextSequence, 1n, "failed application does not consume sequence");
validator.accept(second, () => {});
assert.equal(validator.nextSequence, 2n);

function corrupt(source, mutate) {
  const copy = source.slice();
  mutate(new DataView(copy.buffer), copy);
  return copy;
}

const valid = patchWithText("safe");
const rejected = [
  corrupt(valid, (_view, bytes) => {
    bytes[0] = 0;
  }),
  corrupt(valid, (view) => {
    view.setUint16(4, 2, true);
  }),
  corrupt(valid, (view) => {
    view.setUint8(9, 1);
  }),
  corrupt(valid, (view) => {
    view.setUint32(12, valid.byteLength + 1, true);
  }),
  corrupt(valid, (view) => {
    view.setUint32(16, 2, true);
  }),
  corrupt(valid, (view) => {
    view.setUint32(20, 8, true);
  }),
  corrupt(valid, (view) => {
    view.setBigUint64(24, 9n, true);
  }),
  corrupt(valid, (view) => {
    view.setUint16(GLAMOUR_HEADER_BYTES, 999, true);
  }),
  corrupt(valid, (view) => {
    view.setUint16(GLAMOUR_HEADER_BYTES + 2, 1, true);
  }),
  corrupt(valid, (view) => {
    view.setUint32(GLAMOUR_HEADER_BYTES + 4, 7, true);
  }),
  corrupt(valid, (view) => {
    view.setUint32(GLAMOUR_HEADER_BYTES + 8, 999, true);
  }),
  corrupt(valid, (view) => {
    view.setUint32(GLAMOUR_HEADER_BYTES + 12, 1, true);
  }),
  corrupt(valid, (view) => {
    view.setUint32(GLAMOUR_HEADER_BYTES + 12, GLAMOUR_HEADER_BYTES, true);
  }),
];
for (const candidate of rejected) {
  assert.throws(() => decodeOutputFrame(candidate, manifest, 0n));
}

const invalidUtf8 = patchWithText("xx");
invalidUtf8[invalidUtf8.byteLength - 2] = 0xc3;
invalidUtf8[invalidUtf8.byteLength - 1] = 0x28;
assert.throws(() => decodeOutputFrame(invalidUtf8, manifest, 0n), /UTF-8/);

for (const operation of [
  encodeOperation(Op.EnterBranch, [13, 3, 0]),
  encodeOperation(Op.MountChild, [13, 3, 0, 0]),
]) {
  const zeroInstance = encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: manifest.appId,
    buildId: manifest.buildId,
    sequence: 0n,
    operations: [operation],
  });
  assert.throws(() => decodeOutputFrame(zeroInstance, manifest, 0n), /instance zero/);
}

// Every single-byte mutation either remains a valid, semantically bounded
// frame or is rejected. Most importantly, the decoder never reads outside the
// supplied buffer or returns a partial operation list.
let acceptedMutations = 0;
for (let index = 0; index < valid.byteLength; index += 1) {
  const candidate = valid.slice();
  candidate[index] ^= 0xff;
  try {
    const decoded = decodeOutputFrame(candidate, manifest, 0n);
    assert.equal(decoded.operations.length, 1);
    acceptedMutations += 1;
  } catch (error) {
    assert.match(String(error), /^Error: glamour protocol:/);
  }
}
assert.ok(acceptedMutations < valid.byteLength / 2);

// Seeded generative campaign: produce varied valid UTF-8 frames, then apply
// several random byte mutations to each. Accepted mutations must still decode
// as one complete bounded operation; rejected mutations must fail through the
// protocol boundary rather than a RangeError or partial result.
let fuzzState = 0x1070_1080;
const random = () => {
  fuzzState ^= fuzzState << 13;
  fuzzState ^= fuzzState >>> 17;
  fuzzState ^= fuzzState << 5;
  return fuzzState >>> 0;
};
let fuzzRejected = 0;
for (let sample = 0; sample < 512; sample += 1) {
  const length = random() % 65;
  let value = "";
  for (let index = 0; index < length; index += 1) {
    value += String.fromCharCode(32 + (random() % 95));
  }
  const generated = patchWithText(value);
  assert.equal(decodeOutputFrame(generated, manifest, 0n).operations[0].value, value);
  const candidate = generated.slice();
  const mutations = 1 + (random() % 4);
  for (let mutation = 0; mutation < mutations; mutation += 1) {
    candidate[random() % candidate.byteLength] = random() & 0xff;
  }
  try {
    const decoded = decodeOutputFrame(candidate, manifest, 0n);
    assert.equal(decoded.operations.length, 1);
    assert.equal(decoded.operations[0].kind, "set_text");
    assert.ok(encoder.encode(decoded.operations[0].value).byteLength <= 128);
  } catch (error) {
    assert.match(String(error), /^Error: glamour protocol:/);
    fuzzRejected += 1;
  }
}
assert.ok(fuzzRejected > 400, "the generative campaign must exercise rejection paths");

validator.dispose();
assert.throws(() => validator.validate(patchWithText("late", 2n)), /disposed/);

console.log("GLAMOUR-PROTOCOL OK");
