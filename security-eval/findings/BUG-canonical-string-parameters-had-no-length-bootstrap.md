# Canonical String parameters had no authenticated length bootstrap

## Finding

Canonical SafeWIR represented an incoming `String` as a raw-byte runtime buffer with an unknown
extent, but rejected the four-byte length-header load needed to establish that extent. Dynamic
string operations therefore could not validate without either guessing a count or bypassing the
place rules.

## Resolution

The checker now permits the bootstrap header read only for the catalog-authenticated
`RuntimeBuffer<RawByte>` layout with a four-byte base and byte stride. The loaded count becomes the
object's logical count and conservative capacity only after a dominating runtime guard proves the
complete `4 + count` allocation fits the Wasm address domain. Other unknown layouts remain
fail-closed.

Dynamic byte copies additionally require an exact non-wrapping `start + length` definition and a
dominating `end <= logical_count` relation. The checker derives both facts from final SafeWIR; the
lowerer cannot attach them as authority.

## Evidence

- `utf8_string_concat_reaches_final_safewir_validation`
- `utf8_string_concat_reaches_admitted_execution`
- `byte_copy_requires_a_dominating_dynamic_end_guard`
