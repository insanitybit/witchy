import { FrameKind, encodeOutputFrame } from "./glamour-protocol.mjs";

export const OPTIMIZED_COUNTER_APP_ID = 7;
export const OPTIMIZED_COUNTER_BUILD_ID = 0x0102_0304_0506_0708n;

export function optimizedCounterManifest() {
  return {
    appId: OPTIMIZED_COUNTER_APP_ID,
    buildId: OPTIMIZED_COUNTER_BUILD_ID,
    templates: new Map([
      [
        3,
        {
          root: {
            kind: "element",
            tag: "ul",
            node: 20,
            children: [{ kind: "text", node: 11, text: "" }],
          },
          slots: new Map([[1, { kind: "text", node: 11 }]]),
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
            children: [{ kind: "text", node: 42, text: "1" }],
          },
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
    nodes: new Map([20, 11, 41, 42, 51, 52, 61, 62].map((id) => [id, {}])),
    regions: new Map([[13, { template: 3 }]]),
    properties: new Map(),
    attributes: new Map(),
    aria: new Map(),
    eventClasses: new Map([[29, { name: "click", capture: false }]]),
    eventPlans: new Map([
      [31, { eventClass: 29, instance: 1, preventDefault: true }],
    ]),
  };
}

export function optimizedCounterStartFrame() {
  return encodeOutputFrame({
    kind: FrameKind.Start,
    appId: OPTIMIZED_COUNTER_APP_ID,
    buildId: OPTIMIZED_COUNTER_BUILD_ID,
    sequence: 0n,
  });
}

export function optimizedCounterResumeFrame(count) {
  if (!Number.isInteger(count) || count < 0 || count > 0xffff_ffff) {
    throw new Error("resume count must be a u32");
  }
  const state = new Uint8Array(4);
  new DataView(state.buffer).setUint32(0, count, true);
  return encodeOutputFrame({
    kind: FrameKind.Start,
    appId: OPTIMIZED_COUNTER_APP_ID,
    buildId: OPTIMIZED_COUNTER_BUILD_ID,
    sequence: 0n,
    payloads: [state],
  });
}

export function optimizedCounterStaticOrder(count) {
  return count % 2 === 0 ? [1, 2, 3] : [3, 1, 2];
}

export function optimizedCounterResumeManifest(count) {
  const manifest = optimizedCounterManifest();
  const identities = new Map([
    [1, [41, 42]],
    [2, [51, 52]],
    [3, [61, 62]],
  ]);
  const nodes = [
    { id: 20, path: [0] },
    { id: 11, path: [0, 0] },
  ];
  for (const [index, key] of optimizedCounterStaticOrder(count).entries()) {
    const [root, text] = identities.get(key);
    nodes.push({ id: root, path: [0, index + 1] });
    nodes.push({ id: text, path: [0, index + 1, 0] });
  }
  return {
    ...manifest,
    resume: {
      version: 1,
      sequence: 1,
      inputSequence: 0,
      nodes,
      regions: [
        {
          id: 13,
          parent: 20,
          keys: [...identities].map(([key, [root, text]]) => ({
            key,
            root,
            nodes: [root, text],
          })),
        },
      ],
      events: [{ node: 20, eventClass: 29, eventPlan: 31 }],
      subscriptions: [],
    },
  };
}
