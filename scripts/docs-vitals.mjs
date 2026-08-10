#!/usr/bin/env node
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const args = process.argv.slice(2);
if (args.length < 1) {
  console.error("Usage: node scripts/docs-vitals.mjs <url> [<url> ...]");
  process.exit(1);
}

const urls = args.map((url) => url.replace(/\/?$/, "/"));
const runs = Number.parseInt(process.env.WITCHY_LHCI_RUNS ?? "1", 10);
if (!Number.isInteger(runs) || runs < 1) {
  console.error("WITCHY_LHCI_RUNS must be a positive integer");
  process.exit(1);
}

const outRoot = path.resolve(process.env.WITCHY_VITALS_OUT ?? path.join("projects", "glamour", "acceptance", "vitals"));
fs.mkdirSync(outRoot, { recursive: true });

const lhrsDir = outRoot;
const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "witchy-docs-vitals-"));
const configPath = path.join(tmpDir, "lighthouserc.json");

const config = {
  ci: {
    collect: {
      numberOfRuns: runs,
      url: urls,
      settings: {
        output: ["json"],
      },
    },
    upload: {
      target: "filesystem",
      outputDir: lhrsDir,
    },
  },
};

fs.writeFileSync(configPath, JSON.stringify(config, null, 2));
console.log(`[docs-vitals] writing temporary LHCI config to ${configPath}`);
console.log(`[docs-vitals] collecting vitals for ${urls.length} URL(s), runs=${runs}`);

const result = spawnSync(
  "npx",
  ["-y", "@lhci/cli@0.14.0", "autorun", "--config", configPath],
  { stdio: "inherit", encoding: "utf8" },
);
if (result.status !== 0) {
  console.error("[docs-vitals] lighthouse collection failed");
  process.exit(result.status ?? 1);
}

const metricKeys = [
  "first-contentful-paint",
  "largest-contentful-paint",
  "total-blocking-time",
  "cumulative-layout-shift",
  "speed-index",
  "interactive",
  "max-potential-fid",
];

const reportFiles = fs
  .readdirSync(lhrsDir)
  .filter((name) => name.endsWith(".report.json"));

const records = reportFiles.map((file) => {
  const payload = JSON.parse(fs.readFileSync(path.join(lhrsDir, file), "utf8"));
  const lhr = payload.lighthouseResult ?? payload;
  const audits = lhr.audits ?? {};

  const metrics = Object.fromEntries(
    metricKeys.map((key) => {
      const audit = audits[key];
      if (!audit) return [key, { score: null, numericValue: null }];
      return [
        key,
        {
          score: audit.score ?? null,
          numericValue: audit.numericValue ?? null,
        },
      ];
    }),
  );

  return {
    url: lhr.requestedUrl || lhr.finalUrl || payload.url || "unknown",
    runType: lhr.configSettings?.runOnlyAudits ? "custom" : "full",
    performanceScore: lhr.categories?.performance?.score ?? null,
    runtime: lhr.environment?.hostUserAgent ?? null,
    metrics,
    reportFile: file,
  };
});

const summaryPath = path.join(
  outRoot,
  `docs-vitals-${new Date().toISOString().replace(/[:.]/g, "-")}.json`,
);
const summary = {
  schema: "witchy.docs.vitals.v1",
  recordedAt: new Date().toISOString(),
  runs,
  command: "node scripts/docs-vitals.mjs " + process.argv.slice(2).join(" "),
  records,
};

fs.writeFileSync(summaryPath, JSON.stringify(summary, null, 2));
console.log(`[docs-vitals] wrote summary to ${summaryPath}`);
