//! Independent, byte-copy semantics for admitted persistent worker services.
//!
//! Each worker owns an interpreter heap. Only copied bytes cross the boundary;
//! service and response tokens are private resources of the calling instance.

use std::sync::Arc;

use super::*;
use crate::canonical_safewir::{HostParameterRole, HostResultRole, ImportContract};

const SERVICE_KIND: u32 = 7;
const RESPONSE_KIND: u32 = 8;

#[derive(Debug, Clone)]
enum Resource {
    Service(u64),
    Response(Arc<[u8]>),
}

#[derive(Debug, Clone)]
pub(super) struct Token {
    issuer: Arc<()>,
    resource: Resource,
}

impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.issuer, &other.issuer) && match (&self.resource, &other.resource) {
            (Resource::Service(a), Resource::Service(b)) => a == b,
            (Resource::Response(a), Resource::Response(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for Token {}

struct Worker<'a> {
    engine: Box<Engine<'a>>,
    callback: FunctionId,
    state: Option<u64>,
    bytes_layout: LayoutId,
}

pub(super) struct ServiceState<'a> {
    issuer: Arc<()>,
    next_id: u64,
    // None is a failed service awaiting its one permitted cleanup close.
    // Absence means an unknown or already closed service.
    workers: BTreeMap<u64, Option<Worker<'a>>>,
}

impl<'a> ServiceState<'a> {
    pub(super) fn workers_empty(&self) -> bool { self.workers.is_empty() }
    pub(super) fn new() -> Self {
        Self { issuer: Arc::new(()), next_id: 0, workers: BTreeMap::new() }
    }

    fn take_worker(&mut self, id: u64) -> Result<Worker<'a>, InterpretError> {
        self.workers.get_mut(&id).and_then(Option::take).ok_or(InterpretError::ExplicitTrap)
    }

    fn close_worker(&mut self, id: u64) -> Result<Option<Worker<'a>>, InterpretError> {
        self.workers.remove(&id).ok_or(InterpretError::ExplicitTrap)
    }

    fn mint(&self, resource: Resource) -> CapabilityToken {
        let kind = match &resource {
            Resource::Service(_) => SERVICE_KIND,
            Resource::Response(_) => RESPONSE_KIND,
        };
        CapabilityToken {
            service: Some(Token { issuer: self.issuer.clone(), resource }),
            ..CapabilityToken::root(kind)
        }
    }

    fn authenticate<'b>(&self, capability: &'b CapabilityToken) -> Result<&'b Resource, InterpretError> {
        let token = capability.service.as_ref().ok_or(InterpretError::TypeMismatch("worker resource token"))?;
        if !Arc::ptr_eq(&self.issuer, &token.issuer) {
            return Err(InterpretError::TypeMismatch("worker resource belongs to another instance"));
        }
        let expected = match token.resource {
            Resource::Service(_) => SERVICE_KIND,
            Resource::Response(_) => RESPONSE_KIND,
        };
        if capability.kind != expected {
            return Err(InterpretError::TypeMismatch("worker resource kind"));
        }
        Ok(&token.resource)
    }
}

pub(super) fn is_import(name: &str) -> bool {
    matches!(name,
        "vm_service_start" | "vm_service_step" | "vm_service_response_size" |
        "vm_service_response_into" | "vm_service_close")
}

pub(super) fn isolated_import_allowed(contract: &ImportContract) -> bool {
    // The legacy print import has ambient console authority rather than a
    // capability operand. It cannot be used by a zero-grant worker either.
    if contract.name == "print" { return false; }
    let internal = |kind: u32| matches!(kind, SERVICE_KIND | RESPONSE_KIND);
    contract.parameter_roles.iter().all(|role| match role {
        HostParameterRole::Capability(kind) => internal(kind.get()),
        _ => true,
    }) && contract.result_roles.iter().all(|role| match role {
        HostResultRole::Capability(kind) => internal(kind.get()),
        _ => true,
    })
}

impl<'a> Engine<'a> {
    pub(super) fn vm_service_host_call(
        &mut self,
        name: &str,
        destinations: &[ValueId],
        arguments: &[HostCallArgument],
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let result = match name {
            "vm_service_start" => {
                if arguments.len() != 2 { return Err(InterpretError::TypeMismatch("worker start arguments")); }
                let HostCallArgument::Value(site) = &arguments[0] else {
                    return Err(InterpretError::TypeMismatch("worker site id"));
                };
                let InterpreterValue::Int(site) = values.get(site).ok_or(InterpretError::TypeMismatch("undefined worker site"))? else {
                    return Err(InterpretError::TypeMismatch("worker site id"));
                };
                let site = u32::try_from(*site).map_err(|_| InterpretError::TypeMismatch("worker site id"))?;
                let callback = self.module.vm_services().iter().find(|binding| binding.id() == site)
                    .ok_or(InterpretError::TypeMismatch("unbound worker site"))?.callback();
                let initial = self.service_input(&arguments[1], values)?;
                let (_, bytes_layout) = crate::canonical_launch::argument_layouts()
                    .map_err(|_| InterpretError::TypeMismatch("worker byte layout"))?;
                let mut config = self.config.clone();
                config.fixture = OracleFixture::default();
                config.fixture.dir_grants = Some(0);
                let mut engine = Box::new(Engine::new(self.module, config));
                engine.isolated = true;
                let state = engine.service_allocate_bytes(bytes_layout, &initial)?;
                let id = self.services.next_id;
                self.services.next_id = id.checked_add(1).ok_or(InterpretError::ArithmeticTrap)?;
                self.services.workers.insert(id, Some(Worker {
                    engine, callback, state: Some(state), bytes_layout,
                }));
                Some(InterpreterValue::Capability(self.services.mint(Resource::Service(id))))
            }
            "vm_service_step" => {
                if arguments.len() != 2 { return Err(InterpretError::TypeMismatch("worker step arguments")); }
                let id = self.service_id(&arguments[0], values)?;
                // Removing first makes any callback trap terminal. No partially
                // consumed state is made available to a subsequent request.
                let mut worker = self.services.take_worker(id)?;
                let request = self.service_input(&arguments[1], values)?;
                let request = worker.engine.service_allocate_bytes(worker.bytes_layout, &request)?;
                let state = worker.state.take().ok_or(InterpretError::ExplicitTrap)?;
                let before = worker.engine.steps;
                let remaining = self.config.max_steps.checked_sub(self.steps)
                    .ok_or(InterpretError::StepLimitExceeded)?;
                worker.engine.config.max_steps = before.checked_add(remaining)
                    .ok_or(InterpretError::StepLimitExceeded)?;
                let execution = worker.engine.execute(worker.callback, vec![
                    InterpreterValue::ObjectRef(state), InterpreterValue::ObjectRef(request),
                ]);
                let elapsed = worker.engine.steps.checked_sub(before)
                    .ok_or(InterpretError::StepLimitExceeded)?;
                self.steps = self.steps.checked_add(elapsed).ok_or(InterpretError::StepLimitExceeded)?;
                if self.steps > self.config.max_steps { return Err(InterpretError::StepLimitExceeded); }
                let returned = execution?;
                let [InterpreterValue::ObjectRef(state)] = returned.as_slice() else {
                    return Err(InterpretError::TypeMismatch("worker callback must return owned Bytes"));
                };
                let response = worker.engine.service_read_bytes(*state, worker.bytes_layout)?;
                if !worker.engine.output.is_empty() {
                    return Err(InterpretError::TypeMismatch("isolated worker emitted ambient output"));
                }
                worker.state = Some(*state);
                self.services.workers.insert(id, Some(worker));
                Some(InterpreterValue::Capability(self.services.mint(Resource::Response(response.into()))))
            }
            "vm_service_response_size" => {
                if arguments.len() != 1 { return Err(InterpretError::TypeMismatch("worker response size arguments")); }
                let response = self.service_response(&arguments[0], values)?;
                Some(InterpreterValue::Size(response.len() as u64))
            }
            "vm_service_response_into" => {
                if arguments.len() != 2 { return Err(InterpretError::TypeMismatch("worker response copy arguments")); }
                let response = self.service_response(&arguments[0], values)?;
                let HostCallArgument::Buffer { place, .. } = &arguments[1] else {
                    return Err(InterpretError::TypeMismatch("worker response output buffer"));
                };
                let (object, start, capacity) = self.slice_place(place, values)?;
                self.check_range(object, start, capacity, true)?;
                let required = response.len() as u64;
                if required > SIZE_MAX { return Err(InterpretError::ArithmeticTrap); }
                if required <= capacity {
                    let object = self.object_mut(object)?;
                    for (offset, byte) in response.iter().enumerate() {
                        object.cells.insert(CellKey::Element(start + offset as u64), InterpreterValue::Size(u64::from(*byte)));
                    }
                }
                Some(InterpreterValue::Size(required))
            }
            "vm_service_close" => {
                if arguments.len() != 1 { return Err(InterpretError::TypeMismatch("worker close arguments")); }
                let id = self.service_id(&arguments[0], values)?;
                if let Some(mut worker) = self.services.close_worker(id)? {
                    let state = worker.state.take().ok_or(InterpretError::ExplicitTrap)?;
                    worker.engine.release(state)?;
                    if worker.engine.objects.values().any(|object| matches!(object.storage, RuntimeStorage::Allocation(_)))
                        || !worker.engine.owners.is_empty() || !worker.engine.owner_lists.is_empty()
                        || !worker.engine.services.workers.is_empty()
                    {
                        return Err(InterpretError::UnsupportedOperation("worker leaked an owned allocation or service"));
                    }
                }
                None
            }
            _ => return Err(InterpretError::UnsupportedHostImport),
        };
        match (destinations, result) {
            ([destination], Some(result)) => { values.insert(*destination, result); }
            ([], None) => {}
            _ => return Err(InterpretError::TypeMismatch("worker import result arity")),
        }
        Ok(())
    }

    fn service_input(
        &self, argument: &HostCallArgument, values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<Vec<u8>, InterpretError> {
        let HostCallArgument::Buffer { place, .. } = argument else {
            return Err(InterpretError::TypeMismatch("worker byte input buffer"));
        };
        self.host_bytes(place, values)
    }

    fn service_resource<'b>(
        &self, argument: &HostCallArgument, values: &'b BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<&'b Resource, InterpretError> {
        let HostCallArgument::Capability(id) = argument else {
            return Err(InterpretError::TypeMismatch("worker resource argument"));
        };
        let InterpreterValue::Capability(capability) = values.get(id).ok_or(InterpretError::TypeMismatch("undefined worker resource"))? else {
            return Err(InterpretError::TypeMismatch("worker resource value"));
        };
        self.services.authenticate(capability)
    }

    fn service_id(
        &self, argument: &HostCallArgument, values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<u64, InterpretError> {
        match self.service_resource(argument, values)? {
            Resource::Service(id) => Ok(*id),
            _ => Err(InterpretError::TypeMismatch("expected service, not response")),
        }
    }

    fn service_response(
        &self, argument: &HostCallArgument, values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<Arc<[u8]>, InterpretError> {
        match self.service_resource(argument, values)? {
            Resource::Response(bytes) => Ok(bytes.clone()),
            _ => Err(InterpretError::TypeMismatch("expected response, not service")),
        }
    }

    fn service_allocate_bytes(&mut self, layout: LayoutId, bytes: &[u8]) -> Result<u64, InterpretError> {
        let count = u64::try_from(bytes.len()).map_err(|_| InterpretError::ArithmeticTrap)?;
        if count > SIZE_MAX - 12 { return Err(InterpretError::ArithmeticTrap); }
        let object = self.allocate(layout, AllocationClass::ReferenceCounted,
            RuntimeExtent::Counted { header_bytes: 4, count, capacity: count, stride_bytes: 1 }, false)?;
        let storage = self.object_mut(object)?;
        for (index, byte) in bytes.iter().enumerate() {
            storage.cells.insert(CellKey::Element(index as u64), InterpreterValue::Size(u64::from(*byte)));
        }
        Ok(object)
    }

    fn service_read_bytes(&self, id: u64, layout: LayoutId) -> Result<Vec<u8>, InterpretError> {
        let object = self.object(id)?;
        if object.layout != layout { return Err(InterpretError::TypeMismatch("worker result byte layout")); }
        let RuntimeExtent::Counted { header_bytes: 4, count, capacity, stride_bytes: 1 } = object.extent else {
            return Err(InterpretError::TypeMismatch("worker result counted byte extent"));
        };
        if count > capacity || capacity > SIZE_MAX - 12 { return Err(InterpretError::ArithmeticTrap); }
        (0..count).map(|index| match object.cells.get(&CellKey::Element(index)) {
            Some(InterpreterValue::Size(byte)) => u8::try_from(*byte).map_err(|_| InterpretError::TypeMismatch("worker result byte")),
            _ => Err(InterpretError::TypeMismatch("worker result has an uninitialized byte")),
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_worker_refuses_steps_but_permits_exactly_one_cleanup_close() {
        let mut services = ServiceState::new();
        services.workers.insert(0, None);
        assert!(services.take_worker(0).is_err());
        assert!(services.take_worker(0).is_err());
        assert!(matches!(services.close_worker(0), Ok(None)));
        assert!(services.close_worker(0).is_err());
        assert!(services.take_worker(0).is_err());
        assert!(services.close_worker(1).is_err());
    }

    #[test]
    fn worker_resources_require_the_exact_issuer_and_kind() {
        let owner = ServiceState::new();
        let foreign = ServiceState::new();
        let service = owner.mint(Resource::Service(0));
        assert!(matches!(owner.authenticate(&service), Ok(Resource::Service(0))));
        assert!(foreign.authenticate(&service).is_err());
        assert!(owner.authenticate(&CapabilityToken::root(SERVICE_KIND)).is_err());
        let mut mislabeled = service;
        mislabeled.kind = RESPONSE_KIND;
        assert!(owner.authenticate(&mislabeled).is_err());
    }

    #[test]
    fn worker_responses_are_immutable_copies_not_mutable_pending_state() {
        let owner = ServiceState::new();
        let mut input = vec![0, 255, 42];
        let first = owner.mint(Resource::Response(input.clone().into()));
        input[0] = 77;
        let second = owner.mint(Resource::Response(input.into()));
        let Resource::Response(first_bytes) = owner.authenticate(&first).unwrap() else { panic!("response"); };
        let Resource::Response(second_bytes) = owner.authenticate(&second).unwrap() else { panic!("response"); };
        assert_eq!(&**first_bytes, &[0, 255, 42]);
        assert_eq!(&**second_bytes, &[77, 255, 42]);
        assert_ne!(first, second);
    }
}
