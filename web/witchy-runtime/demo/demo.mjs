// demo.mjs — drive the glamour frontend stack in a REAL browser. This is the
// browser analog of web/witchy-runtime/glamour-dom.test.mjs (counter) and
// highlighter.test.mjs (render), but against the live DOM instead of a fake one.
//
//   * COUNTER: mounted via glamour-dom's `mount()` — the +/- buttons dispatch a
//     Msg into the rune, the rune folds it, and the differ patches the DOM.
//   * HIGHLIGHTER: instantiated under the RFC-0007 pure-compute shim; we call its
//     `export_render({src})` via `callString` and render the returned VNode JSON
//     with createElement/textContent ONLY (never innerHTML) — the same
//     structurally-injection-free walk glamour-dom uses, inlined here so this demo
//     stays self-contained and does not depend on internals of glamour-dom.mjs.
//
// Relative imports resolve when the page is served from web/witchy-runtime/
// (open http://localhost:PORT/demo/).

import { instantiate } from "../witchy-runtime.mjs";
import { mount } from "../glamour-dom.mjs";

const status = document.getElementById("status");
const setStatus = (text, isErr = false) => {
  status.textContent = text;
  status.className = isErr ? "err" : "";
};

// Fetch a compiled rune as bytes. A 404 here means `./build.sh` was not run.
async function fetchWasm(name) {
  const res = await fetch(`./${name}.wasm`);
  if (!res.ok) {
    throw new Error(
      `could not fetch ${name}.wasm (HTTP ${res.status}) — run ./build.sh first`,
    );
  }
  return new Uint8Array(await res.arrayBuffer());
}

// --- VNode → DOM render walk (highlighter) ----------------------------------
// createElement / createTextNode / setAttribute ONLY. There is NO HTML-string
// sink (no innerHTML / insertAdjacentHTML), so a `text` node's content can never
// become markup — the `<script>` in the source lands in a real text node, inert.
// Mirrors glamour-dom.mjs `createNode` for the prop/text subset the highlighter
// emits (it has no `on` bindings).
function renderVNode(v) {
  if (v != null && typeof v.text === "string") {
    return document.createTextNode(v.text);
  }
  if (v == null || typeof v.el !== "string") {
    throw new Error(`malformed vnode: ${JSON.stringify(v)}`);
  }
  const el = document.createElement(v.el);
  for (const [kind, name, value] of v.attrs || []) {
    if (kind === "prop") el.setAttribute(name, value);
    else throw new Error(`unexpected attr kind \`${kind}\` (highlighter emits only props)`);
  }
  for (const k of v.kids || []) el.appendChild(renderVNode(k));
  return el;
}

// Tag the counter's interactive buttons with stable data-attrs so a test can find
// them deterministically. The rune labels them "-" and "+"; the differ patches in
// place, so attributes we set here survive across re-renders.
function tagCounterButtons(root) {
  for (const btn of root.querySelectorAll("button")) {
    const label = btn.textContent.trim();
    if (label === "+") btn.setAttribute("data-action", "inc");
    else if (label === "-") btn.setAttribute("data-action", "dec");
  }
}

async function main() {
  // --- 1. Counter -----------------------------------------------------------
  const counterEl = document.getElementById("counter");
  const counterBytes = await fetchWasm("counter");
  // initialModel 0 — Model = Int (a bare JSON integer round-trips).
  const app = await mount(counterBytes, counterEl, { initialModel: 0 });
  tagCounterButtons(counterEl);
  // Re-tag after every dispatch (the differ keeps the same nodes, but be safe if a
  // future rune replaces them) by wrapping the mount's dispatch is overkill; the
  // in-place differ preserves our data-attrs, so a one-time tag suffices. We also
  // mark the span for easy lookup.
  const countSpan = counterEl.querySelector("span");
  if (countSpan) countSpan.setAttribute("data-role", "count");

  // --- 2. Highlighter -------------------------------------------------------
  const highlightEl = document.getElementById("highlight");
  const highlighterBytes = await fetchWasm("highlighter");
  const { callString } = await instantiate(highlighterBytes);

  // A witchy snippet with a comment, a string, AND a literal <script> — the
  // injection probe. The <script> sits inside a string literal, so it must show
  // up as escaped/inert TEXT, never a live element.
  const src = [
    "fn main(console: Console):",
    "    // greet the world, then try to inject",
    '    let msg = "<script>alert(1)</script>"',
    "    print(console, msg)",
  ].join("\n");

  const out = callString("__export_export_render", JSON.stringify({ src }));
  const tree = JSON.parse(out);
  if (tree.error) {
    throw new Error(`highlighter returned an error: ${tree.error}`);
  }
  highlightEl.appendChild(renderVNode(tree));

  // Expose a tiny inspection summary on the page + window for the Playwright run.
  const kwCount = highlightEl.querySelectorAll("span.kw").length;
  const strCount = highlightEl.querySelectorAll("span.str").length;
  const comCount = highlightEl.querySelectorAll("span.com").length;
  const realScripts = highlightEl.querySelectorAll("script").length;
  const scriptAsText = highlightEl.textContent.includes("<script>");

  window.__demo = {
    count: () => counterEl.querySelector("span")?.textContent,
    model: () => app.getModel(),
    highlight: { kwCount, strCount, comCount, realScripts, scriptAsText },
  };

  setStatus(
    `ready — counter=${counterEl.querySelector("span")?.textContent}, ` +
    `highlight: ${kwCount} kw / ${strCount} str / ${comCount} com spans, ` +
    `real <script> elements=${realScripts}, "<script>" present as text=${scriptAsText}`,
  );
}

main().catch((e) => {
  console.error(e);
  setStatus(`demo failed: ${e.message}`, true);
});
