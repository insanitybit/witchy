// witchy-runtime — a JavaScript host for witchyc-compiled WASM that implements
// only the NON-capability ("pure-compute") half of the `"witchy"` import ABI and
// DENIES every capability import. It is the browser/Node analog of the wasmtime
// host in `src/runtime.rs`, with the capability set fixed to empty.
//
// This is RFC-0007 ("witchy-WASM in the browser: a pure-compute target"). The
// containment guarantee is structural: witchyc TREE-SHAKES imports (a module
// declares only the host functions it actually reaches — see spec/wasm-abi.md),
// so a footprint-empty rune imports only the pure functions this shim provides,
// while a rune that touches Net/Dir/Clock/etc. imports a capability host function
// this shim does NOT provide — and `WebAssembly.instantiate` then throws a
// `LinkError` for the missing import. Deny-by-omission: the host is a sieve that
// admits exactly the pure modules. No trap stubs are needed for the guarantee
// (none are installed); a capability-using module simply cannot instantiate.
//
// The ABI this targets is declared in `src/wir_prelude.rs` (PRELUDE_IMPORTS_WAT)
// and the wasmtime implementations live in `src/runtime.rs`. The pure functions
// below mirror those byte-for-byte, so a browser run and a native run agree on
// every observable byte (the parity rule of CLAUDE.md). See spec/wasm-abi.md.

// The ABI version this shim implements. Bump in lockstep with a breaking change
// to the `"witchy"` import surface (a renamed/re-signatured pure import, or a
// change to the pending-buffer protocol). RFC-0007 §"ABI stabilization".
export const WITCHY_ABI_VERSION = 1;

// ---------------------------------------------------------------------------
// Pure crypto backend. The witchy host functions are SYNCHRONOUS — the guest
// calls `crypto.sha256(in, out)` and expects the digest written into its memory
// before the call returns — so we cannot use the async WebCrypto `subtle.digest`.
// We carry a small self-contained synchronous SHA-256 (enough for `crypto.sha256`,
// `crypto.rune_hash`, and `crypto.hmac_sha256`), and defer the wider set
// (sha512 / sha3_256 / the verifies) to an injected backend — Node's `node:crypto`
// by default — so the shim works with zero dependencies on the SHA-256 core path
// and uses the platform's crypto for the rest where available.
// ---------------------------------------------------------------------------

// --- synchronous SHA-256 (FIPS 180-4), pure JS, no dependencies ---
const K256 = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

function rotr(x, n) {
  return (x >>> n) | (x << (32 - n));
}

// SHA-256 of a Uint8Array, returning a 32-byte Uint8Array digest.
export function sha256Bytes(msg) {
  let h0 = 0x6a09e667, h1 = 0xbb67ae85, h2 = 0x3c6ef372, h3 = 0xa54ff53a;
  let h4 = 0x510e527f, h5 = 0x9b05688c, h6 = 0x1f83d9ab, h7 = 0x5be0cd19;
  const bitLen = msg.length * 8;
  // Pad: 0x80, then zeros, then 64-bit big-endian length.
  const withOne = msg.length + 1;
  const k = (56 - (withOne % 64) + 64) % 64;
  const total = withOne + k + 8;
  const buf = new Uint8Array(total);
  buf.set(msg, 0);
  buf[msg.length] = 0x80;
  // 64-bit length: JS bit ops are 32-bit, so split hi/lo.
  const hi = Math.floor(bitLen / 0x100000000);
  const lo = bitLen >>> 0;
  const dv = new DataView(buf.buffer);
  dv.setUint32(total - 8, hi);
  dv.setUint32(total - 4, lo);

  const w = new Uint32Array(64);
  for (let off = 0; off < total; off += 64) {
    for (let i = 0; i < 16; i++) w[i] = dv.getUint32(off + i * 4);
    for (let i = 16; i < 64; i++) {
      const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >>> 3);
      const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
    }
    let a = h0, b = h1, c = h2, d = h3, e = h4, f = h5, g = h6, h = h7;
    for (let i = 0; i < 64; i++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (h + S1 + ch + K256[i] + w[i]) | 0;
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) | 0;
      h = g; g = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + t2) | 0;
    }
    h0 = (h0 + a) | 0; h1 = (h1 + b) | 0; h2 = (h2 + c) | 0; h3 = (h3 + d) | 0;
    h4 = (h4 + e) | 0; h5 = (h5 + f) | 0; h6 = (h6 + g) | 0; h7 = (h7 + h) | 0;
  }
  const out = new Uint8Array(32);
  const odv = new DataView(out.buffer);
  [h0, h1, h2, h3, h4, h5, h6, h7].forEach((x, i) => odv.setUint32(i * 4, x >>> 0));
  return out;
}

function toHex(bytes) {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}

// HMAC-SHA256(key, msg) -> 32-byte digest, built on the pure SHA-256 above.
function hmacSha256Bytes(key, msg) {
  const block = 64;
  let k = key;
  if (k.length > block) k = sha256Bytes(k);
  const k0 = new Uint8Array(block);
  k0.set(k);
  const ipad = new Uint8Array(block + msg.length);
  const opad = new Uint8Array(block + 32);
  for (let i = 0; i < block; i++) {
    ipad[i] = k0[i] ^ 0x36;
    opad[i] = k0[i] ^ 0x5c;
  }
  ipad.set(msg, block);
  const inner = sha256Bytes(ipad);
  opad.set(inner, block);
  return sha256Bytes(opad);
}

// Try to load Node's `node:crypto` synchronously (for sha512 / sha3_256 /
// ed25519_verify / the p256 verifies). Returns null in a plain browser, where
// only the pure-JS SHA-256 core is available. A capability-free rune that uses
// only sha256/hmac/rune_hash never needs this; one that uses sha512/sha3/verify
// gets a clear error in a browser (rather than silently wrong output).
async function defaultNodeCrypto() {
  try {
    // A dynamic `import("node:crypto")` keeps this a portable ES module: on Node
    // it resolves the built-in; in a plain browser it throws and we fall to null.
    if (typeof process !== "undefined" && process.versions && process.versions.node) {
      const mod = await import("node:crypto");
      return mod.default || mod;
    }
  } catch (_e) {
    // fall through to null
  }
  return null;
}

// The crypto backend the shim uses. `nodeCrypto` (if present) backs the wider
// algorithms; SHA-256/HMAC always use the bundled pure-JS core for portability.
function makeCryptoBackend(nodeCrypto) {
  const need = (name) => {
    if (!nodeCrypto) {
      throw new Error(
        `witchy-runtime: '${name}' needs a platform crypto backend (Node's node:crypto); ` +
        `it is unavailable in this environment (only sha256/hmac_sha256/rune_hash work without it)`,
      );
    }
    return nodeCrypto;
  };
  return {
    sha256: (bytes) => sha256Bytes(bytes),
    hmacSha256: (key, msg) => hmacSha256Bytes(key, msg),
    sha512: (bytes) => new Uint8Array(need("crypto.sha512").createHash("sha512").update(bytes).digest()),
    sha3_256: (bytes) => new Uint8Array(need("crypto.sha3_256").createHash("sha3-256").update(bytes).digest()),
    // Ed25519: pk/sig are RAW bytes (the host decodes the hex before calling).
    ed25519Verify: (pk, msg, sig) => {
      const c = need("crypto.ed25519_verify");
      try {
        const key = c.createPublicKey({
          key: Buffer.concat([
            Buffer.from("302a300506032b6570032100", "hex"), // SPKI Ed25519 prefix
            Buffer.from(pk),
          ]),
          format: "der",
          type: "spki",
        });
        return c.verify(null, Buffer.from(msg), key, Buffer.from(sig));
      } catch (_e) {
        return false;
      }
    },
  };
}

// ---------------------------------------------------------------------------
// Pure helpers that mirror src/native.rs byte-for-byte.
// ---------------------------------------------------------------------------

const utf8 = new TextEncoder();
// Lossy UTF-8 decode (matches Rust's `String::from_utf8_lossy`: invalid bytes
// become U+FFFD). `fatal:false` is the default, but be explicit.
const decodeLossy = (bytes) => new TextDecoder("utf-8", { fatal: false }).decode(bytes);

// render_float — matches src/fmt.rs::render_float: a finite whole-valued float
// gets a trailing ".0"; otherwise the shortest round-trip decimal (Rust's `{x}`
// and JS `Number.toString()` both emit the shortest round-trip form).
export function renderFloat(x) {
  if (Number.isFinite(x) && Math.floor(x) === x && !Object.is(x, -0)) {
    return x.toFixed(1);
  }
  if (Object.is(x, -0)) return "-0.0";
  return String(x);
}

// string.from_code — a single Unicode scalar value as UTF-8; out-of-range or a
// surrogate yields U+FFFD (matches src/native.rs::string::from_code).
function stringFromCode(cp) {
  const n = Number(cp);
  let ch;
  if (!Number.isInteger(n) || n < 0 || n > 0x10ffff || (n >= 0xd800 && n <= 0xdfff)) {
    ch = "�";
  } else {
    ch = String.fromCodePoint(n);
  }
  return utf8.encode(ch);
}

const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

function hexToBytes(s) {
  const nib = (c) => {
    if (c >= 48 && c <= 57) return c - 48;
    if (c >= 97 && c <= 102) return c - 97 + 10;
    if (c >= 65 && c <= 70) return c - 65 + 10;
    return -1;
  };
  const cs = [];
  for (let i = 0; i < s.length; i++) {
    const v = nib(s.charCodeAt(i));
    if (v >= 0) cs.push(v);
  }
  const out = [];
  for (let i = 0; i + 1 < cs.length; i += 2) out.push(cs[i] * 16 + cs[i + 1]);
  return new Uint8Array(out);
}

// encoding(op, input) -> string. op: 0 hex_encode, 1 hex_decode, 2 base64_encode,
// 3 base64_decode, 4 base64url_of_hex. Mirrors src/native.rs::encoding exactly.
function encodingOp(op, input /* Uint8Array */) {
  switch (op) {
    case 0: { // hex_encode of the UTF-8 bytes
      return toHex(input);
    }
    case 1: { // hex_decode (lossy UTF-8); whitespace skipped, odd/non-hex tail ignored
      const text = decodeLossy(input);
      const digits = [];
      for (const ch of text) {
        const c = ch.charCodeAt(0);
        if (c === 9 || c === 10 || c === 13 || c === 32 || c === 11 || c === 12) continue;
        digits.push(c);
      }
      const bytes = [];
      for (let i = 0; i + 1 < digits.length; i += 2) {
        const hi = parseInt(String.fromCharCode(digits[i]), 16);
        const lo = parseInt(String.fromCharCode(digits[i + 1]), 16);
        if (Number.isNaN(hi) || Number.isNaN(lo)) break;
        bytes.push(hi * 16 + lo);
      }
      return decodeLossy(new Uint8Array(bytes));
    }
    case 2: { // standard base64 (=-padded) of the UTF-8 bytes
      let out = "";
      for (let i = 0; i < input.length; i += 3) {
        const b0 = input[i], b1 = input[i + 1] || 0, b2 = input[i + 2] || 0;
        const len = input.length - i;
        const n = (b0 << 16) | (b1 << 8) | b2;
        out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63];
        out += len > 1 ? B64[(n >> 6) & 63] : "=";
        out += len > 2 ? B64[n & 63] : "=";
      }
      return out;
    }
    case 3: { // base64_decode (lossy UTF-8); padding/whitespace tolerated
      const text = decodeLossy(input);
      let acc = 0, nbits = 0;
      const bytes = [];
      for (const ch of text) {
        const c = ch;
        if (c === "=" || /\s/.test(c)) continue;
        const v = B64.indexOf(c);
        if (v < 0) break;
        acc = (acc << 6) | v;
        nbits += 6;
        if (nbits >= 8) {
          nbits -= 8;
          bytes.push((acc >> nbits) & 0xff);
        }
      }
      return decodeLossy(new Uint8Array(bytes));
    }
    case 4: { // base64url (no padding) of the bytes given as a HEX string
      const bytes = hexToBytes(decodeLossy(input));
      let out = "";
      for (let i = 0; i < bytes.length; i += 3) {
        const b0 = bytes[i], b1 = bytes[i + 1] || 0, b2 = bytes[i + 2] || 0;
        const len = bytes.length - i;
        const n = (b0 << 16) | (b1 << 8) | b2;
        out += B64URL[(n >> 18) & 63] + B64URL[(n >> 12) & 63];
        if (len > 1) out += B64URL[(n >> 6) & 63];
        if (len > 2) out += B64URL[n & 63];
      }
      return out;
    }
    default:
      throw new Error(`witchy-runtime: unknown encoding op ${op}`);
  }
}

// regex.match_spans(pattern, text) -> "s,e;s,e;..." in CHARACTER indices (matches
// src/native.rs::regexp::match_spans). JS RegExp is not RE2, but for the
// pure-compute target it is the closest portable engine; documented in
// spec/wasm-abi.md as an approximation of the native `regex` crate.
function matchSpans(pattern, text) {
  let re;
  try {
    re = new RegExp(pattern, "gu");
  } catch (_e) {
    try {
      re = new RegExp(pattern, "g");
    } catch (e2) {
      throw new Error(`witchy-runtime: regex: invalid pattern \`${pattern}\`: ${e2.message}`);
    }
  }
  // Char offset of a JS string-index (UTF-16 code unit) as a code-POINT count,
  // matching Rust char indices.
  const charOff = (idx) => [...text.slice(0, idx)].length;
  const parts = [];
  let m;
  while ((m = re.exec(text)) !== null) {
    const s = charOff(m.index);
    const e = charOff(m.index + m[0].length);
    parts.push(`${s},${e}`);
    if (m[0].length === 0) re.lastIndex++; // avoid an infinite loop on empty matches
  }
  return parts.join(";");
}

// rune_hash(paths, contents) -> "sha256:<hex>" (matches src/native.rs::rune_hash:
// LF-normalize each content, sort by path, length-prefixed concat, sha256).
function runeHash(paths, contents) {
  if (paths.length !== contents.length) {
    throw new Error("witchy-runtime: crypto.rune_hash: paths and contents differ in length");
  }
  const normalizeLf = (bytes) => {
    // Collapse CRLF -> LF only; a lone CR is left unchanged (matches
    // src/native.rs::normalize_lf exactly).
    const out = [];
    for (let i = 0; i < bytes.length; i++) {
      if (bytes[i] === 13 && bytes[i + 1] === 10) {
        out.push(10);
        i++;
      } else {
        out.push(bytes[i]);
      }
    }
    return new Uint8Array(out);
  };
  const files = paths.map((p, i) => ({ path: p, bytes: normalizeLf(utf8.encode(contents[i])) }));
  files.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  const chunks = [];
  const u64le = (n) => {
    const b = new Uint8Array(8);
    const dv = new DataView(b.buffer);
    dv.setUint32(0, n >>> 0, true);
    dv.setUint32(4, Math.floor(n / 0x100000000), true);
    return b;
  };
  for (const f of files) {
    const pb = utf8.encode(f.path);
    chunks.push(u64le(pb.length), pb, u64le(f.bytes.length), f.bytes);
  }
  let total = 0;
  for (const c of chunks) total += c.length;
  const buf = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) { buf.set(c, off); off += c.length; }
  return "sha256:" + toHex(sha256Bytes(buf));
}

// ---------------------------------------------------------------------------
// The instantiation entry point.
// ---------------------------------------------------------------------------

/**
 * Instantiate a witchyc-compiled, footprint-empty WASM module under the
 * pure-compute host. Provides only the NON-capability `"witchy"` imports; a
 * module that imports any capability fails to instantiate with a `LinkError`
 * (deny-by-omission). Returns `{ instance, output, run }`.
 *
 * @param {BufferSource} wasmBytes  the compiled module
 * @param {object} [opts]
 * @param {(line: string) => void} [opts.onPrint]  called per printed line; if
 *        omitted, lines accumulate in the returned `output` array
 * @param {object} [opts.cryptoBackend]  override the crypto backend (testing)
 * @param {object} [opts.nodeCrypto]  a `node:crypto`-shaped object (auto-detected
 *        on Node when omitted)
 */
export async function instantiate(wasmBytes, opts = {}) {
  const output = [];
  const onPrint = opts.onPrint || ((line) => output.push(line));
  const nodeCrypto = opts.nodeCrypto !== undefined ? opts.nodeCrypto : await defaultNodeCrypto();
  const crypto = opts.cryptoBackend || makeCryptoBackend(nodeCrypto);

  // Bound late: instance memory is set after instantiation, but the import
  // closures capture `mem` by reference through this object.
  let memory = null;
  const u8 = () => new Uint8Array(memory.buffer);
  const dv = () => new DataView(memory.buffer);

  // Read a witchy String header `[i32 len][bytes]` at `ptr` -> Uint8Array bytes.
  const readWstr = (ptr) => {
    const len = dv().getInt32(ptr, true);
    return u8().slice(ptr + 4, ptr + 4 + len);
  };
  const readWstrText = (ptr) => decodeLossy(readWstr(ptr));
  // Read a witchy List(String) `[i32 count][count x i64 ptr]` -> string[].
  const readWstrList = (ptr) => {
    const count = dv().getInt32(ptr, true);
    const out = [];
    for (let i = 0; i < count; i++) {
      const lo = dv().getUint32(ptr + 4 + 8 * i, true);
      const hi = dv().getUint32(ptr + 4 + 8 * i + 4, true);
      const elem = hi * 0x100000000 + lo;
      out.push(readWstrText(elem));
    }
    return out;
  };
  // Write `bytes` at `outPtr`; return the byte length (the two-call protocol's
  // fill step expects the guest to have reserved exactly this many bytes).
  const writeAt = (bytes, outPtr) => {
    u8().set(bytes, outPtr);
    return bytes.length;
  };

  // The pending buffer: a host->guest transfer staged by a `*_len` call and
  // drained by the matching `fill_pending`. Mirrors `VmState::pending` in
  // src/runtime.rs (read once, no time-of-check/use gap).
  let pending = null;

  // The witchy import object — PURE functions only. No `dir_*`, `net_*`,
  // `exec_run`, `now`, `env_*`, `args_size`, `secretstore_*`, `crypto_reveal_*`,
  // `build_*`, `compiler_*`, `crypto.sign`, `crypto.public_key`: those are
  // capabilities (or interpreter-only host services) and are DELIBERATELY ABSENT,
  // so a module importing one cannot instantiate here.
  const witchy = {
    // --- output (capturable; output is not authority) ---
    print(ptr, len) {
      // `print` receives a RAW (ptr,len) buffer (the guest's $print_str strips
      // the String header), so read the bytes directly. Trailing newlines are
      // trimmed to mirror the wasmtime host's `trim_end_matches('\n')`.
      onPrint(decodeLossy(u8().slice(ptr, ptr + len)).replace(/\n+$/, ""));
    },
    print_int(n) {
      onPrint(BigInt.asIntN(64, n).toString());
    },
    print_float(x) {
      onPrint(renderFloat(x));
    },

    // --- the pending-buffer string-bridge (pure mechanics, no authority) ---
    fill_pending(outPtr) {
      if (pending === null) throw new Error("witchy-runtime: fill_pending called with nothing staged");
      const bytes = pending;
      pending = null;
      writeAt(bytes, outPtr);
    },
    // write_pending_list lays a staged List(String) out at base_ptr. The shim
    // never stages a list (args_size/dir_list_size are capabilities and absent),
    // so it can only legitimately run with nothing staged — but the import must
    // exist because a footprint-empty module that builds a list literal MAY
    // import it. With nothing staged it is a no-op.
    write_pending_list(_basePtr) {
      // No pure host op stages a list here; honor the call as a no-op.
    },
    // (RFC-0040) user_cap_field_len(param, field): stage the (param, field) policy
    // string of a bare grantable-capability grant for `fill_pending`, returning its
    // byte length. Bare grantable caps carry NO host authority — this is the browser
    // `[user_caps]` grant (policy DATA the app declares, the mirror of `--grants`),
    // not a capability, so the pure/deny-by-omission host may provide it. A missing
    // grant traps, mirroring the wasmtime host (both backends refuse identically).
    user_cap_field_len(param, field) {
      const fields = (opts.userCaps || [])[param];
      const val = fields ? fields[field] : undefined;
      if (val === undefined) {
        throw new Error(
          "witchy-runtime: user_cap_field_len — the app declared a grantable capability but no [user_caps] grant was provided (opts.userCaps)"
        );
      }
      pending = new TextEncoder().encode(String(val));
      return pending.length;
    },

    // --- pure formatting / encoding ---
    float_to_str(x, outPtr) {
      return writeAt(utf8.encode(renderFloat(x)), outPtr);
    },
    string_from_code(cp, outPtr) {
      return writeAt(stringFromCode(cp), outPtr);
    },
    encoding(op, inPtr, outPtr) {
      return writeAt(utf8.encode(encodingOp(op, readWstr(inPtr))), outPtr);
    },
    regex_match_spans_len(patPtr, textPtr) {
      const spans = matchSpans(readWstrText(patPtr), readWstrText(textPtr));
      pending = utf8.encode(spans);
      return pending.length;
    },

    // --- pure crypto (mirrors src/runtime.rs host_* bridges) ---
    "crypto.sha256"(inPtr, outPtr) {
      writeAt(utf8.encode(toHex(crypto.sha256(readWstr(inPtr)))), outPtr);
    },
    "crypto.sha512"(inPtr, outPtr) {
      writeAt(utf8.encode(toHex(crypto.sha512(readWstr(inPtr)))), outPtr);
    },
    "crypto.sha3_256"(inPtr, outPtr) {
      writeAt(utf8.encode(toHex(crypto.sha3_256(readWstr(inPtr)))), outPtr);
    },
    "crypto.hmac_sha256"(keyPtr, msgPtr, outPtr) {
      // The key is a HEX string (so binary keys are representable), the message
      // raw bytes — matching src/native.rs::crypto::hmac_sha256.
      const key = hexToBytes(readWstrText(keyPtr));
      writeAt(utf8.encode(toHex(crypto.hmacSha256(key, readWstr(msgPtr)))), outPtr);
    },
    "crypto.rune_hash"(pathsPtr, contentsPtr, outPtr) {
      const hash = runeHash(readWstrList(pathsPtr), readWstrList(contentsPtr));
      writeAt(utf8.encode(hash), outPtr);
    },
    "crypto.ed25519_verify"(pkPtr, msgPtr, sigPtr) {
      const pk = hexToBytes(readWstrText(pkPtr));
      const sig = hexToBytes(readWstrText(sigPtr));
      return crypto.ed25519Verify(pk, readWstr(msgPtr), sig) ? 1 : 0;
    },
    "crypto.ecdsa_p256_verify"(_pk, _msg, _sig) {
      throw new Error("witchy-runtime: crypto.ecdsa_p256_verify is not supported in the pure-compute host");
    },
    "crypto.ecdsa_p256_verify_hex"(_pk, _msg, _sig) {
      throw new Error("witchy-runtime: crypto.ecdsa_p256_verify_hex is not supported in the pure-compute host");
    },

    // --- reflection field-length stubs (pure reads; ordinary programs never
    // reach them — they are an interpreter host detail; return 0, matching the
    // harmless stubs in src/runtime.rs) ---
    field_str_len(_h) { return 0; },
    field_intlist_len(_h) { return 0; },
    field_strlist_size(_h) { return 0; },
  };

  const { instance } = await WebAssembly.instantiate(wasmBytes, { witchy });
  memory = instance.exports.memory;

  const run = () => {
    if (typeof instance.exports.run !== "function") {
      throw new Error("witchy-runtime: the module does not export `run`");
    }
    instance.exports.run();
    return output;
  };

  // callString(exportName, str) -> str : the `String -> String` export ABI
  // (RFC-0008 §1 / RFC-0007 §"Data marshaling"). A `pub fn export_f(String) ->
  // String` compiles to a `__export_f(in_ptr, in_len) -> out_ptr` export plus a
  // `__galloc(len) -> ptr` bump allocator. We:
  //   1. encode `str` to UTF-8 bytes,
  //   2. `__galloc(4 + len)` to reserve a witchy String header `[i32 len][bytes]`,
  //   3. write the header + bytes into guest memory,
  //   4. call `__export_f(ptr, len)` -> a pointer to a result String header,
  //   5. read `[i32 len][bytes]` back out as a JS string.
  // Pure mechanics over guest memory — no capability, no authority.
  // The `heap` bump-allocator pointer, exported by every heap-using module as `__heap`. Its
  // value right after instantiation is the pristine base (the end of the data section, before
  // any allocation). A `String -> String` export is PURE — its input/working/output allocations
  // are all dead once we read the result out — so we restore the pointer to this base after each
  // call. Without it the never-freeing bump allocator leaks one call's worth of memory per call
  // and a long-lived run loop (glamour MVU: one call per event) eventually exhausts memory and
  // `__galloc` returns an out-of-bounds pointer. Memory then stays at the high-water mark of a
  // single call instead of growing without bound. (Older modules without `__heap` skip the reset.)
  const heapGlobal = instance.exports.__heap;
  const heapBase = heapGlobal != null ? heapGlobal.value : null;
  const callString = (exportName, str) => {
    const fn = instance.exports[exportName];
    if (typeof fn !== "function") {
      throw new Error(`witchy-runtime: the module does not export \`${exportName}\``);
    }
    const galloc = instance.exports.__galloc;
    if (typeof galloc !== "function") {
      throw new Error("witchy-runtime: the module does not export `__galloc`");
    }
    const bytes = utf8.encode(str);
    const inPtr = galloc(4 + bytes.length);
    dv().setInt32(inPtr, bytes.length, true); // little-endian length header
    u8().set(bytes, inPtr + 4);
    const outPtr = fn(inPtr, bytes.length);
    const outLen = dv().getInt32(outPtr, true);
    const result = decodeLossy(u8().slice(outPtr + 4, outPtr + 4 + outLen));
    // Free everything this call allocated: the result is now a detached JS string.
    if (heapGlobal != null) heapGlobal.value = heapBase;
    return result;
  };

  return {
    instance,
    output,
    run,
    callString,
    get memory() { return memory; },
  };
}

/**
 * Convenience: instantiate `wasmBytes` and immediately call its `exportName`
 * string export with `str`, returning the result string. For one-shot
 * `String -> String` calls (the glamour `step` loop holds the instance instead).
 */
export async function callStringExport(wasmBytes, exportName, str, opts = {}) {
  const { callString } = await instantiate(wasmBytes, opts);
  return callString(exportName, str);
}
