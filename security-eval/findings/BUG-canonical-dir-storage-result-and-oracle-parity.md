# Canonical directory storage: result and oracle parity corrections

Status: corrected during integration before storage submission; focused evidence
is in the RFC-0165 acceptance ledger's directory-storage section.

- Void `Dir.replace` and `Dir.write_bytes` host calls originally returned no
  source value from lowering. `let done = root.replace("state", "new")` therefore
  refused despite its valid Nil result type. Lowering now emits Unit after the
  void HostCall, matching other Nil-valued capability operations. The checked
  execution probe binds and uses both results and verifies zero successful-run
  heap leaks on native Wasm and the independent oracle.
- Oracle byte-response sizes originally allowed the whole unsigned Size domain,
  while the native ABI returns nonnegative i32 lengths. Both now refuse sizes
  above i32::MAX; allocation-free boundary probes cover the exact limit.
- Oracle fallback paths originally rejected all symlinks uniformly. The native
  atomic provider instead returns false when create_new finds a leaf symlink,
  and replace replaces that leaf without following its target. The oracle now
  models those outcomes in its fixture overlay, while reads/plain writes of an
  unshadowed link and intermediate-link traversal still refuse.

These findings are source-coverage or parity defects, not evidence of a native
memory-safety bypass. No authority, ownership, or bounds rule was weakened.
