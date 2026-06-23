// Same-origin client for the coven read API (proxied by coven-web). Names encode
// `/` as `~` because coven does NO url-decoding (PLAN.md §4, src/pm/wire.rs:92).
import type { IndexResp, CatalogResp, VersionsResp, Record, Snapshot, Timestamp, SourceResp } from "./types";

export function wireName(n: string): string {
  return n.replace(/\//g, "~");
}

async function getJSON<T>(path: string): Promise<T> {
  const r = await fetch(path, { credentials: "omit" });
  if (!r.ok) throw new Error("HTTP " + r.status);
  return (await r.json()) as T;
}

export function getIndex(): Promise<IndexResp> {
  return getJSON<IndexResp>("/api/coven/index");
}

export function getCatalog(): Promise<CatalogResp> {
  return getJSON<CatalogResp>("/api/coven/catalog");
}

export function getVersions(name: string): Promise<VersionsResp> {
  return getJSON<VersionsResp>("/api/coven/versions?name=" + wireName(name));
}

export function getRecord(name: string, version: string): Promise<Record> {
  return getJSON<Record>(
    "/api/coven/record?name=" + wireName(name) + "&version=" + encodeURIComponent(version),
  );
}

export function getSource(name: string, version: string): Promise<SourceResp> {
  return getJSON<SourceResp>(
    "/api/coven/source?name=" + wireName(name) + "&version=" + encodeURIComponent(version),
  );
}

export async function getRootpub(): Promise<string> {
  // Served as raw hex (the proxy labels it application/json); read as text.
  const r = await fetch("/api/coven/rootpub", { credentials: "omit" });
  if (!r.ok) throw new Error("HTTP " + r.status);
  return (await r.text()).trim();
}

export function getSnapshot(): Promise<Snapshot> {
  return getJSON<Snapshot>("/api/coven/snapshot");
}

export function getTimestamp(): Promise<Timestamp> {
  return getJSON<Timestamp>("/api/coven/timestamp");
}

export interface PromoteResult {
  record: Record;
  separation_of_duties: boolean;
  delta_runtime: string[];
}

async function postJSON<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(path, {
    method: "POST",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const data = (await r.json()) as T & { error?: string };
  if (!r.ok) throw new Error(data.error ?? "HTTP " + r.status);
  return data;
}

// The two-phase release: a STAGED version becomes RELEASED only with a non-empty
// second factor and a promoter distinct from the uploader (coven enforces SoD).
export function promote(
  name: string,
  version: string,
  secondFactor: string,
  promotedBy: string,
): Promise<PromoteResult> {
  return postJSON<PromoteResult>("/api/coven/promote", {
    name,
    version,
    second_factor: secondFactor,
    promoted_by: promotedBy,
  });
}

export function yank(name: string, version: string): Promise<{ ok: boolean }> {
  return postJSON<{ ok: boolean }>("/api/coven/yank", { name, version });
}

// The in-sandbox renderer bundle (witchy-runtime shim + VNode->DOM walk + the
// glamour highlighter WASM, base64-inlined at build time). The parent fetches it
// same-origin (`connect-src 'self'`) and injects it as TEXT into the sandbox
// frame; it is NEVER executed in the parent.
export async function getSourceSandboxJs(): Promise<string> {
  const r = await fetch("/source-sandbox.js", { credentials: "omit" });
  if (!r.ok) throw new Error("HTTP " + r.status);
  return r.text();
}
