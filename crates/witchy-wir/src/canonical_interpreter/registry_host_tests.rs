//! Oracle-only probes. Expected signatures are RFC 8032 vectors, never native
//! host results. These tests do not run a runtime provider or a native backend.
use super::*;
use crate::canonical_safewir::{
    check_initial_with_test_witness, validate_final_for_test, BlockParameter,
    CallableContract, CheckedSourceIdentity, HostBufferExtent, LoweredModuleParts,
    ParameterContract, ParameterOwnership, Revision,
};

#[path = "dir_storage_tests.rs"]
pub mod storage_tests;
#[path = "dir_installer_tests.rs"]
pub mod installer_tests;

const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const CAP: ValueId = ValueId::new(0);
const RESULT: ValueId = ValueId::new(9000);

fn signing() -> OracleSecretFixture { OracleSecretFixture::Signing(SEED.as_bytes().to_vec()) }

fn module(parameters: Vec<ValueType>) -> (ValidatedSafeWir, LayoutId) {
    let mut layouts = LayoutInterner::new();
    let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
    let layout = layouts.intern_runtime_buffer(byte).unwrap();
    let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([193; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [layout]).unwrap(),
        signatures: vec![FunctionSignature {
            id: FunctionSignatureId::new(0), parameters: parameters.clone(), results: vec![],
            contract: CallableContract {
                parameters: parameters.iter().map(|value_type| ParameterContract {
                    value_type: *value_type, ownership: ParameterOwnership::Value,
                }).collect(), results: vec![], parameter_effects: BTreeMap::new(),
            },
        }],
        imports: vec![], proper_tail_sites: vec![],
        functions: vec![Function {
            id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: BlockId::new(0),
            value_types: parameters.iter().enumerate().map(|(i, ty)| (ValueId::new(i as u32), *ty)).collect(),
            blocks: vec![BasicBlock {
                id: BlockId::new(0), parameters: parameters.into_iter().enumerate().map(|(i, ty)| BlockParameter {
                    value: ValueId::new(i as u32), value_type: ty, ownership: None,
                }).collect(), operations: vec![], terminator: Terminator::Return { values: vec![] },
            }],
        }],
    });
    (validate_final_for_test(check_initial_with_test_witness(candidate).unwrap()).unwrap(), layout)
}

fn config() -> InterpreterConfig {
    InterpreterConfig { fixture: OracleFixture {
        clock: Some(OracleClockFixture { grants: 1, observations: vec![] }),
        secret_store: Some(OracleSecretStoreFixture::default()), signing_secret: Some(signing()),
        dir_grants: Some(1), ..Default::default()
    }, ..Default::default() }
}

fn bound(engine: &Engine<'_>, kind: u32) -> InterpreterValue {
    engine.bind_registry_entry(vec![InterpreterValue::capability(kind)]).unwrap().remove(0)
}

fn buffer(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>,
    bytes: &[u8], write: bool) -> HostCallArgument
{
    let object = engine.next_object;
    engine.next_object += 1;
    let id = ValueId::new(100 + object as u32 * 3);
    let start = ValueId::new(101 + object as u32 * 3);
    let length = ValueId::new(102 + object as u32 * 3);
    engine.objects.insert(object, Object {
        layout, storage: RuntimeStorage::Allocation(AllocationClass::ReferenceCounted),
        extent: RuntimeExtent::Counted { header_bytes: 8, count: bytes.len() as u64, capacity: bytes.len() as u64, stride_bytes: 1 },
        references: 1, immutable: false,
        cells: bytes.iter().enumerate().map(|(i, byte)| (CellKey::Element(i as u64), InterpreterValue::Size(u64::from(*byte)))).collect(),
        absent_owned_slots: BTreeSet::new(),
    });
    values.insert(id, InterpreterValue::ObjectRef(object));
    values.insert(start, InterpreterValue::Size(0));
    values.insert(length, InterpreterValue::Size(bytes.len() as u64));
    HostCallArgument::Buffer { place: Place::projected(id, Projection::Slice { start, length },
        ValueType::Ref(layout), if write { AccessMode::Write } else { AccessMode::Read }), extent: HostBufferExtent::Slice }
}

fn place(argument: &HostCallArgument) -> &Place {
    let HostCallArgument::Buffer { place, .. } = argument else { panic!("buffer") };
    place
}

fn bytes(engine: &Engine<'_>, argument: &HostCallArgument, values: &BTreeMap<ValueId, InterpreterValue>) -> Vec<u8> {
    engine.host_bytes(place(argument), values).unwrap()
}

pub fn registry_oracle_mints_bind_kind_scope_and_session() {
    let owner = State::new();
    let foreign = State::new();
    for resource in [Resource::Clock(0), Resource::Dir("inside".into()),
        Resource::Store(Arc::new(OracleSecretStoreFixture::default())), Resource::Secret(Arc::new(signing()))]
    {
        let kind = resource.kind();
        let token = owner.mint(resource.clone());
        assert_eq!(owner.authenticate(&token.clone(), kind).unwrap(), resource, "a capability copy retains issuer identity");
        assert!(foreign.authenticate(&token, kind).is_err());
        assert!(owner.authenticate(&CapabilityToken::root(kind), kind).is_err());
        for wrong_kind in [0, CLOCK, DIR_KIND, STORE, SECRET] {
            if wrong_kind == kind { continue; }
            let mut wrong = token.clone(); wrong.kind = wrong_kind;
            assert!(owner.authenticate(&wrong, wrong_kind).is_err());
        }
        let mut wrong = token; wrong.scope.push_str("/outside");
        assert!(owner.authenticate(&wrong, kind).is_err());
    }
    let root = owner.mint(Resource::Dir(String::new()));
    let child = owner.narrow_dir(&root, "a".into()).unwrap();
    assert!(owner.narrow_dir(&child, "ab".into()).is_err());
    assert!(owner.narrow_dir(&child, "a/b".into()).is_ok());
    let unbound = owner.narrow_dir(&CapabilityToken::root(DIR_KIND), "a".into()).unwrap();
    assert!(owner.authenticate(&unbound, DIR_KIND).is_err(), "narrowing cannot upgrade a numeric root request");
}

pub fn registry_oracle_launch_requires_explicit_grants_even_when_unused() {
    for kind in [CLOCK_CAPABILITY_KIND, SECRET_STORE_CAPABILITY_KIND, SECRET_CAPABILITY_KIND] {
        let (module, _) = module(vec![ValueType::Capability(kind)]);
        let capabilities = vec![InterpreterValue::capability(kind.get())];
        for launch in [false, true] {
            let run = |config| if launch {
                interpret_entry_program(&module, FunctionId::new(0), capabilities.clone(), &[], config)
            } else { interpret_program(&module, FunctionId::new(0), capabilities.clone(), config) };
            assert!(run(InterpreterConfig::default()).is_err());
            assert_eq!(run(config()).unwrap().leaked_objects, 0);
        }
    }
    let (module, _) = module(vec![]);
    let engine = Engine::new(module.module(), config());
    assert!(engine.bind_registry_entry(vec![InterpreterValue::capability(CLOCK); 2]).is_err());
    assert!(engine.bind_registry_entry(vec![InterpreterValue::capability(DIR_KIND); 2]).is_err());
    for kind in [CLOCK, DIR_KIND, STORE, SECRET] {
        let minted = bound(&engine, kind);
        let InterpreterValue::Capability(token) = &minted else { unreachable!() };
        assert!(engine.bind_registry_entry(vec![InterpreterValue::NullableCapability(Some(token.clone()))]).is_err());
        assert!(engine.bind_registry_entry(vec![InterpreterValue::NullableCapability(Some(CapabilityToken::root(kind)))]).is_err());
        assert!(engine.bind_registry_entry(vec![minted.clone()]).is_err());
        let other = Engine::new(module.module(), config());
        assert!(other.bind_registry_entry(vec![minted]).is_err());
    }
    assert_eq!(engine.bind_registry_entry(vec![InterpreterValue::NullableCapability(None)]).unwrap(),
        [InterpreterValue::NullableCapability(None)], "typed absence carries no authority");
    for secret in [None, Some(OracleSecretFixture::Plain(SEED.as_bytes().to_vec())),
        Some(OracleSecretFixture::Sealed(SEED.as_bytes().to_vec()))]
    {
        let mut cfg = config();
        cfg.fixture.signing_secret = secret;
        cfg.fixture.secret_store.as_mut().unwrap().secrets.push(("signing".into(), signing()));
        let engine = Engine::new(module.module(), cfg);
        assert!(engine.bind_registry_entry(vec![InterpreterValue::capability(SECRET)]).is_err(),
            "a named secret is not the direct signing grant");
    }
}







pub fn registry_oracle_dir_write_refuses_explicit_directories_and_granted_root() {
    let (module, layout) = module(vec![]);
    for (scope, directory, target) in [("", "empty", "empty"), ("", ".", ""),
        ("inside", "empty", "empty"), ("inside", ".", "")]
    {
        let mut engine = Engine::new(module.module(), config());
        if !scope.is_empty() { engine.config.fixture.directories.insert(scope.into()); }
        let token = engine.registry.mint(Resource::Dir(scope.into()));
        let mut values = BTreeMap::from([(CAP, InterpreterValue::Capability(token))]);
        let directory = buffer(&mut engine, layout, &mut values, directory.as_bytes(), false);
        engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), directory], &mut values).unwrap();
        let before = engine.config.fixture.clone();
        let path = buffer(&mut engine, layout, &mut values, target.as_bytes(), false);
        let data = buffer(&mut engine, layout, &mut values, b"payload", false);
        assert_eq!(engine.capability_host_call("dir_write_at", &[],
            &[HostCallArgument::Capability(CAP), path, data.clone()], &mut values), Err(InterpretError::ExplicitTrap));
        assert_eq!(engine.config.fixture, before, "scope={scope:?}, target={target:?}");
        let file = buffer(&mut engine, layout, &mut values, b"note", false);
        engine.capability_host_call("dir_write_at", &[], &[HostCallArgument::Capability(CAP), file, data], &mut values).unwrap();
        let name = if scope.is_empty() { "note" } else { "inside/note" };
        assert_eq!(engine.config.fixture.files[name], b"payload");
        assert_eq!(engine.config.fixture.directories, before.directories);
    }
}


pub fn registry_oracle_mkdir_fallback_never_writes_or_follows_links() {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let suffix = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("witchy-registry-oracle-{}-{suffix}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(root.clone());
    std::fs::create_dir(root.join("existing")).unwrap();
    std::fs::write(root.join("file"), b"fixture").unwrap();
    let mut fixture = OracleFixture { root: Some(root.clone()), ..Default::default() };
    fixture.make_directory("existing/new/empty").unwrap();
    assert!(!root.join("existing/new").exists(), "only the fixture map changes");
    assert!(fixture.is_directory("existing/new/empty"));
    let before = fixture.clone();
    for path in ["file", "file/child"] { assert!(fixture.make_directory(path).is_err()); assert_eq!(fixture, before); }
    #[cfg(unix)]
    {
        for (name, target) in [("inside", "existing"), ("outside", "/"), ("dangling", "absent")] {
            std::os::unix::fs::symlink(target, root.join(name)).unwrap();
            assert!(fixture.make_directory(name).is_err());
            assert!(fixture.make_directory(&format!("{name}/child")).is_err());
            assert!(fixture.make_directory(&format!("./{name}/./child")).is_err());
            assert_eq!(fixture, before);
        }
    }
    fixture.root = Some(root.join("file/child"));
    let before = fixture.clone();
    assert!(fixture.make_directory("new").is_err(), "metadata errors cannot mean mkdir success");
    assert_eq!(fixture, before);
}

