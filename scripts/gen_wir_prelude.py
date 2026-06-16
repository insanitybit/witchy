#!/usr/bin/env python3
"""Generator: extract the runtime-prelude `*_WAT` consts from src/codegen.rs
VERBATIM and emit src/wir_prelude.rs (the static helper-text mirror + the
assemble/extract logic).

Run from the repo root (`python3 scripts/gen_wir_prelude.py`) whenever
codegen.rs's prelude consts change, so the mirrored text stays byte-identical.
"""
import re, sys

CG = "src/codegen.rs"
OUT = "src/wir_prelude.rs"

PRELUDE_HEADER = r'''//! Pre-compiled static runtime prelude for the WIR (witchy IR) WASM backend.
//!
//! The legacy backend (`src/codegen.rs::compile_module_with`) builds every
//! module by concatenating ~75 hand-written WAT helper functions (`$mkN`,
//! `$concat`, `$str_eq`, the `$list_*` / `$dict_*` / `$crypto_*` families, the
//! host-wrapper helpers, …) into the module text, then handing the whole text
//! to wasmtime. The WIR migration wants those helpers as *binary* — assembled
//! once, then spliced verbatim into each user module's code section.
//!
//! This module does exactly that: it assembles a single self-contained wasm
//! module (imports + memory + globals + closure types + table + every helper),
//! compiles it to wasm bytes ONCE (lazily, behind a `OnceLock`, so the `wat`
//! crate never runs on the per-program hot path), then reads the binary back
//! with `wasmparser` to expose, per helper: its name, its `(params, results)`
//! signature, and its raw code-section body bytes (locals + instructions, with
//! NO leading size LEB). Imports, globals, and the table are exposed too.
//!
//! # Index layout (what a downstream `lower_module` MUST match)
//!
//! `Prelude` assumes the encoder lays the module out as:
//!
//! 1. **Type section**: closure call types `$clos0 … $clos4` occupy type
//!    indices `0 … 4` (params `(i32 env, i64×N)`, result `i64`). Helper and
//!    user function types follow.
//! 2. **Function index space** (imports first, per the wasm spec):
//!    * indices `0 .. imports.len()` — the imported host functions, in the
//!      exact order of [`Prelude::imports`] (which mirrors `codegen.rs`'s
//!      `emit_imports`, every feature on: `print`, then the `*_host` family).
//!    * the next `funcs.len()` indices — the prelude helpers, in the exact
//!      order of [`Prelude::funcs`]: first `$mk0 … $mk8`, then the static
//!      helpers in [`HELPER_NAMES`] order (the canonical
//!      `emit_data_globals_helpers` "all features on" order).
//!    * user functions follow, at `imports.len() + funcs.len() ..`.
//! 3. **Globals**: `$heap` (mut i32) then `$__witchy_reowns` (mut i64),
//!    matching [`Prelude::globals`]. User/actor-state globals follow.
//! 4. **Table 0** (`funcref`): present for `call_indirect (type $clos1)` inside
//!    `$dict_update` / `$dict_update_cap`. The encoder owns its final size and
//!    elem segments; [`Prelude::table_size`] is the prelude's own minimum.
//!
//! Because a helper body's `call $other` / `call_indirect (type $closN)`
//! operands are encoded as *indices*, the raw bodies splice unchanged ONLY when
//! the encoder reproduces this exact ordering. The whole point of pinning the
//! order here is to make that contract explicit.
//!
//! NOTE: the `*_WAT` consts below are mirrored VERBATIM from `src/codegen.rs`
//! (they are private there, so they cannot be referenced). They MUST stay in
//! sync with codegen.rs; `scripts/gen_wir_prelude.py` regenerates this file
//! from it (this file is GENERATED — edit the generator, not this output).

#![cfg(feature = "native")]

use std::sync::OnceLock;
use wasmparser::{CompositeInnerType, Operator, Parser, Payload, TypeRef, ValType};

'''

PRELUDE_BODY = r'''/// The closure call-signature arities pre-declared in the prelude type section
/// (`$clos0 … $clos{MAX_CLOS}`). A future user module may need higher arities;
/// it appends them after these fixed indices.
pub const MAX_CLOS: usize = 4;

/// The largest record/constructor arity for which a `$mkN` allocator is
/// pre-baked. Arities above this are emitted per-module by the encoder.
pub const MAX_MK: usize = 8;

/// A single value type in a prelude signature, mirrored from wasm core valtypes
/// (the prelude only ever uses the four numeric types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmTy {
    I32,
    I64,
    F32,
    F64,
}

impl WasmTy {
    fn from_val(v: ValType) -> WasmTy {
        match v {
            ValType::I32 => WasmTy::I32,
            ValType::I64 => WasmTy::I64,
            ValType::F32 => WasmTy::F32,
            ValType::F64 => WasmTy::F64,
            other => panic!("prelude uses only numeric valtypes, got {other:?}"),
        }
    }
}

/// An imported host function the prelude depends on.
#[derive(Debug, Clone)]
pub struct PreludeImport {
    /// The two-level wasm import name, e.g. `("witchy", "print")`.
    pub module: String,
    pub name: String,
    pub params: Vec<WasmTy>,
    pub results: Vec<WasmTy>,
}

/// A pre-compiled prelude helper function: its `$name`, its signature, and the
/// raw code-section body (locals declarations + instruction bytes, WITHOUT the
/// leading body-size LEB — the encoder writes that itself).
#[derive(Debug, Clone)]
pub struct PreludeFunc {
    pub name: String,
    pub params: Vec<WasmTy>,
    pub results: Vec<WasmTy>,
    pub raw_body: Vec<u8>,
}

/// A prelude global: its name, value type, mutability, and a single literal
/// `i32`/`i64` init constant (the only init forms the prelude uses).
#[derive(Debug, Clone)]
pub struct PreludeGlobal {
    pub name: String,
    pub ty: WasmTy,
    pub mutable: bool,
    pub init_i64: i64,
}

/// The fully extracted, splice-ready static runtime prelude. See the module
/// doc for the index layout the fields assume.
#[derive(Debug, Clone)]
pub struct Prelude {
    pub imports: Vec<PreludeImport>,
    pub globals: Vec<PreludeGlobal>,
    pub funcs: Vec<PreludeFunc>,
    /// Minimum size of table 0 the prelude itself requires (it constructs no
    /// lambdas, so this is 0; the table exists only so `call_indirect` validates).
    pub table_size: u32,
    /// Element-segment entries the prelude installs (none — the prelude builds
    /// no lambdas). Present so the downstream encoder's shape matches.
    pub elems: Vec<String>,
}

/// The `$mkN` allocator helper for an N-field constructor record, byte-verbatim
/// with `codegen.rs::mk_helper` (kept in sync by hand — it is a 1:1 copy).
fn mk_helper(n: usize) -> String {
    let mut params = String::from("(param $tag i32)");
    for i in 0..n {
        params.push_str(&format!(" (param $f{i} i64)"));
    }
    let size = 4 + 8 * n;
    let mut s = format!("  (func $mk{n} {params} (result i32)\n    (local $p i32)\n");
    s.push_str(&format!("    (call $ensure (i32.const {size}))\n"));
    s.push_str("    global.get $heap local.set $p\n");
    s.push_str("    local.get $p local.get $tag i32.store\n");
    for i in 0..n {
        s.push_str(&format!(
            "    local.get $p i32.const {} i32.add local.get $f{i} i64.store\n",
            4 + 8 * i
        ));
    }
    s.push_str(&format!("    local.get $p i32.const {size} i32.add global.set $heap\n"));
    s.push_str("    local.get $p)\n");
    s
}

/// The `(import "witchy" ...)` lines the prelude helpers reference, in the
/// canonical `emit_imports` order (every feature on). `$print` first, then the
/// `*_host` family. The encoder MUST place these at function indices `0..` in
/// exactly this order for the spliced bodies' `call` operands to resolve.
const PRELUDE_IMPORTS_WAT: &str = r#"  (import "witchy" "print" (func $print (param i32 i32)))
  (import "witchy" "crypto.sha256" (func $crypto_sha256_host (param i32 i32)))
  (import "witchy" "crypto.rune_hash" (func $crypto_rune_hash_host (param i32 i32 i32)))
  (import "witchy" "compiler_footprint_len" (func $compiler_footprint_len_host (param i32) (result i32)))
  (import "witchy" "compiler_diff_len" (func $compiler_diff_len_host (param i32 i32) (result i32)))
  (import "witchy" "field_str_len" (func $field_str_len_host (param i32) (result i32)))
  (import "witchy" "field_intlist_len" (func $field_intlist_len_host (param i32) (result i32)))
  (import "witchy" "field_strlist_size" (func $field_strlist_size_host (param i32) (result i32)))
  (import "witchy" "float_to_str" (func $float_to_str_host (param f64 i32) (result i32)))
  (import "witchy" "encoding" (func $encoding_host (param i32 i32 i32) (result i32)))
  (import "witchy" "crypto.sign" (func $crypto_sign_host (param i32 i32)))
  (import "witchy" "crypto.public_key" (func $crypto_public_key_host (param i32)))
  (import "witchy" "env_len" (func $env_len_host (param i32) (result i32)))
  (import "witchy" "env_fill" (func $env_fill_host (param i32 i32)))
  (import "witchy" "dir_read_len" (func $dir_read_len_host (param i32 i32) (result i32)))
  (import "witchy" "dir_list_size" (func $dir_list_size_host (param i32) (result i32)))
  (import "witchy" "args_size" (func $args_size_host (result i32)))
  (import "witchy" "write_pending_list" (func $write_pending_list_host (param i32)))
  (import "witchy" "build_read_len" (func $build_read_len_host (param i32 i32) (result i32)))
  (import "witchy" "net_recv_line_len" (func $net_recv_line_len_host (param i32) (result i32)))
  (import "witchy" "net_recv_all_len" (func $net_recv_all_len_host (param i32) (result i32)))
  (import "witchy" "net_recv_bytes_len" (func $net_recv_bytes_len_host (param i32 i64) (result i32)))
  (import "witchy" "fill_pending" (func $fill_pending_host (param i32)))
"#;

/// The number of host imports the prelude declares (used to split function
/// indices: imports `0..IMPORT_COUNT`, helpers after).
pub const IMPORT_COUNT: usize = 23;

/// Assemble the full prelude module TEXT. This is what gets compiled once.
///
/// Layout, in module-field order: closure types, imports, memory, globals,
/// table, then the `$mkN` allocators followed by the static helpers.
pub fn prelude_wat() -> String {
    let mut s = String::from("(module\n");
    // Closure call types $clos0..$clos{MAX_CLOS}: env pointer + N i64 args, i64 result.
    for n in 0..=MAX_CLOS {
        let params = format!("(param i32) {}", "(param i64) ".repeat(n));
        s.push_str(&format!("  (type $clos{n} (func {params}(result i64)))\n"));
    }
    s.push_str(PRELUDE_IMPORTS_WAT);
    s.push_str("  (memory (export \"memory\") 1)\n");
    // A funcref table so `call_indirect (type $clos1)` in $dict_update validates.
    s.push_str("  (table 0 funcref)\n");
    // Globals: $heap then $__witchy_reowns. The heap starts past a small fixed
    // reserve; the real per-module value is patched by the encoder, so the init
    // here is a placeholder the downstream layout overwrites.
    s.push_str("  (global $heap (mut i32) (i32.const 1024))\n");
    s.push_str(
        "  (global $__witchy_reowns (export \"__witchy_reowns\") (mut i64) (i64.const 0))\n",
    );
    // The $mkN allocators, N = 0..=MAX_MK.
    for n in 0..=MAX_MK {
        s.push_str(&mk_helper(n));
    }
    // The static helpers, in canonical order.
    for w in HELPER_WATS {
        s.push_str(w);
    }
    s.push_str(")\n");
    s
}

/// The full ordered name list for the funcs section: `$mk0..$mk{MAX_MK}` then
/// the static helper names. Matches the order `prelude_wat` emits bodies, so
/// the i-th code-section entry is `func_names()[i]`.
fn func_names() -> Vec<String> {
    let mut v: Vec<String> = (0..=MAX_MK).map(|n| format!("mk{n}")).collect();
    v.extend(HELPER_NAMES.iter().map(|s| s.to_string()));
    v
}

fn build_prelude() -> Prelude {
    let wat = prelude_wat();
    let bin = wat::parse_str(&wat).expect("prelude WAT assembles to wasm");

    let names = func_names();
    // Type section: collect every function type so import/func type indices resolve.
    let mut func_types: Vec<(Vec<WasmTy>, Vec<WasmTy>)> = Vec::new();
    let mut imports: Vec<PreludeImport> = Vec::new();
    let mut globals: Vec<PreludeGlobal> = Vec::new();
    let mut defined_type_idx: Vec<u32> = Vec::new(); // type index per DEFINED func
    let mut bodies: Vec<Vec<u8>> = Vec::new();
    let mut table_size: u32 = 0;

    for payload in Parser::new(0).parse_all(&bin) {
        match payload.expect("valid prelude wasm") {
            Payload::TypeSection(reader) => {
                for rec in reader {
                    let rec = rec.expect("type rec group");
                    for sub in rec.types() {
                        if let CompositeInnerType::Func(ft) = &sub.composite_type.inner {
                            func_types.push((
                                ft.params().iter().copied().map(WasmTy::from_val).collect(),
                                ft.results().iter().copied().map(WasmTy::from_val).collect(),
                            ));
                        } else {
                            // Non-func types should not occur in the prelude.
                            func_types.push((Vec::new(), Vec::new()));
                        }
                    }
                }
            }
            Payload::ImportSection(reader) => {
                // `into_imports` flattens any compact-import grouping to a flat
                // stream of `Import`s in declaration order.
                for imp in reader.into_imports() {
                    let imp = imp.expect("import");
                    if let TypeRef::Func(ti) = imp.ty {
                        let (p, r) = func_types[ti as usize].clone();
                        imports.push(PreludeImport {
                            module: imp.module.to_string(),
                            name: imp.name.to_string(),
                            params: p,
                            results: r,
                        });
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                for ti in reader {
                    defined_type_idx.push(ti.expect("func type idx"));
                }
            }
            Payload::TableSection(reader) => {
                for t in reader {
                    let t = t.expect("table");
                    table_size = table_size.max(t.ty.initial as u32);
                }
            }
            Payload::GlobalSection(reader) => {
                for g in reader {
                    let g = g.expect("global");
                    let init = const_i64(&g.init_expr);
                    // Names are not in the binary; map by definition order to the
                    // two globals the prelude declares.
                    let name = match globals.len() {
                        0 => "heap",
                        1 => "__witchy_reowns",
                        _ => "?",
                    }
                    .to_string();
                    globals.push(PreludeGlobal {
                        name,
                        ty: WasmTy::from_val(g.ty.content_type),
                        mutable: g.ty.mutable,
                        init_i64: init,
                    });
                }
            }
            Payload::CodeSectionEntry(body) => {
                bodies.push(body.as_bytes().to_vec());
            }
            _ => {}
        }
    }

    assert_eq!(
        bodies.len(),
        names.len(),
        "prelude func body count must match the controlled name list"
    );
    assert_eq!(
        defined_type_idx.len(),
        names.len(),
        "function-section entry count must match defined bodies"
    );

    let funcs = names
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let (params, results) = func_types[defined_type_idx[i] as usize].clone();
            PreludeFunc {
                name,
                params,
                results,
                raw_body: bodies[i].clone(),
            }
        })
        .collect();

    Prelude {
        imports,
        globals,
        funcs,
        table_size,
        elems: Vec::new(),
    }
}

/// Read a single `i32.const` / `i64.const` global init expression as an i64.
fn const_i64(expr: &wasmparser::ConstExpr) -> i64 {
    let mut r = expr.get_operators_reader();
    match r.read().expect("const-expr op") {
        Operator::I32Const { value } => value as i64,
        Operator::I64Const { value } => value,
        other => panic!("prelude global init must be a numeric const, got {other:?}"),
    }
}

/// The pre-compiled, splice-ready static runtime prelude. Assembled and
/// extracted ONCE, lazily, on first call (so the `wat` crate stays off the hot
/// per-program compile path).
pub fn prelude() -> &'static Prelude {
    static PRELUDE: OnceLock<Prelude> = OnceLock::new();
    PRELUDE.get_or_init(build_prelude)
}

#[cfg(test)]
#[cfg(feature = "native")]
mod tests {
    use super::*;

    #[test]
    fn prelude_text_assembles() {
        // The whole assembled prelude module is valid WAT -> wasm.
        let wat = prelude_wat();
        let bin = wat::parse_str(&wat).expect("prelude assembles");
        assert!(bin.len() > 64, "assembled prelude is non-trivial");
        assert_eq!(&bin[0..4], b"\0asm", "wasm magic");
    }

    #[test]
    fn prelude_extracts_funcs_and_signatures() {
        let p = prelude();
        assert!(!p.funcs.is_empty(), "prelude has helper funcs");
        // The fixed allocators and a few well-known helpers are present.
        let by_name = |n: &str| p.funcs.iter().find(|f| f.name == n);

        let mk1 = by_name("mk1").expect("mk1 present");
        // mk1: (param $tag i32) (param $f0 i64) (result i32)
        assert_eq!(mk1.params, vec![WasmTy::I32, WasmTy::I64]);
        assert_eq!(mk1.results, vec![WasmTy::I32]);

        let concat = by_name("concat").expect("concat present");
        // concat: (param $a i32) (param $b i32) (result i32)
        assert_eq!(concat.params, vec![WasmTy::I32, WasmTy::I32]);
        assert_eq!(concat.results, vec![WasmTy::I32]);

        let str_eq = by_name("str_eq").expect("str_eq present");
        // str_eq: (param $a i32) (param $b i32) (result i32)
        assert_eq!(str_eq.params, vec![WasmTy::I32, WasmTy::I32]);
        assert_eq!(str_eq.results, vec![WasmTy::I32]);

        // mk0..mk8 all present.
        for n in 0..=MAX_MK {
            assert!(by_name(&format!("mk{n}")).is_some(), "mk{n} present");
        }
    }

    #[test]
    fn raw_bodies_are_non_empty() {
        let p = prelude();
        for f in &p.funcs {
            assert!(!f.raw_body.is_empty(), "{} has a raw body", f.name);
        }
        // A body begins with the locals-declaration count (a LEB u32); for a
        // helper with locals it is non-zero, but every body has at least the
        // count byte plus its end opcode.
        let concat = p.funcs.iter().find(|f| f.name == "concat").unwrap();
        assert!(concat.raw_body.len() > 2, "concat body has content");
        assert_eq!(*concat.raw_body.last().unwrap(), 0x0b, "body ends with `end`");
    }

    #[test]
    fn imports_and_globals_present() {
        let p = prelude();
        assert_eq!(p.imports.len(), IMPORT_COUNT, "import count matches");
        assert_eq!(p.imports[0].module, "witchy");
        assert_eq!(p.imports[0].name, "print");
        assert_eq!(p.imports[0].params, vec![WasmTy::I32, WasmTy::I32]);
        // Globals: $heap (mut i32) then $__witchy_reowns (mut i64).
        assert_eq!(p.globals.len(), 2);
        assert_eq!(p.globals[0].name, "heap");
        assert_eq!(p.globals[0].ty, WasmTy::I32);
        assert!(p.globals[0].mutable);
        assert_eq!(p.globals[1].name, "__witchy_reowns");
        assert_eq!(p.globals[1].ty, WasmTy::I64);
    }
}
'''


# The exact prelude consts, in the order codegen.rs's emit_data_globals_helpers
# would emit them when EVERY feature is on. This order is the canonical prelude
# layout (see module doc in the generated file).
ORDER = [
    # heap-backed core (need_heap)
    "ENSURE_WAT", "CONCAT_WAT",
    # list ops
    "LIST_AT_WAT", "LIST_PUSH_WAT",
    "LIST_PUSH_CAP_WAT", "STR_APPEND_CAP_WAT", "DICT_INSERT_CAP_WAT", "DICT_UPDATE_CAP_WAT",
    "LIST_CONCAT_WAT", "LIST_DROP_WAT",
    "STARTS_WITH_WAT", "ENDS_WITH_WAT", "SUBSTR_WAT", "ASCII_CASE_WAT",
    # crypto / fields / compiler / float
    "CRYPTO_SHA256_WAT", "CRYPTO_RUNE_HASH_WAT",
    "FIELD_STR_GET_WAT", "FIELD_INTLIST_GET_WAT", "FIELD_STRLIST_GET_WAT",
    "COMPILER_FOOTPRINT_WAT", "COMPILER_DIFF_WAT",
    "FLOAT_TO_STR_WAT", "ENCODING_WAT", "GET_ENV_WAT",
    # dir / build / net / args
    "DIR_READ_WAT", "BUILD_READ_WAT", "DIR_LIST_WAT",
    "NET_RECV_LINE_WAT", "NET_RECV_ALL_WAT", "NET_RECV_BYTES_WAT",
    "BUILD_ARGS_WAT", "CRYPTO_SIGN_WAT", "CRYPTO_PUBLIC_KEY_WAT", "FLOAT_ORD_WAT",
    # string ops
    "SPLIT_WAT", "STR_CHARS_WAT", "FIND_BYTE_WAT", "BYTE_TO_CHAR_WAT",
    "STR_INDEX_OF_WAT", "CHAR_TO_BYTE_WAT", "STR_SUBSTRING_WAT",
    "MATCH_AT_WAT", "REPLACE_WAT", "STR_TO_INT_WAT", "IS_WS_WAT", "TRIM_WAT",
    # dict family
    "DICT_NEW_WAT", "DICT_HASH_WAT", "DICT_FIND_WAT", "DICT_INDEX_PUT_WAT",
    "DICT_INDEX_BUILD_WAT", "KEY_EQ_WAT", "DICT_INSERT_WAT", "DICT_GET_OR_WAT",
    "DICT_HAS_WAT", "DICT_REMOVE_WAT", "DICT_UPDATE_WAT",
    "DICT_KEYS_WAT", "DICT_VALUES_WAT", "DICT_PAIRS_WAT",
    # io / conv / cmp
    "PRINT_STR_WAT", "INT_TO_STRING_WAT", "STR_EQ_WAT", "STR_CMP_WAT",
]

src = open(CG).read()

# Extract each `const NAME_WAT: &str = r#"...."#;` body verbatim.
consts = {}
for m in re.finditer(r'const (\w+_WAT): &str = r#"(.*?)"#;', src, re.S):
    consts[m.group(1)] = m.group(2)

missing = [n for n in ORDER if n not in consts]
if missing:
    sys.exit(f"missing consts in codegen.rs: {missing}")

# Sanity: the set of consts we mirror equals exactly ORDER (no stragglers we forgot).
all_wat = set(consts.keys())
# Some *_WAT consts in codegen.rs are NOT prelude funcs we splice (none expected,
# but guard anyway): warn if there are consts we did not list.
extra = sorted(all_wat - set(ORDER))
if extra:
    sys.stderr.write(f"NOTE: codegen.rs has _WAT consts not in prelude ORDER: {extra}\n")

# The ordered list of function NAMES each const defines. A const may define
# MORE than one `(func $name ...)` (e.g. FLOAT_ORD_WAT bundles five float
# comparators), so collect every name in source order and flatten — the wasm
# code section will contain one entry per `(func)`, in exactly this order.
def func_names_of(body):
    return re.findall(r'\(func \$([A-Za-z0-9_]+)', body)

names_in_order = []
for n in ORDER:
    fns = func_names_of(consts[n])
    assert fns, f"{n} defines no (func)"
    names_in_order.extend(fns)

# Emit the consts block.
def rust_const(name, body):
    return f'const {name}: &str = r#"{body}"#;\n'

parts = []
parts.append(PRELUDE_HEADER)
for n in ORDER:
    parts.append(rust_const(n, consts[n]))
parts.append("\n")
# The const slice referencing them, in order.
parts.append("/// The static helper WAT consts, in canonical prelude order (mirrored\n")
parts.append("/// verbatim from `src/codegen.rs`).\n")
parts.append("const HELPER_WATS: &[&str] = &[\n")
for n in ORDER:
    parts.append(f"    {n},\n")
parts.append("];\n\n")
# The names, in the same order, AFTER the $mkN funcs (see module doc layout).
parts.append("/// Function names for the static helpers above, same order.\n")
parts.append("const HELPER_NAMES: &[&str] = &[\n")
for nm in names_in_order:
    parts.append(f'    "{nm}",\n')
parts.append("];\n\n")
parts.append(PRELUDE_BODY)

open(OUT, "w").write("".join(parts))
print(f"wrote {OUT}: {len(ORDER)} helper consts, {len(names_in_order)} names")
