# Text, Patterns, and Encodings

Most programs spend their time turning bytes into meaning and back: matching a
pattern, pulling fields out of a URL, or moving a value through base64. witchy
keeps each of these jobs in its own small module — `regex`, `url`, `encoding` —
so you import exactly the surface you need. None of them require a capability:
they are pure computation over strings, so the examples below run identically on
both backends.

## Matching with `regex`

`import regex` gives you six functions, all of which take the pattern as an
ordinary `String`. There is no compiled-pattern type to thread around — pass the
pattern and the text together each time.

```witchy
import regex

fn main(console: Console):
    let log = "GET /a 200, POST /b 404, GET /c 500"
    let codes = regex.extract("[0-9]{3}", log)
    console.print("status codes: ${codes.join(" ")}")
    let has_error = regex.matches("[45][0-9]{2}", log)
    console.print("contains 4xx/5xx: ${has_error}")
    let redacted = regex.replace_all("[0-9]{3}", log, "xxx")
    console.print(redacted)
```

```text
status codes: 200 404 500
contains 4xx/5xx: true
GET /a xxx, POST /b xxx, GET /c xxx
```

- `matches` answers a yes/no question.
- `extract` returns every match as a `List(String)`.
- `find` and `find_all` return `(Int, Int)` byte spans when you need positions
  rather than the matched text.
- `replace_all` and `split` transform the whole string.

Reach for `regex` when the shape of the input is genuinely irregular. For fixed
structure — splitting on a comma, checking a prefix — the `string` methods
(`split`, `starts_with`, `trim`) are clearer and faster.

## Encodings with `encoding`

`import encoding` covers the two encodings programs reach for constantly: hex
and base64 (including the URL-safe `base64url` variant). Encoding never fails,
so those functions return a `String`. Decoding *can* fail on malformed input, so
the decoders return a `Result` — match it and you cannot forget the bad-input
case.

```witchy
import encoding

fn main(console: Console):
    let token = "user:hunter2"
    let b64 = encoding.base64_encode(token)
    console.print("encoded: ${b64}")
    match encoding.base64_decode(b64):
        Ok(round) -> console.print("decoded: ${round}")
        Err(e) -> console.print("bad base64: ${e}")
    console.print("hex: ${encoding.hex_encode("hi")}")
```

```text
encoded: dXNlcjpodW50ZXIy
decoded: user:hunter2
hex: 6869
```

Every codec comes in a `String` form and a `Bytes` form
(`base64_encode` / `base64_encode_bytes`), plus `_string` decoder variants whose
error type is already `String` if you would rather propagate with `?` than match
a dedicated `EncodingError`. Cross-format helpers like `hex_to_base64url` save a
decode-then-reencode round trip.

## URLs with `url`

`import url` parses a URL string into a sealed `Url` value and lets you read its
parts back out. `parse` returns a `Result`, because not every string is a valid
URL.

```witchy
import url

fn main(console: Console):
    match url.parse("https://example.com:8443/search?q=witchy#top"):
        Ok(u) ->
            console.print("host: ${url.host(u)}")
            console.print("port: ${url.port(u)}")
            console.print("path: ${url.pathname(u)}")
            match url.query(u):
                Some(q) -> console.print("query: ${q}")
                None -> console.print("no query")
        Err(e) -> console.print("bad url: ${e}")
```

```text
host: example.com
port: 8443
path: /search
query: q=witchy
```

`query` and `fragment` return an `Option`, since a URL may not have them.
`with_query` returns a new `Url` with a parameter added, `request_target` gives
you the path-plus-query string an HTTP request line needs, and `format` renders
the whole thing back to a string. For form and query-string escaping without a
full URL, `url.encode` / `url.decode` operate directly on component strings.

Together these three modules cover the everyday text-wrangling that would
otherwise tempt you into hand-rolled parsing — the kind that quietly mishandles
an empty field or an unescaped character. Let the module own the edge cases.
