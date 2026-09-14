//! Semantic launch inputs. This builds object values and cells, independently
//! of the runtime's byte-image writer and the Wasm operation encoder.

use super::*;
use crate::canonical_safewir::ValueType;

pub fn interpret_entry_program(
    module: &ValidatedSafeWir,
    function: FunctionId,
    capabilities: Vec<InterpreterValue>,
    arguments: &[String],
    config: InterpreterConfig,
) -> Result<ProgramRun, InterpretError> {
    let signature = program_entry_signature(module, function)?;
    // The launch API receives only capability values; argv is constructed
    // separately. Preserve the validated signature's capability ordinals.
    let capability_parameters: Vec<_> = signature.parameters.iter()
        .filter(|parameter| matches!(parameter, ValueType::Capability(_)))
        .cloned().collect();
    validate_entry_parameters(&capability_parameters, &capabilities, config.fixture.net_grant_count())?;
    if let Some(grants) = config.fixture.dir_grants {
        let roots = capabilities.iter().filter(|value| matches!(value,
            InterpreterValue::Capability(token) if token.kind == DIR_KIND && token.scope.is_empty())).count();
        if roots > grants { return Err(InterpretError::ExplicitTrap); }
    }
    let program = module.module();
    let mut engine = Engine::new(program, config);
    engine.server.retain_launch(function, &capabilities, arguments)?;
    let values = engine.prepare_launch(&signature.parameters, capabilities, arguments)?;
    let results = engine.execute(function, values)?;
    let net_trace = engine.finish_net()?;
    engine.finish_exec()?;
    engine.finish_build_exec()?;
    engine.finish_build_net()?;
    engine.finish_fetch()?;
    engine.finish_registry()?;
    let leaked_objects = engine.objects.values().filter(|object|
        matches!(object.storage, RuntimeStorage::Allocation(_))).count()
        + engine.owners.len()
        + engine.owner_lists.len();
    Ok(ProgramRun { results, output: engine.output, leaked_objects, net_trace,
        build_trace: engine.build_out.writes, server_workers: engine.server.completed })
}

impl Engine<'_> {
pub(super) fn prepare_launch(
    &mut self, parameters: &[ValueType], capabilities: Vec<InterpreterValue>, arguments: &[String],
) -> Result<Vec<InterpreterValue>, InterpretError> {
    let (list_layout, string_layout) = crate::canonical_launch::argument_layouts()
        .map_err(|_| InterpretError::TypeMismatch("launch argument layouts"))?;
    let engine = self;
    let capabilities = engine.bind_net_entry(capabilities)?;
    let capabilities = engine.bind_build_entry(capabilities)?;
    let capabilities = engine.bind_build_read_entry(capabilities)?;
    let capabilities = engine.bind_exec_entry(capabilities)?;
    let capabilities = engine.bind_fetch_entry(capabilities)?;
    let mut capabilities = engine.bind_registry_entry(capabilities)?.into_iter();
    let mut values = Vec::new();
    for parameter in parameters {
        if *parameter == ValueType::Ref(list_layout) {
            let mut cells = BTreeMap::new();
            for (index, argument) in arguments.iter().enumerate() {
                if argument.len() as u64 > SIZE_MAX - 12 { return Err(InterpretError::ArithmeticTrap); }
                let object = engine.next_object;
                engine.next_object += 1;
                engine.objects.insert(object, Object {
                    layout: string_layout,
                    storage: RuntimeStorage::Allocation(AllocationClass::ReferenceCounted),
                    extent: RuntimeExtent::Counted { header_bytes: 4, count: argument.len() as u64,
                        capacity: argument.len() as u64, stride_bytes: 1 },
                    references: 1,
                    immutable: false,
                    cells: argument.bytes().enumerate().map(|(index, byte)|
                        (CellKey::Element(index as u64), InterpreterValue::Size(u64::from(byte)))).collect(),
                    absent_owned_slots: BTreeSet::new(),
                });
                cells.insert(CellKey::Element(index as u64), InterpreterValue::ObjectRef(object));
            }
            if arguments.len() as u64 > (SIZE_MAX - 16) / 8 { return Err(InterpretError::ArithmeticTrap); }
            let object = engine.next_object;
            engine.next_object += 1;
            engine.objects.insert(object, Object {
                layout: list_layout,
                storage: RuntimeStorage::Allocation(AllocationClass::ReferenceCounted),
                extent: RuntimeExtent::Counted { header_bytes: 8, count: arguments.len() as u64,
                    capacity: arguments.len() as u64, stride_bytes: 8 },
                references: 1,
                immutable: false,
                cells,
                absent_owned_slots: BTreeSet::new(),
            });
            values.push(InterpreterValue::ObjectRef(object));
        } else if matches!(parameter, ValueType::Capability(_)) {
            values.push(capabilities.next().ok_or(InterpretError::TypeMismatch("missing launch capability"))?);
        } else {
            return Err(InterpretError::TypeMismatch("unsupported launch input"));
        }
    }
    if capabilities.next().is_some() { return Err(InterpretError::TypeMismatch("extra launch capability")); }
    Ok(values)
}
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
