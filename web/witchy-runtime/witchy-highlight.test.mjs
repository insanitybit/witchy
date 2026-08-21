#!/usr/bin/env node
// RFC-0041 Phase 0: the shared witchy highlighter is a data-only function, so it is unit-tested
// in Node with no DOM. Asserts keywords/types/strings/comments/numbers colour correctly, the
// retired `restrict` verb is NOT painted as a builtin, and text is HTML-escaped.
//
// Usage:  node web/witchy-runtime/witchy-highlight.test.mjs

import { escapeHtml, highlightWitchy, highlightShell, highlightToml } from "../witchy-highlight.js";

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

// Keywords + a Title-case type + a builtin.
const h1 = highlightWitchy('fn main(console: Console):\n    console.print("hi")');
ok(h1.includes('<span class="t-kw">fn</span>'), "`fn` is a keyword");
ok(h1.includes('<span class="t-type">Console</span>'), "a Title-case name is a type");
ok(h1.includes('<span class="t-builtin">print</span>'), "`print` is a builtin");
ok(h1.includes('<span class="t-str">'), "a string literal is coloured");

// RFC-0130: generator declarations and yields are keywords, and highlighting is
// lossless so an editor can round-trip the exact source text.
const generator = "gen fn values() -> Iter(Int):\n    yield 1";
const hGenerator = highlightWitchy(generator);
ok(hGenerator.includes('<span class="t-kw">gen</span>'), "`gen` is a keyword");
ok(hGenerator.includes('<span class="t-kw">yield</span>'), "`yield` is a keyword");
ok(hGenerator.replace(/<[^>]+>/g, "") === escapeHtml(generator), "generator syntax reconstructs the escaped source exactly");

// RFC-0122 explicit reference types keep the lifetime together and classify the
// mutable access keyword. The highlighter must still reconstruct the source exactly.
const reference = "fn edit(value: &'a mut String) -> &'a mut String:";
const hRef = highlightWitchy(reference);
ok(hRef.includes('<span class="t-type">\'a</span>'), "an explicit reference lifetime is coloured");
ok(hRef.includes('<span class="t-kw">mut</span>'), "mutable reference access is coloured as a keyword");
ok(hRef.includes('<span class="t-type">String</span>'), "the referenced type is coloured");
ok(hRef.replace(/<[^>]+>/g, "") === escapeHtml(reference), "reference syntax reconstructs the escaped source exactly");

// Capability keywords (RFC-0038/0039 surface).
const h2 = highlightWitchy("grantable capability UiRoot:");
ok(h2.includes('<span class="t-kw">grantable</span>'), "`grantable` is a keyword");
ok(h2.includes('<span class="t-kw">capability</span>'), "`capability` is a keyword");
ok(highlightWitchy("mode opt").includes('<span class="t-kw">mode</span>'), "`mode` is a keyword");
ok(h2.includes('<span class="t-type">UiRoot</span>'), "the capability name is a type");

// `${…}` interpolation is coloured distinctly inside a string.
const h3 = highlightWitchy('let s = "area: ${area(x)}"');
ok(h3.includes('<span class="t-subst">${area(x)}</span>'), "string interpolation is a distinct token");

// A comment.
ok(highlightWitchy("// a note\nfn f():").includes('<span class="t-comment">// a note</span>'), "a comment is coloured");

// A number with a duration suffix.
ok(highlightWitchy("let d = 1500ms").includes('<span class="t-num">1500ms</span>'), "a duration literal is a number");

// The RETIRED `restrict` verb must NOT be painted as a builtin (it would mislead).
ok(!highlightWitchy("let x = restrict(net, a)").includes('<span class="t-builtin">restrict</span>'), "retired `restrict` is not a builtin");
// The current `only` verb IS a builtin.
ok(highlightWitchy("let x = net.only(p)").includes('<span class="t-builtin">only</span>'), "`only` is a builtin");

// HTML in the source is escaped (no injection through the highlighter).
const h4 = highlightWitchy("a < b > c & d");
ok(h4.includes("&lt;") && h4.includes("&gt;") && h4.includes("&amp;"), "HTML metacharacters are escaped");
ok(!h4.includes("<b>"), "raw `<b>` never reaches the output");

// --- Shell + TOML highlighters (the book's `sh` / `toml` fences) ---
const sh = highlightShell("witchy sandbox --dir . f.witchy # note");
ok(sh.includes('<span class="t-kw">--dir</span>'), "a shell flag is coloured");
ok(sh.includes('<span class="t-comment"># note</span>'), "a shell comment is coloured");
ok(highlightShell('echo "<b>" # <i>').includes("&lt;b&gt;") && !highlightShell('echo "<b>"').includes("<b>"), "shell highlighter escapes HTML (no injection)");

const toml = highlightToml('[rune]\nname = "app"\nlisten = true\nport = 8080');
ok(toml.includes('<span class="t-type">[rune]</span>'), "a TOML section is coloured");
ok(toml.includes('<span class="t-builtin">name</span>'), "a TOML key is coloured");
ok(toml.includes('<span class="t-str">"app"</span>'), "a TOML string is coloured");
ok(toml.includes('<span class="t-lit">true</span>'), "a TOML boolean is coloured");
ok(highlightToml('x = "<script>"').includes("&lt;script&gt;"), "TOML highlighter escapes HTML (no injection)");

if (failures > 0) {
  console.error(`\nWITCHY-HIGHLIGHT FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nWITCHY-HIGHLIGHT OK");
process.exit(0);
