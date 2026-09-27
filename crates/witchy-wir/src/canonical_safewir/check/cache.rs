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
//! The in-process memo is bounded. `WITCHY_SAFEWIR_CHECK_CACHE=off` disables
//! memoization entirely, so every function is checked on every pass.
//! Functions with affine ABI proofs use a separate in-process verdict table:
//! the function digest and exact equality of all source-derived affine proof
//! maps must both match. These verdicts never enter the persistent store.
//!
//! # The persistent store
//!
//! With `WITCHY_SAFEWIR_CHECK_CACHE_DIR` set, verdicts that took at least
//! [`PERSIST_THRESHOLD`] to establish are also written to that directory and
//! read back by later processes, so the executor is checked once per checkout
//! rather than once per test process. This is opt-in because a verdict read
//! from disk is trusted the way the compiler binary on disk is: whoever can
//! write the directory can make a function pass. The repo's cargo config opts
//! every cargo- and nextest-launched process in (tests are de facto trusted);
//! a `witchy` run from a shell is not opted in. Entries live under a digest of
//! this crate's sources (`build.rs`), so a changed checker never sees an older
//! checker's verdicts, and each file carries its key and an integrity digest
//! so a truncated or misnamed file is a miss, never a verdict.
//!
//! # One checker per verdict at a time
//!
//! With the store cold -- every gate after a change to this crate -- every
//! test process that links the async executor used to check it at once:
//! none had finished, so none could replay, and ten or twenty processes each
//! spent tens of seconds on the identical function. A large function's check
//! is therefore claimed under an exclusive lock file beside its verdict
//! ([`claim`]); a process that waited re-reads the store before checking.
//! The lock adds no trust: a waiter replays only what the store would have
//! given it anyway, and a crashed holder's lock is released by the OS.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::super::identity::{CanonicalWriter, References};
use super::super::{
    CanonicalAbiCatalog, Function, FunctionSignature, FunctionSignatureId, ImportContract, ImportId,
    ProperTailLocation, TailSiteId,
};
use super::{AffineAbiProofs, FunctionIndex};
use crate::layout::{FieldKind, LayoutDescriptor, LayoutId, LayoutInterner, LayoutKind};

const SERIALIZATION_VERSION: u32 = 1;
const DOMAIN: &[u8] = b"witchy/canonical-safewir/function-check/v1";
const MAX_ENTRIES: usize = 1 << 16;
/// Verdicts cheaper than this are not worth a file: re-checking them costs
/// less than the open would.
pub(super) const PERSIST_THRESHOLD: Duration = Duration::from_millis(10);

/// Functions at least this many blocks long are checked under a [`claim`].
/// Small functions check faster than a lock round trip.
pub(super) const SINGLE_FLIGHT_BLOCKS: usize = 256;

/// The digest of every input `check_function` reads for one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct FunctionCheckKey([u8; 32]);

/// The tail sites an admitted function discharged.
pub(super) type Discharged = Arc<BTreeSet<TailSiteId>>;

static VERDICTS: OnceLock<Mutex<HashMap<FunctionCheckKey, Discharged, foldhash::fast::RandomState>>> =
    OnceLock::new();
// Affine verdicts stay in process: the persistent key intentionally omits
// checked-source affine proofs. Exact proof equality supplements the ordinary
// function key here, including fields the function may reach through calls.
static AFFINE_VERDICTS: OnceLock<Mutex<HashMap<
    FunctionCheckKey,
    (Arc<AffineAbiProofs>, Discharged),
    foldhash::fast::RandomState,
>>> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static STORE_HITS: AtomicU64 = AtomicU64::new(0);

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
    if let Some(found) = found {
        HITS.fetch_add(1, Ordering::Relaxed);
        return Some(found);
    }
    if let Some(discharged) = store::dir().and_then(|dir| store::read(dir, key)) {
        STORE_HITS.fetch_add(1, Ordering::Relaxed);
        if debug() {
            eprintln!("safewir-check-cache: replayed {} from the store", key.hex());
        }
        let discharged = Arc::new(discharged);
        remember(*key, Arc::clone(&discharged));
        return Some(discharged);
    }
    MISSES.fetch_add(1, Ordering::Relaxed);
    None
}

pub(super) fn lookup_affine(key: &FunctionCheckKey, proofs: &AffineAbiProofs) -> Option<Discharged> {
    let verdicts = AFFINE_VERDICTS.get_or_init(Default::default);
    let found = verdicts.lock().ok().and_then(|verdicts| {
        verdicts.get(key).and_then(|(checked, discharged)| {
            (checked.as_ref() == proofs).then(|| Arc::clone(discharged))
        })
    });
    if found.is_some() {
        HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        MISSES.fetch_add(1, Ordering::Relaxed);
    }
    found
}

pub(super) fn record_affine(
    key: FunctionCheckKey,
    proofs: Arc<AffineAbiProofs>,
    discharged: Discharged,
) {
    let verdicts = AFFINE_VERDICTS.get_or_init(Default::default);
    if let Ok(mut verdicts) = verdicts.lock() {
        if verdicts.len() >= MAX_ENTRIES {
            verdicts.clear();
        }
        verdicts.insert(key, (proofs, discharged));
    }
}

/// Wall-clock cost of one fresh check; zero where the target has no clock.
pub(super) struct Stopwatch(#[cfg(not(target_arch = "wasm32"))] std::time::Instant);

impl Stopwatch {
    pub(super) fn start() -> Self {
        Self(
            #[cfg(not(target_arch = "wasm32"))]
            std::time::Instant::now(),
        )
    }

    pub(super) fn elapsed(&self) -> Duration {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.0.elapsed()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Duration::ZERO
        }
    }
}

/// Exclusive right to check one large function across processes, held
/// until dropped. `None` when the store is off, the function is small, or
/// the lock cannot be taken (then the caller simply checks, as before).
pub(super) struct Claim {
    key: FunctionCheckKey,
    /// Held only by the process that took the lock without waiting. A
    /// process that waited has already released it (see `store::claim`).
    #[cfg(not(target_arch = "wasm32"))]
    _lock: Option<std::fs::File>,
}

/// Wait for any other process checking `key`, then hold the claim.
pub(super) fn claim(key: &FunctionCheckKey, blocks: usize) -> Option<Claim> {
    if blocks < SINGLE_FLIGHT_BLOCKS {
        return None;
    }
    let dir = store::dir()?;
    store::claim(dir, key).map(|_lock| Claim {
        key: *key,
        #[cfg(not(target_arch = "wasm32"))]
        _lock,
    })
}

impl Claim {
    /// The verdict another process published while this one waited.
    pub(super) fn published(&self) -> Option<Discharged> {
        let discharged = Arc::new(store::read(store::dir()?, &self.key)?);
        STORE_HITS.fetch_add(1, Ordering::Relaxed);
        if debug() {
            eprintln!("safewir-check-cache: replayed {} after waiting for its claim", self.key.hex());
        }
        remember(self.key, Arc::clone(&discharged));
        Some(discharged)
    }

    /// Record the verdict this claim established, persisting it whatever it
    /// cost: every waiter is about to look for it.
    pub(super) fn record(self, discharged: Discharged) {
        remember(self.key, Arc::clone(&discharged));
        if let Some(dir) = store::dir() {
            store::write(dir, self.key, &discharged);
        }
    }
}

/// Keep an admitted verdict that took `elapsed` to establish.
pub(super) fn record(key: FunctionCheckKey, discharged: Discharged, elapsed: Duration) {
    remember(key, Arc::clone(&discharged));
    if elapsed >= PERSIST_THRESHOLD
        && let Some(dir) = store::dir()
    {
        if debug() {
            eprintln!("safewir-check-cache: stored {} after {elapsed:?}", key.hex());
        }
        store::write(dir, key, &discharged);
    }
}

/// `WITCHY_SAFEWIR_CHECK_CACHE_DEBUG` reports store traffic on stderr.
fn debug() -> bool {
    static DEBUG: OnceLock<bool> = OnceLock::new();
    *DEBUG.get_or_init(|| std::env::var_os("WITCHY_SAFEWIR_CHECK_CACHE_DEBUG").is_some())
}

impl FunctionCheckKey {
    fn hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

fn remember(key: FunctionCheckKey, discharged: Discharged) {
    let verdicts = VERDICTS.get_or_init(Default::default);
    if let Ok(mut verdicts) = verdicts.lock() {
        if verdicts.len() >= MAX_ENTRIES {
            verdicts.clear();
        }
        verdicts.insert(key, discharged);
    }
}

/// Verdict files: `<dir>/<source stamp>/<key hex>`, written whole under a
/// temporary name and renamed into place, so a reader sees a complete file
/// or none. Concurrent writers of one key race benignly: same bytes.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod store {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::time::Duration;

    use super::super::super::TailSiteId;
    use super::FunctionCheckKey;

    const MAGIC: &[u8] = b"WITCHY-SAFEWIR-VERDICT\0";
    const FORMAT_VERSION: u32 = 1;
    const MAX_SITES: u64 = 1 << 16;
    /// Stamp directories of checkers not built for this long are removed
    /// the first time a process writes.
    const STALE_STAMP_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

    /// Digest of this crate's sources at build time, stamped by `build.rs`.
    pub(super) const SOURCE_STAMP: &str = env!("WITCHY_WIR_SOURCE_DIGEST");

    /// The stamp directory for this checker build, when the store is opted in.
    pub(super) fn dir() -> Option<&'static Path> {
        static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
        DIR.get_or_init(|| {
            let root = std::env::var_os("WITCHY_SAFEWIR_CHECK_CACHE_DIR")
                .filter(|value| !value.is_empty())?;
            Some(stamp_dir(Path::new(&root)))
        })
        .as_deref()
    }

    pub(super) fn stamp_dir(root: &Path) -> PathBuf {
        root.join(&SOURCE_STAMP[..16])
    }

    pub(super) fn read(dir: &Path, key: &FunctionCheckKey) -> Option<BTreeSet<TailSiteId>> {
        let bytes = fs::read(dir.join(file_name(key))).ok()?;
        decode(key, &bytes)
    }

    pub(super) fn write(dir: &Path, key: FunctionCheckKey, discharged: &BTreeSet<TailSiteId>) {
        static PRUNED: OnceLock<()> = OnceLock::new();
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        PRUNED.get_or_init(|| prune_stale_stamps(dir));
        let bytes = encode(key, discharged);
        let temporary = dir.join(format!(".{}.{}", file_name(&key), std::process::id()));
        if fs::write(&temporary, bytes).is_err() || fs::rename(&temporary, dir.join(file_name(&key))).is_err() {
            let _ = fs::remove_file(&temporary);
        }
    }

    /// Claim the check of `key`. `Some(Some(lock))`: nobody else was checking
    /// it, so this process checks and publishes under the lock. `Some(None)`:
    /// another process held it; this one waited for that holder to finish and
    /// has already released the lock again, so it only re-reads the store.
    /// If the holder published nothing (a rejection, a failed write) the
    /// waiters then check in parallel rather than one lock-holder at a time,
    /// which bounds the wait at one holder's check.
    pub(super) fn claim(dir: &Path, key: &FunctionCheckKey) -> Option<Option<fs::File>> {
        fs::create_dir_all(dir).ok()?;
        let lock = fs::OpenOptions::new().create(true).truncate(false).write(true)
            .open(dir.join(format!("{}.lock", file_name(key)))).ok()?;
        match lock.try_lock() {
            Ok(()) => Some(Some(lock)),
            Err(fs::TryLockError::WouldBlock) => {
                lock.lock().ok()?;
                drop(lock);
                Some(None)
            }
            Err(fs::TryLockError::Error(_)) => None,
        }
    }

    fn file_name(key: &FunctionCheckKey) -> String {
        key.hex()
    }

    fn encode(key: FunctionCheckKey, discharged: &BTreeSet<TailSiteId>) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + 32 + 8 + 32 * discharged.len() + 32);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&key.0);
        bytes.extend_from_slice(&(discharged.len() as u64).to_le_bytes());
        for site in discharged {
            bytes.extend_from_slice(&site.digest());
        }
        let integrity = blake3::hash(&bytes);
        bytes.extend_from_slice(integrity.as_bytes());
        bytes
    }

    fn decode(key: &FunctionCheckKey, bytes: &[u8]) -> Option<BTreeSet<TailSiteId>> {
        let header = MAGIC.len() + 4 + 32 + 8;
        if bytes.len() < header + 32 || !bytes.starts_with(MAGIC) {
            return None;
        }
        let (body, integrity) = bytes.split_at(bytes.len() - 32);
        if blake3::hash(body).as_bytes() != integrity {
            return None;
        }
        let mut cursor = MAGIC.len();
        let version = u32::from_le_bytes(body[cursor..cursor + 4].try_into().ok()?);
        cursor += 4;
        if version != FORMAT_VERSION || body[cursor..cursor + 32] != key.0 {
            return None;
        }
        cursor += 32;
        let count = u64::from_le_bytes(body[cursor..cursor + 8].try_into().ok()?);
        cursor += 8;
        if count > MAX_SITES || body.len() - cursor != usize::try_from(count).ok()? * 32 {
            return None;
        }
        let mut discharged = BTreeSet::new();
        for site in body[cursor..].chunks_exact(32) {
            let mut digest = [0; 32];
            digest.copy_from_slice(site);
            discharged.insert(TailSiteId::from_digest(digest));
        }
        Some(discharged)
    }

    fn prune_stale_stamps(dir: &Path) {
        let Some(root) = dir.parent() else { return };
        let Ok(entries) = fs::read_dir(root) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path == dir || !path.is_dir() {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STALE_STAMP_AGE);
            if stale {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }

    #[cfg(feature = "raw-encoder-test-api")]
    #[doc(hidden)]
    pub mod self_checks {
        use std::collections::BTreeSet;
        use std::fs;
        use std::sync::Arc;

        use super::super::super::super::TailSiteId;
        use super::super::{AffineAbiProofs, FunctionCheckKey, lookup_affine, record_affine};
        use super::{read, stamp_dir, write};
        use crate::canonical_safewir::{ValueType, closure_owner_contract::{CaptureTransfer, ConstructorId}};

        fn scratch(name: &str) -> std::path::PathBuf {
            let dir = std::env::temp_dir()
                .join(format!("witchy-safewir-verdicts-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        }

        pub fn verdict_files_round_trip_and_reject_damage() {
            let root = scratch("roundtrip");
            let dir = stamp_dir(&root);
            let key = FunctionCheckKey([7; 32]);
            let sites = [[1; 32], [2; 32]].into_iter().map(TailSiteId::from_digest).collect::<BTreeSet<_>>();
            assert!(read(&dir, &key).is_none());
            write(&dir, key, &sites);
            assert_eq!(read(&dir, &key), Some(sites.clone()));
            assert!(read(&dir, &FunctionCheckKey([8; 32])).is_none());

            // A file under another key's name is not that key's verdict.
            let stored = fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
            let other = dir.join("ff".repeat(32));
            fs::copy(&stored, &other).unwrap();
            assert!(read(&dir, &FunctionCheckKey([0xff; 32])).is_none());

            // Truncation and a flipped byte are misses.
            let bytes = fs::read(&stored).unwrap();
            fs::write(&stored, &bytes[..bytes.len() - 1]).unwrap();
            assert!(read(&dir, &key).is_none());
            let mut flipped = bytes.clone();
            flipped[40] ^= 1;
            fs::write(&stored, &flipped).unwrap();
            assert!(read(&dir, &key).is_none());
            fs::write(&stored, &bytes).unwrap();
            assert_eq!(read(&dir, &key), Some(sites));

            // Another checker build's stamp directory is invisible.
            let foreign = root.join("0000000000000000");
            fs::create_dir_all(&foreign).unwrap();
            fs::copy(&stored, foreign.join(stored.file_name().unwrap())).unwrap();
            assert!(read(&foreign, &key).is_some());
            assert_ne!(foreign, dir);
            let _ = fs::remove_dir_all(&root);
        }

        pub fn affine_verdicts_require_exact_capture_proofs() {
            let key = FunctionCheckKey([9; 32]);
            let mut checked = AffineAbiProofs::new();
            checked.owner_captures.insert(
                ConstructorId(1), vec![(CaptureTransfer::CopyScalar, ValueType::Int)],
            );
            assert!(!checked.is_empty(), "capture-only affine proofs cannot use the ordinary verdict cache");
            let mut different = checked.clone();
            different.owner_captures.insert(
                ConstructorId(2), vec![(CaptureTransfer::CopyScalar, ValueType::Int)],
            );
            let sites = [TailSiteId::from_digest([3; 32])].into_iter().collect::<BTreeSet<_>>();
            record_affine(key, Arc::new(checked.clone()), Arc::new(sites.clone()));
            assert_eq!(lookup_affine(&key, &checked).as_deref(), Some(&sites));
            assert!(lookup_affine(&key, &different).is_none(), "different source-affine obligations must miss");
        }

    }
}

#[cfg(target_arch = "wasm32")]
mod store {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::super::super::TailSiteId;
    use super::FunctionCheckKey;

    pub(super) fn dir() -> Option<&'static Path> {
        None
    }

    pub(super) fn read(_: &Path, _: &FunctionCheckKey) -> Option<BTreeSet<TailSiteId>> {
        None
    }

    pub(super) fn write(_: &Path, _: FunctionCheckKey, _: &BTreeSet<TailSiteId>) {}

    pub(super) fn claim(_: &Path, _: &FunctionCheckKey) -> Option<()> {
        None
    }
}
