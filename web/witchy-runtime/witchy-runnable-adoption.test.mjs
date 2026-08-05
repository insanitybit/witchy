#!/usr/bin/env node

import { enhanceRunnableCells } from "../witchy-runnable.js";

class Node {
  constructor() { this.childNodes = []; this.parentNode = null; }
  appendChild(child) { child.parentNode = this; this.childNodes.push(child); return child; }
}

class Text extends Node {
  constructor(value) { super(); this.value = value; }
  get textContent() { return this.value; }
  set textContent(value) { this.value = String(value); }
}

class Element extends Node {
  constructor(tag) { super(); this.tag = tag; this.attributes = new Map(); this.listeners = new Map(); }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  addEventListener(name, listener) { this.listeners.set(name, listener); }
  get textContent() { return this.childNodes.map((child) => child.textContent).join(""); }
  set textContent(value) { this.childNodes = [new Text(String(value))]; }
}

const element = (tag, className, text = "") => {
  const node = new Element(tag);
  node.setAttribute("class", className);
  if (text !== "") node.appendChild(new Text(text));
  return node;
};
const root = new Element("main");
const cell = element("div", "witchy-cell");
cell.setAttribute("data-witchy-runnable", "1");
const editor = element("textarea", "witchy-editor", "seed");
for (const child of [
  editor,
  element("button", "witchy-run", "Run"),
  element("pre", "witchy-output"),
  element("pre", "witchy-stats"),
  element("button", "witchy-copy", "Copy"),
]) cell.appendChild(child);
root.appendChild(cell);

const calls = [];
const runProgram = async (source) => {
  calls.push(source);
  return { ok: true, text: `ran:${source}`, stats: {} };
};
const document = { createElement: (tag) => new Element(tag), createTextNode: (text) => new Text(text) };
const adopted = enhanceRunnableCells(root, { document, runProgram });
if (adopted.length !== 1 || adopted[0].element !== cell) throw new Error("server cell was replaced");
editor.value = "edited";
await adopted[0].run();
if (calls.join(",") !== "edited" || adopted[0].output.textContent !== "ran:edited") {
  throw new Error("adopted cell did not execute its current editor model");
}
if (enhanceRunnableCells(root, { document, runProgram }).length !== 0) {
  throw new Error("adoption was not idempotent");
}
console.log("WITCHY-RUNNABLE-ADOPTION OK");
