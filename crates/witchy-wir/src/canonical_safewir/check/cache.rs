//! Content-keyed memo of admitted per-function checker verdicts.
//!
//! `check_function` is a pure function of the function body and the module
//! items the body names: its own and its callees' signatures, the host
//! imports it calls and the catalog entries they must match, the layouts it
//! touches (transitively, with their nominal bindings and closure-signature
//! entries), and the tail obligations its terminators discharge. Nothing
//! else in the module reaches it. So an admitted verdict can be replayed
//! whenever exactly those inputs recur: the same sealed module rechecked by
//! `validate_final`, or a lowered standard-library function (the async
//! executor is the expensive one) that is byte-identical in every program
//! that links it.
//!
//! The key is a domain-separated BLAKE3 digest over the same tagged
//! canonical serialization `module_digest` uses, never `Debug`, so a
//! formatting change cannot silently widen a hit. Layout identities are
//! content digests that bundle decode verifies (`import_canonical`), so an
//! id pins its descriptor; what the key adds per layout is what the id does
//! not pin: presence, nominal binding, and closure-signature entry. Only
//! `Ok` verdicts are stored, with the tail sites the function discharged so
//! the caller can replay them into the module-level completeness check. A
//! rejected function is rechecked and reports its diagnostic every time.
//!
//! The memo is process-local and bounded. `WITCHY_SAFEWIR_CHECK_CACHE=off`
//! disables it, so every function is checked on every pass.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::super::identity::{CanonicalWriter, References};
use super::super::{
    CanonicalAbiCatalog, Function, FunctionSignature, FunctionSignatureId, ImportContract, ImportId,
    ProperTailLocation, TailSiteId,
};
use super::FunctionIndex;
use crate::layout::{FieldKind, LayoutDescriptor, LayoutId, LayoutInterner, LayoutKind};

const SERIALIZATION_VERSION: u32 = 1;
const DOMAIN: &[u8] = b"witchy/canonical-safewir/function-check/v1";
const MAX_ENTRIES: usize = 1 << 16;

/// The digest of every input `check_function` reads for one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct FunctionCheckKey([u8; 32]);

/// The tail sites an admitted function discharged.
pub(super) type Discharged = Arc<BTreeSet<TailSiteId>>;

static VERDICTS: OnceLock<Mutex<HashMap<FunctionCheckKey, Discharged, foldhash::fast::RandomState>>> =
    OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("WITCHY_SAFEWIR_CHECK_CACHE").is_none_or(|value| value != "off"))
}

/// `None` when the memo is disabled.
#[allow(clippy::too_many_arguments)]
pub(super) fn key(
    function: &Function,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    imports: &BTreeMap<ImportId, &ImportContract>,
    catalog: Option<&CanonicalAbiCatalog>,
    functions: &FunctionIndex,
    tail_obligations: &BTreeMap<TailSiteId, ProperTailLocation>,
    layouts: &LayoutInterner,
    closure_layouts: &BTreeMap<LayoutId, FunctionSignatureId>,
) -> Option<FunctionCheckKey> {
    if !enabled() {
        return None;
    }
    let mut writer = CanonicalWriter::collecting(SERIALIZATION_VERSION, DOMAIN);
    writer.function(function);
    let mut pending = writer.take_references();
    pending.signatures.insert(function.signature);
    let mut done = References::default();

    // Close the reference set: signatures and imports name layouts and
    // further signatures, layouts name nested layouts and (through the
    // closure table) signatures. Each item is encoded exactly once, in id
    // order within its round, so equal inputs give equal bytes.
    loop {
        let functions_round = pending.functions.difference(&done.functions).copied().collect::<Vec<_>>();
        let imports_round = pending.imports.difference(&done.imports).copied().collect::<Vec<_>>();
        let signatures_round = pending.signatures.difference(&done.signatures).copied().collect::<Vec<_>>();
        let layouts_round = pending.layouts.difference(&done.layouts).copied().collect::<Vec<_>>();
        if functions_round.is_empty()
            && imports_round.is_empty()
            && signatures_round.is_empty()
            && layouts_round.is_empty()
        {
            break;
        }

        // A callee contributes only its declared signature id; the body is
        // summarized by the module-level contract pass, not read here.
        writer.u8(b'F');
        writer.len(functions_round.len());
        for id in functions_round {
            writer.u32(id.get());
            match functions.get(&id) {
                Some(callee) => {
                    writer.u8(1);
                    writer.u32(callee.signature.get());
                    pending.signatures.insert(callee.signature);
                }
                None => writer.u8(0),
            }
            done.functions.insert(id);
        }

        // A host call is checked against both the module's declaration and
        // the catalog's authoritative contract for the same id.
        writer.u8(b'I');
        writer.len(imports_round.len());
        for id in imports_round {
            writer.u32(id.get());
            match imports.get(&id) {
                Some(import) => {
                    writer.u8(1);
                    writer.import(import);
                }
                None => writer.u8(0),
            }
            match catalog.and_then(|catalog| catalog.imports.get(&id)) {
                Some(expected) => {
                    writer.u8(1);
                    writer.import(expected);
                }
                None => writer.u8(0),
            }
            merge(&mut pending, writer.take_references());
            done.imports.insert(id);
        }

        writer.u8(b'S');
        writer.len(signatures_round.len());
        for id in signatures_round {
            writer.u32(id.get());
            match signatures.get(&id) {
                Some(signature) => {
                    writer.u8(1);
                    writer.signature(signature);
                }
                None => writer.u8(0),
            }
            merge(&mut pending, writer.take_references());
            done.signatures.insert(id);
        }

        // The id already commits to the descriptor's bytes; record what it
        // does not commit to.
        writer.u8(b'L');
        writer.len(layouts_round.len());
        for id in layouts_round {
            writer.fixed_bytes(id.as_bytes());
            match layouts.get(id) {
                Some(descriptor) => {
                    writer.u8(1);
                    for nested in nested_layouts(descriptor) {
                        pending.layouts.insert(nested);
                    }
                }
                None => writer.u8(0),
            }
            match layouts.nominal_target(id) {
                Some(target) => {
                    writer.u8(1);
                    writer.fixed_bytes(target.as_bytes());
                    pending.layouts.insert(target);
                }
                None => writer.u8(0),
            }
            match closure_layouts.get(&id) {
                Some(signature) => {
                    writer.u8(1);
                    writer.u32(signature.get());
                    pending.signatures.insert(*signature);
                }
                None => writer.u8(0),
            }
            done.layouts.insert(id);
        }
    }

    writer.u8(b'T');
    writer.len(pending.tail_sites.len());
    for site in &pending.tail_sites {
        writer.fixed_bytes(&site.digest());
        match tail_obligations.get(site) {
            Some(location) => {
                writer.u8(1);
                writer.u32(location.function().get());
                writer.u32(location.block().get());
            }
            None => writer.u8(0),
        }
    }

    Some(FunctionCheckKey(*blake3::hash(&writer.finish()).as_bytes()))
}

fn merge(pending: &mut References, more: References) {
    pending.layouts.extend(more.layouts);
    pending.signatures.extend(more.signatures);
    pending.functions.extend(more.functions);
    pending.imports.extend(more.imports);
    pending.tail_sites.extend(more.tail_sites);
}

/// Every layout a descriptor names, through its kind and its field slots.
fn nested_layouts(descriptor: &LayoutDescriptor) -> BTreeSet<LayoutId> {
    let mut nested = BTreeSet::new();
    match descriptor.kind() {
        LayoutKind::Scalar(_) | LayoutKind::OpaqueNominal { .. } => {}
        LayoutKind::Tuple { fields } | LayoutKind::PackedRecord { fields } => {
            nested.extend(fields.iter().copied());
        }
        LayoutKind::PackedList { element, .. }
        | LayoutKind::RuntimeBuffer { element }
        | LayoutKind::ScratchBuffer { element } => {
            nested.insert(*element);
        }
        LayoutKind::ClosedSum { variants } | LayoutKind::RuntimeClosedSum { variants } => {
            nested.extend(variants.iter().flatten().copied());
        }
        LayoutKind::RuntimePointerBuffer { pointee, .. }
        | LayoutKind::RuntimePointerValueSlot { pointee, .. } => {
            nested.insert(*pointee);
        }
        LayoutKind::RuntimeDictionaryBody { key, value, row } => {
            nested.extend([*key, *value, *row]);
        }
        LayoutKind::RuntimeDictionaryRow { key, value }
        | LayoutKind::RuntimeDictionaryIndex { key, value } => {
            nested.extend([*key, *value]);
        }
        LayoutKind::RuntimeValueSlot { value } => {
            nested.insert(*value);
        }
        LayoutKind::ClosureEnvironment { captures } => {
            nested.extend(captures.iter().copied());
        }
        LayoutKind::ExposedRoot { body, .. } => {
            nested.insert(*body);
        }
    }
    let fields = descriptor
        .fields()
        .iter()
        .chain(descriptor.variant_layouts().iter().flat_map(|variant| variant.fields().iter()));
    for field in fields {
        match field.kind() {
            FieldKind::Scalar(_) => {}
            FieldKind::Inline(layout) | FieldKind::Pointer { pointee: layout, .. } => {
                nested.insert(layout);
            }
        }
    }
    nested
}

pub(super) fn lookup(key: &FunctionCheckKey) -> Option<Discharged> {
    let verdicts = VERDICTS.get_or_init(Default::default);
    let found = verdicts.lock().ok().and_then(|verdicts| verdicts.get(key).cloned());
    match &found {
        Some(_) => HITS.fetch_add(1, Ordering::Relaxed),
        None => MISSES.fetch_add(1, Ordering::Relaxed),
    };
    found
}

pub(super) fn record(key: FunctionCheckKey, discharged: Discharged) {
    let verdicts = VERDICTS.get_or_init(Default::default);
    if let Ok(mut verdicts) = verdicts.lock() {
        if verdicts.len() >= MAX_ENTRIES {
            verdicts.clear();
        }
        verdicts.insert(key, discharged);
    }
}

/// Lifetime (hits, misses) of this process's memo.
#[cfg(test)]
pub(super) fn stats() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}
