//! Exercise the native ABI without enabling the optional fixture provider.
#![cfg(all(feature = "native", not(feature = "test-fixtures")))]

use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, Module, TypeSection, ValType,
};
use witchy_runtime::runtime::{Capabilities, Runtime};

#[test]
fn disabled_console_fixture_import_refuses_before_memory_or_output() {
    let mut types = TypeSection::new();
    types.ty().function(
        [ValType::EXTERNREF, ValType::I32, ValType::I32,
         ValType::I32, ValType::I32, ValType::I32],
        [],
    );
    types.ty().function([], []);
    let mut imports = ImportSection::new();
    imports.import("witchy", "console_print_at", EntityType::Function(0));
    let mut functions = FunctionSection::new();
    functions.function(1);
    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, 1);
    // No memory exists. A nullable externref local starts as null, and every
    // scalar is invalid: the disabled handler must refuse without inspecting it.
    let mut run = Function::new([(1, ValType::EXTERNREF)]);
    run.instruction(&Instruction::LocalGet(0));
    for _ in 0..5 {
        run.instruction(&Instruction::I32Const(-1));
    }
    run.instruction(&Instruction::Call(0));
    run.instruction(&Instruction::End);
    let mut code = CodeSection::new();
    code.function(&run);
    let mut module = Module::new();
    module.section(&types);
    module.section(&imports);
    module.section(&functions);
    module.section(&exports);
    module.section(&code);
    let wasm = module.finish();

    let console_grants = Capabilities {
        print: true,
        print_int: true,
        console_read: true,
        ..Capabilities::none()
    };
    for caps in [Capabilities::none(), console_grants] {
        let mut runtime = Runtime::new().expect("native runtime");
        let mut vm = runtime.spawn(&wasm, caps, 4)
            .expect("disabled fixture ABI still links with its exact signature");
        let error = vm.run().expect_err("disabled fixture must reject invocation");
        assert_eq!(error.root_cause().to_string(),
            "Console fixture support is unavailable in this runtime");
        assert!(vm.output().is_empty(), "refusal must produce no output");
    }
}
