#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { runWitchy } from "../witchy-host.js";

const wasmPath = process.env.WITCHY_WASM_PATH
  || resolve("target/wasm32-unknown-unknown/debug/witchy.wasm");
const { instance: compiler } = await WebAssembly.instantiate(
  readFileSync(wasmPath),
  {},
);
const wasm = compiler.exports;
let checks = 0;

function ok(condition, message) {
  if (!condition) throw new Error(`FAIL: ${message}`);
  checks++;
  console.log(`  ok: ${message}`);
}

async function run(source, fixturePlan) {
  return runWitchy(wasm, source, { fixturePlan });
}

const basicSource = `fn main(output: Console[Write], clock: Clock, entropy: Rand, env: Env, args: List(String)):
    output.print("clock \${clock.now()}")
    output.print("rand \${entropy.rand_u64()}")
    match env.get_env("MODE"):
        Some(value) -> output.print("mode \${value}")
        None -> output.print("mode missing")
    for arg in args:
        output.print("arg \${arg}")
`;
const basicPlan = {
  version: 1,
  console: { script: [] },
  clock: { start_ns: "2000000", step_ns: "1000000", script: [] },
  rand: { seed: "7", script: [] },
  env: { values: { MODE: "fixture" }, allow: ["MODE"], script: [] },
  argv: ["one"],
};
const basicFirst = await run(basicSource, basicPlan);
const basicSecond = await run(basicSource, basicPlan);
ok(basicFirst.ok, `basic fixture run succeeds: ${basicFirst.text}`);
ok(
  basicFirst.text === basicSecond.text
  && JSON.stringify(basicFirst.transcript) === JSON.stringify(basicSecond.transcript),
  "Clock, Rand, Env, argv, output, and transcripts reproduce exactly",
);
ok(
  basicFirst.text.startsWith("clock 2\nrand ")
  && basicFirst.text.endsWith("\nmode fixture\narg one"),
  "basic roots expose only their deterministic plan values",
);
const basicFamilies = new Set(basicFirst.transcript.events.map((event) => event.family));
for (const family of ["console", "clock", "rand", "env", "argv"]) {
  ok(basicFamilies.has(family), `${family} calls are transcripted`);
}
ok(
  basicFirst.transcript.events.some(
    (event) => event.source?.module === "main" && Number(event.source.line) > 0,
  ),
  "browser fixture events carry guest source provenance",
);

const input = await run(
  `fn main(input: Console[Read], output: Console[Write]):
    output.print("hello, \${input.read_line()}")
`,
  {
    version: 1,
    console: {
      script: [
        {
          operation: "console_read_len",
          effective_rights: ["Read"],
          outcome: { kind: "return", value: { kind: "string", value: "Ada" } },
        },
        {
          operation: "print",
          arguments: { text: { kind: "string", value: "hello, Ada" } },
          effective_rights: ["Write"],
          outcome: { kind: "return", value: { kind: "null" } },
        },
      ],
    },
  },
);
ok(input.ok && input.text === "hello, Ada", "scripted Console input and output share one FIFO host");

const filesystem = await run(
  `fn main(console: Console[Write], root: Dir):
    console.print(root.read("seed.txt"))
    root.write("new.txt", "new")
    console.print(root.read("new.txt"))
    let file = root.read_file("new.txt")
    console.print(file.read())
`,
  {
    version: 1,
    console: { script: [] },
    filesystem: {
      entries: { "seed.txt": { kind: "file", hex: "6f6c64" } },
      rights: ["Read", "Write"],
      script: [],
    },
  },
);
ok(filesystem.ok && filesystem.text === "old\nnew\nnew", "Dir and File use one confined in-memory state");
ok(
  filesystem.transcript.events.some((event) => event.operation === "file_read_len"),
  "derived File handles remain observable without exposing host handles",
);

const vmFilesystem = await run(
  `import bytes
import vm

fn worker(dir: Dir, name: Bytes) -> Bytes:
    let text = dir.read(bytes.to_string(name))
    dir.write("result.txt", text + "!")
    bytes.from_string(dir.read("result.txt"))

fn main(console: Console[Write], root: Dir):
    root.make_dir("sandbox")
    root.write("sandbox/input.txt", "shared")
    let sandbox = root.subtree("sandbox")
    let result = vm.with_dir(
        sandbox,
        worker,
        bytes.from_string("input.txt"),
    )
    console.print(bytes.to_string(result))
    console.print(sandbox.read("result.txt"))
`,
  {
    version: 1,
    console: { script: [] },
    filesystem: {
      entries: {},
      rights: ["Read", "Write"],
      script: [],
    },
  },
);
ok(
  vmFilesystem.ok && vmFilesystem.text === "shared!\nshared!",
  `vm.with_dir shares only its fixture-backed Dir state: ${vmFilesystem.text}`,
);
ok(
  vmFilesystem.transcript.events.filter(
    (event) => event.family === "filesystem",
  ).length >= 7,
  "vm.with_dir filesystem operations remain in the parent fixture transcript",
);

const fetchUrl = "https://example.com/data";
const fetchArguments = {
  method: { kind: "string", value: "GET" },
  headers: { kind: "list", value: [] },
  body: { kind: "bytes", value: "" },
};
const fetchPlan = {
  version: 1,
  console: { script: [] },
  fetch: {
    origins: ["https://example.com:443"],
    script: [
      {
        operation: "fetch_send_len",
        target: fetchUrl,
        arguments: fetchArguments,
        effective_rights: ["https://example.com:443"],
        outcome: {
          kind: "return",
          value: {
            kind: "map",
            value: {
              status: { kind: "string", value: "200" },
              headers: {
                kind: "list",
                value: [{
                  kind: "map",
                  value: {
                    name: { kind: "string", value: "X-Test" },
                    value: { kind: "string", value: "fixture" },
                  },
                }],
              },
              body: { kind: "bytes", value: "6f6b" },
            },
          },
        },
      },
      {
        operation: "fetch_send_len",
        target: fetchUrl,
        arguments: fetchArguments,
        effective_rights: ["https://example.com:443"],
        outcome: {
          kind: "fail",
          error: { code: "timeout", message: "configured timeout" },
        },
      },
    ],
  },
};
let ambientFetchCalls = 0;
const originalFetch = globalThis.fetch;
globalThis.fetch = async () => {
  ambientFetchCalls++;
  throw new Error("ambient fetch must not run");
};
let fetched;
try {
  fetched = await run(
    `fn main(console: Console[Write], fetch: Fetch):
    let api = fetch.only("https://example.com")
    console.print(api.send_raw("GET", "${fetchUrl}", "", ""))
    console.print(api.send_raw("GET", "${fetchUrl}", "", ""))
`,
    fetchPlan,
  );
} finally {
  globalThis.fetch = originalFetch;
}
ok(
  fetched.ok
  && fetched.text ===
    "HTTP/1.1 200\r\nX-Test: fixture\r\n\r\nok\n"
      + "WITCHY_FETCH_ERROR:timeout:configured timeout",
  "Fetch returns scripted success and normalized failure bytes",
);
ok(ambientFetchCalls === 0, "fixture Fetch never reaches browser fetch()");

const secret = await run(
  `import crypto
import secretstore

fn main(console: Console[Write], secrets: SecretStore):
    console.print(crypto.reveal(secrets.require("token")))
`,
  {
    version: 1,
    console: { script: [] },
    secrets: {
      entries: {
        token: { hex: "746f702d736563726574", usage: "revealable" },
      },
      script: [],
    },
  },
);
ok(secret.ok && secret.text === "top-secret", "SecretStore reveal uses an opaque fixture handle");
const secretEvents = secret.transcript.events.filter((event) => event.family === "secret_store");
ok(
  !JSON.stringify(secretEvents).includes("top-secret")
  && !JSON.stringify(secretEvents).includes("746f702d736563726574"),
  "SecretStore event evidence remains redacted",
);

const executed = await run(
  `import exec

fn main(console: Console[Write], tools: Dir[Read], runner: Exec):
    let result = exec.run(runner, tools, "tool", ["--check"], "input")
    console.print("\${result.0}:\${result.1}")
`,
  {
    version: 1,
    console: { script: [] },
    filesystem: {
      entries: { tool: { kind: "file", hex: "66697874757265" } },
      rights: ["Read"],
      script: [],
    },
    exec: {
      tools: ["tool"],
      script: [{
        operation: "exec_run",
        target: "tool",
        arguments: {
          args: { kind: "list", value: [{ kind: "string", value: "--check" }] },
          stdin: { kind: "string", value: "input" },
        },
        effective_rights: ["exec:tool", "dir:Read"],
        outcome: {
          kind: "return",
          value: {
            kind: "map",
            value: {
              exit_code: { kind: "string", value: "7" },
              stdout: { kind: "string", value: "out" },
              stderr: { kind: "string", value: "err" },
            },
          },
        },
      }],
    },
  },
);
ok(executed.ok && executed.text === "7:outerr", "Exec returns scripted process output without spawning");

const mixed = await runWitchy(wasm, basicSource, {
  fixturePlan: basicPlan,
  capabilities: { clock: true },
});
ok(!mixed.ok && mixed.text.includes("cannot be combined"), "fixture plans reject real browser-provider mixing");

const duplicate = await runWitchy(
  wasm,
  "fn main():\n    0",
  { fixturePlan: "{\"version\":1,\"version\":1}" },
);
ok(!duplicate.ok && duplicate.text.includes("duplicate"), "duplicate fixture JSON fails before guest instantiation");

const enc = new TextEncoder();
const dec = new TextDecoder();
const memory = () => new Uint8Array(wasm.memory.buffer);
const view = () => new DataView(wasm.memory.buffer);
function tagged(text, call) {
  const bytes = enc.encode(text);
  const ptr = wasm.witchy_alloc(bytes.length || 1);
  memory().set(bytes, ptr);
  const result = call(ptr, bytes.length);
  wasm.witchy_free(ptr, bytes.length || 1);
  const status = view().getUint32(result, true);
  const length = view().getUint32(result + 4, true);
  const payload = dec.decode(memory().slice(result + 8, result + 8 + length));
  wasm.witchy_free(result, 8 + length);
  return { status, payload };
}
const opened = tagged(
  "{\"version\":1}",
  (ptr, len) => wasm.witchy_fixture_open(ptr, len),
);
const session = Number(JSON.parse(opened.payload).session);
const finished = tagged(
  "",
  (ptr, len) => wasm.witchy_fixture_finish(session, 0, ptr, len),
);
ok(opened.status === 0 && finished.status === 0, "raw fixture session opens and finishes exactly once");
const stale = tagged(
  "{\"version\":1,\"request\":{\"operation\":\"argv\"}}",
  (ptr, len) => wasm.witchy_fixture_invoke(session, ptr, len),
);
const forged = tagged(
  "{\"version\":1,\"request\":{\"operation\":\"argv\"}}",
  (ptr, len) => wasm.witchy_fixture_invoke(0xffffffff, ptr, len),
);
ok(stale.status === 1 && stale.payload.includes("finished"), "stale fixture sessions fail closed");
ok(forged.status === 1 && forged.payload.includes("unknown"), "forged fixture sessions fail closed");
ok(wasm.witchy_fixture_discard(0xffffffff) === 0, "discarding a forged session has no effect");

console.log(`FIXTURE-HOST OK (${checks} checks)`);
