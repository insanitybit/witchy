# Hashing and Signatures

The `crypto` module provides the cryptographic primitives a program reaches for:
hashes, HMACs, and signature verification. Like the other text modules it's
pure computation - hashing a string needs no capability. Signing, by contrast,
requires a `Secret` (covered with the secret store), because the private key is
authority you must be granted.

## Hashes and HMACs

`sha256` returns the digest as a lowercase hex string:

```witchy
import crypto
import encoding

fn main(console: Console):
    let digest = crypto.sha256("witchy")
    console.print("sha256: ${digest}")
    match encoding.hex_decode_bytes(digest):
        Ok(b) -> console.print("digest bytes: ${b.length()}")
        Err(e) -> console.print("bad hex: ${e}")
```

```text
sha256: 0b21a169ba53c957fa074e1e00cd5fdc4e670dd855366bd2a38b413a1ddf88cd
digest bytes: 32
```

A SHA-256 digest is 32 bytes, which is why decoding its 64-hex-character string
yields a `Bytes` of length 32. Pair `crypto` with `encoding` whenever you need
to move a digest between its hex, base64, and raw-byte forms.

`sha512`, `sha3_256`, and `hmac_sha256` work the same way but are native-only -
a browser-hosted module doesn't get them. `hmac_sha256` computes a keyed hash;
note its key argument is **hex-encoded**, so encode a raw string key with
`encoding.hex_encode` first:

```
let key = encoding.hex_encode("secret-key")
let tag = crypto.hmac_sha256(key, "message")
// tag = "287a3bd8a4fc7731a94c722079055323644d8798bd291bf9878abc9b8fd4b1d0"
```

## Verifying signatures

Signature *verification* takes only public material - a public key, the message,
and the signature - so it needs no secret and no capability. `ed25519_verify`,
`ecdsa_p256_verify`, and `rsa_pkcs1_sha256_verify` each return
`Result(Bool, VerifyError)`: the `Result` distinguishes a *malformed* input
(bad key encoding - an `Err`) from a well-formed input whose signature simply
doesn't match (`Ok(false)`). That distinction matters: treating a decode error
as "signature invalid" can mask a bug, so match both.

```sh
import crypto

fn check(console: Console, pubkey: String, msg: String, sig: String):
    match crypto.ed25519_verify(pubkey, msg, sig):
        Ok(true) -> console.print("signature valid")
        Ok(false) -> console.print("signature does NOT match")
        Err(e) -> console.print("malformed input: ${e}")
```

## Signing needs a `Secret`

To *produce* a signature you need the private key, which arrives as a `Secret` -
never as a plain string in your source. `crypto.sign(key, message)` takes that
`Secret`; `crypto.public_key(key)` derives the shareable public half. The secret
store chapter covers how a `Secret` is granted and why it never prints or
serializes. The shape to remember: verification is public and capability-free,
signing is gated behind a `Secret`, and the two never blur together.

### Sealing a key you only need to sign with

A `Secret` has two rights. `Reveal` reads its bytes; `Seal` uses it by handle.
Signing and public-key derivation are by-handle operations, so they need only
`Seal` - which means a function that just signs can say so in its signature:

```witchy
import crypto
import secretstore

// `Secret[Seal]` is a promise the checker enforces: this cannot read the key.
fn endorse(key: Secret[Seal], release: String) -> String:
    key.sign(release)

fn main(console: Console, secrets: SecretStore):
    let key = secrets.require("signing")
    console.print(endorse(key, "v1.0.0"))
    console.print(crypto.public_key(key))
```

Narrowing only ever drops rights, so `endorse` cannot pass its sealed handle
somewhere that reveals it, and cannot ascribe its way back to a bare `Secret`.
Calling `key.reveal()` inside `endorse` does not compile - the error names the
missing `Reveal` right rather than failing at run time in production.

That is the whole point of the annotation: a reviewer reading `endorse`'s
signature knows it cannot exfiltrate the key, without reading its body. Revealing
is still ordinary and correct for the secrets that need it - an API token you send
to a service is data your program legitimately handles - so bare `Secret` remains
the default.
