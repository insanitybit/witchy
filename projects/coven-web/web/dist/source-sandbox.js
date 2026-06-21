// In-sandbox renderer. This source is fetched as TEXT by the trusted parent and
// inlined into the inner srcdoc iframe — it executes only inside the opaque-origin,
// `connect-src 'none'` sandbox, never in the parent page.
//
// It syntax-highlights witchy source with a self-contained, zero-dependency
// tokenizer. The source bytes only ever reach the DOM through `textContent` on
// `createElement`'d spans — never an HTML string — so even inside this isolated
// frame an injection sink does not exist. The token `class` names are a fixed,
// renderer-controlled set; the colours live in an injected `<style>` (allowed by
// the sandbox CSP's `style-src 'unsafe-inline'`).
(function () {
  "use strict";

  var port = null;

  // --- witchy lexicon (kept in sync with src/lexer.rs) ----------------------
  var KEYWORDS = {};
  (function () {
    var ws = ("fn let var own move if else match for in while return break " +
      "continue type trait impl import pub as gen yield comptime region retain " +
      "without where async await mode").split(" ");
    for (var i = 0; i < ws.length; i++) KEYWORDS[ws[i]] = 1;
  })();
  var LITERALS = { "true": 1, "false": 1, "nil": 1 };

  var STYLE = [
    "pre.hl{margin:0;padding:12px 14px;background:#f6f8fa;color:#24292e;",
    "font:13px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;",
    "white-space:pre-wrap;word-break:break-word;border-radius:6px;overflow:auto}",
    ".hl-c{color:#6a737d;font-style:italic}", // comment
    ".hl-s{color:#032f62}",                    // string
    ".hl-n{color:#005cc5}",                    // number
    ".hl-k{color:#d73a49}",                    // keyword
    ".hl-l{color:#005cc5}",                    // literal (true/false/nil)
    ".hl-t{color:#6f42c1}"                     // Title-case type / constructor
  ].join("");

  function isIdentStart(c) { return /[A-Za-z_]/.test(c); }
  function isIdentPart(c) { return /[A-Za-z0-9_]/.test(c); }

  // Scan a "..." string from `i` (a `"`), tolerating `\` escapes and `${ ... }`
  // interpolation (a `"` inside an interpolation does not close the literal).
  // Returns the index just past the closing quote (or EOF).
  function scanString(src, i) {
    var n = src.length, j = i + 1, depth = 0;
    while (j < n) {
      var c = src.charAt(j);
      if (c === "\\") { j += 2; continue; }
      if (c === "$" && src.charAt(j + 1) === "{") { depth++; j += 2; continue; }
      if (c === "}" && depth > 0) { depth--; j++; continue; }
      if (c === '"' && depth === 0) { j++; break; }
      j++;
    }
    return j;
  }

  // Scan a (possibly nested) `/* ... */` block comment from `i`.
  function scanBlockComment(src, i) {
    var n = src.length, j = i + 2, depth = 1;
    while (j < n && depth > 0) {
      if (src.charAt(j) === "/" && src.charAt(j + 1) === "*") { depth++; j += 2; continue; }
      if (src.charAt(j) === "*" && src.charAt(j + 1) === "/") { depth--; j += 2; continue; }
      j++;
    }
    return j;
  }

  // Scan a number from `i`: decimal/hex/binary with `_` separators, an optional
  // fraction, and an optional duration suffix (ms|hr|s|m|h|d|w, longest first).
  function scanNumber(src, i) {
    var n = src.length, j = i, c1 = src.charAt(i + 1);
    if (src.charAt(i) === "0" && (c1 === "x" || c1 === "X")) {
      j += 2; while (j < n && /[0-9a-fA-F_]/.test(src.charAt(j))) j++; return j;
    }
    if (src.charAt(i) === "0" && (c1 === "b" || c1 === "B")) {
      j += 2; while (j < n && /[01_]/.test(src.charAt(j))) j++; return j;
    }
    while (j < n && /[0-9_]/.test(src.charAt(j))) j++;
    if (src.charAt(j) === "." && /[0-9]/.test(src.charAt(j + 1))) {
      j++; while (j < n && /[0-9_]/.test(src.charAt(j))) j++;
    }
    var two = src.substr(j, 2);
    if (two === "ms" || two === "hr") { j += 2; }
    else { var s = src.charAt(j); if (s === "s" || s === "m" || s === "h" || s === "d" || s === "w") j++; }
    return j;
  }

  // Tokenize `src` into a list of {t, v} where `t` is the class suffix ("c"/"s"/
  // "n"/"k"/"l"/"t") or "p" for plain text. Every character is covered exactly
  // once, so joining the values reproduces the input byte-for-byte.
  function tokenize(src) {
    var toks = [], n = src.length, i = 0, plainStart = -1;
    function flushPlain(end) {
      if (plainStart >= 0 && end > plainStart) toks.push({ t: "p", v: src.slice(plainStart, end) });
      plainStart = -1;
    }
    while (i < n) {
      var c = src.charAt(i), c2 = src.charAt(i + 1), j;
      if (c === "/" && c2 === "/") {
        flushPlain(i); j = src.indexOf("\n", i); if (j < 0) j = n;
        toks.push({ t: "c", v: src.slice(i, j) }); i = j;
      } else if (c === "/" && c2 === "*") {
        flushPlain(i); j = scanBlockComment(src, i);
        toks.push({ t: "c", v: src.slice(i, j) }); i = j;
      } else if (c === '"') {
        flushPlain(i); j = scanString(src, i);
        toks.push({ t: "s", v: src.slice(i, j) }); i = j;
      } else if (c >= "0" && c <= "9") {
        flushPlain(i); j = scanNumber(src, i);
        toks.push({ t: "n", v: src.slice(i, j) }); i = j;
      } else if (isIdentStart(c)) {
        flushPlain(i); j = i + 1;
        while (j < n && isIdentPart(src.charAt(j))) j++;
        var w = src.slice(i, j);
        var t = KEYWORDS[w] ? "k" : (LITERALS[w] ? "l" : (/^[A-Z]/.test(w) ? "t" : "p"));
        toks.push({ t: t, v: w }); i = j;
      } else {
        if (plainStart < 0) plainStart = i; i++;
      }
    }
    flushPlain(n);
    return toks;
  }

  function ensureStyle() {
    if (document.getElementById("hl-style")) return;
    var st = document.createElement("style");
    st.id = "hl-style";
    st.textContent = STYLE; // fixed, renderer-controlled CSS — never source-derived
    (document.head || document.documentElement).appendChild(st);
  }

  // Build a highlighted <pre>. Token text is set via textContent ONLY; the only
  // attribute set from data is a class drawn from the fixed set above.
  function highlight(text) {
    var pre = document.createElement("pre");
    pre.className = "hl";
    var toks = tokenize(text);
    for (var i = 0; i < toks.length; i++) {
      var tk = toks[i];
      if (tk.t === "p") {
        pre.appendChild(document.createTextNode(tk.v));
      } else {
        var span = document.createElement("span");
        span.className = "hl-" + tk.t;
        span.textContent = tk.v;
        pre.appendChild(span);
      }
    }
    return pre;
  }

  function render(text) {
    ensureStyle();
    var out = document.getElementById("out");
    while (out.firstChild) out.removeChild(out.firstChild);
    out.appendChild(highlight(text));
    return (out.scrollHeight || document.body.scrollHeight || 0);
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
        var h = render(m.text == null ? "" : String(m.text));
        networkIsBlocked().then(function (blocked) {
          port.postMessage({ type: "height", px: h, networkBlocked: blocked });
        });
      }
    };
    port.postMessage({ type: "ready" });
  });

  // Test seam: expose the pure tokenizer to a CommonJS harness (Node). In the
  // browser `module` is undefined, so this is a no-op there.
  if (typeof module !== "undefined" && module.exports) module.exports = { tokenize: tokenize };
})();
