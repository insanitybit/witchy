//! Compiler-session imports are authenticated ABI operations AND require an
//! exact checked source launch contract. They are never ordinary services.
use super::*;
use witchy_syntax::compiler_session::CompilerSessionContract;

pub const COMPILER_FRESH_IMPORT: &str = "compiler_syntax_fresh_into";
pub const COMPILER_QUOTE_EXPR_IMPORT: &str = "compiler_syntax_quote_expr_into";
pub const COMPILER_EMIT_EXPR_IMPORT: &str = "compiler_syntax_emit_expr";

fn metered(name: &str) -> bool {
    matches!(name, COMPILER_FRESH_IMPORT | COMPILER_QUOTE_EXPR_IMPORT | COMPILER_EMIT_EXPR_IMPORT)
}

impl CanonicalAbiCatalog {
    pub fn with_compiler_expression_import(
        mut self, name: &str, signature: FunctionSignatureId, string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy::{I32, I64};
        let read = HostParameterRole::Buffer { value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice };
        let write = HostParameterRole::Buffer { value_type: ValueType::Ref(string_layout), access: AccessMode::Write, extent: HostBufferExtent::Slice };
        let import = match name {
            COMPILER_QUOTE_EXPR_IMPORT => authenticated_import(name, &[I32, I32, I64, I32, I32], &[I32], signature,
                vec![read, HostParameterRole::Value(ValueType::Int), write], vec![HostResultRole::Value(ValueType::Size)],
                HostMutation::WriteBuffers, ImportAuthentication::CompilerSession)?,
            COMPILER_EMIT_EXPR_IMPORT => authenticated_import(name, &[I32, I32], &[], signature,
                vec![read], vec![], HostMutation::ReadOnly, ImportAuthentication::CompilerSession)?,
            _ => return Err("unknown compiler expression import".into()),
        };
        self.imports.insert(import.id, import);
        Ok(self)
    }
    pub fn with_compiler_session_import(
        mut self, signature: FunctionSignatureId, string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        let import = authenticated_import(
            COMPILER_FRESH_IMPORT, &[WasmTy::I32; 4], &[WasmTy::I32], signature,
            vec![buffer(AccessMode::Read), buffer(AccessMode::Write)],
            vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers,
            ImportAuthentication::CompilerSession,
        )?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
}

pub(super) fn validate<S: ModuleStage>(
    candidate: &CanonicalModule<S>, source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<(), String> {
    let contract = CompilerSessionContract::decode(source.launch_contract_payload());
    if contract.is_none() && !candidate.imports().iter().any(|import| import.module == "witchy" && metered(&import.name)) {
        return Ok(());
    }
    let contract = contract
        .ok_or("compiler syntax import requires a checked comptime launch contract")?;
    if contract.source_digest() != source.source_witness().source_bundle_digest() {
        return Err("compiler syntax contract differs from checked source".into());
    }
    for import in candidate.imports() {
        // Guest Wasm work is fuelled; the new syntax service explicitly meters
        // its host work. Other host computation/services need their own budget
        // contract before this compiler-only lane may call them.
        let diagnostic = crate::wir_prelude::abi_import_info(&import.name).is_some_and(|info|
            info.class == crate::wir_prelude::AbiImportClass::RuntimeDiagnostic);
        if import.module != "witchy" || (!metered(&import.name) && !diagnostic) {
            return Err(format!("comptime import `{}` has no deterministic compiler-session budget contract", import.name));
        }
    }
    Ok(())
}
