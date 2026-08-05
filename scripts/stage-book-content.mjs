#!/usr/bin/env node
// Stage book Markdown for the Glamour app. The compiler-generated manifest is
// authoritative about browser support; the rendered site never guesses from
// capability names or source text.

import {
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..");

const normalized = (path) => path.split(sep).join("/");

export function annotateWitchyFences(source, entries, file = "<page>") {
  const byBlock = new Map();
  for (const entry of entries) {
    if (!Number.isInteger(entry.block) || entry.block < 1) {
      throw new Error(`${file}: invalid manifest block ${entry.block}`);
    }
    if (byBlock.has(entry.block)) {
      throw new Error(`${file}: duplicate manifest block ${entry.block}`);
    }
    if (typeof entry.browser_runnable !== "boolean") {
      throw new Error(`${file}: block ${entry.block} has no browser_runnable classification`);
    }
    byBlock.set(entry.block, entry.browser_runnable);
  }

  let block = 0;
  let insideWitchy = false;
  const staged = source.split("\n").map((line) => {
    const marker = line.trimEnd();
    if (!insideWitchy && marker === "```witchy") {
      block++;
      if (!byBlock.has(block)) {
        throw new Error(`${file}: Witchy block ${block} is missing from book/examples.json`);
      }
      insideWitchy = true;
      return byBlock.get(block) ? "```witchy-runnable" : "```witchy-static";
    }
    if (insideWitchy && marker === "```") insideWitchy = false;
    return line;
  });

  if (insideWitchy) throw new Error(`${file}: unterminated Witchy fence`);
  if (block !== byBlock.size) {
    throw new Error(`${file}: manifest has ${byBlock.size} Witchy blocks, Markdown has ${block}`);
  }
  return staged.join("\n");
}

export function stageBookContent(sourceDir, outputDir, manifestPath) {
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const entriesByFile = new Map();
  for (const entry of manifest) {
    if (typeof entry.file !== "string" || !entry.file.startsWith("book/src/")) continue;
    const entries = entriesByFile.get(entry.file) || [];
    entries.push(entry);
    entriesByFile.set(entry.file, entries);
  }

  mkdirSync(outputDir, { recursive: true });
  const stagedFiles = new Set();
  for (const name of readdirSync(sourceDir).filter((file) => file.endsWith(".md")).sort()) {
    const sourcePath = resolve(sourceDir, name);
    const file = normalized(relative(REPO, sourcePath));
    const source = readFileSync(sourcePath, "utf8");
    const staged = annotateWitchyFences(source, entriesByFile.get(file) || [], file);
    writeFileSync(resolve(outputDir, basename(name)), staged);
    stagedFiles.add(file);
  }

  const missing = [...entriesByFile.keys()].filter((file) => !stagedFiles.has(file));
  if (missing.length > 0) {
    throw new Error(`manifest names unstaged book files: ${missing.join(", ")}`);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 5) {
    console.error("usage: stage-book-content.mjs <book-src> <output-dir> <examples.json>");
    process.exit(2);
  }
  stageBookContent(resolve(process.argv[2]), resolve(process.argv[3]), resolve(process.argv[4]));
}
