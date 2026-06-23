"use strict";
(() => {
  var __create = Object.create;
  var __defProp = Object.defineProperty;
  var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
  var __getOwnPropNames = Object.getOwnPropertyNames;
  var __getProtoOf = Object.getPrototypeOf;
  var __hasOwnProp = Object.prototype.hasOwnProperty;
  var __require = /* @__PURE__ */ ((x) => typeof require !== "undefined" ? require : typeof Proxy !== "undefined" ? new Proxy(x, {
    get: (a, b) => (typeof require !== "undefined" ? require : a)[b]
  }) : x)(function(x) {
    if (typeof require !== "undefined") return require.apply(this, arguments);
    throw Error('Dynamic require of "' + x + '" is not supported');
  });
  var __copyProps = (to, from, except, desc) => {
    if (from && typeof from === "object" || typeof from === "function") {
      for (let key of __getOwnPropNames(from))
        if (!__hasOwnProp.call(to, key) && key !== except)
          __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
    }
    return to;
  };
  var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
    // If the importer is in node compatibility mode or this is not an ESM
    // file that has been converted to a CommonJS file using a Babel-
    // compatible transform (i.e. "__esModule" has not been set), then set
    // "default" to the CommonJS "module.exports" for node compatibility.
    isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
    mod
  ));

  // ../../../web/witchy-runtime/witchy-runtime.mjs
  var K256 = new Uint32Array([
    1116352408,
    1899447441,
    3049323471,
    3921009573,
    961987163,
    1508970993,
    2453635748,
    2870763221,
    3624381080,
    310598401,
    607225278,
    1426881987,
    1925078388,
    2162078206,
    2614888103,
    3248222580,
    3835390401,
    4022224774,
    264347078,
    604807628,
    770255983,
    1249150122,
    1555081692,
    1996064986,
    2554220882,
    2821834349,
    2952996808,
    3210313671,
    3336571891,
    3584528711,
    113926993,
    338241895,
    666307205,
    773529912,
    1294757372,
    1396182291,
    1695183700,
    1986661051,
    2177026350,
    2456956037,
    2730485921,
    2820302411,
    3259730800,
    3345764771,
    3516065817,
    3600352804,
    4094571909,
    275423344,
    430227734,
    506948616,
    659060556,
    883997877,
    958139571,
    1322822218,
    1537002063,
    1747873779,
    1955562222,
    2024104815,
    2227730452,
    2361852424,
    2428436474,
    2756734187,
    3204031479,
    3329325298
  ]);
  function rotr(x, n) {
    return x >>> n | x << 32 - n;
  }
  function sha256Bytes(msg) {
    let h0 = 1779033703, h1 = 3144134277, h2 = 1013904242, h3 = 2773480762;
    let h4 = 1359893119, h5 = 2600822924, h6 = 528734635, h7 = 1541459225;
    const bitLen = msg.length * 8;
    const withOne = msg.length + 1;
    const k = (56 - withOne % 64 + 64) % 64;
    const total = withOne + k + 8;
    const buf = new Uint8Array(total);
    buf.set(msg, 0);
    buf[msg.length] = 128;
    const hi = Math.floor(bitLen / 4294967296);
    const lo = bitLen >>> 0;
    const dv = new DataView(buf.buffer);
    dv.setUint32(total - 8, hi);
    dv.setUint32(total - 4, lo);
    const w = new Uint32Array(64);
    for (let off = 0; off < total; off += 64) {
      for (let i = 0; i < 16; i++) w[i] = dv.getUint32(off + i * 4);
      for (let i = 16; i < 64; i++) {
        const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ w[i - 15] >>> 3;
        const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ w[i - 2] >>> 10;
        w[i] = w[i - 16] + s0 + w[i - 7] + s1 | 0;
      }
      let a = h0, b = h1, c = h2, d = h3, e = h4, f = h5, g = h6, h = h7;
      for (let i = 0; i < 64; i++) {
        const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
        const ch = e & f ^ ~e & g;
        const t1 = h + S1 + ch + K256[i] + w[i] | 0;
        const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
        const maj = a & b ^ a & c ^ b & c;
        const t2 = S0 + maj | 0;
        h = g;
        g = f;
        f = e;
        e = d + t1 | 0;
        d = c;
        c = b;
        b = a;
        a = t1 + t2 | 0;
      }
      h0 = h0 + a | 0;
      h1 = h1 + b | 0;
      h2 = h2 + c | 0;
      h3 = h3 + d | 0;
      h4 = h4 + e | 0;
      h5 = h5 + f | 0;
      h6 = h6 + g | 0;
      h7 = h7 + h | 0;
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
  function hmacSha256Bytes(key, msg) {
    const block = 64;
    let k = key;
    if (k.length > block) k = sha256Bytes(k);
    const k0 = new Uint8Array(block);
    k0.set(k);
    const ipad = new Uint8Array(block + msg.length);
    const opad = new Uint8Array(block + 32);
    for (let i = 0; i < block; i++) {
      ipad[i] = k0[i] ^ 54;
      opad[i] = k0[i] ^ 92;
    }
    ipad.set(msg, block);
    const inner = sha256Bytes(ipad);
    opad.set(inner, block);
    return sha256Bytes(opad);
  }
  async function defaultNodeCrypto() {
    try {
      if (typeof process !== "undefined" && process.versions && process.versions.node) {
        const mod = await import("node:crypto");
        return mod.default || mod;
      }
    } catch (_e) {
    }
    return null;
  }
  function makeCryptoBackend(nodeCrypto) {
    const need = (name) => {
      if (!nodeCrypto) {
        throw new Error(
          `witchy-runtime: '${name}' needs a platform crypto backend (Node's node:crypto); it is unavailable in this environment (only sha256/hmac_sha256/rune_hash work without it)`
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
              Buffer.from("302a300506032b6570032100", "hex"),
              // SPKI Ed25519 prefix
              Buffer.from(pk)
            ]),
            format: "der",
            type: "spki"
          });
          return c.verify(null, Buffer.from(msg), key, Buffer.from(sig));
        } catch (_e) {
          return false;
        }
      }
    };
  }
  var utf8 = new TextEncoder();
  var decodeLossy = (bytes) => new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  function renderFloat(x) {
    if (Number.isFinite(x) && Math.floor(x) === x && !Object.is(x, -0)) {
      return x.toFixed(1);
    }
    if (Object.is(x, -0)) return "-0.0";
    return String(x);
  }
  function stringFromCode(cp) {
    const n = Number(cp);
    let ch;
    if (!Number.isInteger(n) || n < 0 || n > 1114111 || n >= 55296 && n <= 57343) {
      ch = "\uFFFD";
    } else {
      ch = String.fromCodePoint(n);
    }
    return utf8.encode(ch);
  }
  var B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  var B64URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
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
  function encodingOp(op, input) {
    switch (op) {
      case 0: {
        return toHex(input);
      }
      case 1: {
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
      case 2: {
        let out = "";
        for (let i = 0; i < input.length; i += 3) {
          const b0 = input[i], b1 = input[i + 1] || 0, b2 = input[i + 2] || 0;
          const len = input.length - i;
          const n = b0 << 16 | b1 << 8 | b2;
          out += B64[n >> 18 & 63] + B64[n >> 12 & 63];
          out += len > 1 ? B64[n >> 6 & 63] : "=";
          out += len > 2 ? B64[n & 63] : "=";
        }
        return out;
      }
      case 3: {
        const text = decodeLossy(input);
        let acc = 0, nbits = 0;
        const bytes = [];
        for (const ch of text) {
          const c = ch;
          if (c === "=" || /\s/.test(c)) continue;
          const v = B64.indexOf(c);
          if (v < 0) break;
          acc = acc << 6 | v;
          nbits += 6;
          if (nbits >= 8) {
            nbits -= 8;
            bytes.push(acc >> nbits & 255);
          }
        }
        return decodeLossy(new Uint8Array(bytes));
      }
      case 4: {
        const bytes = hexToBytes(decodeLossy(input));
        let out = "";
        for (let i = 0; i < bytes.length; i += 3) {
          const b0 = bytes[i], b1 = bytes[i + 1] || 0, b2 = bytes[i + 2] || 0;
          const len = bytes.length - i;
          const n = b0 << 16 | b1 << 8 | b2;
          out += B64URL[n >> 18 & 63] + B64URL[n >> 12 & 63];
          if (len > 1) out += B64URL[n >> 6 & 63];
          if (len > 2) out += B64URL[n & 63];
        }
        return out;
      }
      default:
        throw new Error(`witchy-runtime: unknown encoding op ${op}`);
    }
  }
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
    const charOff = (idx) => [...text.slice(0, idx)].length;
    const parts = [];
    let m;
    while ((m = re.exec(text)) !== null) {
      const s = charOff(m.index);
      const e = charOff(m.index + m[0].length);
      parts.push(`${s},${e}`);
      if (m[0].length === 0) re.lastIndex++;
    }
    return parts.join(";");
  }
  function runeHash(paths, contents) {
    if (paths.length !== contents.length) {
      throw new Error("witchy-runtime: crypto.rune_hash: paths and contents differ in length");
    }
    const normalizeLf = (bytes) => {
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
    files.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
    const chunks = [];
    const u64le = (n) => {
      const b = new Uint8Array(8);
      const dv = new DataView(b.buffer);
      dv.setUint32(0, n >>> 0, true);
      dv.setUint32(4, Math.floor(n / 4294967296), true);
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
    for (const c of chunks) {
      buf.set(c, off);
      off += c.length;
    }
    return "sha256:" + toHex(sha256Bytes(buf));
  }
  async function instantiate(wasmBytes, opts = {}) {
    const output = [];
    const onPrint = opts.onPrint || ((line) => output.push(line));
    const nodeCrypto = opts.nodeCrypto !== void 0 ? opts.nodeCrypto : await defaultNodeCrypto();
    const crypto = opts.cryptoBackend || makeCryptoBackend(nodeCrypto);
    let memory = null;
    const u8 = () => new Uint8Array(memory.buffer);
    const dv = () => new DataView(memory.buffer);
    const readWstr = (ptr) => {
      const len = dv().getInt32(ptr, true);
      return u8().slice(ptr + 4, ptr + 4 + len);
    };
    const readWstrText = (ptr) => decodeLossy(readWstr(ptr));
    const readWstrList = (ptr) => {
      const count = dv().getInt32(ptr, true);
      const out = [];
      for (let i = 0; i < count; i++) {
        const lo = dv().getUint32(ptr + 4 + 8 * i, true);
        const hi = dv().getUint32(ptr + 4 + 8 * i + 4, true);
        const elem = hi * 4294967296 + lo;
        out.push(readWstrText(elem));
      }
      return out;
    };
    const writeAt = (bytes, outPtr) => {
      u8().set(bytes, outPtr);
      return bytes.length;
    };
    let pending = null;
    const witchy = {
      // --- output (capturable; output is not authority) ---
      print(ptr, len) {
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
      field_str_len(_h) {
        return 0;
      },
      field_intlist_len(_h) {
        return 0;
      },
      field_strlist_size(_h) {
        return 0;
      }
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
      dv().setInt32(inPtr, bytes.length, true);
      u8().set(bytes, inPtr + 4);
      const outPtr = fn(inPtr, bytes.length);
      const outLen = dv().getInt32(outPtr, true);
      return decodeLossy(u8().slice(outPtr + 4, outPtr + 4 + outLen));
    };
    return {
      instance,
      output,
      run,
      callString,
      get memory() {
        return memory;
      }
    };
  }

  // sandbox-src/source-sandbox.js
  var HIGHLIGHTER_WASM_B64 = "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GAlzdHJfY2hhcnMZB3N0cl9jbXAaBnN0cl9lcRsMc3RyX2luZGV4X29mHA1zdHJfc3Vic3RyaW5nHQpzdHJfdG9faW50HhBzdHJpbmdfZnJvbV9jb2RlHwZzdWJzdHIgB19fbGFtdzAhF2hpZ2hsaWdodGVyLm1zZ190b19qc29uIhZoaWdobGlnaHRlci5pc19rZXl3b3JkIxRoaWdobGlnaHRlci5rZXl3b3JkcyQZaGlnaGxpZ2h0ZXIuaXNfY2FwYWJpbGl0eSUYaGlnaGxpZ2h0ZXIuY2FwYWJpbGl0aWVzJhRoaWdobGlnaHRlci50b2tlbml6ZScaaGlnaGxpZ2h0ZXIuaXNfc2xhc2hfc2xhc2goHWhpZ2hsaWdodGVyLnJlYWRfbGluZV9jb21tZW50KRdoaWdobGlnaHRlci5yZWFkX3N0cmluZyoXaGlnaGxpZ2h0ZXIucmVhZF9udW1iZXIrFmhpZ2hsaWdodGVyLnJlYWRfaWRlbnQsFmhpZ2hsaWdodGVyLnJlYWRfcGxhaW4tFGhpZ2hsaWdodGVyLmlzX2RpZ2l0LhpoaWdobGlnaHRlci5pc19udW1iZXJfY2hhci8aaGlnaGxpZ2h0ZXIuaXNfaWRlbnRfc3RhcnQwGWhpZ2hsaWdodGVyLmlzX2lkZW50X2NvbnQxFGhpZ2hsaWdodGVyLmlzX2FscGhhMhVoaWdobGlnaHRlci5oaWdobGlnaHQzGGhpZ2hsaWdodGVyLnJlbmRlcl90b2tlbjQZaGlnaGxpZ2h0ZXIuZXhwb3J0X3JlbmRlcjUEbWFpbjYRZ2xhbW91ci5ub2RlX2pzb243C2pzb24uZW5jb2RlOBFqc29uLmVuY29kZV9hcnJheTkSanNvbi5lbmNvZGVfb2JqZWN0OhJqc29uLmVuY29kZV9zdHJpbmc7EGpzb24uZXNjYXBlX2NoYXI8C2pzb24uZGVjb2RlPQxqc29uLnNraXBfd3M+EGpzb24ucGFyc2VfdmFsdWU/Dmpzb24ucGFyc2VfbGl0QA1qc29uLmlzX2RpZ2l0QRRqc29uLmRpZ2l0c190b19mbG9hdEIRanNvbi5wYXJzZV9udW1iZXJDFWpzb24ucGFyc2VfZmxvYXRfdGFpbEQKanNvbi5wb3cxMEUSanNvbi5pc19mbG9hdF90YWlsRhFqc29uLnBhcnNlX3N0cmluZ0cQanNvbi5zY2FuX3N0cmluZ0gNanNvbi51bmVzY2FwZUkManNvbi5oZXhfdmFsSglqc29uLmhleDRLEWpzb24uc2Nhbl91bmljb2RlTBBqc29uLnBhcnNlX2FycmF5TRFqc29uLnBhcnNlX29iamVjdE4IanNvbi5nZXRPDWpzb24uZmluZF9rZXlQDmpzb24uYXNfc3RyaW5nUQ9qc29uLmdldF9zdHJpbmdSEHN0cmluZy5mcm9tX2NvZGVTDnN0cmluZy5jaGFyX2F0VA9zdHJpbmcuaXNfZW1wdHlVEHN0cmluZy5wYXJzZV9pbnRWFWxpc3QuY29udGFpbnNfX1N0cmluZ1cUZ2xhbW91ci5lbGVtZW50X19Nc2dYEWdsYW1vdXIudGV4dF9fTXNnWRFnbGFtb3VyLnByb3BfX01zZ1oUZ2xhbW91ci50b19qc29uX19Nc2dbA3J1blwIX19nYWxsb2NdFl9fZXhwb3J0X2V4cG9ydF9yZW5kZXI=";
  function b64ToBytes(b64) {
    var bin = atob(b64);
    var out = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  (function() {
    "use strict";
    var port = null;
    var RENDER_EXPORT = "__export_export_render";
    var WASM_BYTES = null;
    function highlighterBytes() {
      if (WASM_BYTES === null) WASM_BYTES = b64ToBytes(HIGHLIGHTER_WASM_B64);
      return WASM_BYTES;
    }
    var STYLE = [
      "html,body{margin:0;background:#0f0b16}",
      "pre{margin:0;padding:14px 16px;background:#0f0b16;color:#d8cfe8;",
      "font:13px/1.6 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;",
      "white-space:pre-wrap;word-break:break-word;border-radius:8px;overflow:auto;",
      "-webkit-font-smoothing:antialiased}",
      "code{font:inherit}",
      "span.com{color:#6f6585;font-style:italic}",
      // comment
      "span.str{color:#8fe3a8}",
      // string
      "span.num{color:#f0a878}",
      // number
      "span.kw{color:#c99cff}",
      // keyword
      "span.cap{color:#ffd479;font-weight:600}"
      // capability (authority)
    ].join("");
    function ensureStyle() {
      if (document.getElementById("hl-style")) return;
      var st = document.createElement("style");
      st.id = "hl-style";
      st.textContent = STYLE;
      (document.head || document.documentElement).appendChild(st);
    }
    function renderVNode(v) {
      if (v != null && typeof v.text === "string") {
        return document.createTextNode(v.text);
      }
      if (v == null || typeof v.el !== "string") {
        throw new Error("malformed vnode: " + JSON.stringify(v));
      }
      var el = document.createElement(v.el);
      var attrs = v.attrs || [];
      for (var i = 0; i < attrs.length; i++) {
        var kind = attrs[i][0], name = attrs[i][1], value = attrs[i][2];
        if (kind === "prop") {
          el.setAttribute(name, value);
        } else {
          throw new Error("unexpected attr kind `" + kind + "` (highlighter emits only props)");
        }
      }
      var kids = v.kids || [];
      for (var k = 0; k < kids.length; k++) el.appendChild(renderVNode(kids[k]));
      return el;
    }
    async function renderSource(text) {
      ensureStyle();
      var out = document.getElementById("out");
      while (out.firstChild) out.removeChild(out.firstChild);
      var src = text == null ? "" : String(text);
      var node;
      try {
        var rt = await instantiate(highlighterBytes());
        var json = rt.callString(RENDER_EXPORT, JSON.stringify({ src }));
        var tree = JSON.parse(json);
        if (tree && tree.error) throw new Error("highlighter: " + tree.error);
        node = renderVNode(tree);
      } catch (e) {
        var pre = document.createElement("pre");
        pre.textContent = src;
        out.appendChild(pre);
        var err = document.createElement("div");
        err.style.color = "#b00";
        err.textContent = "highlighter unavailable: " + (e && e.message ? e.message : String(e));
        out.appendChild(err);
        return out.scrollHeight || document.body.scrollHeight || 0;
      }
      out.appendChild(node);
      return out.scrollHeight || document.body.scrollHeight || 0;
    }
    function domSummary() {
      var out = document.getElementById("out");
      return {
        kw: out.querySelectorAll("span.kw").length,
        str: out.querySelectorAll("span.str").length,
        com: out.querySelectorAll("span.com").length,
        spans: out.querySelectorAll("span").length,
        realScripts: out.querySelectorAll("script").length,
        scriptAsText: out.textContent.indexOf("<script>") >= 0
      };
    }
    function networkIsBlocked() {
      try {
        return fetch("/api/coven/index").then(function() {
          return false;
        }).catch(function() {
          return true;
        });
      } catch (e) {
        return Promise.resolve(true);
      }
    }
    window.addEventListener("message", function(e) {
      if (!e.data || e.data.type !== "port" || !e.ports.length) return;
      port = e.ports[0];
      port.onmessage = function(ev) {
        var m = ev.data || {};
        if (m.type === "render") {
          renderSource(m.text).then(function(h) {
            var dom = domSummary();
            networkIsBlocked().then(function(blocked) {
              port.postMessage({ type: "height", px: h, networkBlocked: blocked, dom });
            });
          });
        }
      };
      port.postMessage({ type: "ready" });
    });
  })();
})();
