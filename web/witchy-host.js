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
// witchy_crypto_hash / witchy_hmac_sha256 / witchy_regex / witchy_verify.

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

// Compile + instantiate + run `source` on the browser's own engine; return
// `{ ok, text }` (text is the joined output, or the error / trap message).
export async function runWitchy(wasm, source) {
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

  let binary;
  try {
    binary = compile(wasm, source);
  } catch (e) {
    return { ok: false, text: String((e && e.message) || e) };
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
    const r = wasm.witchy_verify(op, ap, a.length, bp, b.length, cp, c.length);
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
    "crypto.ed25519_verify": verifyOp(0),
    "crypto.ecdsa_p256_verify": verifyOp(1),
    "crypto.ecdsa_p256_verify_hex": verifyOp(2),
    regex_match_spans_len(patPtr, textPtr) {
      const p = readWstr(patPtr), t = readWstr(textPtr);
      const pp = toLib(p), tp = toLib(t);
      const res = wasm.witchy_regex(pp, p.length, tp, t.length);
      wasm.witchy_free(pp, p.length || 1);
      wasm.witchy_free(tp, t.length || 1);
      pending = takeLibBytes(res);
      return pending.length;
    },
    fill_pending(outPtr) {
      u8().set(pending, outPtr);
      pending = new Uint8Array(0);
    },
  };

  // Any authority import (now/env/dir_*/net_*/crypto.*/…) is a trapping stub —
  // the browser grants no capabilities.
  const witchy = new Proxy(real, {
    get(target, name) {
      if (name in target) return target[name];
      return () => {
        throw new Error(
          `capability '${String(name)}' is not available in the browser playground`,
        );
      };
    },
  });

  try {
    const { instance } = await WebAssembly.instantiate(binary, { witchy });
    innerMem = instance.exports.memory;
    instance.exports.run();
  } catch (e) {
    const msg = `runtime error: ${String((e && e.message) || e)}`;
    return { ok: false, text: out.concat(msg).join("\n") };
  }
  return { ok: true, text: out.join("\n") };
}
