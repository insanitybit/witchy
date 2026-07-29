#!/usr/bin/env node

import assert from "node:assert/strict";
import { annotateWitchyFences } from "./stage-book-content.mjs";

const page = [
  "# Chapter",
  "",
  "```witchy",
  "fn main(console: Console):",
  '    console.print("hi")',
  "```",
  "",
  "```sh",
  "witchy run chapter.witchy",
  "```",
  "",
  "```witchy",
  "fn helper() -> Int:",
  "    1",
  "```",
  "",
].join("\n");

const staged = annotateWitchyFences(
  page,
  [
    { block: 1, browser_runnable: true },
    { block: 2, browser_runnable: false },
  ],
  "book/src/chapter.md",
);

assert.match(staged, /```witchy-runnable\nfn main/);
assert.match(staged, /```witchy-static\nfn helper/);
assert.match(staged, /```sh\nwitchy run chapter\.witchy/);
assert.equal(staged.endsWith("\n"), true);
assert.throws(
  () => annotateWitchyFences("```witchy\nfn main():\n    ()\n```\n", [], "missing.md"),
  /missing from book\/examples\.json/,
);
assert.throws(
  () => annotateWitchyFences("# Empty\n", [{ block: 1, browser_runnable: true }], "extra.md"),
  /manifest has 1 Witchy blocks, Markdown has 0/,
);
assert.throws(
  () => annotateWitchyFences("```witchy\nfn main():\n    ()\n```\n", [{ block: 1 }], "shape.md"),
  /has no browser_runnable classification/,
);

console.log("STAGE-BOOK-CONTENT OK");
