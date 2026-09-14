//! Oracle-only probes. Expected signatures are RFC 8032 vectors, never native
//! host results. These tests do not run a runtime provider or a native backend.
use super::*;
use crate::canonical_safewir::{
    check_initial_with_test_witness, validate_final_for_test, BlockParameter,
    CallableContract, CheckedSourceIdentity, HostBufferExtent, LoweredModuleParts,
    ParameterContract, ParameterOwnership, Revision,
};

#[path = "dir_storage_tests.rs"]
mod storage_tests;

const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
const SIGNATURE: &str = concat!("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
    "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b");
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

#[test]
fn registry_oracle_frozen_kinds_and_import_order() {
    assert_eq!([CLOCK, DIR_KIND, STORE, SECRET], [19, 1, 20, 21]);
    assert_eq!(IMPORTS, ["clock_now", "dir_make_dir_at", "secretstore_lookup_at", "crypto_sign_into", "crypto_public_key_into"]);
    for (entry, kind) in [(crate::CanonicalEntryCapability::Clock, CLOCK),
        (crate::CanonicalEntryCapability::SecretStore, STORE), (crate::CanonicalEntryCapability::Secret, SECRET)]
    {
        assert_eq!(InterpreterValue::entry_capability(entry), InterpreterValue::capability(kind));
    }
}

#[test]
fn registry_oracle_mints_bind_kind_scope_and_session() {
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

#[test]
fn registry_oracle_launch_requires_explicit_grants_even_when_unused() {
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

#[test]
fn registry_oracle_clock_exact_zero_consumed_unused_and_refusals() {
    let (module, _) = module(vec![]);
    let mut cfg = config();
    cfg.fixture.clock.as_mut().unwrap().observations = vec![0, 1_700_000_000_123];
    let mut engine = Engine::new(module.module(), cfg.clone());
    let mut values = BTreeMap::from([(CAP, bound(&engine, CLOCK))]);
    assert_eq!(engine.finish_registry(), Err(refusal("Clock unused observations: consumed 0 of 2")));
    let request = [HostCallArgument::Capability(CAP)];
    for invalid in [InterpreterValue::capability(CLOCK), bound(&engine, STORE), InterpreterValue::NullableCapability(None)] {
        let saved = values.insert(CAP, invalid).unwrap();
        assert!(engine.registry_host_call("clock_now", &[RESULT], &request, &mut values).is_err());
        assert_eq!(engine.registry.consumed_clock, 0);
        values.insert(CAP, saved);
    }
    assert!(engine.registry_host_call("clock_now", &[], &request, &mut values).is_err());
    for expected in [0, 1_700_000_000_123] {
        engine.registry_host_call("clock_now", &[RESULT], &request, &mut values).unwrap();
        assert_eq!(values[&RESULT], InterpreterValue::Int(expected));
    }
    engine.finish_registry().unwrap();
    assert_eq!(engine.registry_host_call("clock_now", &[RESULT], &request, &mut values),
        Err(refusal("unexpected Clock call after observations end")));
    assert_eq!(engine.registry.consumed_clock, 2);
    assert!(interpret_with_config(&module, FunctionId::new(0), vec![], cfg.clone()).is_err());
    assert!(interpret_program(&module, FunctionId::new(0), vec![], cfg.clone()).is_err());
    assert!(interpret_entry_program(&module, FunctionId::new(0), vec![], &[], cfg).is_err());
}

#[test]
fn registry_oracle_store_empty_absence_first_match_and_snapshot() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    let mut values = BTreeMap::from([(CAP, bound(&engine, STORE))]);
    let name = buffer(&mut engine, layout, &mut values, b"publish", false);
    let request = [HostCallArgument::Capability(CAP), name];
    engine.registry_host_call("secretstore_lookup_at", &[RESULT], &request, &mut values).unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::NullableCapability(None));
    let first = OracleSecretFixture::Sealed(vec![0; 32]);
    engine.config.fixture.secret_store.as_mut().unwrap().secrets = vec![
        ("publish".into(), first.clone()), ("publish".into(), signing())];
    values.insert(CAP, bound(&engine, STORE));
    engine.config.fixture.secret_store.as_mut().unwrap().secrets.clear();
    engine.registry_host_call("secretstore_lookup_at", &[RESULT], &request, &mut values).unwrap();
    let InterpreterValue::NullableCapability(Some(token)) = &values[&RESULT] else { panic!("named secret") };
    assert_eq!(token.kind(), SECRET);
    assert_eq!(engine.registry.authenticate(token, SECRET).unwrap(), Resource::Secret(Arc::new(first)));
    let invalid = buffer(&mut engine, layout, &mut values, &[255], false);
    let saved = values[&RESULT].clone();
    assert_eq!(engine.registry_host_call("secretstore_lookup_at", &[RESULT], &[HostCallArgument::Capability(CAP), invalid], &mut values),
        Err(InterpretError::TypeMismatch("host string is not UTF-8")));
    assert_eq!(values[&RESULT], saved);
}

#[test]
fn registry_oracle_ed25519_seed_forms_known_answers_and_redaction() {
    let raw = seed(&signing()).unwrap().to_vec();
    for secret in [signing(), OracleSecretFixture::Signing(SEED.to_uppercase().into_bytes()),
        OracleSecretFixture::Sealed(raw.clone()), OracleSecretFixture::Plain(raw)]
    {
        assert_eq!(secret_bytes(&secret, None).unwrap(), KEY.as_bytes());
        assert_eq!(secret_bytes(&secret, Some(b"")).unwrap(), SIGNATURE.as_bytes());
        assert!(!format!("{secret:?}").contains(SEED));
        assert!(format!("{secret:?}").contains("redacted"));
    }
    for malformed in [vec![], vec![0; 31], vec![0; 33], vec![b'g'; 64], vec![255; 64], vec![b'0'; 128]] {
        let error = secret_bytes(&OracleSecretFixture::Signing(malformed), None).unwrap_err();
        assert_eq!(error, refusal("Ed25519 secret must be a 32-byte seed or 64 ASCII hex digits"));
    }
}

#[test]
fn registry_oracle_crypto_short_capacity_has_no_partial_write() {
    let (module, layout) = module(vec![]);
    for sign in [false, true] {
        let expected = if sign { SIGNATURE } else { KEY };
        for capacity in [0, 1, 63, 64, 65, 127, 128, 129] {
            let mut engine = Engine::new(module.module(), config());
            let mut values = BTreeMap::from([(CAP, bound(&engine, SECRET))]);
            let message = buffer(&mut engine, layout, &mut values, b"", false);
            let output = buffer(&mut engine, layout, &mut values, &vec![b'!'; capacity], true);
            let mut request = vec![HostCallArgument::Capability(CAP)];
            if sign { request.push(message); }
            request.push(output.clone());
            engine.registry_host_call(if sign { "crypto_sign_into" } else { "crypto_public_key_into" }, &[RESULT], &request, &mut values).unwrap();
            assert_eq!(values[&RESULT], InterpreterValue::Size(expected.len() as u64));
            let mut wanted = vec![b'!'; capacity];
            if capacity >= expected.len() { wanted[..expected.len()].copy_from_slice(expected.as_bytes()); }
            assert_eq!(bytes(&engine, &output, &values), wanted, "sign={sign}, capacity={capacity}");
        }
    }
}

#[test]
fn registry_oracle_crypto_readonly_full_range_and_seed_preflight() {
    let (module, layout) = module(vec![]);
    for capacity in [0, 63, 64, 128] {
        for refusal_case in ["immutable", "static", "read-place", "past-end", "overflow", "bad-seed", "no-secret"] {
            let mut engine = Engine::new(module.module(), config());
            if refusal_case == "bad-seed" { engine.config.fixture.signing_secret = Some(OracleSecretFixture::Signing(vec![1])); }
            let mut values = BTreeMap::from([(CAP, bound(&engine, SECRET)), (RESULT, InterpreterValue::Size(777))]);
            let output = buffer(&mut engine, layout, &mut values, &vec![b'?'; capacity], refusal_case != "read-place");
            let (object, _, _) = engine.slice_place(place(&output), &values).unwrap();
            let before = engine.objects[&object].cells.clone();
            match refusal_case {
                "immutable" => engine.objects.get_mut(&object).unwrap().immutable = true,
                "static" => engine.objects.get_mut(&object).unwrap().storage = RuntimeStorage::StaticData,
                "past-end" | "overflow" => {
                    let Projection::Slice { start, length } = place(&output).projection() else { unreachable!() };
                    values.insert(*length, InterpreterValue::Size(capacity as u64 + 1));
                    if refusal_case == "overflow" { values.insert(*start, InterpreterValue::Size(u64::MAX)); }
                }
                "no-secret" => { values.insert(CAP, InterpreterValue::NullableCapability(None)); }
                _ => {},
            }
            assert!(engine.registry_host_call("crypto_public_key_into", &[RESULT], &[HostCallArgument::Capability(CAP), output], &mut values).is_err(), "{refusal_case}: {capacity}");
            assert_eq!(engine.objects[&object].cells, before);
            assert_eq!(values[&RESULT], InterpreterValue::Size(777));
        }
    }
}

#[test]
fn registry_oracle_sign_snapshots_aliased_utf8_message() {
    let (module, layout) = module(vec![]);
    for valid_utf8 in [false, true] {
        let mut cfg = config();
        cfg.fixture.signing_secret = Some(OracleSecretFixture::Signing(
            b"4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb".to_vec()));
        let mut engine = Engine::new(module.module(), cfg);
        let mut values = BTreeMap::from([(CAP, bound(&engine, SECRET))]);
        let mut initial = vec![b'!'; 128]; initial[0] = if valid_utf8 { b'r' } else { 255 };
        let output = buffer(&mut engine, layout, &mut values, &initial, true);
        let message_length = ValueId::new(9001); values.insert(message_length, InterpreterValue::Size(1));
        let Projection::Slice { start, .. } = place(&output).projection() else { unreachable!() };
        let message = HostCallArgument::Buffer { place: Place::projected(place(&output).object(),
            Projection::Slice { start: *start, length: message_length }, ValueType::Ref(layout), AccessMode::Read), extent: HostBufferExtent::Slice };
        let result = engine.registry_host_call("crypto_sign_into", &[RESULT], &[HostCallArgument::Capability(CAP), message, output.clone()], &mut values);
        if valid_utf8 {
            result.unwrap();
            assert_eq!(bytes(&engine, &output, &values), concat!(
                "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da",
                "085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00").as_bytes());
        } else {
            assert_eq!(result, Err(InterpretError::TypeMismatch("host string is not UTF-8")));
            assert_eq!(bytes(&engine, &output, &values), initial);
            assert!(!values.contains_key(&RESULT));
        }
    }
}

#[test]
fn registry_oracle_sign_public_key_offline_verify_and_tamper() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    let mut values = BTreeMap::from([(CAP, bound(&engine, SECRET))]);
    let message = buffer(&mut engine, layout, &mut values, b"coven publish: sha256:fixture", false);
    let key = buffer(&mut engine, layout, &mut values, &[0; 64], true);
    let signature = buffer(&mut engine, layout, &mut values, &[0; 128], true);
    engine.registry_host_call("crypto_public_key_into", &[RESULT], &[HostCallArgument::Capability(CAP), key.clone()], &mut values).unwrap();
    engine.registry_host_call("crypto_sign_into", &[RESULT], &[HostCallArgument::Capability(CAP), message.clone(), signature.clone()], &mut values).unwrap();
    assert_eq!(bytes(&engine, &key, &values), KEY.as_bytes());
    engine.ed25519_verify_host_call(&[RESULT], &[key.clone(), message, signature.clone()], &mut values).unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Int(1));
    let tampered = buffer(&mut engine, layout, &mut values, b"coven publish: sha256:tampered", false);
    engine.ed25519_verify_host_call(&[RESULT], &[key, tampered, signature], &mut values).unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Int(0));
}

#[test]
fn registry_oracle_mkdir_empty_recursive_idempotent_and_subtree() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
    let path = buffer(&mut engine, layout, &mut values, b"registry//empty/nested", false);
    for _ in 0..2 {
        engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), path.clone()], &mut values).unwrap();
    }
    assert_eq!(engine.config.fixture.directories, ["registry", "registry/empty", "registry/empty/nested"].map(String::from).into_iter().collect());
    assert!(engine.config.fixture.files.is_empty());
    assert_eq!(engine.config.fixture.entry_names(""), ["registry"]);
    assert_eq!(engine.config.fixture.entry_names("registry"), ["empty"]);
    assert!(engine.config.fixture.metadata_is_directory("", "registry/empty/nested"));
    assert!(engine.config.fixture.is_directory("registry/empty/nested"));
    assert!(engine.config.fixture.exists("registry/empty/nested"));
    engine.capability_host_call("dir_exists_at", &[RESULT], &[HostCallArgument::Capability(CAP), path.clone()], &mut values).unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Size(1));
    engine.capability_host_call("dir_subtree_at", &[RESULT], &[HostCallArgument::Capability(CAP), path], &mut values).unwrap();
    let subtree = values[&RESULT].clone(); values.insert(CAP, subtree);
    let child = buffer(&mut engine, layout, &mut values, b"child", false);
    engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), child], &mut values).unwrap();
    assert!(engine.config.fixture.directories.contains("registry/empty/nested/child"));
    let parent = buffer(&mut engine, layout, &mut values, b"../escape", false);
    assert!(engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), parent], &mut values).is_err());
    assert!(!engine.config.fixture.exists("escape"));
}

#[test]
fn registry_oracle_mkdir_file_conflicts_and_path_denials_are_atomic() {
    let (module, layout) = module(vec![]);
    for path in ["file", "./file", "file/child", "new/file/child", "new/./file/child", "../escape", "/absolute", "a/../b", "bad\0name"] {
        let mut engine = Engine::new(module.module(), config());
        engine.config.fixture.files.insert("file".into(), b"content".to_vec());
        engine.config.fixture.files.insert("new/file".into(), b"content".to_vec());
        let before = engine.config.fixture.clone();
        let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
        let path = buffer(&mut engine, layout, &mut values, path.as_bytes(), false);
        assert!(engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), path], &mut values).is_err());
        assert_eq!(engine.config.fixture, before);
    }
    let mut engine = Engine::new(module.module(), config());
    let mut values = BTreeMap::from([(CAP, InterpreterValue::capability(DIR_KIND))]);
    let path = buffer(&mut engine, layout, &mut values, b"new", false);
    assert!(engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), path], &mut values).is_err());
    assert!(engine.config.fixture.directories.is_empty());
}

#[test]
fn registry_oracle_mkdir_curdir_normalization_preserves_scope() {
    let (module, layout) = module(vec![]);
    for scope in ["", "inside"] {
        for (path, normalized) in [("", Some("")), ("child//grand/", Some("child/grand")), (".", Some("")), ("./", Some("")),
            ("./child", Some("child")), ("child/.", Some("child")), ("child/./grand", Some("child/grand")), ("..", None),
            ("child/../escape", None), ("/absolute", None), ("bad\0name", None)]
        {
            let mut engine = Engine::new(module.module(), config());
            if !scope.is_empty() { engine.config.fixture.directories.insert(scope.into()); }
            let mut direct = engine.config.fixture.clone();
            let before = direct.clone();
            let token = engine.registry.mint(Resource::Dir(scope.into()));
            let mut values = BTreeMap::from([(CAP, InterpreterValue::Capability(token))]);
            let argument = buffer(&mut engine, layout, &mut values, path.as_bytes(), false);
            let through_host = engine.registry_host_call("dir_make_dir_at", &[],
                &[HostCallArgument::Capability(CAP), argument], &mut values);
            let direct_path = if scope.is_empty() || path.starts_with('/') { path.to_owned() }
                else if path.is_empty() { scope.to_owned() } else { format!("{scope}/{path}") };
            assert_eq!(direct.make_directory(&direct_path), through_host, "scope={scope:?}, path={path:?}");
            assert_eq!(through_host.is_ok(), normalized.is_some(), "scope={scope:?}, path={path:?}");
            assert_eq!(direct, engine.config.fixture);
            if let Some(normalized) = normalized {
                let mut expected = before.directories.clone();
                let mut prefix = scope.to_owned();
                for part in normalized.split('/').filter(|part| !part.is_empty()) {
                    if !prefix.is_empty() { prefix.push('/'); }
                    prefix.push_str(part);
                    expected.insert(prefix.clone());
                }
                assert_eq!(engine.config.fixture.directories, expected);
                let dot = buffer(&mut engine, layout, &mut values, b".", false);
                engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), dot], &mut values).unwrap();
                assert_eq!(engine.config.fixture, direct, "mkdir of the granted root is idempotent");
            } else { assert_eq!(direct, before); }
            assert!(engine.host_path(scope, ".").is_err(), "mkdir must not widen the existing read-path ABI");
        }
    }
}

#[test]
fn registry_oracle_dir_write_refuses_explicit_directories_and_granted_root() {
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

#[test]
fn registry_oracle_server_pool_dir_binding_reads_and_narrows() {
    let (module, layout) = module(vec![ValueType::Capability(crate::canonical_safewir::DIR_CAPABILITY_KIND)]);
    let mut cfg = config();
    cfg.fixture.clock = None;
    cfg.fixture.secret_store = None;
    cfg.fixture.signing_secret = None;
    cfg.fixture.server_pool = Some(OracleServerPoolFixture::default());
    cfg.fixture.files.insert("inside/note".into(), b"hello".to_vec());
    let mut engine = Engine::new(module.module(), cfg);
    let roots = engine.prepare_launch(&[ValueType::Capability(crate::canonical_safewir::DIR_CAPABILITY_KIND)],
        vec![InterpreterValue::capability(DIR_KIND)], &[]).unwrap();
    let mut values = BTreeMap::from([(CAP, roots[0].clone())]);
    let path = buffer(&mut engine, layout, &mut values, b"inside", false);
    engine.server.before_host_call("dir_subtree_at").unwrap();
    engine.capability_host_call("dir_subtree_at", &[RESULT], &[HostCallArgument::Capability(CAP), path], &mut values).unwrap();
    values.insert(CAP, values[&RESULT].clone());
    let name = buffer(&mut engine, layout, &mut values, b"note", false);
    let output = buffer(&mut engine, layout, &mut values, &[0; 5], true);
    engine.server.before_host_call("dir_read_into").unwrap();
    engine.capability_host_call("dir_read_into", &[RESULT],
        &[HostCallArgument::Capability(CAP), name, output.clone()], &mut values).unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Size(5));
    assert_eq!(bytes(&engine, &output, &values), b"hello");
    let before = engine.config.fixture.clone();
    let path = buffer(&mut engine, layout, &mut values, b"new", false);
    assert_eq!(engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), path], &mut values),
        Err(InterpretError::UnsupportedOperation("server pool registry host effect ordering")));
    assert_eq!(engine.config.fixture, before);
    for kind in [CLOCK, STORE, SECRET] {
        assert!(engine.bind_registry_entry(vec![InterpreterValue::capability(kind)]).is_err(), "Dir binding grants no other capability");
    }
}

#[test]
fn registry_oracle_mkdir_fallback_never_writes_or_follows_links() {
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

#[test]
fn registry_oracle_isolated_and_server_worker_channels_refuse() {
    let (module, _) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    engine.isolated = true;
    for kind in [CLOCK, STORE, SECRET, DIR_KIND] {
        assert!(engine.bind_registry_entry(vec![InterpreterValue::capability(kind)]).is_err());
    }
    for name in IMPORTS {
        assert_eq!(engine.registry_host_call(name, &[], &[], &mut BTreeMap::new()), Err(InterpretError::UnsupportedHostImport));
    }
    let mut cfg = config();
    cfg.fixture.server_pool = Some(OracleServerPoolFixture::default());
    let mut engine = Engine::new(module.module(), cfg);
    for name in IMPORTS {
        assert_eq!(engine.registry_host_call(name, &[], &[], &mut BTreeMap::new()),
            Err(InterpretError::UnsupportedOperation("server pool registry host effect ordering")));
    }
    // Explicit grants can be bound without executing an effect. The server
    // profile still refuses every new import, even with authenticated tokens.
    for (kind, name) in [(CLOCK, "clock_now"), (STORE, "secretstore_lookup_at"),
        (SECRET, "crypto_sign_into"), (DIR_KIND, "dir_make_dir_at")]
    {
        let mut values = BTreeMap::from([(CAP, bound(&engine, kind))]);
        assert_eq!(engine.registry_host_call(name, &[RESULT], &[HostCallArgument::Capability(CAP)], &mut values),
            Err(InterpretError::UnsupportedOperation("server pool registry host effect ordering")));
    }
}
