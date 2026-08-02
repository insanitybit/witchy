// witchy-runtime — a JavaScript host for witchyc-compiled WASM that implements
// only the browser-supported non-authority subset of the `"witchy"` import ABI
// and DENIES every capability import. It is the browser/Node analog of the
// wasmtime host in `crates/witchy-runtime/src/runtime.rs`, with the capability
// set fixed to empty.
//
// This is RFC-0007 ("witchy-WASM in the browser: a pure-compute target"). The
// containment guarantee is structural: witchyc TREE-SHAKES imports (a module
// declares only the host functions it actually reaches — see spec/wasm-abi.md),
// so a rune that touches Net/Dir/Clock/etc. imports a capability host function
// this shim does NOT provide — and `WebAssembly.instantiate` then throws a
// `LinkError` for the missing import. Deny-by-omission: the host admits no
// authority-bearing module, while also omitting native-only non-authority
// services such as argv and compiler introspection. No trap stubs are needed
// for the guarantee (none are installed).
//
// The ABI this targets is declared in `crates/witchy-wir/src/wir_prelude.rs`
// and the wasmtime implementations live in
// `crates/witchy-runtime/src/runtime.rs`. The shared functions below mirror
// those byte-for-byte, so a browser run and a native run agree on every
// observable byte (the parity rule of CLAUDE.md). See spec/wasm-abi.md.

// The ABI version this shim implements. Bump in lockstep with a breaking change
// to the `"witchy"` import surface (a renamed/re-signatured pure import, or a
// change to the pending-buffer protocol). RFC-0007 §"ABI stabilization".
export const WITCHY_ABI_VERSION = 7;

// Exact deny-by-omission surface implemented below. The Rust ABI catalog test
// compares this list with `wir_prelude` and `instantiate` compares it with the
// actual import object, so adding a host function requires one explicit ABI
// classification instead of silently widening the browser host.
export const WITCHY_BROWSER_IMPORTS = Object.freeze([
  "__witchy_abort",
  "args_size",
  "crypto.__ecdsa_p256_verify_hex_status",
  "crypto.__ecdsa_p256_verify_status",
  "crypto.__ed25519_verify_status",
  "crypto.__rsa_pkcs1_sha256_verify_status",
  "crypto.hmac_sha256",
  "crypto.rune_hash",
  "crypto.sha256",
  "crypto.sha3_256",
  "crypto.sha512",
  "encoding",
  "field_intlist_len",
  "field_str_len",
  "field_strlist_size",
  "fill_pending",
  "float_to_str",
  "print",
  "print_float",
  "print_int",
  "regex_match_spans_len",
  "string_from_code",
  "user_cap_field_len",
  "write_pending_list",
]);

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
// crypto verify status / the p256 verifies). Returns null in a plain browser, where
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
      const c = need("crypto.__ed25519_verify_status");
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

// STRICT hex decode, byte-for-byte with native `hex_decode`/`hex_bytes`
// (crates/witchy-runtime/src/native.rs): an odd length or ANY non-hex character
// (whitespace included) is a hard reject — returns `null`, never a silently
// filtered/truncated buffer. The old lossy codec dropped non-hex chars and an
// odd tail, so the browser accepted keys/signatures native REJECTS — a
// parity/security divergence (BUG-276). Crypto verifier status imports map
// malformed hex to negative status codes, matching native.
export function hexToBytes(s) {
  const nib = (c) => {
    if (c >= 48 && c <= 57) return c - 48;
    if (c >= 97 && c <= 102) return c - 97 + 10;
    if (c >= 65 && c <= 70) return c - 65 + 10;
    return -1;
  };
  if (s.length % 2 !== 0) return null;
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < s.length; i += 2) {
    const hi = nib(s.charCodeAt(i));
    const lo = nib(s.charCodeAt(i + 1));
    if (hi < 0 || lo < 0) return null;
    out[i / 2] = hi * 16 + lo;
  }
  return out;
}

function base64String(input, alphabet, padding) {
  let out = "";
  for (let i = 0; i < input.length; i += 3) {
    const b0 = input[i], b1 = input[i + 1] || 0, b2 = input[i + 2] || 0;
    const len = input.length - i;
    const n = (b0 << 16) | (b1 << 8) | b2;
    out += alphabet[(n >> 18) & 63] + alphabet[(n >> 12) & 63];
    if (len > 1) out += alphabet[(n >> 6) & 63];
    else if (padding) out += "=";
    if (len > 2) out += alphabet[n & 63];
    else if (padding) out += "=";
  }
  return out;
}

const isAsciiWhitespace = (b) => b === 0x20 || (b >= 0x09 && b <= 0x0d);

// Match native `base64_bytes`: padding and ASCII whitespace are ignored, and
// the first non-alphabet byte ends the raw decode. Public witchy wrappers reject
// malformed input before reaching these primitives.
function base64Bytes(input, alphabet) {
  let acc = 0, nbits = 0;
  const bytes = [];
  for (const b of input) {
    if (b === 0x3d || isAsciiWhitespace(b)) continue;
    const v = alphabet.indexOf(String.fromCharCode(b));
    if (v < 0) break;
    acc = ((acc << 6) | v) >>> 0;
    nbits += 6;
    if (nbits >= 8) {
      nbits -= 8;
      bytes.push((acc >>> nbits) & 0xff);
    }
  }
  return new Uint8Array(bytes);
}

// `encoding(op, input) -> bytes`, mirroring the native host's complete op
// table. Text results are returned as UTF-8 bytes; byte decoders stay raw.
export function encodingOp(op, input /* Uint8Array */) {
  switch (op) {
    case 0: // hex_encode of a String's UTF-8 bytes
    case 8: { // hex_encode_bytes
      return utf8.encode(toHex(input));
    }
    case 1: { // hex_decode: lossy UTF-8 of the DECODED bytes. The hex alphabet is
      // decoded STRICTLY — a non-hex char or odd length is a hard error, matching
      // native `encoding::hex_decode_lossy` (BUG-276), never a silent whitespace-
      // skip / odd-tail drop.
      const bytes = hexToBytes(decodeLossy(input));
      if (bytes === null) throw new Error("encoding.hex_decode: input is not valid hex");
      return utf8.encode(decodeLossy(bytes));
    }
    case 2: // base64_encode of a String's UTF-8 bytes
    case 9: { // base64_encode_bytes
      return utf8.encode(base64String(input, B64, true));
    }
    case 3: { // base64_decode (lossy UTF-8); padding/whitespace tolerated
      return utf8.encode(decodeLossy(base64Bytes(input, B64)));
    }
    case 4: { // base64url (no padding) of the bytes given as a HEX string; the hex
      // is decoded STRICTLY, matching native `encoding::hex_to_base64url` (BUG-276).
      const bytes = hexToBytes(decodeLossy(input));
      if (bytes === null) throw new Error("encoding.hex_to_base64url: input is not valid hex");
      return utf8.encode(base64String(bytes, B64URL, false));
    }
    case 5: { // base64url_decode_lossy
      return utf8.encode(decodeLossy(base64Bytes(input, B64URL)));
    }
    case 6: { // base64url_to_hex_lossy
      return utf8.encode(toHex(base64Bytes(input, B64URL)));
    }
    case 7: { // utf8_lossy (bytes.to_string): lossy UTF-8 decode, invalid -> U+FFFD.
      // `input` was read raw (readWstr); decode it lossily here so the JS host
      // matches the interpreter's `String::from_utf8_lossy` byte-for-byte.
      return utf8.encode(decodeLossy(input));
    }
    case 10: { // base64url_encode_bytes
      return utf8.encode(base64String(input, B64URL, false));
    }
    case 11: { // hex_decode_bytes_raw
      const bytes = hexToBytes(decodeLossy(input));
      if (bytes === null) throw new Error("encoding.hex_decode_bytes: input is not valid hex");
      return bytes;
    }
    case 12: { // base64_decode_bytes_raw
      return base64Bytes(input, B64);
    }
    case 13: { // base64url_decode_bytes_raw
      return base64Bytes(input, B64URL);
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
// (RFC-0091) The OPT-IN teaching/playground capability host.
//
// The default `instantiate` above stays capability-DENIED (deny-by-omission).
// A caller that explicitly passes `opts.capabilities` opts into a SUPERSET
// import object that adds ONLY the requested families — never an ambient
// widening. The families the browser can honestly back are:
//
//   * Clock  — real browser wall/monotonic time on the existing `now`/
//              `now_monotonic` i64 ABI (no virtualization needed).
//   * Env    — EMPTY by default; a page may supply an immutable string map.
//              An absent name reads back as unset (`env_len` -> -1), exactly
//              like the native host's unset variable.
//   * Dir    — a per-run IN-MEMORY tree. Path normalization and `..`/absolute
//              confinement, the entry-policy guard (`dir_only`/`dir_admits`),
//              read/write/append/list/subtree/exists/is_dir/open/create and the
//              File handles they mint all mirror the native `Dir` semantics in
//              crates/witchy-runtime/src/runtime.rs + crates/witchy-caps. It
//              NEVER touches a real filesystem (no `node:fs`): the whole tree
//              lives in JS `Map`/`Set` state scoped to this instantiation.
//   * Fetch  — real browser fetch(), scoped to an explicit canonical-origin
//              allowlist. `fetch_send_len` is a JSPI-suspending import, so the
//              guest's ordinary synchronous call resumes after the Promise
//              settles without blocking the browser thread.
//   * SecretStore — an immutable page-supplied named map. Opaque externrefs
//              preserve use-only policy; WebCrypto sign/public-key calls
//              suspend through JSPI without exposing key bytes to the guest.
//
// `Exec`, bare root `Secret`, raw `Net`, `mint_file` (a top-level File grant), and
// compiler introspection remain DENIED BY OMISSION even under this host — they
// are simply not built, so a module reaching them still fails to instantiate
// with a `LinkError`. See rfcs/0091-browser-virtual-capabilities.md.
// ---------------------------------------------------------------------------

// The opt-in capability import surfaces, one frozen list per family. Adding a
// host function to a family requires listing it here first — the same explicit
// classification the pure `WITCHY_BROWSER_IMPORTS` list enforces, so the
// playground host can never silently widen either.
export const WITCHY_CLOCK_IMPORTS = Object.freeze(["now", "now_monotonic"]);
export const WITCHY_CONSOLE_IMPORTS = Object.freeze(["console_read_len"]);
export const WITCHY_ENV_IMPORTS = Object.freeze([
  "mint_env",
  "env_only",
  "env_len",
  "env_fill",
]);
export const WITCHY_DIR_IMPORTS = Object.freeze([
  // Dir capability ops (RFC-0005 externref handles) …
  "mint_dir",
  "dir_subdir",
  "dir_only",
  "dir_open",
  "dir_create",
  "dir_read_len",
  "dir_list_size",
  "dir_exists",
  "dir_is_dir",
  "dir_write",
  "dir_append",
  "dir_make_dir",
  // … plus the File handles `dir_open`/`dir_create` mint (a Dir-derived File,
  // NOT the top-level `mint_file` grant, which stays denied).
  "file_read_len",
  "file_write",
]);
export const WITCHY_FETCH_IMPORTS = Object.freeze([
  "mint_fetch",
  "fetch_only",
  "fetch_send_len",
]);
export const WITCHY_SECRET_STORE_IMPORTS = Object.freeze([
  "crypto.sign",
  "crypto.public_key",
  "secretstore_lookup",
  "crypto_reveal_len",
]);
export const WITCHY_VM_IMPORTS = Object.freeze([
  "vm_par_map_run",
  "vm_par_map_write",
  "vm_par_map_bytes_run",
  "vm_par_map_bytes_write",
  "vm_with_dir_run",
  "vm_serve_run",
]);

// The DIR_DENY_ALL sentinel — a single NUL (U+0000), byte-identical to
// `crates/witchy-caps/src/capabilities.rs` (`const DIR_DENY_ALL: &str = "\u{0}"`).
// Written as an escape so this source stays plain ASCII. Produced by `dirOnly`
// and consumed by `dirAdmits`; no legitimate `ext:`/`kind:` pattern can collide
// with it (they all contain a `:`), so a fail-closed narrowing stays fail-closed.
const DIR_DENY_ALL = "\u0000";

// Normalize a Dir-relative path the way native `mock_normalize` /
// `confine::resolve` do LEXICALLY: reject absolute paths and any `..`
// component, drop `.` and empty segments, join the rest with `/`. In an
// in-memory tree there are no symlinks, so this lexical check IS the full
// confinement boundary (the security invariant: a `Dir` is a subtree). Throws
// on an escape attempt, exactly as the native host traps.
function normalizeMemPath(rel) {
  // The reference (interpreter/native) uses Unix `std::path` semantics: only a
  // leading `/` is absolute; backslash is an ordinary character. Match that.
  if (rel.startsWith("/")) {
    throw new Error(`in-memory Dir path \`${rel}\` must be relative`);
  }
  const out = [];
  for (const seg of rel.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      throw new Error(`\`..\` escapes the Dir capability`);
    }
    out.push(seg);
  }
  return out.join("/");
}

// Join a Dir handle's `root` with a relative path (mirrors native `mock_join`).
function mockJoin(root, rel) {
  const norm = normalizeMemPath(rel);
  if (norm === "") return root;
  if (root === "") return norm;
  return `${root}/${norm}`;
}

// Port of `witchy_caps::capabilities::dir_admits`: whether an entry policy
// admits touching `name` (a directory iff `isDir`). An empty policy admits all.
function dirAdmits(policy, name, isDir) {
  if (policy === "") return true;
  let hasExt = false, extOk = false, hasKind = false, kindOk = false;
  for (const p of policy.split("\n")) {
    if (p === DIR_DENY_ALL) return false;
    else if (p.startsWith("ext:")) {
      hasExt = true;
      const ext = p.slice(4);
      if (isDir || name.endsWith(ext)) extOk = true;
    } else if (p.startsWith("kind:")) {
      hasKind = true;
      const kind = p.slice(5);
      if ((kind === "dir") === isDir) kindOk = true;
    }
  }
  return (!hasExt || extOk) && (!hasKind || kindOk);
}

// Port of `witchy_caps::capabilities::dir_only`: narrow `current` by `refine`
// (intersect within a shared dimension, AND across dimensions; a non-empty
// refine with no valid `dim:pattern` fails closed to DIR_DENY_ALL — BUG-257).
function dirOnly(current, refine) {
  if (refine === "") return current;
  const group = (s) => {
    const m = new Map();
    for (const p of s.split("\n")) {
      const idx = p.indexOf(":");
      if (idx >= 0) {
        const dim = p.slice(0, idx);
        if (!m.has(dim)) m.set(dim, new Set());
        m.get(dim).add(p);
      }
    }
    return m;
  };
  const refi = group(refine);
  if (refi.size === 0) return DIR_DENY_ALL;
  if (current === "") return refine;
  const cur = group(current);
  const out = new Set();
  for (const [dim, pats] of cur) {
    if (!refi.has(dim)) for (const x of pats) out.add(x);
  }
  for (const [dim, rpats] of refi) {
    const cpats = cur.get(dim);
    if (cpats) {
      const common = [...rpats].filter((x) => cpats.has(x));
      if (common.length === 0) return DIR_DENY_ALL;
      for (const x of common) out.add(x);
    } else {
      for (const x of rpats) out.add(x);
    }
  }
  // Native collects into a BTreeSet (sorted), then joins with '\n'.
  return [...out].sort().join("\n");
}

// A per-run in-memory filesystem: `files` maps a normalized relative path to
// its raw bytes; `dirs` records explicitly-created (possibly empty) directory
// paths. Directories are otherwise implicit by path prefix. Mirrors the native
// mock backing, extended to be writable like the native Fs backing.
function makeMemFs(filesObj) {
  const files = new Map();
  const dirs = new Set();
  const recordAncestors = (path) => {
    const parts = path.split("/");
    parts.pop();
    let acc = "";
    for (const part of parts) {
      acc = acc ? `${acc}/${part}` : part;
      dirs.add(acc);
    }
  };
  for (const [k, v] of Object.entries(filesObj || {})) {
    const p = normalizeMemPath(k);
    if (p === "") throw new Error("in-memory Dir entry path must name a file");
    const bytes = typeof v === "string" ? utf8.encode(v) : new Uint8Array(v);
    files.set(p, bytes);
    recordAncestors(p);
  }
  return { files, dirs, recordAncestors };
}

function memIsDir(fs, path) {
  if (path === "") return true;
  if (fs.dirs.has(path)) return true;
  const prefix = `${path}/`;
  for (const k of fs.files.keys()) if (k.startsWith(prefix)) return true;
  for (const d of fs.dirs) if (d.startsWith(prefix)) return true;
  return false;
}

function memExists(fs, path) {
  return fs.files.has(path) || memIsDir(fs, path);
}

function memList(fs, path) {
  if (!memIsDir(fs, path)) {
    throw new Error(`list failed for in-memory Dir \`${path}\`: not a directory`);
  }
  const prefix = path === "" ? "" : `${path}/`;
  const names = new Set();
  const firstSeg = (full) => {
    const rest = full.slice(prefix.length);
    if (rest === "") return null;
    const slash = rest.indexOf("/");
    return slash < 0 ? rest : rest.slice(0, slash);
  };
  for (const k of fs.files.keys()) {
    if (k.startsWith(prefix)) {
      const name = firstSeg(k);
      if (name) names.add(name);
    }
  }
  for (const d of fs.dirs) {
    if (d.startsWith(prefix)) {
      const name = firstSeg(d);
      if (name) names.add(name);
    }
  }
  return [...names].sort();
}

function memParent(path) {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? "" : path.slice(0, slash);
}

// Writing requires the parent directory to exist, mirroring native
// `confine::resolve_write` (which canonicalizes the parent).
function memRequireParent(fs, path) {
  const parent = memParent(path);
  if (parent !== "" && !memIsDir(fs, parent)) {
    throw new Error(`cannot access \`${parent}\`: no such directory`);
  }
}

function memRead(fs, path) {
  const bytes = fs.files.get(path);
  if (bytes === undefined) {
    throw new Error(`read failed for in-memory Dir \`${path}\`: no such file`);
  }
  return bytes;
}

function memWrite(fs, path, bytes) {
  if (path === "") throw new Error("write failed: path names the Dir root, not a file");
  memRequireParent(fs, path);
  fs.files.set(path, bytes);
  fs.recordAncestors(path);
}

function memAppend(fs, path, bytes) {
  if (path === "") throw new Error("append failed: path names the Dir root, not a file");
  memRequireParent(fs, path);
  const existing = fs.files.get(path);
  if (existing === undefined) {
    fs.files.set(path, bytes);
  } else {
    const merged = new Uint8Array(existing.length + bytes.length);
    merged.set(existing, 0);
    merged.set(bytes, existing.length);
    fs.files.set(path, merged);
  }
  fs.recordAncestors(path);
}

// `make_dir` is recursive (native FS uses `create_dir_all`): create every level.
function memMakeDir(fs, path) {
  if (path === "") return;
  fs.dirs.add(path);
  fs.recordAncestors(path);
}

const FETCH_DEFAULT_TIMEOUT_MS = 30_000;
const FETCH_DEFAULT_MAX_RESPONSE_BYTES = 16 * 1024 * 1024;

function fetchFailure(code, message) {
  return { code, message };
}

function invalidFetch(message) {
  return fetchFailure("invalid-request", `invalid Fetch request: ${message}`);
}

function parseFetchUrl(input, originOnly = false) {
  const text = String(input);
  for (let i = 0; i < text.length; i++) {
    const cp = text.charCodeAt(i);
    if (cp <= 0x1f || cp === 0x7f || cp === 0x20) {
      throw invalidFetch("URL contains whitespace or a control character");
    }
  }
  const fragment = text.indexOf("#");
  if (originOnly && fragment >= 0) {
    throw invalidFetch("an origin grant must not contain a path, query, or fragment");
  }
  const requestUrl = fragment >= 0 ? text.slice(0, fragment) : text;
  const schemeEnd = requestUrl.indexOf("://");
  if (schemeEnd < 0) {
    throw invalidFetch("URL is missing `scheme://`");
  }
  const scheme = requestUrl.slice(0, schemeEnd).toLowerCase();
  if (scheme !== "http" && scheme !== "https") {
    throw invalidFetch("Fetch URLs and origins must use `http` or `https`");
  }
  const rest = requestUrl.slice(schemeEnd + 3);
  let authorityEnd = rest.length;
  for (const separator of ["/", "?"]) {
    const index = rest.indexOf(separator);
    if (index >= 0) authorityEnd = Math.min(authorityEnd, index);
  }
  const authority = rest.slice(0, authorityEnd);
  if (authority === "") throw invalidFetch("URL has an empty host");
  if (authority.includes("@")) {
    throw invalidFetch(
      "URL credentials are forbidden; pass explicit authorization headers",
    );
  }

  const defaultPort = scheme === "http" ? 80 : 443;
  let host;
  let port = defaultPort;
  if (authority.startsWith("[")) {
    const close = authority.indexOf("]");
    if (close < 0) throw invalidFetch("unterminated IPv6 host");
    host = authority.slice(0, close + 1);
    const suffix = authority.slice(close + 1);
    if (suffix !== "") {
      if (!suffix.startsWith(":")) throw invalidFetch("invalid IPv6 authority");
      port = parseFetchPort(suffix.slice(1));
    }
  } else {
    const colons = [...authority].filter((char) => char === ":").length;
    if (colons > 1) {
      throw invalidFetch("IPv6 URL hosts must be enclosed in brackets");
    }
    const colon = authority.lastIndexOf(":");
    if (colon >= 0) {
      host = authority.slice(0, colon);
      const portText = authority.slice(colon + 1);
      if (host === "" || portText === "") throw invalidFetch("invalid URL authority");
      port = parseFetchPort(portText);
    } else {
      host = authority;
    }
  }
  host = host.toLowerCase();
  const suffix = rest.slice(authorityEnd);
  const pathAndQuery =
    suffix === "" ? "/" : suffix.startsWith("?") ? `/${suffix}` : suffix;
  if (originOnly && pathAndQuery !== "/") {
    throw invalidFetch("an origin grant must not contain a path or query");
  }
  return {
    origin: `${scheme}://${host}:${port}`,
    url: requestUrl,
  };
}

function parseFetchPort(text) {
  if (!/^[0-9]+$/.test(text)) throw invalidFetch("invalid URL port");
  const port = Number(text);
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    throw invalidFetch("invalid URL port");
  }
  return port;
}

function normalizeFetchGrant(grant) {
  if (!grant || typeof grant !== "object" || Array.isArray(grant)) {
    throw new Error(
      "witchy-runtime: Fetch requires an explicit grant object with an `origins` array",
    );
  }
  if (!Array.isArray(grant.origins)) {
    throw new Error(
      "witchy-runtime: Fetch grant must contain an explicit `origins` array",
    );
  }
  const origins = new Set();
  for (const value of grant.origins) {
    try {
      origins.add(parseFetchUrl(String(value), true).origin);
    } catch (error) {
      throw new Error(`witchy-runtime: invalid Fetch grant: ${error.message}`);
    }
  }
  const timeoutMs =
    grant.timeoutMs === undefined ? FETCH_DEFAULT_TIMEOUT_MS : Number(grant.timeoutMs);
  const maxResponseBytes =
    grant.maxResponseBytes === undefined
      ? FETCH_DEFAULT_MAX_RESPONSE_BYTES
      : Number(grant.maxResponseBytes);
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) {
    throw new Error("witchy-runtime: Fetch grant `timeoutMs` must be a positive integer");
  }
  if (!Number.isInteger(maxResponseBytes) || maxResponseBytes < 0) {
    throw new Error(
      "witchy-runtime: Fetch grant `maxResponseBytes` must be a non-negative integer",
    );
  }
  return { origins, timeoutMs, maxResponseBytes };
}

function normalizeSecretStore(spec) {
  if (spec === true) return new Map();
  if (!spec || typeof spec !== "object" || Array.isArray(spec)) {
    throw new Error(
      "witchy-runtime: SecretStore requires `true`, a Map, or a named-secret object",
    );
  }
  const entries = spec instanceof Map ? spec.entries() : Object.entries(spec);
  const secrets = new Map();
  for (const [rawName, rawGrant] of entries) {
    const name = String(rawName);
    let value = rawGrant;
    let useOnly = false;
    if (
      rawGrant
      && typeof rawGrant === "object"
      && !(rawGrant instanceof Uint8Array)
      && !ArrayBuffer.isView(rawGrant)
    ) {
      if (!Object.prototype.hasOwnProperty.call(rawGrant, "value")) {
        throw new Error(`witchy-runtime: secret \`${name}\` is missing its \`value\``);
      }
      value = rawGrant.value;
      useOnly = rawGrant.useOnly === true;
    }
    let bytes;
    if (typeof value === "string") {
      bytes = utf8.encode(value);
    } else if (value instanceof Uint8Array || ArrayBuffer.isView(value)) {
      bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength).slice();
    } else {
      throw new Error(
        `witchy-runtime: secret \`${name}\` must be a string or byte array`,
      );
    }
    secrets.set(name, { bytes, useOnly });
  }
  return secrets;
}

// Normalize `opts.capabilities` into `{ console, clock, env, dir, fetch, secrets, vm }`. Absent/false =>
// that family stays DENIED (its imports are never built). `env` becomes a Map
// (empty when enabled with no entries); `dir` becomes an array of grant specs
// (one per `mint_dir` ordinal), each `{ fs, read, write }`; `fetch` becomes an
// array of explicit origin-scoped grants (one per `mint_fetch` ordinal);
// `secrets` becomes an immutable named map whose copied bytes remain host-side.
function normalizeCapabilities(spec) {
  const out = {
    console: null,
    clock: false,
    env: null,
    dir: null,
    fetch: null,
    secrets: null,
    vm: false,
  };
  if (!spec || typeof spec !== "object") return out;

  if (spec.console === true) {
    out.console = [];
  } else if (spec.console && typeof spec.console === "object") {
    const input = Array.isArray(spec.console.input) ? spec.console.input : [];
    out.console = input.map(String);
  }

  out.clock = spec.clock === true;

  if (spec.env === true) {
    out.env = new Map();
  } else if (spec.env && typeof spec.env === "object") {
    out.env = new Map(Object.entries(spec.env).map(([k, v]) => [k, String(v)]));
  }

  if (spec.dir) {
    // A single grant object, or an array of them (one per `mint_dir` ordinal).
    const grants = Array.isArray(spec.dir) ? spec.dir : [spec.dir];
    out.dir = grants.map((g) => {
      const grant = g === true ? {} : (g || {});
      return {
        fs: makeMemFs(grant.files),
        read: grant.read !== false, // default: readable
        write: grant.write === true, // default: NOT writable
      };
    });
  }

  if (spec.fetch) {
    const grants = Array.isArray(spec.fetch) ? spec.fetch : [spec.fetch];
    out.fetch = grants.map(normalizeFetchGrant);
  }

  if (spec.secrets) out.secrets = normalizeSecretStore(spec.secrets);
  out.vm = spec.vm === true;

  return out;
}

function normalizeCspHostSource(source) {
  const value = String(source);
  if (value === "'self'") return value;
  try {
    return parseFetchUrl(value, true).origin;
  } catch (error) {
    throw new Error(`witchy-runtime: invalid CSP host connect source: ${error.message}`);
  }
}

/**
 * Derive a page CSP from the same normalized capability object used to build
 * browser host imports. `hostConnect` is for bootstrap I/O such as loading the
 * compiler WASM; every other connect source comes from concrete Fetch grants.
 */
export function deriveContentSecurityPolicy(
  capabilities,
  {
    hostConnect = [],
    scriptSources = ["'self'", "'wasm-unsafe-eval'"],
    styleSources = ["'self'"],
    imageSources = [],
    fontSources = [],
    frameSources = [],
  } = {},
) {
  const caps = normalizeCapabilities(capabilities);
  const connectSources = new Set(hostConnect.map(normalizeCspHostSource));
  for (const grant of caps.fetch || []) {
    for (const origin of grant.origins) connectSources.add(origin);
  }
  const directive = (name, sources) =>
    `${name} ${sources.length === 0 ? "'none'" : [...new Set(sources)].sort().join(" ")}`;
  return [
    "default-src 'none'",
    directive("script-src", scriptSources),
    directive("style-src", styleSources),
    directive("img-src", imageSources),
    directive("media-src", []),
    directive("font-src", fontSources),
    directive("connect-src", [...connectSources]),
    directive("frame-src", frameSources),
    "worker-src 'none'",
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
  ].join("; ");
}

// Assert a family's implemented imports exactly match its frozen catalog — the
// per-family analog of the pure-surface drift check, so no opt-in host silently
// widens beyond its declared surface.
function checkFamilyImports(family, imports, catalog) {
  const actual = Object.keys(imports).sort();
  const expected = [...catalog].sort();
  if (actual.join("\0") !== expected.join("\0")) {
    throw new Error(
      `witchy-runtime: ${family} imports drifted from their catalog\n` +
      `  declared: ${expected.join(", ")}\n` +
      `  actual:   ${actual.join(", ")}`
    );
  }
}

function secretSeed32(bytes) {
  if (bytes.length === 32) return bytes;
  if (bytes.length === 64) {
    const seed = hexToBytes(decodeLossy(bytes));
    if (seed !== null && seed.length === 32) return seed;
    throw new Error("secret is not a valid hex Ed25519 seed");
  }
  throw new Error("secret is not a signing key (need a 32-byte seed or 64 hex chars)");
}

function decodeBase64Url(value) {
  const standard = value.replace(/-/g, "+").replace(/_/g, "/");
  const padded = standard + "=".repeat((4 - (standard.length % 4)) % 4);
  if (typeof globalThis.atob !== "function") {
    throw new Error("witchy-runtime: this platform cannot decode an Ed25519 public key");
  }
  const binary = globalThis.atob(padded);
  return Uint8Array.from(binary, (char) => char.charCodeAt(0));
}

// SecretStore: a page-supplied immutable named map. Lookups return branded
// opaque externrefs; only this closure can unwrap them. Reveal enforces the
// per-entry use-only bit. Ed25519 signing/public-key derivation runs in the
// platform WebCrypto provider through JSPI, so key bytes never enter guest
// memory. This family deliberately excludes `mint_secret`: a bare root Secret
// remains denied even when a SecretStore is granted.
export function makeSecretStoreImports(
  secretSpec,
  { readWstr, readWstrText, writeAt, stagePending },
  subtleCrypto = globalThis.crypto && globalThis.crypto.subtle,
) {
  if (typeof WebAssembly.Suspending !== "function" || typeof WebAssembly.promising !== "function") {
    throw new Error(
      "witchy-runtime: browser SecretStore signing requires WebAssembly JSPI " +
      "(`WebAssembly.Suspending` and `WebAssembly.promising`)",
    );
  }
  if (!subtleCrypto) {
    throw new Error("witchy-runtime: browser SecretStore signing requires WebCrypto");
  }

  const grants = secretSpec instanceof Map ? secretSpec : normalizeSecretStore(secretSpec);
  const handles = new Map();
  const branded = new WeakSet();
  for (const [name, grant] of grants) {
    const handle = { bytes: grant.bytes.slice(), useOnly: grant.useOnly === true };
    branded.add(handle);
    handles.set(name, handle);
  }
  const privateKeys = new WeakMap();
  const requireHandle = (handle) => {
    if (!handle || typeof handle !== "object" || !branded.has(handle)) {
      throw new Error("Secret externref has wrong host data");
    }
    return handle;
  };
  const privateKey = async (handle) => {
    let key = privateKeys.get(handle);
    if (!key) {
      const seed = secretSeed32(handle.bytes);
      const prefix = hexToBytes("302e020100300506032b657004220420");
      const pkcs8 = new Uint8Array(prefix.length + seed.length);
      pkcs8.set(prefix);
      pkcs8.set(seed, prefix.length);
      key = await subtleCrypto.importKey(
        "pkcs8",
        pkcs8,
        { name: "Ed25519" },
        true,
        ["sign"],
      );
      privateKeys.set(handle, key);
    }
    return key;
  };

  const imports = {
    secretstore_lookup(namePtr) {
      return handles.get(readWstrText(namePtr)) || null;
    },
    crypto_reveal_len(secret) {
      const handle = requireHandle(secret);
      if (handle.useOnly) {
        throw new Error("this secret is use-only and cannot be revealed");
      }
      const bytes = utf8.encode(decodeLossy(handle.bytes));
      stagePending(bytes);
      return bytes.length;
    },
    async "crypto.sign"(secret, messagePtr, outPtr) {
      const handle = requireHandle(secret);
      const message = readWstr(messagePtr);
      const signature = await subtleCrypto.sign(
        { name: "Ed25519" },
        await privateKey(handle),
        message,
      );
      writeAt(utf8.encode(toHex(new Uint8Array(signature))), outPtr);
    },
    async "crypto.public_key"(secret, outPtr) {
      const handle = requireHandle(secret);
      const jwk = await subtleCrypto.exportKey("jwk", await privateKey(handle));
      if (typeof jwk.x !== "string") {
        throw new Error("witchy-runtime: WebCrypto did not return an Ed25519 public key");
      }
      const publicKey = decodeBase64Url(jwk.x);
      if (publicKey.length !== 32) {
        throw new Error("witchy-runtime: WebCrypto returned a malformed Ed25519 public key");
      }
      writeAt(utf8.encode(toHex(publicKey)), outPtr);
    },
  };
  checkFamilyImports("SecretStore", imports, WITCHY_SECRET_STORE_IMPORTS);
  imports["crypto.sign"] = new WebAssembly.Suspending(imports["crypto.sign"]);
  imports["crypto.public_key"] = new WebAssembly.Suspending(imports["crypto.public_key"]);
  return imports;
}

function makeVmImports(
  { readI64List, readWstrListRaw, writeAt, stagePending },
  spawnWorker,
) {
  if (typeof WebAssembly.Suspending !== "function" || typeof WebAssembly.promising !== "function") {
    throw new Error(
      "witchy-runtime: browser VM workers require WebAssembly JSPI " +
      "(`WebAssembly.Suspending` and `WebAssembly.promising`)",
    );
  }

  let pendingInts = null;
  let pendingBytes = null;

  const workerMemory = (worker) => {
    const memory = worker.instance.exports.memory;
    if (!(memory instanceof WebAssembly.Memory)) {
      throw new Error("witchy-runtime: VM worker exports no memory");
    }
    return memory;
  };
  const copyIntoWorker = (worker, bytes) => {
    const galloc = worker.instance.exports.__galloc;
    if (typeof galloc !== "function") {
      throw new Error("witchy-runtime: VM worker exports no __galloc");
    }
    const ptr = galloc(4 + bytes.length);
    const memory = workerMemory(worker);
    const view = new DataView(memory.buffer);
    view.setInt32(ptr, bytes.length, true);
    new Uint8Array(memory.buffer).set(bytes, ptr + 4);
    return ptr;
  };
  const readWorkerBytes = (worker, rawPtr) => {
    const ptr = Number(rawPtr);
    const memory = workerMemory(worker);
    const view = new DataView(memory.buffer);
    const len = view.getInt32(ptr, true);
    if (len < 0 || ptr < 0 || ptr + 4 + len > memory.buffer.byteLength) {
      throw new Error("witchy-runtime: VM worker returned an invalid Bytes pointer");
    }
    return new Uint8Array(memory.buffer).slice(ptr + 4, ptr + 4 + len);
  };
  const call1 = async (worker, codeIdx, arg) => {
    const callback = worker.instance.exports.__call_idx;
    if (typeof callback !== "function") {
      throw new Error("witchy-runtime: VM worker exports no __call_idx");
    }
    return WebAssembly.promising(callback)(codeIdx, arg);
  };
  const call2 = async (worker, codeIdx, left, right) => {
    const callback = worker.instance.exports.__call2;
    if (typeof callback !== "function") {
      throw new Error("witchy-runtime: VM worker exports no __call2");
    }
    return WebAssembly.promising(callback)(codeIdx, left, right);
  };
  const writeBytesList = (items, basePtr) => {
    const size = 4 + 8 * items.length
      + items.reduce((total, bytes) => total + 4 + bytes.length, 0);
    const out = new Uint8Array(size);
    const view = new DataView(out.buffer);
    view.setInt32(0, items.length, true);
    let payloadOffset = 4 + 8 * items.length;
    for (let i = 0; i < items.length; i++) {
      view.setBigInt64(4 + 8 * i, BigInt(basePtr + payloadOffset), true);
      view.setInt32(payloadOffset, items[i].length, true);
      out.set(items[i], payloadOffset + 4);
      payloadOffset += 4 + items[i].length;
    }
    writeAt(out, basePtr);
  };

  const imports = {
    async vm_par_map_run(xsPtr, codeIdx) {
      const worker = await spawnWorker();
      const results = [];
      for (const value of readI64List(xsPtr)) {
        results.push(await call1(worker, codeIdx, value));
      }
      pendingInts = results;
      return 4 + 8 * results.length;
    },
    vm_par_map_write(basePtr) {
      if (pendingInts === null) {
        throw new Error("vm_par_map_write called with nothing staged");
      }
      const values = pendingInts;
      pendingInts = null;
      const out = new Uint8Array(4 + 8 * values.length);
      const view = new DataView(out.buffer);
      view.setInt32(0, values.length, true);
      values.forEach((value, index) => view.setBigInt64(4 + 8 * index, value, true));
      writeAt(out, basePtr);
    },
    async vm_par_map_bytes_run(xsPtr, codeIdx) {
      const worker = await spawnWorker();
      const results = [];
      for (const bytes of readWstrListRaw(xsPtr)) {
        const inputPtr = copyIntoWorker(worker, bytes);
        const resultPtr = await call1(worker, codeIdx, BigInt(inputPtr));
        results.push(readWorkerBytes(worker, resultPtr));
      }
      pendingBytes = results;
      return 4 + 8 * results.length
        + results.reduce((total, bytes) => total + 4 + bytes.length, 0);
    },
    vm_par_map_bytes_write(basePtr) {
      if (pendingBytes === null) {
        throw new Error("vm_par_map_bytes_write called with nothing staged");
      }
      const values = pendingBytes;
      pendingBytes = null;
      writeBytesList(values, basePtr);
    },
    async vm_with_dir_run(dir, codeIdx, inputPtr) {
      const worker = await spawnWorker(dir);
      const input = readWstrListRaw(inputPtr, true);
      const workerInput = copyIntoWorker(worker, input);
      const callback = worker.instance.exports.__call_dir_bytes;
      if (typeof callback !== "function") {
        throw new Error("witchy-runtime: VM worker exports no __call_dir_bytes");
      }
      const resultPtr = await WebAssembly.promising(callback)(
        codeIdx,
        dir,
        workerInput,
      );
      const result = readWorkerBytes(worker, resultPtr);
      const staged = new Uint8Array(4 + result.length);
      new DataView(staged.buffer).setInt32(0, result.length, true);
      staged.set(result, 4);
      stagePending(staged);
      return staged.length;
    },
    async vm_serve_run(initPtr, requestsPtr, codeIdx) {
      const worker = await spawnWorker();
      let statePtr = copyIntoWorker(worker, readWstrListRaw(initPtr, true));
      const results = [];
      for (const request of readWstrListRaw(requestsPtr)) {
        const requestPtr = copyIntoWorker(worker, request);
        statePtr = Number(await call2(
          worker,
          codeIdx,
          BigInt(statePtr),
          BigInt(requestPtr),
        ));
        results.push(readWorkerBytes(worker, statePtr));
      }
      pendingBytes = results;
      return 4 + 8 * results.length
        + results.reduce((total, bytes) => total + 4 + bytes.length, 0);
    },
  };
  checkFamilyImports("VM", imports, WITCHY_VM_IMPORTS);
  imports.vm_par_map_run = new WebAssembly.Suspending(imports.vm_par_map_run);
  imports.vm_par_map_bytes_run =
    new WebAssembly.Suspending(imports.vm_par_map_bytes_run);
  imports.vm_with_dir_run = new WebAssembly.Suspending(imports.vm_with_dir_run);
  imports.vm_serve_run = new WebAssembly.Suspending(imports.vm_serve_run);
  return imports;
}

// Clock: real browser wall/monotonic time on the existing i64 ABI. `now` is ms
// since the UNIX epoch (native `SystemTime::now`), `now_monotonic` is a
// nanosecond monotonic count (native `Instant` elapsed nanos). Both are i64, so
// they must return BigInt.
function makeClockImports() {
  const monotonicNs = () => {
    if (typeof performance !== "undefined" && typeof performance.now === "function") {
      return BigInt(Math.round(performance.now() * 1e6));
    }
    return BigInt(Date.now()) * 1000000n;
  };
  const imports = {
    now() { return BigInt(Date.now()); },
    now_monotonic() { return monotonicNs(); },
  };
  checkFamilyImports("Clock", imports, WITCHY_CLOCK_IMPORTS);
  return imports;
}

function makeConsoleImports(input, { stagePending }) {
  const lines = input.slice();
  const imports = {
    console_read_len() {
      const bytes = utf8.encode(lines.length === 0 ? "" : lines.shift());
      stagePending(bytes);
      return bytes.length;
    },
  };
  checkFamilyImports("Console", imports, WITCHY_CONSOLE_IMPORTS);
  return imports;
}

// Env: an immutable page-supplied string map (empty by default). A name present
// in the map reads back its value's UTF-8 byte length; an absent name is UNSET
// (`env_len` -> -1), byte-identical to the native host with no explicit
// allow-list. Values are frozen at instantiation — the guest cannot mutate them.
function makeEnvImports(envMap, { readWstrText, readWstrList, writeAt }) {
  const handles = new WeakSet();
  const makeHandle = (allowed) => {
    const handle = Object.freeze({ allowed });
    handles.add(handle);
    return handle;
  };
  const requireHandle = (handle) => {
    if (!handle || !handles.has(handle)) {
      throw new Error("Env externref has wrong host data");
    }
    return handle;
  };
  const valueBytes = (handle, name) => {
    const { allowed } = requireHandle(handle);
    if (allowed !== null && !allowed.has(name)) {
      throw new Error(`get_env: \`${name}\` is not in this Env grant's allow-list`);
    }
    if (!envMap.has(name)) return null;
    return utf8.encode(envMap.get(name));
  };
  const imports = {
    mint_env() {
      return makeHandle(null);
    },
    env_only(handle, namesPtr) {
      const current = requireHandle(handle).allowed;
      const requested = readWstrList(namesPtr);
      return makeHandle(new Set(
        current === null ? requested : requested.filter((name) => current.has(name)),
      ));
    },
    env_len(handle, namePtr) {
      const bytes = valueBytes(handle, readWstrText(namePtr));
      return bytes === null ? -1 : bytes.length;
    },
    env_fill(handle, namePtr, outPtr) {
      // A well-behaved guest only calls this after a non-negative `env_len`;
      // an absent name writes nothing (native `unwrap_or_default` → empty).
      const bytes = valueBytes(handle, readWstrText(namePtr));
      if (bytes !== null) writeAt(bytes, outPtr);
    },
  };
  checkFamilyImports("Env", imports, WITCHY_ENV_IMPORTS);
  return imports;
}

// Dir: a per-run in-memory tree with native `Dir`/`File` semantics. `grants`
// is the array from `normalizeCapabilities` (one entry per `mint_dir` ordinal).
// Handles are plain JS objects passed to the guest as externref (reference
// types): a Dir handle carries `{ fs, root, policy, read, write }`; a File
// handle `{ fs, path, read, write }`. Every path resolves through the SAME
// lexical `..`/absolute confinement and entry-policy guard the native host
// uses, and NOTHING here touches a real filesystem.
function makeDirImports(grants, { readWstr, readWstrText, stagePending, stageList }) {
  const dirGuard = (dir, name, isDir) => {
    if (!dirAdmits(dir.policy, name, isDir)) {
      throw new Error(`\`${name}\` is not permitted by this Dir capability's entry policy`);
    }
  };
  const requireRead = (dir) => {
    if (!dir.read) throw new Error("this Dir capability does not grant Read");
  };
  const requireWrite = (dir) => {
    if (!dir.write) throw new Error("this Dir capability does not grant Write");
  };

  const imports = {
    // `mint_dir(ordinal) -> externref`: the root Dir handle for grant `ordinal`.
    mint_dir(ordinal) {
      const grant = grants[ordinal];
      if (!grant) throw new Error(`invalid Dir grant index ${ordinal}`);
      if (grant.kind === "dir") return grant;
      return { kind: "dir", fs: grant.fs, root: "", policy: "", read: grant.read, write: grant.write };
    },
    // `dir_subdir(dir, name) -> externref`: open a child directory (a traversal;
    // guarded as a directory entry).
    dir_subdir(dir, namePtr) {
      const name = readWstrText(namePtr);
      requireRead(dir);
      dirGuard(dir, name, true);
      return { kind: "dir", fs: dir.fs, root: mockJoin(dir.root, name), policy: dir.policy, read: dir.read, write: dir.write };
    },
    // `dir_only(dir, policy) -> externref`: narrow the entry policy (never widen).
    dir_only(dir, policyPtr) {
      const refine = readWstrText(policyPtr);
      requireRead(dir);
      return { kind: "dir", fs: dir.fs, root: dir.root, policy: dirOnly(dir.policy, refine), read: dir.read, write: dir.write };
    },
    // `dir_open(dir, rel) -> externref`: a read-only File handle for `rel`.
    dir_open(dir, relPtr) {
      const rel = readWstrText(relPtr);
      requireRead(dir);
      dirGuard(dir, rel, false);
      return { kind: "file", fs: dir.fs, path: mockJoin(dir.root, rel), read: true, write: false };
    },
    // `dir_create(dir, rel) -> externref`: a write-only File handle for `rel`.
    dir_create(dir, relPtr) {
      const rel = readWstrText(relPtr);
      requireWrite(dir);
      dirGuard(dir, rel, false);
      return { kind: "file", fs: dir.fs, path: mockJoin(dir.root, rel), read: false, write: true };
    },
    // `dir_read_len(dir, rel) -> i32`: stage the file's bytes for `fill_pending`.
    dir_read_len(dir, relPtr) {
      const rel = readWstrText(relPtr);
      requireRead(dir);
      dirGuard(dir, rel, false);
      const bytes = memRead(dir.fs, mockJoin(dir.root, rel));
      stagePending(bytes);
      return bytes.length;
    },
    // `dir_list_size(dir) -> i32`: stage the sorted child names for
    // `write_pending_list`; return the exact byte size the guest must reserve
    // (native layout: i32 count + count·i64 ptrs + each `[i32 len][bytes]`).
    dir_list_size(dir) {
      requireRead(dir);
      const names = memList(dir.fs, dir.root);
      stageList(names);
      let size = 4 + 8 * names.length;
      for (const n of names) size += 4 + utf8.encode(n).length;
      return size;
    },
    dir_exists(dir, relPtr) {
      const rel = readWstrText(relPtr);
      requireRead(dir);
      let path;
      try { path = mockJoin(dir.root, rel); } catch (_e) { return 0; }
      return memExists(dir.fs, path) ? 1 : 0;
    },
    dir_is_dir(dir, relPtr) {
      const rel = readWstrText(relPtr);
      requireRead(dir);
      let path;
      try { path = mockJoin(dir.root, rel); } catch (_e) { return 0; }
      return memIsDir(dir.fs, path) ? 1 : 0;
    },
    dir_write(dir, relPtr, valPtr) {
      const rel = readWstrText(relPtr);
      requireWrite(dir);
      dirGuard(dir, rel, false);
      memWrite(dir.fs, mockJoin(dir.root, rel), readWstr(valPtr));
    },
    dir_append(dir, relPtr, valPtr) {
      const rel = readWstrText(relPtr);
      requireWrite(dir);
      dirGuard(dir, rel, false);
      memAppend(dir.fs, mockJoin(dir.root, rel), readWstr(valPtr));
    },
    dir_make_dir(dir, namePtr) {
      const name = readWstrText(namePtr);
      requireWrite(dir);
      dirGuard(dir, name, true);
      memMakeDir(dir.fs, mockJoin(dir.root, name));
    },
    // File handles minted by dir_open/dir_create.
    file_read_len(file) {
      if (!file.read) throw new Error("this File capability does not grant Read");
      const bytes = memRead(file.fs, file.path);
      stagePending(bytes);
      return bytes.length;
    },
    file_write(file, valPtr) {
      if (!file.write) throw new Error("this File capability does not grant Write");
      memWrite(file.fs, file.path, readWstr(valPtr));
    },
  };
  checkFamilyImports("Dir", imports, WITCHY_DIR_IMPORTS);
  return imports;
}

function parseFetchHeaders(text) {
  const headers = [];
  for (let line of text.split("\n")) {
    if (line.endsWith("\r")) line = line.slice(0, -1);
    const colon = line.indexOf(":");
    if (colon < 0) continue;
    const name = line.slice(0, colon).trim();
    const value = line.slice(colon + 1).trim();
    if (name === "" || !/^[A-Za-z0-9!#$%&'*+\-.^_`|~]+$/.test(name)) {
      throw invalidFetch(`header name \`${name}\` is not an HTTP token`);
    }
    if (/[\u0000-\u0008\u000b-\u001f\u007f\r\n]/.test(value)) {
      throw invalidFetch(`header \`${name}\` contains a forbidden control character`);
    }
    if (["host", "connection", "content-length", "transfer-encoding"].includes(name.toLowerCase())) {
      throw invalidFetch(`header \`${name}\` is controlled by the Fetch provider`);
    }
    headers.push([name, value]);
  }
  return headers;
}

async function readBoundedFetchBody(response, limit) {
  if (response.body && typeof response.body.getReader === "function") {
    const reader = response.body.getReader();
    const chunks = [];
    let length = 0;
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      const bytes = new Uint8Array(value);
      length += bytes.length;
      if (length > limit) {
        await reader.cancel();
        throw fetchFailure(
          "response-too-large",
          `Fetch response exceeds the ${limit}-byte host limit`,
        );
      }
      chunks.push(bytes);
    }
    const body = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      body.set(chunk, offset);
      offset += chunk.length;
    }
    return body;
  }
  const body = new Uint8Array(await response.arrayBuffer());
  if (body.length > limit) {
    throw fetchFailure(
      "response-too-large",
      `Fetch response exceeds the ${limit}-byte host limit`,
    );
  }
  return body;
}

function makeFetchImports(grants, { readWstrText, stagePending }, fetchImpl) {
  if (typeof WebAssembly.Suspending !== "function" || typeof WebAssembly.promising !== "function") {
    throw new Error(
      "witchy-runtime: browser Fetch requires WebAssembly JSPI " +
      "(`WebAssembly.Suspending` and `WebAssembly.promising`)",
    );
  }
  const platformFetch = fetchImpl || globalThis.fetch;
  if (typeof platformFetch !== "function") {
    throw new Error("witchy-runtime: browser Fetch requires a `fetch()` implementation");
  }

  const imports = {
    mint_fetch(ordinal) {
      const grant = grants[ordinal];
      if (!grant) throw new Error(`invalid Fetch grant index ${ordinal}`);
      return {
        kind: "fetch",
        origins: new Set(grant.origins),
        timeoutMs: grant.timeoutMs,
        maxResponseBytes: grant.maxResponseBytes,
      };
    },
    fetch_only(fetch, originsPtr) {
      const text = readWstrText(originsPtr);
      const requested = text === "" ? [] : text.split(/\r?\n/);
      const origins = new Set();
      for (const value of requested) {
        let origin;
        try {
          origin = parseFetchUrl(value, true).origin;
        } catch (error) {
          throw new Error(`fetch.only: ${error.message}`);
        }
        if (!fetch.origins.has(origin)) {
          throw new Error(`fetch.only: Fetch origin \`${origin}\` is not granted`);
        }
        origins.add(origin);
      }
      return {
        kind: "fetch",
        origins,
        timeoutMs: fetch.timeoutMs,
        maxResponseBytes: fetch.maxResponseBytes,
      };
    },
    async fetch_send_len(fetch, methodPtr, urlPtr, headersPtr, bodyPtr) {
      let failure = null;
      let payload = null;
      const method = readWstrText(methodPtr);
      const url = readWstrText(urlPtr);
      const headersText = readWstrText(headersPtr);
      const bodyText = readWstrText(bodyPtr);
      try {
        if (method === "" || !/^[A-Za-z0-9!#$%&'*+\-.^_`|~]+$/.test(method)) {
          throw invalidFetch("method is not an HTTP token");
        }
        const parsed = parseFetchUrl(url);
        const headers = parseFetchHeaders(headersText);
        if (!fetch.origins.has(parsed.origin)) {
          throw fetchFailure(
            "denied",
            `Fetch origin \`${parsed.origin}\` is not granted`,
          );
        }

        const controller = new AbortController();
        let timedOut = false;
        const timer = setTimeout(() => {
          timedOut = true;
          controller.abort();
        }, fetch.timeoutMs);
        let response;
        let body;
        try {
          response = await platformFetch(parsed.url, {
            method,
            headers,
            body: bodyText === "" ? undefined : bodyText,
            credentials: "omit",
            redirect: "manual",
            signal: controller.signal,
          });
          if (
            response.redirected ||
            response.type === "opaqueredirect" ||
            (response.status >= 300 && response.status < 400)
          ) {
            throw fetchFailure(
              "redirect",
              `Fetch redirects are disabled (HTTP status ${response.status})`,
            );
          }
          body = await readBoundedFetchBody(response, fetch.maxResponseBytes);
        } catch (error) {
          if (timedOut || error?.name === "AbortError") {
            throw fetchFailure("timeout", "Fetch request timed out");
          }
          throw error;
        } finally {
          clearTimeout(timer);
        }

        let raw = `HTTP/1.1 ${response.status}\r\n`;
        for (const [name, value] of response.headers) {
          raw += `${name}: ${value}\r\n`;
        }
        raw += `\r\n${decodeLossy(body)}`;
        payload = raw;
      } catch (error) {
        failure =
          error && typeof error.code === "string" && typeof error.message === "string"
            ? error
            : fetchFailure(
                "network",
                `Fetch network error: ${error?.message || String(error)}`,
              );
      }
      if (failure) payload = `WITCHY_FETCH_ERROR:${failure.code}:${failure.message}`;
      const bytes = utf8.encode(payload);
      stagePending(bytes);
      return bytes.length;
    },
  };
  checkFamilyImports("Fetch", imports, WITCHY_FETCH_IMPORTS);
  imports.fetch_send_len = new WebAssembly.Suspending(imports.fetch_send_len);
  return imports;
}

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
 * @param {SubtleCrypto} [opts.subtleCrypto]  WebCrypto override for SecretStore
 * @param {(instance: WebAssembly.Instance) => void} [opts.onVmSpawn]
 *        test/telemetry hook called for each fresh sequential worker instance
 * @param {Function} [opts.fetchImpl]  injected fetch implementation (testing);
 *        defaults to the platform's global fetch
 */
export async function instantiate(wasmBytes, opts = {}) {
  const output = [];
  const onPrint = opts.onPrint || ((line) => output.push(line));
  const args = Array.isArray(opts.args) ? opts.args.map(String) : [];
  const nodeCrypto = opts.nodeCrypto !== undefined ? opts.nodeCrypto : await defaultNodeCrypto();
  const crypto = opts.cryptoBackend || makeCryptoBackend(nodeCrypto);
  const module = wasmBytes instanceof WebAssembly.Module
    ? wasmBytes
    : await WebAssembly.compile(wasmBytes);

  // Bound late: instance memory is set after instantiation, but the import
  // closures capture `mem` by reference through this object.
  let memory = null;
  let instance = null;
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
  const readWstrListRaw = (ptr, single = false) => {
    if (single) return readWstr(ptr);
    const count = dv().getInt32(ptr, true);
    const out = [];
    for (let i = 0; i < count; i++) {
      out.push(readWstr(Number(dv().getBigInt64(ptr + 4 + 8 * i, true))));
    }
    return out;
  };
  const readI64List = (ptr) => {
    const count = dv().getInt32(ptr, true);
    const out = [];
    for (let i = 0; i < count; i++) {
      out.push(dv().getBigInt64(ptr + 4 + 8 * i, true));
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
  // The pending List(String): staged by `dir_list_size`, drained by
  // `write_pending_list`. Only the opt-in Dir host stages one; the pure host
  // leaves it null (its `write_pending_list` is a no-op). Mirrors
  // `VmState::pending_list`.
  let pendingList = null;

  // The witchy import object — PURE functions only. No `dir_*`, `net_*`,
  // `exec_run`, `now`, `env_*`, `secretstore_*`, `crypto_reveal_*`,
  // `build_*`, `compiler_*`, `crypto.sign`, `crypto.public_key`: those are
  // capabilities (or interpreter-only host services) and are DELIBERATELY ABSENT,
  // so a module importing one cannot instantiate here.
  // (RFC-0045) Render a runtime-abort message from a `DiagTemplate` id + its
  // holes — a byte-for-byte mirror of `DiagTemplate::render` in
  // crates/witchy-syntax/src/diag.rs. The compiled-abort matrix pins every pure
  // template against complete expected messages. The template ids are the
  // compiled ABI (`DiagTemplate::id`); do not renumber. `a`/`b` are i64 holes
  // (BigInt), `s` the string hole.
  const renderDiag = (template, a, b, s) => {
    switch (template) {
      case 1: return `list index ${a} out of bounds (length ${b})`;
      case 2: return `bytes index ${a} out of bounds (length ${b})`;
      case 3: return `cannot parse \`${s}\` as an Int`;
      case 4: return "cannot compare NaN";
      case 5: return s;
      case 6: return `required secret \`${s}\` was not granted`;
      case 7: return "math.to_int: NaN cannot be converted to Int";
      case 8: return "dict.at: missing key";
      case 9: return "division by zero";
      case 10: return "integer overflow in `/`";
      case 11: return "modulo by zero";
      default: return `abort with unknown diagnostic template id ${template}`;
    }
  };

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

    // --- (RFC-0045) the always-present, authority-free abort channel ---
    // `__witchy_abort(template, a, b, str_ptr)`: render the shared DiagTemplate
    // and throw a JS Error whose `.message` is the same location-prefixed
    // `runtime error` string the native host produces, so an abort surfaces
    // identically here. It grants NO authority (it reads only the
    // string it is handed, returns nothing, and only terminates execution — an
    // ability the guest already has via `unreachable`), so, like `print`, the
    // pure/deny-by-omission host may provide it; it MUST be present or every
    // footprint-empty module that can abort would fail to instantiate.
    __witchy_abort(template, a, b, strPtr) {
      const s = strPtr !== 0 ? readWstrText(strPtr) : "";
      const core = renderDiag(Number(template), a, b, s);
      const site = BigInt(instance?.exports.__witchy_diagnostic_site?.value || 0n);
      const funcPtr = Number((site >> 32n) & 0xffffffffn);
      const line = Number(site & 0xffffffffn);
      const func = funcPtr !== 0 ? readWstrText(funcPtr) : "";
      const location = line > 0 ? (func ? `\`${func}\`, line ${line}: ` : `line ${line}: `) : "";
      throw new Error(`runtime error: ${location}${core}`);
    },

    // --- the pending-buffer string-bridge (pure mechanics, no authority) ---
    fill_pending(outPtr) {
      if (pending === null) throw new Error("witchy-runtime: fill_pending called with nothing staged");
      const bytes = pending;
      pending = null;
      writeAt(bytes, outPtr);
    },
    // argv is host-chosen launch input, not authority. Stage an immutable copy
    // for the same two-call List(String) transfer used by the native runtime.
    args_size() {
      pendingList = args.slice();
      let size = 4 + 8 * pendingList.length;
      for (const arg of pendingList) size += 4 + utf8.encode(arg).length;
      return size;
    },
    // write_pending_list lays a staged List(String) out at base_ptr. No PURE
    // host op stages a list (args_size/dir_list_size are capabilities), so under
    // the default deny-by-omission host `pendingList` is always null and this is
    // a no-op — but the import must exist because a footprint-empty module that
    // builds a list literal MAY import it. The opt-in Dir host's `dir_list_size`
    // DOES stage a list; when one is staged we lay it out in the native
    // `host_write_pending_list` layout: an i32 count, `count` i64 element
    // pointers, then each String `[i32 len][bytes]` packed after the pointer
    // table (read once, then cleared).
    write_pending_list(basePtr) {
      if (pendingList === null) return;
      const names = pendingList;
      pendingList = null;
      const encoded = names.map((s) => utf8.encode(s));
      const n = names.length;
      const stringsStart = basePtr + 4 + 8 * n;
      const view = dv();
      const bytes = u8();
      view.setInt32(basePtr, n, true);
      let offset = 0;
      for (let i = 0; i < n; i++) {
        const ptr = stringsStart + offset;
        // i64 little-endian pointer (guest pointers fit in 32 bits).
        view.setUint32(basePtr + 4 + 8 * i, ptr >>> 0, true);
        view.setUint32(basePtr + 4 + 8 * i + 4, 0, true);
        offset += 4 + encoded[i].length;
      }
      let cursor = stringsStart;
      for (const enc of encoded) {
        view.setInt32(cursor, enc.length, true);
        bytes.set(enc, cursor + 4);
        cursor += 4 + enc.length;
      }
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
      return writeAt(encodingOp(op, readWstr(inPtr)), outPtr);
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
      // Malformed hex key is a hard error, matching native crypto::hmac_sha256
      // (`hex_decode(...).ok_or_else(...)`) — never a silently mangled key (BUG-276).
      if (key === null) throw new Error("crypto.hmac_sha256: key is not valid hex");
      writeAt(utf8.encode(toHex(crypto.hmacSha256(key, readWstr(msgPtr)))), outPtr);
    },
    "crypto.rune_hash"(pathsPtr, contentsPtr, outPtr) {
      const hash = runeHash(readWstrList(pathsPtr), readWstrList(contentsPtr));
      writeAt(utf8.encode(hash), outPtr);
    },
    "crypto.__ed25519_verify_status"(pkPtr, msgPtr, sigPtr) {
      const pk = hexToBytes(readWstrText(pkPtr));
      const sig = hexToBytes(readWstrText(sigPtr));
      if (pk === null || pk.length !== 32) return -1n;
      if (sig === null || sig.length !== 64) return -3n;
      return crypto.ed25519Verify(pk, readWstr(msgPtr), sig) ? 1n : 0n;
    },
    "crypto.__ecdsa_p256_verify_status"(_pk, _msg, _sig) {
      return -4n;
    },
    "crypto.__ecdsa_p256_verify_hex_status"(_pk, _msg, _sig) {
      return -4n;
    },
    "crypto.__rsa_pkcs1_sha256_verify_status"(_pk, _msg, _sig) {
      return -4n;
    },

    // --- reflection field-length stubs (pure reads; ordinary programs never
    // reach them — they are an interpreter host detail; return 0, matching the
    // harmless stubs in src/runtime.rs) ---
    field_str_len(_h) { return 0; },
    field_intlist_len(_h) { return 0; },
    field_strlist_size(_h) { return 0; },
  };

  const actualHostImports = Object.keys(witchy).sort();
  if (actualHostImports.join("\0") !== WITCHY_BROWSER_IMPORTS.join("\0")) {
    throw new Error(
      `witchy-runtime: implemented imports drifted from WITCHY_BROWSER_IMPORTS\n` +
      `  declared: ${WITCHY_BROWSER_IMPORTS.join(", ")}\n` +
      `  actual:   ${actualHostImports.join(", ")}`
    );
  }

  // (RFC-0091) Merge in the EXPLICITLY-requested capability families. The
  // default (no `opts.capabilities`) leaves `witchy` exactly the pure surface
  // above — deny-by-omission is preserved. Each family's imports come from a
  // helper that also drift-checks against its frozen catalog, so an opt-in host
  // can never silently widen either.
  const caps = normalizeCapabilities(opts.capabilities);
  if (Array.isArray(opts._vmGrantedDirs)) {
    caps.dir = opts._vmGrantedDirs;
  }
  const marshal = {
    readWstr,
    readWstrText,
    readWstrList,
    readWstrListRaw,
    readI64List,
    writeAt,
    stagePending: (bytes) => { pending = bytes; },
    stageList: (names) => { pendingList = names; },
  };
  if (caps.console) Object.assign(witchy, makeConsoleImports(caps.console, marshal));
  if (caps.clock) Object.assign(witchy, makeClockImports());
  if (caps.env) Object.assign(witchy, makeEnvImports(caps.env, marshal));
  if (caps.dir) Object.assign(witchy, makeDirImports(caps.dir, marshal));
  if (caps.fetch) {
    Object.assign(witchy, makeFetchImports(caps.fetch, marshal, opts.fetchImpl));
  }
  if (caps.secrets) {
    const subtleCrypto =
      opts.subtleCrypto
      || (globalThis.crypto && globalThis.crypto.subtle)
      || (nodeCrypto && nodeCrypto.webcrypto && nodeCrypto.webcrypto.subtle);
    Object.assign(
      witchy,
      makeSecretStoreImports(caps.secrets, marshal, subtleCrypto),
    );
  }
  if (caps.vm) {
    const spawnWorker = async (dir) => {
      const worker = await instantiate(module, {
        cryptoBackend: crypto,
        nodeCrypto,
        subtleCrypto: opts.subtleCrypto,
        capabilities: { vm: true },
        _vmGrantedDirs: dir === undefined ? undefined : [dir],
        _trapUnknownImports: true,
      });
      if (typeof opts.onVmSpawn === "function") opts.onVmSpawn(worker.instance);
      return worker;
    };
    Object.assign(witchy, makeVmImports(marshal, spawnWorker));
  }

  if (opts._trapUnknownImports === true) {
    for (const imported of WebAssembly.Module.imports(module)) {
      if (
        imported.module === "witchy"
        && imported.kind === "function"
        && !Object.prototype.hasOwnProperty.call(witchy, imported.name)
      ) {
        witchy[imported.name] = () => {
          throw new Error(
            `VM worker has no authority for witchy::${imported.name}`,
          );
        };
      }
    }
  }

  instance = new WebAssembly.Instance(module, { witchy });
  memory = instance.exports.memory;

  const run = () => {
    if (typeof instance.exports.run !== "function") {
      throw new Error("witchy-runtime: the module does not export `run`");
    }
    if (caps.fetch || caps.secrets || caps.vm) {
      return WebAssembly.promising(instance.exports.run)().then(() => output);
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
    const finish = (outPtr) => {
      const outLen = dv().getInt32(outPtr, true);
      const result = decodeLossy(u8().slice(outPtr + 4, outPtr + 4 + outLen));
      // Free everything this call allocated: the result is now a detached JS string.
      if (heapGlobal != null) heapGlobal.value = heapBase;
      return result;
    };
    if (caps.fetch || caps.secrets || caps.vm) {
      return WebAssembly.promising(fn)(inPtr, bytes.length).then(finish);
    }
    return finish(fn(inPtr, bytes.length));
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
