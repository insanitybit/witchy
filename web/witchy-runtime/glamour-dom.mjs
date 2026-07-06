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
// An isolated foreign-code compartment (RFC-0015): {"compartment": renderer, "grant":
// json-string, "on": msg-tag}. The renderer runs in a locked-down iframe, never the page.
const isCompartment = (v) => v != null && typeof v.compartment === "string";
// A keyed child: {"key": <stable id>, "node": <vnode>}. The key drives list diffing.
const isKeyed = (v) => v != null && typeof v.key === "string" && v.node != null;
// A host-owned SECRET input (RFC-0039): {"secret": {"form","field"}, "on_ready": msg-tag}.
// The rune emitted NO value — the typed bytes live only in this shell's custody (the real
// <input> + `dispatch.__secrets`), never crossing back into the rune. (BUG-357) The shape is
// checked STRICTLY — `form`/`field`/`on_ready` must all be strings — so a malformed/forged
// vnode can never coax the shell into mounting a host password field.
const isSecret = (v) =>
  v != null &&
  v.secret != null &&
  typeof v.secret.form === "string" &&
  typeof v.secret.field === "string" &&
  typeof v.on_ready === "string";
// (BUG-273) Collision-free host-custody slot key. A raw `form + "/" + field` join lets
// `("a/b","c")` and `("a","b/c")` collapse to one key; percent-escaping `%` then `/` in each
// component makes the `/` separator unambiguous (injective). MUST match glamour's `slot_encode`.
const escSlot = (s) => String(s).replace(/%/g, "%25").replace(/\//g, "%2F");
const secretSlot = (v) => `${escSlot(v.secret.form)}/${escSlot(v.secret.field)}`;
// A host SLOT (RFC-0041): {"slot": kind, "data": payload}. The host's `kind` renderer mounts
// a widget here (main frame); glamour renders it once and NEVER diffs into it.
const isSlot = (v) => v != null && typeof v.slot === "string" && typeof v.data === "string";

// (BUG-260) The DOM element ALLOWLIST — the structural XSS boundary for the live sink. A pure
// rune emits an arbitrary `element(tag, …)`, so `createElement` on an unvalidated name would
// let it mint `<script>`, an unsandboxed `<iframe srcdoc=…>`, `<object>`, `<link>`, `<meta>`,
// etc. — trusted-page script/resource execution WITHOUT holding a browser capability. Only
// inert/presentation elements below are built as themselves; anything else is FAIL CLOSED to an
// inert `<span data-glamour-blocked-tag>` (its safe children still render). Sandboxed
// compartments use `mountCompartment` (their own path), never this element sink.
const SAFE_ELEMENTS = new Set([
  // flow / sections
  "a", "abbr", "address", "article", "aside", "b", "bdi", "bdo", "blockquote", "br",
  "button", "caption", "cite", "code", "col", "colgroup", "data", "datalist", "dd", "del", "details",
  "dfn", "dialog", "div", "dl", "dt", "em", "fieldset", "figcaption", "figure", "footer",
  "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hgroup", "hr", "i", "img", "input",
  "ins", "kbd", "label", "legend", "li", "main", "mark", "menu", "meter", "nav", "ol",
  "optgroup", "option", "output", "p", "picture", "pre", "progress", "q", "rp", "rt", "ruby",
  "s", "samp", "section", "select", "small", "source", "span", "strong", "sub", "summary",
  "sup", "table", "tbody", "td", "textarea", "tfoot", "th", "thead", "time", "tr", "u", "ul",
  "var", "wbr",
]);
const isSafeElement = (tag) => SAFE_ELEMENTS.has(String(tag).toLowerCase());

// (BUG-260) Attribute names that are HTML/script sinks even on a structural element and are
// NOT plain URL attributes (those go through `safeUrl`). `srcdoc` embeds a whole HTML document
// into an iframe, so it is never written.
const DANGEROUS_ATTRS = new Set(["srcdoc"]);

// (BUG-272) A compartment renderer id is a REGISTRY KEY, never URL path material. Same grammar
// as glamour's `valid_renderer_id`: lowercase ASCII alnum / `-` / `_`, leading alnum, non-empty.
// Excludes `/ . % ? #` so a crafted id can't escape the `/compartments/<id>/` namespace.
const isValidRendererId = (id) => typeof id === "string" && /^[a-z0-9][a-z0-9_-]*$/.test(id);

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
  // The network is the shell's authority too: the rune emits an `http` Cmd DESCRIPTION;
  // the shell performs the fetch — attaching any session credential ITSELF via
  // `opts.authHeaders` so the token never enters the rune — and dispatches the result
  // back as a msg. The history API backs the `nav` Cmd + Back/Forward routing. All
  // injectable so a headless test drives fakes (exactly like the timer).
  const doFetch = opts.fetch || (typeof fetch !== "undefined" ? fetch : null);
  const history = opts.history || (typeof window !== "undefined" ? window.history : null);
  const location = opts.location || (typeof window !== "undefined" ? window.location : null);

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
  // (BUG-377) Explicit lifecycle state. Once `unmount()` sets `disposed`, `dispatch` and
  // `interpretCmd` become no-ops, so a late timer/fetch/port callback can no longer run the
  // rune, patch the (removed) DOM, or arm more effects. Pending timers are tracked so they can
  // be cancelled, and the popstate listener is removed, on unmount.
  let disposed = false;
  const timers = new Set();
  const clearTimer =
    opts.clearTimeout || (typeof clearTimeout !== "undefined" ? clearTimeout : null);
  let removeRouteListener = null;

  // interpretCmd(cmd) — PERFORM the effect the rune merely described. The rune
  // cannot do any of this (it holds no capability); the shell does, because the
  // authority lives at the edge:
  //   * none  -> nothing;
  //   * after -> arm the (injected) timer; when it fires, dispatch the deferred
  //              msg back into the loop (which re-renders and may arm the next);
  //   * batch -> interpret each sub-command.
  // (BUG-357) A port is invocable only if it passes the app's port policy. `opts.allowedPorts`
  // (array or Set of names) is an explicit allowlist for the mounted app; when absent, the set
  // of registered `opts.ports`/`opts.secretPorts` keys is the effective allowlist (an unlisted
  // port still fails the registration check below). Either way an unknown name is refused.
  const allowedPortSet = opts.allowedPorts ? new Set(opts.allowedPorts) : null;
  const portAllowed = (name) => !allowedPortSet || allowedPortSet.has(name);

  const interpretCmd = (cmd) => {
    if (disposed) return;                          // (BUG-377) no effects after unmount
    if (!cmd || typeof cmd.cmd !== "string") return;
    if (cmd.cmd === "none") return;
    if (cmd.cmd === "after") {
      if (!setTimer) throw new Error("glamour-dom: an `after` Cmd needs a timer (pass opts.setTimeout)");
      // (BUG-017) The host is the real authority on the timer floor: re-clamp the delay to the
      // token's `min_ms` carried on the wire, rather than trusting the emitted `ms` blindly.
      const ms = Math.max(Number(cmd.ms) || 0, Number(cmd.min_ms) || 0);
      // (BUG-377) Track the id so unmount can cancel a still-pending timer.
      let id;
      id = setTimer(() => {
        timers.delete(id);
        dispatch(cmd.msg);
      }, ms);
      timers.add(id);
      return;
    }
    if (cmd.cmd === "batch") {
      for (const sub of cmd.cmds || []) interpretCmd(sub);
      return;
    }
    if (cmd.cmd === "http") {
      if (!doFetch) throw new Error("glamour-dom: an `http` Cmd needs fetch (pass opts.fetch)");
      // (BUG-017) Re-enforce the `UiFetch` policy carried on the wire — the host is the real
      // authority boundary. A method outside the token's set, or a URL outside its prefix, is
      // FAIL CLOSED (dropped) so a forged/off-policy command never reaches the network.
      if (typeof cmd.url !== "string" || typeof cmd.method !== "string") return;
      if (cmd.methods != null && !methodAllowed(cmd.methods, cmd.method)) return;
      if (cmd.prefix != null && !cmd.url.startsWith(cmd.prefix)) return;
      // The SHELL attaches credentials — the rune never holds the session token.
      const headers = opts.authHeaders ? opts.authHeaders() : {};
      const init = { method: cmd.method, headers };
      if (cmd.body) init.body = cmd.body;
      Promise.resolve(doFetch(cmd.url, init))
        .then((resp) => Promise.resolve(resp.text()).then((body) => ({ status: resp.status, body })))
        .then(({ status, body }) => dispatch({ $variant: cmd.tag, $values: [status, body] }))
        // A transport error is status 0 — an ordinary `update` arm, never an exception.
        .catch((e) => dispatch({ $variant: cmd.tag, $values: [0, String(e)] }));
      return;
    }
    if (cmd.cmd === "nav") {
      // (BUG-017) Re-enforce the `UiRoute` `base` carried on the wire — a path outside the
      // authorized base is FAIL CLOSED (no pushState, no route msg).
      if (typeof cmd.path !== "string") return;
      if (cmd.base != null && !cmd.path.startsWith(cmd.base)) return;
      if (history) history.pushState({}, "", cmd.path);
      if (opts.routeTag) dispatch({ $variant: opts.routeTag, $values: [cmd.path] });
      return;
    }
    if (cmd.cmd === "port") {
      // A host capability the rune cannot perform (a session/WebAuthn ceremony, storage):
      // run it HERE — the credential/`navigator.credentials`/token never enters the rune —
      // and hand only the result back as a msg. (BUG-357) Validate the command SHAPE and the
      // port IDENTITY before invoking: a forged/malformed `port` command is refused. The
      // registered `opts.ports` keys ARE the port allowlist; an optional `opts.allowedPorts`
      // narrows it further for the mounted app.
      if (typeof cmd.port !== "string" || typeof cmd.tag !== "string") throw new Error("glamour-dom: malformed `port` cmd");
      if (!portAllowed(cmd.port)) throw new Error(`glamour-dom: port \`${cmd.port}\` is not in the allowed-port policy`);
      const fn = opts.ports && opts.ports[cmd.port];
      if (!fn) throw new Error(`glamour-dom: no port \`${cmd.port}\` (pass opts.ports.${cmd.port})`);
      Promise.resolve(fn(cmd.arg))
        .then((result) => dispatch({ $variant: cmd.tag, $values: [String(result)] }))
        .catch((e) => dispatch({ $variant: cmd.tag, $values: ["error:" + String(e)] }));
      return;
    }
    if (cmd.cmd === "submit_secret") {
      // Read the secret from the shell's OWN custody (the rune never held it) and hand it to
      // the host credential port. Only the port's result returns, as an ordinary msg — the
      // password bytes go host -> port and stop there. A missing/empty field submits "".
      // (BUG-357) Validate the SHAPE, the port IDENTITY, and that `slot` names a CURRENTLY
      // RENDERED host-owned secret — so a forged command cannot exfiltrate an arbitrary slot
      // or invoke an un-granted port.
      if (typeof cmd.slot !== "string" || typeof cmd.port !== "string" || typeof cmd.tag !== "string") throw new Error("glamour-dom: malformed `submit_secret` cmd");
      const renderedSlots = dispatch.__secretSlots || new Set();
      if (!renderedSlots.has(cmd.slot)) throw new Error("glamour-dom: submit_secret for a slot that is not a rendered host-owned secret");
      if (!portAllowed(cmd.port)) throw new Error(`glamour-dom: port \`${cmd.port}\` is not in the allowed-port policy`);
      const store = dispatch.__secrets || {};
      const secret = typeof store[cmd.slot] === "string" ? store[cmd.slot] : "";
      const fn = (opts.secretPorts && opts.secretPorts[cmd.port]) || (opts.ports && opts.ports[cmd.port]);
      if (!fn) throw new Error(`glamour-dom: no port \`${cmd.port}\` for submit_secret (pass opts.ports.${cmd.port})`);
      Promise.resolve(fn(secret))
        .then((result) => dispatch({ $variant: cmd.tag, $values: [String(result)] }))
        .catch((e) => dispatch({ $variant: cmd.tag, $values: ["error:" + String(e)] }));
      return;
    }
    throw new Error(`glamour-dom: unknown cmd kind \`${cmd.cmd}\``);
  };

  // dispatch(msg) — the heart of the loop: run update+view via the rune, diff the
  // new tree against the old, patch the DOM, remember the new state, and interpret
  // the command the rune returned. `msg` is a wire-format msg value (the exact
  // JSON `to_json` embedded in an `on` attr, or carried by an `after` Cmd).
  const dispatch = (msg) => {
    if (disposed) return;                          // (BUG-377) a late callback is a no-op
    const { model: nextModel, vnode, cmd } = step(model, { msg });
    domRoot = patch(doc, root, domRoot, lastVNode, vnode, dispatch);
    model = nextModel;
    lastVNode = vnode;
    interpretCmd(cmd);
  };
  // Host-slot renderers (RFC-0041): `{ [kind]: (doc, data) => domNode }`. Stashed on
  // `dispatch` so the module-level `mountSlot` (which only receives `dispatch`) can reach them,
  // mirroring the secret store.
  dispatch.__slots = opts.slots || {};
  // (BUG-357) The set of host-owned secret slots that have actually been RENDERED. A
  // `submit_secret` command is honored only for a slot in this set (see `interpretCmd`).
  dispatch.__secretSlots = new Set();

  // Initial render: model-only input, then build the DOM fresh and interpret the
  // command (the initial step emits `none`, but interpreting it keeps the loop
  // uniform should an app want a startup effect).
  const first = step(model, {});
  model = first.model;
  lastVNode = first.vnode;
  domRoot = patch(doc, root, null, null, first.vnode, dispatch);
  interpretCmd(first.cmd);

  // Client routing: if the app declares a route msg tag, mirror the URL into the loop —
  // deliver the initial path now, and re-deliver on Back/Forward (popstate). The app's
  // `update` maps the path to the view; `Nav` Cmds push new paths. Injectable for tests.
  if (opts.routeTag) {
    const fireRoute = () =>
      dispatch({ $variant: opts.routeTag, $values: [location ? location.pathname : "/"] });
    // (BUG-377) Keep an unsubscribe so `unmount()` can drop the popstate listener. `onPopState`
    // may return its own unsubscribe (contract extension); otherwise the default window path
    // removes the exact listener it added.
    if (opts.onPopState) {
      const maybeUnsub = opts.onPopState(fireRoute);
      if (typeof maybeUnsub === "function") removeRouteListener = maybeUnsub;
    } else if (typeof window !== "undefined") {
      window.addEventListener("popstate", fireRoute);
      removeRouteListener = () => window.removeEventListener("popstate", fireRoute);
    }
    fireRoute();
  }

  return {
    dispatch,
    getModel: () => model,
    // (BUG-377) Fully dispose: after this, no pending timer/fetch/port callback can run the
    // rune or touch the DOM (the `disposed` guard), the popstate listener is removed, and
    // pending timers are cancelled. Idempotent.
    unmount() {
      if (disposed) return;
      disposed = true;
      if (clearTimer) for (const id of timers) clearTimer(id);
      timers.clear();
      if (removeRouteListener) {
        removeRouteListener();
        removeRouteListener = null;
      }
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
  // Unwrap keyed wrappers: a `Keyed` carries identity for reconcileKeyed's matching ONLY;
  // structurally it patches as its inner node. The keyed path already passes `.node`, but the
  // index-diff path can hand us a Keyed OLD child against an unkeyed NEW child (e.g. navigating
  // a keyed-list view back to a plain one under the same parent tag) — unwrap here so the diff
  // sees real nodes instead of mistaking the wrapper for an unknown shape (which slipped past
  // `kindOrTagChanged` and reached `reconcileAttrs` on the wrong node).
  if (isKeyed(oldV)) oldV = oldV.node;
  if (isKeyed(newV)) newV = newV.node;
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

  if (isCompartment(newV)) {
    // Same renderer (a different one would have been rebuilt by kindOrTagChanged): keep the
    // live iframe but APPLY a changed grant/event (BUG-387) — repost the grant over the channel
    // and adopt the new `on` tag, so the foreign widget never drifts from the trusted tree.
    updateCompartment(dom, newV);
    return dom;
  }

  if (isSecret(newV)) {
    // Same slot (a different one would have been rebuilt): keep the live <input> so the
    // host-held value and caret survive. The rune never described a value to re-apply.
    return dom;
  }

  if (isSlot(newV)) {
    // Same slot kind + data (a change would have been rebuilt by kindOrTagChanged): KEEP the
    // live host widget — glamour never diffs into a host slot, so a re-render (e.g. a nav-only
    // change) can't clobber a runnable cell's mounted editor/output.
    return dom;
  }

  // Same element tag: reconcile attributes and recurse into children.
  reconcileAttrs(dom, oldV.attrs || [], newV.attrs || [], dispatch);
  const oldKids = oldV.kids || [];
  const newKids = newV.kids || [];
  // Snapshot the live child nodes up front (patch mutates `dom.childNodes`).
  const childNodes = Array.from(dom.childNodes);
  // Keyed reconciliation when any new child carries a stable key: reorders/inserts/removes
  // reuse the matching DOM node (preserving identity and `<input>` focus) instead of
  // rebuilding by position. Otherwise the simple, correct index diff.
  if (newKids.some(isKeyed)) {
    reconcileKeyed(doc, dom, oldKids, newKids, childNodes, dispatch);
    return dom;
  }
  const max = Math.max(oldKids.length, newKids.length);
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

// Reconcile a list of children by stable KEY (instead of by index): match each new keyed
// child to the old DOM node with the same key, patch it in place, and re-order by moving
// the existing nodes — so a reused node keeps its identity (and an `<input>`'s focus and
// caret) across reordering, insertion, and removal. Unkeyed children mixed in are rebuilt.
function reconcileKeyed(doc, parent, oldKids, newKids, childNodes, dispatch) {
  const oldByKey = new Map();
  oldKids.forEach((ok, i) => {
    if (isKeyed(ok)) oldByKey.set(ok.key, { v: ok.node, dom: childNodes[i] });
  });
  const ordered = [];
  for (const nk of newKids) {
    if (isKeyed(nk)) {
      const prev = oldByKey.get(nk.key);
      if (prev && prev.dom) {
        ordered.push(patch(doc, parent, prev.dom, prev.v, nk.node, dispatch));
        oldByKey.delete(nk.key);
      } else {
        ordered.push(createNode(doc, nk.node, dispatch)); // a new key -> a fresh node
      }
    } else {
      ordered.push(createNode(doc, nk, dispatch));
    }
  }
  // Drop old keyed nodes whose key didn't reappear.
  for (const { dom: d } of oldByKey.values()) {
    if (d && d.parentNode === parent) parent.removeChild(d);
  }
  // Drop any remaining child not in the new order (stale unkeyed nodes).
  for (const c of Array.from(parent.childNodes)) {
    if (!ordered.includes(c)) parent.removeChild(c);
  }
  // Re-attach in the new order. `appendChild` MOVES an already-attached node, so a reused
  // keyed node is repositioned without being recreated (its focus/caret survive).
  for (const d of ordered) parent.appendChild(d);
}

function kindOrTagChanged(oldV, newV) {
  if (isText(oldV) !== isText(newV)) return true;
  if (isCompartment(oldV) !== isCompartment(newV)) return true;
  if (isSecret(oldV) !== isSecret(newV)) return true;
  if (isSlot(oldV) !== isSlot(newV)) return true;
  // A different renderer is a different compartment — rebuild (a fresh isolated frame).
  if (isCompartment(oldV) && isCompartment(newV)) return oldV.compartment !== newV.compartment;
  // A different slot is a different field — rebuild; same slot KEEPS the live <input> so its
  // (host-held) value and caret survive a re-render.
  if (isSecret(oldV) && isSecret(newV)) return secretSlot(oldV) !== secretSlot(newV);
  // A host slot rebuilds only if its kind or payload changed; same kind+data keeps the live
  // widget (so a nav-only re-render never remounts a runnable cell).
  if (isSlot(oldV) && isSlot(newV)) return oldV.slot !== newV.slot || oldV.data !== newV.data;
  if (isElement(oldV) && isElement(newV)) return oldV.el !== newV.el;
  return false;
}

// Build a DOM node from a VNode — createElement / textContent / setAttribute /
// addEventListener ONLY. There is NO HTML-string sink (no innerHTML / no
// insertAdjacentHTML), so this path is structurally incapable of the injection
// Perfect Types exists to neutralize (RFC-0007/0008 security composition).
function createNode(doc, v, dispatch) {
  if (isText(v)) return doc.createTextNode(v.text);
  if (isKeyed(v)) return createNode(doc, v.node, dispatch); // the key is a diff hint only
  if (isCompartment(v)) return mountCompartment(doc, v, dispatch);
  if (isSecret(v)) return mountSecretInput(doc, v, dispatch);
  if (isSlot(v)) return mountSlot(doc, v, dispatch);
  if (!isElement(v)) {
    throw new Error(`glamour-dom: malformed vnode: ${JSON.stringify(v)}`);
  }
  // (BUG-260) FAIL CLOSED at the element sink: only allowlisted inert/presentation tags are
  // built as themselves. A disallowed tag (`script`, unsandboxed `iframe`, `object`, `link`,
  // `meta`, …) — or a name `createElement` would reject — becomes an inert `<span>` recording
  // the blocked tag, so its (still-sanitized) children render but no executable element is
  // created in the trusted page.
  const safe = isSafeElement(v.el);
  const el = doc.createElement(safe ? v.el : "span");
  if (!safe) el.setAttribute("data-glamour-blocked-tag", String(v.el));
  for (const a of v.attrs || []) applyAttr(el, a, dispatch);
  for (const k of v.kids || []) el.appendChild(createNode(doc, k, dispatch));
  return el;
}

// Mount an isolated foreign-code compartment (RFC-0015). The renderer is loaded into a
// LOCKED-DOWN iframe — `sandbox="allow-scripts"` WITHOUT `allow-same-origin` gives it an
// opaque origin (no cookies, no `localStorage`, no access to the parent DOM), and the
// renderer is served from `/compartments/<id>/` under its own `connect-src 'none'` CSP
// (no network egress). The ONLY data that crosses in is `grant` (a non-sensitive JSON
// string), over a one-way MessageChannel established after load; the ONLY thing that
// comes back is the narrow `on` event (e.g. a height). So even a fully-compromised
// renderer (an XSS in d3, a swapped-out bundle) is contained: nothing to steal, nowhere
// to send it, no reach into the trusted page. The witchy rune holds no authority over
// it — it only described the compartment; the host shell (this code) spawns it.
function mountCompartment(doc, node, dispatch) {
  const frame = doc.createElement("iframe");
  frame.setAttribute("sandbox", "allow-scripts");
  frame.setAttribute("class", "glamour-compartment");
  // (BUG-272) The renderer id is a REGISTRY KEY, not URL path material. A value carrying
  // `/`/`.`/`%`/`?`/`#` (which URL resolution could normalize out of the `/compartments/<id>/`
  // namespace) is FAIL CLOSED to an inert `about:blank` frame, and its channel is not wired.
  const validId = isValidRendererId(node.compartment);
  frame.setAttribute("src", validId ? `/compartments/${node.compartment}/` : "about:blank");
  // (BUG-387) Keep mutable per-frame state so a same-renderer re-render can repost a changed
  // grant and route later iframe events to the CURRENT `on` tag (see `updateCompartment`).
  const state = { node, channel: null };
  frame.__glamourCompartment = state;
  // The grant/event channel is wired only where a real iframe + MessageChannel exist
  // (a browser) AND the renderer id is valid; the isolation CONFIG above is what actually
  // contains the renderer.
  if (validId && typeof MessageChannel !== "undefined" && typeof frame.addEventListener === "function") {
    const channel = new MessageChannel();
    state.channel = channel;
    frame.addEventListener("load", () => {
      try {
        frame.contentWindow.postMessage({ kind: "init" }, "*", [channel.port2]);
        channel.port1.postMessage({ kind: "grant", data: state.node.grant }); // the only data IN
      } catch (_e) {
        /* an opaque-origin frame may reject; the grant is non-sensitive either way */
      }
    });
    channel.port1.onmessage = (ev) => {
      // The only thing the compartment may say back: a tagged, schema-checked event, routed to
      // the CURRENT on-event tag (BUG-387).
      if (ev.data && ev.data.kind === "event" && Array.isArray(ev.data.values)) {
        dispatch({ $variant: state.node.on, $values: ev.data.values });
      }
    };
  }
  return frame;
}

// (BUG-387) Apply a same-renderer compartment re-render: repost a changed grant over the live
// channel and adopt the new `on` tag for subsequent events. A changed RENDERER is not handled
// here — `kindOrTagChanged` already rebuilds the iframe for that.
function updateCompartment(frame, newNode) {
  const state = frame && frame.__glamourCompartment;
  if (!state) return;
  const grantChanged = state.node.grant !== newNode.grant;
  state.node = newNode; // later events dispatch the new `on`; a repost reads the new grant
  if (grantChanged && state.channel) {
    try {
      state.channel.port1.postMessage({ kind: "grant", data: newNode.grant });
    } catch (_e) {
      /* opaque-origin frame may reject; the grant is non-sensitive either way */
    }
  }
}

// Mount a host-owned SECRET input (RFC-0039). The typed value lives ONLY in host custody —
// the real `<input type=password>` and the shell's `dispatch.__secrets` store — and NEVER
// crosses into the rune. On input we record the value keyed by its slot and dispatch the
// rune's `on_ready` msg carrying only a NON-sensitive status ("Empty"/"NonEmpty"), so the
// rune can enable a submit button without ever observing the bytes. To act on the value the
// rune emits a `submit_secret` Cmd naming the slot; `interpretCmd` reads it from this store
// and hands it to a host port. A sibling component holds no DOM authority and cannot read
// this store, so it can never intercept another component's password.
function mountSecretInput(doc, node, dispatch) {
  const el = doc.createElement("input");
  el.setAttribute("type", "password");
  el.setAttribute("class", "glamour-secret");
  const slot = secretSlot(node);
  // (BUG-357) Record that this slot is a genuinely rendered host-owned secret, so a
  // `submit_secret` command naming it can be honored (and a forged slot cannot).
  (dispatch.__secretSlots = dispatch.__secretSlots || new Set()).add(slot);
  const store = (dispatch.__secrets = dispatch.__secrets || {});
  if (typeof el.addEventListener === "function") {
    el.addEventListener("input", (ev) => {
      const value = (ev && ev.target && typeof ev.target.value === "string") ? ev.target.value : "";
      store[slot] = value; // host custody — the value stops HERE, never returned to the rune
      dispatch({ $variant: node.on_ready, $values: [value.length > 0 ? "NonEmpty" : "Empty"] });
    });
  }
  return el;
}

// Mount a host SLOT (RFC-0041): the host's `kind` renderer builds the widget in the MAIN
// frame; glamour renders this once and never diffs into it (see `patch`). The renderers come
// from `opts.slots` (stashed on `dispatch.__slots`). A `kind` with no registered renderer
// falls back to an inert code block showing the payload — so the source is still visible.
function mountSlot(doc, node, dispatch) {
  const render = (dispatch.__slots || {})[node.slot];
  if (typeof render === "function") {
    const el = render(doc, node.data);
    if (el) return el;
  }
  const pre = doc.createElement("pre");
  pre.setAttribute("class", "glamour-slot");
  const code = doc.createElement("code");
  code.appendChild(doc.createTextNode(node.data));
  pre.appendChild(code);
  return pre;
}

// (BUG-017) Membership test for a `UiFetch` method-set carried on the `http` wire cmd.
// `methods` is spelled with any of `,`/space/`|` separators; a method is allowed only as a
// whole, case-insensitive token — mirroring glamour's `method_allowed`.
function methodAllowed(methods, method) {
  const norm = String(methods).toUpperCase().replace(/[ |]/g, ",");
  return ("," + norm + ",").includes("," + String(method).toUpperCase() + ",");
}

// URL-bearing attributes whose value is a navigation/fetch target: a `javascript:`
// (or `data:text/html`) URL here is a script sink, so the value is scheme-checked.
const URL_ATTRS = new Set(["href", "src", "action", "formaction", "poster", "xlink:href"]);

// Keep relative URLs and the safe schemes; collapse everything else (above all
// `javascript:`) to an inert `#`. `data:` is allowed only for inline images.
function safeUrl(value) {
  const s = String(value).trim();
  // (BUG-283) Reject on any ASCII C0 control or DEL BEFORE scheme detection. Browsers strip
  // TAB/LF/CR from URLs during canonicalization, so `java\nscript:alert(1)` becomes
  // `javascript:` only after the shell has already written it. Release-facing URLs have no
  // legitimate raw controls — fail closed to `#`.
  if (/[\u0000-\u001F\u007F]/.test(s)) return "#";
  // A single leading slash is a same-origin relative path; `//host` is protocol-relative
  // (off-origin navigation) — SEC-025 — so the slash branch rejects a second slash.
  if (/^(https?:|mailto:|tel:|\.|#|\?|\/(?!\/))/i.test(s)) return s; // relative or known-safe scheme
  // Inline images only, and only raster — `data:image/svg+xml` can carry script (SEC-026).
  if (/^data:image\/(png|jpe?g|gif|webp)[;,]/i.test(s)) return s;
  return "#";                                                  // javascript:, data:text/html, svg, //host → inert
}

// Apply one wire-format attribute to an element. This is the SINGLE DOM-write choke
// point — both the create (`createNode`) and update (`reconcileAttrs`) paths route
// here — so two structural XSS guards live in exactly one place:
//   - event-handler attributes (`on*`) are NEVER written; handlers attach only via
//     the typed `on(event, msg)` path, so `prop("onclick", "alert(1)")` is inert;
//   - URL attributes are scheme-checked, so a `javascript:` href can't reach the DOM.
function applyAttr(el, attr, dispatch) {
  const [kind, a, b] = attr;
  if (kind === "prop") {
    if (/^on/i.test(a)) return;                              // no string event handlers, ever
    if (DANGEROUS_ATTRS.has(String(a).toLowerCase())) return; // (BUG-260) srcdoc is an HTML sink
    if (URL_ATTRS.has(a.toLowerCase())) el.setAttribute(a, safeUrl(b));
    else el.setAttribute(a, b);
  } else if (kind === "on") {
    // `b` is the msg VALUE (its wire JSON). The handler hands it straight back to
    // the rune via dispatch — events are data, the shell performs no logic.
    addHandler(el, a, b, dispatch);
  } else if (kind === "oninput") {
    // `b` is the msg-variant TAG; on the event, dispatch `tag(target.value)` so the
    // rune receives the field's value as data (the forms/controlled-input path).
    addInputHandler(el, a, b, dispatch);
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

// Like addHandler, but dispatches the variant `tag` carrying the target's current value —
// the value is read at event time (so it reflects the keystroke), then handed to the rune
// as data. Tracked in the same per-element map so re-renders replace it cleanly.
function addInputHandler(el, event, tag, dispatch) {
  let perEl = HANDLERS.get(el);
  if (!perEl) {
    perEl = new Map();
    HANDLERS.set(el, perEl);
  }
  const existing = perEl.get(event);
  if (existing) el.removeEventListener(event, existing.fn);
  const fn = (e) => dispatch({ $variant: tag, $values: [e && e.target ? e.target.value : ""] });
  el.addEventListener(event, fn);
  perEl.set(event, { fn, msg: tag });
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
    else if (kind === "on" || kind === "oninput") oldEvents.add(a);
  }
  const newProps = new Set();
  const newEvents = new Set();
  for (const attr of newAttrs) {
    const [kind, a] = attr;
    if (kind === "prop") newProps.add(a);
    else if (kind === "on" || kind === "oninput") newEvents.add(a);
    applyAttr(el, attr, dispatch); // setAttribute / (re)addHandler with the new msg
  }
  // Drop props/handlers no longer present.
  for (const name of oldProps) if (!newProps.has(name)) el.removeAttribute(name);
  for (const event of oldEvents) if (!newEvents.has(event)) removeHandler(el, event);
}
