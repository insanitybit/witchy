//! Instance-bound owner bridges registered only from an admitted structural recipe.
use std::sync::{Arc, Mutex};
use wasmtime::{bail, AsContextMut, ExternRef, OwnedRooted, Result, Rooted};

/// This entry point takes the immutable admitted artifact, not raw Wasm bytes
/// or a name-selected contract. Each invocation mints fresh instance identity.
pub(in crate::runtime) fn register(
    linker: &mut wasmtime::Linker<super::super::VmState>,
    artifact: &witchy_wir::AdmittedCanonicalArtifact,
) -> Result<()> {
    use std::collections::BTreeMap;
    use wasmtime::{FieldType, FuncType, Mutability, StructRef, StructRefPre, StructType, Val, ValType};
    use witchy_wir::canonical_safewir::{ValueType, closure_owner_contract::CaptureStorage};
    use witchy_wir::OwnerBridgeRecipe as R;
    let instance = Arc::new(());
    let mut families = BTreeMap::new();
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
                let (ValueType::Capability(kind), CaptureStorage::OwnerField(field)) = (capture.value_type, capture.storage) else {
                    bail!("invalid opaque owner capture field");
                };
                if field < 2 || field as usize >= slots { bail!("invalid owner capture field order"); }
                captures.push(CaptureField { capture: ordinal as u32, field: field as usize, kind });
            }
            constructors.insert(constructor.id.0, Arc::new(Binding {
                instance: instance.clone(), constructor: Arc::new(()), family: family.clone(),
                payload_type: StructType::new(linker.engine(), fields)?, capability_fields: captures.into_boxed_slice(),
            }));
        }
    }
    let wasm_type = |ty: &ValueType| -> Result<ValType> {
        Ok(match ty {
            ValueType::Gc(_) | ValueType::Capability(_) => ValType::EXTERNREF,
            ValueType::Size | ValueType::Callable(_) | ValueType::Ref(_) => ValType::I32,
            _ => bail!("unsupported private owner ABI lane"),
        })
    };
    for bridge in artifact.owner_bridges() {
        let constructor = match bridge.recipe() {
            R::Construct(c) | R::ReadCapability(c, _) | R::CloneEnvironment(c) => *c,
            _ => bridge.family().constructors.first().ok_or_else(|| wasmtime::Error::msg("empty owner family"))?.id,
        };
        let binding = constructors.get(&constructor.0).ok_or_else(|| wasmtime::Error::msg("missing owner binding"))?.clone();
        let recipe = bridge.recipe().clone();
        let code = bridge.callable_indices().iter().find(|(id, _)| *id == constructor)
            .ok_or_else(|| wasmtime::Error::msg("missing owner callable identity"))?.1;
        let allowed_codes = bridge.callable_indices().iter().map(|(_, code)| *code as i32).collect::<Vec<_>>();
        let parameters = bridge.parameters().iter().map(&wasm_type).collect::<Result<Vec<_>>>()?;
        let results = if bridge.payload_result() { vec![ValType::ANYREF] } else { bridge.results().iter().map(&wasm_type).collect::<Result<Vec<_>>>()? };
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
let mut fields = vec![Val::I32(*environment), Val::I32(code as i32)];
fields.extend(binding.payload_type.fields().skip(2).map(|_| Val::ExternRef(None)));
if arguments.len() != binding.capability_fields.len() + 1 { bail!("owner capture arity"); }
for (field, value) in binding.capability_fields.iter().zip(&arguments[1..]) {
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
                    let payload = binding.family_release(&caller, owner(0)?)?;
                    results[0] = Val::AnyRef(payload.map(|p| p.to_rooted(&mut caller).to_anyref()));
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
    results[0] = Val::ExternRef(Some(binding.push_list(&mut caller, owner(0)?, owner(1)?)?));
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
    instance: Arc<()>,
    constructor: Arc<()>,
    family: Arc<()>,
    payload_type: wasmtime::StructType,
    capability_fields: Box<[CaptureField]>,
}

struct CaptureField {
    capture: u32,
    field: usize,
    kind: witchy_wir::canonical_safewir::CapabilityKindId,
}

fn require_capability(store: impl wasmtime::AsContext, token: Rooted<ExternRef>, kind: witchy_wir::canonical_safewir::CapabilityKindId) -> Result<()> {
    let context = store.as_context();
    let data = token.data(context)?.ok_or_else(|| wasmtime::Error::msg("capability has no host data"))?;
    let matches = match kind.get() {
        0 => data.is::<super::super::ConsoleCapability>(),
        1 => data.is::<super::super::DirAuthority>(),
        2 => data.is::<super::super::EnvAuthority>(),
        3 => data.is::<super::super::FileAuthority>(),
        _ => false,
    };
    if !matches { bail!("closure capture has wrong capability kind"); }
    Ok(())
}

struct Live<P> {
    references: usize,
    payload: Option<P>,
    capabilities: Vec<OwnedRooted<ExternRef>>,
}

struct Owner<P> {
    instance: Arc<()>,
    constructor: Arc<()>,
    family: Arc<()>,
    live: Mutex<Live<P>>,
}

impl Binding {
    // The immutable binding owns the exact checked layout and capture mapping.
    // A bridge call supplies neither a layout nor a field selector.
    fn seal_gc(
        &self,
        mut store: impl AsContextMut,
        payload: Rooted<wasmtime::StructRef>,
    ) -> Result<Rooted<ExternRef>> {
        if !wasmtime::StructType::eq(&payload.ty(store.as_context())?, &self.payload_type)
            || self.payload_type.fields().any(|field| field.mutability() != wasmtime::Mutability::Const) {
            bail!("closure payload does not match immutable constructor layout");
        }
        let mut capabilities = Vec::with_capacity(self.capability_fields.len());
        for field in &self.capability_fields {
            let wasmtime::Val::ExternRef(Some(token)) = payload.field(&mut store, field.field)? else {
                bail!("closure payload lacks actual capability capture");
            };
            capabilities.push(token);
        }
        let payload = payload.to_owned_rooted(&mut store)?;
        self.seal(&mut store, payload, &capabilities)
    }

    // P is the rooted typed GC payload in the eventual bridge. Keeping the
    // lifecycle generic lets the explicit cleanup handoff be tested without
    // enabling unreviewed GC construction or pretending a scalar is a payload.
    fn seal<P: Send + Sync + 'static>(
        &self,
        mut store: impl AsContextMut,
        payload: P,
        capabilities: &[Rooted<ExternRef>],
    ) -> Result<Rooted<ExternRef>> {
        if capabilities.len() != self.capability_fields.len() { bail!("closure capability arity mismatch"); }
        let mut rooted = Vec::with_capacity(capabilities.len());
        for (token, field) in capabilities.iter().zip(&self.capability_fields) {
            require_capability(store.as_context(), *token, field.kind)?;
            rooted.push(token.to_owned_rooted(&mut store)?);
        }
        ExternRef::new(&mut store, Owner {
            instance: self.instance.clone(),
            constructor: self.constructor.clone(),
            family: self.family.clone(),
            live: Mutex::new(Live { references: 1, payload: Some(payload), capabilities: rooted }),
        })
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
            || !Arc::ptr_eq(&self.constructor, &owner.constructor) {
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
    fn capability<P: Send + Sync + 'static>(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>, capture: u32) -> Result<OwnedRooted<ExternRef>> {
        let index = self.capability_fields.iter().position(|field| field.capture == capture)
            .ok_or_else(|| wasmtime::Error::msg("not a capability capture"))?;
        self.access::<P, _>(store, owner, |live| {
            live.capabilities.get(index).cloned().ok_or_else(|| wasmtime::Error::msg("missing captured capability"))
        })
    }

    #[cfg(test)]
    fn console<P: Send + Sync + 'static>(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<OwnedRooted<ExternRef>> {
        self.capability::<P>(store, owner, 0)
    }

    // No callback, guest invocation, or GC finalizer performs RC destruction.
    // Final release retires the owner before transferring P to the kernel.
    fn release<P: Send + Sync + 'static>(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<Option<P>> {
        self.access::<P, _>(store, owner, |live| {
            live.references -= 1;
            if live.references != 0 { return Ok(None); }
            live.capabilities.clear();
            Ok(live.payload.take())
        })
    }
}

struct OwnerList {
    instance: Arc<()>,
    family: Arc<()>,
    live: Mutex<(usize, Option<OwnedRooted<wasmtime::ArrayRef>>)>,
}

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
        if !Arc::ptr_eq(&self.instance, &owner.instance) || !Arc::ptr_eq(&self.family, &owner.family) {
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

    fn family_release(&self, store: impl wasmtime::AsContext, owner: Rooted<ExternRef>) -> Result<Option<OwnedRooted<wasmtime::StructRef>>> {
        self.family_access(store, owner, |live| {
            live.references -= 1;
            if live.references != 0 { return Ok(None); }
            live.capabilities.clear();
            Ok(live.payload.take())
        })
    }

    fn list_access<R>(
        &self,
        store: impl wasmtime::AsContext,
        list: Rooted<ExternRef>,
        action: impl FnOnce(&mut (usize, Option<OwnedRooted<wasmtime::ArrayRef>>)) -> Result<R>,
    ) -> Result<R> {
        let context = store.as_context();
        let list = list.data(context)?.and_then(|data| data.downcast_ref::<OwnerList>())
            .ok_or_else(|| wasmtime::Error::msg("not an owner list"))?;
        if !Arc::ptr_eq(&self.instance, &list.instance) || !Arc::ptr_eq(&self.family, &list.family) {
            bail!("foreign owner list family");
        }
        let mut live = list.live.lock().map_err(|_| wasmtime::Error::msg("poisoned owner list"))?;
        if live.0 == 0 { bail!("released owner list"); }
        action(&mut live)
    }

    // Explicitly clones the incoming tokens, as a shared list construction.
    // There is no host lookup from an element index to an authority table:
    // the array itself contains the actual references supplied by the caller.
    fn new_shared_list(&self, mut store: impl AsContextMut, elements: &[Rooted<ExternRef>]) -> Result<Rooted<ExternRef>> {
        use wasmtime::{ArrayRef, ArrayRefPre, ArrayType, FieldType, Mutability, Val, ValType};
        for element in elements { self.family_access(store.as_context(), *element, |_| Ok(()))?; }
        let ty = ArrayType::new(store.as_context().engine(), FieldType::new(Mutability::Const, ValType::EXTERNREF.into()));
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
            instance: self.instance.clone(), family: self.family.clone(), live: Mutex::new((1, Some(array))),
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

// ListPush consumes one list token and one element token, while preserving
// other list aliases. Construction first retains every actual element, so
// releasing the two input tokens cannot retire a payload during this step.
fn push_list(&self, mut store: impl AsContextMut, list: Rooted<ExternRef>, value: Rooted<ExternRef>) -> Result<Rooted<ExternRef>> {
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
    if !self.release_list(&mut store, list)?.is_empty() {
        bail!("owner list transfer retired a retained element");
    }
    if self.family_release(store.as_context(), value)?.is_some() {
        bail!("owner list transfer retired its retained input");
    }
    Ok(result)
}

// Only final payloads cross back to the private Wasm destructor. The
// immutable GC array has actual anyrefs; no numeric authority lookup or
// host-to-guest callback participates in list destruction.
fn release_list_payload_array(&self, mut store: impl AsContextMut, list: Rooted<ExternRef>) -> Result<Option<Rooted<wasmtime::AnyRef>>> {
    use wasmtime::{ArrayRef, ArrayRefPre, ArrayType, FieldType, Mutability, Val, ValType};
    let payloads = self.release_list(&mut store, list)?;
    if payloads.is_empty() { return Ok(None); }
    let ty = ArrayType::new(store.as_context().engine(), FieldType::new(Mutability::Const, ValType::ANYREF.into()));
    let pre = ArrayRefPre::new(&mut store, ty);
    let values = payloads.iter().map(|payload| Val::AnyRef(Some(payload.to_rooted(&mut store).to_anyref()))).collect::<Vec<_>>();
    Ok(Some(ArrayRef::new_fixed(&mut store, &pre, &values)?.to_anyref()))
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
    fn release_list(&self, mut store: impl AsContextMut, list: Rooted<ExternRef>) -> Result<Vec<OwnedRooted<wasmtime::StructRef>>> {
        let array = self.list_access(store.as_context(), list, |live| {
            live.0 -= 1;
            Ok(if live.0 == 0 { live.1.take() } else { None })
        })?;
        let Some(array) = array else { return Ok(Vec::new()) };
        let mut final_payloads = Vec::new();
        for index in 0..array.len(store.as_context())? {
            let wasmtime::Val::ExternRef(Some(owner)) = array.get(&mut store, index)? else { bail!("invalid owner list element") };
            if let Some(payload) = self.family_release(store.as_context(), owner)? { final_payloads.push(payload); }
        }
        Ok(final_payloads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        let vm = runtime.instantiate_module(empty, caps, 1, None, None)?;
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
        let binding = Binding { instance: Arc::new(()), constructor: Arc::new(()), family: Arc::new(()), payload_type: payload_type.clone(), capability_fields: vec![CaptureField { capture: 0, field: 0, kind: witchy_wir::canonical_safewir::CapabilityKindId::new(0) }].into_boxed_slice() };
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
        let binding = Binding { instance: Arc::new(()), constructor: Arc::new(()), family: Arc::new(()), payload_type: payload_type.clone(), capability_fields: vec![CaptureField { capture: 0, field: 0, kind: witchy_wir::canonical_safewir::CapabilityKindId::new(0) }].into_boxed_slice() };
        let wrong_constructor = Binding { instance: binding.instance.clone(), constructor: Arc::new(()), family: Arc::new(()), payload_type: payload_type.clone(), capability_fields: vec![CaptureField { capture: 0, field: 0, kind: witchy_wir::canonical_safewir::CapabilityKindId::new(0) }].into_boxed_slice() };
        let foreign_instance = Binding { instance: Arc::new(()), constructor: binding.constructor.clone(), family: binding.family.clone(), payload_type, capability_fields: vec![CaptureField { capture: 0, field: 0, kind: witchy_wir::canonical_safewir::CapabilityKindId::new(0) }].into_boxed_slice() };
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
        let instance = Arc::new(());
        let family = Arc::new(());
        let make_binding = || Binding {
            instance: instance.clone(), family: family.clone(), constructor: Arc::new(()),
            payload_type: payload_type.clone(), capability_fields: vec![CaptureField {
                capture: 0, field: 0, kind: witchy_wir::canonical_safewir::CapabilityKindId::new(0),
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
        assert!(first.release_list(&mut store, list)?.is_empty());
        let final_payloads = first.release_list(&mut store, list)?;
        assert_eq!(final_payloads.len(), 2, "both element tokens need kernel teardown");
        assert!(first.list_get(&mut store, list, 0).is_err());
        assert!(first.clone_list(&store, list).is_err());
        assert!(first.release_list(&mut store, list).is_err());
        assert!(first.family_retain(&store, a).is_err());
        assert!(second.family_retain(&store, b).is_err());
        Ok(())
    }
}
