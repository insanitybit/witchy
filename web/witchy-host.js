import {
  instantiate as instantiateRuntime,
  makeSecretStoreImports,
  WITCHY_CLOCK_IMPORTS,
  WITCHY_CONSOLE_IMPORTS,
  WITCHY_DIR_IMPORTS,
  WITCHY_ENV_IMPORTS,
  WITCHY_FETCH_IMPORTS,
  WITCHY_VM_IMPORTS,
} from "./witchy-runtime/witchy-runtime.mjs";

// Shared host shim for the witchy browser playground — imported by both
// web/playground.js (browser) and scripts/pg_validate.mjs (the Node/V8 validator),
// so it stays free of DOM/fetch: callers load the lib wasm and pass its `exports`
// in as `wasm`.
//
// It compiles a snippet to a wasm module (`witchy_compile`) and runs it, acting as
// the capability host: `print` collects output; the pure helpers (float /
// encoding / string_from_code / regex / crypto) delegate to the lib's exports so
// they match the native backend byte-for-byte; every authority import traps (the
// browser grants no capabilities).
//
// Lib C ABI: witchy_alloc / witchy_free for marshaling; witchy_compile(ptr,len) ->
// `[u32 status][u32 len][payload]` (status 0 = wasm bytes, 1 = utf-8 error); and
// witchy_render_float / witchy_string_from_code / witchy_encoding /
// witchy_crypto_hash / witchy_hmac_sha256 / witchy_regex / witchy_verify_status.

// Compile `source` to a wasm binary (a detached copy), or throw the compiler's
// error message.
export function compile(wasm, source) {
  const u8 = () => new Uint8Array(wasm.memory.buffer);
  const dv = () => new DataView(wasm.memory.buffer);
  const enc = new TextEncoder().encode(source);
  const ptr = wasm.witchy_alloc(enc.length || 1);
  u8().set(enc, ptr);
  const res = wasm.witchy_compile(ptr, enc.length);
  wasm.witchy_free(ptr, enc.length || 1);
  const status = dv().getUint32(res, true);
  const len = dv().getUint32(res + 4, true);
  const payload = u8().slice(res + 8, res + 8 + len);
  wasm.witchy_free(res, 8 + len);
  if (status !== 0) throw new Error(new TextDecoder().decode(payload));
  return payload;
}

// RFC-0089's resource proof is already embedded in every heap-using compiled
// module as exported monotonic globals. Native `witchy stats` reads the same
// globals through wasmtime; the browser can read them directly from the
// WebAssembly instance. Keep the public names independent of the compiler's
// reserved export spellings so the book/UI never has to know that ABI detail.
const OPTIMIZATION_COUNTER_EXPORTS = Object.freeze([
  ["rc_alloc_calls", "__witchy_rc_alloc_calls"],
  ["bump_alloc_calls", "__witchy_bump_alloc_calls"],
  ["rc_reuse_calls", "__witchy_rc_reuse_calls"],
  ["rc_free_calls", "__witchy_rc_free_calls"],
  ["region_rewind_calls", "__witchy_region_rewind_calls"],
]);

export function readOptimizationStats(exports) {
  const stats = {};
  for (const [name, exportName] of OPTIMIZATION_COUNTER_EXPORTS) {
    const counter = exports[exportName];
    if (counter != null && "value" in counter) stats[name] = BigInt(counter.value);
  }
  return stats;
}

const FIXTURE_AUTHORITY_IMPORTS = new Set([
  "print",
  "console_read_len",
  "now",
  "now_monotonic",
  "rand_u64",
  "mint_env",
  "env_only",
  "env_len",
  "env_fill",
  "args_size",
  "mint_dir",
  "dir_subdir",
  "dir_only",
  "dir_read_len",
  "dir_exists",
  "dir_is_dir",
  "dir_list_size",
  "dir_open",
  "dir_write",
  "dir_append",
  "dir_make_dir",
  "dir_create",
  "dir_create_new",
  "dir_replace",
  "dir_rename",
  "file_read_len",
  "file_write",
  "mint_fetch",
  "fetch_only",
  "fetch_send_len",
  "secretstore_lookup",
  "crypto_reveal_len",
  "crypto.sign",
  "crypto.public_key",
  "mint_exec",
  "exec_only",
  "exec_run",
]);
const FIXTURE_WORKER_DIR_IMPORTS = new Set(
  WITCHY_DIR_IMPORTS.filter((name) => name !== "mint_dir"),
);

class FixtureProviderError extends Error {
  constructor(failure) {
    super(`fixture ${failure.code}: ${failure.message}`);
    this.failure = failure;
  }
}

function createFixtureBridge(wasm, plan) {
  const required = [
    "witchy_fixture_open",
    "witchy_fixture_invoke",
    "witchy_fixture_finish",
    "witchy_fixture_discard",
  ];
  for (const name of required) {
    if (typeof wasm[name] !== "function") {
      throw new Error(
        "browser compiler lacks fixture support; rebuild it with " +
        "`./scripts/build-playground.sh`",
      );
    }
  }
  const enc = new TextEncoder();
  const dec = new TextDecoder("utf-8", { fatal: true });
  const u8 = () => new Uint8Array(wasm.memory.buffer);
  const dv = () => new DataView(wasm.memory.buffer);
  const tagged = (text, call) => {
    const bytes = enc.encode(text);
    const ptr = wasm.witchy_alloc(bytes.length || 1);
    u8().set(bytes, ptr);
    let result;
    try {
      result = call(ptr, bytes.length);
    } finally {
      wasm.witchy_free(ptr, bytes.length || 1);
    }
    const status = dv().getUint32(result, true);
    const length = dv().getUint32(result + 4, true);
    const payload = u8().slice(result + 8, result + 8 + length);
    wasm.witchy_free(result, 8 + length);
    const decoded = dec.decode(payload);
    if (status !== 0) throw new Error(decoded);
    return decoded;
  };
  const planJson = typeof plan === "string" ? plan : JSON.stringify(plan);
  const opened = JSON.parse(tagged(
    planJson,
    (ptr, len) => wasm.witchy_fixture_open(ptr, len),
  ));
  if (
    opened.version !== 1
    || !opened.host
    || opened.host.version !== 1
    || !opened.host.roots
    || typeof opened.session !== "string"
    || !/^[1-9][0-9]*$/.test(opened.session)
  ) {
    throw new Error("browser compiler returned a malformed fixture session");
  }
  const session = Number(opened.session);
  if (!Number.isSafeInteger(session) || session > 0xffffffff) {
    throw new Error("browser compiler returned an out-of-range fixture session");
  }
  let active = true;
  return {
    roots: opened.host.roots,
    invoke(operation, fields, source) {
      if (!active) throw new Error("fixture session is already finished");
      const request = {
        version: 1,
        request: { operation, ...fields },
      };
      if (source) request.source = source;
      const response = JSON.parse(tagged(
        JSON.stringify(request),
        (ptr, len) => wasm.witchy_fixture_invoke(session, ptr, len),
      ));
      if (response.version !== 1 || !response.outcome) {
        throw new Error("browser compiler returned a malformed fixture response");
      }
      return response.outcome;
    },
    finish(status, message = "") {
      if (!active) throw new Error("fixture session is already finished");
      const transcript = JSON.parse(tagged(
        message,
        (ptr, len) => wasm.witchy_fixture_finish(session, status, ptr, len),
      ));
      active = false;
      return transcript;
    },
    discard() {
      if (!active) return false;
      active = false;
      return wasm.witchy_fixture_discard(session) === 1;
    },
  };
}

function installFixtureImports(real, bridge, io) {
  const roots = bridge.roots;
  const handles = new WeakSet();
  const handle = (raw, family) => {
    if (typeof raw !== "string" || !/^[1-9][0-9]*$/.test(raw)) {
      throw new Error(`fixture ${family} returned a malformed handle`);
    }
    const value = Object.freeze({ raw, family });
    handles.add(value);
    return value;
  };
  const rawHandle = (value, family) => {
    if (!value || typeof value !== "object" || !handles.has(value) || value.family !== family) {
      throw new Error(`${family} fixture externref has wrong host data`);
    }
    return value.raw;
  };
  const outcome = (operation, fields = {}) =>
    bridge.invoke(operation, fields, io.source());
  const response = (operation, fields, kind) => {
    const result = outcome(operation, fields);
    if (result.kind === "fail") throw new FixtureProviderError(result.error);
    if (result.kind !== "return" || !result.value || result.value.kind !== kind) {
      throw new Error(`fixture ${operation} returned an unexpected response`);
    }
    return result.value.value;
  };
  const unit = (operation, fields = {}) => {
    const result = outcome(operation, fields);
    if (result.kind === "fail") throw new FixtureProviderError(result.error);
    if (result.kind !== "return" || !result.value || result.value.kind !== "unit") {
      throw new Error(`fixture ${operation} returned an unexpected response`);
    }
  };
  const root = (name, family) => {
    const raw = roots[name];
    if (typeof raw !== "string") {
      throw new Error(`fixture plan declared no ${family} provider`);
    }
    return handle(raw, family);
  };
  const stageBytes = (bytes, label) => {
    const staged = Uint8Array.from(bytes);
    if (staged.length > 0x7fffffff) {
      throw new Error(`fixture ${label} exceeds the guest ABI size limit`);
    }
    io.stage(staged);
    return staged.length;
  };

  if (roots.console) {
    real.print = (ptr, len) => {
      const text = io.readRawText(ptr, len).replace(/\n+$/, "");
      unit("console_write", { text });
      io.capture(text);
    };
    real.console_read_len = () =>
      stageBytes(
        io.encode(response("console_read", {}, "string")),
        "Console input",
      );
  } else {
    delete real.print;
  }

  if (roots.clock) {
    const clock = () => BigInt(response("clock_now", {}, "u64"));
    real.now = () => BigInt.asIntN(64, clock() / 1000000n);
    real.now_monotonic = () => BigInt.asIntN(64, clock());
  }
  if (roots.rand) {
    real.rand_u64 = () =>
      BigInt.asIntN(64, BigInt(response("rand_u64", {}, "u64")));
  }

  if (typeof roots.env === "string") {
    real.mint_env = () => root("env", "Env");
    real.env_only = (env, namesPtr) =>
      handle(
        response(
          "env_only",
          { env: rawHandle(env, "Env"), names: io.readWstrList(namesPtr) },
          "handle",
        ),
        "Env",
      );
    real.env_len = (env, namePtr) => {
      const value = response(
        "env_get",
        { env: rawHandle(env, "Env"), name: io.readWstrText(namePtr) },
        "optional_string",
      );
      if (value === null) {
        io.clearStage();
        return -1;
      }
      return stageBytes(io.encode(value), "Env value");
    };
    real.env_fill = (_env, _namePtr, outPtr) => io.fill(outPtr);
  }

  if (roots.argv) {
    real.args_size = () => {
      const values = response("argv", {}, "strings");
      io.stageList(values);
      return io.listSize(values);
    };
  } else {
    real.args_size = () => {
      throw new Error("fixture plan declared no Argv provider");
    };
  }

  if (typeof roots.filesystem === "string") {
    real.mint_dir = (index) => {
      if (index !== 0) throw new Error(`invalid fixture Dir grant index ${index}`);
      return root("filesystem", "Dir");
    };
    real.dir_subdir = (dir, namePtr) =>
      handle(
        response(
          "dir_subdir",
          { dir: rawHandle(dir, "Dir"), name: io.readWstrText(namePtr) },
          "handle",
        ),
        "Dir",
      );
    real.dir_only = (dir, refinePtr) =>
      handle(
        response(
          "dir_only",
          { dir: rawHandle(dir, "Dir"), refine: io.readWstrText(refinePtr) },
          "handle",
        ),
        "Dir",
      );
    real.dir_read_len = (dir, pathPtr) =>
      stageBytes(
        response(
          "dir_read",
          { dir: rawHandle(dir, "Dir"), path: io.readWstrText(pathPtr) },
          "bytes",
        ),
        "Dir read",
      );
    real.dir_exists = (dir, pathPtr) =>
      Number(response(
        "dir_exists",
        { dir: rawHandle(dir, "Dir"), path: io.readWstrText(pathPtr) },
        "bool",
      ));
    real.dir_is_dir = (dir, pathPtr) =>
      Number(response(
        "dir_is_dir",
        { dir: rawHandle(dir, "Dir"), path: io.readWstrText(pathPtr) },
        "bool",
      ));
    real.dir_list_size = (dir) => {
      const values = response(
        "dir_list",
        { dir: rawHandle(dir, "Dir") },
        "strings",
      );
      io.stageList(values);
      return io.listSize(values);
    };
    real.dir_open = (dir, pathPtr) =>
      handle(
        response(
          "dir_open",
          { dir: rawHandle(dir, "Dir"), path: io.readWstrText(pathPtr) },
          "handle",
        ),
        "File",
      );
    real.dir_write = (dir, pathPtr, contentsPtr) => {
      response(
        "dir_write",
        {
          dir: rawHandle(dir, "Dir"),
          path: io.readWstrText(pathPtr),
          bytes: [...io.readWstr(contentsPtr)],
        },
        "count",
      );
    };
    real.dir_append = (dir, pathPtr, contentsPtr) => {
      response(
        "dir_append",
        {
          dir: rawHandle(dir, "Dir"),
          path: io.readWstrText(pathPtr),
          bytes: [...io.readWstr(contentsPtr)],
        },
        "count",
      );
    };
    real.dir_make_dir = (dir, pathPtr) =>
      unit("dir_make_dir", {
        dir: rawHandle(dir, "Dir"),
        path: io.readWstrText(pathPtr),
      });
    real.dir_create = (dir, pathPtr) =>
      handle(
        response(
          "dir_create",
          { dir: rawHandle(dir, "Dir"), path: io.readWstrText(pathPtr) },
          "handle",
        ),
        "File",
      );
    real.dir_create_new = (dir, pathPtr, contentsPtr) =>
      Number(response(
        "dir_create_new",
        {
          dir: rawHandle(dir, "Dir"),
          path: io.readWstrText(pathPtr),
          bytes: [...io.readWstr(contentsPtr)],
        },
        "bool",
      ));
    real.dir_replace = (dir, pathPtr, contentsPtr) =>
      unit("dir_replace", {
        dir: rawHandle(dir, "Dir"),
        path: io.readWstrText(pathPtr),
        bytes: [...io.readWstr(contentsPtr)],
      });
    real.dir_rename = (dir, fromPtr, toPtr) =>
      unit("dir_rename", {
        dir: rawHandle(dir, "Dir"),
        from: io.readWstrText(fromPtr),
        to: io.readWstrText(toPtr),
      });
    real.file_read_len = (file) =>
      stageBytes(
        response("file_read", { file: rawHandle(file, "File") }, "bytes"),
        "File read",
      );
    real.file_write = (file, contentsPtr) => {
      response(
        "file_write",
        {
          file: rawHandle(file, "File"),
          bytes: [...io.readWstr(contentsPtr)],
        },
        "count",
      );
    };
  }

  if (typeof roots.fetch === "string") {
    real.mint_fetch = (index) => {
      if (index !== 0) throw new Error(`invalid fixture Fetch grant index ${index}`);
      return root("fetch", "Fetch");
    };
    real.fetch_only = (fetch, originsPtr) =>
      handle(
        response(
          "fetch_only",
          {
            fetch: rawHandle(fetch, "Fetch"),
            origins: io.readWstrText(originsPtr).split("\n"),
          },
          "handle",
        ),
        "Fetch",
      );
    real.fetch_send_len = (fetch, methodPtr, urlPtr, headersPtr, bodyPtr) => {
      const headers = io.readWstrText(headersPtr)
        .split("\n")
        .filter((line) => line.includes(":"))
        .map((line) => {
          const colon = line.indexOf(":");
          return [line.slice(0, colon).trim(), line.slice(colon + 1).trim()];
        });
      const result = outcome("fetch_send", {
        fetch: rawHandle(fetch, "Fetch"),
        request: {
          method: io.readWstrText(methodPtr),
          url: io.readWstrText(urlPtr),
          headers,
          body: [...io.readWstr(bodyPtr)],
        },
      });
      let payload;
      if (result.kind === "fail") {
        const codes = {
          denied: "denied",
          permission_denied: "denied",
          invalid_request: "invalid-request",
          timeout: "timeout",
          redirect: "redirect",
          network: "network",
          invalid_data: "malformed-response",
          response_too_large: "response-too-large",
        };
        const code = codes[result.error.code];
        if (!code) throw new FixtureProviderError(result.error);
        payload = `WITCHY_FETCH_ERROR:${code}:${result.error.message}`;
      } else if (
        result.kind === "return"
        && result.value
        && result.value.kind === "fetch"
      ) {
        const fetched = result.value.value;
        let raw = `HTTP/1.1 ${fetched.status}\r\n`;
        for (const [name, value] of fetched.headers) raw += `${name}: ${value}\r\n`;
        payload = raw + "\r\n" + io.decode(Uint8Array.from(fetched.body));
      } else {
        throw new Error("fixture fetch_send returned an unexpected response");
      }
      return stageBytes(io.encode(payload), "Fetch response");
    };
  }

  if (typeof roots.secrets === "string") {
    const store = root("secrets", "SecretStore");
    real.secretstore_lookup = (namePtr) => {
      const raw = response(
        "secret_store_lookup",
        { store: rawHandle(store, "SecretStore"), name: io.readWstrText(namePtr) },
        "optional_handle",
      );
      return raw === null ? null : handle(raw, "Secret");
    };
    real.crypto_reveal_len = (secret) =>
      stageBytes(
        io.encode(response(
          "secret_reveal",
          { secret: rawHandle(secret, "Secret") },
          "string",
        )),
        "Secret reveal",
      );
    real["crypto.sign"] = (secret, messagePtr, outPtr) => {
      const signature = response(
        "secret_sign",
        {
          secret: rawHandle(secret, "Secret"),
          message: io.readWstrText(messagePtr),
        },
        "string",
      );
      io.write(io.encode(signature), outPtr);
    };
    real["crypto.public_key"] = (secret, outPtr) => {
      const key = response(
        "secret_public_key",
        { secret: rawHandle(secret, "Secret") },
        "string",
      );
      io.write(io.encode(key), outPtr);
    };
  }

  if (typeof roots.exec === "string") {
    real.mint_exec = () => root("exec", "Exec");
    real.exec_only = (exec, toolsPtr) =>
      handle(
        response(
          "exec_only",
          { exec: rawHandle(exec, "Exec"), tools: io.readWstrList(toolsPtr) },
          "handle",
        ),
        "Exec",
      );
    real.exec_run = (exec, dir, pathPtr, argumentsPtr, stdinPtr) => {
      const joined = io.readWstrText(argumentsPtr);
      const value = response(
        "exec_run",
        {
          exec: rawHandle(exec, "Exec"),
          dir: rawHandle(dir, "Dir"),
          path: io.readWstrText(pathPtr),
          arguments: joined === "" ? [] : joined.split("\0"),
          stdin: io.readWstrText(stdinPtr),
        },
        "exec",
      );
      return stageBytes(
        io.encode(`${value.exit_code}\n${value.stdout}${value.stderr}`),
        "Exec response",
      );
    };
  }
}

function fixtureRunResult(transcript, stats, fallbackMessage = "") {
  const passed = transcript.result && transcript.result.kind === "passed";
  const lines = [...(transcript.stdout || []), ...(transcript.stderr || [])];
  if (!passed) {
    const message = transcript.result && transcript.result.message;
    if (message) lines.push(message);
    else if (fallbackMessage) lines.push(fallbackMessage);
  }
  return {
    ok: passed,
    text: lines.join("\n"),
    stats,
    transcript,
  };
}

// Compile + instantiate + run `source` on the browser's own engine; return
// `{ ok, text, stats }` (text is the joined output, or the error / trap message;
// stats are the compiled module's deterministic RFC-0089 resource counters).
export async function runWitchy(wasm, source, opts = {}) {
  let binary;
  try {
    binary = compile(wasm, source);
  } catch (e) {
    return { ok: false, text: String((e && e.message) || e) };
  }
  return runCompiledWitchy(wasm, binary, opts);
}

// Run an already-compiled guest. The sandboxed book path compiles in its
// trusted parent and calls this function inside an opaque-origin iframe, so no
// untrusted guest instruction executes in the docs page's main realm.
export async function runCompiledWitchy(wasm, binary, opts = {}) {
  // Fresh views — any lib call that allocates may grow (and detach) the buffer.
  const libU8 = () => new Uint8Array(wasm.memory.buffer);
  const libDV = () => new DataView(wasm.memory.buffer);
  // Read a `[u32 len][bytes]` block from lib memory at `ptr`, copy out, free it.
  const takeLibBytes = (ptr) => {
    const len = libDV().getUint32(ptr, true);
    const bytes = libU8().slice(ptr + 4, ptr + 4 + len);
    wasm.witchy_free(ptr, 4 + len);
    return bytes;
  };

  const compiled = new WebAssembly.Module(binary);
  const hasFixturePlan = opts.fixturePlan !== undefined;
  if (
    hasFixturePlan
    && (opts.capabilities !== undefined
      || opts.args !== undefined
      || opts.fetchImpl !== undefined)
  ) {
    return {
      ok: false,
      text: "fixture plans cannot be combined with real browser providers, argv, or fetch",
      stats: {},
    };
  }
  const imports = WebAssembly.Module.imports(compiled);
  const importedWitchyNames = new Set(
    imports
      .filter((entry) => entry.module === "witchy")
      .map((entry) => entry.name),
  );
  const runtimeFamilies = [
    ["clock", WITCHY_CLOCK_IMPORTS],
    ["console", WITCHY_CONSOLE_IMPORTS],
    ["dir", WITCHY_DIR_IMPORTS],
    ["env", WITCHY_ENV_IMPORTS],
    ["fetch", WITCHY_FETCH_IMPORTS],
    ["vm", WITCHY_VM_IMPORTS],
  ];
  const hasRuntimeProvider = runtimeFamilies.some(
    ([family, names]) =>
      opts.capabilities
      && opts.capabilities[family]
      && names.some((name) => importedWitchyNames.has(name)),
  );
  if (hasRuntimeProvider && !hasFixturePlan) {
    try {
      const host = await instantiateRuntime(compiled, opts);
      const lines = await host.run();
      return {
        ok: true,
        text: lines.join("\n"),
        stats: readOptimizationStats(host.instance.exports),
      };
    } catch (e) {
      return {
        ok: false,
        text: `runtime error: ${String((e && e.message) || e)}`,
        stats: {},
      };
    }
  }

  const out = [];
  let innerMem = null;
  const u8 = () => new Uint8Array(innerMem.buffer);
  const dv = () => new DataView(innerMem.buffer);
  const dec = new TextDecoder();
  // Every `*_to_str` / `encoding` host op writes its bytes into the guest buffer
  // at `outPtr` and returns the byte length.
  const writeInner = (bytes, outPtr) => {
    u8().set(bytes, outPtr);
    return bytes.length;
  };
  // Read a witchy string (`[u32 len][utf-8]`) from inner memory at `ptr`.
  const readWstr = (ptr) => {
    const len = dv().getUint32(ptr, true);
    return u8().slice(ptr + 4, ptr + 4 + len);
  };
  const readWstrText = (ptr) => dec.decode(readWstr(ptr));
  const readWstrList = (ptr) => {
    const count = dv().getInt32(ptr, true);
    if (count < 0) throw new Error("negative List(String) length");
    const values = [];
    for (let index = 0; index < count; index++) {
      const valuePtr = Number(dv().getBigUint64(ptr + 4 + 8 * index, true));
      values.push(readWstrText(valuePtr));
    }
    return values;
  };
  // Copy `bytes` into a fresh lib buffer; the caller frees it.
  const toLib = (bytes) => {
    const p = wasm.witchy_alloc(bytes.length || 1);
    libU8().set(bytes, p);
    return p;
  };
  // The bundled `regex`/`crypto` std modules are native-backed (the browser has
  // no filesystem to resolve a pure sibling), so their host ops are delegated to
  // the lib's exports — reusing the real Rust regex/sha2 path, byte-for-byte.
  let pending = new Uint8Array(0); // staged by `regex_match_spans_len`, drained by `fill_pending`
  let pendingList = null;
  const args = Array.isArray(opts.args) ? opts.args.map(String) : [];
  const hashOp = (op) => (inPtr, outPtr) => {
    const input = readWstr(inPtr);
    const lp = toLib(input);
    const res = wasm.witchy_crypto_hash(op, lp, input.length);
    wasm.witchy_free(lp, input.length || 1);
    return writeInner(takeLibBytes(res), outPtr);
  };
  const verifyOp = (op) => (pkPtr, msgPtr, sigPtr) => {
    const a = readWstr(pkPtr), b = readWstr(msgPtr), c = readWstr(sigPtr);
    const ap = toLib(a), bp = toLib(b), cp = toLib(c);
    const r = wasm.witchy_verify_status(op, ap, a.length, bp, b.length, cp, c.length);
    wasm.witchy_free(ap, a.length || 1);
    wasm.witchy_free(bp, b.length || 1);
    wasm.witchy_free(cp, c.length || 1);
    return r;
  };

  const real = {
    print(ptr, len) {
      out.push(dec.decode(u8().slice(ptr, ptr + len)).replace(/\n+$/, ""));
    },
    print_int(n) {
      out.push(n.toString());
    },
    print_float(x) {
      out.push(dec.decode(takeLibBytes(wasm.witchy_render_float(x))));
    },
    float_to_str(x, outPtr) {
      return writeInner(takeLibBytes(wasm.witchy_render_float(x)), outPtr);
    },
    string_from_code(cp, outPtr) {
      return writeInner(takeLibBytes(wasm.witchy_string_from_code(cp)), outPtr);
    },
    encoding(op, inPtr, outPtr) {
      const input = readWstr(inPtr);
      const lp = toLib(input);
      const res = wasm.witchy_encoding(op, lp, input.length);
      wasm.witchy_free(lp, input.length || 1);
      return writeInner(takeLibBytes(res), outPtr);
    },
    "crypto.sha256": hashOp(0),
    "crypto.sha512": hashOp(1),
    "crypto.sha3_256": hashOp(2),
    "crypto.hmac_sha256"(keyPtr, msgPtr, outPtr) {
      const k = readWstr(keyPtr), m = readWstr(msgPtr);
      const kp = toLib(k), mp = toLib(m);
      const res = wasm.witchy_hmac_sha256(kp, k.length, mp, m.length);
      wasm.witchy_free(kp, k.length || 1);
      wasm.witchy_free(mp, m.length || 1);
      return writeInner(takeLibBytes(res), outPtr);
    },
    "crypto.__ed25519_verify_status": verifyOp(0),
    "crypto.__ecdsa_p256_verify_status": verifyOp(1),
    "crypto.__ecdsa_p256_verify_hex_status": verifyOp(2),
    "crypto.__rsa_pkcs1_sha256_verify_status": () => -4n,
    regex_match_spans_len(patPtr, textPtr) {
      const p = readWstr(patPtr), t = readWstr(textPtr);
      const pp = toLib(p), tp = toLib(t);
      const res = wasm.witchy_regex(pp, p.length, tp, t.length);
      wasm.witchy_free(pp, p.length || 1);
      wasm.witchy_free(tp, t.length || 1);
      pending = takeLibBytes(res);
      if (pending[0] === 0x1f) {
        throw new Error(dec.decode(pending.slice(1)));
      }
      return pending.length;
    },
    fill_pending(outPtr) {
      u8().set(pending, outPtr);
      pending = new Uint8Array(0);
    },
    args_size() {
      pendingList = args.slice();
      let size = 4 + 8 * pendingList.length;
      for (const arg of pendingList) size += 4 + new TextEncoder().encode(arg).length;
      return size;
    },
    write_pending_list(basePtr) {
      if (pendingList === null) return;
      const values = pendingList;
      pendingList = null;
      const encoded = values.map((value) => new TextEncoder().encode(value));
      const stringsStart = basePtr + 4 + 8 * values.length;
      dv().setInt32(basePtr, values.length, true);
      let offset = 0;
      for (let i = 0; i < encoded.length; i++) {
        const ptr = stringsStart + offset;
        dv().setUint32(basePtr + 4 + 8 * i, ptr >>> 0, true);
        dv().setUint32(basePtr + 4 + 8 * i + 4, 0, true);
        offset += 4 + encoded[i].length;
      }
      let cursor = stringsStart;
      for (const bytes of encoded) {
        dv().setInt32(cursor, bytes.length, true);
        u8().set(bytes, cursor + 4);
        cursor += 4 + bytes.length;
      }
    },
  };

  let instance = null;
  let activeInstance = null;
  let fixtureBridge = null;
  if (hasFixturePlan) {
    try {
      fixtureBridge = createFixtureBridge(wasm, opts.fixturePlan);
      const source = () => {
        const site = BigInt(activeInstance?.exports.__witchy_diagnostic_site?.value || 0n);
        if (site === 0n) return undefined;
        const functionPtr = Number((site >> 32n) & 0xffffffffn);
        const line = Number(site & 0xffffffffn);
        const functionName = functionPtr === 0 ? "" : readWstrText(functionPtr);
        const dot = functionName.lastIndexOf(".");
        return {
          module: dot < 0 ? functionName : functionName.slice(0, dot),
          line: String(line),
          column: "1",
        };
      };
      installFixtureImports(real, fixtureBridge, {
        encode: (value) => new TextEncoder().encode(value),
        decode: (bytes) => dec.decode(bytes),
        readRawText: (ptr, len) => dec.decode(u8().slice(ptr, ptr + len)),
        readWstr,
        readWstrText,
        readWstrList,
        write: writeInner,
        stage: (bytes) => { pending = bytes; },
        clearStage: () => { pending = new Uint8Array(0); },
        fill: (outPtr) => {
          u8().set(pending, outPtr);
          pending = new Uint8Array(0);
        },
        stageList: (values) => { pendingList = values.slice(); },
        listSize: (values) =>
          values.reduce(
            (size, value) => size + 4 + new TextEncoder().encode(value).length,
            4 + 8 * values.length,
          ),
        capture: (line) => out.push(line),
        source,
      });
    } catch (error) {
      if (fixtureBridge) fixtureBridge.discard();
      return {
        ok: false,
        text: `fixture error: ${String((error && error.message) || error)}`,
        stats: {},
      };
    }
  }

  const secretSpec = !hasFixturePlan && opts.capabilities && opts.capabilities.secrets;
  const hasSecretStore = secretSpec !== undefined && secretSpec !== false;
  if (hasSecretStore) {
    Object.assign(
      real,
      makeSecretStoreImports(
        secretSpec,
        {
          readWstr,
          readWstrText: (ptr) => dec.decode(readWstr(ptr)),
          writeAt: writeInner,
          stagePending: (bytes) => { pending = bytes; },
        },
        globalThis.crypto && globalThis.crypto.subtle,
      ),
    );
  }

  const hasFixtureVm = hasFixturePlan && importedWitchyNames.has("vm_with_dir_run");
  let workerWitchy = null;
  if (hasFixtureVm) {
    if (
      typeof WebAssembly.Suspending !== "function"
      || typeof WebAssembly.promising !== "function"
    ) {
      return {
        ok: false,
        text: "runtime error: browser vm.with_dir requires WebAssembly JSPI",
        stats: {},
      };
    }
    real.vm_with_dir_run = new WebAssembly.Suspending(
      async (dir, codeIdx, inputPtr) => {
        const input = readWstr(inputPtr);
        const parentMemory = innerMem;
        const parentInstance = activeInstance;
        let result;
        try {
          const worker = await WebAssembly.instantiate(compiled, {
            witchy: workerWitchy,
          });
          const workerMemory = worker.exports.memory;
          const galloc = worker.exports.__galloc;
          const callback = worker.exports.__call_dir_bytes;
          if (
            !(workerMemory instanceof WebAssembly.Memory)
            || typeof galloc !== "function"
            || typeof callback !== "function"
          ) {
            throw new Error(
              "vm.with_dir worker requires memory, __galloc, and __call_dir_bytes exports",
            );
          }
          innerMem = workerMemory;
          activeInstance = worker;
          const workerInput = galloc(4 + input.length);
          new DataView(workerMemory.buffer).setInt32(workerInput, input.length, true);
          new Uint8Array(workerMemory.buffer).set(input, workerInput + 4);
          const resultPtr = Number(await WebAssembly.promising(callback)(
            codeIdx,
            dir,
            workerInput,
          ));
          const view = new DataView(workerMemory.buffer);
          const length = view.getInt32(resultPtr, true);
          if (
            length < 0
            || resultPtr < 0
            || resultPtr + 4 + length > workerMemory.buffer.byteLength
          ) {
            throw new Error("vm.with_dir worker returned an invalid Bytes pointer");
          }
          result = new Uint8Array(workerMemory.buffer)
            .slice(resultPtr + 4, resultPtr + 4 + length);
        } finally {
          innerMem = parentMemory;
          activeInstance = parentInstance;
        }
        const staged = new Uint8Array(4 + result.length);
        new DataView(staged.buffer).setInt32(0, result.length, true);
        staged.set(result, 4);
        pending = staged;
        return staged.length;
      },
    );
  }

  // Any authority import (now/env/dir_*/net_*/crypto.*/…) is a trapping stub —
  // the browser grants no capabilities.
  const witchy = new Proxy(real, {
    get(target, name) {
      if (name in target) return target[name];
      if (hasFixturePlan && FIXTURE_AUTHORITY_IMPORTS.has(String(name))) {
        return undefined;
      }
      return () => {
        throw new Error(
          `capability '${String(name)}' is not available in the browser playground`,
        );
      };
    },
  });
  workerWitchy = new Proxy({}, {
    get(_target, name) {
      const importName = String(name);
      if (
        FIXTURE_WORKER_DIR_IMPORTS.has(importName)
        || importName === "vm_with_dir_run"
      ) {
        return witchy[importName];
      }
      if (
        FIXTURE_AUTHORITY_IMPORTS.has(importName)
        || WITCHY_VM_IMPORTS.includes(importName)
      ) {
        return () => {
          throw new Error(
            `capability '${importName}' is not granted to the vm.with_dir worker`,
          );
        };
      }
      return witchy[importName];
    },
  });

  try {
    ({ instance } = await WebAssembly.instantiate(binary, { witchy }));
    innerMem = instance.exports.memory;
    activeInstance = instance;
    if (hasSecretStore || hasFixtureVm) {
      await WebAssembly.promising(instance.exports.run)();
    } else {
      instance.exports.run();
    }
    if (fixtureBridge) {
      const transcript = fixtureBridge.finish(0);
      fixtureBridge = null;
      return fixtureRunResult(
        transcript,
        readOptimizationStats(instance.exports),
      );
    }
  } catch (e) {
    const msg = `runtime error: ${String((e && e.message) || e)}`;
    const stats = instance == null ? {} : readOptimizationStats(instance.exports);
    if (fixtureBridge) {
      try {
        const transcript = fixtureBridge.finish(1, msg);
        fixtureBridge = null;
        return fixtureRunResult(transcript, stats, msg);
      } catch (finishError) {
        fixtureBridge.discard();
        fixtureBridge = null;
        return {
          ok: false,
          text: out.concat(
            msg,
            `fixture cleanup error: ${String((finishError && finishError.message) || finishError)}`,
          ).join("\n"),
          stats,
        };
      }
    }
    return { ok: false, text: out.concat(msg).join("\n"), stats };
  } finally {
    if (fixtureBridge) fixtureBridge.discard();
  }
  return { ok: true, text: out.join("\n"), stats: readOptimizationStats(instance.exports) };
}
