//! Instance-bound owner bridges registered only from an admitted structural recipe.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use wasmtime::{bail, AsContextMut, ExternRef, OwnedRooted, Result, Rooted};

/// This entry point takes the immutable admitted artifact, not raw Wasm bytes
/// or a name-selected contract. Each invocation mints fresh instance identity.
pub(in crate::runtime) fn register(
    linker: &mut wasmtime::Linker<super::super::VmState>,
    artifact: &witchy_wir::AdmittedCanonicalArtifact,
) -> Result<()> {
    use std::collections::BTreeMap;
    use wasmtime::{FieldType, FuncType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
    use witchy_wir::canonical_safewir::{ValueType, closure_owner_contract::{CaptureStorage, OwnerFamilySource}};
    use witchy_wir::OwnerBridgeRecipe as R;
    let mut families = BTreeMap::new();
    let mut owner_types = BTreeMap::new();
    for bridge in artifact.owner_bridges() {
        let family = families.entry(bridge.family().signature).or_insert_with(|| Arc::new(())).clone();
        for (ty, kind) in [(bridge.family().owner_type, OwnerKind::Struct), (bridge.family().list_type, OwnerKind::List)] {
            if let Some(previous) = owner_types.insert(ty, OwnerIdentity { kind, family: family.clone() }) {
                if previous.kind != kind || !Arc::ptr_eq(&previous.family, &family) {
                    bail!("conflicting admitted owner type identity");
                }
            }
        }
    }
    let instance = Arc::new(InstanceIdentity { next_owner: AtomicU64::new(0), owner_types });
    let mut constructors = BTreeMap::new();
    for bridge in artifact.owner_bridges() {
        let family = families.entry(bridge.family().signature).or_insert_with(|| Arc::new(())).clone();
        for constructor in &bridge.family().constructors {
            if constructors.contains_key(&constructor.id.0) { continue; }
            let mut fields = vec![FieldType::new(Mutability::Const, ValType::I32.into()); 2];
let slots = bridge.family().constructors.iter().flat_map(|c| &c.captures).filter_map(|c| match c.storage {
    CaptureStorage::OwnerField(field) => Some(field as usize + 1), _ => None,
}).max().unwrap_or(2);
fields.extend((2..slots).map(|_| FieldType::new(Mutability::Const, ValType::EXTERNREF.into())));
let mut captures = Vec::new();

            for (ordinal, capture) in constructor.captures.iter().enumerate() {
                if matches!(capture.storage, CaptureStorage::EnvironmentField(_)) { continue; }
                let CaptureStorage::OwnerField(field) = capture.storage else {
                    bail!("invalid opaque owner capture field");
                };
                if field < 2 || field as usize >= slots { bail!("invalid owner capture field order"); }
                let kind = match capture.value_type {
                    ValueType::Capability(kind) => CaptureFieldKind::Capability(kind),
                    ValueType::Gc(owner_type) => CaptureFieldKind::Owner(owner_type),
                    _ => bail!("invalid opaque owner capture type"),
                };
                captures.push(CaptureField { capture: ordinal as u32, field: field as usize, kind });
            }
            constructors.insert(constructor.id.0, Arc::new(Binding {
                instance: instance.clone(), constructor: Arc::new(()), family: family.clone(),
                owner_type: bridge.family().owner_type,
                list_type: bridge.family().list_type,
                cleanup_tag: bridge.family().signature.get(),
                environment_tag: constructor.environment_tag,
                payload_type: StructType::new(linker.engine(), fields)?, capture_fields: captures.into_boxed_slice(),
            }));
        }
    }
    let wasm_type = |ty: &ValueType| -> Result<ValType> {
        Ok(match ty {
            ValueType::Gc(_) | ValueType::Capability(_) => ValType::EXTERNREF,
            ValueType::Size | ValueType::Callable(_) | ValueType::Ref(_) => ValType::I32,
            ValueType::Int => ValType::I64,
            _ => bail!("unsupported private owner ABI lane"),
        })
    };
    for bridge in artifact.owner_bridges() {
        let constructor = match bridge.recipe() {
            R::Construct(c) | R::ReadCapability(c, _) | R::ReadOwner(c, _)
            | R::CloneEnvironment(c) => *c,
            _ => bridge.family().constructors.first().ok_or_else(|| wasmtime::Error::msg("empty owner family"))?.id,
        };
        let binding = constructors.get(&constructor.0).ok_or_else(|| wasmtime::Error::msg("missing owner binding"))?.clone();
        let recipe = bridge.recipe().clone();
        let code = match &bridge.family().source {
            OwnerFamilySource::Closure => Some(bridge.callable_indices().iter()
                .find(|(id, _)| *id == constructor)
                .ok_or_else(|| wasmtime::Error::msg("missing owner callable identity"))?.1),
            OwnerFamilySource::Aggregate(_) => {
                if !bridge.callable_indices().is_empty() || matches!(recipe, R::ReadCallable(_)) {
                    bail!("aggregate owner has no callable identity");
                }
                None
            }
        };
        let allowed_codes = bridge.callable_indices().iter().map(|(_, code)| *code as i32).collect::<Vec<_>>();
        let parameters = bridge.parameters().iter().map(&wasm_type).collect::<Result<Vec<_>>>()?;
        let mut results = if bridge.payload_result() { vec![ValType::ANYREF] } else { bridge.results().iter().map(&wasm_type).collect::<Result<Vec<_>>>()? };
        if bridge.cleanup_result() { results.push(ValType::ANYREF); }
        let signature = FuncType::new(linker.engine(), parameters, results);
        linker.func_new(bridge.module(), &bridge.name(), signature, move |mut caller, arguments, results| {
            let owner = |index: usize| -> Result<Rooted<ExternRef>> {
                match arguments.get(index) {
                    Some(Val::ExternRef(Some(owner))) => Ok(*owner),
                    _ => bail!("null or non-reference private owner operand"),
                }
            };
            match &recipe {
                R::Construct(_) => {
                    // No callback, allocation-address lookup, ambient token,
                    // or guest-selected constructor participates in sealing.
let Some(Val::I32(environment)) = arguments.first() else { bail!("missing checked owner environment"); };
// Aggregates retain the common payload shape, but this lane is inert.
// Only closure families can register a ReadCallable bridge.
let mut fields = vec![Val::I32(*environment), Val::I32(code.unwrap_or(0) as i32)];
fields.extend(binding.payload_type.fields().skip(2).map(|_| Val::ExternRef(None)));
if arguments.len() != binding.capture_fields.len() + 1 { bail!("owner capture arity"); }
for (field, value) in binding.capture_fields.iter().zip(&arguments[1..]) {
    fields[field.field] = *value;
}

                    let pre = StructRefPre::new(&mut caller, binding.payload_type.clone());
                    let payload = StructRef::new(&mut caller, &pre, &fields)?;
                    results[0] = Val::ExternRef(Some(binding.seal_gc(&mut caller, payload)?));
                }
                R::ReadCapability(_, capture) => {
                    let token = binding.capability::<OwnedRooted<StructRef>>(&caller, owner(0)?, capture.0)?;
                    results[0] = Val::ExternRef(Some(token.to_rooted(&mut caller)));
                }
                R::ReadOwner(_, capture) => {
                    let (expected, nested) = binding.owner_capture(&caller, owner(0)?, capture.0)?;
                    let nested = nested.to_rooted(&mut caller);
                    Binding::require_owner(&caller, &binding.instance, nested, expected)?;
                    Binding::retain_any(&caller, &binding.instance, nested)?;
                    results[0] = Val::ExternRef(Some(nested));
                }
                R::ReadTag(_) => {
                    results[0] = Val::I64(binding.family_tag(&caller, owner(0)?)?);
                }
                R::ReadCallable(_) => {
                    let payload = binding.family_access(&caller, owner(0)?, |live| Ok(live.payload.as_ref().expect("live payload").clone()))?;
                    let Val::I32(code) = payload.field(&mut caller, 1)? else { bail!("invalid owner callable lane") };
                    if !allowed_codes.contains(&code) { bail!("owner callable is outside its checked family"); }
                    results[0] = Val::I32(code);
                }
                R::Clone(_) => {
                    let owner = owner(0)?;
                    binding.family_retain(&caller, owner)?;
                    results[0] = Val::ExternRef(Some(owner));
                }
                R::Drop(_) => {
                    results[0] = Val::AnyRef(binding.release_owner_cleanup_array(&mut caller, owner(0)?)?);
                }
                R::CloneEnvironment(_) => {
                    let payload = binding.access::<OwnedRooted<StructRef>, _>(&caller, owner(0)?, |live| Ok(live.payload.as_ref().expect("live payload").clone()))?;
                    results[0] = Val::AnyRef(Some(payload.to_rooted(&mut caller).to_anyref()));
                }
R::NewList(_, count) => {
    if arguments.len() != *count as usize { bail!("owner list constructor arity"); }
    let elements = (0..arguments.len()).map(&owner).collect::<Result<Vec<_>>>()?;
    results[0] = Val::ExternRef(Some(binding.new_shared_list(&mut caller, &elements)?));
}
R::ListGet(_) => {
    let Some(Val::I32(index)) = arguments.get(1) else { bail!("owner list index type"); };
    results[0] = Val::ExternRef(Some(binding.list_get(&mut caller, owner(0)?, *index as u32)?));
}
R::ListLength(_) => {
    results[0] = Val::I32(binding.list_length(&caller, owner(0)?)? as i32);
}
R::CloneList(_) => {
    let list = owner(0)?;
    binding.clone_list(&caller, list)?;
    results[0] = Val::ExternRef(Some(list));
}
	R::ListPush(_) => {
	    let (list, cleanup) = binding.push_list(&mut caller, owner(0)?, owner(1)?)?;
	    results[0] = Val::ExternRef(Some(list));
	    results[1] = Val::AnyRef(Binding::cleanup_array(&mut caller, &cleanup)?);
	}
	R::ListSet(_) => {
	    let Some(Val::I32(index)) = arguments.get(1) else { bail!("owner list index type"); };
	    let (list, cleanup) = binding.set_list(&mut caller, owner(0)?, *index as u32, owner(2)?)?;
	    results[0] = Val::ExternRef(Some(list));
	    results[1] = Val::AnyRef(Binding::cleanup_array(&mut caller, &cleanup)?);
}
R::DropList(_) => {
    results[0] = Val::AnyRef(binding.release_list_payload_array(&mut caller, owner(0)?)?);
}

            }
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(test)]
use super::super::ConsoleCapability;

// These host-only identities will belong to the instance and its structurally
// authenticated constructor registration. They have no integer representation
// or guest-selected lookup operation. Creating registrations is intentionally
// unavailable outside this module pending authority review.
struct Binding {
    instance: Arc<InstanceIdentity>,
    constructor: Arc<()>,
    family: Arc<()>,
    owner_type: witchy_wir::canonical_safewir::GcTypeId,
    list_type: witchy_wir::canonical_safewir::GcTypeId,
    cleanup_tag: u32,
    environment_tag: i64,
    payload_type: wasmtime::StructType,
    capture_fields: Box<[CaptureField]>,
}

#[derive(Default)]
struct InstanceIdentity {
    next_owner: AtomicU64,
    owner_types: std::collections::BTreeMap<witchy_wir::canonical_safewir::GcTypeId, OwnerIdentity>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnerKind { Struct, List }

struct OwnerIdentity {
    kind: OwnerKind,
    family: Arc<()>,
}

struct CaptureField {
    capture: u32,
    field: usize,
    kind: CaptureFieldKind,
}

#[derive(Clone, Copy)]
enum CaptureFieldKind {
    Capability(witchy_wir::canonical_safewir::CapabilityKindId),
    Owner(witchy_wir::canonical_safewir::GcTypeId),
}

// Store authority is private to this bridge. Capturing a fixture token must
// authenticate it against the current session, not merely its Rust payload
// type. This exposes no token constructor or public capability policy API.
trait CaptureAuthority: 'static {
    fn require_console(&self, data: &dyn std::any::Any) -> Result<()>;
    fn require_build_out(&self, _data: &dyn std::any::Any) -> Result<()> {
        bail!("BuildOut capture has no instance authority");
    }
    fn require_build_read(&self, _data: &dyn std::any::Any) -> Result<()> {
        bail!("BuildRead capture has no instance authority");
    }
    fn require_build_read_response(&self, _data: &dyn std::any::Any) -> Result<()> {
        bail!("BuildRead response capture has no instance authority");
    }
}

fn require_production_console(data: &dyn std::any::Any) -> Result<()> {
    if !data.is::<super::super::ConsoleCapability>() {
        bail!("closure capture has wrong capability kind");
    }
    Ok(())
}

impl CaptureAuthority for super::super::VmState {
    fn require_build_out(&self, data: &dyn std::any::Any) -> Result<()> {
        super::build_out::authenticate_capture(self, data)
    }
    fn require_build_read(&self, data: &dyn std::any::Any) -> Result<()> {
        super::build_read::authenticate_capture(self, data)
    }
    fn require_build_read_response(&self, data: &dyn std::any::Any) -> Result<()> {
        super::build_read::authenticate_response_capture(self, data)
    }
    fn require_console(&self, data: &dyn std::any::Any) -> Result<()> {
        #[cfg(feature = "test-fixtures")]
        if self.fixture_console.is_some() {
            return super::canonical_fixture::authenticate(self.fixture_console.as_ref(), data);
        }
        require_production_console(data)
    }
}

#[cfg(test)]
impl CaptureAuthority for () {
    fn require_console(&self, data: &dyn std::any::Any) -> Result<()> {
        require_production_console(data)
    }
}

fn require_capability<T: CaptureAuthority>(store: impl wasmtime::AsContext<Data = T>, token: Rooted<ExternRef>, kind: witchy_wir::canonical_safewir::CapabilityKindId) -> Result<()> {
    let context = store.as_context();
    let data = token.data(&context)?.ok_or_else(|| wasmtime::Error::msg("capability has no host data"))?;
    require_capability_data(context.data(), data, kind)
}

fn require_capability_data<T: CaptureAuthority>(state: &T, data: &dyn std::any::Any, kind: witchy_wir::canonical_safewir::CapabilityKindId) -> Result<()> {
    let matches = match kind.get() {
        0 => return state.require_console(data),
        9 => return state.require_build_out(data),
        id if id == witchy_wir::canonical_safewir::BUILD_READ_CAPABILITY_KIND.get() =>
            return state.require_build_read(data),
        id if id == witchy_wir::canonical_safewir::BUILD_READ_RESPONSE_KIND.get() =>
            return state.require_build_read_response(data),
        1 => data.is::<super::super::DirAuthority>(),
        2 => data.is::<super::super::EnvAuthority>(),
        3 => data.is::<super::super::FileAuthority>(),
        id if id == witchy_wir::canonical_safewir::NET_CAPABILITY_KIND.get() =>
            data.is::<super::super::NetAuthority>(),
        id if id == witchy_wir::canonical_safewir::EXEC_CAPABILITY_KIND.get() =>
            data.is::<super::super::ExecAuthority>(),
        _ => false,
    };
    if !matches { bail!("closure capture has wrong capability kind"); }
    Ok(())
}

#[cfg(all(test, feature = "test-fixtures"))]
pub(super) fn check_fixture_capture_for_test(
    store: &wasmtime::Store<super::super::VmState>, token: Rooted<ExternRef>,
    kind: witchy_wir::canonical_safewir::CapabilityKindId,
) -> Result<()> {
    require_capability(store, token, kind)
}

struct Live<P> {
    references: usize,
    payload: Option<P>,
    capabilities: Vec<OwnedRooted<ExternRef>>,
    owners: Vec<OwnedRooted<ExternRef>>,
}

struct Owner<P> {
    instance: Arc<InstanceIdentity>,
    constructor: Arc<()>,
    family: Arc<()>,
    owner_type: witchy_wir::canonical_safewir::GcTypeId,
    cleanup_tag: u32,
    environment_tag: i64,
    creation: u64,
    live: Mutex<Live<P>>,
}

struct CleanupItem {
    family_tag: u32,
    payload: OwnedRooted<wasmtime::StructRef>,
}

impl Binding {
    // The immutable binding owns the exact checked layout and capture mapping.
    // A bridge call supplies neither a layout nor a field selector.
    fn seal_gc<T: CaptureAuthority>(
        &self,
        mut store: impl AsContextMut<Data = T>,
        payload: Rooted<wasmtime::StructRef>,
    ) -> Result<Rooted<ExternRef>> {
        if !wasmtime::StructType::eq(&payload.ty(store.as_context())?, &self.payload_type)
            || self.payload_type.fields().any(|field| field.mutability() != wasmtime::Mutability::Const) {
            bail!("closure payload does not match immutable constructor layout");
        }
        let mut capabilities = Vec::new();
        let mut owners = Vec::new();
        for field in &self.capture_fields {
            let wasmtime::Val::ExternRef(Some(token)) = payload.field(&mut store, field.field)? else {
                bail!("closure payload lacks actual opaque capture");
            };
            match field.kind {
                CaptureFieldKind::Capability(kind) => {
                    require_capability(store.as_context(), token, kind)?;
                    capabilities.push(token.to_owned_rooted(&mut store)?);
                }
                CaptureFieldKind::Owner(owner_type) => {
                    Self::require_owner(store.as_context(), &self.instance, token, owner_type)?;
                    owners.push(token.to_owned_rooted(&mut store)?);
                }
            }
        }
        let payload = payload.to_owned_rooted(&mut store)?;
        self.seal_typed(&mut store, payload, capabilities, owners)
    }

    // P is the rooted typed GC payload in the eventual bridge. Keeping the
    // lifecycle generic lets the explicit cleanup handoff be tested without
    // enabling unreviewed GC construction or pretending a scalar is a payload.
    fn seal<P: Send + Sync + 'static, T: CaptureAuthority>(
        &self,
        mut store: impl AsContextMut<Data = T>,
        payload: P,
        capabilities: &[Rooted<ExternRef>],
    ) -> Result<Rooted<ExternRef>> {
        let capability_fields = self.capture_fields.iter().filter_map(|field| match field.kind {
            CaptureFieldKind::Capability(kind) => Some(kind),
            CaptureFieldKind::Owner(_) => None,
        }).collect::<Vec<_>>();
        if capabilities.len() != capability_fields.len() { bail!("closure capability arity mismatch"); }
        let mut rooted = Vec::with_capacity(capabilities.len());
        for (token, kind) in capabilities.iter().zip(capability_fields) {
            require_capability(store.as_context(), *token, kind)?;
            rooted.push(token.to_owned_rooted(&mut store)?);
        }
        let creation = self.instance.next_owner.fetch_add(1, Ordering::Relaxed);
        ExternRef::new(&mut store, Owner {
            instance: self.instance.clone(),
            constructor: self.constructor.clone(),
            family: self.family.clone(),
            owner_type: self.owner_type,
            cleanup_tag: self.cleanup_tag,
            environment_tag: self.environment_tag,
            creation,
            live: Mutex::new(Live { references: 1, payload: Some(payload), capabilities: rooted, owners: Vec::new() }),
        })
    }

    fn seal_typed(
        &self,
        mut store: impl AsContextMut,
        payload: OwnedRooted<wasmtime::StructRef>,
        capabilities: Vec<OwnedRooted<ExternRef>>,
        owners: Vec<OwnedRooted<ExternRef>>,
    ) -> Result<Rooted<ExternRef>> {
        let creation = self.instance.next_owner.fetch_add(1, Ordering::Relaxed);
        for owner in &owners {
            let rooted = owner.to_rooted(&mut store);
            let child_creation = Self::owner_creation(store.as_context(), &self.instance, rooted)?;
            if child_creation >= creation { bail!("opaque owner graph is not newer-to-older"); }
        }
        let mut retained = Vec::with_capacity(owners.len());
        for owner in &owners {
            let rooted = owner.to_rooted(&mut store);
            if let Err(error) = Self::retain_any(store.as_context(), &self.instance, rooted) {
                for previous in retained.into_iter().rev() {
                    Self::release_retained_any(store.as_context(), &self.instance, previous)?;
                }
                return Err(error);
            }
            retained.push(rooted);
        }
        let result = ExternRef::new(&mut store, Owner {
            instance: self.instance.clone(),
            constructor: self.constructor.clone(),
            family: self.family.clone(),
            owner_type: self.owner_type,
            cleanup_tag: self.cleanup_tag,
            environment_tag: self.environment_tag,
            creation,
            live: Mutex::new(Live { references: 1, payload: Some(payload), capabilities, owners }),
        });
        if result.is_err() {
            for owner in retained.into_iter().rev() {
                Self::release_retained_any(store.as_context(), &self.instance, owner)?;
            }
        }
        result
    }

    fn access<P: Send + Sync + 'static, R>(
        &self,
        store: impl wasmtime::AsContext,
        owner: Rooted<ExternRef>,
        action: impl FnOnce(&mut Live<P>) -> Result<R>,
    ) -> Result<R> {
        let context = store.as_context();
        let owner = owner.data(context)?.and_then(|data| data.downcast_ref::<Owner<P>>())
            .ok_or_else(|| wasmtime::Error::msg("not a closure owner"))?;
        if !Arc::ptr_eq(&self.instance, &owner.instance)
            || !Arc::ptr_eq(&self.constructor, &owner.constructor)
            || self.owner_type != owner.owner_type {
            bail!("foreign closure owner binding");
        }
        let mut live = owner.live.lock().map_err(|_| wasmtime::Error::msg("poisoned closure owner"))?;
        if live.references == 0 { bail!("released closure owner"); }
        action(&mut live)
    }

    fn retain<P: Send + Sync + 'static>(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<()> {
        self.access::<P, _>(store, owner, |live| {
            live.references = live.references.checked_add(1)
                .ok_or_else(|| wasmtime::Error::msg("closure owner reference overflow"))?;
            Ok(())
        })
    }

    // capture is a fixed Rust bridge-registration operand, never a Wasm input.
    fn capability<P: Send + Sync + 'static>(&self, store: impl wasmtime::AsContext<Data: CaptureAuthority>, owner: Rooted<ExternRef>, capture: u32) -> Result<OwnedRooted<ExternRef>> {
        let (index, kind) = self.capture_fields.iter().filter_map(|field| match field.kind {
            CaptureFieldKind::Capability(kind) => Some((field.capture, kind)),
            CaptureFieldKind::Owner(_) => None,
        }).enumerate().find_map(|(index, (ordinal, kind))| (ordinal == capture).then_some((index, kind)))
            .ok_or_else(|| wasmtime::Error::msg("not a capability capture"))?;
        let context = store.as_context();
        self.access::<P, _>(&context, owner, |live| {
            let token = live.capabilities.get(index)
                .ok_or_else(|| wasmtime::Error::msg("missing captured capability"))?;
            let data = token.data(&context)?
                .ok_or_else(|| wasmtime::Error::msg("capability has no host data"))?;
            // Sealing authenticates the original capture. Projection must also
            // satisfy the current store's authority, before cloning its root.
            require_capability_data(context.data(), data, kind)?;
            Ok(token.clone())
        })
    }

    #[cfg(test)]
    fn console<P: Send + Sync + 'static>(&self, store: impl wasmtime::AsContext<Data: CaptureAuthority>, owner: Rooted<ExternRef>) -> Result<OwnedRooted<ExternRef>> {
        self.capability::<P>(store, owner, 0)
    }

    // No callback, guest invocation, or GC finalizer performs RC destruction.
    // Final release retires the owner before transferring P to the kernel.
    fn release<P: Send + Sync + 'static>(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<Option<P>> {
        self.access::<P, _>(store, owner, |live| {
            live.references -= 1;
            if live.references != 0 { return Ok(None); }
            live.capabilities.clear();
            live.owners.clear();
            Ok(live.payload.take())
        })
    }

    fn owner_capture(
        &self,
        store: impl wasmtime::AsContext,
        owner: Rooted<ExternRef>,
        capture: u32,
    ) -> Result<(witchy_wir::canonical_safewir::GcTypeId, OwnedRooted<ExternRef>)> {
        let mut owner_index = 0;
        let expected = self.capture_fields.iter().find_map(|field| match field.kind {
            CaptureFieldKind::Owner(owner_type) if field.capture == capture => Some(owner_type),
            CaptureFieldKind::Owner(_) => { owner_index += 1; None }
            CaptureFieldKind::Capability(_) => None,
        }).ok_or_else(|| wasmtime::Error::msg("not a nested owner capture"))?;
        self.access::<OwnedRooted<wasmtime::StructRef>, _>(store, owner, |live| {
            let nested = live.owners.get(owner_index).cloned()
                .ok_or_else(|| wasmtime::Error::msg("missing nested owner capture"))?;
            Ok((expected, nested))
        })
    }
}

struct OwnerList {
    instance: Arc<InstanceIdentity>,
    family: Arc<()>,
    list_type: witchy_wir::canonical_safewir::GcTypeId,
    live: Mutex<ListLive>,
}

// Reference count, actual array, queued cleanup, and ownership-graph age.
type ListLive = (usize, Option<OwnedRooted<wasmtime::ArrayRef>>, Vec<CleanupItem>, u64);

impl Binding {
    // Family operations accept every constructor of the same checked family;
    // capture projection above still requires the exact constructor identity.
    fn family_access<R>(
        &self,
        store: impl wasmtime::AsContext,
        owner: Rooted<ExternRef>,
        action: impl FnOnce(&mut Live<OwnedRooted<wasmtime::StructRef>>) -> Result<R>,
    ) -> Result<R> {
        let context = store.as_context();
        let owner = owner.data(context)?.and_then(|data| data.downcast_ref::<Owner<OwnedRooted<wasmtime::StructRef>>>())
            .ok_or_else(|| wasmtime::Error::msg("not a typed closure owner"))?;
        if !Arc::ptr_eq(&self.instance, &owner.instance) || !Arc::ptr_eq(&self.family, &owner.family)
            || self.owner_type != owner.owner_type {
            bail!("foreign closure owner family");
        }
        let mut live = owner.live.lock().map_err(|_| wasmtime::Error::msg("poisoned closure owner"))?;
        if live.references == 0 { bail!("released closure owner"); }
        action(&mut live)
    }

    fn family_retain(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<()> {
        self.family_access(store, owner, |live| {
            live.references = live.references.checked_add(1).ok_or_else(|| wasmtime::Error::msg("closure owner reference overflow"))?;
            Ok(())
        })
    }

    fn family_tag(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<i64> {
        let context = store.as_context();
        let owner = owner.data(context)?
            .and_then(|data| data.downcast_ref::<Owner<OwnedRooted<wasmtime::StructRef>>>())
            .ok_or_else(|| wasmtime::Error::msg("not a typed closure owner"))?;
        if !Arc::ptr_eq(&self.instance, &owner.instance)
            || !Arc::ptr_eq(&self.family, &owner.family)
            || self.owner_type != owner.owner_type
        {
            bail!("foreign closure owner family");
        }
        let live = owner.live.lock().map_err(|_| wasmtime::Error::msg("poisoned closure owner"))?;
        if live.references == 0 { bail!("released closure owner"); }
        Ok(owner.environment_tag)
    }

    fn family_release(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<Option<OwnedRooted<wasmtime::StructRef>>> {
        self.family_access(store, owner, |live| {
            if live.references == 1 && !live.owners.is_empty() {
                bail!("nested owner final release requires cleanup worklist");
            }
            live.references -= 1;
            if live.references != 0 { return Ok(None); }
            live.capabilities.clear();
            live.owners.clear();
            Ok(live.payload.take())
        })
    }

    fn any_owner_access<R>(
        store: impl wasmtime::AsContext,
        instance: &Arc<InstanceIdentity>,
        owner: Rooted<ExternRef>,
        action: impl FnOnce(&mut Live<OwnedRooted<wasmtime::StructRef>>, &Owner<OwnedRooted<wasmtime::StructRef>>) -> Result<R>,
    ) -> Result<R> {
        let context = store.as_context();
        let owner = owner.data(context)?.and_then(|data| data.downcast_ref::<Owner<OwnedRooted<wasmtime::StructRef>>>())
            .ok_or_else(|| wasmtime::Error::msg("not a typed closure owner"))?;
        if !Arc::ptr_eq(instance, &owner.instance) { bail!("foreign closure owner instance"); }
        let mut live = owner.live.lock().map_err(|_| wasmtime::Error::msg("poisoned closure owner"))?;
        if live.references == 0 { bail!("released closure owner"); }
        action(&mut live, owner)
    }

    fn require_owner(
        store: impl wasmtime::AsContext,
        instance: &Arc<InstanceIdentity>,
        owner: Rooted<ExternRef>,
        expected: witchy_wir::canonical_safewir::GcTypeId,
    ) -> Result<()> {
        let identity = instance.owner_types.get(&expected)
            .ok_or_else(|| wasmtime::Error::msg("capture type has no admitted owner family"))?;
        match identity.kind {
            OwnerKind::Struct => Self::any_owner_access(store, instance, owner, |_, owner| {
                if owner.owner_type != expected || !Arc::ptr_eq(&identity.family, &owner.family) {
                    bail!("nested closure owner has wrong type or family");
                }
                Ok(())
            }),
            OwnerKind::List => Self::any_list_access(store, instance, owner, |_, list| {
                if list.list_type != expected || !Arc::ptr_eq(&identity.family, &list.family) {
                    bail!("captured owner list has wrong type or family");
                }
                Ok(())
            }),
        }
    }

    fn is_list(store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<bool> {
        Ok(owner.data(store.as_context())?.is_some_and(|data| data.is::<OwnerList>()))
    }

    fn any_list_access<R>(
        store: impl wasmtime::AsContext,
        instance: &Arc<InstanceIdentity>,
        list: Rooted<ExternRef>,
        action: impl FnOnce(&mut ListLive, &OwnerList) -> Result<R>,
    ) -> Result<R> {
        let context = store.as_context();
        let list = list.data(context)?.and_then(|data| data.downcast_ref::<OwnerList>())
            .ok_or_else(|| wasmtime::Error::msg("not an owner list"))?;
        if !Arc::ptr_eq(instance, &list.instance) { bail!("foreign owner list instance"); }
        let mut live = list.live.lock().map_err(|_| wasmtime::Error::msg("poisoned owner list"))?;
        if live.0 == 0 { bail!("released owner list"); }
        action(&mut live, list)
    }

    fn owner_creation(
        store: impl wasmtime::AsContext,
        instance: &Arc<InstanceIdentity>,
        owner: Rooted<ExternRef>,
    ) -> Result<u64> {
        if Self::is_list(store.as_context(), owner)? {
            return Self::any_list_access(store, instance, owner, |live, _| Ok(live.3));
        }
        Self::any_owner_access(store, instance, owner, |_, owner| Ok(owner.creation))
    }

    fn retain_any(
        store: impl wasmtime::AsContext,
        instance: &Arc<InstanceIdentity>,
        owner: Rooted<ExternRef>,
    ) -> Result<()> {
        if Self::is_list(store.as_context(), owner)? {
            return Self::any_list_access(store, instance, owner, |live, _| {
                live.0 = live.0.checked_add(1)
                    .ok_or_else(|| wasmtime::Error::msg("owner list reference overflow"))?;
                Ok(())
            });
        }
        Self::any_owner_access(store, instance, owner, |live, _| {
            live.references = live.references.checked_add(1)
                .ok_or_else(|| wasmtime::Error::msg("closure owner reference overflow"))?;
            Ok(())
        })
    }

    fn release_retained_any(
        store: impl wasmtime::AsContext,
        instance: &Arc<InstanceIdentity>,
        owner: Rooted<ExternRef>,
    ) -> Result<()> {
        if Self::is_list(store.as_context(), owner)? {
            return Self::any_list_access(store, instance, owner, |live, _| {
                if live.0 <= 1 { bail!("retained list rollback would retire payload"); }
                live.0 -= 1;
                Ok(())
            });
        }
        Self::any_owner_access(store, instance, owner, |live, _| {
            if live.references <= 1 { bail!("retained owner rollback would retire payload"); }
            live.references -= 1;
            Ok(())
        })
    }

    fn collect_cleanup(
        mut store: impl AsContextMut,
        instance: &Arc<InstanceIdentity>,
        roots: impl IntoIterator<Item = OwnedRooted<ExternRef>>,
    ) -> Result<Vec<CleanupItem>> {
        let mut pending = roots.into_iter().collect::<Vec<_>>();
        pending.reverse();
        let mut cleanup = Vec::new();
        while let Some(owner) = pending.pop() {
            let rooted = owner.to_rooted(&mut store);
            if Self::is_list(store.as_context(), rooted)? {
                let (children, inherited) = Self::release_any_list_roots(&mut store, instance, rooted)?;
                cleanup.extend(inherited);
                for child in children.into_iter().rev() { pending.push(child); }
                continue;
            }
            let released = Self::any_owner_access(store.as_context(), instance, rooted, |live, owner| {
                live.references -= 1;
                if live.references != 0 { return Ok(None); }
                live.capabilities.clear();
                let payload = live.payload.take().expect("live owner payload");
                let children = std::mem::take(&mut live.owners);
                Ok(Some((CleanupItem { family_tag: owner.cleanup_tag, payload }, children)))
            })?;
            let Some((item, children)) = released else { continue };
            cleanup.push(item);
            for child in children.into_iter().rev() { pending.push(child); }
        }
        Ok(cleanup)
    }

    fn cleanup_array(
        mut store: impl AsContextMut,
        cleanup: &[CleanupItem],
    ) -> Result<Option<Rooted<wasmtime::AnyRef>>> {
        use wasmtime::{ArrayRef, ArrayRefPre, ArrayType, FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        if cleanup.is_empty() { return Ok(None); }
        let item_type = StructType::new(store.as_context().engine(), [
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::ANYREF.into()),
        ])?;
        let item_pre = StructRefPre::new(&mut store, item_type);
        let mut values = Vec::with_capacity(cleanup.len());
        for item in cleanup {
            let payload = item.payload.to_rooted(&mut store).to_anyref();
            let tagged = StructRef::new(&mut store, &item_pre, &[
                Val::I32(item.family_tag as i32),
                Val::AnyRef(Some(payload)),
            ])?;
            values.push(Val::AnyRef(Some(tagged.to_anyref())));
        }
        let array_type = ArrayType::new(store.as_context().engine(), FieldType::new(Mutability::Const, ValType::ANYREF.into()));
        let array_pre = ArrayRefPre::new(&mut store, array_type);
        Ok(Some(ArrayRef::new_fixed(&mut store, &array_pre, &values)?.to_anyref()))
    }

    fn release_owner_cleanup_items(
        &self,
        mut store: impl AsContextMut,
        owner: Rooted<ExternRef>,
    ) -> Result<Vec<CleanupItem>> {
        self.family_access(store.as_context(), owner, |_| Ok(()))?;
        let root = owner.to_owned_rooted(&mut store)?;
        Self::collect_cleanup(&mut store, &self.instance, [root])
    }

    fn release_owner_cleanup_array(
        &self,
        mut store: impl AsContextMut,
        owner: Rooted<ExternRef>,
    ) -> Result<Option<Rooted<wasmtime::AnyRef>>> {
        let cleanup = self.release_owner_cleanup_items(&mut store, owner)?;
        Self::cleanup_array(&mut store, &cleanup)
    }

    fn list_access<R>(
        &self,
        store: impl wasmtime::AsContext,
        list: Rooted<ExternRef>,
        action: impl FnOnce(&mut ListLive) -> Result<R>,
    ) -> Result<R> {
        Self::any_list_access(store, &self.instance, list, |live, list| {
            if self.list_type != list.list_type || !Arc::ptr_eq(&self.family, &list.family) {
                bail!("foreign owner list family");
            }
            action(live)
        })
    }

    // Explicitly clones the incoming tokens, as a shared list construction.
    // There is no host lookup from an element index to an authority table:
    // the array itself contains the actual references supplied by the caller.
    fn new_shared_list(&self, mut store: impl AsContextMut, elements: &[Rooted<ExternRef>]) -> Result<Rooted<ExternRef>> {
        use wasmtime::{ArrayRef, ArrayRefPre, ArrayType, FieldType, Mutability, Val, ValType};
        for element in elements { self.family_access(store.as_context(), *element, |_| Ok(()))?; }
        let ty = ArrayType::new(store.as_context().engine(), FieldType::new(Mutability::Var, ValType::EXTERNREF.into()));
        let pre = ArrayRefPre::new(&mut store, ty);
        let values = elements.iter().map(|element| Val::ExternRef(Some(*element))).collect::<Vec<_>>();
        let array = ArrayRef::new_fixed(&mut store, &pre, &values)?.to_owned_rooted(&mut store)?;
        for (index, element) in elements.iter().enumerate() {
            if let Err(error) = self.family_retain(store.as_context(), *element) {
                for previous in &elements[..index] { self.family_release(store.as_context(), *previous)?; }
                return Err(error);
            }
        }
        let result = ExternRef::new(&mut store, OwnerList {
            instance: self.instance.clone(), family: self.family.clone(), list_type: self.list_type,
            live: Mutex::new((1, Some(array), Vec::new(), self.instance.next_owner.fetch_add(1, Ordering::Relaxed))),
        });
        if result.is_err() {
            for element in elements { self.family_release(store.as_context(), *element)?; }
        }
        result
    }

    fn list_get(&self, mut store: impl AsContextMut, list: Rooted<ExternRef>, index: u32) -> Result<Rooted<ExternRef>> {
        let array = self.list_access(store.as_context(), list, |live| Ok(live.1.as_ref().expect("live list payload").clone()))?;
        if index >= array.len(store.as_context())? { bail!("owner list index out of bounds"); }
        let wasmtime::Val::ExternRef(Some(owner)) = array.get(&mut store, index)? else { bail!("invalid owner list element") };
        self.family_retain(store.as_context(), owner)?;
        Ok(owner)
    }

fn list_length(&self, store: impl wasmtime::AsContext, list: Rooted<ExternRef>) -> Result<u32> {
    let array = self.list_access(store.as_context(), list, |live| Ok(live.1.as_ref().expect("live list payload").clone()))?;
    array.len(store.as_context())
}

// A replacement list has already retained every current element. Releasing the
// consumed source token must therefore retire none of those current elements.
// Cleanup queued by an earlier in-place replacement is different: it belongs
// to the logical list value and moves to the replacement list with that value.
fn transfer_list_release(
    &self,
    mut store: impl AsContextMut,
    source: Rooted<ExternRef>,
    retired_element_error: &'static str,
) -> Result<Vec<CleanupItem>> {
    let (roots, inherited_cleanup) = self.release_list_roots(&mut store, source)?;
    let retired_current = Self::collect_cleanup(&mut store, &self.instance, roots)?;
    if !retired_current.is_empty() {
        return Err(wasmtime::Error::msg(retired_element_error));
    }
    Ok(inherited_cleanup)
}

// ListPush consumes one list token and one element token, while preserving
// other list aliases. Construction first retains every actual element, so
// releasing the two input tokens cannot retire a payload during this step.
fn push_list(&self, mut store: impl AsContextMut, list: Rooted<ExternRef>, value: Rooted<ExternRef>) -> Result<(Rooted<ExternRef>, Vec<CleanupItem>)> {
    self.family_access(store.as_context(), value, |_| Ok(()))?;
    let array = self.list_access(store.as_context(), list, |live| Ok(live.1.as_ref().expect("live list payload").clone()))?;
    let length = array.len(store.as_context())?;
    let capacity = length.checked_add(1).ok_or_else(|| wasmtime::Error::msg("owner list length overflow"))?;
    let mut elements = Vec::new();
    elements.try_reserve_exact(capacity as usize).map_err(|_| wasmtime::Error::msg("owner list allocation failed"))?;
    for index in 0..length {
        let wasmtime::Val::ExternRef(Some(element)) = array.get(&mut store, index)? else { bail!("invalid owner list element"); };
        elements.push(element);
    }
    elements.push(value);
    let result = self.new_shared_list(&mut store, &elements)?;
    let cleanup = self.transfer_list_release(
        &mut store,
        list,
        "owner list transfer retired a retained element",
    )?;
    if self.family_release(store.as_context(), value)?.is_some() {
        bail!("owner list transfer retired its retained input");
    }
    Ok((result, cleanup))
}

// ListSet consumes one list token and one element token. A unique list keeps
// its owner identity and transfers the value token directly into the slot. A
// shared list receives a new backing array, preserving every existing alias.
// Retired element payloads stay authenticated and are returned by DropList for
// the ordinary heterogeneous kernel teardown.
fn set_list(
    &self,
    mut store: impl AsContextMut,
    list: Rooted<ExternRef>,
    index: u32,
    value: Rooted<ExternRef>,
) -> Result<(Rooted<ExternRef>, Vec<CleanupItem>)> {
    self.family_access(store.as_context(), value, |_| Ok(()))?;
    let (references, array) = self.list_access(store.as_context(), list, |live| {
        Ok((live.0, live.1.as_ref().expect("live list payload").clone()))
    })?;
    let length = array.len(store.as_context())?;
    if index >= length { bail!("owner list index out of bounds"); }

    if references == 1 {
        let wasmtime::Val::ExternRef(Some(replaced)) = array.get(&mut store, index)? else {
            bail!("invalid owner list element");
        };
        array.set(&mut store, index, wasmtime::Val::ExternRef(Some(value)))?;
        // A unique list has no live capturing parent: such a capture retains
        // it and forces the copy-on-write path. Refresh its age after adding
        // a newer child so a subsequent capture preserves newer-to-older edges.
        self.list_access(store.as_context(), list, |live| {
            live.3 = self.instance.next_owner.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })?;
        let replaced = replaced.to_owned_rooted(&mut store)?;
        let cleanup = Self::collect_cleanup(&mut store, &self.instance, [replaced])?;
        return Ok((list, cleanup));
    }

    let mut elements = Vec::new();
    elements.try_reserve_exact(length as usize)
        .map_err(|_| wasmtime::Error::msg("owner list allocation failed"))?;
    for ordinal in 0..length {
        if ordinal == index {
            elements.push(value);
            continue;
        }
        let wasmtime::Val::ExternRef(Some(element)) = array.get(&mut store, ordinal)? else {
            bail!("invalid owner list element");
        };
        elements.push(element);
    }
    let result = self.new_shared_list(&mut store, &elements)?;
    let cleanup = self.transfer_list_release(
        &mut store,
        list,
        "shared owner list transfer retired an aliased element",
    )?;
    if self.family_release(store.as_context(), value)?.is_some() {
        bail!("owner list transfer retired its retained input");
    }
    Ok((result, cleanup))
}

// Only final payloads cross back to the private Wasm destructor. The
// immutable GC array has actual anyrefs; no numeric authority lookup or
// host-to-guest callback participates in list destruction.
fn release_list_payload_array(&self, mut store: impl AsContextMut, list: Rooted<ExternRef>) -> Result<Option<Rooted<wasmtime::AnyRef>>> {
    let cleanup = self.release_list_cleanup_items(&mut store, list)?;
    Self::cleanup_array(&mut store, &cleanup)
}

    fn clone_list(&self, store: impl wasmtime::AsContext, list: Rooted<ExternRef>) -> Result<()> {
        self.list_access(store, list, |live| {
            live.0 = live.0.checked_add(1).ok_or_else(|| wasmtime::Error::msg("owner list reference overflow"))?;
            Ok(())
        })
    }

    // Retires the list first and consumes every element token on final release.
    // Returned payloads are destruction work, not values exposed to the guest;
    // the enclosing kernel must finish their explicit linear-RC teardown.
    fn release_list_roots(&self, mut store: impl AsContextMut, list: Rooted<ExternRef>) -> Result<(Vec<OwnedRooted<ExternRef>>, Vec<CleanupItem>)> {
        self.list_access(store.as_context(), list, |_| Ok(()))?;
        Self::release_any_list_roots(&mut store, &self.instance, list)
    }

    fn release_any_list_roots(mut store: impl AsContextMut, instance: &Arc<InstanceIdentity>, list: Rooted<ExternRef>) -> Result<(Vec<OwnedRooted<ExternRef>>, Vec<CleanupItem>)> {
        let (array, cleanup) = Self::any_list_access(store.as_context(), instance, list, |live, _| {
            live.0 -= 1;
            Ok(if live.0 == 0 {
                (live.1.take(), std::mem::take(&mut live.2))
            } else {
                (None, Vec::new())
            })
        })?;
        let Some(array) = array else { return Ok((Vec::new(), cleanup)) };
        let mut roots = Vec::new();
        for index in 0..array.len(store.as_context())? {
            let wasmtime::Val::ExternRef(Some(owner)) = array.get(&mut store, index)? else { bail!("invalid owner list element") };
            roots.push(owner.to_owned_rooted(&mut store)?);
        }
        Ok((roots, cleanup))
    }

    fn release_list_cleanup_items(&self, mut store: impl AsContextMut, list: Rooted<ExternRef>) -> Result<Vec<CleanupItem>> {
        let (roots, mut cleanup) = self.release_list_roots(&mut store, list)?;
        cleanup.extend(Self::collect_cleanup(&mut store, &self.instance, roots)?);
        Ok(cleanup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_instance(types: &[witchy_wir::canonical_safewir::GcTypeId]) -> Arc<InstanceIdentity> {
        let mut owner_types = std::collections::BTreeMap::new();
        for ty in types {
            let family = Arc::new(());
            owner_types.insert(*ty, OwnerIdentity { kind: OwnerKind::Struct, family: family.clone() });
            owner_types.insert(witchy_wir::canonical_safewir::GcTypeId::new(ty.get() + 1000),
                OwnerIdentity { kind: OwnerKind::List, family });
        }
        Arc::new(InstanceIdentity { next_owner: AtomicU64::new(0), owner_types })
    }

    fn list_capture_bindings(engine: &wasmtime::Engine) -> Result<(Binding, Binding)> {
        use wasmtime::{FieldType, Mutability, StructType, ValType};
        use witchy_wir::canonical_safewir::GcTypeId;
        let child_type = GcTypeId::new(91);
        let outer_type = GcTypeId::new(92);
        let instance = test_instance(&[child_type, outer_type]);
        let child = Binding {
            instance: instance.clone(), constructor: Arc::new(()),
            family: instance.owner_types[&child_type].family.clone(),
            owner_type: child_type, list_type: GcTypeId::new(1091),
            cleanup_tag: 91, environment_tag: 0,
            payload_type: StructType::new(engine, vec![FieldType::new(Mutability::Const, ValType::I32.into()); 2])?,
            capture_fields: Box::new([]),
        };
        let outer = Binding {
            instance: instance.clone(), constructor: Arc::new(()),
            family: instance.owner_types[&outer_type].family.clone(),
            owner_type: outer_type, list_type: GcTypeId::new(1092),
            cleanup_tag: 92, environment_tag: 0,
            payload_type: StructType::new(engine, [
                FieldType::new(Mutability::Const, ValType::I32.into()),
                FieldType::new(Mutability::Const, ValType::I32.into()),
                FieldType::new(Mutability::Const, ValType::EXTERNREF.into()),
            ])?,
            capture_fields: vec![CaptureField { capture: 0, field: 2, kind: CaptureFieldKind::Owner(child.list_type) }].into_boxed_slice(),
        };
        Ok((child, outer))
    }

    fn test_seal(binding: &Binding, store: &mut wasmtime::Store<()>, fields: &[wasmtime::Val]) -> Result<Rooted<ExternRef>> {
        let pre = wasmtime::StructRefPre::new(&mut *store, binding.payload_type.clone());
        let payload = wasmtime::StructRef::new(&mut *store, &pre, fields)?;
        binding.seal_gc(store, payload)
    }

    #[test]
    fn build_read_captures_authenticate_kind_session_projection_and_cleanup() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        use witchy_wir::canonical_safewir::{BUILD_READ_CAPABILITY_KIND, BUILD_READ_RESPONSE_KIND};
        let (_root, mut vm, grant) = super::super::build_read::tests::setup();
        let response = super::super::build_read::tests::read(&mut vm, grant, "sample.txt")?;
        for (kind, token, wrong_kind) in [
            (BUILD_READ_CAPABILITY_KIND, grant.unwrap(), response.unwrap()),
            (BUILD_READ_RESPONSE_KIND, response.unwrap(), grant.unwrap()),
        ] {
            let (mut binding, _) = list_capture_bindings(vm.store.engine())?;
            binding.payload_type = StructType::new(vm.store.engine(), [
                FieldType::new(Mutability::Const, ValType::I32.into()),
                FieldType::new(Mutability::Const, ValType::I32.into()),
                FieldType::new(Mutability::Const, ValType::EXTERNREF.into()),
            ])?;
            binding.capture_fields = vec![CaptureField {
                capture: 0, field: 2, kind: CaptureFieldKind::Capability(kind),
            }].into_boxed_slice();
            let pre = StructRefPre::new(&mut vm.store, binding.payload_type.clone());
            for invalid in [wrong_kind, ExternRef::new(&mut vm.store, kind.get())?] {
                let payload = StructRef::new(&mut vm.store, &pre,
                    &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(invalid))])?;
                assert!(binding.seal_gc(&mut vm.store, payload).is_err());
            }
            let payload = StructRef::new(&mut vm.store, &pre,
                &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(token))])?;
            let owner = binding.seal_gc(&mut vm.store, payload)?;
            assert!(require_capability(&vm.store, owner, kind).is_err(), "owner wrapper is not the captured token");
            let projected = binding.capability::<OwnedRooted<StructRef>>(&vm.store, owner, 0)?;
            assert!(Rooted::ref_eq(&vm.store, &token, &projected)?);
            drop(projected);
            let issuer = vm.store.data().build_read_issuer.clone();
            vm.store.data_mut().build_read_issuer = Arc::new(());
            assert!(require_capability(&vm.store, token, kind).is_err());
            assert!(binding.capability::<OwnedRooted<StructRef>>(&vm.store, owner, 0).is_err(),
                "projection must reauthenticate the captured session");
            vm.store.data_mut().build_read_issuer = issuer;
            let projected = binding.capability::<OwnedRooted<StructRef>>(&vm.store, owner, 0)?;
            assert!(Rooted::ref_eq(&vm.store, &token, &projected)?,
                "failed projection must not consume or replace the captured token");
            drop(projected);
            binding.family_retain(&vm.store, owner)?;
            assert!(binding.release_owner_cleanup_items(&mut vm.store, owner)?.is_empty());
            let cleanup = binding.release_owner_cleanup_items(&mut vm.store, owner)?;
            assert_eq!(cleanup.iter().map(|item| item.family_tag).collect::<Vec<_>>(), [91]);
            assert!(binding.family_retain(&vm.store, owner).is_err());
        }
        Ok(())
    }

    #[test]
    fn exec_capture_requires_exact_host_grant_not_wrapper_or_response_kind() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructType, Val, ValType};
        use witchy_wir::canonical_safewir::{EXEC_CAPABILITY_KIND, EXEC_RESPONSE_KIND};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let (mut wrapper, _) = list_capture_bindings(&engine)?;
        wrapper.payload_type = StructType::new(&engine, [
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::EXTERNREF.into()),
        ])?;
        wrapper.capture_fields = vec![CaptureField {
            capture: 0, field: 2, kind: CaptureFieldKind::Capability(EXEC_CAPABILITY_KIND),
        }].into_boxed_slice();
        let grant = ExternRef::new(&mut store, super::super::super::ExecAuthority {
            allow: Some(["probe".to_owned()].into_iter().collect()),
        })?;
        let console = ExternRef::new(&mut store, ConsoleCapability)?;
        let forged = ExternRef::new(&mut store, EXEC_CAPABILITY_KIND.get())?;
        require_capability(&store, grant, EXEC_CAPABILITY_KIND)?;
        assert!(require_capability(&store, grant, EXEC_RESPONSE_KIND).is_err());
        for invalid in [console, forged] {
            assert!(test_seal(&wrapper, &mut store,
                &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(invalid))]).is_err());
        }
        let owner = test_seal(&wrapper, &mut store,
            &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(grant))])?;
        assert!(require_capability(&store, owner, EXEC_CAPABILITY_KIND).is_err());
        let projected = wrapper.capability::<OwnedRooted<wasmtime::StructRef>>(&store, owner, 0)?;
        assert!(Rooted::ref_eq(&store, &grant, &projected)?);
        let cleanup = wrapper.release_owner_cleanup_items(&mut store, owner)?;
        assert_eq!(cleanup.iter().map(|item| item.family_tag).collect::<Vec<_>>(), [91]);
        assert!(wrapper.family_retain(&store, owner).is_err());
        Ok(())
    }

    #[test]
    fn carried_net_capture_requires_host_kind_not_wrapper_authority() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructType, Val, ValType};
        use witchy_wir::canonical_safewir::{CapabilityKindId, NET_CAPABILITY_KIND};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let (mut wrapper, mut outer) = list_capture_bindings(&engine)?;
        wrapper.payload_type = StructType::new(&engine, [
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::EXTERNREF.into()),
        ])?;
        wrapper.capture_fields = vec![CaptureField {
            capture: 0, field: 2, kind: CaptureFieldKind::Capability(NET_CAPABILITY_KIND),
        }].into_boxed_slice();
        outer.capture_fields[0].kind = CaptureFieldKind::Owner(wrapper.owner_type);
        let net = ExternRef::new(&mut store, super::super::super::NetAuthority { allow: Vec::new() })?;
        let console = ExternRef::new(&mut store, ConsoleCapability)?;
        let forged = ExternRef::new(&mut store, 4_i32)?;
        require_capability(&store, net, NET_CAPABILITY_KIND)?;
        assert!(require_capability(&store, net, CapabilityKindId::new(0)).is_err());
        assert!(require_capability(&store, net, CapabilityKindId::new(u32::MAX)).is_err());
        for invalid in [console, forged] {
            let error = test_seal(&wrapper, &mut store,
                &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(invalid))]).unwrap_err();
            assert!(error.to_string().contains("closure capture has wrong capability kind"));
        }
        let owner = test_seal(&wrapper, &mut store,
            &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(net))])?;
        // Possessing an authenticated aggregate containing Net is not itself
        // possession of a host Net token, nor can Net stand in for that owner.
        assert!(require_capability(&store, owner, NET_CAPABILITY_KIND).is_err());
        assert!(test_seal(&wrapper, &mut store,
            &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(owner))]).is_err());
        assert!(test_seal(&outer, &mut store,
            &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(net))]).is_err());
        let captured = test_seal(&outer, &mut store,
            &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(owner))])?;
        assert!(wrapper.release_owner_cleanup_items(&mut store, owner)?.is_empty());
        let actual = wrapper.capability::<OwnedRooted<wasmtime::StructRef>>(&store, owner, 0)?;
        assert!(Rooted::ref_eq(&store, &net, &actual)?);
        let cleanup = outer.release_owner_cleanup_items(&mut store, captured)?;
        assert_eq!(cleanup.iter().map(|item| item.family_tag).collect::<Vec<_>>(), [92, 91]);
        assert!(wrapper.family_retain(&store, owner).is_err());
        assert!(outer.family_retain(&store, captured).is_err());
        Ok(())
    }

    #[test]
    fn captured_owner_list_retains_reads_and_cleans_duplicate_elements_once() -> Result<()> {
        use wasmtime::Val;
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let (child, outer) = list_capture_bindings(&engine)?;
        let element = test_seal(&child, &mut store, &[Val::I32(7), Val::I32(0)])?;
        let list = child.new_shared_list(&mut store, &[element, element])?;
        assert!(child.release_owner_cleanup_items(&mut store, element)?.is_empty());
        let owner = test_seal(&outer, &mut store, &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(list))])?;
        assert!(child.release_list_cleanup_items(&mut store, list)?.is_empty());
        let (expected, captured) = outer.owner_capture(&store, owner, 0)?;
        assert_eq!(expected, child.list_type);
        let captured = captured.to_rooted(&mut store);
        Binding::require_owner(&store, &outer.instance, captured, expected)?;
        Binding::retain_any(&store, &outer.instance, captured)?;
        assert_eq!(child.list_length(&store, captured)?, 2);
        let read = child.list_get(&mut store, captured, 0)?;
        assert!(Rooted::ref_eq(&store, &read, &element)?);
        assert!(child.release_owner_cleanup_items(&mut store, read)?.is_empty());
        assert!(child.release_list_cleanup_items(&mut store, captured)?.is_empty());
        let cleanup = outer.release_owner_cleanup_items(&mut store, owner)?;
        assert_eq!(cleanup.iter().map(|item| item.family_tag).collect::<Vec<_>>(), [92, 91]);
        assert!(child.clone_list(&store, list).is_err());
        assert!(child.family_retain(&store, element).is_err());
        assert!(Binding::require_owner(&store, &outer.instance, list, expected).is_err());
        Ok(())
    }

    #[test]
    fn captured_owner_list_rejects_wrong_kind_family_instance_and_forgery() -> Result<()> {
        use wasmtime::{ArrayRef, ArrayRefPre, ArrayType, FieldType, Mutability, Val, ValType};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let (child, outer) = list_capture_bindings(&engine)?;
        let wrong_family = Binding {
            instance: child.instance.clone(), constructor: Arc::new(()), family: Arc::new(()),
            owner_type: child.owner_type, list_type: child.list_type, cleanup_tag: 91, environment_tag: 0,
            payload_type: child.payload_type.clone(), capture_fields: Box::new([]),
        };
        let (foreign, _) = list_capture_bindings(&engine)?;
        let wrong_type = outer.new_shared_list(&mut store, &[])?;
        let substituted = wrong_family.new_shared_list(&mut store, &[])?;
        let foreign_list = foreign.new_shared_list(&mut store, &[])?;
        let struct_owner = test_seal(&child, &mut store, &[Val::I32(0), Val::I32(0)])?;
        let forged = ExternRef::new(&mut store, 1091_u32)?;
        let array_type = ArrayType::new(&engine, FieldType::new(Mutability::Const, ValType::EXTERNREF.into()));
        let array_pre = ArrayRefPre::new(&mut store, array_type);
        let array = ArrayRef::new_fixed(&mut store, &array_pre, &[])?.to_owned_rooted(&mut store)?;
        let raw_array = ExternRef::new(&mut store, array)?;
        for token in [wrong_type, substituted, foreign_list, struct_owner, forged, raw_array] {
            assert!(test_seal(&outer, &mut store, &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(token))]).is_err());
        }
        for (binding, token) in [(&outer, wrong_type), (&wrong_family, substituted), (&foreign, foreign_list)] {
            assert!(binding.release_list_cleanup_items(&mut store, token)?.is_empty());
            assert!(binding.clone_list(&store, token).is_err(), "rejected capture retained a list");
        }
        assert_eq!(child.release_owner_cleanup_items(&mut store, struct_owner)?.len(), 1);
        Ok(())
    }

    #[test]
    fn captured_owner_list_mutation_preserves_age_and_copy_on_write() -> Result<()> {
        use wasmtime::Val;
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let (child, outer) = list_capture_bindings(&engine)?;
        let first = test_seal(&child, &mut store, &[Val::I32(10), Val::I32(0)])?;
        let list = child.new_shared_list(&mut store, &[first])?;
        assert!(child.release_owner_cleanup_items(&mut store, first)?.is_empty());
        let replacement = test_seal(&child, &mut store, &[Val::I32(20), Val::I32(0)])?;
        let (list, replaced) = child.set_list(&mut store, list, 0, replacement)?;
        assert_eq!(replaced.len(), 1);
        assert!(Binding::owner_creation(&store, &child.instance, list)? > Binding::owner_creation(&store, &child.instance, replacement)?);
        let owner = test_seal(&outer, &mut store, &[Val::I32(0), Val::I32(0), Val::ExternRef(Some(list))])?;
        let newest = test_seal(&child, &mut store, &[Val::I32(30), Val::I32(0)])?;
        let (copied, cleanup) = child.set_list(&mut store, list, 0, newest)?;
        assert!(cleanup.is_empty());
        assert!(!Rooted::ref_eq(&store, &list, &copied)?);
        let (_, captured) = outer.owner_capture(&store, owner, 0)?;
        let captured = captured.to_rooted(&mut store);
        let original = child.list_get(&mut store, captured, 0)?;
        assert!(Rooted::ref_eq(&store, &original, &replacement)?);
        assert!(child.release_owner_cleanup_items(&mut store, original)?.is_empty());
        assert_eq!(outer.release_owner_cleanup_items(&mut store, owner)?.len(), 2);
        assert_eq!(child.release_list_cleanup_items(&mut store, copied)?.len(), 1);
        for dead in [first, replacement, newest] { assert!(child.family_retain(&store, dead).is_err()); }
        Ok(())
    }

    // No Drop implementation: cleanup is observable only when explicitly run.
    struct Payload(Arc<AtomicUsize>);
    impl Payload {
        fn reclaim(self) { self.0.fetch_add(1, Ordering::SeqCst); }
    }

    #[test]
    fn opaque_owner_gc_payload_and_list_hold_actual_references() -> Result<()> {
        use wasmtime::{ArrayRef, ArrayRefPre, ArrayType, FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        let mut runtime = super::super::super::Runtime::new()?;
        let engine = runtime.engine.clone();
        let empty = wasmtime::Module::new(&engine, "(module)")?;
        let caps = super::super::super::Capabilities {
            print: true, quiet: true, ..super::super::super::Capabilities::none()
        };
        let vm = runtime.instantiate_module(crate::runtime::WorkerModule::Unadmitted(empty), caps, 1, None)?;
        let mut store = vm.store;
        // Test-only registration: production has no corresponding import yet.
        let mut linker = wasmtime::Linker::new(&engine);
        linker.func_wrap("test", "captured_console", super::super::console::host_print_captured)?;
        let consumer = wasmtime::Module::new(&engine, r#"(module
            (import "test" "captured_console" (func $print (param externref i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "log: ok")
            (func (export "call") (param externref)
                local.get 0 i32.const 0 i32.const 7 call $print))"#)?;
        let instance = linker.instantiate(&mut store, &consumer)?;
        let call = instance.get_typed_func::<Option<Rooted<ExternRef>>, ()>(&mut store, "call")?;
        let console = ExternRef::new(&mut store, ConsoleCapability)?;
        let payload_type = StructType::new(&engine, [
            FieldType::new(Mutability::Const, ValType::EXTERNREF.into()),
            FieldType::new(Mutability::Const, ValType::I32.into()),
        ])?;
        let binding = Binding { instance: Arc::new(InstanceIdentity::default()), constructor: Arc::new(()), family: Arc::new(()), owner_type: witchy_wir::canonical_safewir::GcTypeId::new(1), list_type: witchy_wir::canonical_safewir::GcTypeId::new((witchy_wir::canonical_safewir::GcTypeId::new(1)).get() + 1000), cleanup_tag: 1, environment_tag: 1, payload_type: payload_type.clone(), capture_fields: vec![CaptureField { capture: 0, field: 0, kind: CaptureFieldKind::Capability(witchy_wir::canonical_safewir::CapabilityKindId::new(0)) }].into_boxed_slice() };
        let payload_pre = StructRefPre::new(&mut store, payload_type.clone());
        // The integer is ordinary environment data, never used to find a cap.
        let payload = StructRef::new(&mut store, &payload_pre, &[Val::ExternRef(Some(console)), Val::I32(64)])?;
        let owner = binding.seal_gc(&mut store, payload)?;
        let wrong_type = StructType::new(&engine, [FieldType::new(Mutability::Const, ValType::I32.into())])?;
        let wrong_pre = StructRefPre::new(&mut store, wrong_type);
        let wrong_payload = StructRef::new(&mut store, &wrong_pre, &[Val::I32(64)])?;
        assert!(binding.seal_gc(&mut store, wrong_payload).is_err());
        let null_payload = StructRef::new(&mut store, &payload_pre, &[Val::ExternRef(None), Val::I32(64)])?;
        assert!(binding.seal_gc(&mut store, null_payload).is_err());
        type GcPayload = OwnedRooted<StructRef>;
        binding.retain::<GcPayload>(&store, owner)?;
        let list_type = ArrayType::new(&engine, FieldType::new(Mutability::Const, ValType::EXTERNREF.into()));
        let list_pre = ArrayRefPre::new(&mut store, list_type);
        let list = ArrayRef::new_fixed(&mut store, &list_pre, &[Val::ExternRef(Some(owner))])?;
        assert!(ArrayRef::new_fixed(&mut store, &list_pre, &[Val::I32(64)]).is_err());
        assert!(binding.release::<GcPayload>(&store, owner)?.is_none());
        // Twice after original cleanup: the list owns the remaining token.
        for _ in 0..2 {
            let Val::ExternRef(Some(element)) = list.get(&mut store, 0)? else { panic!("typed owner element") };
            binding.retain::<GcPayload>(&store, element)?;
            let actual = binding.console::<GcPayload>(&store, element)?;
            assert!(Rooted::ref_eq(&store, &console, &actual)?);
            let actual = actual.to_rooted(&mut store);
            call.call(&mut store, Some(actual))?;
            assert!(binding.release::<GcPayload>(&store, element)?.is_none());
        }
        assert_eq!(*store.data().output.lock().unwrap(), ["log: ok", "log: ok"]);
        assert!(call.call(&mut store, None).is_err());
        let forged_console = ExternRef::new(&mut store, 64_i32)?;
        assert!(call.call(&mut store, Some(forged_console)).is_err());
        assert!(list.get(&mut store, 1).is_err());
        let Val::ExternRef(Some(element)) = list.get(&mut store, 0)? else { panic!("typed owner element") };
        let returned = binding.release::<GcPayload>(&store, element)?.unwrap();
        assert!(matches!(returned.field(&mut store, 1)?, Val::I32(64)));
        let Val::ExternRef(Some(actual)) = returned.field(&mut store, 0)? else { panic!("actual captured capability") };
        assert!(Rooted::ref_eq(&store, &console, &actual)?);
        assert!(binding.console::<GcPayload>(&store, element).is_err());
        Ok(())
    }

    #[test]
    fn opaque_owner_lifecycle_and_substitution_negatives() -> Result<()> {
        let mut store = wasmtime::Store::new(&wasmtime::Engine::default(), ());
        let payload_type = wasmtime::StructType::new(store.engine(), [wasmtime::FieldType::new(wasmtime::Mutability::Const, wasmtime::ValType::EXTERNREF.into())])?;
        let binding = Binding { instance: Arc::new(InstanceIdentity::default()), constructor: Arc::new(()), family: Arc::new(()), owner_type: witchy_wir::canonical_safewir::GcTypeId::new(1), list_type: witchy_wir::canonical_safewir::GcTypeId::new((witchy_wir::canonical_safewir::GcTypeId::new(1)).get() + 1000), cleanup_tag: 1, environment_tag: 1, payload_type: payload_type.clone(), capture_fields: vec![CaptureField { capture: 0, field: 0, kind: CaptureFieldKind::Capability(witchy_wir::canonical_safewir::CapabilityKindId::new(0)) }].into_boxed_slice() };
        let wrong_constructor = Binding { instance: binding.instance.clone(), constructor: Arc::new(()), family: Arc::new(()), owner_type: binding.owner_type, list_type: witchy_wir::canonical_safewir::GcTypeId::new((binding.owner_type).get() + 1000), cleanup_tag: binding.cleanup_tag, environment_tag: 1, payload_type: payload_type.clone(), capture_fields: vec![CaptureField { capture: 0, field: 0, kind: CaptureFieldKind::Capability(witchy_wir::canonical_safewir::CapabilityKindId::new(0)) }].into_boxed_slice() };
        let foreign_instance = Binding { instance: Arc::new(InstanceIdentity::default()), constructor: binding.constructor.clone(), family: binding.family.clone(), owner_type: binding.owner_type, list_type: witchy_wir::canonical_safewir::GcTypeId::new((binding.owner_type).get() + 1000), cleanup_tag: binding.cleanup_tag, environment_tag: 1, payload_type, capture_fields: vec![CaptureField { capture: 0, field: 0, kind: CaptureFieldKind::Capability(witchy_wir::canonical_safewir::CapabilityKindId::new(0)) }].into_boxed_slice() };
        let console = ExternRef::new(&mut store, ConsoleCapability)?;
        let cleanup = Arc::new(AtomicUsize::new(0));
        let owner = binding.seal(&mut store, Payload(cleanup.clone()), &[console])?;
        let other = wrong_constructor.seal(&mut store, Payload(cleanup.clone()), &[console])?;
        assert!(binding.console::<Payload>(&store, other).is_err());
        assert!(foreign_instance.console::<Payload>(&store, owner).is_err());
        let forged = ExternRef::new(&mut store, 0_i32)?;
        assert!(binding.console::<Payload>(&store, forged).is_err());
        assert!(binding.seal(&mut store, Payload(cleanup.clone()), &[forged]).is_err());
        assert!(binding.console::<Payload>(&store, console).is_err());
        let captured = binding.console::<Payload>(&store, owner)?;
        assert!(captured.data(&store)?.unwrap().is::<ConsoleCapability>());
        assert!(Rooted::ref_eq(&store, &console, &captured)?);
        binding.retain::<Payload>(&store, owner)?;
        assert!(binding.release::<Payload>(&store, owner)?.is_none());
        assert_eq!(cleanup.load(Ordering::SeqCst), 0);
        let final_payload = binding.release::<Payload>(&store, owner)?.unwrap();
        assert!(binding.console::<Payload>(&store, owner).is_err());
        assert!(binding.retain::<Payload>(&store, owner).is_err());
        assert!(binding.release::<Payload>(&store, owner).is_err());
        final_payload.reclaim();
        assert_eq!(cleanup.load(Ordering::SeqCst), 1);
        wrong_constructor.release::<Payload>(&store, other)?.unwrap().reclaim();
        let replacement = binding.seal(&mut store, Payload(cleanup.clone()), &[console])?;
        assert!(binding.console::<Payload>(&store, owner).is_err());
        binding.release::<Payload>(&store, replacement)?.unwrap().reclaim();
        assert_eq!(cleanup.load(Ordering::SeqCst), 3);
        Ok(())
    }

    #[test]
    fn opaque_owner_list_drops_all_variants_and_refuses_dead_list() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let payload_type = StructType::new(&engine, [FieldType::new(Mutability::Const, ValType::EXTERNREF.into())])?;
        let instance = Arc::new(InstanceIdentity::default());
        let family = Arc::new(());
        let make_binding = || Binding {
            instance: instance.clone(), family: family.clone(), constructor: Arc::new(()),
            owner_type: witchy_wir::canonical_safewir::GcTypeId::new(1), list_type: witchy_wir::canonical_safewir::GcTypeId::new((witchy_wir::canonical_safewir::GcTypeId::new(1)).get() + 1000),
            cleanup_tag: 1,
            environment_tag: 1,
            payload_type: payload_type.clone(), capture_fields: vec![CaptureField {
                capture: 0, field: 0, kind: CaptureFieldKind::Capability(witchy_wir::canonical_safewir::CapabilityKindId::new(0)),
            }].into_boxed_slice(),
        };
        let first = make_binding();
        let second = make_binding();
        let pre = StructRefPre::new(&mut store, payload_type);
        let cap_a = ExternRef::new(&mut store, ConsoleCapability)?;
        let cap_b = ExternRef::new(&mut store, ConsoleCapability)?;
        let payload_a = StructRef::new(&mut store, &pre, &[Val::ExternRef(Some(cap_a))])?;
        let payload_b = StructRef::new(&mut store, &pre, &[Val::ExternRef(Some(cap_b))])?;
        let a = first.seal_gc(&mut store, payload_a)?;
        let b = second.seal_gc(&mut store, payload_b)?;
        let list = first.new_shared_list(&mut store, &[a, b])?;
        assert!(first.family_release(&store, a)?.is_none());
        assert!(first.family_release(&store, b)?.is_none());
        for (index, binding, cap) in [(0, &first, cap_a), (1, &second, cap_b)] {
            let element = first.list_get(&mut store, list, index)?;
            let actual = binding.capability::<OwnedRooted<StructRef>>(&store, element, 0)?;
            assert!(Rooted::ref_eq(&store, &cap, &actual)?);
            assert!(first.family_release(&store, element)?.is_none());
        }
        assert!(first.list_get(&mut store, list, 2).is_err());
        first.clone_list(&store, list)?;
        assert!(first.release_list_cleanup_items(&mut store, list)?.is_empty());
        let final_payloads = first.release_list_cleanup_items(&mut store, list)?;
        assert_eq!(final_payloads.len(), 2, "both element tokens need kernel teardown");
        assert!(first.list_get(&mut store, list, 0).is_err());
        assert!(first.clone_list(&store, list).is_err());
        assert!(first.release_list_cleanup_items(&mut store, list).is_err());
        assert!(first.family_retain(&store, a).is_err());
        assert!(second.family_retain(&store, b).is_err());
        Ok(())
    }

    #[test]
    fn nested_owner_fields_retain_read_and_cleanup_in_capture_order() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let child_type = witchy_wir::canonical_safewir::GcTypeId::new(41);
        let outer_type = witchy_wir::canonical_safewir::GcTypeId::new(42);
        let instance = test_instance(&[child_type, outer_type]);
        let child_payload_type = StructType::new(&engine, [
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::I32.into()),
        ])?;
        let outer_payload_type = StructType::new(&engine, [
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::EXTERNREF.into()),
            FieldType::new(Mutability::Const, ValType::EXTERNREF.into()),
        ])?;
        let child = Binding {
            instance: instance.clone(), constructor: Arc::new(()), family: instance.owner_types[&child_type].family.clone(),
            owner_type: child_type, list_type: witchy_wir::canonical_safewir::GcTypeId::new((child_type).get() + 1000), cleanup_tag: child_type.get(), environment_tag: 1, payload_type: child_payload_type.clone(), capture_fields: Box::new([]),
        };
        let outer = Binding {
            instance: instance.clone(), constructor: Arc::new(()), family: instance.owner_types[&outer_type].family.clone(),
            owner_type: outer_type, list_type: witchy_wir::canonical_safewir::GcTypeId::new((outer_type).get() + 1000), cleanup_tag: outer_type.get(), environment_tag: 1, payload_type: outer_payload_type.clone(),
            capture_fields: vec![
                CaptureField { capture: 0, field: 2, kind: CaptureFieldKind::Owner(child_type) },
                CaptureField { capture: 1, field: 3, kind: CaptureFieldKind::Capability(
                    witchy_wir::canonical_safewir::CapabilityKindId::new(0),
                ) },
            ].into_boxed_slice(),
        };
        let child_pre = StructRefPre::new(&mut store, child_payload_type);
        let child_payload = StructRef::new(&mut store, &child_pre, &[Val::I32(7), Val::I32(11)])?;
        let child_owner = child.seal_gc(&mut store, child_payload)?;
        let console = ExternRef::new(&mut store, ConsoleCapability)?;
        let outer_pre = StructRefPre::new(&mut store, outer_payload_type);
        let outer_payload = StructRef::new(&mut store, &outer_pre, &[
            Val::I32(13), Val::I32(17), Val::ExternRef(Some(child_owner)), Val::ExternRef(Some(console)),
        ])?;
        let outer_owner = outer.seal_gc(&mut store, outer_payload)?;

        let (actual_type, captured) = outer.owner_capture(&store, outer_owner, 0)?;
        assert_eq!(actual_type, child_type);
        let captured = captured.to_rooted(&mut store);
        child.family_retain(&store, captured)?;
        assert!(child.release_owner_cleanup_items(&mut store, child_owner)?.is_empty());
        assert!(child.release_owner_cleanup_items(&mut store, captured)?.is_empty());
        let cleanup = outer.release_owner_cleanup_items(&mut store, outer_owner)?;
        assert_eq!(cleanup.iter().map(|item| item.family_tag).collect::<Vec<_>>(), [outer_type.get(), child_type.get()]);

        let child_payload = StructRef::new(&mut store, &child_pre, &[Val::I32(19), Val::I32(23)])?;
        let child_owner = child.seal_gc(&mut store, child_payload)?;
        let outer_payload = StructRef::new(&mut store, &outer_pre, &[
            Val::I32(29), Val::I32(31), Val::ExternRef(Some(child_owner)), Val::ExternRef(Some(console)),
        ])?;
        let outer_owner = outer.seal_gc(&mut store, outer_payload)?;
        let list = outer.new_shared_list(&mut store, &[outer_owner])?;
        assert!(child.release_owner_cleanup_items(&mut store, child_owner)?.is_empty());
        assert!(outer.release_owner_cleanup_items(&mut store, outer_owner)?.is_empty());
        let cleanup = outer.release_list_cleanup_items(&mut store, list)?;
        assert_eq!(cleanup.iter().map(|item| item.family_tag).collect::<Vec<_>>(), [outer_type.get(), child_type.get()]);
        Ok(())
    }

    #[test]
    fn nested_owner_construction_rejects_substitution_without_retaining() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let child_type = witchy_wir::canonical_safewir::GcTypeId::new(51);
        let instance = test_instance(&[child_type]);
        let payload_type = StructType::new(&engine, [
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::I32.into()),
        ])?;
        let outer_payload_type = StructType::new(&engine, [
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::I32.into()),
            FieldType::new(Mutability::Const, ValType::EXTERNREF.into()),
        ])?;
        let child = Binding {
            instance: instance.clone(), constructor: Arc::new(()), family: instance.owner_types[&child_type].family.clone(),
            owner_type: child_type, list_type: witchy_wir::canonical_safewir::GcTypeId::new((child_type).get() + 1000), cleanup_tag: child_type.get(), environment_tag: 1, payload_type: payload_type.clone(), capture_fields: Box::new([]),
        };
        let rejecting_outer = |instance: Arc<InstanceIdentity>, expected| Binding {
            instance, constructor: Arc::new(()), family: Arc::new(()),
            owner_type: witchy_wir::canonical_safewir::GcTypeId::new(52), list_type: witchy_wir::canonical_safewir::GcTypeId::new((witchy_wir::canonical_safewir::GcTypeId::new(52)).get() + 1000),
            cleanup_tag: 52,
            environment_tag: 1,
            payload_type: outer_payload_type.clone(),
            capture_fields: vec![CaptureField { capture: 0, field: 2, kind: CaptureFieldKind::Owner(expected) }].into_boxed_slice(),
        };
        let child_pre = StructRefPre::new(&mut store, payload_type);
        let outer_pre = StructRefPre::new(&mut store, outer_payload_type.clone());

        let child_payload = StructRef::new(&mut store, &child_pre, &[Val::I32(1), Val::I32(2)])?;
        let owner = child.seal_gc(&mut store, child_payload)?;
        let wrong_type = rejecting_outer(instance.clone(), witchy_wir::canonical_safewir::GcTypeId::new(999));
        let payload = StructRef::new(&mut store, &outer_pre, &[Val::I32(3), Val::I32(4), Val::ExternRef(Some(owner))])?;
        assert!(wrong_type.seal_gc(&mut store, payload).is_err());
        assert_eq!(child.release_owner_cleanup_items(&mut store, owner)?.len(), 1, "failed type validation retained the child");

        let foreign_child = Binding {
            instance: Arc::new(InstanceIdentity::default()), constructor: Arc::new(()), family: Arc::new(()),
            owner_type: child_type, list_type: witchy_wir::canonical_safewir::GcTypeId::new((child_type).get() + 1000), cleanup_tag: child_type.get(), environment_tag: 1, payload_type: child.payload_type.clone(), capture_fields: Box::new([]),
        };
        let child_payload = StructRef::new(&mut store, &child_pre, &[Val::I32(5), Val::I32(6)])?;
        let owner = foreign_child.seal_gc(&mut store, child_payload)?;
        let outer = rejecting_outer(instance, child_type);
        let payload = StructRef::new(&mut store, &outer_pre, &[Val::I32(7), Val::I32(8), Val::ExternRef(Some(owner))])?;
        assert!(outer.seal_gc(&mut store, payload).is_err());
        assert_eq!(foreign_child.release_owner_cleanup_items(&mut store, owner)?.len(), 1, "failed instance validation retained the child");
        Ok(())
    }

    #[test]
    fn owner_tag_is_constructor_metadata_with_family_level_authentication() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = Arc::new(InstanceIdentity::default());
        let family = Arc::new(());
        let owner_type = witchy_wir::canonical_safewir::GcTypeId::new(61);
        let payload_type = StructType::new(
            &engine,
            [FieldType::new(Mutability::Const, ValType::I32.into())],
        )?;
        let binding = |constructor, family, owner_type, environment_tag| Binding {
            instance: instance.clone(),
            constructor,
            family,
            owner_type,
            list_type: witchy_wir::canonical_safewir::GcTypeId::new(owner_type.get() + 1000),
            cleanup_tag: owner_type.get(),
            environment_tag,
            payload_type: payload_type.clone(),
            capture_fields: Box::new([]),
        };
        let first = binding(Arc::new(()), family.clone(), owner_type, 11);
        let second = binding(Arc::new(()), family.clone(), owner_type, 22);
        let wrong_family = binding(Arc::new(()), Arc::new(()), owner_type, 33);
        let wrong_owner_type = binding(
            Arc::new(()),
            family.clone(),
            witchy_wir::canonical_safewir::GcTypeId::new(62),
            44,
        );
        let foreign_instance = Binding {
            instance: Arc::new(InstanceIdentity::default()),
            constructor: Arc::new(()),
            family: family.clone(),
            owner_type,
            list_type: witchy_wir::canonical_safewir::GcTypeId::new(owner_type.get() + 1000),
            cleanup_tag: owner_type.get(),
            environment_tag: 55,
            payload_type: payload_type.clone(),
            capture_fields: Box::new([]),
        };
        let payload_pre = StructRefPre::new(&mut store, payload_type);
        let payload = StructRef::new(&mut store, &payload_pre, &[Val::I32(999)])?;
        let owner = second.seal_gc(&mut store, payload)?;

        assert_eq!(first.family_tag(&store, owner)?, 22);
        assert!(wrong_family.family_tag(&store, owner).is_err());
        assert!(wrong_owner_type.family_tag(&store, owner).is_err());
        assert!(foreign_instance.family_tag(&store, owner).is_err());
        let forged = ExternRef::new(&mut store, 22_i64)?;
        assert!(first.family_tag(&store, forged).is_err());

        let wrong_payload = StructRef::new(&mut store, &payload_pre, &[Val::I32(44)])?;
        let wrong_owner = wrong_owner_type.seal_gc(&mut store, wrong_payload)?;
        assert!(first.family_tag(&store, wrong_owner).is_err());
        assert!(wrong_owner_type.family_release(&store, wrong_owner)?.is_some());

        assert!(first.family_release(&store, owner)?.is_some());
        assert!(first.family_tag(&store, owner).is_err());
        Ok(())
    }

    #[test]
    fn owner_list_set_is_bounds_checked_copy_on_write_and_cleans_replacement_once() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = Arc::new(InstanceIdentity::default());
        let family = Arc::new(());
        let owner_type = witchy_wir::canonical_safewir::GcTypeId::new(71);
        let payload_type = StructType::new(
            &engine,
            [FieldType::new(Mutability::Const, ValType::I32.into())],
        )?;
        let binding = Binding {
            instance: instance.clone(),
            constructor: Arc::new(()),
            family: family.clone(),
            owner_type,
            list_type: witchy_wir::canonical_safewir::GcTypeId::new(owner_type.get() + 1000),
            cleanup_tag: owner_type.get(),
            environment_tag: 1,
            payload_type: payload_type.clone(),
            capture_fields: Box::new([]),
        };
        let wrong_family = Binding {
            instance,
            constructor: Arc::new(()),
            family: Arc::new(()),
            owner_type,
            list_type: witchy_wir::canonical_safewir::GcTypeId::new(owner_type.get() + 1000),
            cleanup_tag: owner_type.get(),
            environment_tag: 2,
            payload_type: payload_type.clone(),
            capture_fields: Box::new([]),
        };
        let payload_pre = StructRefPre::new(&mut store, payload_type);
        let make_owner = |store: &mut wasmtime::Store<()>, binding: &Binding, value| {
            let payload = StructRef::new(store.as_context_mut(), &payload_pre, &[Val::I32(value)])?;
            binding.seal_gc(store, payload)
        };
        let payload_value = |store: &mut wasmtime::Store<()>, item: CleanupItem| -> Result<i32> {
            let Val::I32(value) = item.payload.field(store.as_context_mut(), 0)? else {
                bail!("owner test payload type");
            };
            Ok(value)
        };

        let a = make_owner(&mut store, &binding, 10)?;
        let b = make_owner(&mut store, &binding, 20)?;
        let c = make_owner(&mut store, &binding, 30)?;
        let d = make_owner(&mut store, &binding, 40)?;
        let list = binding.new_shared_list(&mut store, &[a, b])?;
        assert!(binding.family_release(&store, a)?.is_none());
        assert!(binding.family_release(&store, b)?.is_none());

        let (updated, cleanup) = binding.set_list(&mut store, list, 0, c)?;
        assert_eq!(cleanup.len(), 1);
        assert_eq!(payload_value(&mut store, cleanup.into_iter().next().expect("replacement cleanup"))?, 10);
        assert!(Rooted::ref_eq(&store, &list, &updated)?);
        assert!(binding.family_retain(&store, a).is_err(), "replaced token must be released");
        let actual = binding.list_get(&mut store, updated, 0)?;
        assert!(Rooted::ref_eq(&store, &c, &actual)?);
        assert!(binding.family_release(&store, actual)?.is_none());

        assert!(binding.set_list(&mut store, updated, 2, d).is_err());
        assert_eq!(binding.list_length(&store, updated)?, 2);
        assert_eq!(binding.family_tag(&store, d)?, 1, "bounds refusal consumed the value");

        let substituted = make_owner(&mut store, &wrong_family, 60)?;
        assert!(binding.set_list(&mut store, updated, 1, substituted).is_err());
        assert_eq!(wrong_family.family_tag(&store, substituted)?, 2);

        binding.clone_list(&store, updated)?;
        let e = make_owner(&mut store, &binding, 50)?;
        let (copied, cleanup) = binding.set_list(&mut store, updated, 1, e)?;
        assert!(cleanup.is_empty());
        assert!(!Rooted::ref_eq(&store, &updated, &copied)?);
        let original_value = binding.list_get(&mut store, updated, 1)?;
        assert!(Rooted::ref_eq(&store, &b, &original_value)?);
        assert!(binding.family_release(&store, original_value)?.is_none());
        let copied_value = binding.list_get(&mut store, copied, 1)?;
        assert!(Rooted::ref_eq(&store, &e, &copied_value)?);
        assert!(binding.family_release(&store, copied_value)?.is_none());

        let mut reclaimed = Vec::new();
        for item in binding.release_owner_cleanup_items(&mut store, d)? {
            reclaimed.push(payload_value(&mut store, item)?);
        }
        for item in wrong_family.release_owner_cleanup_items(&mut store, substituted)? {
            assert_eq!(payload_value(&mut store, item)?, 60);
        }
        for item in binding.release_list_cleanup_items(&mut store, updated)? {
            reclaimed.push(payload_value(&mut store, item)?);
        }
        for item in binding.release_list_cleanup_items(&mut store, copied)? {
            reclaimed.push(payload_value(&mut store, item)?);
        }
        reclaimed.sort_unstable();
        assert_eq!(reclaimed, [20, 30, 40, 50]);
        Ok(())
    }

    #[test]
    fn owner_list_transfer_keeps_replacement_cleanup_and_duplicate_tokens() -> Result<()> {
        use wasmtime::{FieldType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, ());
        let payload_type = StructType::new(
            &engine,
            [FieldType::new(Mutability::Const, ValType::I32.into())],
        )?;
        let binding = Binding {
            instance: Arc::new(InstanceIdentity::default()),
            constructor: Arc::new(()),
            family: Arc::new(()),
            owner_type: witchy_wir::canonical_safewir::GcTypeId::new(81), list_type: witchy_wir::canonical_safewir::GcTypeId::new((witchy_wir::canonical_safewir::GcTypeId::new(81)).get() + 1000),
            cleanup_tag: 81,
            environment_tag: 1,
            payload_type: payload_type.clone(),
            capture_fields: Box::new([]),
        };
        let payload_pre = StructRefPre::new(&mut store, payload_type);
        let make_owner = |store: &mut wasmtime::Store<()>, value| {
            let payload = StructRef::new(store.as_context_mut(), &payload_pre, &[Val::I32(value)])?;
            binding.seal_gc(store, payload)
        };

        let replaced = make_owner(&mut store, 10)?;
        let current = make_owner(&mut store, 20)?;
        let list = binding.new_shared_list(&mut store, &[replaced])?;
        assert!(binding.family_release(&store, replaced)?.is_none());
        let (list, cleanup) = binding.set_list(&mut store, list, 0, current)?;
        assert_eq!(cleanup.len(), 1, "replacement cleanup must drain with ListSet");
        let Val::I32(replaced_value) = cleanup[0].payload.field(&mut store, 0)? else {
            bail!("owner test payload type");
        };
        assert_eq!(replaced_value, 10);
        assert!(binding.family_retain(&store, replaced).is_err());

        let duplicate = binding.list_get(&mut store, list, 0)?;
        let (list, cleanup) = binding.push_list(&mut store, list, duplicate)?;
        assert!(cleanup.is_empty());
        assert_eq!(binding.list_length(&store, list)?, 2);
        for index in 0..2 {
            let actual = binding.list_get(&mut store, list, index)?;
            assert!(Rooted::ref_eq(&store, &current, &actual)?);
            assert!(binding.family_release(&store, actual)?.is_none());
        }

        let mut reclaimed = binding.release_list_cleanup_items(&mut store, list)?
            .into_iter()
            .map(|item| {
                let Val::I32(value) = item.payload.field(&mut store, 0)? else {
                    bail!("owner test payload type");
                };
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        reclaimed.sort_unstable();
        assert_eq!(reclaimed, [20]);
        assert!(binding.family_retain(&store, current).is_err());
        Ok(())
    }
}
