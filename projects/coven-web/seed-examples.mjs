// Seed a local coven with witchy's real example runes, so coven-web has a
// populated, browsable, searchable registry (not a single demo rune).
//
//   COVEN_URL=http://127.0.0.1:8787 node projects/coven-web/seed-examples.mjs
//
// Each example under examples/<dir> is published as `examples/<entry>` (coven
// requires a `namespace/name`). We DECLARE a capability superset and let coven
// recompute + store the REAL footprint from source (it rejects only UNDER-declared
// manifests, and ignores the surplus). Runes that don't compile standalone (path
// deps, etc.) simply 4xx and are skipped — the self-contained majority seed fine.
import { readFileSync, readdirSync, existsSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..", "..");
const EX = join(REPO, "examples");
const COVEN = process.env.COVEN_URL || "http://127.0.0.1:8787";

// coven stores the recomputed footprint, so over-declaring here is harmless; it
// only needs to be a superset of every example's real footprint.
const ALLCAPS = ["Console", "Net", "Dir", "Clock", "Env", "Random", "SecretStore"];

function witchyFiles(dir) {
  const sdir = join(dir, "src");
  if (!existsSync(sdir)) return [];
  return readdirSync(sdir).filter((f) => f.endsWith(".witchy") && !f.endsWith("_test.witchy"));
}

async function publish(name, manifest, files) {
  const r = await fetch(COVEN + "/coven/publish", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ manifest_toml: manifest, uploaded_by: "examples-bot", source: { files } }),
  });
  return { status: r.status, text: await r.text() };
}

const dirs = readdirSync(EX)
  .filter((d) => {
    const p = join(EX, d);
    return statSync(p).isDirectory() && existsSync(join(p, "witchy.toml"));
  })
  .sort();

let ok = 0;
const failures = [];
for (const d of dirs) {
  const dir = join(EX, d);
  const files = witchyFiles(dir);
  // coven derives the entry as src/<last-name-segment>.witchy, so name the rune
  // after its real entry file (prefer the file matching the dir).
  const main = files.includes(d + ".witchy") ? d + ".witchy" : files[0];
  if (!main) continue;
  const name = "examples/" + main.replace(/\.witchy$/, "");
  const toml = readFileSync(join(dir, "witchy.toml"), "utf8");
  const version = (toml.match(/version\s*=\s*"([^"]+)"/) || [, "0.1.0"])[1];
  const manifest =
    `[rune]\nname = "${name}"\nversion = "${version}"\n\n` +
    `[capabilities]\nruntime = [${ALLCAPS.map((c) => `"${c}"`).join(", ")}]\n`;
  const source = [
    ["witchy.toml", manifest],
    ...files.map((f) => ["src/" + f, readFileSync(join(dir, "src", f), "utf8")]),
  ];
  try {
    const res = await publish(name, manifest, source);
    if (res.status === 200) {
      ok++;
      console.log("ok  ", name + "@" + version);
    } else {
      failures.push([name, res.status, res.text.slice(0, 120)]);
    }
  } catch (e) {
    failures.push([name, "ERR", e.message]);
  }
}

console.log(`\nseeded ${ok} runes; ${failures.length} skipped`);
for (const [n, s, t] of failures) console.log("  skip", n, s, t);
