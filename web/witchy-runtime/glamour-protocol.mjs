// Glamour optimized browser protocol (RFC-0108).
//
// This module is deliberately free of DOM calls. It turns an untrusted Wasm
// buffer into inert, fully validated records. The DOM host may apply the
// returned records only after this function succeeds for the complete frame.

export const GLAMOUR_PROTOCOL_MAJOR = 1;
export const GLAMOUR_PROTOCOL_MINOR = 4;
export const GLAMOUR_HEADER_BYTES = 48;

export const FrameKind = Object.freeze({
  Start: 1,
  Event: 2,
  EffectCompletion: 3,
  ActionInput: 4,
  ActionCompletion: 5,
  Activation: 6,
  Mount: 16,
  Patch: 17,
  Effects: 18,
  Diagnostic: 31,
});

export const Op = Object.freeze({
  Mount: 1,
  SetText: 2,
  SetProperty: 3,
  SetAttribute: 4,
  RemoveAttribute: 5,
  SetBooleanAttribute: 6,
  EnterBranch: 7,
  LeaveBranch: 8,
  ListInsert: 9,
  ListMove: 10,
  ListRemove: 11,
  MountChild: 12,
  UnmountChild: 13,
  SetEventPlan: 14,
  RemoveEventPlan: 15,
  SetClassList: 16,
  SetAria: 17,
  SetCustomProperty: 18,
  ListInsertDynamic: 19,
  ListMoveDynamic: 20,
  ListRemoveDynamic: 21,
  UpdateDynamicSlots: 22,
});

export const EffectOp = Object.freeze({
  Start: 0x100,
  Cancel: 0x101,
  SyncSubscription: 0x102,
  RemoveSubscription: 0x103,
});

export const CompletionSource = Object.freeze({
  Effect: 1,
  Subscription: 2,
});

export const CompletionStatus = Object.freeze({
  Ok: 0,
  Error: 1,
});

export const ActionFieldKind = Object.freeze({
  Text: 1,
  Email: 2,
  Number: 3,
  Checkbox: 4,
});

export const ActionCompletionStatus = Object.freeze({
  Succeeded: 0,
  ValidationFailed: 1,
  ServerFailed: 2,
  NetworkFailed: 3,
  Cancelled: 4,
  InvalidSubmission: 5,
});

const KNOWN_FRAME_KINDS = new Set(Object.values(FrameKind));
const KNOWN_OUTPUT_KINDS = new Set([
  FrameKind.Mount,
  FrameKind.Patch,
  FrameKind.Effects,
  FrameKind.Diagnostic,
]);
const STRICT_UTF8 = new TextDecoder("utf-8", { fatal: true });
const U32_MAX = 0xffff_ffff;
const ACTION_INPUT_MAX_BYTES = 64 * 1024;

const DEFAULT_LIMITS = Object.freeze({
  maxFrameBytes: 4 * 1024 * 1024,
  maxOperations: 100_000,
  maxPayloadBytes: 2 * 1024 * 1024,
  maxStringBytes: 1024 * 1024,
  maxSlots: 65_535,
});

function fail(message) {
  throw new Error(`glamour protocol: ${message}`);
}

function bytesOf(input) {
  if (input instanceof Uint8Array) return input;
  if (ArrayBuffer.isView(input)) {
    return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  }
  if (input instanceof ArrayBuffer) return new Uint8Array(input);
  fail("frame must be an ArrayBuffer or byte view");
}

function checkedEnd(start, length, total, label) {
  if (!Number.isInteger(start) || !Number.isInteger(length) || start < 0 || length < 0) {
    fail(`${label} has a negative or non-integer range`);
  }
  const end = start + length;
  if (!Number.isSafeInteger(end) || end > U32_MAX || end > total) {
    fail(`${label} is outside the frame`);
  }
  return end;
}

function strictString(bytes, offset, length, limits, label) {
  if (length > limits.maxStringBytes) fail(`${label} exceeds the string limit`);
  const end = checkedEnd(offset, length, bytes.byteLength, label);
  try {
    return STRICT_UTF8.decode(bytes.subarray(offset, end));
  } catch {
    fail(`${label} is not valid UTF-8`);
  }
}

function toIdSet(values) {
  if (values == null) return null;
  if (values instanceof Map) return new Set(values.keys());
  return values instanceof Set ? values : new Set(values);
}

function requireId(registry, id, label) {
  if (registry != null && !registry.has(id)) fail(`unknown ${label} ${id}`);
}

function readPayloadRef(view, bytes, cursor, limits, label) {
  const offset = view.getUint32(cursor, true);
  const length = view.getUint32(cursor + 4, true);
  if (offset < limits.payloadStart) fail(`${label} does not start in the payload table`);
  if (checkedEnd(offset, length, bytes.byteLength, label) > limits.payloadEnd) {
    fail(`${label} extends beyond the payload table`);
  }
  return {
    offset,
    length,
    value: strictString(bytes, offset, length, limits, label),
  };
}

function exactLength(actual, expected, name) {
  if (actual !== expected) fail(`${name} record has length ${actual}, expected ${expected}`);
}

function atLeastLength(actual, minimum, name) {
  if (actual < minimum) fail(`${name} record is shorter than ${minimum} bytes`);
}

function decodeSlots(view, bytes, cursor, count, context, label) {
  const { limits } = context;
  if (count > limits.maxSlots) fail(`${label} exceeds the slot limit`);
  const slots = [];
  const seen = new Set();
  for (let index = 0; index < count; index += 1) {
    const slot = view.getUint32(cursor, true);
    if (slot === 0 || seen.has(slot)) fail(`${label} contains an invalid or duplicate slot`);
    seen.add(slot);
    const payload = readPayloadRef(view, bytes, cursor + 4, limits, `${label} ${slot}`);
    slots.push(Object.freeze({ slot, ...payload }));
    cursor += 12;
  }
  return Object.freeze(slots);
}

function decodeOperation(tag, flags, length, cursor, view, bytes, context) {
  if (flags !== 0) fail(`operation ${tag} has unknown flags`);
  const body = cursor + 8;
  const { limits, registries } = context;
  const node = () => {
    const value = view.getUint32(body, true);
    requireId(registries.nodes, value, "node");
    return value;
  };
  const region = () => {
    const value = view.getUint32(body, true);
    requireId(registries.regions, value, "region");
    return value;
  };
  const template = (offset = 0) => {
    const value = view.getUint32(body + offset, true);
    requireId(registries.templates, value, "template");
    return value;
  };
  const dynamicKey = (offset, label, allowEmpty = false) => {
    const payload = readPayloadRef(view, bytes, body + offset, limits, label);
    if (payload.length > 1024 || (!allowEmpty && payload.length === 0)) {
      fail(`${label} is empty or exceeds 1024 bytes`);
    }
    return payload.value;
  };

  switch (tag) {
    case Op.Mount: {
      atLeastLength(length, 28, "Mount");
      const templateId = template();
      const instance = view.getUint32(body + 4, true);
      const parentRegion = view.getUint32(body + 8, true);
      const before = view.getUint32(body + 12, true);
      const slotCount = view.getUint32(body + 16, true);
      exactLength(length, 28 + slotCount * 12, "Mount");
      const slots = decodeSlots(view, bytes, body + 20, slotCount, context, "Mount slot");
      return Object.freeze({
        tag,
        kind: "mount",
        template: templateId,
        instance,
        parentRegion,
        before,
        slots,
      });
    }
    case Op.SetText: {
      exactLength(length, 20, "SetText");
      const nodeId = node();
      const payload = readPayloadRef(view, bytes, body + 4, limits, "SetText value");
      return Object.freeze({ tag, kind: "set_text", node: nodeId, ...payload });
    }
    case Op.SetProperty:
    case Op.SetAttribute:
    case Op.SetAria: {
      exactLength(length, 24, tag === Op.SetProperty ? "SetProperty" : tag === Op.SetAria ? "SetAria" : "SetAttribute");
      const nodeId = node();
      const sink = view.getUint32(body + 4, true);
      if (tag === Op.SetProperty) requireId(registries.properties, sink, "property");
      else if (tag === Op.SetAria) requireId(registries.aria, sink, "ARIA attribute");
      else requireId(registries.attributes, sink, "attribute");
      const payload = readPayloadRef(view, bytes, body + 8, limits, "attribute value");
      return Object.freeze({
        tag,
        kind: tag === Op.SetProperty ? "set_property" : tag === Op.SetAria ? "set_aria" : "set_attribute",
        node: nodeId,
        sink,
        ...payload,
      });
    }
    case Op.SetCustomProperty: {
      if (context.minor < 2) fail("SetCustomProperty requires protocol minor 2");
      exactLength(length, 24, "SetCustomProperty");
      const nodeId = node();
      const sink = view.getUint32(body + 4, true);
      requireId(registries.customProperties, sink, "custom property");
      const payload = readPayloadRef(view, bytes, body + 8, limits, "custom property value");
      return Object.freeze({
        tag,
        kind: "set_custom_property",
        node: nodeId,
        sink,
        ...payload,
      });
    }
    case Op.RemoveAttribute: {
      exactLength(length, 16, "RemoveAttribute");
      const nodeId = node();
      const attribute = view.getUint32(body + 4, true);
      requireId(registries.attributes, attribute, "attribute");
      return Object.freeze({ tag, kind: "remove_attribute", node: nodeId, attribute });
    }
    case Op.SetBooleanAttribute: {
      exactLength(length, 20, "SetBooleanAttribute");
      const nodeId = node();
      const attribute = view.getUint32(body + 4, true);
      requireId(registries.attributes, attribute, "attribute");
      const raw = view.getUint32(body + 8, true);
      if (raw > 1) fail("SetBooleanAttribute value is not boolean");
      return Object.freeze({
        tag,
        kind: "set_boolean_attribute",
        node: nodeId,
        attribute,
        enabled: raw === 1,
      });
    }
    case Op.EnterBranch: {
      const regionId = region();
      const templateId = template(4);
      const instance = view.getUint32(body + 8, true);
      if (instance === 0) fail("EnterBranch instance zero is reserved");
      let slots = Object.freeze([]);
      if (length === 20) {
        // Protocol 1.0 branch records carried no slot-count word.
      } else {
        if (context.minor < 1) fail("EnterBranch slot payloads require protocol minor 1");
        atLeastLength(length, 24, "EnterBranch");
        const slotCount = view.getUint32(body + 12, true);
        exactLength(length, 24 + slotCount * 12, "EnterBranch");
        slots = decodeSlots(view, bytes, body + 16, slotCount, context, "EnterBranch slot");
      }
      return Object.freeze({
        tag,
        kind: "enter_branch",
        region: regionId,
        template: templateId,
        instance,
        slots,
      });
    }
    case Op.LeaveBranch:
    case Op.UnmountChild: {
      exactLength(length, 12, tag === Op.LeaveBranch ? "LeaveBranch" : "UnmountChild");
      return Object.freeze({
        tag,
        kind: tag === Op.LeaveBranch ? "leave_branch" : "unmount_child",
        region: region(),
      });
    }
    case Op.ListInsert: {
      const regionId = region();
      const key = view.getUint32(body + 4, true);
      const beforeKey = view.getUint32(body + 8, true);
      const templateId = template(12);
      const instance = view.getUint32(body + 16, true);
      const slotCount = view.getUint32(body + 20, true);
      if (slotCount !== 0 && context.minor < 1) {
        fail("ListInsert slot payloads require protocol minor 1");
      }
      exactLength(length, 32 + slotCount * 12, "ListInsert");
      const slots = decodeSlots(view, bytes, body + 24, slotCount, context, "ListInsert slot");
      return Object.freeze({
        tag,
        kind: "list_insert",
        region: regionId,
        key,
        beforeKey,
        template: templateId,
        instance,
        slots,
      });
    }
    case Op.ListMove: {
      exactLength(length, 20, "ListMove");
      return Object.freeze({
        tag,
        kind: "list_move",
        region: region(),
        key: view.getUint32(body + 4, true),
        beforeKey: view.getUint32(body + 8, true),
      });
    }
    case Op.ListRemove: {
      exactLength(length, 16, "ListRemove");
      return Object.freeze({
        tag,
        kind: "list_remove",
        region: region(),
        key: view.getUint32(body + 4, true),
      });
    }
    case Op.ListInsertDynamic: {
      if (context.minor < 3) fail("ListInsertDynamic requires protocol minor 3");
      const regionId = region();
      const key = dynamicKey(4, "ListInsertDynamic key");
      const beforeKey = dynamicKey(12, "ListInsertDynamic before key", true);
      const templateId = template(20);
      const slotCount = view.getUint32(body + 24, true);
      exactLength(length, 36 + slotCount * 12, "ListInsertDynamic");
      const slots = decodeSlots(view, bytes, body + 28, slotCount, context, "ListInsertDynamic slot");
      return Object.freeze({
        tag,
        kind: "list_insert_dynamic",
        region: regionId,
        key,
        beforeKey,
        template: templateId,
        slots,
      });
    }
    case Op.ListMoveDynamic: {
      if (context.minor < 3) fail("ListMoveDynamic requires protocol minor 3");
      exactLength(length, 28, "ListMoveDynamic");
      return Object.freeze({
        tag,
        kind: "list_move_dynamic",
        region: region(),
        key: dynamicKey(4, "ListMoveDynamic key"),
        beforeKey: dynamicKey(12, "ListMoveDynamic before key", true),
      });
    }
    case Op.ListRemoveDynamic: {
      if (context.minor < 3) fail("ListRemoveDynamic requires protocol minor 3");
      exactLength(length, 20, "ListRemoveDynamic");
      return Object.freeze({
        tag,
        kind: "list_remove_dynamic",
        region: region(),
        key: dynamicKey(4, "ListRemoveDynamic key"),
      });
    }
    case Op.UpdateDynamicSlots: {
      if (context.minor < 3) fail("UpdateDynamicSlots requires protocol minor 3");
      const regionId = region();
      const key = dynamicKey(4, "UpdateDynamicSlots key");
      const slotCount = view.getUint32(body + 12, true);
      exactLength(length, 24 + slotCount * 12, "UpdateDynamicSlots");
      const slots = decodeSlots(view, bytes, body + 16, slotCount, context, "UpdateDynamicSlots slot");
      return Object.freeze({ tag, kind: "update_dynamic_slots", region: regionId, key, slots });
    }
    case Op.MountChild: {
      const regionId = region();
      const templateId = template(4);
      const instance = view.getUint32(body + 8, true);
      const slotCount = view.getUint32(body + 12, true);
      if (instance === 0) fail("MountChild instance zero is reserved");
      if (slotCount !== 0 && context.minor < 1) {
        fail("MountChild slot payloads require protocol minor 1");
      }
      exactLength(length, 24 + slotCount * 12, "MountChild");
      const slots = decodeSlots(view, bytes, body + 16, slotCount, context, "MountChild slot");
      return Object.freeze({
        tag,
        kind: "mount_child",
        region: regionId,
        template: templateId,
        instance,
        slots,
      });
    }
    case Op.SetEventPlan: {
      exactLength(length, 20, "SetEventPlan");
      const nodeId = node();
      const eventClass = view.getUint32(body + 4, true);
      const eventPlan = view.getUint32(body + 8, true);
      requireId(registries.eventClasses, eventClass, "event class");
      requireId(registries.eventPlans, eventPlan, "event plan");
      return Object.freeze({
        tag,
        kind: "set_event_plan",
        node: nodeId,
        eventClass,
        eventPlan,
      });
    }
    case Op.RemoveEventPlan: {
      exactLength(length, 16, "RemoveEventPlan");
      const nodeId = node();
      const eventClass = view.getUint32(body + 4, true);
      requireId(registries.eventClasses, eventClass, "event class");
      return Object.freeze({
        tag,
        kind: "remove_event_plan",
        node: nodeId,
        eventClass,
      });
    }
    case Op.SetClassList: {
      exactLength(length, 20, "SetClassList");
      const nodeId = node();
      const payload = readPayloadRef(view, bytes, body + 4, limits, "class list");
      return Object.freeze({ tag, kind: "set_class_list", node: nodeId, ...payload });
    }
    case EffectOp.Start: {
      exactLength(length, 28, "StartEffect");
      const instance = view.getUint32(body, true);
      const cancellationKey = view.getUint32(body + 4, true);
      const descriptor = view.getUint32(body + 8, true);
      requireId(registries.effectDescriptors, descriptor, "effect descriptor");
      const request = readPayloadRef(
        view,
        bytes,
        body + 12,
        limits,
        "effect request",
      );
      if (instance === 0) fail("StartEffect instance zero is reserved");
      return Object.freeze({
        tag,
        kind: "start_effect",
        instance,
        cancellationKey,
        descriptor,
        request: request.value,
      });
    }
    case EffectOp.Cancel: {
      exactLength(length, 12, "CancelEffect");
      const cancellationKey = view.getUint32(body, true);
      if (cancellationKey === 0) fail("CancelEffect key zero is reserved");
      return Object.freeze({ tag, kind: "cancel_effect", cancellationKey });
    }
    case EffectOp.SyncSubscription: {
      exactLength(length, 24, "SyncSubscription");
      const subscription = view.getUint32(body, true);
      const descriptor = view.getUint32(body + 4, true);
      requireId(registries.subscriptionDescriptors, descriptor, "subscription descriptor");
      const request = readPayloadRef(
        view,
        bytes,
        body + 8,
        limits,
        "subscription request",
      );
      if (subscription === 0) fail("SyncSubscription identity zero is reserved");
      return Object.freeze({
        tag,
        kind: "sync_subscription",
        subscription,
        descriptor,
        request: request.value,
      });
    }
    case EffectOp.RemoveSubscription: {
      exactLength(length, 12, "RemoveSubscription");
      const subscription = view.getUint32(body, true);
      if (subscription === 0) fail("RemoveSubscription identity zero is reserved");
      return Object.freeze({ tag, kind: "remove_subscription", subscription });
    }
    default:
      fail(`unknown operation ${tag}`);
  }
}

function normalizedManifest(manifest = {}) {
  const limits = Object.freeze({ ...DEFAULT_LIMITS, ...(manifest.limits || {}) });
  for (const [name, value] of Object.entries(limits)) {
    if (!Number.isInteger(value) || value < 0 || value > U32_MAX) {
      fail(`manifest limit ${name} is invalid`);
    }
  }
  const buildId =
    typeof manifest.buildId === "bigint" ? manifest.buildId : BigInt(manifest.buildId || 0);
  return Object.freeze({
    appId: Number(manifest.appId || 0),
    buildId,
    limits,
    registries: Object.freeze({
      templates: toIdSet(manifest.templates),
      nodes: toIdSet(manifest.nodes),
      regions: toIdSet(manifest.regions),
      properties: toIdSet(manifest.properties),
      attributes: toIdSet(manifest.attributes),
      aria: toIdSet(manifest.aria),
      customProperties: toIdSet(manifest.customProperties),
      eventClasses: toIdSet(manifest.eventClasses),
      eventPlans: toIdSet(manifest.eventPlans),
      effectDescriptors: toIdSet(manifest.effectDescriptors),
      subscriptionDescriptors: toIdSet(manifest.subscriptionDescriptors),
    }),
  });
}

/**
 * Decode and validate one complete Wasm-to-host frame.
 *
 * No caller state is mutated. `expectedSequence` is checked when supplied.
 * Registry arrays/sets in `manifest` make IDs deny-by-default; an omitted
 * registry defers that identity class to the later live-DOM validation pass.
 */
export function decodeOutputFrame(input, manifest = {}, expectedSequence = undefined) {
  const bytes = bytesOf(input);
  const context = normalizedManifest(manifest);
  const { limits } = context;
  if (bytes.byteLength < GLAMOUR_HEADER_BYTES) fail("frame is shorter than its header");
  if (bytes.byteLength > limits.maxFrameBytes) fail("frame exceeds the byte limit");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (
    bytes[0] !== 0x47 ||
    bytes[1] !== 0x4c ||
    bytes[2] !== 0x4d ||
    bytes[3] !== 0x52
  ) {
    fail("bad magic");
  }
  const major = view.getUint16(4, true);
  const minor = view.getUint16(6, true);
  if (major !== GLAMOUR_PROTOCOL_MAJOR) fail(`unsupported major version ${major}`);
  if (minor > GLAMOUR_PROTOCOL_MINOR) fail(`unsupported minor version ${minor}`);
  const kind = view.getUint8(8);
  const flags = view.getUint8(9);
  const headerLength = view.getUint16(10, true);
  const totalLength = view.getUint32(12, true);
  const operationCount = view.getUint32(16, true);
  const appId = view.getUint32(20, true);
  const buildId = view.getBigUint64(24, true);
  const sequence = view.getBigUint64(32, true);
  const payloadOffset = view.getUint32(40, true);
  const traceOffset = view.getUint32(44, true);

  if (!KNOWN_FRAME_KINDS.has(kind) || !KNOWN_OUTPUT_KINDS.has(kind)) {
    fail(`invalid output frame kind ${kind}`);
  }
  if (flags !== 0) fail("frame has unknown flags");
  if (headerLength !== GLAMOUR_HEADER_BYTES) fail(`unsupported header length ${headerLength}`);
  if (totalLength !== bytes.byteLength) fail("declared byte length does not match the buffer");
  if (operationCount > limits.maxOperations) fail("frame exceeds the operation limit");
  if (appId !== context.appId) fail(`application identity ${appId} does not match`);
  if (buildId !== context.buildId) fail("build identity does not match");
  if (expectedSequence !== undefined && sequence !== BigInt(expectedSequence)) {
    fail(`sequence ${sequence} does not match expected ${expectedSequence}`);
  }
  if (payloadOffset !== 0) {
    if (payloadOffset < headerLength || payloadOffset > totalLength) {
      fail("payload table offset is outside the frame");
    }
    if (totalLength - payloadOffset > limits.maxPayloadBytes) {
      fail("frame exceeds the payload limit");
    }
  }
  if (traceOffset !== 0 && (traceOffset < headerLength || traceOffset >= totalLength)) {
    fail("trace offset is outside the frame");
  }

  if (payloadOffset !== 0 && traceOffset !== 0 && traceOffset <= payloadOffset) {
    fail("trace data must follow the payload table");
  }
  const operationEnd = payloadOffset || traceOffset || totalLength;
  if (operationEnd < headerLength) fail("operation area ends inside the header");
  const payloadStart = payloadOffset || operationEnd;
  const payloadEnd = traceOffset || totalLength;
  const decodeContext = {
    ...context,
    minor,
    limits: Object.freeze({ ...limits, payloadStart, payloadEnd }),
  };
  const operations = [];
  let cursor = headerLength;
  for (let index = 0; index < operationCount; index += 1) {
    checkedEnd(cursor, 8, operationEnd, `operation ${index} prefix`);
    const tag = view.getUint16(cursor, true);
    const opFlags = view.getUint16(cursor + 2, true);
    const length = view.getUint32(cursor + 4, true);
    if (length < 8) fail(`operation ${index} has an undersized record`);
    checkedEnd(cursor, length, operationEnd, `operation ${index}`);
    operations.push(decodeOperation(tag, opFlags, length, cursor, view, bytes, decodeContext));
    cursor += length;
  }
  if (cursor !== operationEnd) {
    fail("operation count does not consume the operation area exactly");
  }
  if (
    kind === FrameKind.Effects &&
    operations.some((operation) => operation.tag < EffectOp.Start)
  ) {
    fail("effects frame contains a DOM operation");
  }

  return Object.freeze({
    major,
    minor,
    kind,
    sequence,
    appId,
    buildId,
    byteLength: totalLength,
    operations: Object.freeze(operations),
    traceOffset,
  });
}

/**
 * Stateful sequence validator. Sequence advances only after the callback
 * successfully accepts the complete inert frame, so a decoder or DOM failure
 * cannot silently consume a sequence number.
 */
export function createOutputValidator(manifest, initialSequence = 0n) {
  let nextSequence = BigInt(initialSequence);
  let disposed = false;
  return Object.freeze({
    validate(input) {
      if (disposed) fail("validator is disposed");
      return decodeOutputFrame(input, manifest, nextSequence);
    },
    accept(frame, apply) {
      if (disposed) fail("validator is disposed");
      if (frame.sequence !== nextSequence) fail("frame was not validated for the next sequence");
      apply(frame);
      nextSequence += 1n;
    },
    dispose() {
      disposed = true;
    },
    get nextSequence() {
      return nextSequence;
    },
  });
}

// Test/compiler helper for deterministic fixture frames. Production Wasm
// constructs the same bytes itself; this helper is not used to translate JSON.
export function encodeOutputFrame({
  kind = FrameKind.Patch,
  appId = 0,
  buildId = 0n,
  sequence = 0n,
  operations = [],
  payloads = [],
  minor = 0,
}) {
  const opBytes = operations.reduce((sum, operation) => sum + operation.byteLength, 0);
  const payloadBytes = payloads.reduce((sum, payload) => sum + bytesOf(payload).byteLength, 0);
  const payloadOffset = payloadBytes === 0 ? 0 : GLAMOUR_HEADER_BYTES + opBytes;
  const out = new Uint8Array(GLAMOUR_HEADER_BYTES + opBytes + payloadBytes);
  const view = new DataView(out.buffer);
  out.set([0x47, 0x4c, 0x4d, 0x52], 0);
  view.setUint16(4, GLAMOUR_PROTOCOL_MAJOR, true);
  if (!Number.isInteger(minor) || minor < 0 || minor > GLAMOUR_PROTOCOL_MINOR) {
    fail("output frame minor version is unsupported");
  }
  view.setUint16(6, minor, true);
  view.setUint8(8, kind);
  view.setUint8(9, 0);
  view.setUint16(10, GLAMOUR_HEADER_BYTES, true);
  view.setUint32(12, out.byteLength, true);
  view.setUint32(16, operations.length, true);
  view.setUint32(20, appId, true);
  view.setBigUint64(24, BigInt(buildId), true);
  view.setBigUint64(32, BigInt(sequence), true);
  view.setUint32(40, payloadOffset, true);
  view.setUint32(44, 0, true);
  let cursor = GLAMOUR_HEADER_BYTES;
  for (const operation of operations) {
    out.set(bytesOf(operation), cursor);
    cursor += operation.byteLength;
  }
  for (const payload of payloads) {
    const bytes = bytesOf(payload);
    out.set(bytes, cursor);
    cursor += bytes.byteLength;
  }
  return out;
}

export function encodeEventFrame({
  appId,
  buildId,
  sequence,
  eventPlan,
  instance,
  eventClass,
  value = "",
  key = "",
  checked = false,
  composing = false,
  autofill = false,
  userActivation = false,
}) {
  const valueBytes = new TextEncoder().encode(String(value));
  const keyBytes = new TextEncoder().encode(String(key));
  const recordBytes = 48;
  const payloadOffset = GLAMOUR_HEADER_BYTES + recordBytes;
  const out = new Uint8Array(payloadOffset + valueBytes.byteLength + keyBytes.byteLength);
  const view = new DataView(out.buffer);
  out.set([0x47, 0x4c, 0x4d, 0x52], 0);
  view.setUint16(4, GLAMOUR_PROTOCOL_MAJOR, true);
  view.setUint16(6, 0, true);
  view.setUint8(8, FrameKind.Event);
  view.setUint16(10, GLAMOUR_HEADER_BYTES, true);
  view.setUint32(12, out.byteLength, true);
  view.setUint32(16, 1, true);
  view.setUint32(20, appId, true);
  view.setBigUint64(24, BigInt(buildId), true);
  view.setBigUint64(32, BigInt(sequence), true);
  view.setUint32(40, payloadOffset, true);
  view.setUint16(GLAMOUR_HEADER_BYTES, 1, true);
  view.setUint16(GLAMOUR_HEADER_BYTES + 2, 0, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 4, recordBytes, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 8, eventPlan, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 12, instance, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 16, eventClass, true);
  let eventFlags = checked ? 1 : 0;
  if (composing) eventFlags |= 2;
  if (autofill) eventFlags |= 4;
  if (userActivation) eventFlags |= 8;
  view.setUint32(GLAMOUR_HEADER_BYTES + 20, eventFlags, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 24, payloadOffset, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 28, valueBytes.byteLength, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 32, payloadOffset + valueBytes.byteLength, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 36, keyBytes.byteLength, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 40, 0, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 44, 0, true);
  out.set(valueBytes, payloadOffset);
  out.set(keyBytes, payloadOffset + valueBytes.byteLength);
  return out;
}

export function encodeActivationFrame({ appId, buildId, sequence }) {
  const out = new Uint8Array(GLAMOUR_HEADER_BYTES);
  const view = new DataView(out.buffer);
  out.set([0x47, 0x4c, 0x4d, 0x52], 0);
  view.setUint16(4, GLAMOUR_PROTOCOL_MAJOR, true);
  view.setUint16(6, 0, true);
  view.setUint8(8, FrameKind.Activation);
  view.setUint8(9, 0);
  view.setUint16(10, GLAMOUR_HEADER_BYTES, true);
  view.setUint32(12, GLAMOUR_HEADER_BYTES, true);
  view.setUint32(16, 0, true);
  view.setUint32(20, appId, true);
  view.setBigUint64(24, BigInt(buildId), true);
  view.setBigUint64(32, BigInt(sequence), true);
  view.setUint32(40, 0, true);
  view.setUint32(44, 0, true);
  return out;
}

export function encodeEffectCompletionFrame({
  appId,
  buildId,
  sequence,
  source,
  instance,
  generation,
  descriptor,
  resultSchema,
  status = CompletionStatus.Ok,
  payload,
  value = "",
}) {
  if (!Object.values(CompletionSource).includes(source)) {
    fail("completion source is invalid");
  }
  if (!Object.values(CompletionStatus).includes(status)) {
    fail("completion status is invalid");
  }
  const encodedPayload = payload === undefined
    ? new TextEncoder().encode(String(value))
    : bytesOf(payload);
  const recordBytes = 40;
  const payloadOffset = GLAMOUR_HEADER_BYTES + recordBytes;
  const out = new Uint8Array(payloadOffset + encodedPayload.byteLength);
  const view = new DataView(out.buffer);
  out.set([0x47, 0x4c, 0x4d, 0x52], 0);
  view.setUint16(4, GLAMOUR_PROTOCOL_MAJOR, true);
  view.setUint16(6, 0, true);
  view.setUint8(8, FrameKind.EffectCompletion);
  view.setUint16(10, GLAMOUR_HEADER_BYTES, true);
  view.setUint32(12, out.byteLength, true);
  view.setUint32(16, 1, true);
  view.setUint32(20, appId, true);
  view.setBigUint64(24, BigInt(buildId), true);
  view.setBigUint64(32, BigInt(sequence), true);
  view.setUint32(40, payloadOffset, true);
  view.setUint16(GLAMOUR_HEADER_BYTES, 1, true);
  view.setUint16(GLAMOUR_HEADER_BYTES + 2, 0, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 4, recordBytes, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 8, source, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 12, instance, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 16, generation, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 20, descriptor, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 24, resultSchema, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 28, status, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 32, payloadOffset, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 36, encodedPayload.byteLength, true);
  out.set(encodedPayload, payloadOffset);
  return out;
}

export function encodeActionInputFrame({
  appId,
  buildId,
  sequence,
  inputSchema,
  generation,
  fields = [],
}) {
  if (
    !u32Identity(inputSchema) ||
    !u32Identity(generation) ||
    !Array.isArray(fields) ||
    fields.length > 256
  ) {
    fail("action input identity or fields are invalid");
  }
  const encoder = new TextEncoder();
  const seen = new Set();
  let previousOrdinal = -1;
  const encoded = fields.map((field) => {
    if (
      field === null ||
      typeof field !== "object" ||
      !Number.isInteger(field.ordinal) ||
      field.ordinal < 0 ||
      field.ordinal > 0xffff ||
      !Object.values(ActionFieldKind).includes(field.kind) ||
      typeof field.value !== "string" ||
      seen.has(field.ordinal) ||
      field.ordinal <= previousOrdinal
    ) {
      fail("action input field is invalid");
    }
    seen.add(field.ordinal);
    previousOrdinal = field.ordinal;
    return { ...field, bytes: encoder.encode(field.value) };
  });
  const recordBytes = 24 + encoded.length * 16;
  const payloadBytes = encoded.reduce((sum, field) => sum + field.bytes.byteLength, 0);
  const payloadOffset = payloadBytes === 0 ? 0 : GLAMOUR_HEADER_BYTES + recordBytes;
  const totalBytes = GLAMOUR_HEADER_BYTES + recordBytes + payloadBytes;
  if (totalBytes > ACTION_INPUT_MAX_BYTES) fail("action input exceeds the byte limit");
  const out = new Uint8Array(totalBytes);
  const view = new DataView(out.buffer);
  writeInputHeader(view, out, FrameKind.ActionInput, appId, buildId, sequence, payloadOffset);
  view.setUint16(GLAMOUR_HEADER_BYTES, 1, true);
  view.setUint16(GLAMOUR_HEADER_BYTES + 2, 0, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 4, recordBytes, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 8, inputSchema, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 12, generation, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 16, encoded.length, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 20, 0, true);
  let payloadCursor = payloadOffset;
  for (const [index, field] of encoded.entries()) {
    const cursor = GLAMOUR_HEADER_BYTES + 24 + index * 16;
    view.setUint16(cursor, field.ordinal, true);
    view.setUint8(cursor + 2, field.kind);
    view.setUint8(cursor + 3, 0);
    view.setUint32(cursor + 4, payloadCursor, true);
    view.setUint32(cursor + 8, field.bytes.byteLength, true);
    view.setUint32(cursor + 12, 0, true);
    out.set(field.bytes, payloadCursor);
    payloadCursor += field.bytes.byteLength;
  }
  return out;
}

export function encodeActionCompletionFrame({
  appId,
  buildId,
  sequence,
  resultSchema,
  generation,
  status,
  httpStatus = 0,
}) {
  if (
    !u32Identity(resultSchema) ||
    !u32Identity(generation) ||
    !Object.values(ActionCompletionStatus).includes(status) ||
    (!Number.isInteger(httpStatus) || (httpStatus !== 0 && (httpStatus < 100 || httpStatus > 599)))
  ) {
    fail("action completion is invalid");
  }
  const recordBytes = 32;
  const out = new Uint8Array(GLAMOUR_HEADER_BYTES + recordBytes);
  const view = new DataView(out.buffer);
  writeInputHeader(view, out, FrameKind.ActionCompletion, appId, buildId, sequence, 0);
  view.setUint16(GLAMOUR_HEADER_BYTES, 1, true);
  view.setUint16(GLAMOUR_HEADER_BYTES + 2, 0, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 4, recordBytes, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 8, resultSchema, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 12, generation, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 16, status, true);
  view.setUint32(GLAMOUR_HEADER_BYTES + 20, httpStatus, true);
  view.setBigUint64(GLAMOUR_HEADER_BYTES + 24, 0n, true);
  return out;
}

function u32Identity(value) {
  return Number.isInteger(value) && value > 0 && value <= U32_MAX;
}

function writeInputHeader(view, out, kind, appId, buildId, sequence, payloadOffset) {
  if (!u32Identity(appId)) fail("application identity is invalid");
  out.set([0x47, 0x4c, 0x4d, 0x52], 0);
  view.setUint16(4, GLAMOUR_PROTOCOL_MAJOR, true);
  view.setUint16(6, GLAMOUR_PROTOCOL_MINOR, true);
  view.setUint8(8, kind);
  view.setUint8(9, 0);
  view.setUint16(10, GLAMOUR_HEADER_BYTES, true);
  view.setUint32(12, out.byteLength, true);
  view.setUint32(16, 1, true);
  view.setUint32(20, appId, true);
  view.setBigUint64(24, BigInt(buildId), true);
  view.setBigUint64(32, BigInt(sequence), true);
  view.setUint32(40, payloadOffset, true);
  view.setUint32(44, 0, true);
}

export function encodeOperation(tag, words = []) {
  const out = new Uint8Array(8 + words.length * 4);
  const view = new DataView(out.buffer);
  view.setUint16(0, tag, true);
  view.setUint16(2, 0, true);
  view.setUint32(4, out.byteLength, true);
  words.forEach((word, index) => view.setUint32(8 + index * 4, word, true));
  return out;
}
