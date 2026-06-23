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
// The rune computes; the shell acts. A `Cmd` is interpreted here too (the shell,
// not the rune, performs effects) — this slice handles the empty command only
// (`NoCmd`), structured so effects-as-data can grow without touching the rune.

import { instantiate } from "./witchy-runtime.mjs";

// The wire format (must match glamour's `to_json`/`node_json`, spec'd there):
//   element : {"el": tag, "attrs": [<attr>...], "kids": [<node>...]}
//   text    : {"text": "..."}
//   <attr>  : ["prop", name, value] | ["on", event, <msg-json>]
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
 * @param {object} [opts.instantiateOpts]  forwarded to the RFC-0007 host shim
 */
export async function mount(wasmBytes, root, opts = {}) {
  const doc = opts.document || (typeof document !== "undefined" ? document : null);
  if (!doc) throw new Error("glamour-dom: no `document` available (pass opts.document)");
  const stepExport = opts.stepExport || "__export_export_step";
  const initialModel = opts.initialModel !== undefined ? opts.initialModel : {};

  const { callString } = await instantiate(wasmBytes, opts.instantiateOpts || {});

  // Call the rune's pure step function. `extra` is `{}` for the initial render or
  // `{ msg }` after an event; the rune returns `{ model, vnode }` (or `{ error }`).
  const step = (model, extra) => {
    const input = JSON.stringify({ model, ...extra });
    const out = callString(stepExport, input);
    const parsed = JSON.parse(out);
    if (parsed.error) throw new Error(`glamour rune step error: ${parsed.error}`);
    return parsed; // { model, vnode }
  };

  // The live state: the current model (an opaque JS value the rune round-trips)
  // and the last VNode (the diff's old tree).
  let model = initialModel;
  let lastVNode = null;
  // The single mounted DOM node (the app is single-rooted, matching glamour's
  // `html`/serializer). Replaced wholesale only on a tag change.
  let domRoot = null;

  // dispatch(msg) — the heart of the loop: run update+view via the rune, diff the
  // new tree against the old, patch the DOM, and remember the new state. `msg` is
  // a wire-format msg value (the exact JSON `to_json` embedded in an `on` attr).
  const dispatch = (msg) => {
    const { model: nextModel, vnode } = step(model, { msg });
    domRoot = patch(doc, root, domRoot, lastVNode, vnode, dispatch);
    model = nextModel;
    lastVNode = vnode;
  };

  // Initial render: model-only input, then build the DOM fresh.
  const first = step(model, {});
  model = first.model;
  lastVNode = first.vnode;
  domRoot = patch(doc, root, null, null, first.vnode, dispatch);

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
