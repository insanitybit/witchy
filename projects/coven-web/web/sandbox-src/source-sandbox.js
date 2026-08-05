// In-sandbox renderer (WS-I/M6: the glamour-WASM source highlighter).
//
// This module is esbuild-bundled into dist/source-sandbox.js (an IIFE) by
// build.sh. The trusted parent fetches that bundle as TEXT and inlines it into
// the inner srcdoc iframe — it executes ONLY inside the opaque-origin,
// `connect-src 'none'` sandbox, never in the parent page.
//
// It highlights witchy source by running the glamour `highlighter` rune, compiled
// to footprint-empty WASM, under the RFC-0007 pure-compute host (the bundled
// witchy-runtime shim, which implements only the non-capability "witchy" imports
// and DENIES every capability import — a capability-using module simply fails to
// instantiate). The rune returns a glamour VNode tree as JSON; we build the DOM
// from it with createElement / textContent / setAttribute ONLY — never an
// HTML-string sink — so untrusted source containing `<script>` lands in real text
// nodes, inert. The token `class` names are produced by the rune from a fixed set;
// the colours live in an injected `<style>` (sandbox CSP `style-src 'unsafe-inline'`).
//
// The wasm bytes are NOT fetched here (the frame is network-firewalled,
// `connect-src 'none'`): the highlighter module is base64-INLINED into this
// bundle at build time (build.sh injects it where `__HIGHLIGHTER_WASM_B64__` is
// referenced). The trusted parent fetches this whole bundle same-origin and
// injects it as TEXT into the frame; the parent then sends only the source text
// over the private MessageChannel. See web/src/sandbox.ts (parent) and PLAN.md §5.2.

import { instantiate } from "../../../../web/witchy-runtime/witchy-runtime.mjs";

// The glamour highlighter WASM, base64. build.sh replaces this placeholder with
// the real bytes after compiling the rune. If the placeholder survives (e.g. an
// incomplete build), instantiation fails and the renderer falls back to inert text.
var HIGHLIGHTER_WASM_B64 = "__HIGHLIGHTER_WASM_B64__";

// Decode base64 -> Uint8Array. `atob` is always available in a browser realm
// (no network); we map to bytes by char code.
function b64ToBytes(b64) {
  var bin = atob(b64);
  var out = new Uint8Array(bin.length);
  for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

(function () {
  "use strict";

  var port = null;
  // The render export of the glamour `highlighter` rune (`pub fn export_render`).
  var RENDER_EXPORT = "__export_export_render";
  // Decoded once; reused for every render in this frame.
  var WASM_BYTES = null;
  function highlighterBytes() {
    if (WASM_BYTES === null) WASM_BYTES = b64ToBytes(HIGHLIGHTER_WASM_B64);
    return WASM_BYTES;
  }

  // Token-class colours. The class names ("kw"/"str"/"com"/"num") are emitted by
  // the highlighter rune from a FIXED, renderer-controlled set — never derived
  // from the source text — so this stylesheet is a closed allowlist of classes.
  // A dark "witchy" theme, cohesive with the parent app's palette. Capabilities
  // get their own GOLD class (the highlighter rune emits `span.cap` for host caps)
  // so a reader's eye lands on exactly the tokens that carry authority — the same
  // visual language coven-web uses for capability chips.
  var STYLE = [
    "html,body{margin:0;background:#0f0b16}",
    "pre{margin:0;padding:14px 16px;background:#0f0b16;color:#d8cfe8;",
    "font:13px/1.6 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;",
    "white-space:pre-wrap;word-break:break-word;border-radius:8px;overflow:auto;",
    "-webkit-font-smoothing:antialiased}",
    "code{font:inherit}",
    "span.com{color:#6f6585;font-style:italic}", // comment
    "span.str{color:#8fe3a8}",                    // string
    "span.num{color:#f0a878}",                    // number
    "span.kw{color:#c99cff}",                     // keyword
    "span.cap{color:#ffd479;font-weight:600}"     // capability (authority)
  ].join("");

  function ensureStyle() {
    if (document.getElementById("hl-style")) return;
    var st = document.createElement("style");
    st.id = "hl-style";
    st.textContent = STYLE; // fixed, renderer-controlled CSS — never source-derived
    (document.head || document.documentElement).appendChild(st);
  }

  // VNode -> DOM. createElement / createTextNode / setAttribute ONLY — there is NO
  // HTML-string sink (no innerHTML / insertAdjacentHTML), so a `text` node's
  // content can NEVER become markup: a literal `<script>` in the source lands in a
  // real text node, inert. Mirrors web/witchy-runtime/glamour-dom.mjs `createNode`
  // for the prop/text subset the highlighter emits (it has no `on` bindings).
  function renderVNode(v) {
    if (v != null && typeof v.text === "string") {
      return document.createTextNode(v.text);
    }
    if (v == null || typeof v.el !== "string") {
      throw new Error("malformed vnode: " + JSON.stringify(v));
    }
    var el = document.createElement(v.el);
    var attrs = v.attrs || [];
    for (var i = 0; i < attrs.length; i++) {
      var kind = attrs[i][0], name = attrs[i][1], value = attrs[i][2];
      if (kind === "prop") {
        el.setAttribute(name, value);
      } else {
        // The highlighter emits only `prop`; an `on` binding (event handler) would
        // be untrusted-source-derived behaviour — refuse it.
        throw new Error("unexpected attr kind `" + kind + "` (highlighter emits only props)");
      }
    }
    var kids = v.kids || [];
    for (var k = 0; k < kids.length; k++) el.appendChild(renderVNode(kids[k]));
    return el;
  }

  // Instantiate the highlighter WASM under the pure-compute host and render the
  // source. Returns the out element's scrollHeight (for the parent's auto-size).
  async function renderSource(text) {
    ensureStyle();
    var out = document.getElementById("out");
    while (out.firstChild) out.removeChild(out.firstChild);

    var src = text == null ? "" : String(text);
    var node;
    try {
      var rt = await instantiate(highlighterBytes());
      var json = rt.callString(RENDER_EXPORT, JSON.stringify({ src: src }));
      var tree = JSON.parse(json);
      if (tree && tree.error) throw new Error("highlighter: " + tree.error);
      node = renderVNode(tree);
    } catch (e) {
      // Fail closed and visible: if the WASM highlighter cannot run, show the raw
      // source as INERT text (textContent only — still no markup sink), and surface
      // the error. The source is never injected as HTML.
      var pre = document.createElement("pre");
      pre.textContent = src;
      out.appendChild(pre);
      var err = document.createElement("div");
      err.style.color = "#b00";
      err.textContent = "highlighter unavailable: " + (e && e.message ? e.message : String(e));
      out.appendChild(err);
      return out.scrollHeight || document.body.scrollHeight || 0;
    }
    out.appendChild(node);
    return out.scrollHeight || document.body.scrollHeight || 0;
  }

  // A small, non-sensitive DOM summary reported back to the parent: how many
  // classed highlight spans rendered, how many REAL <script> elements exist (must
  // be 0 — the injection probe must never form markup), and whether the literal
  // text "<script>" is present as inert text. Only structured counts cross the
  // boundary, never DOM content.
  function domSummary() {
    var out = document.getElementById("out");
    return {
      kw: out.querySelectorAll("span.kw").length,
      str: out.querySelectorAll("span.str").length,
      com: out.querySelectorAll("span.com").length,
      spans: out.querySelectorAll("span").length,
      realScripts: out.querySelectorAll("script").length,
      scriptAsText: out.textContent.indexOf("<script>") >= 0
    };
  }

  // Prove the network firewall: under `connect-src 'none'` (and an opaque origin)
  // this fetch MUST reject. We report the result so the parent can display it.
  function networkIsBlocked() {
    try {
      return fetch("/api/coven/index")
        .then(function () { return false; })
        .catch(function () { return true; });
    } catch (e) {
      return Promise.resolve(true);
    }
  }

  window.addEventListener("message", function (e) {
    if (!e.data || e.data.type !== "port" || !e.ports.length) return;
    port = e.ports[0];
    port.onmessage = function (ev) {
      var m = ev.data || {};
      if (m.type === "render") {
        renderSource(m.text).then(function (h) {
          var dom = domSummary();
          networkIsBlocked().then(function (blocked) {
            port.postMessage({ type: "height", px: h, networkBlocked: blocked, dom: dom });
          });
        });
      }
    };
    port.postMessage({ type: "ready" });
  });
})();
