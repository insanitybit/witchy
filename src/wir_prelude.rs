//! Pre-compiled static runtime prelude for the WIR (witchy IR) WASM backend.
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

// The mirrored `*_WAT` helper consts are assembled into the prelude blob via the
// generated module text; some are referenced only there, so they read as dead to
// the compiler. (Mirrors `wir.rs`'s allow.) The prelude isn't wired into the
// pipeline yet either — the M3 sink-flip is the next step.
#![allow(dead_code)]

use std::sync::OnceLock;
use wasmparser::{CompositeInnerType, Operator, Parser, Payload, TypeRef, ValType};

const ENSURE_WAT: &str = r#"  (func $ensure (param $size i32)
    (local $need i32) (local $have i32)
    (local.set $need (i32.add (global.get $heap) (local.get $size)))
    (local.set $have (i32.mul (memory.size) (i32.const 65536)))
    (if (i32.gt_u (local.get $need) (local.get $have))
      (then (drop (memory.grow
        (i32.div_u (i32.add (i32.sub (local.get $need) (local.get $have)) (i32.const 65535)) (i32.const 65536)))))))
"#;
const CONCAT_WAT: &str = r#"  (func $concat (param $a i32) (param $b i32) (result i32)
    (local $alen i32) (local $blen i32) (local $res i32)
    local.get $a i32.load local.set $alen
    local.get $b i32.load local.set $blen
    (call $ensure (i32.add (i32.const 4) (i32.add (local.get $alen) (local.get $blen))))
    global.get $heap local.set $res
    local.get $res local.get $alen local.get $blen i32.add i32.store
    local.get $res i32.const 4 i32.add
    local.get $a i32.const 4 i32.add
    local.get $alen
    memory.copy
    local.get $res i32.const 4 i32.add local.get $alen i32.add
    local.get $b i32.const 4 i32.add
    local.get $blen
    memory.copy
    local.get $res i32.const 4 i32.add local.get $alen i32.add local.get $blen i32.add
    global.set $heap
    local.get $res)
"#;
const LIST_AT_WAT: &str = r#"  (func $list_at (param $list i32) (param $i i32) (result i64)
    (if (i32.or
          (i32.lt_s (local.get $i) (i32.const 0))
          (i32.ge_s (local.get $i) (i32.load (local.get $list))))
      (then (unreachable)))
    (i64.load
      (i32.add (i32.add (local.get $list) (i32.const 4))
               (i32.mul (local.get $i) (i32.const 8)))))
"#;
const LIST_PUSH_WAT: &str = r#"  (func $list_push (param $list i32) (param $x i64) (result i32)
    (local $len i32) (local $new i32)
    local.get $list i32.load local.set $len
    (call $ensure (i32.add (i32.const 4) (i32.mul (i32.add (local.get $len) (i32.const 1)) (i32.const 8))))
    global.get $heap local.set $new
    local.get $new local.get $len i32.const 1 i32.add i32.store
    local.get $new i32.const 4 i32.add
    local.get $list i32.const 4 i32.add
    local.get $len i32.const 8 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $len i32.const 8 i32.mul i32.add
    local.get $x i64.store
    local.get $new i32.const 4 i32.add local.get $len i32.const 1 i32.add i32.const 8 i32.mul i32.add global.set $heap
    local.get $new)
"#;
const LIST_PUSH_CAP_WAT: &str = r#"  (func $list_push_cap (param $list i32) (param $x i64) (param $cap i32) (result i32 i32)
    (local $len i32) (local $new i32) (local $newcap i32)
    (if (i32.eqz (local.get $cap))
      (then (global.set $__witchy_reowns (i64.add (global.get $__witchy_reowns) (i64.const 1)))))
    local.get $list i32.load local.set $len
    (if (i32.gt_s (local.get $cap) (local.get $len))
      (then
        (i64.store (i32.add (i32.add (local.get $list) (i32.const 4)) (i32.mul (local.get $len) (i32.const 8))) (local.get $x))
        (i32.store (local.get $list) (i32.add (local.get $len) (i32.const 1)))
        local.get $list local.get $cap
        return))
    (local.set $newcap (i32.mul (i32.add (local.get $len) (i32.const 1)) (i32.const 2)))
    (if (i32.lt_s (local.get $newcap) (i32.const 8))
      (then (local.set $newcap (i32.const 8))))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $newcap) (i32.const 8))))
    global.get $heap local.set $new
    (i32.store (local.get $new) (i32.add (local.get $len) (i32.const 1)))
    (memory.copy
      (i32.add (local.get $new) (i32.const 4))
      (i32.add (local.get $list) (i32.const 4))
      (i32.mul (local.get $len) (i32.const 8)))
    (i64.store (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $len) (i32.const 8))) (local.get $x))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $newcap) (i32.const 8))))
    local.get $new local.get $newcap)
"#;
const STR_APPEND_CAP_WAT: &str = r#"  (func $str_append_cap (param $s i32) (param $piece i32) (param $cap i32) (result i32 i32)
    (local $len i32) (local $plen i32) (local $need i32) (local $new i32) (local $newcap i32)
    (if (i32.eqz (local.get $cap))
      (then (global.set $__witchy_reowns (i64.add (global.get $__witchy_reowns) (i64.const 1)))))
    local.get $s i32.load local.set $len
    local.get $piece i32.load local.set $plen
    (local.set $need (i32.add (local.get $len) (local.get $plen)))
    (if (i32.ge_s (local.get $cap) (local.get $need))
      (then
        (memory.copy
          (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $len))
          (i32.add (local.get $piece) (i32.const 4))
          (local.get $plen))
        (i32.store (local.get $s) (local.get $need))
        local.get $s local.get $cap
        return))
    (local.set $newcap (i32.mul (local.get $need) (i32.const 2)))
    (if (i32.lt_s (local.get $newcap) (i32.const 16))
      (then (local.set $newcap (i32.const 16))))
    (call $ensure (i32.add (i32.const 4) (local.get $newcap)))
    global.get $heap local.set $new
    (i32.store (local.get $new) (local.get $need))
    (memory.copy (i32.add (local.get $new) (i32.const 4)) (i32.add (local.get $s) (i32.const 4)) (local.get $len))
    (memory.copy
      (i32.add (i32.add (local.get $new) (i32.const 4)) (local.get $len))
      (i32.add (local.get $piece) (i32.const 4))
      (local.get $plen))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (local.get $newcap)))
    local.get $new local.get $newcap)
"#;
const DICT_INSERT_CAP_WAT: &str = r#"  (func $dict_insert_cap (param $d i32) (param $k i64) (param $v i64) (param $mode i32) (param $cap i32) (result i32 i32)
    (local $count i32) (local $found i32) (local $new i32) (local $bytes i32) (local $newcap i32) (local $idx i32)
    (if (i32.eqz (local.get $cap))
      (then (global.set $__witchy_reowns (i64.add (global.get $__witchy_reowns) (i64.const 1)))))
    (local.set $count (i32.load (local.get $d)))
    (local.set $found (call $dict_find (local.get $d) (local.get $k) (local.get $mode)))
    (if (i32.and (i32.ge_s (local.get $found) (i32.const 0)) (i32.gt_s (local.get $cap) (i32.const 0)))
      (then
        (i64.store (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $found) (i32.const 16))) (local.get $v))
        local.get $d local.get $cap
        return))
    (if (i32.and (i32.lt_s (local.get $found) (i32.const 0)) (i32.gt_s (local.get $cap) (local.get $count)))
      (then
        (i64.store (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $count) (i32.const 16))) (local.get $k))
        (i64.store (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $count) (i32.const 16))) (local.get $v))
        (i32.store (local.get $d) (i32.add (local.get $count) (i32.const 1)))
        (local.set $idx (i32.load (i32.sub (local.get $d) (i32.const 4))))
        (if (i32.ne (local.get $idx) (i32.const 0))
          (then (call $dict_index_put (local.get $idx) (local.get $k) (local.get $mode) (local.get $count))))
        local.get $d local.get $cap
        return))
    (local.set $newcap (i32.mul (i32.add (local.get $count) (i32.const 1)) (i32.const 2)))
    (if (i32.lt_s (local.get $newcap) (i32.const 8))
      (then (local.set $newcap (i32.const 8))))
    (call $ensure (i32.add (i32.const 8) (i32.mul (local.get $newcap) (i32.const 16))))
    (local.set $new (i32.add (global.get $heap) (i32.const 4)))
    (i32.store (i32.sub (local.get $new) (i32.const 4)) (i32.const 0))
    (local.set $bytes (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 16))))
    (memory.copy (local.get $new) (local.get $d) (local.get $bytes))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $newcap) (i32.const 16))))
    (if (i32.ge_s (local.get $found) (i32.const 0))
      (then
        (i64.store (i32.add (i32.add (local.get $new) (i32.const 12)) (i32.mul (local.get $found) (i32.const 16))) (local.get $v)))
      (else
        (i64.store (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $count) (i32.const 16))) (local.get $k))
        (i64.store (i32.add (i32.add (local.get $new) (i32.const 12)) (i32.mul (local.get $count) (i32.const 16))) (local.get $v))
        (i32.store (local.get $new) (i32.add (local.get $count) (i32.const 1)))))
    (call $dict_index_build (local.get $new) (local.get $mode) (local.get $newcap))
    local.get $new local.get $newcap)
"#;
const DICT_UPDATE_CAP_WAT: &str = r#"  (func $dict_update_cap (param $d i32) (param $k i64) (param $default i64) (param $mode i32) (param $clos i32) (param $cap i32) (result i32 i32)
    (local $new i64)
    (local.set $new
      (call_indirect (type $clos1)
        (local.get $clos)
        (call $dict_get_or (local.get $d) (local.get $k) (local.get $default) (local.get $mode))
        (i32.load (local.get $clos))))
    (call $dict_insert_cap (local.get $d) (local.get $k) (local.get $new) (local.get $mode) (local.get $cap)))
"#;
const LIST_CONCAT_WAT: &str = r#"  (func $list_concat (param $a i32) (param $b i32) (result i32)
    (local $alen i32) (local $blen i32) (local $new i32)
    local.get $a i32.load local.set $alen
    local.get $b i32.load local.set $blen
    (call $ensure (i32.add (i32.const 4) (i32.mul (i32.add (local.get $alen) (local.get $blen)) (i32.const 8))))
    global.get $heap local.set $new
    local.get $new local.get $alen local.get $blen i32.add i32.store
    local.get $new i32.const 4 i32.add
    local.get $a i32.const 4 i32.add
    local.get $alen i32.const 8 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $alen i32.const 8 i32.mul i32.add
    local.get $b i32.const 4 i32.add
    local.get $blen i32.const 8 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $alen local.get $blen i32.add i32.const 8 i32.mul i32.add global.set $heap
    local.get $new)
"#;
const LIST_DROP_WAT: &str = r#"  (func $list_drop (param $list i32) (param $k i32) (result i32)
    (local $newlen i32) (local $new i32)
    local.get $list i32.load local.get $k i32.sub local.set $newlen
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $newlen) (i32.const 8))))
    global.get $heap local.set $new
    local.get $new local.get $newlen i32.store
    local.get $new i32.const 4 i32.add
    local.get $list i32.const 4 i32.add local.get $k i32.const 8 i32.mul i32.add
    local.get $newlen i32.const 8 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $newlen i32.const 8 i32.mul i32.add global.set $heap
    local.get $new)
"#;
const STARTS_WITH_WAT: &str = r#"  (func $starts_with (param $s i32) (param $p i32) (result i32)
    (local $plen i32) (local $i i32)
    (local.set $plen (i32.load (local.get $p)))
    (if (i32.gt_s (local.get $plen) (i32.load (local.get $s)))
      (then (return (i32.const 0))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $plen)))
        (if (i32.ne
              (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i)))
              (i32.load8_u (i32.add (i32.add (local.get $p) (i32.const 4)) (local.get $i))))
          (then (return (i32.const 0))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.const 1))
"#;
const ENDS_WITH_WAT: &str = r#"  (func $ends_with (param $s i32) (param $p i32) (result i32)
    (local $plen i32) (local $off i32) (local $i i32)
    (local.set $plen (i32.load (local.get $p)))
    (local.set $off (i32.sub (i32.load (local.get $s)) (local.get $plen)))
    (if (i32.lt_s (local.get $off) (i32.const 0))
      (then (return (i32.const 0))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $plen)))
        (if (i32.ne
              (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.add (local.get $off) (local.get $i))))
              (i32.load8_u (i32.add (i32.add (local.get $p) (i32.const 4)) (local.get $i))))
          (then (return (i32.const 0))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.const 1))
"#;
const SUBSTR_WAT: &str = r#"  (func $substr (param $src i32) (param $start i32) (param $len i32) (result i32)
    (local $res i32)
    (call $ensure (i32.add (i32.const 4) (local.get $len)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (memory.copy
      (i32.add (local.get $res) (i32.const 4))
      (i32.add (i32.add (local.get $src) (i32.const 4)) (local.get $start))
      (local.get $len))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const ASCII_CASE_WAT: &str = r#"  (func $ascii_case (param $s i32) (param $up i32) (result i32)
    (local $len i32) (local $i i32) (local $res i32) (local $b i32)
    (local.set $len (i32.load (local.get $s)))
    (call $ensure (i32.add (i32.const 4) (local.get $len)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (if (local.get $up)
          (then
            (if (i32.and (i32.ge_u (local.get $b) (i32.const 97)) (i32.le_u (local.get $b) (i32.const 122)))
              (then (local.set $b (i32.sub (local.get $b) (i32.const 32))))))
          (else
            (if (i32.and (i32.ge_u (local.get $b) (i32.const 65)) (i32.le_u (local.get $b) (i32.const 90)))
              (then (local.set $b (i32.add (local.get $b) (i32.const 32)))))))
        (i32.store8 (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $i)) (local.get $b))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const CRYPTO_SHA256_WAT: &str = r#"  (func $crypto_sha256 (param $in i32) (result i32)
    (local $res i32)
    (call $ensure (i32.const 68))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 64))
    (call $crypto_sha256_host (local.get $in) (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (local.get $res) (i32.const 68)))
    (local.get $res))
"#;
const CRYPTO_RUNE_HASH_WAT: &str = r#"  (func $crypto_rune_hash (param $paths i32) (param $contents i32) (result i32)
    (local $res i32)
    (call $ensure (i32.const 75))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 71))
    (call $crypto_rune_hash_host (local.get $paths) (local.get $contents) (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (local.get $res) (i32.const 75)))
    (local.get $res))
"#;
const FIELD_STR_GET_WAT: &str = r#"  (func $field_str_get (param $idx i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $field_str_len_host (local.get $idx)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const FIELD_INTLIST_GET_WAT: &str = r#"  (func $field_intlist_get (param $idx i32) (result i32)
    (local $size i32) (local $res i32)
    (local.set $size (call $field_intlist_len_host (local.get $idx)))
    (call $ensure (local.get $size))
    (local.set $res (global.get $heap))
    (call $fill_pending_host (local.get $res))
    (global.set $heap (i32.add (local.get $res) (local.get $size)))
    (local.get $res))
"#;
const FIELD_STRLIST_GET_WAT: &str = r#"  (func $field_strlist_get (param $idx i32) (result i32)
    (local $size i32) (local $res i32)
    (local.set $size (call $field_strlist_size_host (local.get $idx)))
    (call $ensure (local.get $size))
    (local.set $res (global.get $heap))
    (call $write_pending_list_host (local.get $res))
    (global.set $heap (i32.add (local.get $res) (local.get $size)))
    (local.get $res))
"#;
const COMPILER_FOOTPRINT_WAT: &str = r#"  (func $compiler_footprint (param $src i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $compiler_footprint_len_host (local.get $src)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const COMPILER_DIFF_WAT: &str = r#"  (func $compiler_diff (param $old i32) (param $new i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $compiler_diff_len_host (local.get $old) (local.get $new)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const FLOAT_TO_STR_WAT: &str = r#"  (func $float_to_str (param $x f64) (result i32)
    (local $res i32) (local $n i32)
    (call $ensure (i32.const 516))
    (local.set $res (global.get $heap))
    (local.set $n (call $float_to_str_host (local.get $x) (i32.add (local.get $res) (i32.const 4))))
    (i32.store (local.get $res) (local.get $n))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $n)))
    (local.get $res))
"#;
const ENCODING_WAT: &str = r#"  (func $encoding (param $op i32) (param $in i32) (result i32)
    (local $res i32) (local $n i32)
    (call $ensure (i32.add (i32.mul (i32.load (local.get $in)) (i32.const 2)) (i32.const 20)))
    (local.set $res (global.get $heap))
    (local.set $n (call $encoding_host (local.get $op) (local.get $in) (i32.add (local.get $res) (i32.const 4))))
    (i32.store (local.get $res) (local.get $n))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $n)))
    (local.get $res))
"#;
const GET_ENV_WAT: &str = r#"  (func $get_env (param $name i32) (result i32)
    (local $len i32) (local $str i32) (local $res i32)
    (local.set $len (call $env_len_host (local.get $name)))
    (if (i32.lt_s (local.get $len) (i32.const 0))
      (then
        (call $ensure (i32.const 4))
        (local.set $res (global.get $heap))
        (i32.store (local.get $res) (i32.const 1))
        (global.set $heap (i32.add (local.get $res) (i32.const 4)))
        (return (local.get $res))))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $str (global.get $heap))
    (i32.store (local.get $str) (local.get $len))
    (call $env_fill_host (local.get $name) (i32.add (local.get $str) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $str) (i32.const 4)) (local.get $len)))
    (call $ensure (i32.const 12))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 0))
    (i64.store (i32.add (local.get $res) (i32.const 4)) (i64.extend_i32_s (local.get $str)))
    (global.set $heap (i32.add (local.get $res) (i32.const 12)))
    (local.get $res))
"#;
const DIR_READ_WAT: &str = r#"  (func $dir_read (param $h i32) (param $rel i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $dir_read_len_host (local.get $h) (local.get $rel)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const BUILD_READ_WAT: &str = r#"  (func $build_read (param $h i32) (param $rel i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $build_read_len_host (local.get $h) (local.get $rel)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const DIR_LIST_WAT: &str = r#"  (func $dir_list (param $h i32) (result i32)
    (local $size i32) (local $res i32)
    (local.set $size (call $dir_list_size_host (local.get $h)))
    (call $ensure (local.get $size))
    (local.set $res (global.get $heap))
    (call $write_pending_list_host (local.get $res))
    (global.set $heap (i32.add (local.get $res) (local.get $size)))
    (local.get $res))
"#;
const NET_RECV_LINE_WAT: &str = r#"  (func $net_recv_line (param $s i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $net_recv_line_len_host (local.get $s)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const NET_RECV_ALL_WAT: &str = r#"  (func $net_recv_all (param $s i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $net_recv_all_len_host (local.get $s)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const NET_RECV_BYTES_WAT: &str = r#"  (func $net_recv_bytes (param $s i32) (param $n i64) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $net_recv_bytes_len_host (local.get $s) (local.get $n)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;
const BUILD_ARGS_WAT: &str = r#"  (func $build_args (result i32)
    (local $size i32) (local $res i32)
    (local.set $size (call $args_size_host))
    (call $ensure (local.get $size))
    (local.set $res (global.get $heap))
    (call $write_pending_list_host (local.get $res))
    (global.set $heap (i32.add (local.get $res) (local.get $size)))
    (local.get $res))
"#;
const CRYPTO_SIGN_WAT: &str = r#"  (func $crypto_sign (param $msg i32) (result i32)
    (local $res i32)
    (call $ensure (i32.const 132))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 128))
    (call $crypto_sign_host (local.get $msg) (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (local.get $res) (i32.const 132)))
    (local.get $res))
"#;
const CRYPTO_PUBLIC_KEY_WAT: &str = r#"  (func $crypto_public_key (result i32)
    (local $res i32)
    (call $ensure (i32.const 68))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 64))
    (call $crypto_public_key_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (local.get $res) (i32.const 68)))
    (local.get $res))
"#;
const FLOAT_ORD_WAT: &str = r#"  (func $f_nan_guard (param $a f64) (param $b f64)
    (if (i32.or (f64.ne (local.get $a) (local.get $a)) (f64.ne (local.get $b) (local.get $b)))
      (then (unreachable))))
  (func $f_lt (param $a f64) (param $b f64) (result i32)
    (call $f_nan_guard (local.get $a) (local.get $b))
    (f64.lt (local.get $a) (local.get $b)))
  (func $f_le (param $a f64) (param $b f64) (result i32)
    (call $f_nan_guard (local.get $a) (local.get $b))
    (f64.le (local.get $a) (local.get $b)))
  (func $f_gt (param $a f64) (param $b f64) (result i32)
    (call $f_nan_guard (local.get $a) (local.get $b))
    (f64.gt (local.get $a) (local.get $b)))
  (func $f_ge (param $a f64) (param $b f64) (result i32)
    (call $f_nan_guard (local.get $a) (local.get $b))
    (f64.ge (local.get $a) (local.get $b)))
"#;
const SPLIT_WAT: &str = r#"  (func $split (param $s i32) (param $sep i32) (result i32)
    (local $slen i32) (local $seplen i32) (local $result i32)
    (local $start i32) (local $i i32) (local $j i32) (local $match i32)
    (local.set $slen (i32.load (local.get $s)))
    (local.set $seplen (i32.load (local.get $sep)))
    (call $ensure (i32.const 4))
    (local.set $result (global.get $heap))
    (i32.store (local.get $result) (i32.const 0))
    (global.set $heap (i32.add (local.get $result) (i32.const 4)))
    (if (i32.eqz (local.get $seplen))
      (then (return (call $list_push (local.get $result) (i64.extend_i32_u (local.get $s))))))
    (local.set $start (i32.const 0))
    (local.set $i (i32.const 0))
    (block $done
      (loop $scan
        (br_if $done (i32.gt_s (local.get $i) (i32.sub (local.get $slen) (local.get $seplen))))
        (local.set $match (i32.const 1))
        (local.set $j (i32.const 0))
        (block $cmpdone
          (loop $cmp
            (br_if $cmpdone (i32.ge_s (local.get $j) (local.get $seplen)))
            (if (i32.ne
                  (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.add (local.get $i) (local.get $j))))
                  (i32.load8_u (i32.add (i32.add (local.get $sep) (i32.const 4)) (local.get $j))))
              (then (local.set $match (i32.const 0)) (br $cmpdone)))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $cmp)))
        (if (local.get $match)
          (then
            (local.set $result
              (call $list_push (local.get $result)
                (i64.extend_i32_u (call $substr (local.get $s) (local.get $start) (i32.sub (local.get $i) (local.get $start))))))
            (local.set $i (i32.add (local.get $i) (local.get $seplen)))
            (local.set $start (local.get $i)))
          (else
            (local.set $i (i32.add (local.get $i) (i32.const 1)))))
        (br $scan)))
    (local.set $result
      (call $list_push (local.get $result)
        (i64.extend_i32_u (call $substr (local.get $s) (local.get $start) (i32.sub (local.get $slen) (local.get $start))))))
    (local.get $result))
"#;
const STR_CHARS_WAT: &str = r#"  (func $str_chars (param $s i32) (result i32)
    (local $n i32) (local $i i32) (local $result i32)
    (local.set $n (call $byte_to_char (local.get $s) (i32.load (local.get $s))))
    (call $ensure (i32.const 4))
    (local.set $result (global.get $heap))
    (i32.store (local.get $result) (i32.const 0))
    (global.set $heap (i32.add (local.get $result) (i32.const 4)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
        (local.set $result
          (call $list_push (local.get $result)
            (i64.extend_i32_u (call $str_substring (local.get $s) (local.get $i) (i32.add (local.get $i) (i32.const 1))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $result))
"#;
const FIND_BYTE_WAT: &str = r#"  (func $find_byte (param $s i32) (param $sub i32) (result i32)
    (local $slen i32) (local $sublen i32) (local $i i32) (local $j i32) (local $match i32)
    (local.set $slen (i32.load (local.get $s)))
    (local.set $sublen (i32.load (local.get $sub)))
    (if (i32.eqz (local.get $sublen)) (then (return (i32.const 0))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $scan
        (br_if $done (i32.gt_s (local.get $i) (i32.sub (local.get $slen) (local.get $sublen))))
        (local.set $match (i32.const 1))
        (local.set $j (i32.const 0))
        (block $cmpdone
          (loop $cmp
            (br_if $cmpdone (i32.ge_s (local.get $j) (local.get $sublen)))
            (if (i32.ne
                  (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.add (local.get $i) (local.get $j))))
                  (i32.load8_u (i32.add (i32.add (local.get $sub) (i32.const 4)) (local.get $j))))
              (then (local.set $match (i32.const 0)) (br $cmpdone)))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $cmp)))
        (if (local.get $match) (then (return (local.get $i))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $scan)))
    (i32.const -1))
"#;
const BYTE_TO_CHAR_WAT: &str = r#"  (func $byte_to_char (param $s i32) (param $bytelen i32) (result i32)
    (local $i i32) (local $count i32) (local $b i32)
    (local.set $i (i32.const 0))
    (local.set $count (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $bytelen)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (if (i32.ne (i32.and (local.get $b) (i32.const 0xc0)) (i32.const 0x80))
          (then (local.set $count (i32.add (local.get $count) (i32.const 1)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $count))
"#;
const STR_INDEX_OF_WAT: &str = r#"  (func $str_index_of (param $s i32) (param $sub i32) (result i32)
    (local $b i32)
    (local.set $b (call $find_byte (local.get $s) (local.get $sub)))
    (if (result i32) (i32.lt_s (local.get $b) (i32.const 0))
      (then (i32.const -1))
      (else (call $byte_to_char (local.get $s) (local.get $b)))))
"#;
const CHAR_TO_BYTE_WAT: &str = r#"  (func $char_to_byte (param $s i32) (param $n i32) (result i32)
    (local $slen i32) (local $i i32) (local $count i32) (local $b i32)
    (local.set $slen (i32.load (local.get $s)))
    (local.set $i (i32.const 0))
    (local.set $count (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $slen)))
        (br_if $done (i32.ge_s (local.get $count) (local.get $n)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (local.set $i (i32.add (local.get $i)
          (if (result i32) (i32.lt_u (local.get $b) (i32.const 0x80)) (then (i32.const 1))
            (else (if (result i32) (i32.lt_u (local.get $b) (i32.const 0xe0)) (then (i32.const 2))
              (else (if (result i32) (i32.lt_u (local.get $b) (i32.const 0xf0)) (then (i32.const 3))
                (else (i32.const 4)))))))))
        (local.set $count (i32.add (local.get $count) (i32.const 1)))
        (br $l)))
    (local.get $i))
"#;
const STR_SUBSTRING_WAT: &str = r#"  (func $str_substring (param $s i32) (param $start i32) (param $end i32) (result i32)
    (local $lo i32) (local $hi i32)
    (local.set $lo (call $char_to_byte (local.get $s) (local.get $start)))
    (local.set $hi (call $char_to_byte (local.get $s) (local.get $end)))
    (if (result i32) (i32.ge_s (local.get $lo) (local.get $hi))
      (then (call $substr (local.get $s) (i32.const 0) (i32.const 0)))
      (else (call $substr (local.get $s) (local.get $lo) (i32.sub (local.get $hi) (local.get $lo))))))
"#;
const MATCH_AT_WAT: &str = r#"  (func $match_at (param $s i32) (param $from i32) (param $pos i32) (result i32)
    (local $flen i32) (local $j i32)
    (local.set $flen (i32.load (local.get $from)))
    (if (i32.gt_s (i32.add (local.get $pos) (local.get $flen)) (i32.load (local.get $s)))
      (then (return (i32.const 0))))
    (local.set $j (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $j) (local.get $flen)))
        (if (i32.ne
              (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.add (local.get $pos) (local.get $j))))
              (i32.load8_u (i32.add (i32.add (local.get $from) (i32.const 4)) (local.get $j))))
          (then (return (i32.const 0))))
        (local.set $j (i32.add (local.get $j) (i32.const 1)))
        (br $l)))
    (i32.const 1))
"#;
const REPLACE_WAT: &str = r#"  (func $replace (param $s i32) (param $from i32) (param $to i32) (result i32)
    (local $slen i32) (local $flen i32) (local $tlen i32) (local $cnt i32)
    (local $src i32) (local $dst i32) (local $res i32) (local $reslen i32) (local $b i32) (local $clen i32)
    (local.set $slen (i32.load (local.get $s)))
    (local.set $flen (i32.load (local.get $from)))
    (local.set $tlen (i32.load (local.get $to)))
    (call $ensure (i32.add (i32.add (i32.const 4) (local.get $slen))
      (i32.mul (i32.add (local.get $slen) (i32.const 1)) (local.get $tlen))))
    (if (i32.eqz (local.get $flen))
      (then
        (local.set $res (global.get $heap))
        (local.set $dst (i32.add (local.get $res) (i32.const 4)))
        (memory.copy (local.get $dst) (i32.add (local.get $to) (i32.const 4)) (local.get $tlen))
        (local.set $dst (i32.add (local.get $dst) (local.get $tlen)))
        (local.set $src (i32.const 0))
        (block $cdone
          (loop $cl
            (br_if $cdone (i32.ge_s (local.get $src) (local.get $slen)))
            (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $src))))
            (local.set $clen
              (if (result i32) (i32.lt_u (local.get $b) (i32.const 0x80)) (then (i32.const 1))
                (else (if (result i32) (i32.lt_u (local.get $b) (i32.const 0xe0)) (then (i32.const 2))
                  (else (if (result i32) (i32.lt_u (local.get $b) (i32.const 0xf0)) (then (i32.const 3))
                    (else (i32.const 4))))))))
            (memory.copy (local.get $dst) (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $src)) (local.get $clen))
            (local.set $dst (i32.add (local.get $dst) (local.get $clen)))
            (memory.copy (local.get $dst) (i32.add (local.get $to) (i32.const 4)) (local.get $tlen))
            (local.set $dst (i32.add (local.get $dst) (local.get $tlen)))
            (local.set $src (i32.add (local.get $src) (local.get $clen)))
            (br $cl)))
        (local.set $reslen (i32.sub (local.get $dst) (i32.add (local.get $res) (i32.const 4))))
        (i32.store (local.get $res) (local.get $reslen))
        (global.set $heap (local.get $dst))
        (return (local.get $res))))
    (local.set $cnt (i32.const 0))
    (local.set $src (i32.const 0))
    (block $countdone
      (loop $cl2
        (br_if $countdone (i32.gt_s (i32.add (local.get $src) (local.get $flen)) (local.get $slen)))
        (if (call $match_at (local.get $s) (local.get $from) (local.get $src))
          (then
            (local.set $cnt (i32.add (local.get $cnt) (i32.const 1)))
            (local.set $src (i32.add (local.get $src) (local.get $flen))))
          (else
            (local.set $src (i32.add (local.get $src) (i32.const 1)))))
        (br $cl2)))
    (local.set $reslen (i32.add (local.get $slen) (i32.mul (local.get $cnt) (i32.sub (local.get $tlen) (local.get $flen)))))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $reslen))
    (local.set $dst (i32.add (local.get $res) (i32.const 4)))
    (local.set $src (i32.const 0))
    (block $filldone
      (loop $fl
        (br_if $filldone (i32.ge_s (local.get $src) (local.get $slen)))
        (if (call $match_at (local.get $s) (local.get $from) (local.get $src))
          (then
            (memory.copy (local.get $dst) (i32.add (local.get $to) (i32.const 4)) (local.get $tlen))
            (local.set $dst (i32.add (local.get $dst) (local.get $tlen)))
            (local.set $src (i32.add (local.get $src) (local.get $flen))))
          (else
            (i32.store8 (local.get $dst) (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $src))))
            (local.set $dst (i32.add (local.get $dst) (i32.const 1)))
            (local.set $src (i32.add (local.get $src) (i32.const 1)))))
        (br $fl)))
    (global.set $heap (local.get $dst))
    (local.get $res))
"#;
const STR_TO_INT_WAT: &str = r#"  (func $str_to_int (param $s i32) (result i64)
    (local $len i32) (local $i i32) (local $b i32) (local $acc i64) (local $neg i32) (local $got i32) (local $limit i64)
    (local.set $len (i32.load (local.get $s)))
    (local.set $i (i32.const 0))
    (local.set $acc (i64.const 0))
    (local.set $neg (i32.const 0))
    (local.set $got (i32.const 0))
    (block $wsdone
      (loop $ws
        (br_if $wsdone (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (br_if $wsdone (i32.eqz (i32.or
          (i32.eq (local.get $b) (i32.const 32))
          (i32.or (i32.eq (local.get $b) (i32.const 9))
          (i32.or (i32.eq (local.get $b) (i32.const 10))
          (i32.or (i32.eq (local.get $b) (i32.const 13))
          (i32.or (i32.eq (local.get $b) (i32.const 11))
                  (i32.eq (local.get $b) (i32.const 12)))))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $ws)))
    (if (i32.lt_s (local.get $i) (local.get $len))
      (then
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (if (i32.eq (local.get $b) (i32.const 45))
          (then (local.set $neg (i32.const 1)) (local.set $i (i32.add (local.get $i) (i32.const 1))))
          (else (if (i32.eq (local.get $b) (i32.const 43))
            (then (local.set $i (i32.add (local.get $i) (i32.const 1)))))))))
    ;; Magnitude bound (unsigned): 2^63 for a negative value (|i64::MIN|), else
    ;; 2^63 - 1 (i64::MAX). The digit loop traps past it, matching Rust's checked
    ;; parse rather than silently wrapping.
    (local.set $limit (if (result i64) (local.get $neg)
      (then (i64.const -9223372036854775808))
      (else (i64.const 9223372036854775807))))
    (block $digdone
      (loop $dig
        (br_if $digdone (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (br_if $digdone (i32.or
          (i32.lt_u (local.get $b) (i32.const 48))
          (i32.gt_u (local.get $b) (i32.const 57))))
        ;; Overflow if acc*10 + d would exceed `limit` (unsigned magnitude), i.e.
        ;; acc > (limit - d) / 10.
        (if (i64.gt_u (local.get $acc)
              (i64.div_u
                (i64.sub (local.get $limit) (i64.extend_i32_u (i32.sub (local.get $b) (i32.const 48))))
                (i64.const 10)))
          (then (unreachable)))
        (local.set $acc (i64.add (i64.mul (local.get $acc) (i64.const 10))
          (i64.extend_i32_u (i32.sub (local.get $b) (i32.const 48)))))
        (local.set $got (i32.const 1))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $dig)))
    (block $twsdone
      (loop $tws
        (br_if $twsdone (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (br_if $twsdone (i32.eqz (i32.or
          (i32.eq (local.get $b) (i32.const 32))
          (i32.or (i32.eq (local.get $b) (i32.const 9))
          (i32.or (i32.eq (local.get $b) (i32.const 10))
          (i32.or (i32.eq (local.get $b) (i32.const 13))
          (i32.or (i32.eq (local.get $b) (i32.const 11))
                  (i32.eq (local.get $b) (i32.const 12)))))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $tws)))
    (if (i32.or (i32.eqz (local.get $got)) (i32.lt_s (local.get $i) (local.get $len)))
      (then (unreachable)))
    (if (result i64) (local.get $neg)
      (then (i64.sub (i64.const 0) (local.get $acc)))
      (else (local.get $acc))))
"#;
const IS_WS_WAT: &str = r#"  (func $is_ws (param $b i32) (result i32)
    (i32.or
      (i32.eq (local.get $b) (i32.const 32))
      (i32.or (i32.eq (local.get $b) (i32.const 9))
      (i32.or (i32.eq (local.get $b) (i32.const 10))
      (i32.or (i32.eq (local.get $b) (i32.const 13))
      (i32.or (i32.eq (local.get $b) (i32.const 11))
              (i32.eq (local.get $b) (i32.const 12))))))))
"#;
const TRIM_WAT: &str = r#"  (func $trim (param $s i32) (result i32)
    (local $len i32) (local $lo i32) (local $hi i32)
    (local.set $len (i32.load (local.get $s)))
    (local.set $lo (i32.const 0))
    (local.set $hi (local.get $len))
    (block $lodone
      (loop $l
        (br_if $lodone (i32.ge_s (local.get $lo) (local.get $hi)))
        (br_if $lodone (i32.eqz (call $is_ws
          (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $lo))))))
        (local.set $lo (i32.add (local.get $lo) (i32.const 1)))
        (br $l)))
    (block $hidone
      (loop $h
        (br_if $hidone (i32.le_s (local.get $hi) (local.get $lo)))
        (br_if $hidone (i32.eqz (call $is_ws
          (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.sub (local.get $hi) (i32.const 1)))))))
        (local.set $hi (i32.sub (local.get $hi) (i32.const 1)))
        (br $h)))
    (call $substr (local.get $s) (local.get $lo) (i32.sub (local.get $hi) (local.get $lo))))
"#;
const DICT_NEW_WAT: &str = r#"  (func $dict_new (result i32)
    (local $p i32)
    (call $ensure (i32.const 8))
    (local.set $p (i32.add (global.get $heap) (i32.const 4)))
    (i32.store (i32.sub (local.get $p) (i32.const 4)) (i32.const 0))
    (i32.store (local.get $p) (i32.const 0))
    (global.set $heap (i32.add (local.get $p) (i32.const 4)))
    (local.get $p))
"#;
const DICT_HASH_WAT: &str = r#"  (func $dict_hash (param $k i64) (param $mode i32) (result i32)
    (local $x i64) (local $p i32) (local $len i32) (local $i i32) (local $h i32)
    (if (i32.eqz (local.get $mode))
      (then
        (local.set $x (local.get $k))
        (local.set $x (i64.xor (local.get $x) (i64.shr_u (local.get $x) (i64.const 33))))
        (local.set $x (i64.mul (local.get $x) (i64.const -49064778989728563)))
        (local.set $x (i64.xor (local.get $x) (i64.shr_u (local.get $x) (i64.const 33))))
        (return (i32.wrap_i64 (local.get $x)))))
    (local.set $p (i32.wrap_i64 (local.get $k)))
    (local.set $len (i32.load (local.get $p)))
    (local.set $h (i32.const -2128831035))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $h (i32.xor (local.get $h) (i32.load8_u (i32.add (i32.add (local.get $p) (i32.const 4)) (local.get $i)))))
        (local.set $h (i32.mul (local.get $h) (i32.const 16777619)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $h))
"#;
const DICT_FIND_WAT: &str = r#"  (func $dict_find (param $d i32) (param $k i64) (param $mode i32) (result i32)
    (local $idx i32) (local $count i32) (local $i i32) (local $slots i32) (local $h i32) (local $e i32)
    (local.set $idx (i32.load (i32.sub (local.get $d) (i32.const 4))))
    (if (i32.eqz (local.get $idx))
      (then
        (local.set $count (i32.load (local.get $d)))
        (local.set $i (i32.const 0))
        (block $done
          (loop $l
            (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
            (if (call $key_eq
                  (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16))))
                  (local.get $k) (local.get $mode))
              (then (return (local.get $i))))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br $l)))
        (return (i32.const -1))))
    (local.set $slots (i32.load (local.get $idx)))
    (local.set $h (i32.and (call $dict_hash (local.get $k) (local.get $mode)) (i32.sub (local.get $slots) (i32.const 1))))
    (block $miss
      (loop $p
        (local.set $e (i32.load (i32.add (i32.add (local.get $idx) (i32.const 4)) (i32.mul (local.get $h) (i32.const 4)))))
        (br_if $miss (i32.eqz (local.get $e)))
        (if (call $key_eq
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (i32.sub (local.get $e) (i32.const 1)) (i32.const 16))))
              (local.get $k) (local.get $mode))
          (then (return (i32.sub (local.get $e) (i32.const 1)))))
        (local.set $h (i32.and (i32.add (local.get $h) (i32.const 1)) (i32.sub (local.get $slots) (i32.const 1))))
        (br $p)))
    (i32.const -1))
"#;
const DICT_INDEX_PUT_WAT: &str = r#"  (func $dict_index_put (param $idx i32) (param $k i64) (param $mode i32) (param $entry i32)
    (local $slots i32) (local $h i32)
    (local.set $slots (i32.load (local.get $idx)))
    (local.set $h (i32.and (call $dict_hash (local.get $k) (local.get $mode)) (i32.sub (local.get $slots) (i32.const 1))))
    (block $done
      (loop $p
        (br_if $done (i32.eqz (i32.load (i32.add (i32.add (local.get $idx) (i32.const 4)) (i32.mul (local.get $h) (i32.const 4))))))
        (local.set $h (i32.and (i32.add (local.get $h) (i32.const 1)) (i32.sub (local.get $slots) (i32.const 1))))
        (br $p)))
    (i32.store (i32.add (i32.add (local.get $idx) (i32.const 4)) (i32.mul (local.get $h) (i32.const 4))) (i32.add (local.get $entry) (i32.const 1))))
"#;
const DICT_INDEX_BUILD_WAT: &str = r#"  (func $dict_index_build (param $d i32) (param $mode i32) (param $cap i32)
    (local $slots i32) (local $idx i32) (local $count i32) (local $i i32)
    (local.set $slots (i32.const 8))
    (block $sz
      (loop $g
        (br_if $sz (i32.ge_s (local.get $slots) (i32.mul (local.get $cap) (i32.const 2))))
        (local.set $slots (i32.mul (local.get $slots) (i32.const 2)))
        (br $g)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $slots) (i32.const 4))))
    (local.set $idx (global.get $heap))
    (global.set $heap (i32.add (i32.add (local.get $idx) (i32.const 4)) (i32.mul (local.get $slots) (i32.const 4))))
    (i32.store (local.get $idx) (local.get $slots))
    (memory.fill (i32.add (local.get $idx) (i32.const 4)) (i32.const 0) (i32.mul (local.get $slots) (i32.const 4)))
    (local.set $count (i32.load (local.get $d)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (call $dict_index_put (local.get $idx)
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16))))
          (local.get $mode) (local.get $i))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.store (i32.sub (local.get $d) (i32.const 4)) (local.get $idx)))
"#;
const KEY_EQ_WAT: &str = r#"  (func $key_eq (param $a i64) (param $b i64) (param $mode i32) (result i32)
    (if (result i32) (i32.eqz (local.get $mode))
      (then (i64.eq (local.get $a) (local.get $b)))
      (else (if (result i32) (i32.eq (local.get $mode) (i32.const 1))
        (then (call $str_eq (i32.wrap_i64 (local.get $a)) (i32.wrap_i64 (local.get $b))))
        (else (f64.eq (f64.reinterpret_i64 (local.get $a)) (f64.reinterpret_i64 (local.get $b))))))))
"#;
const DICT_INSERT_WAT: &str = r#"  (func $dict_insert (param $d i32) (param $k i64) (param $v i64) (param $mode i32) (result i32)
    (local $count i32) (local $found i32) (local $new i32) (local $bytes i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 24) (i32.mul (local.get $count) (i32.const 16))))
    (local.set $found (call $dict_find (local.get $d) (local.get $k) (local.get $mode)))
    (local.set $bytes (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 16))))
    (local.set $new (i32.add (global.get $heap) (i32.const 4)))
    (i32.store (i32.sub (local.get $new) (i32.const 4)) (i32.const 0))
    (memory.copy (local.get $new) (local.get $d) (local.get $bytes))
    (if (result i32) (i32.ge_s (local.get $found) (i32.const 0))
      (then
        (i64.store (i32.add (i32.add (local.get $new) (i32.const 12)) (i32.mul (local.get $found) (i32.const 16))) (local.get $v))
        (global.set $heap (i32.add (local.get $new) (local.get $bytes)))
        (local.get $new))
      (else
        (i32.store (local.get $new) (i32.add (local.get $count) (i32.const 1)))
        (i64.store (i32.add (local.get $new) (local.get $bytes)) (local.get $k))
        (i64.store (i32.add (i32.add (local.get $new) (local.get $bytes)) (i32.const 8)) (local.get $v))
        (global.set $heap (i32.add (i32.add (local.get $new) (local.get $bytes)) (i32.const 16)))
        (local.get $new))))
"#;
const DICT_GET_OR_WAT: &str = r#"  (func $dict_get_or (param $d i32) (param $k i64) (param $default i64) (param $mode i32) (result i64)
    (local $found i32)
    (local.set $found (call $dict_find (local.get $d) (local.get $k) (local.get $mode)))
    (if (i32.lt_s (local.get $found) (i32.const 0))
      (then (return (local.get $default))))
    (i64.load (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $found) (i32.const 16)))))
"#;
const DICT_HAS_WAT: &str = r#"  (func $dict_has (param $d i32) (param $k i64) (param $mode i32) (result i32)
    (i32.ge_s (call $dict_find (local.get $d) (local.get $k) (local.get $mode)) (i32.const 0)))
"#;
const DICT_REMOVE_WAT: &str = r#"  (func $dict_remove (param $d i32) (param $k i64) (param $mode i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32) (local $n i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 8) (i32.mul (local.get $count) (i32.const 16))))
    (local.set $new (i32.add (global.get $heap) (i32.const 4)))
    (i32.store (i32.sub (local.get $new) (i32.const 4)) (i32.const 0))
    (local.set $i (i32.const 0))
    (local.set $n (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (if (i32.eqz (call $key_eq
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16))))
              (local.get $k) (local.get $mode)))
          (then
            (i64.store (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $n) (i32.const 16)))
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))))
            (i64.store (i32.add (i32.add (local.get $new) (i32.const 12)) (i32.mul (local.get $n) (i32.const 16)))
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))))
            (local.set $n (i32.add (local.get $n) (i32.const 1)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.store (local.get $new) (local.get $n))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $n) (i32.const 16))))
    (local.get $new))
"#;
const DICT_UPDATE_WAT: &str = r#"  (func $dict_update (param $d i32) (param $k i64) (param $default i64) (param $mode i32) (param $clos i32) (result i32)
    (local $new i64)
    (local.set $new
      (call_indirect (type $clos1)
        (local.get $clos)
        (call $dict_get_or (local.get $d) (local.get $k) (local.get $default) (local.get $mode))
        (i32.load (local.get $clos))))
    (call $dict_insert (local.get $d) (local.get $k) (local.get $new) (local.get $mode)))
"#;
const DICT_KEYS_WAT: &str = r#"  (func $dict_keys (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $new (global.get $heap))
    (i32.store (local.get $new) (local.get $count))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (i64.store
          (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $count) (i32.const 8))))
    (local.get $new))
"#;
const DICT_VALUES_WAT: &str = r#"  (func $dict_values (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $new (global.get $heap))
    (i32.store (local.get $new) (local.get $count))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (i64.store
          (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $count) (i32.const 8))))
    (local.get $new))
"#;
const DICT_PAIRS_WAT: &str = r#"  (func $dict_pairs (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $list i32) (local $tup i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 8))) (i32.mul (local.get $count) (i32.const 20))))
    (local.set $list (global.get $heap))
    (i32.store (local.get $list) (local.get $count))
    (global.set $heap (i32.add (i32.add (local.get $list) (i32.const 4)) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (local.set $tup (global.get $heap))
        (i32.store (local.get $tup) (i32.const 0))
        (i64.store (i32.add (local.get $tup) (i32.const 4))
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))))
        (i64.store (i32.add (local.get $tup) (i32.const 12))
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))))
        (global.set $heap (i32.add (local.get $tup) (i32.const 20)))
        (i64.store
          (i32.add (i32.add (local.get $list) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))
          (i64.extend_i32_u (local.get $tup)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $list))
"#;
const PRINT_STR_WAT: &str = r#"  (func $print_str (param $s i32)
    local.get $s i32.const 4 i32.add
    local.get $s i32.load
    call $print)
"#;
const INT_TO_STRING_WAT: &str = r#"  (func $int_to_string (param $n i64) (result i32)
    (local $mag i64) (local $t i64) (local $ndigits i32) (local $len i32) (local $res i32) (local $p i32) (local $neg i32)
    (call $ensure (i32.const 28))
    (if (result i32) (i64.eqz (local.get $n))
      (then
        (local.set $res (global.get $heap))
        (i32.store (local.get $res) (i32.const 1))
        (i32.store8 (i32.add (local.get $res) (i32.const 4)) (i32.const 48))
        (global.set $heap (i32.add (local.get $res) (i32.const 5)))
        (local.get $res))
      (else
        (local.set $neg (i64.lt_s (local.get $n) (i64.const 0)))
        (local.set $mag
          (if (result i64) (local.get $neg)
            (then (i64.sub (i64.const 0) (local.get $n)))
            (else (local.get $n))))
        (local.set $ndigits (i32.const 0))
        (local.set $t (local.get $mag))
        (block $b1
          (loop $l1
            (br_if $b1 (i64.eqz (local.get $t)))
            (local.set $ndigits (i32.add (local.get $ndigits) (i32.const 1)))
            (local.set $t (i64.div_u (local.get $t) (i64.const 10)))
            (br $l1)))
        (local.set $len (i32.add (local.get $ndigits) (local.get $neg)))
        (local.set $res (global.get $heap))
        (i32.store (local.get $res) (local.get $len))
        (if (local.get $neg)
          (then (i32.store8 (i32.add (local.get $res) (i32.const 4)) (i32.const 45))))
        (local.set $p (i32.sub (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)) (i32.const 1)))
        (local.set $t (local.get $mag))
        (block $b2
          (loop $l2
            (br_if $b2 (i64.eqz (local.get $t)))
            (i32.store8 (local.get $p) (i32.add (i32.wrap_i64 (i64.rem_u (local.get $t) (i64.const 10))) (i32.const 48)))
            (local.set $p (i32.sub (local.get $p) (i32.const 1)))
            (local.set $t (i64.div_u (local.get $t) (i64.const 10)))
            (br $l2)))
        (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
        (local.get $res))))
"#;
const STR_EQ_WAT: &str = r#"  (func $str_eq (param $a i32) (param $b i32) (result i32)
    (local $len i32) (local $i i32)
    (if (i32.eq (local.get $a) (local.get $b)) (then (return (i32.const 1))))
    (if (i32.ne (i32.load (local.get $a)) (i32.load (local.get $b)))
      (then (return (i32.const 0))))
    (local.set $len (i32.load (local.get $a)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $len)))
        (if (i32.ne
              (i32.load8_u (i32.add (i32.add (local.get $a) (i32.const 4)) (local.get $i)))
              (i32.load8_u (i32.add (i32.add (local.get $b) (i32.const 4)) (local.get $i))))
          (then (return (i32.const 0))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.const 1))
"#;
const STR_CMP_WAT: &str = r#"  (func $str_cmp (param $a i32) (param $b i32) (result i32)
    (local $alen i32) (local $blen i32) (local $n i32) (local $i i32) (local $ca i32) (local $cb i32)
    (local.set $alen (i32.load (local.get $a)))
    (local.set $blen (i32.load (local.get $b)))
    (local.set $n (select (local.get $alen) (local.get $blen) (i32.lt_s (local.get $alen) (local.get $blen))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
        (local.set $ca (i32.load8_u (i32.add (i32.add (local.get $a) (i32.const 4)) (local.get $i))))
        (local.set $cb (i32.load8_u (i32.add (i32.add (local.get $b) (i32.const 4)) (local.get $i))))
        (if (i32.ne (local.get $ca) (local.get $cb))
          (then (return (i32.sub (local.get $ca) (local.get $cb)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.sub (local.get $alen) (local.get $blen)))
"#;

/// The static helper WAT consts, in canonical prelude order (mirrored
/// verbatim from `src/codegen.rs`).
const HELPER_WATS: &[&str] = &[
    ENSURE_WAT,
    CONCAT_WAT,
    LIST_AT_WAT,
    LIST_PUSH_WAT,
    LIST_PUSH_CAP_WAT,
    STR_APPEND_CAP_WAT,
    DICT_INSERT_CAP_WAT,
    DICT_UPDATE_CAP_WAT,
    LIST_CONCAT_WAT,
    LIST_DROP_WAT,
    STARTS_WITH_WAT,
    ENDS_WITH_WAT,
    SUBSTR_WAT,
    ASCII_CASE_WAT,
    CRYPTO_SHA256_WAT,
    CRYPTO_RUNE_HASH_WAT,
    FIELD_STR_GET_WAT,
    FIELD_INTLIST_GET_WAT,
    FIELD_STRLIST_GET_WAT,
    COMPILER_FOOTPRINT_WAT,
    COMPILER_DIFF_WAT,
    FLOAT_TO_STR_WAT,
    ENCODING_WAT,
    GET_ENV_WAT,
    DIR_READ_WAT,
    BUILD_READ_WAT,
    DIR_LIST_WAT,
    NET_RECV_LINE_WAT,
    NET_RECV_ALL_WAT,
    NET_RECV_BYTES_WAT,
    BUILD_ARGS_WAT,
    CRYPTO_SIGN_WAT,
    CRYPTO_PUBLIC_KEY_WAT,
    FLOAT_ORD_WAT,
    SPLIT_WAT,
    STR_CHARS_WAT,
    FIND_BYTE_WAT,
    BYTE_TO_CHAR_WAT,
    STR_INDEX_OF_WAT,
    CHAR_TO_BYTE_WAT,
    STR_SUBSTRING_WAT,
    MATCH_AT_WAT,
    REPLACE_WAT,
    STR_TO_INT_WAT,
    IS_WS_WAT,
    TRIM_WAT,
    DICT_NEW_WAT,
    DICT_HASH_WAT,
    DICT_FIND_WAT,
    DICT_INDEX_PUT_WAT,
    DICT_INDEX_BUILD_WAT,
    KEY_EQ_WAT,
    DICT_INSERT_WAT,
    DICT_GET_OR_WAT,
    DICT_HAS_WAT,
    DICT_REMOVE_WAT,
    DICT_UPDATE_WAT,
    DICT_KEYS_WAT,
    DICT_VALUES_WAT,
    DICT_PAIRS_WAT,
    PRINT_STR_WAT,
    INT_TO_STRING_WAT,
    STR_EQ_WAT,
    STR_CMP_WAT,
];

/// Function names for the static helpers above, same order.
const HELPER_NAMES: &[&str] = &[
    "ensure",
    "concat",
    "list_at",
    "list_push",
    "list_push_cap",
    "str_append_cap",
    "dict_insert_cap",
    "dict_update_cap",
    "list_concat",
    "list_drop",
    "starts_with",
    "ends_with",
    "substr",
    "ascii_case",
    "crypto_sha256",
    "crypto_rune_hash",
    "field_str_get",
    "field_intlist_get",
    "field_strlist_get",
    "compiler_footprint",
    "compiler_diff",
    "float_to_str",
    "encoding",
    "get_env",
    "dir_read",
    "build_read",
    "dir_list",
    "net_recv_line",
    "net_recv_all",
    "net_recv_bytes",
    "build_args",
    "crypto_sign",
    "crypto_public_key",
    "f_nan_guard",
    "f_lt",
    "f_le",
    "f_gt",
    "f_ge",
    "split",
    "str_chars",
    "find_byte",
    "byte_to_char",
    "str_index_of",
    "char_to_byte",
    "str_substring",
    "match_at",
    "replace",
    "str_to_int",
    "is_ws",
    "trim",
    "dict_new",
    "dict_hash",
    "dict_find",
    "dict_index_put",
    "dict_index_build",
    "key_eq",
    "dict_insert",
    "dict_get_or",
    "dict_has",
    "dict_remove",
    "dict_update",
    "dict_keys",
    "dict_values",
    "dict_pairs",
    "print_str",
    "int_to_string",
    "str_eq",
    "str_cmp",
];

/// The closure call-signature arities pre-declared in the prelude type section
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
  (import "witchy" "crypto.sha512" (func $crypto_sha512_host (param i32 i32)))
  (import "witchy" "crypto.sha3_256" (func $crypto_sha3_256_host (param i32 i32)))
  (import "witchy" "crypto.hmac_sha256" (func $crypto_hmac_sha256_host (param i32 i32 i32)))
  (import "witchy" "print_int" (func $print_int (param i64)))
  (import "witchy" "print_float" (func $print_float (param f64)))
  (import "witchy" "string_from_code" (func $string_from_code_host (param i64 i32) (result i32)))
  (import "witchy" "dir_subdir" (func $dir_subdir_host (param i32 i32) (result i32)))
  (import "witchy" "dir_exists" (func $dir_exists_host (param i32 i32) (result i32)))
  (import "witchy" "dir_is_dir" (func $dir_is_dir_host (param i32 i32) (result i32)))
  (import "witchy" "dir_write" (func $dir_write_host (param i32 i32 i32)))
  (import "witchy" "dir_append" (func $dir_append_host (param i32 i32 i32)))
  (import "witchy" "dir_make_dir" (func $dir_make_dir_host (param i32 i32)))
  (import "witchy" "net_connect" (func $net_connect_host (param i32 i32) (result i32)))
  (import "witchy" "net_listen" (func $net_listen_host (param i32 i32) (result i32)))
  (import "witchy" "net_accept" (func $net_accept_host (param i32) (result i32)))
  (import "witchy" "net_restrict" (func $net_restrict_host (param i32 i32) (result i32)))
  (import "witchy" "net_send_line" (func $net_send_line_host (param i32 i32)))
  (import "witchy" "net_send_bytes" (func $net_send_bytes_host (param i32 i32)))
  (import "witchy" "net_close" (func $net_close_host (param i32)))
  (import "witchy" "now" (func $now_host (result i64)))
"#;

/// The number of host imports the prelude declares (used to split function
/// indices: imports `0..IMPORT_COUNT`, helpers after).
pub const IMPORT_COUNT: usize = 43;

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
    // The prelude is committed pre-assembled (`src/prelude.wasm`) so the runtime
    // never depends on the `wat` crate. `prelude_wat` stays the source of truth;
    // `committed_prelude_blob_is_current` guards the cache against drift.
    let bin: &[u8] = include_bytes!("prelude.wasm");

    let names = func_names();
    // Type section: collect every function type so import/func type indices resolve.
    let mut func_types: Vec<(Vec<WasmTy>, Vec<WasmTy>)> = Vec::new();
    let mut imports: Vec<PreludeImport> = Vec::new();
    let mut globals: Vec<PreludeGlobal> = Vec::new();
    let mut defined_type_idx: Vec<u32> = Vec::new(); // type index per DEFINED func
    let mut bodies: Vec<Vec<u8>> = Vec::new();
    let mut table_size: u32 = 0;

    for payload in Parser::new(0).parse_all(bin) {
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

    /// The committed `src/prelude.wasm` cache (loaded by `build_prelude` so the
    /// runtime never assembles WAT) must stay byte-identical to what `prelude_wat`
    /// assembles. `prelude_wat` is the source of truth; this guard catches drift.
    /// Regenerate after editing any `*_WAT` helper: `REGEN_PRELUDE=1 cargo test
    /// --features native committed_prelude_blob_is_current`.
    #[test]
    fn committed_prelude_blob_is_current() {
        let want = wat::parse_str(&prelude_wat()).expect("prelude assembles");
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/prelude.wasm");
        if std::env::var("REGEN_PRELUDE").is_ok() {
            std::fs::write(path, &want).expect("write prelude blob");
        }
        let have = std::fs::read(path).expect("read committed src/prelude.wasm");
        assert_eq!(
            have, want,
            "src/prelude.wasm is stale — run `REGEN_PRELUDE=1 cargo test --features native committed_prelude_blob_is_current`"
        );
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
