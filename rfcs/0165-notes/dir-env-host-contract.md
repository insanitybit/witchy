# Frozen contract: canonical Dir/Env capability host calls (RFC-0165 slice 42)

Branch: impl/rfc0165-interpreter (worktree .codex/worktrees/rfc0165-interpreter). Design note:
rfcs/0165-memory-mode-boundary.md, section "Capability host calls: `Dir` and `Env`".

## Capability kinds (ValueType::Capability(CapabilityKindId::new(k)))
- 0 Console (exists), 1 Dir (any rights arguments: `Dir`, `Dir[Read]`, `Dir[Write]`, `Dir[Read, Write]`), 2 Env.
- Tokens are externrefs minted by the host only. Rights and subtree scope live in the host's authority object
  (runtime `DirAuthority` / `EnvAuthority`); a rights or scope violation is a host trap (= interpreter abort).
- Entry: `CanonicalEntryCapability::{Console, Dir, Env}` (tags 0/1/2, already added to canonical_wasmir.rs and
  derived from the entry signature's kinds). Runtime mints Dir by ordinal among the entry's Dir parameters
  (`dirs[i]`, like `mint_dir(i)`), Env from the Env grant; missing grant = refuse to launch (like Console).

## Imports (module "witchy"), canonical route only; the frozen route keeps its legacy imports
All are HostReentrancy::Forbidden. `Read Slice` = (ptr:i32, len:i32) of a checked string slice
(HostParameterRole::Buffer { Ref(string_layout), Read, Slice }). `Write Slice` = (ptr:i32, len:i32) where
len is the buffer's CAPACITY (HostParameterRole::Buffer { Ref(string_layout), Write, Slice }) — a NEW backend
form (today only Read+Slice and Write+AtLeast exist): encode as (ptr, len) like Read+Slice; the checker already
bounds a Write slice by the object's capacity (check/memory.rs check_host_buffer).
Results are HostResultRole::Value(ValueType::Size) (wasm i32).

| name | wasm params | result | authority | semantics |
|---|---|---|---|---|
| dir_read_into | externref, path Read Slice, out Write Slice | i32 | DirRead | Read the confined file at `path` NOW (same confinement/UTF-8 rules as legacy `dir_read_len`: invalid UTF-8 or a missing/denied file traps). Write min(len, out_cap) bytes at out_ptr (must fit guest memory, else trap). Return the file's full byte length (> i32::MAX traps). Stateless: no `pending`. |
| dir_list_into | externref, out Write Slice | i32 | DirRead | The admitted entry names (sorted, legacy `dir_list_size`'s policy filter) joined by NUL, through the bounded slice protocol; the guest splits them (an empty payload is the empty list). |
| dir_exists_at | externref, path Read Slice | i32 (0/1) | DirRead | Semantics of legacy `dir_exists`. |
| dir_write_at | externref, path Read Slice, data Read Slice | (none) | DirWrite | Semantics of legacy `dir_write` (data is UTF-8 text). |
| dir_subtree_at | externref, path Read Slice | externref | DirGrant (narrowing) | Semantics of legacy `dir_subdir`; result role HostResultRole::Capability(kind 1). |
| env_has_var | externref, name Read Slice | i32 (0/1) | Env | 1 iff the variable is set AND allowed by the grant's allow-list (a name outside the allow-list traps, like legacy `env_len`). |
| env_get_into | externref, name Read Slice, out Write Slice | i32 | Env | Write min(len, cap) bytes of the value; return the full length; unset variable traps (guest calls env_has_var first). Allow-list as legacy. |
Mutation: dir_read_into/env_get_into = WriteBuffers; the others ReadOnly.

## Guest protocol (lowering, mine)
Allocate a unique non-static string buffer with capacity C (initial 256), call `*_into` with Write Slice
(object, 0, C); on return `n`: publish count = min(n, C) after a guard; if n > C reallocate with capacity n and
call again, again publishing min(n2, C2). Never index by the first call's answer.

## Oracle (canonical_interpreter.rs)
`InterpreterConfig` gains `pub fixture: OracleFixture` (`#[derive(Default)]`; every constructor site in the repo
must be updated, including src/commands/execution.rs and tests): `OracleFixture { files: BTreeMap<String,
Vec<u8>> (relative paths, "/"-separated, no leading "/"), environment: BTreeMap<String, String>, dir_rights: ...
}`. Capability values: `InterpreterValue::Capability(kind)` today carries only the kind; extend to carry a
scope (kind 1: a subtree prefix String, kind 2: none). dir_read_into reads files[join(scope, path)];
dir_write_at inserts; dir_exists_at; dir_subtree_at joins (refuse `..`, absolute paths, like the runtime's
dir_guard); env_* read `environment`. Entry arguments for Dir params = scope "" (root), Env = kind 2.

## API names the lowering will call (witchy-wir)
- `CanonicalAbiCatalog::from_authenticated_prelude(print_sig, float_sig, string_layout)` unchanged.
- NEW `CanonicalAbiCatalog::with_capability_imports(self, signatures: CapabilityImportSignatures, string_layout: LayoutId) -> Result<Self, String>`
  where `pub struct CapabilityImportSignatures { pub dir_read_into: FunctionSignatureId, pub dir_exists_at: FunctionSignatureId, pub dir_write_at: FunctionSignatureId, pub dir_subtree_at: FunctionSignatureId, pub env_has_var: FunctionSignatureId, pub env_get_into: FunctionSignatureId }`.
  Projection = `authenticated_capability_import(...)`: like `authenticated_prelude_import` but REQUIRES
  `abi_import_info(name).authorities` to be exactly the authority listed above (refuse empty or mismatched).
- `pub const DIR_CAPABILITY_KIND: CapabilityKindId = new(1); pub const ENV_CAPABILITY_KIND = new(2)` in canonical_safewir.rs.
- Lowering finds imports by `import.name` on `catalog.imports()` as today.
