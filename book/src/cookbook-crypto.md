# Hashing and Signatures

The `crypto` module provides the cryptographic primitives a program reaches for:
hashes, HMACs, and signature verification. Like the other text modules it is
pure computation — hashing a string needs no capability. Signing, by contrast,
requires a `Secret` (covered with the secret store), because the private key is
authority you must be granted.

## Hashes and HMACs

`sha256` (and `sha512`, `sha3_256`) return the digest as a lowercase hex string.
`hmac_sha256` computes a keyed hash — note its key argument is **hex-encoded**,
so encode a raw string key with `encoding.hex_encode` first:

```witchy
import crypto
import encoding

fn main(console: Console):
    let digest = crypto.sha256("witchy")
    console.print("sha256: ${digest}")
    // hmac_sha256 takes a hex-encoded key.
    let key = encoding.hex_encode("secret-key")
    console.print("hmac: ${crypto.hmac_sha256(key, "message")}")
    match encoding.hex_decode_bytes(digest):
        Ok(b) -> console.print("digest bytes: ${b.length()}")
        Err(e) -> console.print("bad hex: ${e}")
```

```text
sha256: 0b21a169ba53c957fa074e1e00cd5fdc4e670dd855366bd2a38b413a1ddf88cd
hmac: 287a3bd8a4fc7731a94c722079055323644d8798bd291bf9878abc9b8fd4b1d0
digest bytes: 32
```

A SHA-256 digest is 32 bytes, which is why decoding its 64-hex-character string
yields a `Bytes` of length 32. Pair `crypto` with `encoding` whenever you need
to move a digest between its hex, base64, and raw-byte forms.

## Verifying signatures

Signature *verification* takes only public material — a public key, the message,
and the signature — so it needs no secret and no capability. `ed25519_verify`,
`ecdsa_p256_verify`, and `rsa_pkcs1_sha256_verify` each return
`Result(Bool, VerifyError)`: the `Result` distinguishes a *malformed* input
(bad key encoding — an `Err`) from a well-formed input whose signature simply
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

To *produce* a signature you need the private key, which arrives as a `Secret` —
never as a plain string in your source. `crypto.sign(key, message)` takes that
`Secret`; `crypto.public_key(key)` derives the shareable public half. The secret
store chapter covers how a `Secret` is granted and why it never prints or
serializes. The shape to remember: verification is public and capability-free,
signing is gated behind a `Secret`, and the two never blur together.
