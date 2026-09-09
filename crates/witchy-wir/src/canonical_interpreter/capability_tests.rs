//! The oracle's capability host calls, exercised on a hand-built canonical
//! module: a `dir_read_into` against a fixture file, with the guest
//! publishing the bounded count and printing what it read.

use std::collections::BTreeMap;

use super::{
    interpret_program, CapabilityToken, InterpretError, InterpreterConfig, InterpreterValue,
    OracleFixture,
};
use crate::canonical_safewir::{
    check_initial_with_test_witness, validate_final_for_test, AccessMode, BasicBlock, BlockId,
    BlockParameter, CallableContract, CanonicalAbiCatalog, CanonicalModule,
    CapabilityImportSignatures, CheckedSourceIdentity, ComparisonOperator, Extent, Function,
    FunctionId, FunctionSignature, FunctionSignatureId, GuardKind, HostBufferExtent,
    HostCallArgument, LoweredModuleParts, Operation, ParameterContract, ParameterOwnership, Place,
    ProperTailSite, Projection, ResultContract, Revision, StaticDataId, StaticDataSegment,
    Terminator, ValidatedSafeWir, ValueId, ValueType, AllocationClass, DIR_CAPABILITY_KIND,
};
use crate::layout::{LayoutBundle, LayoutId, LayoutInterner, LayoutSize, ScalarKind};

const FUNCTION: FunctionId = FunctionId::new(0);
const ENTRY: BlockId = BlockId::new(0);
const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
const PRINT_SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(1);
const READ_SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(2);
const FLOAT_SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(3);

/// The byte buffer both a `String` slice and the fill buffer use, with the
/// header width its descriptor declares.
fn byte_buffer_layout() -> (LayoutBundle, LayoutId, u32) {
    let mut layouts = LayoutInterner::new();
    let element = layouts.intern_scalar(ScalarKind::RawByte).expect("raw byte layout");
    let buffer = layouts.intern_runtime_buffer(element).expect("byte buffer layout");
    let LayoutSize::Dynamic { base, .. } = layouts.get(buffer).expect("buffer descriptor").size() else {
        panic!("a runtime byte buffer is dynamically sized");
    };
    let bundle = LayoutBundle::from_interner(&layouts, [buffer]).expect("layout bundle");
    (bundle, buffer, base)
}

fn capability_signatures() -> CapabilityImportSignatures {
    CapabilityImportSignatures {
        dir_read_into: READ_SIGNATURE,
        dir_list_into: READ_SIGNATURE,
        dir_exists_at: READ_SIGNATURE,
        dir_write_at: READ_SIGNATURE,
        dir_subtree_at: READ_SIGNATURE,
        dir_read_file: READ_SIGNATURE,
        file_read_into: READ_SIGNATURE,
        env_has_var: READ_SIGNATURE,
        env_get_into: READ_SIGNATURE,
    }
}

fn value_parameters(types: Vec<ValueType>) -> Vec<ParameterContract> {
    types
        .into_iter()
        .map(|value_type| ParameterContract { value_type, ownership: ParameterOwnership::Value })
        .collect()
}

/// `fn main(dir: Dir):` — read the fixture's one-byte-named file into a
/// 32-byte buffer, publish the bounded count, print it, and return the
/// file's full length.
fn read_into_program(path_bytes: &[u8]) -> ValidatedSafeWir {
    let (layouts, layout, header_bytes) = byte_buffer_layout();
    let catalog = CanonicalAbiCatalog::from_authenticated_prelude(PRINT_SIGNATURE, FLOAT_SIGNATURE, layout)
        .expect("authenticated prelude")
        .with_capability_imports(capability_signatures(), layout)
        .expect("authenticated capability imports");
    let imports = catalog
        .imports()
        .into_iter()
        .filter(|import| import.name == "print" || import.name == "dir_read_into")
        .collect::<Vec<_>>();
    let print = imports.iter().find(|import| import.name == "print").expect("print import").id;
    let read = imports.iter().find(|import| import.name == "dir_read_into").expect("read import").id;

    let dir = ValueId::new(0);
    let zero = ValueId::new(1);
    let path_length = ValueId::new(2);
    let path = ValueId::new(3);
    let capacity = ValueId::new(4);
    let capacity_ok = ValueId::new(5);
    let out = ValueId::new(6);
    let length = ValueId::new(7);
    let length_ok = ValueId::new(8);

    // Static data for the path: the buffer header, carrying the count the
    // extent declares, then the name's bytes.
    let mut segment = vec![0; header_bytes as usize];
    segment[..4].copy_from_slice(&(path_bytes.len() as u32).to_le_bytes());
    segment.extend_from_slice(path_bytes);

    let candidate = CanonicalModule::from_lowering_with_static_data(
        LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([13; 32]),
            layouts,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: vec![ValueType::Capability(DIR_CAPABILITY_KIND)],
                    results: vec![ValueType::Size],
                    contract: CallableContract {
                        parameters: value_parameters(vec![ValueType::Capability(DIR_CAPABILITY_KIND)]),
                        results: vec![ResultContract::Value(ValueType::Size)],
                        parameter_effects: BTreeMap::new(),
                    },
                },
                FunctionSignature {
                    id: PRINT_SIGNATURE,
                    parameters: vec![ValueType::Ref(layout)],
                    results: Vec::new(),
                    contract: CallableContract {
                        parameters: value_parameters(vec![ValueType::Ref(layout)]),
                        results: Vec::new(),
                        parameter_effects: BTreeMap::new(),
                    },
                },
                FunctionSignature {
                    id: READ_SIGNATURE,
                    parameters: vec![
                        ValueType::Capability(DIR_CAPABILITY_KIND),
                        ValueType::Ref(layout),
                        ValueType::Ref(layout),
                    ],
                    results: vec![ValueType::Size],
                    contract: CallableContract {
                        parameters: value_parameters(vec![
                            ValueType::Capability(DIR_CAPABILITY_KIND),
                            ValueType::Ref(layout),
                            ValueType::Ref(layout),
                        ]),
                        results: vec![ResultContract::Value(ValueType::Size)],
                        parameter_effects: BTreeMap::new(),
                    },
                },
            ],
            imports,
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (dir, ValueType::Capability(DIR_CAPABILITY_KIND)),
                    (zero, ValueType::Size),
                    (path_length, ValueType::Size),
                    (path, ValueType::Ref(layout)),
                    (capacity, ValueType::Size),
                    (capacity_ok, ValueType::Bool),
                    (out, ValueType::Ref(layout)),
                    (length, ValueType::Size),
                    (length_ok, ValueType::Bool),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: vec![BlockParameter {
                        value: dir,
                        value_type: ValueType::Capability(DIR_CAPABILITY_KIND),
                        ownership: None,
                    }],
                    operations: vec![
                        Operation::ConstSize { destination: zero, value: 0 },
                        Operation::ConstSize {
                            destination: path_length,
                            value: path_bytes.len() as u64,
                        },
                        Operation::StaticData {
                            destination: path,
                            layout,
                            extent: Extent::Counted {
                                header_bytes,
                                count: path_length,
                                capacity: path_length,
                                stride_bytes: 1,
                            },
                            data: StaticDataId::new(0),
                        },
                        Operation::ConstSize { destination: capacity, value: 32 },
                        Operation::Compare {
                            destination: capacity_ok,
                            operator: ComparisonOperator::LessEqualUnsigned,
                            left: capacity,
                            right: capacity,
                        },
                        Operation::Guard { condition: capacity_ok, kind: GuardKind::Bounds },
                        Operation::Alloc {
                            destination: out,
                            layout,
                            extent: Extent::Counted {
                                header_bytes,
                                count: capacity,
                                capacity,
                                stride_bytes: 1,
                            },
                            class: AllocationClass::ReferenceCounted,
                        },
                        Operation::UpdateCount { object: out, count: capacity },
                        Operation::HostCall {
                            destinations: vec![length],
                            import: read,
                            arguments: vec![
                                HostCallArgument::Capability(dir),
                                HostCallArgument::Buffer {
                                    place: Place::projected(
                                        path,
                                        Projection::Slice { start: zero, length: path_length },
                                        ValueType::Ref(layout),
                                        AccessMode::Read,
                                    ),
                                    extent: HostBufferExtent::Slice,
                                },
                                HostCallArgument::Buffer {
                                    place: Place::projected(
                                        out,
                                        Projection::Slice { start: zero, length: capacity },
                                        ValueType::Ref(layout),
                                        AccessMode::Write,
                                    ),
                                    extent: HostBufferExtent::Slice,
                                },
                            ],
                        },
                        // The guest publishes min(length, capacity) itself,
                        // under its own guard, never the host's answer.
                        Operation::Compare {
                            destination: length_ok,
                            operator: ComparisonOperator::LessEqualUnsigned,
                            left: length,
                            right: capacity,
                        },
                        Operation::Guard { condition: length_ok, kind: GuardKind::Bounds },
                        Operation::UpdateCount { object: out, count: length },
                        Operation::HostCall {
                            destinations: Vec::new(),
                            import: print,
                            arguments: vec![HostCallArgument::Buffer {
                                place: Place::projected(
                                    out,
                                    Projection::Slice { start: zero, length },
                                    ValueType::Ref(layout),
                                    AccessMode::Read,
                                ),
                                extent: HostBufferExtent::Slice,
                            }],
                        },
                        Operation::DropRef { object: out },
                        Operation::DropRef { object: path },
                    ],
                    terminator: Terminator::Return { values: vec![length] },
                }],
            }],
            proper_tail_sites: Vec::<ProperTailSite>::new(),
        },
        vec![StaticDataSegment::new(StaticDataId::new(0), segment)],
    )
    .with_abi_catalog(catalog);
    let checked = check_initial_with_test_witness(candidate).expect("initial validation");
    validate_final_for_test(checked).expect("final validation")
}

fn fixture(files: &[(&str, &[u8])], environment: &[(&str, &str)]) -> InterpreterConfig {
    InterpreterConfig {
        fixture: OracleFixture {
            files: files.iter().map(|(name, data)| ((*name).to_string(), data.to_vec())).collect(),
            environment: environment
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn dir_read_into_answers_the_full_length_and_fills_the_buffer() {
    let module = read_into_program(b"f");
    let run = interpret_program(
        &module,
        FUNCTION,
        vec![InterpreterValue::capability(DIR_CAPABILITY_KIND.get())],
        fixture(&[("f", b"hello")], &[]),
    )
    .expect("the program runs");
    assert_eq!(run.results, vec![InterpreterValue::Size(5)]);
    assert_eq!(run.output, vec!["hello".to_string()]);
    assert_eq!(run.leaked_objects, 0);
}

#[test]
fn dir_read_into_aborts_on_a_missing_file() {
    let module = read_into_program(b"f");
    assert_eq!(
        interpret_program(
            &module,
            FUNCTION,
            vec![InterpreterValue::capability(DIR_CAPABILITY_KIND.get())],
            fixture(&[("other", b"hello")], &[]),
        ),
        Err(InterpretError::ExplicitTrap)
    );
}

#[test]
fn a_token_of_another_kind_cannot_serve_a_dir_operation() {
    let module = read_into_program(b"f");
    assert_eq!(
        interpret_program(
            &module,
            FUNCTION,
            vec![InterpreterValue::capability(0)],
            fixture(&[("f", b"hello")], &[]),
        ),
        Err(InterpretError::TypeMismatch("capability kind"))
    );
}

/// A root token is scoped to the whole grant; the launcher never mints one
/// with a scope of its own.
#[test]
fn entry_tokens_are_root_scoped() {
    let token = CapabilityToken::root(DIR_CAPABILITY_KIND.get());
    assert_eq!(token.kind(), 1);
    assert_eq!(token.scope(), "");
    assert_eq!(
        InterpreterValue::entry_capability(crate::CanonicalEntryCapability::Env),
        InterpreterValue::capability(2)
    );
}

/// The root fallback never follows a symbolic link (the runtime's confined
/// directory refuses one that leads out of the root), reads only regular
/// files, and tolerates an empty path component like the runtime does.
#[test]
fn root_fallback_refuses_symlinks_and_non_files() {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos();
    let root = std::env::temp_dir().join(format!("witchy-oracle-root-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(root.join("inner")).expect("root");
    std::fs::write(root.join("inner/note.txt"), b"hello").expect("file");
    #[cfg(unix)]
    std::os::unix::fs::symlink("/", root.join("escape")).expect("symlink");
    let fixture = super::OracleFixture { root: Some(root.clone()), ..Default::default() };
    assert_eq!(fixture.file("inner/note.txt"), Some(b"hello".to_vec()));
    assert_eq!(fixture.file("inner//note.txt"), Some(b"hello".to_vec()), "an empty component is tolerated");
    assert!(fixture.exists("inner"));
    assert!(fixture.is_directory("inner"));
    assert_eq!(fixture.file("inner"), None, "a directory is not a readable file");
    #[cfg(unix)]
    {
        assert_eq!(fixture.file("escape/etc/hosts"), None, "a link is never followed");
        assert!(!fixture.exists("escape"), "a link does not exist to the oracle");
        assert!(!fixture.is_directory("escape"));
    }
    assert_eq!(fixture.file("missing.txt"), None);
    std::fs::remove_dir_all(&root).expect("cleanup");
}
