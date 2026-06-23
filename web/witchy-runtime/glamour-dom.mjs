// glamour-dom — the capability-HOLDING DOM host shell for a glamour MVU rune
// (RFC-0008). The witchy rune is pure (capability-free): it computes a VNode tree
// and folds messages into the next model. This shell holds ALL authority — the
// DOM, the events, and (later) effects — and drives the loop:
//
//   1. instantiate the rune under RFC-0007's pure-compute host (deny-all imports);
//   2. call `export_step({model})` to get the initial {model, vnode};
//   3. build the DOM from the vnode (createElement/textContent/setAttribute ONLY —
//      NEVER innerHTML: the Perfect-Types-safe, structurally-injection-free path
//      RFC-0007/0008 require);
//   4. an `on` attr becomes an addEventListener that calls
//      `export_step({model, msg})`, diffs the new vnode against the old, patches
//      the DOM, and repeats.
//
// The rune computes; the shell acts. A `Cmd` is interpreted HERE — the shell, not
// the rune, performs effects. The rune is capability-denied: it cannot read a
// clock or arm a timer, so it can only DESCRIBE the effect as a `Cmd` value. This
// shell, which holds the authority, reads that description and performs it. The
// timer authority (`setTimeout`) is injectable (`opts.setTimeout`) so a headless
// test can drive a fake, controllable clock.

import { instantiate } from "./witchy-runtime.mjs";

// The vnode wire format (must match glamour's `to_json`/`node_json`, spec'd there):
//   element : {"el": tag, "attrs": [<attr>...], "kids": [<node>...]}
//   text    : {"text": "..."}
//   <attr>  : ["prop", name, value] | ["on", event, <msg-json>]
//
// The cmd wire format (must match glamour's `cmd_to_json`):
//   none  : {"cmd": "none"}
//   after : {"cmd": "after", "ms": <int>, "msg": <msg-json>}
//   batch : {"cmd": "batch", "cmds": [<cmd>...]}
const isText = (v) => v != null && typeof v.text === "string";
const isElement = (v) => v != null && typeof v.el === "string";

/**
 * Mount a glamour rune into `root`. Returns `{ dispatch, getModel, unmount }`.
 *
 * @param {BufferSource} wasmBytes  the compiled glamour app (exports `export_step`)
 * @param {Element} root            the DOM element to render into
 * @param {object} [opts]
 * @param {object} [opts.initialModel]  the initial model as a JS value (default `{}`)
 * @param {string} [opts.stepExport]    the export name (default `__export_export_step`)
 * @param {Document} [opts.document]    the DOM document (default the global `document`;
 *                                      pass a jsdom document for headless tests)
 * @param {function} [opts.setTimeout]  the timer the shell uses to perform an
 *                                      `after` Cmd: `(fn, ms) => id` (default the
 *                                      global `setTimeout`). INJECTABLE so a
 *                                      headless test can drive a fake clock — this
 *                                      is the capability the rune lacks and the
 *                                      shell holds.
 * @param {object} [opts.instantiateOpts]  forwarded to the RFC-0007 host shim
 */
export async function mount(wasmBytes, root, opts = {}) {
  const doc = opts.document || (typeof document !== "undefined" ? document : null);
  if (!doc) throw new Error("glamour-dom: no `document` available (pass opts.document)");
  const stepExport = opts.stepExport || "__export_export_step";
  const initialModel = opts.initialModel !== undefined ? opts.initialModel : {};
  // The timer is the shell's authority. Default to the global `setTimeout`; a
  // headless test injects a fake/controllable clock here.
  const setTimer =
    opts.setTimeout || (typeof setTimeout !== "undefined" ? setTimeout : null);

  const { callString } = await instantiate(wasmBytes, opts.instantiateOpts || {});

  // Call the rune's pure step function. `extra` is `{}` for the initial render or
  // `{ msg }` after an event; the rune returns `{ model, vnode, cmd }` (or
  // `{ error }`). `cmd` is the effect DESCRIPTION the shell interprets below.
  const step = (model, extra) => {
    const input = JSON.stringify({ model, ...extra });
    const out = callString(stepExport, input);
    const parsed = JSON.parse(out);
    if (parsed.error) throw new Error(`glamour rune step error: ${parsed.error}`);
    return parsed; // { model, vnode, cmd }
  };

  // The live state: the current model (an opaque JS value the rune round-trips)
  // and the last VNode (the diff's old tree).
  let model = initialModel;
  let lastVNode = null;
  // The single mounted DOM node (the app is single-rooted, matching glamour's
  // `html`/serializer). Replaced wholesale only on a tag change.
  let domRoot = null;

  // interpretCmd(cmd) — PERFORM the effect the rune merely described. The rune
  // cannot do any of this (it holds no capability); the shell does, because the
  // authority lives at the edge:
  //   * none  -> nothing;
  //   * after -> arm the (injected) timer; when it fires, dispatch the deferred
  //              msg back into the loop (which re-renders and may arm the next);
  //   * batch -> interpret each sub-command.
  const interpretCmd = (cmd) => {
    if (!cmd || typeof cmd.cmd !== "string") return;
    if (cmd.cmd === "none") return;
    if (cmd.cmd === "after") {
      if (!setTimer) throw new Error("glamour-dom: an `after` Cmd needs a timer (pass opts.setTimeout)");
      setTimer(() => dispatch(cmd.msg), cmd.ms);
      return;
    }
    if (cmd.cmd === "batch") {
      for (const sub of cmd.cmds || []) interpretCmd(sub);
      return;
    }
    throw new Error(`glamour-dom: unknown cmd kind \`${cmd.cmd}\``);
  };

  // dispatch(msg) — the heart of the loop: run update+view via the rune, diff the
  // new tree against the old, patch the DOM, remember the new state, and interpret
  // the command the rune returned. `msg` is a wire-format msg value (the exact
  // JSON `to_json` embedded in an `on` attr, or carried by an `after` Cmd).
  const dispatch = (msg) => {
    const { model: nextModel, vnode, cmd } = step(model, { msg });
    domRoot = patch(doc, root, domRoot, lastVNode, vnode, dispatch);
    model = nextModel;
    lastVNode = vnode;
    interpretCmd(cmd);
  };

  // Initial render: model-only input, then build the DOM fresh and interpret the
  // command (the initial step emits `none`, but interpreting it keeps the loop
  // uniform should an app want a startup effect).
  const first = step(model, {});
  model = first.model;
  lastVNode = first.vnode;
  domRoot = patch(doc, root, null, null, first.vnode, dispatch);
  interpretCmd(first.cmd);

  return {
    dispatch,
    getModel: () => model,
    unmount() {
      if (domRoot && domRoot.parentNode === root) root.removeChild(domRoot);
      domRoot = null;
      lastVNode = null;
    },
  };
}

// --- the differ -------------------------------------------------------------
// Diff `oldV` -> `newV` and patch the DOM, returning the (possibly new) DOM node
// for `newV`. `dom` is the existing DOM node for `oldV` (null on first render).
// The strategy is the simple, correct Elm-style structural diff:
//   * different KIND (text<->element) or different element TAG -> replace wholesale;
//   * same text node, changed string -> set textContent;
//   * same element tag -> reconcile attributes, then diff children by index.
// Keys (stable identity) are a later refinement; index diffing is correct for the
// static-structure apps this slice targets.
function patch(doc, parent, dom, oldV, newV, dispatch) {
  // Create fresh when there is no existing node or the shape/tag changed.
  if (dom == null || oldV == null || kindOrTagChanged(oldV, newV)) {
    const created = createNode(doc, newV, dispatch);
    if (dom == null) {
      parent.appendChild(created);
    } else {
      parent.replaceChild(created, dom);
    }
    return created;
  }

  if (isText(newV)) {
    // Same text node; update the string only if it changed.
    if (oldV.text !== newV.text) dom.textContent = newV.text;
    return dom;
  }

  // Same element tag: reconcile attributes and recurse into children.
  reconcileAttrs(dom, oldV.attrs || [], newV.attrs || [], dispatch);
  const oldKids = oldV.kids || [];
  const newKids = newV.kids || [];
  const max = Math.max(oldKids.length, newKids.length);
  // Snapshot the live child nodes up front (patch mutates `dom.childNodes`).
  const childNodes = Array.from(dom.childNodes);
  for (let i = 0; i < max; i++) {
    const oldChild = oldKids[i];
    const newChild = newKids[i];
    if (newChild == null) {
      // Surplus old child: remove it.
      if (childNodes[i] && childNodes[i].parentNode === dom) dom.removeChild(childNodes[i]);
    } else if (oldChild == null) {
      // New child: build and append.
      dom.appendChild(createNode(doc, newChild, dispatch));
    } else {
      patch(doc, dom, childNodes[i], oldChild, newChild, dispatch);
    }
  }
  return dom;
}

function kindOrTagChanged(oldV, newV) {
  if (isText(oldV) !== isText(newV)) return true;
  if (isElement(oldV) && isElement(newV)) return oldV.el !== newV.el;
  return false;
}

// Build a DOM node from a VNode — createElement / textContent / setAttribute /
// addEventListener ONLY. There is NO HTML-string sink (no innerHTML / no
// insertAdjacentHTML), so this path is structurally incapable of the injection
// Perfect Types exists to neutralize (RFC-0007/0008 security composition).
function createNode(doc, v, dispatch) {
  if (isText(v)) return doc.createTextNode(v.text);
  if (!isElement(v)) {
    throw new Error(`glamour-dom: malformed vnode: ${JSON.stringify(v)}`);
  }
  const el = doc.createElement(v.el);
  for (const a of v.attrs || []) applyAttr(el, a, dispatch);
  for (const k of v.kids || []) el.appendChild(createNode(doc, k, dispatch));
  return el;
}

// Apply one wire-format attribute to a fresh element.
function applyAttr(el, attr, dispatch) {
  const [kind, a, b] = attr;
  if (kind === "prop") {
    el.setAttribute(a, b);
  } else if (kind === "on") {
    // `b` is the msg VALUE (its wire JSON). The handler hands it straight back to
    // the rune via dispatch — events are data, the shell performs no logic.
    addHandler(el, a, b, dispatch);
  } else {
    throw new Error(`glamour-dom: unknown attr kind \`${kind}\``);
  }
}

// Track event handlers per element/event so re-renders can replace them (the msg
// payload can change between renders) without leaking listeners.
const HANDLERS = new WeakMap();

function addHandler(el, event, msg, dispatch) {
  let perEl = HANDLERS.get(el);
  if (!perEl) {
    perEl = new Map();
    HANDLERS.set(el, perEl);
  }
  const existing = perEl.get(event);
  if (existing) el.removeEventListener(event, existing.fn);
  const fn = () => dispatch(msg);
  el.addEventListener(event, fn);
  perEl.set(event, { fn, msg });
}

function removeHandler(el, event) {
  const perEl = HANDLERS.get(el);
  if (!perEl) return;
  const existing = perEl.get(event);
  if (existing) {
    el.removeEventListener(event, existing.fn);
    perEl.delete(event);
  }
}

// Reconcile an element's attributes between renders: set/replace the new ones,
// remove props/handlers that disappeared.
function reconcileAttrs(el, oldAttrs, newAttrs, dispatch) {
  const oldProps = new Set();
  const oldEvents = new Set();
  for (const [kind, a] of oldAttrs) {
    if (kind === "prop") oldProps.add(a);
    else if (kind === "on") oldEvents.add(a);
  }
  const newProps = new Set();
  const newEvents = new Set();
  for (const attr of newAttrs) {
    const [kind, a] = attr;
    if (kind === "prop") newProps.add(a);
    else if (kind === "on") newEvents.add(a);
    applyAttr(el, attr, dispatch); // setAttribute / (re)addHandler with the new msg
  }
  // Drop props/handlers no longer present.
  for (const name of oldProps) if (!newProps.has(name)) el.removeAttribute(name);
  for (const event of oldEvents) if (!newEvents.has(event)) removeHandler(el, event);
}
