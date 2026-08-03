// RFC-0108 optimized Glamour host.
//
// Static DOM comes from a build-authenticated manifest. Wasm output is decoded
// into inert records in glamour-protocol.mjs and fully planned before the first
// live DOM mutation. Browser events are delegated at the application root.

import { instantiate as instantiateWitchy } from "./witchy-runtime.mjs";
import { encodeCompletionResult } from "./glamour-completion-codecs.mjs";
import { createEffectHost } from "./glamour-effect-host.mjs";
import { installProgressiveForms } from "./glamour-forms.mjs";
import {
  ActionCompletionStatus,
  ActionFieldKind,
  CompletionSource,
  CompletionStatus,
  FrameKind,
  GLAMOUR_PROTOCOL_MAJOR,
  GLAMOUR_PROTOCOL_MINOR,
  createOutputValidator,
  encodeActivationFrame,
  encodeActionCompletionFrame,
  encodeActionInputFrame,
  encodeEffectCompletionFrame,
  encodeEventFrame,
} from "./glamour-protocol.mjs";

// Protocol-v1 static-template element table. This intentionally matches the
// compatibility host's structural sink boundary; executable/resource container
// elements remain unavailable even if a manifest is malformed.
const SAFE_ELEMENTS = new Set([
  "a", "abbr", "address", "article", "aside", "b", "bdi", "bdo", "blockquote", "br",
  "button", "caption", "cite", "code", "col", "colgroup", "data", "datalist", "dd",
  "del", "details", "dfn", "dialog", "div", "dl", "dt", "em", "fieldset", "figcaption",
  "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header",
  "hgroup", "hr", "i", "img", "input", "ins", "kbd", "label", "legend", "li", "main",
  "mark", "menu", "meter", "nav", "ol", "optgroup", "option", "output", "p", "picture",
  "pre", "progress", "q", "rp", "rt", "ruby", "s", "samp", "section", "select", "small",
  "source", "span", "strong", "sub", "summary", "sup", "table", "tbody", "td", "textarea",
  "tfoot", "th", "thead", "time", "tr", "u", "ul", "var", "wbr",
]);

function fail(message) {
  throw new Error(`glamour optimized: ${message}`);
}

function developmentFrameDigest(frame) {
  // A bounded non-cryptographic identity is sufficient here: the frame bytes
  // never leave the private recorder, and the digest is only a correlation key
  // for development tooling. Payload bytes are deliberately never exposed.
  let hash = 2166136261;
  for (const byte of frame) {
    hash ^= byte;
    hash = Math.imul(hash, 16777619) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

function asMap(value) {
  if (value instanceof Map) return value;
  if (Array.isArray(value)) {
    const result = new Map();
    for (const item of value) {
      if (!item || typeof item !== "object" || !Number.isInteger(item.id) || result.has(item.id)) {
        fail("manifest registry array contains an invalid or duplicate identity");
      }
      result.set(item.id, item);
    }
    return result;
  }
  return new Map(Object.entries(value || {}).map(([key, item]) => [Number(key), item]));
}

function asNameMap(value, label) {
  const entries = asMap(value);
  const result = new Map();
  for (const [id, item] of entries) {
    const name = typeof item === "string" ? item : item?.name;
    if (!Number.isInteger(id) || id <= 0 || typeof name !== "string" || name.length === 0 || name.length > 128) {
      fail(`${label} registry contains an invalid identity or name`);
    }
    result.set(id, name);
  }
  return result;
}

function asCustomPropertyMap(value) {
  const entries = asMap(value);
  const result = new Map();
  for (const [id, item] of entries) {
    const name = item?.name;
    const category = item?.category;
    if (
      !Number.isInteger(id) ||
      id <= 0 ||
      typeof name !== "string" ||
      !["color", "length", "number", "percentage", "angle", "time"].includes(category) ||
      name.length > 74 ||
      !/^--glamour-[A-Za-z_][A-Za-z0-9_]*$/.test(name)
    ) {
      fail("custom property registry contains an invalid identity, name, or category");
    }
    result.set(id, Object.freeze({ name, category }));
  }
  return result;
}

function validCustomPropertyValue(category, value) {
  if (/^var\(--glamour-[A-Za-z_][A-Za-z0-9_]*\)$/.test(value)) return true;
  if (category === "color") {
    return new Set([
      "transparent", "currentcolor", "black", "white", "red", "green", "blue", "rebeccapurple",
    ]).has(value) || /^#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/.test(value);
  }
  const patterns = {
    length: /^(-?(?:0|[1-9][0-9]*))(px|rem)$/,
    number: /^(-?(?:0|[1-9][0-9]*))$/,
    percentage: /^(-?(?:0|[1-9][0-9]*))%$/,
    angle: /^(-?(?:0|[1-9][0-9]*))deg$/,
    time: /^(-?(?:0|[1-9][0-9]*))ms$/,
  };
  const match = patterns[category]?.exec(value) || null;
  if (!match) return false;
  const number = Number(match[1]);
  const maximum = category === "length" && match[2] === "rem"
    ? 10_000
    : category === "percentage"
      ? 100_000
      : category === "angle"
        ? 360_000
        : category === "time"
          ? 3_600_000
          : 1_000_000;
  return Number.isSafeInteger(number) && Math.abs(number) <= maximum;
}

function exactKeys(value, expected, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    fail(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    fail(`${label} has unknown or missing fields`);
  }
}

function regionKind(value, label) {
  const kind = value?.kind ?? "list";
  if (!["list", "branch", "child"].includes(kind)) fail(`${label} has an invalid kind`);
  return kind;
}

function existingNodeAt(root, path, label) {
  if (!Array.isArray(path) || path.length > 64) fail(`${label} has an invalid DOM path`);
  let node = root;
  for (const index of path) {
    if (!Number.isInteger(index) || index < 0 || index >= (node?.childNodes?.length ?? 0)) {
      fail(`${label} does not match the existing DOM`);
    }
    node = node.childNodes[index];
  }
  return node;
}

function validDynamicKey(value, allowEmpty = false) {
  if (typeof value !== "string" || (!allowEmpty && value.length === 0)) return false;
  return new TextEncoder().encode(value).byteLength <= 1024;
}

function dynamicTemplateId(declaration, kind, templates, label) {
  const id = declaration?.dynamicTemplate ?? 0;
  if (!Number.isInteger(id) || id < 0 || (kind !== "list" && id !== 0)) {
    fail(`${label} has an invalid dynamic template`);
  }
  if (id === 0) return 0;
  const template = templates.get(id);
  if (
    !template ||
    !Array.isArray(template.events || []) ||
    (template.events || []).length !== 0 ||
    asMap(template.regions).size !== 0
  ) {
    fail(`${label} dynamic template must be event-free and region-free`);
  }
  return id;
}

function mapTemplateNodes(description, actual, mapped) {
  if (!description || !actual || mapped.has(description.node)) {
    fail("dynamic entry does not match its authenticated template");
  }
  mapped.set(description.node, actual);
  if (description.kind === "text") {
    if (typeof actual.setAttribute === "function" || (actual.childNodes?.length ?? 0) !== 0) {
      fail("dynamic entry text shape does not match its authenticated template");
    }
  } else if (description.kind === "element") {
    const tag = actual.localName || actual.tag;
    const children = actual.childNodes || [];
    if (tag !== description.tag || children.length !== (description.children || []).length) {
      fail("dynamic entry element shape does not match its authenticated template");
    }
    for (let index = 0; index < children.length; index += 1) {
      mapTemplateNodes(description.children[index], children[index], mapped);
    }
  } else {
    fail("dynamic template contains an unsupported node kind");
  }
}

function adoptExistingDom({
  root,
  plan,
  templates,
  declaredNodes,
  declaredRegions,
  eventClasses,
  eventPlans,
  subscriptionDescriptors,
  encoder,
  maximumStringBytes,
  nodes,
  regions,
  nodePlans,
}) {
  exactKeys(
    plan,
    [
      "version",
      "sequence",
      "inputSequence",
      "nodes",
      "regions",
      "events",
      "subscriptions",
    ],
    "resume plan",
  );
  if (plan.version !== 1) fail("resume plan version is unsupported");
  if (!Number.isSafeInteger(plan.sequence) || plan.sequence < 1) {
    fail("resume plan output sequence is invalid");
  }
  if (!Number.isSafeInteger(plan.inputSequence) || plan.inputSequence < 0) {
    fail("resume plan input sequence is invalid");
  }
  if (!Array.isArray(plan.nodes) || plan.nodes.length > declaredNodes.size) {
    fail("resume plan identifies more nodes than the artifact declares");
  }
  const adopted = new Set();
  for (const record of plan.nodes) {
    exactKeys(record, ["id", "path"], "resume node");
    if (!Number.isInteger(record.id) || !declaredNodes.has(record.id) || nodes.has(record.id)) {
      fail("resume plan names an unknown or duplicate node");
    }
    const node = existingNodeAt(root, record.path, `resume node ${record.id}`);
    if (adopted.has(node)) fail("resume plan aliases two node identities");
    adopted.add(node);
    nodes.set(record.id, node);
  }
  if (!Array.isArray(plan.regions) || plan.regions.length > declaredRegions.size) {
    fail("resume plan identifies more regions than the artifact declares");
  }
  for (const record of plan.regions) {
    exactKeys(
      record,
      [
        "id",
        "parent",
        "keys",
        ...(Object.hasOwn(record, "before") ? ["before"] : []),
        ...(Object.hasOwn(record, "child") ? ["child"] : []),
      ],
      "resume region",
    );
    const declaration = declaredRegions.get(record.id);
    const parent = nodes.get(record.parent);
    const kind = regionKind(declaration, "resume region");
    const dynamicTemplate = dynamicTemplateId(
      declaration,
      kind,
      templates,
      "resume region",
    );
    if (
      !Number.isInteger(record.id) ||
      !declaredRegions.has(record.id) ||
      regions.has(record.id) ||
      !parent ||
      !Array.isArray(record.keys) ||
      (record.before !== undefined && !Array.isArray(record.before)) ||
      (declaration?.parent !== undefined && declaration.parent !== record.parent) ||
      (declaration?.nodes !== undefined && !Array.isArray(declaration.nodes)) ||
      (declaration?.template !== undefined && !Number.isInteger(declaration.template))
    ) {
      fail("resume plan names an invalid or duplicate region");
    }
    if (
      Object.hasOwn(declaration || {}, "kind") &&
      declaration?.template !== undefined &&
      ((kind === "list" && declaration.template !== 0) ||
        (kind !== "list" && declaration.template <= 0))
    ) {
      fail("resume region has an invalid structural template");
    }
    const before = record.before || [];
    if (
      new Set(before).size !== before.length ||
      before.some((id) => {
        const node = nodes.get(id);
        return (
          !Number.isInteger(id) ||
          !declaredNodes.has(id) ||
          (node && node.parentNode !== parent)
        );
      })
    ) {
      fail("resume region has an invalid insertion order");
    }
    const keys = new Map();
    for (const item of record.keys) {
      exactKeys(
        item,
        ["key", "root", "nodes", ...(Object.hasOwn(item || {}, "source") ? ["source"] : [])],
        "resume region key",
      );
      const itemRoot = nodes.get(item.root);
      const identity = dynamicTemplate === 0 ? item.key : item.source;
      if (
        !Number.isInteger(item.key) ||
        item.key <= 0 ||
        (dynamicTemplate !== 0 && !validDynamicKey(item.source)) ||
        keys.has(identity) ||
        !itemRoot ||
        itemRoot.parentNode !== parent ||
        !Array.isArray(item.nodes) ||
        item.nodes.some((id) => !nodes.has(id))
      ) {
        fail("resume plan contains an invalid region key");
      }
      let localNodes = null;
      if (dynamicTemplate !== 0) {
        localNodes = new Map();
        mapTemplateNodes(templates.get(dynamicTemplate).root, itemRoot, localNodes);
      }
      keys.set(identity, {
        root: itemRoot,
        nodeIds: [...item.nodes],
        regionIds: [],
        dynamic: dynamicTemplate !== 0,
        template: dynamicTemplate || null,
        localNodes,
      });
    }
    if (kind !== "list" && keys.size !== 0) {
      fail("non-list resume region contains keyed entries");
    }
    let child = null;
    if (record.child !== null && record.child !== undefined) {
      exactKeys(record.child, ["root", "nodes"], "resume region child");
      const childRoot = nodes.get(record.child.root);
      if (
        kind === "list" ||
        !childRoot ||
        childRoot.parentNode !== parent ||
        !Array.isArray(record.child.nodes) ||
        record.child.nodes.some((id) => !nodes.has(id))
      ) {
        fail("resume plan contains an invalid region child");
      }
      child = { root: childRoot, nodeIds: [...record.child.nodes], regionIds: [] };
    }
    const ownedNodes = new Set([
      ...[...keys.values()].flatMap((entry) => entry.nodeIds),
      ...(child?.nodeIds || []),
    ]);
    if (before.some((id) => ownedNodes.has(id))) {
      fail("resume region insertion order aliases its owned subtree");
    }
    regions.set(record.id, {
      parent,
      keys,
      kind,
      child,
      before: [...before],
      template: Object.hasOwn(declaration || {}, "kind")
        ? declaration?.template || null
        : null,
      dynamicTemplate,
    });
  }
  for (const region of regions.values()) {
    for (const entry of region.keys.values()) {
      entry.regionIds = [...regions]
        .filter(([_id, nested]) => entry.nodeIds.some((id) => nodes.get(id) === nested.parent))
        .map(([id]) => id);
    }
    if (region.child) {
      region.child.regionIds = [...regions]
        .filter(([_id, nested]) =>
          region.child.nodeIds.some((id) => nodes.get(id) === nested.parent),
        )
        .map(([id]) => id);
    }
  }
  const inactiveNodes = new Set();
  for (const [id, region] of regions) {
    if (region.kind === "list" || region.child) continue;
    const declaration = declaredRegions.get(id);
    if (!Array.isArray(declaration?.nodes)) {
      fail("inactive structural region has no declared node ownership");
    }
    for (const nodeId of declaration.nodes) {
      if (!Number.isInteger(nodeId) || !declaredNodes.has(nodeId)) {
        fail("inactive structural region owns an undeclared node");
      }
      inactiveNodes.add(nodeId);
    }
  }
  for (const nodeId of declaredNodes.keys()) {
    if (!nodes.has(nodeId) && !inactiveNodes.has(nodeId)) {
      fail("resume plan omits a node outside an inactive structural region");
    }
    if (nodes.has(nodeId) && inactiveNodes.has(nodeId)) {
      fail("inactive structural region claims a live resume node");
    }
  }
  for (const [regionId, declaration] of declaredRegions) {
    if (regions.has(regionId)) continue;
    if (!Number.isInteger(declaration?.parent) || !inactiveNodes.has(declaration.parent)) {
      fail("resume plan omits a region whose parent remains live");
    }
  }
  if (!Array.isArray(plan.events)) fail("resume plan events must be a list");
  const installedEvents = new Set();
  for (const record of plan.events) {
    exactKeys(record, ["node", "eventClass", "eventPlan"], "resume event");
    const node = nodes.get(record.node);
    const eventClass = eventClasses.get(record.eventClass);
    const eventPlan = eventPlans.get(record.eventPlan);
    const identity = `${record.node}:${record.eventClass}`;
    if (
      !node ||
      !eventClass ||
      !eventPlan ||
      eventPlan.eventClass !== record.eventClass ||
      installedEvents.has(identity)
    ) {
      fail("resume plan contains an invalid or duplicate event binding");
    }
    installedEvents.add(identity);
    let perNode = nodePlans.get(node);
    if (!perNode) {
      perNode = new Map();
      nodePlans.set(node, perNode);
    }
    perNode.set(record.eventClass, {
      ...eventPlan,
      id: record.eventPlan,
      instance: eventPlan.instance,
    });
  }
  if (!Array.isArray(plan.subscriptions)) {
    fail("resume plan subscriptions must be a list");
  }
  const subscriptions = [];
  const subscriptionIds = new Set();
  for (const record of plan.subscriptions) {
    exactKeys(record, ["subscription", "descriptor", "request"], "resume subscription");
    if (
      !Number.isInteger(record.subscription) ||
      record.subscription <= 0 ||
      subscriptionIds.has(record.subscription) ||
      !Number.isInteger(record.descriptor) ||
      !subscriptionDescriptors.has(record.descriptor) ||
      typeof record.request !== "string" ||
      encoder.encode(record.request).byteLength > maximumStringBytes
    ) {
      fail("resume plan contains an invalid or duplicate subscription");
    }
    subscriptionIds.add(record.subscription);
    subscriptions.push(Object.freeze({
      subscription: record.subscription,
      descriptor: record.descriptor,
      request: record.request,
    }));
  }
  return Object.freeze({
    sequence: BigInt(plan.sequence),
    inputSequence: BigInt(plan.inputSequence),
    subscriptions: Object.freeze(subscriptions),
  });
}

function manifestForProtocol(manifest) {
  return {
    ...manifest,
    templates: asMap(manifest.templates),
    nodes: [...asMap(manifest.nodes).keys()],
    regions: [...asMap(manifest.regions).keys()],
    properties: [...asMap(manifest.properties).keys()],
    attributes: [...asMap(manifest.attributes).keys()],
    aria: [...asMap(manifest.aria).keys()],
    customProperties: [...asCustomPropertyMap(manifest.customProperties).keys()],
    eventClasses: [...asMap(manifest.eventClasses).keys()],
    eventPlans: [...asMap(manifest.eventPlans).keys()],
    effectDescriptors: [...asMap(manifest.effectDescriptors).keys()],
    subscriptionDescriptors: [...asMap(manifest.subscriptionDescriptors).keys()],
  };
}

function hex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function readDevelopmentMetadata(wasm, memory) {
  if (typeof wasm?.__glamour_dev_metadata !== "function") return null;
  const pointer = wasm.__glamour_dev_metadata();
  const view = new DataView(memory.buffer);
  if (!Number.isInteger(pointer) || pointer < 0 || pointer + 4 > view.byteLength) {
    fail("development metadata pointer is invalid");
  }
  const length = view.getUint32(pointer, true);
  if (length < 80 || length > 64 * 1024 || pointer + 4 + length > view.byteLength) {
    fail("development metadata length is invalid");
  }
  const bytes = new Uint8Array(memory.buffer, pointer + 4, length);
  if (new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, 4)) !== "WGDM") {
    fail("development metadata magic is invalid");
  }
  const payload = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const abi = payload.getUint16(4, true);
  const snapshotFormat = payload.getUint16(6, true);
  const protocolMajor = payload.getUint16(8, true);
  const protocolMinor = payload.getUint16(10, true);
  const fieldCount = payload.getUint32(12, true);
  if (
    ![1, 2].includes(abi) ||
    ![0, 1, 2].includes(snapshotFormat) ||
    protocolMajor !== GLAMOUR_PROTOCOL_MAJOR ||
    protocolMinor > GLAMOUR_PROTOCOL_MINOR
  ) {
    fail("development metadata version is unsupported");
  }
  if (80 + fieldCount > length) fail("development metadata field table is malformed");
  const fields = [...bytes.subarray(80, 80 + fieldCount)];
  if (fields.some((field) => ![1, 2, 3, 4].includes(field))) {
    fail("development metadata contains an unknown field kind");
  }
  if (snapshotFormat === 1 && fields.includes(4)) {
    fail("development snapshot metadata contains an aggregate field");
  }
  if (snapshotFormat === 2 && !fields.includes(4)) {
    fail("development aggregate snapshot metadata has no aggregate field");
  }
  const names = [];
  let cursor = 80 + fieldCount;
  if (abi === 1) {
    if (cursor !== length) fail("development metadata field table is malformed");
    names.push(...Array(fieldCount).fill(null));
  } else {
    const decoder = new TextDecoder("utf-8", { fatal: true });
    for (let index = 0; index < fieldCount; index += 1) {
      if (cursor + 2 > length) fail("development metadata field names are truncated");
      const nameLength = payload.getUint16(cursor, true);
      cursor += 2;
      if (nameLength > 1024 || cursor + nameLength > length) {
        fail("development metadata field name is invalid");
      }
      let name;
      try {
        name = nameLength === 0 ? null : decoder.decode(bytes.subarray(cursor, cursor + nameLength));
      } catch {
        fail("development metadata field name is not valid UTF-8");
      }
      names.push(name);
      cursor += nameLength;
    }
    if (cursor !== length) fail("development metadata field names have trailing bytes");
  }
  return Object.freeze({
    abi,
    snapshotFormat,
    protocolMajor,
    protocolMinor,
    modelSchema: hex(bytes.subarray(16, 48)),
    authorizationSchema: hex(bytes.subarray(48, 80)),
    fields: Object.freeze(fields),
    names: Object.freeze(names),
    snapshotBytes: snapshotFormat === 1 ? 40 + fieldCount * 8 : null,
  });
}

const DEVELOPMENT_TRACE_LIMIT = 128;
const DEVELOPMENT_DESCRIPTOR_SEMANTICS = new Set([
  "resource",
  "navigation",
  "timer",
  "port",
  "host-port",
  "storage",
  "worker",
  "custom",
]);

function monotonicMilliseconds() {
  const now = globalThis.performance?.now;
  return typeof now === "function" ? now.call(globalThis.performance) : Date.now();
}

function developmentDescriptorSemantic(descriptor) {
  return DEVELOPMENT_DESCRIPTOR_SEMANTICS.has(descriptor?.semantic)
    ? descriptor.semantic
    : null;
}

function developmentDescriptorSummaries(descriptors) {
  return Object.freeze(
    [...descriptors]
      .sort(([left], [right]) => Number(left) - Number(right))
      .map(([id, descriptor]) => {
        const semantic = developmentDescriptorSemantic(descriptor);
        return Object.freeze({
          id: Number(id),
          handler:
            typeof descriptor?.handler === "string" || Number.isInteger(descriptor?.handler)
              ? descriptor.handler
              : null,
          resultSchema: Number.isInteger(descriptor?.resultSchema)
            ? descriptor.resultSchema
            : null,
          ownerScope: Number.isInteger(descriptor?.ownerScope)
            ? descriptor.ownerScope
            : null,
          ...(semantic === null ? {} : { semantic }),
        });
      }),
  );
}

function developmentHostLifecycleSummary(event, effectDescriptors, subscriptionDescriptors) {
  const descriptors = event.kind === "effect" ? effectDescriptors : subscriptionDescriptors;
  const semantic = developmentDescriptorSemantic(descriptors.get(event.descriptor));
  return Object.freeze({
    ...event,
    ...(semantic === null ? {} : { semantic }),
  });
}

function developmentOperationSummary(operation, encoder) {
  const summary = { kind: operation.kind };
  for (const field of [
    "node",
    "sink",
    "attribute",
    "region",
    "template",
    "instance",
    "parentRegion",
    "before",
    "eventClass",
    "eventPlan",
    "cancellationKey",
    "descriptor",
    "subscription",
  ]) {
    if (Number.isInteger(operation[field])) summary[field] = operation[field];
  }
  if (Array.isArray(operation.slots)) {
    summary.slots = Object.freeze(operation.slots.map((slot) => slot.slot));
  }
  for (const field of ["value", "request", "key", "beforeKey"]) {
    if (typeof operation[field] === "string") {
      summary[`${field}Bytes`] = encoder.encode(operation[field]).byteLength;
    }
  }
  return Object.freeze(summary);
}

function developmentModelSummary(metadata) {
  const kinds = new Map([[1, "Int"], [2, "Float"], [3, "Bool"], [4, "Aggregate"]]);
  return Object.freeze({
    schema: metadata.modelSchema,
    snapshotFormat: metadata.snapshotFormat,
    fields: Object.freeze(metadata.fields.map((kind, index) => Object.freeze({
      index,
      ...(metadata.names[index] === null ? {} : { name: metadata.names[index] }),
      kind: kinds.get(kind),
      value: "<redacted>",
    }))),
  });
}

function developmentModelChanges(wasm, memory, metadata) {
  if (
    typeof wasm?.__glamour_dev_changes !== "function" ||
    typeof wasm?.__glamour_dev_changes_length !== "function"
  ) {
    fail("development model-change metadata is unavailable");
  }
  const pointer = wasm.__glamour_dev_changes();
  const length = wasm.__glamour_dev_changes_length();
  if (
    !Number.isInteger(pointer) ||
    !Number.isInteger(length) ||
    pointer < 0 ||
    length !== metadata.fields.length ||
    pointer + length > memory.buffer.byteLength
  ) {
    fail("development model-change range is invalid");
  }
  const changes = new Uint8Array(memory.buffer, pointer, length);
  if (changes.some((changed) => changed > 1)) {
    fail("development model-change bitmap is malformed");
  }
  return Object.freeze(
    [...changes]
      .map((changed, index) => changed === 1 ? index : null)
      .filter((index) => index !== null),
  );
}

function createStaticNode(document, description, pendingNodes) {
  if (!description || typeof description !== "object") fail("template contains an invalid node");
  let node;
  if (description.kind === "text") {
    node = document.createTextNode(String(description.text || ""));
  } else if (description.kind === "element") {
    if (!SAFE_ELEMENTS.has(description.tag)) {
      fail("template contains an invalid element name");
    }
    node = document.createElement(description.tag);
    for (const [name, value] of Object.entries(description.attributes || {})) {
      if (
        !/^[A-Za-z_:][A-Za-z0-9_.:-]*$/.test(name) ||
        /^on/i.test(name) ||
        name.toLowerCase() === "srcdoc"
      ) {
        fail("template contains an invalid static attribute");
      }
      node.setAttribute(name, String(value));
    }
    for (const child of description.children || []) {
      node.appendChild(createStaticNode(document, child, pendingNodes));
    }
  } else {
    fail(`template contains unknown node kind ${description.kind}`);
  }
  if (!Number.isInteger(description.node)) fail("template node is missing its numeric identity");
  if (pendingNodes.has(description.node)) fail(`template repeats node ${description.node}`);
  pendingNodes.set(description.node, node);
  return node;
}

function applyTemplateSlots({
  template,
  suppliedSlots,
  pendingNodes,
  properties,
  attributes,
  aria,
  customProperties,
  requireComplete,
}) {
  if (!Array.isArray(suppliedSlots)) fail("template slot payloads must be a list");
  const templateSlots = asMap(template.slots);
  const supplied = new Set();
  const actions = [];
  for (const value of suppliedSlots) {
    if (!Number.isInteger(value?.slot) || value.slot <= 0 || supplied.has(value.slot)) {
      fail("template slot payload contains an invalid or duplicate identity");
    }
    supplied.add(value.slot);
    const slot = templateSlots.get(value.slot);
    const node = slot && pendingNodes.get(slot.node);
    if (!slot || !node) fail(`template names unknown slot ${value.slot}`);
    let registry = null;
    if (slot.kind === "property") registry = properties;
    else if (slot.kind === "aria") registry = aria;
    else if (slot.kind === "attribute" || slot.kind === "boolean") registry = attributes;
    else if (slot.kind === "custom-property") registry = customProperties;
    if (slot.kind === "text") {
      actions.push(() => {
        node.textContent = value.value;
      });
    } else if (slot.kind === "class") {
      if (typeof node.setAttribute !== "function") {
        fail(`template slot ${value.slot} targets class on a non-element node`);
      }
      actions.push(() => node.setAttribute("class", value.value));
    } else if (registry) {
      const sink = registry.get(slot.sink);
      const name = slot.kind === "custom-property" ? sink?.name : sink;
      if (!name) fail(`template slot ${value.slot} names an unknown ${slot.kind} sink`);
      if (slot.kind === "property") {
        actions.push(() => {
          node[name] = value.value;
        });
      } else if (slot.kind === "boolean") {
        if (typeof node.setAttribute !== "function" || typeof node.removeAttribute !== "function") {
          fail(`template slot ${value.slot} targets a boolean attribute on a non-element node`);
        }
        if (value.value === "1") actions.push(() => node.setAttribute(name, ""));
        else if (value.value === "0") actions.push(() => node.removeAttribute(name));
        else fail(`template slot ${value.slot} contains a non-canonical boolean`);
      } else if (slot.kind === "custom-property") {
        if (typeof node.style?.setProperty !== "function") {
          fail(`template slot ${value.slot} targets a custom property on a non-element node`);
        }
        if (!validCustomPropertyValue(sink.category, value.value)) {
          fail(`template slot ${value.slot} contains an invalid ${sink.category} value`);
        }
        actions.push(() => node.style.setProperty(name, value.value));
      } else {
        if (typeof node.setAttribute !== "function") {
          fail(`template slot ${value.slot} targets an attribute on a non-element node`);
        }
        actions.push(() => node.setAttribute(name, value.value));
      }
    } else {
      fail(`template slot ${value.slot} has unsupported kind ${slot.kind}`);
    }
  }
  if (requireComplete && supplied.size !== templateSlots.size) {
    fail("template slot payload does not exactly cover the authenticated slot table");
  }
  return actions;
}

function createStaticRegions(template, pendingNodes, stagedRegions, templates) {
  const pendingRegions = new Map();
  const parentIds = new Map();
  for (const [regionIdText, description] of Object.entries(template.regions || {})) {
    const regionId = Number(regionIdText);
    const legacy = Number.isInteger(description);
    if (!legacy) {
      exactKeys(
        description,
        [
          "parent",
          "keys",
          ...(Object.hasOwn(description, "kind") ? ["kind"] : []),
          ...(Object.hasOwn(description, "before") ? ["before"] : []),
          ...(Object.hasOwn(description, "template") ? ["template"] : []),
          ...(Object.hasOwn(description, "dynamicTemplate") ? ["dynamicTemplate"] : []),
          ...(Object.hasOwn(description, "child") ? ["child"] : []),
        ],
        "template region",
      );
    }
    const parentId = legacy ? description : description.parent;
    const parent = pendingNodes.get(parentId);
    const kind = legacy ? "list" : regionKind(description, "template region");
    const dynamicTemplate = legacy
      ? 0
      : dynamicTemplateId(description, kind, templates, "template region");
    if (
      !Number.isInteger(regionId) ||
      regionId <= 0 ||
      !Number.isInteger(parentId) ||
      !parent ||
      (!legacy && !Array.isArray(description.keys)) ||
      (!legacy && description.before !== undefined && !Array.isArray(description.before)) ||
      (!legacy && description.template !== undefined && !Number.isInteger(description.template)) ||
      stagedRegions.has(regionId)
    ) {
      fail("template contains an invalid or duplicate region");
    }
    if (
      !legacy &&
      description.template !== undefined &&
      ((kind === "list" && description.template !== 0) ||
        (kind !== "list" && description.template <= 0))
    ) {
      fail("template region has an invalid structural template");
    }
    const before = legacy ? [] : description.before || [];
    if (
      new Set(before).size !== before.length ||
      before.some((id) => {
        const node = pendingNodes.get(id);
        return !Number.isInteger(id) || !node || node.parentNode !== parent;
      })
    ) {
      fail("template region has an invalid insertion order");
    }
    let child = null;
    if (!legacy && description.child !== null && description.child !== undefined) {
      exactKeys(description.child, ["root", "nodes"], "template region child");
      const childRoot = pendingNodes.get(description.child.root);
      if (
        kind === "list" ||
        !childRoot ||
        !Array.isArray(description.child.nodes) ||
        description.child.nodes.some((id) => !pendingNodes.has(id))
      ) {
        fail("template contains an invalid region child");
      }
      child = { root: childRoot, nodeIds: [...description.child.nodes], regionIds: [] };
    }
    const region = {
      parent,
      keys: new Map(),
      kind,
      child,
      before: [...before],
      template: legacy ? null : description.template || null,
      dynamicTemplate,
    };
    pendingRegions.set(regionId, region);
    parentIds.set(regionId, parentId);
    stagedRegions.set(regionId, region);
  }
  for (const [regionIdText, description] of Object.entries(template.regions || {})) {
    if (Number.isInteger(description)) continue;
    const region = pendingRegions.get(Number(regionIdText));
    if (region.kind !== "list" && description.keys.length !== 0) {
      fail("non-list template region contains keyed entries");
    }
    for (const item of description.keys) {
      exactKeys(
        item,
        ["key", "root", "nodes", ...(Object.hasOwn(item || {}, "source") ? ["source"] : [])],
        "template region key",
      );
      const root = pendingNodes.get(item.root);
      const identity = region.dynamicTemplate === 0 ? item.key : item.source;
      if (
        !Number.isInteger(item.key) ||
        item.key <= 0 ||
        (region.dynamicTemplate !== 0 && !validDynamicKey(item.source)) ||
        region.keys.has(identity) ||
        !root ||
        !Array.isArray(item.nodes) ||
        item.nodes.some((id) => !pendingNodes.has(id))
      ) {
        fail("template contains an invalid region key");
      }
      let localNodes = null;
      if (region.dynamicTemplate !== 0) {
        localNodes = new Map();
        mapTemplateNodes(templates.get(region.dynamicTemplate).root, root, localNodes);
      }
      region.keys.set(identity, {
        root,
        nodeIds: [...item.nodes],
        regionIds: [],
        dynamic: region.dynamicTemplate !== 0,
        template: region.dynamicTemplate || null,
        localNodes,
      });
    }
  }
  for (const region of pendingRegions.values()) {
    const ownedNodes = new Set([
      ...[...region.keys.values()].flatMap((entry) => entry.nodeIds),
      ...(region.child?.nodeIds || []),
    ]);
    if (region.before.some((id) => ownedNodes.has(id))) {
      fail("template region insertion order aliases its owned subtree");
    }
    for (const entry of region.keys.values()) {
      entry.regionIds = [...parentIds]
        .filter(([_id, parent]) => entry.nodeIds.includes(parent))
        .map(([id]) => id);
    }
    if (region.child) {
      region.child.regionIds = [...parentIds]
        .filter(([_id, parent]) => region.child.nodeIds.includes(parent))
        .map(([id]) => id);
    }
  }
  return pendingRegions;
}

function createStaticEvents(template, pendingNodes, eventClasses, eventPlans) {
  if (!Array.isArray(template.events || [])) fail("template events must be a list");
  const pendingEvents = [];
  const identities = new Set();
  for (const record of template.events || []) {
    exactKeys(record, ["node", "eventClass", "eventPlan"], "template event");
    const node = pendingNodes.get(record.node);
    const eventClass = eventClasses.get(record.eventClass);
    const eventPlan = eventPlans.get(record.eventPlan);
    const identity = `${record.node}:${record.eventClass}`;
    if (
      !node ||
      !eventClass ||
      !eventPlan ||
      eventPlan.eventClass !== record.eventClass ||
      identities.has(identity)
    ) {
      fail("template contains an invalid or duplicate event binding");
    }
    identities.add(identity);
    pendingEvents.push({ node, eventClass: record.eventClass, eventPlan: record.eventPlan });
  }
  return pendingEvents;
}

function installStaticEvents(pendingEvents, eventPlans, nodePlans) {
  for (const binding of pendingEvents) {
    const plan = eventPlans.get(binding.eventPlan);
    let perNode = nodePlans.get(binding.node);
    if (!perNode) {
      perNode = new Map();
      nodePlans.set(binding.node, perNode);
    }
    perNode.set(binding.eventClass, {
      ...plan,
      id: binding.eventPlan,
      instance: plan.instance,
    });
  }
}

function insertStructuralRoot(region, root, nodes) {
  const before = (region.before || [])
    .map((id) => nodes.get(id))
    .find((node) => node?.parentNode === region.parent);
  if (before) region.parent.insertBefore(root, before);
  else region.parent.appendChild(root);
}

function composedPath(event, root) {
  if (event && typeof event.composedPath === "function") {
    const path = event.composedPath();
    const rootIndex = path.indexOf(root);
    return rootIndex >= 0 ? path.slice(0, rootIndex + 1) : [];
  }
  const path = [];
  let node = event?.target || null;
  while (node) {
    path.push(node);
    if (node === root) break;
    node = node.parentNode;
  }
  return path.at(-1) === root ? path : [];
}

export async function mountOptimized(wasmBytes, initialRoot, options = {}) {
  if (!initialRoot || typeof initialRoot.addEventListener !== "function") {
    fail("root must be an event-capable element");
  }
  let root = initialRoot;
  const manifest = options.manifest;
  if (!manifest) fail("a build manifest is required");
  const document = options.document || globalThis.document;
  if (!document) fail("a document implementation is required");
  const templates = asMap(manifest.templates);
  const eventClasses = asMap(manifest.eventClasses);
  const eventPlans = asMap(manifest.eventPlans);
  const ownerInstances = asMap(manifest.ownerInstances);
  if (ownerInstances.size > 0) {
    const allowedKinds = new Set(["root", "key", "branch", "child", "route", "resource"]);
    for (const [instance, owner] of ownerInstances) {
      if (
        !Number.isInteger(instance) ||
        instance <= 0 ||
        !owner ||
        typeof owner !== "object" ||
        !Number.isInteger(owner.declaration) ||
        owner.declaration <= 0 ||
        !allowedKinds.has(owner.kind)
      ) {
        fail("owner instance registry is invalid");
      }
    }
    for (const plan of eventPlans.values()) {
      const owner = ownerInstances.get(plan?.instance);
      if (!owner || plan?.ownerScope !== owner.declaration) {
        fail("event plan does not belong to its authenticated owner instance");
      }
    }
  } else if (manifest.registryId !== undefined) {
    if (!Number.isInteger(manifest.registryId) || manifest.registryId <= 0) {
      fail("manifest registry identity is invalid");
    }
    for (const plan of eventPlans.values()) {
      if (plan?.instance !== manifest.registryId) {
        fail("event plan does not belong to the manifest registry");
      }
    }
  }
  const properties = asNameMap(manifest.properties, "property");
  const attributes = asNameMap(manifest.attributes, "attribute");
  const aria = asNameMap(manifest.aria, "ARIA");
  const customProperties = asCustomPropertyMap(manifest.customProperties);
  const effectDescriptors = asMap(manifest.effectDescriptors);
  const subscriptionDescriptors = asMap(manifest.subscriptionDescriptors);
  const declaredNodes = asMap(manifest.nodes);
  const declaredRegions = asMap(manifest.regions);
  const encoder = new TextEncoder();
  const protocolManifest = manifestForProtocol(manifest);
  const nodes = new Map();
  const regions = new Map();
  const nodePlans = new Map();
  const resumeMode = options.resume === true;
  const startupBarrier = manifest.features?.startupBarrier === true;
  if (options.resume !== undefined && !resumeMode) fail("resume must be true when present");
  const replaceRoot = options.replaceRoot === true;
  if (options.replaceRoot !== undefined && !replaceRoot) fail("replaceRoot must be true when present");
  if (resumeMode && replaceRoot) fail("resumed applications cannot replace their adopted root");
  if (replaceRoot && typeof root.replaceChildren !== "function") {
    fail("fresh replacement requires an atomic root.replaceChildren operation");
  }
  if (resumeMode && !manifest.resume) fail("an authenticated resume plan is required");
  const resumed = resumeMode
    ? adoptExistingDom({
        root,
        plan: manifest.resume,
        templates,
        declaredNodes,
        declaredRegions,
        eventClasses,
        eventPlans,
        subscriptionDescriptors,
        encoder,
        maximumStringBytes: Math.min(
          manifest.limits?.maxStringBytes ?? 1024 * 1024,
          manifest.limits?.maxPayloadBytes ?? 2 * 1024 * 1024,
        ),
        nodes,
        regions,
        nodePlans,
      })
    : null;
  const validator = createOutputValidator(protocolManifest, resumed?.sequence ?? 0n);
  const makeRuntime = options.instantiate || instantiateWitchy;
  const runtime = await makeRuntime(wasmBytes, options.instantiateOptions || {});
  const { instance, memory } = runtime;
  const wasm = instance?.exports;
  for (const name of [
    "__glamour_protocol_version",
    "__glamour_input_reserve",
    "__glamour_init",
    "__glamour_dispatch",
    "__glamour_output_length",
    "__glamour_output_release",
    "__glamour_dispose",
  ]) {
    if (typeof wasm?.[name] !== "function") fail(`Wasm is missing ${name}`);
  }
  if (resumeMode && typeof wasm?.__glamour_resume !== "function") {
    fail("Wasm is missing __glamour_resume");
  }
  const wasmProtocol = wasm.__glamour_protocol_version();
  const wasmProtocolMajor = wasmProtocol >>> 16;
  const wasmProtocolMinor = wasmProtocol & 0xffff;
  if (
    wasmProtocolMajor !== GLAMOUR_PROTOCOL_MAJOR ||
    wasmProtocolMinor > GLAMOUR_PROTOCOL_MINOR
  ) {
    fail("Wasm protocol version does not match the host");
  }
  const developmentMetadata = readDevelopmentMetadata(wasm, memory);
  const developmentTracing =
    manifest.features?.mode === "development" && developmentMetadata !== null;
  const developmentActivation = options.restoreSnapshot !== undefined
    ? "hot-swap"
    : resumeMode
      ? "resume"
      : "fresh";
  const developmentTimeline = [];
  const developmentInputs = [];
  const developmentReplayFrames = [];
  const developmentHostLifecycle = [];
  const developmentDescriptors = developmentTracing
    ? Object.freeze({
        effects: developmentDescriptorSummaries(effectDescriptors),
        subscriptions: developmentDescriptorSummaries(subscriptionDescriptors),
      })
    : null;

  const listeners = new Map();
  let effectHost;
  let progressiveForms;
  let frameHost;
  let disposed = false;
  let deferredActivation = options.deferActivation === true;
  const activationQueue = [];
  let inputSequence = resumed?.inputSequence ?? 0n;
  const runtimeStats = {
    frames: 0,
    operations: 0,
    outputBytes: 0,
  };

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    validator.dispose();
    effectHost?.dispose();
    progressiveForms?.dispose();
    frameHost?.dispose();
    for (const [name, registration] of listeners) {
      root.removeEventListener(name, registration.listener, registration.capture);
    }
    listeners.clear();
    activationQueue.length = 0;
    nodePlans.clear();
    nodes.clear();
    regions.clear();
    try {
      wasm.__glamour_dispose();
    } catch {
      // A trapped Wasm instance may reject cleanup; host authority is already gone.
    }
  };

  const reportAsyncError = (error) => {
    dispose();
    if (typeof options.onError === "function") options.onError(error);
  };

  const callWithFrame = (name, frame) => {
    if (disposed) fail("application is disposed");
    if (developmentTracing && developmentReplayFrames.length < DEVELOPMENT_TRACE_LIMIT) {
      const privateFrame = frame.slice();
      developmentReplayFrames.push(Object.freeze({ name, frame: privateFrame }));
      developmentInputs.push(Object.freeze({
        ordinal: developmentInputs.length,
        name,
        byteLength: privateFrame.byteLength,
        digest: developmentFrameDigest(privateFrame),
      }));
      if (developmentInputs.length > DEVELOPMENT_TRACE_LIMIT) developmentInputs.shift();
    }
    const pointer = wasm.__glamour_input_reserve(frame.byteLength);
    if (!Number.isInteger(pointer) || pointer < 0 || pointer + frame.byteLength > memory.buffer.byteLength) {
      dispose();
      fail("Wasm reserved an invalid input range");
    }
    new Uint8Array(memory.buffer).set(frame, pointer);
    let outputPointer;
    try {
      outputPointer = wasm[name](pointer, frame.byteLength);
      const outputLength = wasm.__glamour_output_length();
      if (
        !Number.isInteger(outputPointer) ||
        !Number.isInteger(outputLength) ||
        outputPointer < 0 ||
        outputLength < 0 ||
        outputPointer + outputLength > memory.buffer.byteLength
      ) {
        fail("Wasm returned an invalid output range");
      }
      return new Uint8Array(memory.buffer).slice(outputPointer, outputPointer + outputLength);
    } catch (error) {
      dispose();
      throw error;
    } finally {
      try {
        wasm.__glamour_output_release();
      } catch {
        dispose();
      }
    }
  };

  const callWithFrameWithoutOutput = (name, frame) => {
    if (disposed) fail("application is disposed");
    const pointer = wasm.__glamour_input_reserve(frame.byteLength);
    if (!Number.isInteger(pointer) || pointer < 0 || pointer + frame.byteLength > memory.buffer.byteLength) {
      dispose();
      fail("Wasm reserved an invalid input range");
    }
    new Uint8Array(memory.buffer).set(frame, pointer);
    try {
      const outputPointer = wasm[name](pointer, frame.byteLength);
      if (outputPointer !== 0 || wasm.__glamour_output_length() !== 0) {
        fail("Wasm resume attempted to replay initial output");
      }
    } catch (error) {
      dispose();
      throw error;
    } finally {
      try {
        wasm.__glamour_output_release();
      } catch {
        dispose();
      }
    }
  };

  const dispatchCompletion = ({
    source,
    instance,
    generation,
    descriptor,
    resultSchema,
    status,
    value,
  }) => {
    if (disposed) return;
    try {
      const resultLimit = Math.min(
        manifest.limits?.maxStringBytes ?? 1024 * 1024,
        manifest.limits?.maxPayloadBytes ?? 2 * 1024 * 1024,
        Math.max(0, (manifest.limits?.maxFrameBytes ?? 4 * 1024 * 1024) - 88),
        manifest.limits?.maxCompletionBytes ?? 60 * 1024,
      );
      const descriptors = source === CompletionSource.Effect
        ? effectDescriptors
        : subscriptionDescriptors;
      const result = encodeCompletionResult({
        descriptor: manifest.features?.mode === "development"
          ? null
          : descriptors.get(descriptor),
        status,
        value,
        maxBytes: resultLimit,
      });
      const completion = encodeEffectCompletionFrame({
        appId: manifest.appId,
        buildId: manifest.buildId,
        sequence: inputSequence,
        source,
        instance,
        generation,
        descriptor,
        resultSchema,
        status: result.status,
        payload: result.payload,
      });
      inputSequence += 1n;
      acceptOutput(callWithFrame("__glamour_dispatch", completion));
    } catch (error) {
      reportAsyncError(error);
    }
  };

  const dispatchActionLifecycle = (state, action) => {
    if (disposed || state.phase === "Validating") return;
    try {
      let frame;
      if (state.phase === "Submitting") {
        const fields = [];
        const values = new Map(state.values.map(({ name, value }) => [name, value]));
        const byteLimit = Math.min(
          manifest.limits?.maxPayloadBytes ?? 2 * 1024 * 1024,
          manifest.limits?.maxActionBytes ?? 56 * 1024,
          Math.max(0, (manifest.limits?.maxFrameBytes ?? 4 * 1024 * 1024) - 72),
          Math.max(0, 64 * 1024 - 72 - action.fields.length * 16),
        );
        let bytes = 0;
        for (const field of action.fields) {
          if (field.kind === "secret" || !values.has(field.name)) continue;
          const kind = {
            text: ActionFieldKind.Text,
            email: ActionFieldKind.Email,
            number: ActionFieldKind.Number,
            checkbox: ActionFieldKind.Checkbox,
          }[field.kind];
          const value = values.get(field.name);
          bytes += encoder.encode(value).byteLength;
          if (bytes > byteLimit) fail("progressive action input exceeds its byte limit");
          fields.push({ ordinal: field.ordinal, kind, value });
        }
        frame = encodeActionInputFrame({
          appId: manifest.appId,
          buildId: manifest.buildId,
          sequence: inputSequence,
          inputSchema: action.inputSchema,
          generation: state.generation,
          fields,
        });
      } else {
        const status = state.phase === "Succeeded"
          ? ActionCompletionStatus.Succeeded
          : state.phase === "Cancelled"
            ? ActionCompletionStatus.Cancelled
            : {
                validation: ActionCompletionStatus.ValidationFailed,
                server: ActionCompletionStatus.ServerFailed,
                network: ActionCompletionStatus.NetworkFailed,
                "form-data": ActionCompletionStatus.InvalidSubmission,
              }[state.reason];
        if (status === undefined) return;
        frame = encodeActionCompletionFrame({
          appId: manifest.appId,
          buildId: manifest.buildId,
          sequence: inputSequence,
          resultSchema: action.resultSchema,
          generation: state.generation,
          status,
          httpStatus: state.result?.status ?? 0,
        });
      }
      inputSequence += 1n;
      acceptOutput(callWithFrame("__glamour_dispatch", frame));
    } catch (error) {
      reportAsyncError(error);
      throw error;
    }
  };

  effectHost = createEffectHost({
    effectDescriptors,
    subscriptionDescriptors,
    effectHandlers: options.effectHandlers,
    subscriptionHandlers: options.subscriptionHandlers,
    complete: dispatchCompletion,
    observeLifecycle: developmentTracing
      ? (event) => {
          developmentHostLifecycle.push(
            developmentHostLifecycleSummary(
              event,
              effectDescriptors,
              subscriptionDescriptors,
            ),
          );
          if (developmentHostLifecycle.length > DEVELOPMENT_TRACE_LIMIT) {
            developmentHostLifecycle.shift();
          }
        }
      : undefined,
  });

  const planFrame = (frame) => {
    const dom = [];
    const effectCancellations = [];
    const subscriptionCancellations = [];
    const effectStarts = [];
    const subscriptionStarts = [];
    const effectInstances = new Set();
    const subscriptionIds = new Set();
    const pendingInstances = new Set();
    const dynamicSlotUpdates = new Set();
    const replacingInitialRoot = replaceRoot && nodes.size === 0 && regions.size === 0;
    let rootMounts = 0;
    const stagedNodes = new Map(nodes);
    const stagedRegions = new Map(
      [...regions].map(([id, region]) => [id, { ...region, keys: new Map(region.keys) }]),
    );
    for (const operation of frame.operations) {
      switch (operation.kind) {
        case "mount": {
          rootMounts += 1;
          if (replacingInitialRoot && rootMounts !== 1) {
            fail("fresh replacement must contain exactly one root mount");
          }
          const template = templates.get(operation.template);
          if (!template) fail(`missing template ${operation.template}`);
          if (pendingInstances.has(operation.instance)) {
            fail(`duplicate template instance ${operation.instance}`);
          }
          pendingInstances.add(operation.instance);
          const pendingNodes = new Map();
          const staticRoot = createStaticNode(document, template.root, pendingNodes);
          for (const nodeId of pendingNodes.keys()) {
            if (stagedNodes.has(nodeId)) fail(`node ${nodeId} is already live`);
          }
          for (const [nodeId, node] of pendingNodes) {
            stagedNodes.set(nodeId, node);
          }
          dom.push(...applyTemplateSlots({
            template,
            suppliedSlots: operation.slots,
            pendingNodes,
            properties,
            attributes,
            aria,
            customProperties,
            requireComplete: frame.minor >= 1,
          }));
          const pendingRegions = createStaticRegions(
            template,
            pendingNodes,
            stagedRegions,
            templates,
          );
          const pendingEvents = createStaticEvents(
            template,
            pendingNodes,
            eventClasses,
            eventPlans,
          );
          dom.push(() => {
            if (replacingInitialRoot) root.replaceChildren(staticRoot);
            else root.appendChild(staticRoot);
            for (const [id, node] of pendingNodes) nodes.set(id, node);
            for (const [id, region] of pendingRegions) regions.set(id, region);
            installStaticEvents(pendingEvents, eventPlans, nodePlans);
          });
          break;
        }
        case "set_text": {
          const node = stagedNodes.get(operation.node);
          if (!node) fail(`SetText names non-live node ${operation.node}`);
          dom.push(() => {
            node.textContent = operation.value;
          });
          break;
        }
        case "set_property": {
          const node = stagedNodes.get(operation.node);
          const name = properties.get(operation.sink);
          if (!node || !name) fail("SetProperty names a non-live node or property");
          dom.push(() => {
            node[name] = operation.value;
          });
          break;
        }
        case "set_attribute":
        case "set_aria": {
          const node = stagedNodes.get(operation.node);
          const name = (operation.kind === "set_aria" ? aria : attributes).get(operation.sink);
          if (!node || !name) fail("attribute patch names a non-live node or sink");
          dom.push(() => node.setAttribute(name, operation.value));
          break;
        }
        case "set_custom_property": {
          const node = stagedNodes.get(operation.node);
          const descriptor = customProperties.get(operation.sink);
          if (!node || !descriptor || typeof node.style?.setProperty !== "function") {
            fail("custom-property patch names a non-live element or sink");
          }
          if (!validCustomPropertyValue(descriptor.category, operation.value)) {
            fail(`custom-property patch contains an invalid ${descriptor.category} value`);
          }
          dom.push(() => node.style.setProperty(descriptor.name, operation.value));
          break;
        }
        case "remove_attribute": {
          const node = stagedNodes.get(operation.node);
          const name = attributes.get(operation.attribute);
          if (!node || !name) fail("RemoveAttribute names a non-live node or attribute");
          dom.push(() => node.removeAttribute(name));
          break;
        }
        case "set_boolean_attribute": {
          const node = stagedNodes.get(operation.node);
          const name = attributes.get(operation.attribute);
          if (!node || !name) fail("boolean patch names a non-live node or attribute");
          dom.push(() => {
            if (operation.enabled) node.setAttribute(name, "");
            else node.removeAttribute(name);
          });
          break;
        }
        case "set_class_list": {
          const node = stagedNodes.get(operation.node);
          if (!node) fail(`SetClassList names non-live node ${operation.node}`);
          dom.push(() => node.setAttribute("class", operation.value));
          break;
        }
        case "set_event_plan": {
          const node = stagedNodes.get(operation.node);
          const plan = eventPlans.get(operation.eventPlan);
          const eventClass = eventClasses.get(operation.eventClass);
          if (!node || !plan || !eventClass || plan.eventClass !== operation.eventClass) {
            fail("event patch names an incompatible node, class, or plan");
          }
          dom.push(() => {
            let perNode = nodePlans.get(node);
            if (!perNode) {
              perNode = new Map();
              nodePlans.set(node, perNode);
            }
            perNode.set(operation.eventClass, {
              ...plan,
              id: operation.eventPlan,
              instance: plan.instance,
            });
          });
          break;
        }
        case "remove_event_plan": {
          const node = stagedNodes.get(operation.node);
          if (!node) fail(`RemoveEventPlan names non-live node ${operation.node}`);
          dom.push(() => nodePlans.get(node)?.delete(operation.eventClass));
          break;
        }
        case "enter_branch":
        case "mount_child": {
          const expectedKind = operation.kind === "enter_branch" ? "branch" : "child";
          const region = stagedRegions.get(operation.region);
          const template = templates.get(operation.template);
          if (
            !region ||
            region.kind !== expectedKind ||
            region.child ||
            !template ||
            (region.template !== null && region.template !== operation.template)
          ) {
            fail(`${operation.kind} names an occupied or incompatible region or template`);
          }
          if (pendingInstances.has(operation.instance)) {
            fail(`duplicate template instance ${operation.instance}`);
          }
          pendingInstances.add(operation.instance);
          const pendingNodes = new Map();
          const childRoot = createStaticNode(document, template.root, pendingNodes);
          for (const nodeId of pendingNodes.keys()) {
            if (stagedNodes.has(nodeId)) fail(`node ${nodeId} is already live`);
          }
          for (const [nodeId, node] of pendingNodes) stagedNodes.set(nodeId, node);
          dom.push(...applyTemplateSlots({
            template,
            suppliedSlots: operation.slots,
            pendingNodes,
            properties,
            attributes,
            aria,
            customProperties,
            requireComplete: frame.minor >= 1,
          }));
          const pendingRegions = createStaticRegions(
            template,
            pendingNodes,
            stagedRegions,
            templates,
          );
          const pendingEvents = createStaticEvents(
            template,
            pendingNodes,
            eventClasses,
            eventPlans,
          );
          const entry = {
            root: childRoot,
            nodeIds: [...pendingNodes.keys()],
            regionIds: [...pendingRegions.keys()],
          };
          region.child = entry;
          dom.push(() => {
            const liveRegion = regions.get(operation.region);
            if (!liveRegion || liveRegion.child) {
              fail(`${operation.kind} region changed during application`);
            }
            insertStructuralRoot(liveRegion, childRoot, nodes);
            liveRegion.child = entry;
            for (const [id, node] of pendingNodes) nodes.set(id, node);
            for (const [id, nestedRegion] of pendingRegions) regions.set(id, nestedRegion);
            installStaticEvents(pendingEvents, eventPlans, nodePlans);
          });
          break;
        }
        case "leave_branch":
        case "unmount_child": {
          const expectedKind = operation.kind === "leave_branch" ? "branch" : "child";
          const region = stagedRegions.get(operation.region);
          const entry = region?.child;
          if (!region || region.kind !== expectedKind || !entry) {
            fail(`${operation.kind} names an empty or incompatible region`);
          }
          region.child = null;
          for (const nodeId of entry.nodeIds) stagedNodes.delete(nodeId);
          for (const regionId of entry.regionIds || []) stagedRegions.delete(regionId);
          dom.push(() => {
            const liveRegion = regions.get(operation.region);
            if (!liveRegion?.child) fail(`${operation.kind} region changed during application`);
            liveRegion.parent.removeChild(entry.root);
            liveRegion.child = null;
            for (const nodeId of entry.nodeIds) {
              nodePlans.delete(nodes.get(nodeId));
              nodes.delete(nodeId);
            }
            for (const regionId of entry.regionIds || []) regions.delete(regionId);
          });
          break;
        }
        case "list_insert": {
          const region = stagedRegions.get(operation.region);
          const template = templates.get(operation.template);
          if (!region || region.kind !== "list" || region.dynamicTemplate !== 0 || !template) {
            fail("ListInsert names a non-live list region or template");
          }
          if (region.keys.has(operation.key)) fail(`duplicate key ${operation.key}`);
          const beforeEntry =
            operation.beforeKey === 0 ? null : region.keys.get(operation.beforeKey);
          const before = beforeEntry?.root || null;
          if (operation.beforeKey !== 0 && !before) {
            fail(`ListInsert names missing before-key ${operation.beforeKey}`);
          }
          const pendingNodes = new Map();
          const itemRoot = createStaticNode(document, template.root, pendingNodes);
          for (const nodeId of pendingNodes.keys()) {
            if (stagedNodes.has(nodeId)) fail(`node ${nodeId} is already live`);
          }
          for (const [nodeId, node] of pendingNodes) stagedNodes.set(nodeId, node);
          dom.push(...applyTemplateSlots({
            template,
            suppliedSlots: operation.slots,
            pendingNodes,
            properties,
            attributes,
            aria,
            customProperties,
            requireComplete: frame.minor >= 1,
          }));
          const pendingRegions = createStaticRegions(
            template,
            pendingNodes,
            stagedRegions,
            templates,
          );
          const pendingEvents = createStaticEvents(
            template,
            pendingNodes,
            eventClasses,
            eventPlans,
          );
          const entry = {
            root: itemRoot,
            nodeIds: [...pendingNodes.keys()],
            regionIds: [...pendingRegions.keys()],
          };
          region.keys.set(operation.key, entry);
          dom.push(() => {
            const liveRegion = regions.get(operation.region);
            if (!liveRegion) fail("ListInsert region disappeared during application");
            if (before) liveRegion.parent.insertBefore(itemRoot, before);
            else liveRegion.parent.appendChild(itemRoot);
            liveRegion.keys.set(operation.key, entry);
            for (const [id, node] of pendingNodes) nodes.set(id, node);
            for (const [id, nestedRegion] of pendingRegions) regions.set(id, nestedRegion);
            installStaticEvents(pendingEvents, eventPlans, nodePlans);
          });
          break;
        }
        case "list_move": {
          const region = stagedRegions.get(operation.region);
          const item = region?.keys.get(operation.key)?.root;
          const before =
            operation.beforeKey === 0 ? null : region?.keys.get(operation.beforeKey)?.root;
          if (
            !region ||
            region.kind !== "list" ||
            region.dynamicTemplate !== 0 ||
            !item ||
            (operation.beforeKey !== 0 && !before)
          ) {
            fail("ListMove names a non-live region, key, or before-key");
          }
          dom.push(() => {
            if (item === before) return;
            if (before) region.parent.insertBefore(item, before);
            else region.parent.appendChild(item);
          });
          break;
        }
        case "list_remove": {
          const region = stagedRegions.get(operation.region);
          const entry = region?.keys.get(operation.key);
          if (!region || region.kind !== "list" || region.dynamicTemplate !== 0 || !entry) {
            fail("ListRemove names a non-live list region or key");
          }
          region.keys.delete(operation.key);
          for (const nodeId of entry.nodeIds) stagedNodes.delete(nodeId);
          for (const regionId of entry.regionIds || []) stagedRegions.delete(regionId);
          dom.push(() => {
            region.parent.removeChild(entry.root);
            regions.get(operation.region)?.keys.delete(operation.key);
            for (const nodeId of entry.nodeIds) {
              nodePlans.delete(nodes.get(nodeId));
              nodes.delete(nodeId);
            }
            for (const regionId of entry.regionIds || []) regions.delete(regionId);
          });
          break;
        }
        case "list_insert_dynamic": {
          const region = stagedRegions.get(operation.region);
          const template = templates.get(operation.template);
          if (
            !region ||
            region.kind !== "list" ||
            region.dynamicTemplate === 0 ||
            region.dynamicTemplate !== operation.template ||
            !template
          ) {
            fail("ListInsertDynamic names an incompatible region or template");
          }
          if (!validDynamicKey(operation.key) || !validDynamicKey(operation.beforeKey, true)) {
            fail("ListInsertDynamic contains an invalid key");
          }
          if (region.keys.has(operation.key)) {
            fail("ListInsertDynamic names a duplicate key");
          }
          const beforeEntry =
            operation.beforeKey === "" ? null : region.keys.get(operation.beforeKey);
          const before = beforeEntry?.root || null;
          if (operation.beforeKey !== "" && !before) {
            fail("ListInsertDynamic names a missing before-key");
          }
          const pendingNodes = new Map();
          const itemRoot = createStaticNode(document, template.root, pendingNodes);
          dom.push(...applyTemplateSlots({
            template,
            suppliedSlots: operation.slots,
            pendingNodes,
            properties,
            attributes,
            aria,
            customProperties,
            requireComplete: true,
          }));
          const entry = {
            root: itemRoot,
            nodeIds: [],
            regionIds: [],
            dynamic: true,
            template: operation.template,
            localNodes: pendingNodes,
          };
          region.keys.set(operation.key, entry);
          dom.push(() => {
            const liveRegion = regions.get(operation.region);
            if (!liveRegion || liveRegion.dynamicTemplate !== operation.template) {
              fail("ListInsertDynamic region changed during application");
            }
            if (before) liveRegion.parent.insertBefore(itemRoot, before);
            else liveRegion.parent.appendChild(itemRoot);
            liveRegion.keys.set(operation.key, entry);
          });
          break;
        }
        case "list_move_dynamic": {
          const region = stagedRegions.get(operation.region);
          const entry = region?.keys.get(operation.key);
          const beforeEntry =
            operation.beforeKey === "" ? null : region?.keys.get(operation.beforeKey);
          if (
            !region ||
            region.kind !== "list" ||
            region.dynamicTemplate === 0 ||
            !validDynamicKey(operation.key) ||
            !validDynamicKey(operation.beforeKey, true) ||
            !entry?.dynamic ||
            (operation.beforeKey !== "" && !beforeEntry)
          ) {
            fail("ListMoveDynamic names a non-live region, key, or before-key");
          }
          dom.push(() => {
            if (entry === beforeEntry) return;
            if (beforeEntry) region.parent.insertBefore(entry.root, beforeEntry.root);
            else region.parent.appendChild(entry.root);
          });
          break;
        }
        case "list_remove_dynamic": {
          const region = stagedRegions.get(operation.region);
          const entry = region?.keys.get(operation.key);
          if (
            !region ||
            region.kind !== "list" ||
            region.dynamicTemplate === 0 ||
            !validDynamicKey(operation.key) ||
            !entry?.dynamic
          ) {
            fail("ListRemoveDynamic names a non-live list region or key");
          }
          region.keys.delete(operation.key);
          dom.push(() => {
            const liveRegion = regions.get(operation.region);
            if (!liveRegion?.keys.has(operation.key)) {
              fail("ListRemoveDynamic region changed during application");
            }
            liveRegion.parent.removeChild(entry.root);
            liveRegion.keys.delete(operation.key);
          });
          break;
        }
        case "update_dynamic_slots": {
          const region = stagedRegions.get(operation.region);
          const entry = region?.keys.get(operation.key);
          const identity = `${operation.region}:${operation.key}`;
          if (
            !region ||
            region.kind !== "list" ||
            region.dynamicTemplate === 0 ||
            !validDynamicKey(operation.key) ||
            !entry?.dynamic ||
            entry.template !== region.dynamicTemplate ||
            !(entry.localNodes instanceof Map) ||
            dynamicSlotUpdates.has(identity)
          ) {
            fail("UpdateDynamicSlots names an incompatible or repeated dynamic entry");
          }
          dynamicSlotUpdates.add(identity);
          const template = templates.get(entry.template);
          if (!template) fail("UpdateDynamicSlots names a missing authenticated template");
          dom.push(...applyTemplateSlots({
            template,
            suppliedSlots: operation.slots,
            pendingNodes: entry.localNodes,
            properties,
            attributes,
            aria,
            customProperties,
            requireComplete: true,
          }));
          break;
        }
        case "start_effect": {
          if (effectInstances.has(operation.instance)) {
            fail(`duplicate effect instance ${operation.instance}`);
          }
          if (effectHost.hasEffectInstance(operation.instance)) {
            fail(`effect instance ${operation.instance} is already live`);
          }
          effectInstances.add(operation.instance);
          effectHost.validateEffectDescriptor(operation.descriptor);
          effectStarts.push(() => effectHost.startEffect(operation));
          break;
        }
        case "cancel_effect":
          effectCancellations.push(() =>
            effectHost.cancelEffectKey(operation.cancellationKey)
          );
          break;
        case "sync_subscription": {
          if (subscriptionIds.has(operation.subscription)) {
            fail(`duplicate subscription identity ${operation.subscription}`);
          }
          subscriptionIds.add(operation.subscription);
          effectHost.validateSubscriptionDescriptor(operation.descriptor);
          subscriptionStarts.push(() => effectHost.syncSubscription(operation));
          break;
        }
        case "remove_subscription":
          if (subscriptionIds.has(operation.subscription)) {
            fail(`duplicate subscription identity ${operation.subscription}`);
          }
          subscriptionIds.add(operation.subscription);
          subscriptionCancellations.push(() =>
            effectHost.cancelSubscription(operation.subscription)
          );
          break;
        default:
          fail(`operation ${operation.kind} is not implemented by this host slice`);
      }
    }
    if (replacingInitialRoot && rootMounts !== 1) {
      fail("fresh replacement must contain exactly one root mount");
    }
    return {
      dom,
      effectCancellations,
      subscriptionCancellations,
      effectStarts,
      subscriptionStarts,
    };
  };

  const acceptOutput = (bytes) => {
    let frame;
    const started = monotonicMilliseconds();
    let validatedAt = started;
    let plannedAt = started;
    let domMilliseconds = 0;
    let hostMilliseconds = 0;
    try {
      frame = validator.validate(bytes);
      validatedAt = monotonicMilliseconds();
      if (frame.minor > wasmProtocolMinor) {
        fail("Wasm emitted a frame newer than its declared protocol version");
      }
      const planned = planFrame(frame);
      plannedAt = monotonicMilliseconds();
      validator.accept(frame, () => {
        const domStarted = monotonicMilliseconds();
        for (const apply of planned.dom) apply();
        frameHost?.sync();
        domMilliseconds = monotonicMilliseconds() - domStarted;
        const hostWork = [
          ...planned.effectCancellations,
          ...planned.subscriptionCancellations,
          ...planned.effectStarts,
          ...planned.subscriptionStarts,
        ];
        if (deferredActivation) {
          activationQueue.push(...hostWork);
        } else {
          const hostStarted = monotonicMilliseconds();
          for (const apply of hostWork) apply();
          hostMilliseconds = monotonicMilliseconds() - hostStarted;
        }
      });
      runtimeStats.frames += 1;
      runtimeStats.operations += frame.operations.length;
      runtimeStats.outputBytes += frame.byteLength;
      if (developmentTracing) {
        const finished = monotonicMilliseconds();
        developmentTimeline.push(Object.freeze({
          sequence: frame.sequence.toString(),
          frameKind: frame.kind,
          byteLength: frame.byteLength,
          operations: Object.freeze(
            frame.operations.map((operation) => developmentOperationSummary(operation, encoder)),
          ),
          modelChanges: developmentModelChanges(wasm, memory, developmentMetadata),
          timing: Object.freeze({
            validateMs: validatedAt - started,
            planMs: plannedAt - validatedAt,
            domMs: domMilliseconds,
            hostMs: hostMilliseconds,
            totalMs: finished - started,
          }),
          hostWorkDeferred: deferredActivation,
          input: developmentInputs.at(-1) || null,
        }));
        if (developmentTimeline.length > DEVELOPMENT_TRACE_LIMIT) {
          developmentTimeline.shift();
        }
      }
    } catch (error) {
      dispose();
      throw error;
    }
    return frame;
  };

  const dispatchEventData = (eventClassId, plan, data) => {
    const maximumStringBytes = Math.min(
      manifest.limits?.maxStringBytes ?? 1024 * 1024,
      manifest.limits?.maxPayloadBytes ?? 2 * 1024 * 1024,
      manifest.limits?.maxEventBytes ?? 60 * 1024,
    );
    if (
      typeof data.value !== "string" ||
      typeof data.key !== "string" ||
      encoder.encode(data.value).byteLength + encoder.encode(data.key).byteLength > maximumStringBytes
    ) {
      fail("delegated event payload is invalid or oversized");
    }
    const eventFrame = encodeEventFrame({
      appId: manifest.appId,
      buildId: manifest.buildId,
      sequence: inputSequence,
      eventPlan: plan.id,
      instance: plan.instance,
      eventClass: eventClassId,
      value: plan.readValue ? data.value : "",
      key: plan.readKey ? data.key : "",
      checked: plan.readChecked ? data.checked : false,
      composing: data.composing,
      autofill: data.autofill,
      userActivation: data.userActivation,
    });
    inputSequence += 1n;
    acceptOutput(callWithFrame("__glamour_dispatch", eventFrame));
  };

  const dispatchEventPlan = (eventClassId, plan, event) => {
    if (plan.preventDefault && typeof event?.preventDefault === "function") event.preventDefault();
    if (plan.stopPropagation && typeof event?.stopPropagation === "function") event.stopPropagation();
    const target = event?.target;
    dispatchEventData(eventClassId, plan, {
      value: typeof target?.value === "string" ? target.value : "",
      key: typeof event?.key === "string" ? event.key : "",
      checked: target?.checked === true,
      composing: event?.isComposing === true,
      autofill: event?.isTrusted === true && event?.inputType == null,
      userActivation: event?.isTrusted === true,
    });
  };

  const dispatchSnapshot = (snapshot) => {
    exactKeys(
      snapshot,
      ["plan", "node", "name", "value", "checked", "key", "composing", "userActivation"],
      "delegated event snapshot",
    );
    if (
      !Number.isInteger(snapshot.plan) ||
      !Number.isInteger(snapshot.node) ||
      typeof snapshot.name !== "string" ||
      typeof snapshot.checked !== "boolean" ||
      typeof snapshot.composing !== "boolean" ||
      typeof snapshot.userActivation !== "boolean"
    ) {
      fail("delegated event snapshot is malformed");
    }
    const matches = [...eventClasses].filter(([, eventClass]) => eventClass.name === snapshot.name);
    if (matches.length !== 1) fail("delegated event snapshot names an unknown event class");
    const [eventClassId] = matches[0];
    const plan = nodePlans.get(nodes.get(snapshot.node))?.get(eventClassId);
    if (!plan || plan.id !== snapshot.plan) {
      fail("delegated event snapshot does not match the adopted node plan");
    }
    dispatchEventData(eventClassId, plan, {
      value: snapshot.value,
      key: snapshot.key,
      checked: snapshot.checked,
      composing: snapshot.composing,
      autofill: false,
      userActivation: snapshot.userActivation,
    });
  };

  if (Array.isArray(manifest.frames) && manifest.frames.length > 0) {
    if (typeof options.installFrames !== "function") {
      dispose();
      fail("authenticated frames require a compartment host");
    }
    frameHost = options.installFrames({
      frames: manifest.frames,
      resolveNode: (id) => nodes.get(id),
      dispatch: dispatchSnapshot,
      onError: reportAsyncError,
    });
    if (!frameHost || typeof frameHost.sync !== "function" || typeof frameHost.dispose !== "function") {
      dispose();
      fail("the compartment host is invalid");
    }
  }

  for (const [eventClassId, eventClass] of eventClasses) {
    if (!eventClass || typeof eventClass.name !== "string") fail("event class is malformed");
    if (listeners.has(eventClass.name)) fail(`duplicate event class ${eventClass.name}`);
    const listener = (event) => {
      if (disposed) return;
      for (const node of composedPath(event, root)) {
        const plan = nodePlans.get(node)?.get(eventClassId);
        if (plan) {
          dispatchEventPlan(eventClassId, plan, event);
          return;
        }
      }
    };
    const capture = eventClass.capture === true;
    root.addEventListener(eventClass.name, listener, capture);
    listeners.set(eventClass.name, { listener, capture });
  }

  if (options.restoreSnapshot !== undefined) {
    if (
      !(options.restoreSnapshot instanceof Uint8Array) ||
      !developmentMetadata ||
      ![1, 2].includes(developmentMetadata.snapshotFormat) ||
      typeof wasm.__glamour_dev_restore !== "function"
    ) {
      dispose();
      fail("a compiler-authenticated development snapshot is required");
    }
    acceptOutput(callWithFrame("__glamour_dev_restore", options.restoreSnapshot));
  } else {
    const startFrame = options.startFrame;
    if (!(startFrame instanceof Uint8Array) || startFrame[8] !== FrameKind.Start) {
      dispose();
      fail("a binary start frame is required");
    }
    if (resumeMode) {
      const resumeFrame = startupBarrier ? startFrame.slice() : startFrame;
      if (startupBarrier) resumeFrame[9] = 1;
      callWithFrameWithoutOutput("__glamour_resume", resumeFrame);
      if (startupBarrier) {
        if (options.activationEvent !== undefined) {
          dispatchSnapshot(options.activationEvent);
        } else {
          const activation = encodeActivationFrame({
            appId: manifest.appId,
            buildId: manifest.buildId,
            sequence: inputSequence,
          });
          inputSequence += 1n;
          acceptOutput(callWithFrame("__glamour_dispatch", activation));
        }
      }
  } else acceptOutput(callWithFrame("__glamour_init", startFrame));
  }

  frameHost?.sync();

  if (resumed && !startupBarrier) {
    try {
      for (const subscription of resumed.subscriptions) {
        effectHost.syncSubscription(subscription);
      }
    } catch (error) {
      dispose();
      throw error;
    }
  }

  if (Array.isArray(manifest.actions) && manifest.actions.length > 0) {
    try {
      progressiveForms = installProgressiveForms({
        root,
        actions: manifest.actions,
        fetch: options.formFetch,
        FormData: options.FormData,
        baseUrl: options.baseUrl,
        onState: options.onFormState,
        onLifecycle: dispatchActionLifecycle,
      });
    } catch (error) {
      dispose();
      throw error;
    }
  }

  const application = {
    dispose,
    dispatch: dispatchSnapshot,
    developmentMetadata,
    snapshot() {
      if (
        disposed ||
        !developmentMetadata ||
        ![1, 2].includes(developmentMetadata.snapshotFormat) ||
        typeof wasm.__glamour_dev_snapshot !== "function" ||
        typeof wasm.__glamour_dev_snapshot_length !== "function"
      ) {
        fail("development snapshot is unavailable");
      }
      const pointer = wasm.__glamour_dev_snapshot();
      const length = wasm.__glamour_dev_snapshot_length();
      const manifestLimit =
        manifest.development?.maxSnapshotBytes ??
        manifest.limits?.maxSnapshotBytes ??
        1024 * 1024;
      const limit = developmentMetadata.snapshotBytes === null
        ? manifestLimit
        : Math.min(manifestLimit, developmentMetadata.snapshotBytes);
      if (
        !Number.isInteger(pointer) ||
        !Number.isInteger(length) ||
        (developmentMetadata.snapshotBytes !== null &&
          length !== developmentMetadata.snapshotBytes) ||
        (developmentMetadata.snapshotFormat === 2 && length < 44) ||
        length > limit ||
        pointer < 0 ||
        pointer + length > memory.buffer.byteLength
      ) {
        fail("development snapshot range is invalid");
      }
      const snapshot = new Uint8Array(memory.buffer).slice(pointer, pointer + length);
      if (developmentMetadata.snapshotFormat === 2) {
        const view = new DataView(snapshot.buffer, snapshot.byteOffset, snapshot.byteLength);
        const magic = new TextDecoder().decode(snapshot.subarray(0, 4));
        const schema = hex(snapshot.subarray(8, 40));
        if (
          magic !== "WGST" ||
          view.getUint16(4, true) !== 2 ||
          view.getUint16(6, true) !== developmentMetadata.fields.length ||
          schema !== developmentMetadata.modelSchema ||
          view.getUint32(40, true) + 44 !== snapshot.byteLength
        ) {
          fail("development aggregate snapshot is malformed");
        }
      }
      return snapshot;
    },
    activate(nextRoot) {
      if (disposed || !deferredActivation) fail("deferred activation is unavailable");
      if (!nextRoot || typeof nextRoot.replaceChildren !== "function") {
        fail("activation root is invalid");
      }
      for (const [name, registration] of listeners) {
        root.removeEventListener(name, registration.listener, registration.capture);
      }
      const children = [...root.childNodes];
      nextRoot.replaceChildren(...children);
      root = nextRoot;
      progressiveForms?.rebind(root);
      for (const [name, registration] of listeners) {
        root.addEventListener(name, registration.listener, registration.capture);
      }
      deferredActivation = false;
      for (const apply of activationQueue.splice(0)) apply();
    },
    get disposed() {
      return disposed;
    },
    get listenerCount() {
      return listeners.size;
    },
    get activeEffectCount() {
      return effectHost.activeEffectCount;
    },
    get activeSubscriptionCount() {
      return effectHost.activeSubscriptionCount;
    },
    getRuntimeStats() {
      return {
        ...runtimeStats,
        rootListeners: listeners.size,
        activeEffects: effectHost.activeEffectCount,
        activeSubscriptions: effectHost.activeSubscriptionCount,
        wasmMemoryPages: memory.buffer.byteLength / (64 * 1024),
      };
    },
  };
  if (developmentTracing) {
    application.inspectDevelopment = () => Object.freeze({
      schema: "witchy.glamour.devtools.v1",
      buildIdentity:
        typeof manifest.buildIdentity === "string" ? manifest.buildIdentity : null,
      model: developmentModelSummary(developmentMetadata),
      application: Object.freeze({
        kind: "application",
        activation: developmentActivation,
        liveNodes: nodes.size,
        liveRegions: regions.size,
        rootListeners: listeners.size,
        islands: Object.freeze([]),
      }),
      descriptors: developmentDescriptors,
      timeline: Object.freeze([...developmentTimeline]),
      replay: Object.freeze({
        schema: "witchy.glamour.replay.v1",
        retainedInputs: developmentInputs.length,
        maxInputs: DEVELOPMENT_TRACE_LIMIT,
        payloads: "private",
        commandResults: "recorded-as-authenticated-completion-frames",
      }),
      hostLifecycle: Object.freeze([...developmentHostLifecycle]),
      stats: Object.freeze(application.getRuntimeStats()),
    });
  }
  return Object.freeze(application);
}
