#!/usr/bin/env node
// BUG-276 parity test: the browser runtime's `hexToBytes` must reject exactly
// what native's `hex_decode`/`hex_bytes` (crates/witchy-runtime/src/native.rs)
// reject — an odd length or ANY non-hex character (whitespace included). The old
// lossy codec filtered non-hex chars and dropped an odd tail, so the browser
// accepted keys/signatures the NATIVE backend rejects (a security divergence
// feeding crypto.hmac_sha256 / crypto.ed25519_verify). Both must reject
// IDENTICALLY: `hexToBytes` returns `null` on malformed input, mirroring native's
// `Option::None`.
import { hexToBytes } from "./witchy-runtime.mjs";
import { strict as assert } from "node:assert";

// Valid, even-length lowercase/uppercase/mixed hex decodes byte-for-byte.
assert.deepEqual([...hexToBytes("")], [], "empty is empty");
assert.deepEqual([...hexToBytes("00ff")], [0x00, 0xff], "lowercase decodes");
assert.deepEqual([...hexToBytes("00FF")], [0x00, 0xff], "uppercase decodes");
assert.deepEqual([...hexToBytes("DeadBeef")], [0xde, 0xad, 0xbe, 0xef], "mixed case decodes");

// Native `hex_bytes` rejects odd length up front → None. Browser must return null,
// NOT drop the trailing nibble.
assert.equal(hexToBytes("abc"), null, "odd length rejected");
assert.equal(hexToBytes("0"), null, "single nibble rejected");

// Any non-hex character rejects the whole string (no silent filtering). This is
// the security core: a signature/key with embedded junk must NOT parse.
assert.equal(hexToBytes("zz"), null, "non-hex char rejected");
assert.equal(hexToBytes("00gg"), null, "trailing non-hex rejected");
assert.equal(hexToBytes("de ad"), null, "internal whitespace rejected (native rejects it too)");
assert.equal(hexToBytes(" 00"), null, "leading whitespace rejected");
assert.equal(hexToBytes("00\n"), null, "trailing whitespace rejected");
assert.equal(hexToBytes("0x00"), null, "0x prefix rejected");

// A 64-hex-char Ed25519 pubkey (a valid case that MUST still work).
const pk = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";
assert.equal(hexToBytes(pk).length, 32, "a 64-char key decodes to 32 bytes");

console.log("hex-strict: OK — browser hexToBytes rejects identically to native hex_decode (BUG-276)");
