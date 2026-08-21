#!/usr/bin/env node
// RFC-0015 Phase D: test the committed d3-runes-chart compartment renderer's
// data-only chart core. The renderer is intentionally self-contained in index.html so
// the opaque-origin compartment does not need network/module fetches.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
const html = readFileSync(join(here, "index.html"), "utf8");
const script = html.match(/<script>\n([\s\S]*?)\n  <\/script>/);
if (!script) throw new Error("chart.test.mjs: inline renderer script not found");

const listeners = new Map();
const sandbox = {
  Number,
  Math,
  JSON,
  Array,
  Object,
  String,
  globalThis: null,
  addEventListener(type, handler) {
    listeners.set(type, handler);
  },
};
sandbox.globalThis = sandbox;
vm.runInNewContext(script[1], sandbox, { filename: "d3-runes-chart/index.html<script>" });

const chart = sandbox.__runesChart;
if (!chart || typeof chart.barChartSvg !== "function") {
  throw new Error("chart.test.mjs: __runesChart.barChartSvg was not exported");
}

let failures = 0;
function ok(condition, message) {
  console.log(`  ${condition ? "ok" : "FAIL"}: ${message}`);
  if (!condition) failures++;
}

function rects(svg) {
  return [...svg.matchAll(/<rect\b([^>]*)>/g)].map((match) => match[1]);
}

function attr(fragment, name) {
  const match = fragment.match(new RegExp(`${name}="([^"]*)"`));
  return match ? match[1] : "";
}

const hostile = `"><script>globalThis.pwned=1</script><rect height="999"`;
const svg = chart.barChartSvg([
  { label: "one", count: 2 },
  { label: hostile, count: 6 },
  { label: "bad", count: "not numeric" },
]);
const bars = rects(svg);

ok(bars.length === 3, "one bar is emitted per input point");
ok(Number(attr(bars[1], "height")) > Number(attr(bars[0], "height")), "larger counts scale to taller bars");
ok(attr(bars[2], "data-count") === "0", "non-numeric counts are clamped to zero");
ok(!svg.includes(hostile), "labels are not interpolated into the SVG");
ok(!svg.includes("<script>") && !svg.includes("height=\"999\""), "hostile label markup cannot inject SVG/HTML");

const parsed = chart.parseGrant(JSON.stringify([{ label: hostile, count: 4 }]));
ok(parsed.length === 1 && parsed[0].count === 4, "grant JSON parses to numeric chart points");

const bad = chart.parseGrant("{not json");
ok(Array.isArray(bad) && bad.length === 0, "malformed grants fail closed to an empty chart");

if (failures > 0) {
  console.error(`\nRUNES-CHART FAILED (${failures} check(s))`);
  process.exit(1);
}

console.log("\nRUNES-CHART OK");
