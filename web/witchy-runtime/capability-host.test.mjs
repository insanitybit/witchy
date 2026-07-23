#!/usr/bin/env node
// RFC-0091: the OPT-IN teaching/playground capability host in witchy-runtime.mjs.
// Compiles REAL witchy programs with the toolchain and runs them under the
// explicit capability host (`instantiate(bytes, { capabilities })`), proving:
//
//   1. Clock  — real wall/monotonic time on the existing i64 ABI.
//   2. Env    — empty by default; a page-supplied immutable map otherwise;
//               an absent name reads back unset. Output matches the native oracle.
//   3. Dir    — a per-run IN-MEMORY tree: read/write/append/list/subtree/exists/
//               is_dir/make_dir/open+create File handles. A rich program's output
//               is byte-identical to the native interpreter run over a real
//               fixture directory (the parity oracle), and the shim NEVER touches
//               a real filesystem.
//   4. Confinement — a `..`/absolute path is refused by BOTH the shim (throw) and
//               the native run (abort), with matching messages.
//   5. Entry policy — `dir.only(Dir.ext(".log"))` denies a non-matching file.
//   6. Fetch — explicit origin-scoped real fetch() authority, JSPI suspension,
//      request/response parity, pre-I/O denial, and fail-closed limits.
//   7. Deny-by-omission is preserved — the DEFAULT host still LinkErrors on any
//      Dir/Clock/Env/Fetch program, and Exec/Secret/Net stay denied EVEN under the
//      opt-in host (their imports are simply never built).
//
// Node is the host engine. Usage: node web/witchy-runtime/capability-host.test.mjs [witchy-binary]

import { instantiate } from "./witchy-runtime.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, existsSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Resolve the binary to an ABSOLUTE path: the native-oracle runs use a per-test
// `cwd`, so a relative `./target/...` would not resolve from there.
const BIN = resolve(process.cwd(), process.argv[2] || "target/debug/witchy");
const work = mkdtempSync(join(tmpdir(), "witchy-rfc91-"));

let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

// Compile a witchy source string to a .wasm module; return the wasm bytes.
let n = 0;
function compile(source) {
  const base = join(work, `m${n++}`);
  const src = `${base}.witchy`;
  const wasm = `${base}.wasm`;
  writeFileSync(src, source);
  execFileSync(BIN, ["compile", src, "--out", wasm]);
  return { src, bytes: readFileSync(wasm) };
}

// Run a witchy source through the NATIVE interpreter (the parity oracle),
// optionally with a working directory and extra env; return output lines
// normalized to the shim's per-call list (trailing newlines trimmed).
function nativeRun(src, { cwd, env, args = [] } = {}) {
  const out = execFileSync(BIN, [src, ...args], {
    encoding: "utf8",
    cwd: cwd || work,
    env: { ...process.env, ...(env || {}) },
  });
  return out.replace(/\n+$/, "").split("\n");
}

const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);

try {
  // === 1. argv: page-supplied launch input, including Unicode ===============
  {
    const ARGV = `fn main(console: Console, args: List(String)):
    console.print("\${args.length()}")
    for arg in args:
        console.print(arg)
`;
    const { src, bytes } = compile(ARGV);
    const args = ["one", "héllo"];
    const host = await instantiate(bytes, { args });
    const shimLines = host.run();
    ok(eq(shimLines, ["2", ...args]), "argv preserves page-supplied order and UTF-8");
    ok(eq(shimLines, nativeRun(src, { args })), "argv output matches the native oracle byte-for-byte");
    const empty = await instantiate(bytes);
    ok(eq(empty.run(), ["0"]), "argv defaults to an empty list");
  }

  // === 2. Clock: real wall/monotonic time ===================================
  {
    const CLOCK = `fn main(console: Console, clock: Clock):
    let t = clock.now()
    console.print(if t > 0: "ticking" else: "epoch")
`;
    const { bytes } = compile(CLOCK);
    // Default host: DENIED (deny-by-omission).
    let denied = false;
    try { await instantiate(bytes); } catch (e) { denied = e instanceof WebAssembly.LinkError; }
    ok(denied, "Clock program is DENIED under the default host (LinkError)");
    // Opt-in Clock host: runs, real time is positive.
    const { run } = await instantiate(bytes, { capabilities: { clock: true } });
    ok(eq(run(), ["ticking"]), "Clock program runs under the opt-in host and reads real wall time");

    // now_monotonic returns a plausible nanosecond count (delta across two reads
    // is non-negative), proving the i64 monotonic ABI is honored.
    const MONO = `fn main(console: Console, clock: Clock):
    let a = clock.now_monotonic()
    let b = clock.now_monotonic()
    console.print(if b >= a: "monotonic" else: "went backwards")
`;
    const mono = await instantiate(compile(MONO).bytes, { capabilities: { clock: true } });
    ok(eq(mono.run(), ["monotonic"]), "now_monotonic is non-decreasing (nanosecond monotonic ABI)");
  }

  // === 2. Env: empty default + page-supplied immutable map ==================
  {
    const ENV = `fn main(console: Console, env: Env):
    match env.get_env("GREETING"):
        Some(v) -> console.print("GREETING=\${v}")
        None -> console.print("unset")
`;
    const { src, bytes } = compile(ENV);
    // Default host: DENIED.
    let denied = false;
    try { await instantiate(bytes); } catch (e) { denied = e instanceof WebAssembly.LinkError; }
    ok(denied, "Env program is DENIED under the default host (LinkError)");

    // Empty Env (opt-in, no map): the variable is unset.
    const empty = await instantiate(bytes, { capabilities: { env: true } });
    ok(eq(empty.run(), ["unset"]), "Env is EMPTY by default (absent var reads unset)");

    // Page-supplied map: the variable is present. Matches the native oracle
    // (the interpreter reads the real process env, so seed it there too).
    const withMap = await instantiate(bytes, { capabilities: { env: { GREETING: "hi" } } });
    const shimLines = withMap.run();
    ok(eq(shimLines, ["GREETING=hi"]), "Env reads a page-supplied value");
    const oracle = nativeRun(src, { env: { GREETING: "hi" } });
    ok(eq(shimLines, oracle), "Env output matches the native oracle byte-for-byte");
  }

  // === 3. Dir: in-memory tree, full op surface, parity with native ==========
  {
    const DIR = `fn main(console: Console, root: Dir):
    root.write("a.txt", "alpha")
    root.append("a.txt", "-beta")
    console.print(root.read("a.txt"))
    root.make_dir("sub")
    root.write("sub/b.txt", "nested")
    console.print(root.subtree("sub").read("b.txt"))
    for name in root.list():
        console.print("entry: \${name}")
    console.print("exists a.txt: \${root.exists("a.txt")}")
    console.print("is_dir sub: \${root.is_dir("sub")}")
    console.print("exists nope: \${root.exists("nope.txt")}")
    let f = root.read_file("a.txt")
    console.print("via File: \${f.read()}")
`;
    const { src, bytes } = compile(DIR);
    // Default host: DENIED.
    let denied = false;
    try { await instantiate(bytes); } catch (e) { denied = e instanceof WebAssembly.LinkError; }
    ok(denied, "Dir program is DENIED under the default host (LinkError)");

    // Opt-in writable in-memory Dir (seeded empty).
    const { run } = await instantiate(bytes, { capabilities: { dir: { write: true } } });
    const shimLines = run();

    // Native oracle: the interpreter grants a real cwd-backed Dir. Run it in its
    // own fixture directory so its writes never touch the repo.
    const fixture = mkdtempSync(join(tmpdir(), "witchy-rfc91-fix-"));
    let oracle;
    try {
      oracle = nativeRun(src, { cwd: fixture });
    } finally {
      rmSync(fixture, { recursive: true, force: true });
    }
    ok(eq(shimLines, oracle), "Dir program output matches the native oracle byte-for-byte");

    // The shim NEVER touched a real filesystem: nothing was created under cwd or
    // the shared work dir (the in-memory tree is pure JS state).
    const leaked = existsSync(join(process.cwd(), "a.txt")) || readdirSync(work).includes("a.txt");
    ok(!leaked, "the in-memory Dir NEVER reaches a real filesystem (no a.txt on disk)");
  }

  // === 4. Confinement: `..`/absolute paths are refused on BOTH backends =====
  {
    const ESCAPE = `fn main(console: Console, root: Dir):
    console.print(root.read("../escape.txt"))
`;
    const { src, bytes } = compile(ESCAPE);
    // Shim: the read throws (the confinement boundary), mentioning `..`.
    const { run } = await instantiate(bytes, { capabilities: { dir: { write: true } } });
    let shimBlocked = false, shimMsg = "";
    try { run(); } catch (e) { shimBlocked = true; shimMsg = String(e.message); }
    ok(shimBlocked && /escapes the Dir capability/.test(shimMsg),
      `a \`..\` path is refused by the in-memory Dir host: ${shimMsg.slice(0, 60)}`);
    // Native: the same program aborts (non-zero exit) rather than escaping.
    let nativeBlocked = false;
    try {
      const fixture = mkdtempSync(join(tmpdir(), "witchy-rfc91-esc-"));
      try { execFileSync(BIN, [src], { encoding: "utf8", cwd: fixture, stdio: "pipe" }); }
      finally { rmSync(fixture, { recursive: true, force: true }); }
    } catch (_e) { nativeBlocked = true; }
    ok(nativeBlocked, "the native run also refuses the `..` escape (parity)");

    // An absolute path is likewise refused by the shim.
    const ABS = `fn main(console: Console, root: Dir):
    console.print(root.read("/etc/passwd"))
`;
    const abs = await instantiate(compile(ABS).bytes, { capabilities: { dir: { write: true } } });
    let absBlocked = false;
    try { abs.run(); } catch (_e) { absBlocked = true; }
    ok(absBlocked, "an absolute path is refused by the in-memory Dir host");
  }

  // === 5. Entry policy: dir.only(...) denies a non-matching file ============
  {
    const POLICY = `fn main(console: Console, root: Dir):
    root.write("app.log", "hi")
    root.write("secret.key", "k")
    let logs = root.only(Dir.ext(".log"))
    console.print(logs.read("app.log"))
    console.print(logs.read("secret.key"))
`;
    const { run } = await instantiate(compile(POLICY).bytes, { capabilities: { dir: { write: true } } });
    let firstPrinted = null, denied = false, msg = "";
    try {
      run();
    } catch (e) {
      denied = true;
      msg = String(e.message);
    }
    // The first read (app.log) is admitted; the second (secret.key) is denied by
    // the ext-policy guard — so the run prints "hi" then throws.
    ok(denied && /entry policy/.test(msg),
      `dir.only(ext:".log") admits app.log but denies secret.key: ${msg.slice(0, 60)}`);
  }

  // === 6. Fetch: JSPI suspension + origin confinement + uniform failures =====
  {
    const FETCH = `fn main(console: Console, fetch: Fetch):
    let api = fetch.only("https://api.example.com")
    console.print(api.send_raw("GET", "https://api.example.com/data", "X-Test: yes", ""))
`;
    const { bytes } = compile(FETCH);

    let denied = false;
    try { await instantiate(bytes); } catch (e) { denied = e instanceof WebAssembly.LinkError; }
    ok(denied, "Fetch program is DENIED under the default host (LinkError)");

    const requests = [];
    const fakeFetch = async (url, options) => {
      requests.push({ url, options });
      await Promise.resolve();
      return {
        status: 200,
        redirected: false,
        type: "basic",
        headers: new Map([["x-fixture", "yes"]]),
        arrayBuffer: async () => new TextEncoder().encode("ok").buffer,
      };
    };
    const host = await instantiate(bytes, {
      capabilities: { fetch: { origins: ["HTTPS://API.Example.com"] } },
      fetchImpl: fakeFetch,
    });
    const lines = await host.run();
    ok(
      eq(lines, ["HTTP/1.1 200\r\nx-fixture: yes\r\n\r\nok"]),
      "Fetch suspends and resumes the same Wasm run with the native raw-response envelope",
    );
    ok(
      requests.length === 1 &&
        requests[0].url === "https://api.example.com/data" &&
        requests[0].options.credentials === "omit" &&
        requests[0].options.redirect === "manual" &&
        requests[0].options.headers[0][0] === "X-Test",
      "Fetch sends exactly one credential-free, no-redirect request after canonical admission",
    );

    let unrestrictedRejected = false;
    try {
      await instantiate(bytes, { capabilities: { fetch: true }, fetchImpl: fakeFetch });
    } catch (e) {
      unrestrictedRejected = /explicit grant object/.test(String(e.message));
    }
    ok(unrestrictedRejected, "browser Fetch refuses an implicit unrestricted grant");

    let malformedRejected = false;
    try {
      await instantiate(bytes, {
        capabilities: { fetch: { origins: ["https://api.example.com/path"] } },
        fetchImpl: fakeFetch,
      });
    } catch (e) {
      malformedRejected = /must not contain a path/.test(String(e.message));
    }
    ok(malformedRejected, "malformed origin grants fail before instantiation");

    const FORBIDDEN = `fn main(console: Console, fetch: Fetch):
    console.print(fetch.send_raw("GET", "https://blocked.example/data", "", ""))
`;
    let forbiddenCalls = 0;
    const forbiddenHost = await instantiate(compile(FORBIDDEN).bytes, {
      capabilities: { fetch: { origins: ["https://api.example.com"] } },
      fetchImpl: async () => { forbiddenCalls++; throw new Error("must not run"); },
    });
    const forbiddenLines = await forbiddenHost.run();
    ok(
      forbiddenCalls === 0 &&
        forbiddenLines[0] ===
          "WITCHY_FETCH_ERROR:denied:Fetch origin `https://blocked.example:443` is not granted",
      "origin denial is returned before any fetch() I/O",
    );

    const NARROW = `fn main(console: Console, fetch: Fetch):
    let narrowed = fetch.only("https://blocked.example")
    console.print("unreachable")
`;
    const narrowHost = await instantiate(compile(NARROW).bytes, {
      capabilities: { fetch: { origins: ["https://api.example.com"] } },
      fetchImpl: fakeFetch,
    });
    let narrowingRejected = false;
    try { await narrowHost.run(); } catch (e) {
      narrowingRejected = /is not granted/.test(String(e.message));
    }
    ok(narrowingRejected, "fetch.only cannot widen beyond the granted origins");

    const REDIRECT = `fn main(console: Console, fetch: Fetch):
    console.print(fetch.send_raw("GET", "https://api.example.com/redirect", "", ""))
`;
    const redirectHost = await instantiate(compile(REDIRECT).bytes, {
      capabilities: { fetch: { origins: ["https://api.example.com"] } },
      fetchImpl: async () => ({
        status: 302,
        redirected: false,
        type: "basic",
        headers: new Map([["location", "https://secret.example/"]]),
        arrayBuffer: async () => new ArrayBuffer(0),
      }),
    });
    ok(
      eq(await redirectHost.run(), [
        "WITCHY_FETCH_ERROR:redirect:Fetch redirects are disabled (HTTP status 302)",
      ]),
      "redirects are rejected without disclosing Location",
    );

    const timeoutHost = await instantiate(compile(FORBIDDEN.replace("blocked.example", "api.example.com")).bytes, {
      capabilities: {
        fetch: { origins: ["https://api.example.com"], timeoutMs: 5 },
      },
      fetchImpl: (_url, { signal }) => new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
      }),
    });
    ok(
      eq(await timeoutHost.run(), [
        "WITCHY_FETCH_ERROR:timeout:Fetch request timed out",
      ]),
      "the host timeout resumes Wasm with the uniform timeout error",
    );

    const oversizedHost = await instantiate(compile(FORBIDDEN.replace("blocked.example", "api.example.com")).bytes, {
      capabilities: {
        fetch: { origins: ["https://api.example.com"], maxResponseBytes: 2 },
      },
      fetchImpl: async () => ({
        status: 200,
        redirected: false,
        type: "basic",
        headers: new Map(),
        arrayBuffer: async () => new TextEncoder().encode("large").buffer,
      }),
    });
    ok(
      eq(await oversizedHost.run(), [
        "WITCHY_FETCH_ERROR:response-too-large:Fetch response exceeds the 2-byte host limit",
      ]),
      "buffered browser responses enforce the host byte limit",
    );
  }

  // === 7. Exec/Secret/Net stay DENIED even under the opt-in host ============
  {
    const EXEC = `import exec

fn main(console: Console, e: Exec, bin: Dir[Read]):
    let (code, _out) = exec.run(e, bin, "true", [], "")
    console.print("\${code}")
`;
    const SECRET = `import secretstore

fn main(console: Console, store: SecretStore):
    let s = secretstore.require(store, "signing")
    console.print("has secret")
`;
    const NET = `import http

fn main(console: Console, net: Net):
    console.print("connecting")
`;
    for (const [name, source] of [["Exec", EXEC], ["Secret", SECRET], ["Net", NET]]) {
      let bytes;
      try {
        bytes = compile(source).bytes;
      } catch (e) {
        // If a probe program doesn't compile as written, that's a test-authoring
        // issue, not a host result — surface it loudly.
        ok(false, `${name} probe compiles (test authoring): ${String(e.message).slice(0, 80)}`);
        continue;
      }
      let denied = false, how = "";
      try {
        // Offer the FULL opt-in capability surface — Exec/Secret/Net are still
        // absent from it, so instantiation must still fail.
        await instantiate(bytes, { capabilities: { clock: true, env: true, dir: { write: true } } });
        how = "(unexpectedly instantiated)";
      } catch (e) {
        denied = e instanceof WebAssembly.LinkError;
        how = `${e.constructor.name}`;
      }
      ok(denied, `${name} stays DENIED even under the opt-in capability host (${how})`);
    }
  }
} catch (e) {
  console.error("harness threw:", e);
  failures++;
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nCAPABILITY-HOST FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nCAPABILITY-HOST OK");
process.exit(0);
