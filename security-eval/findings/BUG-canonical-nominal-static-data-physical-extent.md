# Nominal static-data checks must inspect the physical body

Status: latent checker defect corrected in the sealed recursive integration
slice; not a demonstrated production exploit.

The initial production checker still rejects every opaque nominal catalog.
Before that seal can be removed, static-data validation must resolve nominal
bindings. Previously `layout_contains_pointer` and `static_data_extent_matches`
read the opaque identity descriptor, whose fixed size is zero and whose field
list is empty. Their combined local judgments could accept an empty segment
as pointer-free storage even when the identity was bound to a recursive,
pointer-bearing record.

Both judgments now resolve the physical descriptor first. Recursive inline
classification goes through the same resolver, and the runtime-buffer element
classification resolves its descriptor too. Missing bindings reject. The
production seal remains; downstream exploitability was not established.

Focused regression:

```sh
CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target \
  env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= taskpolicy -c utility \
  cargo test -p witchy-wir --lib --features native \
  nominal_static_data_checks_resolve_physical_storage -- --nocapture
```

One test passed. It rejects zero-byte storage for a recursive nominal, checks a
pointer-free bound record against its actual eight-byte extent, and rejects
unbound identities. It exercises the static-data judgments directly so the
outer production seal cannot mask the defect.
