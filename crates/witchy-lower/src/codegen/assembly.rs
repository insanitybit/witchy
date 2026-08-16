//! Module assembly: the compile entry points and the wiring that turns lowered
//! per-function WIR into a finished module — reachability roots, item
//! registration, the static prelude/helper-registry selection, and the final
//! `WirModule` -> wasm encode. `compile_module_binary`/`assemble_wir_module`/
//! `compile_build_module` are the public entry points (re-exported by the parent).

use super::*;
use super::glamour_metadata::{
    checked_glamour_development_codec_module, GlamourDevelopmentCodecSpec,
    GlamourDevelopmentMigrationCodec,
};
use sha2::{Digest, Sha256};

struct ModuleLayoutResolver<'a> {
    definitions: BTreeMap<&'a str, &'a witchy_syntax::ast::TypeDef>,
    header_free_lists: Vec<Type>,
}

impl<'a> ModuleLayoutResolver<'a> {
    fn new(module: &'a Module, header_free_lists: Vec<Type>) -> Self {
        let mut definitions = BTreeMap::new();
        for item in &module.items {
            if let Item::Type(definition) = item {
                definitions.insert(definition.name.as_str(), definition);
            }
        }
        Self { definitions, header_free_lists }
    }

    fn definition(&self, name: &str) -> Option<&'a witchy_syntax::ast::TypeDef> {
        self.definitions.get(name).copied().or_else(|| {
            self.definitions
                .values()
                .find(|definition| definition.name.rsplit('.').next() == Some(name))
                .copied()
        })
    }
}

impl witchy_wir::layout::ClosedTypeResolver for ModuleLayoutResolver<'_> {
    fn resolve_named<'a>(
        &'a self,
        name: &str,
        arguments: &[Type],
    ) -> Option<witchy_wir::layout::ResolvedNamed<'a>> {
        use witchy_wir::layout::{RcHeader, ReferenceKind, ResolvedNamed, ScalarKind};
        match name {
            "Bool" => Some(ResolvedNamed::Scalar(ScalarKind::Bool)),
            "Int" => Some(ResolvedNamed::Scalar(ScalarKind::Int)),
            "Float" => Some(ResolvedNamed::Scalar(ScalarKind::Float)),
            "Duration" => Some(ResolvedNamed::Scalar(ScalarKind::Duration)),
            "List" => {
                let list = Type::Named(name.to_string(), arguments.to_vec());
                let rc = if self
                    .header_free_lists
                    .iter()
                    .any(|known| known.unqualified() == list.unqualified())
                {
                    RcHeader::Elided
                } else {
                    RcHeader::Required
                };
                Some(ResolvedNamed::PackedList { rc })
            }
            _ => {
                let definition = self.definition(name)?;
                if definition.is_capability {
                    Some(ResolvedNamed::Reference(ReferenceKind::Capability))
                } else if definition.packed {
                    if definition.variants.len() == 1 {
                        Some(ResolvedNamed::PackedRecord(definition))
                    } else {
                        Some(ResolvedNamed::ClosedSum(definition))
                    }
                } else {
                    Some(ResolvedNamed::Reference(ReferenceKind::Owning))
                }
            }
        }
    }
}

fn type_requests_specialized_layout(
    ty: &Type,
    resolver: &ModuleLayoutResolver<'_>,
) -> bool {
    match ty.unqualified() {
        Type::Named(name, arguments) if name == "List" => arguments
            .first()
            .is_some_and(|element| type_requests_specialized_layout(element, resolver)),
        Type::Named(name, _) => resolver.definition(name).is_some_and(|definition| definition.packed),
        // Tuples have no qualifier of their own. A closed tuple participates
        // when it contains a declared-packed component; scalar-only tuples keep
        // the existing uniform ABI until a source contract selects them.
        Type::Tuple(fields) => fields
            .iter()
            .any(|field| type_requests_specialized_layout(field, resolver)),
        _ => false,
    }
}

/// Collect every closed physical shape nested in a checked type. Callable
/// values have no aggregate layout themselves, but their parameter/result
/// positions can: a lambda returning `(Point, Int)` must register that tuple
/// before boundary rejection asks for its exact `LayoutId`. Lists and tuples
/// recurse so an unsupported dynamic-inline composition is rejected by the
/// canonical interner instead of silently falling back to the uniform ABI.
fn collect_specialized_layout_requests(
    ty: &Type,
    resolver: &ModuleLayoutResolver<'_>,
    requested: &mut Vec<Type>,
) {
    let ty = ty.unqualified();
    if type_requests_specialized_layout(ty, resolver) {
        requested.push(ty.clone());
    }
    match ty {
        Type::Named(_, arguments) | Type::Dyn(_, arguments) => {
            for argument in arguments {
                collect_specialized_layout_requests(argument, resolver, requested);
            }
        }
        Type::Tuple(fields) => {
            for field in fields {
                collect_specialized_layout_requests(field, resolver, requested);
            }
        }
        Type::Fn(parameters, result, _) => {
            for parameter in parameters {
                collect_specialized_layout_requests(parameter, resolver, requested);
            }
            collect_specialized_layout_requests(result, resolver, requested);
        }
        Type::RecordCompose { base, fields } => {
            collect_specialized_layout_requests(base, resolver, requested);
            for (_, field) in fields {
                collect_specialized_layout_requests(field, resolver, requested);
            }
        }
        Type::Qualified(_, inner) => {
            collect_specialized_layout_requests(inner, resolver, requested);
        }
    }
}

fn register_specialized_layouts(cg: &mut Codegen<'_>, module: &Module) {
    // RFC-0111 extends the `unbox` optimization rather than creating a second
    // always-on representation mode. With the lever disabled, declared packed
    // values deliberately use the uniform boxed ABI as the differential oracle.
    if !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::Unbox) {
        return;
    }
    let header_free_lists = header_elision::proven_header_free_lists(
        module,
        cg.type_table,
        &cg.loan_facts,
    );
    let resolver = ModuleLayoutResolver::new(module, header_free_lists);
    let mut requested = Vec::new();
    for item in &module.items {
        match item {
            Item::Type(definition)
                if definition.packed
                    && witchy_syntax::ast::effective_type_def_params(definition).is_empty() =>
            {
                let ty = Type::Named(definition.name.clone(), Vec::new());
                requested.push(ty.clone());
                requested.push(Type::Named("List".into(), vec![ty]));
            }
            Item::Function(function) => {
                for ty in function.params.iter().filter_map(|parameter| parameter.ty.as_ref()) {
                    collect_specialized_layout_requests(ty, &resolver, &mut requested);
                }
                if let Some(ty) = &function.ret {
                    collect_specialized_layout_requests(ty, &resolver, &mut requested);
                }
            }
            _ => {}
        }
    }
    // The checked table is the exact source for inferred lambda and application
    // types. Walking every concrete entry covers closures declared only inside a
    // body, including inferred/nested `fn(...) -> T` positions that are absent
    // from top-level declaration syntax.
    for ty in cg
        .type_table
        .concrete_types()
        .filter_map(witchy_types::typeck::ty_to_ast)
    {
        collect_specialized_layout_requests(&ty, &resolver, &mut requested);
    }
    for ty in requested {
        if cg
            .specialized_type_ids
            .iter()
            .any(|(known, _)| known.unqualified() == ty.unqualified())
        {
            continue;
        }
        match cg.specialized_layouts.intern_type(&ty, &resolver) {
            Ok(id) => cg.specialized_type_ids.push((ty, id)),
            Err(error) => {
                cg.reject_reason.get_or_insert_with(|| CodegenError {
                    message: format!("declared packed layout rejected: {error}"),
                });
            }
        }
    }
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        let parameters = function
            .params
            .iter()
            .map(|parameter| {
                parameter.ty.as_ref().and_then(|ty| {
                    cg.specialized_type_ids
                        .iter()
                        .find(|(known, _)| known.unqualified() == ty.unqualified())
                        .map(|(_, id)| *id)
                })
            })
            .collect();
        let result = function.ret.as_ref().and_then(|ty| {
            cg.specialized_type_ids
                .iter()
                .find(|(known, _)| known.unqualified() == ty.unqualified())
                .map(|(_, id)| *id)
        });
        let signature = CallableLayoutSignature::new(parameters, result);
        if signature.has_specialized_layout() {
            cg.callable_layouts.insert(function.name.clone(), signature);
        }
    }
}

/// Every value-producing path in this deliberately small destination ABI slice
/// ends in a constructor. Requiring a single tail statement per block excludes
/// early returns and mixed control flow until their proof is represented here.
fn destination_constructor_tail_block(block: &Block) -> bool {
    matches!(block.stmts.as_slice(), [Stmt::Expr(expr)] if destination_constructor_tail_expr(expr))
}

fn destination_constructor_tail_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Ctor { .. } => true,
        Expr::If {
            then_block,
            else_block: Some(else_block),
            ..
        } => {
            destination_constructor_tail_block(then_block)
                && destination_constructor_tail_block(else_block)
        }
        Expr::Match { arms, .. } => {
            !arms.is_empty()
                && arms
                    .iter()
                    .all(|arm| destination_constructor_tail_expr(&arm.body))
        }
        Expr::Block(block) => destination_constructor_tail_block(block),
        _ => false,
    }
}

fn destination_layout_is_flat(cg: &Codegen<'_>, id: LayoutId) -> bool {
    let Some(layout) = cg.specialized_layouts.get(id) else {
        return false;
    };
    layout
        .fields()
        .iter()
        .all(|field| matches!(field.kind(), FieldKind::Scalar(_)))
        && layout.variant_layouts().iter().all(|variant| {
            variant
                .fields()
                .iter()
                .all(|field| matches!(field.kind(), FieldKind::Scalar(_)))
        })
}

fn direct_scalar_result_fields(function: &Function) -> Option<&[Expr]> {
    let [Stmt::Expr(Expr::Ctor { args, .. })] = function.body.stmts.as_slice() else {
        return None;
    };
    args.iter().all(direct_scalar_result_expr).then_some(args)
}

fn direct_scalar_result_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Bool(_) | Expr::Var(_) => true,
        Expr::Unary { expr, .. } | Expr::As { expr, .. } => direct_scalar_result_expr(expr),
        Expr::Binary { lhs, rhs, .. } => {
            direct_scalar_result_expr(lhs) && direct_scalar_result_expr(rhs)
        }
        _ => false,
    }
}

/// The names of every JS-callable string export in declaration order (`__export_*`
/// wrappers are emitted for these and they are extra reachability roots).
fn string_export_functions(module: &Module) -> Vec<String> {
    let grantable = grantable_cap_names(module);
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) if is_string_export(f, &grantable) => Some(f.name.clone()),
            _ => None,
        })
        .collect()
}

/// (RFC-0040) The bare grantable capability type names declared in the module.
fn grantable_cap_names(module: &Module) -> HashSet<&str> {
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect()
}

fn is_compiler_syntax_type_name(name: &str) -> bool {
    matches!(
        name,
        "meta.ItemSyntax" | "meta.ModuleSyntax" | "meta.Span" | "meta.TypeSyntax" | "meta.ExprSyntax" | "meta.PatternSyntax"
            | "meta.SyntaxHole" | "meta.StmtSyntax" | "meta.BlockSyntax"
            | "meta.MatchArmSyntax" | "meta.ParamSyntax" | "meta.Ident" | "ItemSyntax"
            | "TypeSyntax" | "ExprSyntax" | "PatternSyntax" | "SyntaxHole"
            | "StmtSyntax" | "BlockSyntax" | "MatchArmSyntax" | "ParamSyntax" | "Ident"
            | "ModuleSyntax" | "Span"
    )
}

fn ast_type_mentions_compiler_syntax(ty: &Type) -> bool {
    match ty {
        Type::Named(name, args) => {
            is_compiler_syntax_type_name(name)
                || args.iter().any(ast_type_mentions_compiler_syntax)
        }
        Type::Dyn(_, args) => args.iter().any(ast_type_mentions_compiler_syntax),
        Type::Tuple(items) => items.iter().any(ast_type_mentions_compiler_syntax),
        Type::Fn(params, ret, _) => {
            params.iter().any(ast_type_mentions_compiler_syntax)
                || ast_type_mentions_compiler_syntax(ret)
        }
        Type::RecordCompose { base, fields } => {
            ast_type_mentions_compiler_syntax(base)
                || fields
                    .iter()
                    .any(|(_, field)| ast_type_mentions_compiler_syntax(field))
        }
        Type::Qualified(_, inner) => ast_type_mentions_compiler_syntax(inner),
    }
}

fn function_signature_mentions_compiler_syntax(f: &Function) -> bool {
    f.params
        .iter()
        .filter_map(|p| p.ty.as_ref())
        .any(ast_type_mentions_compiler_syntax)
        || f.ret.as_ref().is_some_and(ast_type_mentions_compiler_syntax)
}

fn strip_compiler_syntax_items_for_runtime(mut module: Module) -> Module {
    module.items.retain(|item| match item {
        Item::Type(t) => !is_compiler_syntax_type_name(&t.name),
        Item::Function(f) => !function_signature_mentions_compiler_syntax(f),
        _ => true,
    });
    module
}

fn collect_gc_tuple_type(
    cg: &Codegen<'_>,
    ty: &Type,
    layouts: &mut BTreeMap<GcTupleShape, Vec<Type>>,
) {
    match ty {
        Type::Qualified(_, inner) => collect_gc_tuple_type(cg, inner, layouts),
        Type::Tuple(items) => {
            for item in items {
                collect_gc_tuple_type(cg, item, layouts);
            }
            if let Some(shape) = cg.gc_tuple_shape(ty) {
                layouts.entry(shape).or_insert_with(|| items.clone());
            }
        }
        Type::Named(_, args) | Type::Dyn(_, args) => {
            for arg in args {
                collect_gc_tuple_type(cg, arg, layouts);
            }
        }
        Type::Fn(params, ret, _) => {
            for param in params {
                collect_gc_tuple_type(cg, param, layouts);
            }
            collect_gc_tuple_type(cg, ret, layouts);
        }
        Type::RecordCompose { .. } => unreachable!(
            "compiler invariant violated: record composition must be normalized before Wasm layout collection"
        ),
    }
}

fn collect_gc_tuple_expr(
    cg: &Codegen<'_>,
    expr: &Expr,
    layouts: &mut BTreeMap<GcTupleShape, Vec<Type>>,
) {
    if let Some(ty) = cg.ast_type_of_expr(expr) {
        collect_gc_tuple_type(cg, &ty, layouts);
    }
    crate::escape::for_each_immediate_subexpr(expr, &mut |child| {
        collect_gc_tuple_expr(cg, child, layouts);
    });
}

fn collect_gc_tuple_block(
    cg: &Codegen<'_>,
    block: &Block,
    layouts: &mut BTreeMap<GcTupleShape, Vec<Type>>,
) {
    for stmt in &block.stmts {
        let expr = match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => Some(value),
            Stmt::Return(value) => value.as_ref(),
            Stmt::Break | Stmt::Continue => None,
        };
        if let Some(expr) = expr {
            collect_gc_tuple_expr(cg, expr, layouts);
        }
    }
}

fn collect_gc_tuple_layouts(
    cg: &Codegen<'_>,
    module: &Module,
) -> BTreeMap<GcTupleShape, Vec<Type>> {
    let mut layouts = BTreeMap::new();
    for item in &module.items {
        match item {
            Item::Function(function) => {
                for param in &function.params {
                    if let Some(ty) = &param.ty {
                        collect_gc_tuple_type(cg, ty, &mut layouts);
                    }
                }
                if let Some(ret) = &function.ret {
                    collect_gc_tuple_type(cg, ret, &mut layouts);
                }
                collect_gc_tuple_block(cg, &function.body, &mut layouts);
            }
            Item::Type(def) => {
                for field in def.variants.iter().flat_map(|variant| &variant.fields) {
                    collect_gc_tuple_type(cg, field, &mut layouts);
                }
            }
            Item::TypeAlias { ty, .. } => collect_gc_tuple_type(cg, ty, &mut layouts),
            Item::Trait(_)
            | Item::Impl(_)
            | Item::Const { .. }
            | Item::Comptime(_) => {}
        }
    }
    layouts
}

fn collect_lambda_env_keys_expr(owner: &str, expr: &Expr, keys: &mut Vec<u64>) {
    if let Expr::Lambda { params, body, .. } = expr {
        keys.push(Codegen::lambda_content_key(owner, params, body));
    }
    crate::escape::for_each_immediate_subexpr(expr, &mut |child| {
        collect_lambda_env_keys_expr(owner, child, keys);
    });
}

fn collect_lambda_env_keys_block(owner: &str, block: &Block, keys: &mut Vec<u64>) {
    for stmt in &block.stmts {
        let expr = match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => Some(value),
            Stmt::Return(value) => value.as_ref(),
            Stmt::Break | Stmt::Continue => None,
        };
        if let Some(expr) = expr {
            collect_lambda_env_keys_expr(owner, expr, keys);
        }
    }
}

#[derive(Clone)]
struct GcNominalPlan {
    owner: String,
    variant_names: Vec<String>,
    variants: Vec<Vec<Type>>,
}

fn type_has_planned_reference(
    cg: &Codegen<'_>,
    ty: &Type,
    storage: &witchy_types::storage::ReferenceStorageClassifier<'_>,
    nominals: &BTreeMap<String, GcNominalPlan>,
    reference_lists: &BTreeMap<String, Type>,
) -> bool {
    match ty.unqualified() {
        Type::Fn(_, _, _) => true,
        // An existential's erased envelope is always a GC reference. Its trait
        // arguments may themselves contain references, but cannot determine
        // whether the envelope needs a typed reference container.
        Type::Dyn(_, _) => true,
        Type::Tuple(items) => items.iter().any(|item| {
            type_has_planned_reference(
                cg,
                item,
                storage,
                nominals,
                reference_lists,
            )
        }),
        Type::Named(name, args) if name == "List" => {
            reference_lists.contains_key(&cg.gc_lookup_type_key(ty))
                || args.first().is_some_and(|element| {
                    type_has_planned_reference(
                        cg,
                        element,
                        storage,
                        nominals,
                        reference_lists,
                    )
                })
        }
        Type::Named(_, args) => {
            storage.first_reference(ty).is_some()
                || nominals.contains_key(&cg.gc_lookup_type_key(ty))
                || args.iter().any(|arg| {
                    type_has_planned_reference(
                        cg,
                        arg,
                        storage,
                        nominals,
                        reference_lists,
                    )
                })
        }
        Type::RecordCompose { .. } => unreachable!(
            "compiler invariant violated: record composition must be normalized before Wasm reference planning"
        ),
        Type::Qualified(_, _) => unreachable!("unqualified above"),
    }
}

fn collect_gc_type_plans(
    cg: &Codegen<'_>,
    ty: &Type,
    defs: &HashMap<String, &witchy_syntax::ast::TypeDef>,
    storage: &witchy_types::storage::ReferenceStorageClassifier<'_>,
    nominals: &mut BTreeMap<String, GcNominalPlan>,
    reference_lists: &mut BTreeMap<String, Type>,
) {
    let ty = ty.unqualified();
    match ty {
        Type::Named(name, args) => {
            for arg in args {
                collect_gc_type_plans(
                    cg,
                    arg,
                    defs,
                    storage,
                    nominals,
                    reference_lists,
                );
            }
            if name == "List" {
                if let Some(element) = args.first()
                    && type_has_planned_reference(
                        cg,
                        element,
                        storage,
                        nominals,
                        reference_lists,
                    )
                {
                    reference_lists
                        .entry(cg.gc_lookup_type_key(ty))
                        .or_insert_with(|| ty.clone());
                }
                return;
            }
            if matches!(name.as_str(), "Dict") {
                return;
            }
            let owner = cg.gc_nominal_names.get(name).unwrap_or(name);
            if type_has_var(ty) {
                return;
            }
            // Nullable direct externrefs keep the established zero-allocation
            // `None = null` representation rather than receiving an ADT box.
            if name == "Option"
                && args.len() == 1
                && !matches!(args[0].unqualified(), Type::Named(inner, _) if inner == "Option")
                && storage.first_reference(&args[0]).is_some()
            {
                return;
            }
            let (variant_names, variants) = if let Some(def) = defs.get(owner) {
                if witchy_types::typeck::type_def_params(def).len() != args.len() {
                    return;
                }
                (
                    def.variants
                        .iter()
                        .map(|variant| variant.name.clone())
                        .collect(),
                    witchy_types::storage::instantiate_type_def_fields(def, args),
                )
            } else if name == "Result" && args.len() == 2 {
                (
                    vec!["Ok".to_string(), "Err".to_string()],
                    vec![vec![args[0].clone()], vec![args[1].clone()]],
                )
            } else {
                return;
            };
            if !variants
                .iter()
                .flatten()
                .any(|field| storage.requires_reference_storage(field))
            {
                return;
            }
            let key = cg.gc_lookup_type_key(ty);
            if nominals.contains_key(&key) {
                return;
            }
            nominals.insert(
                key,
                GcNominalPlan {
                    owner: owner.clone(),
                    variant_names,
                    variants: variants.clone(),
                },
            );
            for field in variants.iter().flatten() {
                collect_gc_type_plans(
                    cg,
                    field,
                    defs,
                    storage,
                    nominals,
                    reference_lists,
                );
            }
        }
        Type::Tuple(items) | Type::Dyn(_, items) => {
            for item in items {
                collect_gc_type_plans(
                    cg,
                    item,
                    defs,
                    storage,
                    nominals,
                    reference_lists,
                );
            }
        }
        Type::Fn(params, result, _) => {
            for param in params {
                collect_gc_type_plans(
                    cg,
                    param,
                    defs,
                    storage,
                    nominals,
                    reference_lists,
                );
            }
            collect_gc_type_plans(
                cg,
                result,
                defs,
                storage,
                nominals,
                reference_lists,
            );
        }
        Type::RecordCompose { .. } => unreachable!(
            "compiler invariant violated: record composition must be normalized before Wasm layout planning"
        ),
        Type::Qualified(_, _) => unreachable!("unqualified above"),
    }
}

fn collect_gc_expr_plans(
    cg: &Codegen<'_>,
    expr: &Expr,
    defs: &HashMap<String, &witchy_syntax::ast::TypeDef>,
    storage: &witchy_types::storage::ReferenceStorageClassifier<'_>,
    nominals: &mut BTreeMap<String, GcNominalPlan>,
    reference_lists: &mut BTreeMap<String, Type>,
) {
    if let Some(ty) = cg.ast_type_of_expr(expr) {
        collect_gc_type_plans(
            cg,
            &ty,
            defs,
            storage,
            nominals,
            reference_lists,
        );
    }
    crate::escape::for_each_immediate_subexpr(expr, &mut |child| {
        collect_gc_expr_plans(
            cg,
            child,
            defs,
            storage,
            nominals,
            reference_lists,
        );
    });
}

fn collect_gc_block_plans(
    cg: &Codegen<'_>,
    block: &Block,
    defs: &HashMap<String, &witchy_syntax::ast::TypeDef>,
    storage: &witchy_types::storage::ReferenceStorageClassifier<'_>,
    nominals: &mut BTreeMap<String, GcNominalPlan>,
    reference_lists: &mut BTreeMap<String, Type>,
) {
    for stmt in &block.stmts {
        // A declaration type can carry representation demand that the RHS's
        // pre-coercion TypeTable entry does not. In particular, a `List(dyn T)`
        // literal starts as concrete elements and is packed after annotation.
        if let Stmt::Let { ty: Some(ty), .. } = stmt {
            collect_gc_type_plans(
                cg,
                ty,
                defs,
                storage,
                nominals,
                reference_lists,
            );
        }
        let expr = match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => Some(value),
            Stmt::Return(value) => value.as_ref(),
            Stmt::Break | Stmt::Continue => None,
        };
        if let Some(expr) = expr {
            collect_gc_expr_plans(
                cg,
                expr,
                defs,
                storage,
                nominals,
                reference_lists,
            );
        }
    }
}

fn gc_type_plans(
    cg: &Codegen<'_>,
    module: &Module,
    reachable: &HashSet<String>,
) -> (BTreeMap<String, GcNominalPlan>, BTreeMap<String, Type>) {
    let defs: HashMap<String, &witchy_syntax::ast::TypeDef> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(def) => Some((def.name.clone(), def)),
            _ => None,
        })
        .collect();
    let storage = witchy_types::storage::ReferenceStorageClassifier::new(module);
    let mut nominals = BTreeMap::new();
    let mut reference_lists = BTreeMap::new();
    for item in &module.items {
        if let Item::Function(function) = item
            && reachable.contains(&function.name)
        {
            for ty in function.params.iter().filter_map(|param| param.ty.as_ref()) {
                collect_gc_type_plans(
                    cg,
                    ty,
                    &defs,
                    &storage,
                    &mut nominals,
                    &mut reference_lists,
                );
            }
            if let Some(ty) = &function.ret {
                collect_gc_type_plans(
                    cg,
                    ty,
                    &defs,
                    &storage,
                    &mut nominals,
                    &mut reference_lists,
                );
            }
            collect_gc_block_plans(
                cg,
                &function.body,
                &defs,
                &storage,
                &mut nominals,
                &mut reference_lists,
            );
        }
    }
    (nominals, reference_lists)
}

/// (RFC-0040) If `f` is a cap-gated string export (`export_*(cap, String)`), the
/// leading grantable capability's `(type name, field count)`.
fn export_cap_of<'a>(f: &'a Function, module: &'a Module) -> Option<(&'a str, usize)> {
    let grantable = grantable_cap_names(module);
    let cap = match f.params.as_slice() {
        [cap, _s] => crate::codegen::export_cap_name(cap).filter(|n| grantable.contains(n))?,
        _ => return None,
    };
    let nfields = module.items.iter().find_map(|it| match it {
        Item::Type(t) if t.name == cap => t.variants.first().map(|v| v.fields.len()),
        _ => None,
    })?;
    Some((cap, nfields))
}

#[derive(Debug, Clone)]
struct GlamourExportFamily {
    init: String,
    dispatch: String,
    emit: String,
    release: String,
    cap_type: String,
    cap_fields: usize,
    state_type: Type,
    state_fields: Vec<Type>,
    state_constructor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlamourDevelopmentField {
    I64,
    F64,
    Bool,
    /// A compiler-known public-model field whose nested representation remains
    /// inside Wasm. Development tracing reports only its structural change bit;
    /// format-1 hot-swap snapshots remain restricted to scalar fields.
    Aggregate,
}

impl GlamourDevelopmentField {
    fn wire_tag(self) -> u8 {
        match self {
            Self::I64 => 1,
            Self::F64 => 2,
            Self::Bool => 3,
            Self::Aggregate => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlamourDevelopmentMetadata {
    pub model_schema: [u8; 32],
    pub authorization_schema: [u8; 32],
    pub state_fields: Vec<GlamourDevelopmentField>,
    pub state_field_names: Vec<String>,
    snapshot_codec: Option<GlamourDevelopmentCodecSpec>,
}

impl GlamourDevelopmentMetadata {
    pub const SCALAR_SNAPSHOT_FORMAT: u16 = 1;
    pub const AGGREGATE_SNAPSHOT_FORMAT: u16 = 2;

    pub fn supports_snapshot(&self) -> bool {
        self.snapshot_format() != 0
    }

    pub fn snapshot_format(&self) -> u16 {
        if self
            .state_fields
            .iter()
            .all(|field| *field != GlamourDevelopmentField::Aggregate)
        {
            Self::SCALAR_SNAPSHOT_FORMAT
        } else if self.snapshot_codec.is_some() {
            Self::AGGREGATE_SNAPSHOT_FORMAT
        } else {
            0
        }
    }

    pub fn model_schema_hex(&self) -> String {
        hex_bytes(&self.model_schema)
    }

    pub fn authorization_schema_hex(&self) -> String {
        hex_bytes(&self.authorization_schema)
    }

    pub fn migration_schema_hexes(&self) -> Vec<String> {
        self.snapshot_codec
            .as_ref()
            .map(|codec| {
                codec
                    .migrations
                    .iter()
                    .map(|migration| hex_bytes(&migration.model_schema))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn wire_payload(&self) -> Vec<u8> {
        let name_bytes = self
            .state_field_names
            .iter()
            .map(|name| 2 + name.len())
            .sum::<usize>();
        let mut payload = Vec::with_capacity(80 + self.state_fields.len() + name_bytes);
        payload.extend_from_slice(b"WGDM");
        payload.extend_from_slice(&2_u16.to_le_bytes());
        payload.extend_from_slice(&self.snapshot_format().to_le_bytes());
        payload.extend_from_slice(&1_u16.to_le_bytes());
        payload.extend_from_slice(&4_u16.to_le_bytes());
        payload.extend_from_slice(
            &u32::try_from(self.state_fields.len()).unwrap_or(u32::MAX).to_le_bytes(),
        );
        payload.extend_from_slice(&self.model_schema);
        payload.extend_from_slice(&self.authorization_schema);
        payload.extend(self.state_fields.iter().map(|field| field.wire_tag()));
        for name in &self.state_field_names {
            payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
            payload.extend_from_slice(name.as_bytes());
        }
        payload
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledDevelopmentModule {
    pub wasm: Vec<u8>,
    pub glamour: Option<GlamourDevelopmentMetadata>,
    pub source_instructions: Vec<witchy_wir::wir_encode::SourceInstructionRange>,
    pub source_expressions:
        Vec<witchy_wir::wir_encode::SourceExpressionInstructionRange>,
}

fn function_tail_name(function: &Function) -> &str {
    function.name.rsplit('.').next().unwrap_or(&function.name)
}

fn is_plain_named(ty: &Option<Type>, expected: &str) -> bool {
    matches!(ty.as_ref().map(Type::unqualified), Some(Type::Named(name, args))
        if name == expected && args.is_empty())
}

/// RFC-0108's compiler-owned state boundary. The four source functions are
/// ordinary typed Witchy and are never exported directly; this validates the
/// family before WIR reachability or wrapper synthesis can observe it.
fn glamour_export_family(module: &Module) -> Result<Option<GlamourExportFamily>, CodegenError> {
    const MEMBERS: [&str; 4] = [
        "glamour_init",
        "glamour_dispatch",
        "glamour_emit",
        "glamour_release",
    ];
    let mut found: HashMap<&str, &Function> = HashMap::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        let tail = function_tail_name(function);
        if MEMBERS.contains(&tail) {
            if found.insert(tail, function).is_some() {
                return Err(CodegenError {
                    message: format!("duplicate RFC-0108 application function `{tail}`"),
                });
            }
        }
    }
    if found.is_empty() {
        return Ok(None);
    }
    for name in MEMBERS {
        if !found.contains_key(name) {
            return Err(CodegenError {
                message: format!(
                    "incomplete RFC-0108 application export family: missing `{name}`"
                ),
            });
        }
    }
    let init = found["glamour_init"];
    let dispatch = found["glamour_dispatch"];
    let emit = found["glamour_emit"];
    let release = found["glamour_release"];
    for function in [init, dispatch, emit, release] {
        if !function.public
            || function.comptime_only
            || function.is_async
            || function.is_gen
            || !function.bounds.is_empty()
            || !function.attributes.iter().any(|attribute| attribute == "browser")
        {
            return Err(CodegenError {
                message: format!(
                    "`{}` must be public, non-generic, synchronous, and `@browser`",
                    function_tail_name(function)
                ),
            });
        }
    }
    let grantable = grantable_cap_names(module);
    let [root, init_input] = init.params.as_slice() else {
        return Err(CodegenError {
            message: "`glamour_init` must have `(UiRoot, Bytes) -> State` shape".into(),
        });
    };
    let Some(cap_type) = export_cap_name(root).filter(|name| grantable.contains(name)) else {
        return Err(CodegenError {
            message: "`glamour_init` must begin with one bare grantable root capability".into(),
        });
    };
    if !is_plain_named(&init_input.ty, "Bytes") {
        return Err(CodegenError {
            message: "`glamour_init` input must be `Bytes`".into(),
        });
    }
    let Some(state_type) = init.ret.clone() else {
        return Err(CodegenError {
            message: "`glamour_init` must return a private nominal state value".into(),
        });
    };
    if !matches!(state_type.unqualified(), Type::Named(name, args)
        if name != "Bytes" && name != "String" && args.is_empty())
    {
        return Err(CodegenError {
            message: "`glamour_init` state must be a private nominal type".into(),
        });
    }
    let [dispatch_state, dispatch_input] = dispatch.params.as_slice() else {
        return Err(CodegenError {
            message: "`glamour_dispatch` must have `(State, Bytes) -> State` shape".into(),
        });
    };
    if dispatch_state.ty.as_ref() != Some(&state_type)
        || dispatch.ret.as_ref() != Some(&state_type)
        || !is_plain_named(&dispatch_input.ty, "Bytes")
    {
        return Err(CodegenError {
            message: "`glamour_dispatch` must accept and return the init state type plus `Bytes`"
                .into(),
        });
    }
    let [emit_state] = emit.params.as_slice() else {
        return Err(CodegenError {
            message: "`glamour_emit` must have `(State) -> Bytes` shape".into(),
        });
    };
    if emit_state.ty.as_ref() != Some(&state_type) || !is_plain_named(&emit.ret, "Bytes") {
        return Err(CodegenError {
            message: "`glamour_emit` must accept the state type and return `Bytes`".into(),
        });
    }
    let [release_state] = release.params.as_slice() else {
        return Err(CodegenError {
            message: "`glamour_release` must have `(own State) -> Nil` shape".into(),
        });
    };
    if release_state.ty.as_ref() != Some(&state_type)
        || release_state.convention != Convention::Own
    {
        return Err(CodegenError {
            message: "`glamour_release` must consume `own` the same state type".into(),
        });
    }
    let cap_fields = module.items.iter().find_map(|item| match item {
        Item::Type(definition) if definition.name == cap_type => {
            definition.variants.first().map(|variant| variant.fields.len())
        }
        _ => None,
    }).ok_or_else(|| CodegenError {
        message: "RFC-0108 root capability definition is missing".into(),
    })?;
    let state_name = match state_type.unqualified() {
        Type::Named(name, _) => name,
        _ => unreachable!("state nominal checked above"),
    };
    let (state_fields, state_constructor) = module.items.iter().find_map(|item| match item {
        Item::Type(definition) if definition.name == *state_name => {
            Some(if definition.variants.len() == 1 {
                (
                    definition.variants[0].fields.clone(),
                    Some(definition.variants[0].name.clone()),
                )
            } else {
                (Vec::new(), None)
            })
        }
        _ => None,
    }).ok_or_else(|| CodegenError {
        message: "RFC-0108 state definition is missing".into(),
    })?;
    Ok(Some(GlamourExportFamily {
        init: init.name.clone(),
        dispatch: dispatch.name.clone(),
        emit: emit.name.clone(),
        release: release.name.clone(),
        cap_type: cap_type.to_string(),
        cap_fields,
        state_type,
        state_fields,
        state_constructor,
    }))
}

fn checked_glamour_development_metadata(
    checked: &witchy_types::pipeline::CheckedModule,
) -> Result<Option<GlamourDevelopmentMetadata>, CodegenError> {
    let module = checked.module();
    let init = module.items.iter().find_map(|item| match item {
        Item::Function(function) if function_tail_name(function) == "glamour_init" => {
            Some(function)
        }
        _ => None,
    });
    let Some(init) = init else { return Ok(None) };
    let Some(state_type) = init.ret.as_ref() else { return Ok(None) };
    let Type::Named(state_name, state_arguments) = state_type.unqualified() else {
        return Ok(None);
    };
    if !state_arguments.is_empty() {
        return Ok(None);
    }
    let Some(root_type) = init.params.first().and_then(|param| param.ty.as_ref()) else {
        return Ok(None);
    };
    let catalog = checked.runtime_declaration_catalog().map_err(|error| CodegenError {
        message: format!("cannot authenticate Glamour development schema: {error}"),
    })?;
    let state_identity = catalog.type_identity(state_type).map_err(|error| CodegenError {
        message: format!("cannot authenticate Glamour model schema: {error}"),
    })?;
    let root_identity = catalog.type_identity(root_type).map_err(|error| CodegenError {
        message: format!("cannot authenticate Glamour authorization schema: {error}"),
    })?;
    let state_definition = module.items.iter().find_map(|item| match item {
        Item::Type(definition)
            if definition.name == *state_name
                || catalog.resolve(
                    &definition.name,
                    witchy_types::runtime_type::DeclarationKind::Type,
                ) == match &state_identity {
                    witchy_types::runtime_type::RuntimeTypeIdentity::Nominal {
                        declaration,
                        ..
                    } => Some(declaration),
                    _ => None,
                } =>
        {
            Some(definition)
        }
        _ => None,
    });
    let Some(state_definition) = state_definition else { return Ok(None) };
    let [state_variant] = state_definition.variants.as_slice() else {
        return Ok(None);
    };
    let state_field_names = if state_variant.field_names.len() == state_variant.fields.len() {
        state_variant.field_names.clone()
    } else {
        vec![String::new(); state_variant.fields.len()]
    };
    if state_field_names.iter().any(|name| name.len() > 1024) {
        return Err(CodegenError {
            message: "Glamour development model field name exceeds 1024 bytes".into(),
        });
    }
    let mut state_fields = Vec::with_capacity(state_variant.fields.len());
    for field in &state_variant.fields {
        let scalar = match field.unqualified() {
            Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
                "Int" | "Duration" => GlamourDevelopmentField::I64,
                "Float" => GlamourDevelopmentField::F64,
                "Bool" => GlamourDevelopmentField::Bool,
                _ => GlamourDevelopmentField::Aggregate,
            },
            _ => GlamourDevelopmentField::Aggregate,
        };
        state_fields.push(scalar);
    }
    let root_definition = match &root_identity {
        witchy_types::runtime_type::RuntimeTypeIdentity::Nominal { declaration, .. } => {
            module.items.iter().find_map(|item| match item {
                Item::Type(definition)
                    if catalog.resolve(
                        &definition.name,
                        witchy_types::runtime_type::DeclarationKind::Type,
                    ) == Some(declaration) =>
                {
                    Some(definition)
                }
                _ => None,
            })
        }
        _ => None,
    };
    let public_schema = public_state_schema(module, state_type);
    let model_schema = public_schema.as_ref().map_or_else(
        || {
            schema_digest(
                b"witchy.glamour.model-schema.v1",
                &format!("{state_identity:?}|{}", type_definition_schema(state_definition)),
            )
        },
        |schema| schema_digest(b"witchy.glamour.public-model-schema.v2", schema),
    );
    let authorization_schema = schema_digest(
        b"witchy.glamour.authorization-schema.v1",
        &format!(
            "{root_identity:?}|{}",
            root_definition.map(type_definition_schema).unwrap_or_default()
        ),
    );
    let aggregate = state_fields.contains(&GlamourDevelopmentField::Aggregate);
    let snapshot_codec = if aggregate && public_schema.is_some() {
        let owner = module.linked_entry.as_deref().unwrap_or_default();
        let name = |local: &str| {
            if owner.is_empty() { local.to_string() } else { format!("{owner}.{local}") }
        };
        let mut migration_functions = module.items.iter().filter_map(|item| match item {
            Item::Function(function) if function_tail_name(function) == "glamour_migrate" => {
                Some(function)
            }
            _ => None,
        });
        let migration = migration_functions.next();
        if migration_functions.next().is_some() {
            return Err(CodegenError {
                message: "Glamour development module defines more than one `glamour_migrate`"
                    .into(),
            });
        }
        let mut migrations = Vec::new();
        if let Some(migration) = migration {
            let [source] = migration.params.as_slice() else {
                return Err(CodegenError {
                    message: "`glamour_migrate` must accept exactly one previous PublicState model"
                        .into(),
                });
            };
            let source_type = source.ty.clone().ok_or_else(|| CodegenError {
                message: "`glamour_migrate` parameter must have an explicit PublicState type"
                    .into(),
            })?;
            if migration.ret.as_ref() != Some(state_type)
                || migration.comptime_only
                || migration.is_async
                || migration.is_gen
                || !migration.bounds.is_empty()
            {
                return Err(CodegenError {
                    message: "`glamour_migrate` must be a synchronous, non-generic `fn(OldState) -> State`"
                        .into(),
                });
            }
            let source_schema = public_state_schema(module, &source_type).ok_or_else(|| {
                CodegenError {
                    message: "`glamour_migrate` input must have a compiler-authenticated PublicState shape"
                        .into(),
                }
            })?;
            let source_schema = schema_digest(
                b"witchy.glamour.public-model-schema.v2",
                &source_schema,
            );
            if source_schema == model_schema {
                return Err(CodegenError {
                    message: "`glamour_migrate` input schema is identical to the current model"
                        .into(),
                });
            }
            migrations.push(GlamourDevelopmentMigrationCodec {
                model_schema: source_schema,
                source_type,
                decoder: name("glamour_development_migrate_decode"),
                migration: migration.name.clone(),
            });
        }
        Some(GlamourDevelopmentCodecSpec {
            state_type: state_type.clone(),
            encoder: name("glamour_development_snapshot_encode"),
            decoder: name("glamour_development_snapshot_decode"),
            migrations,
        })
    } else {
        None
    };
    Ok(Some(GlamourDevelopmentMetadata {
        model_schema,
        authorization_schema,
        state_fields,
        state_field_names,
        snapshot_codec,
    }))
}

fn public_state_schema(module: &Module, ty: &Type) -> Option<String> {
    fn definition_for<'a>(module: &'a Module, name: &str) -> Option<&'a witchy_syntax::ast::TypeDef> {
        if let Some(exact) = module.items.iter().find_map(|item| match item {
            Item::Type(definition) if definition.name == name => Some(definition),
            _ => None,
        }) {
            return Some(exact);
        }
        let mut matches = module.items.iter().filter_map(|item| match item {
            Item::Type(definition) if definition.name.ends_with(&format!(".{name}")) => {
                Some(definition)
            }
            _ => None,
        });
        let definition = matches.next()?;
        matches.next().is_none().then_some(definition)
    }

    fn visit(
        module: &Module,
        ty: &Type,
        seen: &mut BTreeMap<String, usize>,
    ) -> Option<String> {
        let ty = ty.unqualified();
        let material = witchy_syntax::format::type_str(ty);
        if let Some(index) = seen.get(&material) {
            return Some(format!("ref={index}"));
        }
        let index = seen.len();
        seen.insert(material.clone(), index);
        let Type::Named(name, arguments) = ty else { return None };
        let head = name.rsplit('.').next().unwrap_or(name);
        match (head, arguments.as_slice()) {
            ("Nil" | "Bool" | "Int" | "Float" | "Duration" | "String", []) => {
                Some(format!("{index}:std={head}"))
            }
            ("List" | "Option", [item]) => Some(format!(
                "{index}:std={head}<{}>",
                visit(module, item, seen)?,
            )),
            ("Result", [ok, error]) => Some(format!(
                "{index}:std=Result<{},{}>",
                visit(module, ok, seen)?,
                visit(module, error, seen)?,
            )),
            _ => {
                let definition = definition_for(module, name)
                    .or_else(|| definition_for(module, head))?;
                if definition.sealed
                    || definition.is_capability
                    || !definition.public_state_derived
                    || definition.params.len() != arguments.len()
                {
                    return None;
                }
                let fields = witchy_types::storage::instantiate_type_def_fields(
                    definition,
                    arguments,
                );
                let mut output = format!(
                    "{index}:nominal={}|args={arguments:?}|variants={}",
                    definition.name,
                    definition.variants.len(),
                );
                for (variant, fields) in definition.variants.iter().zip(fields) {
                    output.push_str(&format!(
                        "|variant={}|names={:?}",
                        variant.name, variant.field_names,
                    ));
                    for field in fields {
                        output.push_str("|field=");
                        output.push_str(&visit(module, &field, seen)?);
                    }
                }
                Some(output)
            }
        }
    }

    visit(module, ty, &mut BTreeMap::new())
}

fn type_definition_schema(definition: &witchy_syntax::ast::TypeDef) -> String {
    let mut output = format!(
        "{}|params={:?}|sealed={}|capability={}|grantable={}|packed={}",
        definition.name,
        definition.params,
        definition.sealed,
        definition.is_capability,
        definition.grantable,
        definition.packed,
    );
    for variant in &definition.variants {
        output.push_str(&format!(
            "|variant={}|names={:?}|fields={:?}",
            variant.name, variant.field_names, variant.fields
        ));
    }
    output
}

fn schema_digest(domain: &[u8], material: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update([0]);
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    hash.update([0]);
    hash.update(material.as_bytes());
    hash.finalize().into()
}

fn hex_bytes(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

/// The functions reachable from `main` (+ string-export roots), plus `extra_roots`
/// — additional reachability roots for functions a reached AST body does not name
/// directly. (RFC-0047) A container `==` over a CUSTOM-`PartialEq` element type
/// calls that type's `PartialEq__T__eq` from a codegen-synthesized eq helper, so the
/// call is invisible to the AST walk; seeding those impls as roots keeps them (and
/// their transitive callees) emitted, so the honored-at-every-depth guarantee holds
/// for the compiled backend too.
fn reachable_functions_with(module: &Module, extra_roots: &[String]) -> HashSet<String> {
    let mut bodies: HashMap<&str, &Block> = HashMap::new();
    for item in &module.items {
        if let Item::Function(f) = item {
            bodies.insert(f.name.as_str(), &f.body);
        }
    }
    let mut reachable: HashSet<String> = HashSet::new();
    let mut work: Vec<String> = Vec::new();
    if bodies.contains_key("main") {
        reachable.insert("main".to_string());
        work.push("main".to_string());
    }
    // String exports (`pub fn f(String) -> String`) are additional roots: the host
    // calls them directly through their `__export_*` wrapper, so they must be
    // compiled and kept even when `main` never reaches them.
    for name in string_export_functions(module) {
        if reachable.insert(name.clone()) {
            work.push(name);
        }
    }
    for name in extra_roots {
        if bodies.contains_key(name.as_str()) && reachable.insert(name.clone()) {
            work.push(name.clone());
        }
    }
    while let Some(name) = work.pop() {
        if let Some(body) = bodies.get(name.as_str()) {
            let mut refs = HashSet::new();
            collect_fn_refs_block(body, &mut refs);
            for r in refs {
                if bodies.contains_key(r.as_str()) && reachable.insert(r.clone()) {
                    work.push(r);
                }
            }
        }
    }
    reachable
}

fn eq_impl_types(module: &Module) -> HashSet<String> {
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Impl(im) if im.trait_name.as_deref() == Some("Eq") => Some(im.type_name.clone()),
            _ => None,
        })
        .collect()
}

fn custom_eq_function_roots(module: &Module) -> Vec<String> {
    let functions: HashSet<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some(function.name.as_str()),
            _ => None,
        })
        .collect();
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(def) if !def.partial_eq_derived => {
                let name = format!("PartialEq__{}__eq", def.name);
                functions.contains(name.as_str()).then_some(name)
            }
            _ => None,
        })
        .collect()
}

fn transparent_externref_brand_entries(module: &Module) -> Vec<(String, String, Type)> {
    let candidates: Vec<(String, String, Type)> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(t) if t.is_capability && t.variants.len() == 1 => {
                let variant = t.variants.first()?;
                if variant.name == t.name && variant.field_names.is_empty() && variant.fields.len() == 1 {
                    Some((t.name.clone(), variant.name.clone(), variant.fields[0].clone()))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    let mut transparent: HashSet<String> = HashSet::new();
    let mut out: Vec<(String, String, Type)> = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        for (brand, ctor, field) in &candidates {
            if transparent.contains(brand) {
                continue;
            }
            let is_ref = match field.unqualified() {
                Type::Named(n, _) if is_builtin_externref_type(n) => true,
                Type::Named(n, args) if args.is_empty() => transparent.contains(n),
                _ => false,
            };
            if is_ref {
                transparent.insert(brand.clone());
                out.push((brand.clone(), ctor.clone(), field.clone()));
                changed = true;
            }
        }
    }
    out
}

/// Register every item's compile-time metadata (parameter conventions,
/// return kinds/types, record fields, generic shape hints, ...) on `cg`.
fn register_module_items(
    cg: &mut Codegen,
    module: &Module,
    reachable: &HashSet<String>,
    witnesses: &witchy_types::witness::WitnessPlan,
    generic_specializations: &BTreeMap<
        String,
        witchy_types::traits::LogicalSpecializationIdentity,
    >,
) -> Result<(), CodegenError> {
    register_specialized_layouts(cg, module);
    cg.register_generic_callable_instances(generic_specializations)?;
    let existential_dispatch = witnesses
        .dispatch_index()
        .expect("witness construction assigns dense runtime IDs")
        .table_len(witnesses.witnesses.len())
        .expect("existential witness table fits u32 addressing");
    cg.existential_table_len = existential_dispatch;
    cg.existential_dispatch_stride = witnesses
        .dispatch_index()
        .expect("witness construction assigns dense runtime IDs")
        .stride();
    cg.existential_upcasts = witnesses
        .upcasts
        .iter()
        .filter_map(|upcast| {
            witnesses
                .by_id(upcast.target)
                .map(|target| (upcast.source, target.existential.clone(), upcast.target))
        })
        .collect();
    // `Option`/`Result` are language-level (`?`, `Some`/`Ok` literals, the
    // interpreter evaluates them natively): their constructors exist for
    // patterns whether or not std/option / std/result are linked. Tags match
    // the std declarations (Some=0/None=1, Ok=0/Err=1); if the modules ARE
    // linked, the Item::Type pass below re-registers identical values.
    for (ty, variants) in [
        ("Option", [("Some", 1usize), ("None", 0)]),
        ("Result", [("Ok", 1), ("Err", 1)]),
    ] {
        cg.adt_variant_names
            .insert(ty.to_string(), variants.iter().map(|(n, _)| n.to_string()).collect());
        for (tag, (name, nfields)) in variants.iter().enumerate() {
            cg.ctor_type_name.insert(name.to_string(), ty.to_string());
            cg.ctors.insert(name.to_string(), (tag as u32, *nfields));
        }
    }
    for (brand, ctor, field) in transparent_externref_brand_entries(module) {
        cg.transparent_externref_brands.insert(brand);
        cg.transparent_externref_ctors.insert(ctor, field);
    }
    // Types zero and one are stable across every module: all first-class
    // function values use type zero, and RFC-0081 packs use the erased wrapper
    // at type one. Concrete payload boxes are reserved below and retain their
    // real field kinds after the ordinary nominal layouts have been assigned.
    cg.gc_structs.push(witchy_wir::wir::closure_wrapper_struct());
    cg.gc_structs.push(witchy_wir::wir::existential_wrapper_struct());
    cg.gc_structs.push(witchy_wir::wir::reference_i64_cell_struct());
    cg.gc_structs.push(witchy_wir::wir::place_reference_struct());
    cg.gc_structs.push(witchy_wir::wir::reference_i32_cell_struct());
    // Every witness for the same concrete type must name the same payload-box
    // layout. A supertrait upcast changes only the authenticated witness ID and
    // deliberately reuses the owned payload reference; allocating one nominal
    // box type per witness would make the target adapter's `ref.cast` fail even
    // though the concrete payload is unchanged.
    let mut existential_payload_boxes = HashMap::new();
    for witness in &witnesses.witnesses {
        let key = cg.gc_lookup_type_key(&witness.concrete);
        let payload_id = if let Some(payload_id) = existential_payload_boxes.get(&key) {
            *payload_id
        } else {
            let payload_id = cg.gc_structs.len() as u32;
            cg.gc_structs.push(witchy_wir::wir::WirStructDef {
                fields: Vec::new(),
                mutable: false,
            });
            existential_payload_boxes.insert(key.clone(), payload_id);
            payload_id
        };
        cg.existential_payload_ids.insert(witness.id, payload_id);
        cg.existential_payload_type_ids.insert(key, payload_id);
    }
    let mut lambda_keys = Vec::new();
    for item in &module.items {
        if let Item::Function(function) = item {
            collect_lambda_env_keys_block(&function.name, &function.body, &mut lambda_keys);
        }
    }
    lambda_keys.sort_unstable();
    lambda_keys.dedup();
    for key in lambda_keys {
        let id = cg.gc_structs.len() as u32;
        cg.lambda_gc_env_ids.insert(key, id);
        cg.gc_structs.push(witchy_wir::wir::WirStructDef {
            fields: Vec::new(),
            mutable: true,
        });
    }
    for item in &module.items {
        if let Item::Type(def) = item {
            cg.gc_nominal_names.insert(def.name.clone(), def.name.clone());
            let bare = def.name.rsplit('.').next().unwrap_or(&def.name).to_string();
            cg.gc_nominal_names.entry(bare).or_insert_with(|| def.name.clone());
        }
    }
    // Demand-plan every closed nominal instance that transitively stores a
    // WebAssembly reference. Keys include the concrete type arguments, so
    // `Task(Int)` and `Task(String)` cannot accidentally share a field layout.
    let (gc_nominal_plans, gc_reference_lists) = gc_type_plans(cg, module, reachable);
    let gc_aggregate_name_set: HashSet<String> =
        gc_nominal_plans.values().map(|plan| plan.owner.clone()).collect();
    for key in gc_nominal_plans.keys() {
        let id = cg.gc_structs.len() as u32;
        cg.gc_aggregate_ids.insert(key.clone(), id);
        cg.gc_structs.push(witchy_wir::wir::WirStructDef {
            fields: Vec::new(),
            mutable: true,
        });
    }
    let mut gc_tuple_layouts = collect_gc_tuple_layouts(cg, module);
    for plan in gc_nominal_plans.values() {
        for field in plan.variants.iter().flatten() {
            collect_gc_tuple_type(cg, field, &mut gc_tuple_layouts);
        }
    }
    for shape in gc_tuple_layouts.keys() {
        let id = cg.gc_structs.len() as u32;
        cg.gc_tuple_ids.insert(shape.clone(), id);
        cg.gc_structs.push(witchy_wir::wir::WirStructDef {
            fields: Vec::new(),
            mutable: true,
        });
    }
    for (key, ty) in &gc_reference_lists {
        let Type::Named(_, args) = ty.unqualified() else {
            continue;
        };
        let Some(element) = args.first() else {
            continue;
        };
        let element_kind = cg.kind_for_type(element);
        if !matches!(element_kind, Kind::ExternRef | Kind::GcRef(_)) {
            continue;
        }
        let wir_element = Codegen::wir_kind(element_kind);
        if let Some(array_id) = cg
            .gc_arrays
            .iter()
            .position(|array| array.element == wir_element)
        {
            let type_id = cg.gc_structs.len() as u32 + array_id as u32;
            cg.gc_reference_list_ids.insert(key.clone(), type_id);
            continue;
        }
        let id = cg.gc_structs.len() as u32 + cg.gc_arrays.len() as u32;
        cg.gc_reference_list_ids.insert(key.clone(), id);
        cg.gc_arrays.push(witchy_wir::wir::WirArrayDef {
            element: wir_element,
        });
    }
    // Collect parameter conventions up front so call sites can resolve `var`
    // write-back even for forward references.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                if let Some(signature) = cg.access_facts.declaration(&f.name) {
                    cg.fn_conventions.insert(
                        f.name.clone(),
                        signature
                            .params()
                            .iter()
                            .map(|param| match param.kind() {
                                witchy_types::access::AccessKind::OwnedImmutable => Convention::Let,
                                witchy_types::access::AccessKind::SharedBorrow => Convention::Borrow,
                                witchy_types::access::AccessKind::ExclusiveWriteback => Convention::Var,
                                witchy_types::access::AccessKind::Consuming => Convention::Own,
                            })
                            .collect(),
                    );
                }
                let mut params = f.params.clone();
                if let Some(signature) = cg.access_facts.declaration(&f.name) {
                    for (param, access) in params.iter_mut().zip(signature.params()) {
                        param.ty = Some(access.ty().clone());
                    }
                }
                cg.fn_params.insert(f.name.clone(), params);
                let resolved_ret = cg
                    .access_facts
                    .declaration(&f.name)
                    .map(|signature| signature.result().ty())
                    .or(f.ret.as_ref());
                let ret = resolved_ret.map(|t| cg.kind_for_type(t)).unwrap_or(Kind::I32);
                cg.fn_ret.insert(f.name.clone(), ret);
                if let Some(t) = resolved_ret {
                    cg.fn_ret_valtype.insert(f.name.clone(), ty_to_valtype(t));
                    cg.fn_ret_ty.insert(f.name.clone(), t.clone());
                    // RFC-0111 destination ABI. An exact `unique` packed record
                    // retains the reassignment proof; a fixed closed sum also
                    // admits a nonescaping immediate-consumer scratch destination.
                    // Both require every result path to initialize a constructor,
                    // and the final checked access envelope must have no own/var
                    // state inputs before the hidden destination parameter.
                    let checked_access = cg.access_facts.declaration(&f.name);
                    let checked_ownership = checked_access
                        .map(|signature| {
                            cg.ownership_envelope_for_named_signature(&f.name, signature)
                        })
                        .unwrap_or_default();
                    if !f.public
                        && f.name.rsplit('.').next() != Some("main")
                        && destination_constructor_tail_block(&f.body)
                        && checked_access.is_some()
                        && checked_ownership.own_capacity_param.is_none()
                        && checked_ownership.var_capacity_params.is_empty()
                        && let Some(id) = cg
                            .specialized_type_ids
                            .iter()
                            .find(|(known, _)| known.unqualified() == t.unqualified())
                            .map(|(_, id)| *id)
                        && destination_layout_is_flat(cg, id)
                        && cg.specialized_layouts.get(id).is_some_and(|layout| {
                            matches!(layout.size(), LayoutSize::Fixed(_))
                                && (matches!(layout.kind(), LayoutKind::ClosedSum { .. })
                                    || (matches!(layout.kind(), LayoutKind::PackedRecord { .. })
                                        && checked_access.is_some_and(
                                            Codegen::signature_has_unique_layout_result,
                                        )))
                        })
                    {
                        cg.fn_destination_layouts.insert(f.name.clone(), id);
                        if matches!(
                            cg.specialized_layouts.get(id).map(|layout| layout.kind()),
                            Some(LayoutKind::PackedRecord { .. })
                        ) && checked_access.is_some_and(|signature| {
                            signature.params().iter().all(|param| {
                                matches!(
                                    param.kind(),
                                    witchy_types::access::AccessKind::OwnedImmutable
                                        | witchy_types::access::AccessKind::SharedBorrow
                                )
                            })
                        })
                            && let Some(fields) = direct_scalar_result_fields(f)
                            && !fields.is_empty()
                            && fields.iter().all(|field| !cg.kind_of(field).is_ref())
                        {
                            cg.scalar_record_producers.insert(
                                f.name.clone(),
                                ScalarRecordProducer {
                                    layout: id,
                                    field_count: fields.len(),
                                },
                            );
                        }
                    }
                }
                // A function returning a closure (`-> fn(...) -> RET`): record the
                // closure's return kind so a `let f = make(...)` then `f(x)` call
                // recovers the result at the right width.
                if let Some(Type::Fn(_, cret, _)) = resolved_ret {
                    cg.fn_ret_closure_kind.insert(f.name.clone(), cg.kind_for_type(cret));
                }
                // A function returning a tuple: record its slot value types so a
                // `let (a, b) = f(...)` destructures each at the right width.
                if let Some(Type::Tuple(slots)) = resolved_ret {
                    cg.fn_ret_tuple_slots
                        .insert(f.name.clone(), slots.iter().map(ty_to_valtype).collect());
                    // Per slot, the element type if the slot is `List(<scalar>)`
                    // (e.g. unzip's `(List(Int), List(Int))`), so a destructure
                    // binds each list var's element type.
                    let elems: Vec<Option<ValType>> = slots
                        .iter()
                        .map(|t| match t {
                            Type::Named(n, a) if n == "List" => a.first().and_then(|e| {
                                match ty_to_valtype(e) {
                                    ValType::Other => None,
                                    vt => Some(vt),
                                }
                            }),
                            _ => None,
                        })
                        .collect();
                    if elems.iter().any(|e| e.is_some()) {
                        cg.fn_ret_tuple_slot_list_elem.insert(f.name.clone(), elems);
                    }
                }
            }
            Item::Type(t) if !is_compiler_syntax_type_name(&t.name) => {
                if t.packed {
                    cg.packed_types.insert(t.name.clone());
                }
                cg.adt_variants
                    .insert(t.name.clone(), t.variants.iter().map(|v| v.fields.clone()).collect());
                cg.adt_variant_names
                    .insert(t.name.clone(), t.variants.iter().map(|v| v.name.clone()).collect());
                for (tag, variant) in t.variants.iter().enumerate() {
                    cg.ctor_type_name.insert(variant.name.clone(), t.name.clone());
                    cg.ctors
                        .insert(variant.name.clone(), (tag as u32, variant.fields.len()));
                    if gc_aggregate_name_set.contains(&t.name)
                        && t.variants.len() == 1
                        && variant.field_names.is_empty()
                    {
                        cg.record_field_types.insert(t.name.clone(), variant.fields.clone());
                    }
                    if !variant.field_names.is_empty() {
                        let fields = variant
                            .field_names
                            .iter()
                            .zip(&variant.fields)
                            .map(|(name, ty)| {
                                let ty_name = match ty {
                                    Type::Named(n, _) => Some(n.clone()),
                                    _ => None,
                                };
                                (name.clone(), ty_name)
                            })
                            .collect();
                        cg.record_fields.insert(t.name.clone(), fields);
                        cg.record_field_types.insert(t.name.clone(), variant.fields.clone());
                        // Effective type parameters, in explicit then inferred order,
                        // so a generic record's `RecInst` maps use-site type arguments
                        // to the correct field type variable even when fields are
                        // declared out of parameter order (BUG-319).
                        cg.record_generics
                            .insert(t.name.clone(), witchy_types::typeck::type_def_params(t));
                    }
                }
            }
            Item::Type(_)
            | Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    // Every nominal and tuple ID is reserved before materializing a field kind,
    // so recursive references can point forward in the single Wasm GC group.
    for (owner_key, plan) in &gc_nominal_plans {
        let Some(id) = cg.gc_aggregate_ids.get(owner_key).copied() else {
            continue;
        };
        let tagged = plan.variants.len() > 1;
        let mut fields = if tagged {
            vec![witchy_wir::wir::Kind::I32]
        } else {
            Vec::new()
        };
        for (tag, (variant_name, field_types)) in
            plan.variant_names.iter().zip(&plan.variants).enumerate()
        {
            let field_base = fields.len() as u32;
            fields.extend(
                field_types
                    .iter()
                    .map(|ty| Codegen::wir_kind(cg.gc_field_storage_kind(ty))),
            );
            cg.gc_ctor_layouts.insert(
                (owner_key.clone(), variant_name.clone()),
                GcCtorLayout {
                    owner_key: owner_key.clone(),
                    tag: tagged.then_some(tag as u32),
                    field_base,
                    field_types: field_types.clone(),
                },
            );
        }
        if let Some(slot) = cg.gc_structs.get_mut(id as usize) {
            slot.fields = fields;
        }
    }
    for (shape, fields) in &gc_tuple_layouts {
        let Some(id) = cg.gc_tuple_ids.get(shape).copied() else {
            continue;
        };
        let field_kinds = fields
            .iter()
            .map(|ty| Codegen::wir_kind(cg.kind_for_type(ty)))
            .collect();
        if let Some(slot) = cg.gc_structs.get_mut(id as usize) {
            slot.fields = field_kinds;
        }
    }
    // A witness payload is one concrete value, not a universal slot. Materialize
    // its field only after nominal/tuple IDs are known so nested GC references
    // keep their typed representation across the erased envelope boundary.
    for witness in &witnesses.witnesses {
        let Some(payload_id) = cg.existential_payload_ids.get(&witness.id).copied() else {
            continue;
        };
        let field = Codegen::wir_kind(cg.kind_for_type(&witness.concrete));
        if let Some(slot) = cg.gc_structs.get_mut(payload_id as usize) {
            slot.fields = vec![field];
        }
    }
    // Function return kinds may have been recorded before the GC-aggregate registry
    // existed. Refresh them now so forward references to cap-carrying aggregates use
    // the `(ref null $s)` ABI at call sites.
    for item in &module.items {
        if let Item::Function(f) = item {
            let ret = f.ret.as_ref().map(|t| cg.kind_for_type(t)).unwrap_or(Kind::I32);
            cg.fn_ret.insert(f.name.clone(), ret);
            if let Some(Type::Fn(_, cret, _)) = &f.ret {
                cg.fn_ret_closure_kind.insert(f.name.clone(), cg.kind_for_type(cret));
            }
        }
    }
    // (RFC-0047) The whole-program set of types with a CUSTOM (non-derived)
    // `PartialEq`. Detected post-lowering (like the interpreter): a declared type
    // whose `PartialEq__T__eq` function exists but which did NOT derive PartialEq.
    // A compound `==` over such an element type calls that impl rather than
    // recursing structurally; everything else keeps the structural fast path.
    {
        let has_eq_fn = |name: &str| {
            let mangled = format!("PartialEq__{name}__eq");
            module
                .items
                .iter()
                .any(|it| matches!(it, Item::Function(f) if f.name == mangled))
        };
        for item in &module.items {
            if let Item::Type(t) = item {
                if is_compiler_syntax_type_name(&t.name) {
                    continue;
                }
                if !t.partial_eq_derived && has_eq_fn(&t.name) {
                    cg.custom_eq_types.insert(t.name.clone());
                }
            }
        }
    }
    // Now that all record types are known, record which constructor fields are
    // records, so binding `Circle(p)` in a pattern lets `p.field` resolve.
    for item in &module.items {
        if let Item::Type(t) = item {
            if is_compiler_syntax_type_name(&t.name) {
                continue;
            }
            for variant in &t.variants {
                let field_recs: Vec<Option<String>> = variant
                    .fields
                    .iter()
                    .map(|ty| match ty {
                        Type::Named(n, _) if cg.record_fields.contains_key(n) => Some(n.clone()),
                        _ => None,
                    })
                    .collect();
                if field_recs.iter().any(|r| r.is_some()) {
                    cg.ctor_field_records.insert(variant.name.clone(), field_recs);
                }
            }
        }
    }
    // Now that record types are known, note which functions return a record, so
    // `let q = f(...)` resolves `q.field`; and which return a Result/Option whose
    // success payload is a record, so `let q = f(...)?` resolves it too.
    for item in &module.items {
        if let Item::Function(f) = item {
            if let Some(Type::Named(n, args)) = &f.ret {
                if cg.record_fields.contains_key(n) {
                    cg.fn_ret_records.insert(f.name.clone(), n.clone());
                } else if n == "List" {
                    // `List(Account)`: `for x in f(...)` binds x to that record.
                    if let Some(Type::Named(elem, _)) = args.first() {
                        if cg.record_fields.contains_key(elem) {
                            cg.fn_ret_list_elem.insert(f.name.clone(), elem.clone());
                        }
                    }
                    // `List(String)` etc.: record the scalar element value type so
                    // `list.at(f(...), i)` is typed (e.g. a String element compares by
                    // content). Skips `Other` (generic / non-scalar elements).
                    if let Some(elem) = args.first() {
                        let evt = ty_to_valtype(elem);
                        if evt != ValType::Other {
                            cg.fn_ret_list_elem_valtype.insert(f.name.clone(), evt);
                        }
                        // `List((T, U))` (e.g. zip): record the element tuple's
                        // slot types so a destructure of `list.at(f(...), i)` is typed.
                        if let Type::Tuple(slots) = elem {
                            cg.fn_ret_list_elem_tuple_slots
                                .insert(f.name.clone(), slots.iter().map(ty_to_valtype).collect());
                        }
                    }
                } else if let Some(payload) = args.first() {
                    // e.g. `Result(Account, _)` / `Option(Account)`: `?` yields it.
                    if let Type::Named(rec, _) = payload {
                        if cg.record_fields.contains_key(rec) {
                            cg.fn_ret_result_record.insert(f.name.clone(), rec.clone());
                        }
                    }
                    // A scalar success payload (e.g. `Option(Int)` from parse_int,
                    // or a user `R(Int, _)`): record it so a `match`/`?` recovers
                    // the Some/Ok value at the right width instead of truncating a
                    // big Int to the generic i32. The success payload is the first
                    // type argument (true for Option/Result and result-like sum
                    // types); only ever consulted at a Some/Ok/`?` site, so a
                    // non-result type's first arg is harmless.
                    let pvt = ty_to_valtype(payload);
                    if pvt != ValType::Other {
                        cg.fn_ret_result_valtype.insert(f.name.clone(), pvt);
                    }
                }
            }
            // Generic shapes over a `List(a)` argument: `-> Option(a)/Result(a,_)`
            // (find/head/min_by) and `-> List(a)` (filter/take/reverse/sort_by).
            // Record which argument carries `a` so a call's payload / element
            // record type resolves from that argument, without full inference.
            if let Some(tv) = payload_type_var(&f.ret) {
                if let Some(k) = list_param_of_var(&f.params, &tv) {
                    cg.fn_ret_option_of_list_arg.insert(f.name.clone(), k);
                }
            }
            if let Some(tv) = list_elem_type_var(&f.ret) {
                if let Some(k) = list_param_of_var(&f.params, &tv) {
                    cg.fn_ret_list_of_list_arg.insert(f.name.clone(), k);
                } else if let Some(k) = fn_param_returning_var(&f.params, &tv) {
                    // `map`: result element type is the mapper's return type.
                    cg.fn_ret_list_of_fn_arg.insert(f.name.clone(), k);
                }
            }
        }
    }
    Ok(())
}

/// Build the generated repair entry for every lowered conventional source
/// function.  The wrapper's parameter and result vectors are cloned from the
/// proven entry verbatim, including `var` write-backs and ownership outputs.
/// It changes only ownership-token inputs to zero, which makes the established
/// copy-on-write path create fresh storage before the opt body mutates it.
///
/// A repair entry is therefore a physical entry strategy, not a second source
/// function or an alternate callable identity.  Direct normal calls select it
/// from the checked `BoundaryEntrySelection`; opt calls continue to target the
/// proven entry and never receive an implicit repair.
fn build_boundary_repair_adapters(
    cg: &Codegen<'_>,
    user_order: &[String],
) -> Vec<witchy_wir::wir::WirFunc> {
    use witchy_wir::wir::{WirExpr as E, WirFunc, WirLocal, WirNode as N};

    user_order
        .iter()
        .filter(|proven_name| cg.boundary_repair_targets.contains(*proven_name))
        .filter_map(|proven_name| {
            let proven = cg.wir_funcs.get(proven_name)?;
            let name = Codegen::boundary_repair_adapter_name(proven_name);
            let result_locals: Vec<WirLocal> = proven
                .ret
                .iter()
                .enumerate()
                .map(|(index, ty)| WirLocal {
                    name: format!("__witchy_repair_result_{index}"),
                    ty: ty.clone(),
                })
                .collect();
            let args = proven
                .params
                .iter()
                .map(|param| {
                    if param.name.ends_with("__cap") {
                        E::ConstI32(0)
                    } else {
                        E::GetLocal(param.name.clone())
                    }
                })
                .collect();
            let mut body = vec![N::CallStoreMulti {
                func: proven_name.clone(),
                args,
                dests: result_locals.iter().map(|local| local.name.clone()).collect(),
            }];
            body.extend(
                result_locals
                    .iter()
                    .map(|local| N::Push(E::GetLocal(local.name.clone()))),
            );
            Some(WirFunc {
                name,
                params: proven.params.clone(),
                ret: proven.ret.clone(),
                locals: result_locals,
                body,
                raw_body: None,
            })
        })
        .collect()
}

/// Materialize the closed witness plan as typed Wasm table functions.
///
/// Each wrapper receives the erased existential envelope, casts only through
/// compiler-reserved GC layouts, extracts the concrete payload, and calls the
/// monomorphized impl adapter. The table index is the backend-neutral dense
/// `(witness_id, static_slot)` contract from `witchy-types::witness`.
fn build_existential_adapter_funcs(
    cg: &Codegen<'_>,
    witnesses: &witchy_types::witness::WitnessPlan,
) -> Result<(Vec<witchy_wir::wir::WirFunc>, Vec<String>), LoweringFailure> {
    use witchy_wir::wir::{WirExpr as E, WirFunc, WirLocal, WirNode as N, WirTy};

    let index = witnesses.dispatch_index().map_err(|message| {
        LoweringFailure::Rejected(CodegenError { message })
    })?;
    let Some(table_len) = index.table_len(witnesses.witnesses.len()) else {
        return unsupported("existential witness table exceeds u32 addressing");
    };
    if table_len == 0 {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut funcs = Vec::new();
    let mut entries = vec![String::new(); usize::try_from(table_len).unwrap_or(0)];
    for witness in &witnesses.witnesses {
        let payload_id = *cg.existential_payload_ids.get(&witness.id).ok_or_else(|| {
            LoweringFailure::Rejected(CodegenError {
                message: format!("missing payload box for existential witness {}", witness.id),
            })
        })?;
        for (slot_index, slot) in witness.slots.iter().enumerate() {
            let access = cg.access_facts.declaration(&slot.adapter).ok_or_else(|| {
                LoweringFailure::Rejected(CodegenError {
                    message: format!(
                        "checked access facts omit existential adapter `{}`",
                        slot.adapter
                    ),
                })
            })?;
            let receiver_access = access.params().first().map(|param| param.kind());
            let receiver_is_var = receiver_access
                == Some(witchy_types::access::AccessKind::ExclusiveWriteback);
            let receiver_is_own =
                receiver_access == Some(witchy_types::access::AccessKind::Consuming);
            let explicit_access = access.params().iter().skip(1).collect::<Vec<_>>();
            if explicit_access.iter().any(|param| {
                !matches!(
                    param.kind(),
                    witchy_types::access::AccessKind::OwnedImmutable
                        | witchy_types::access::AccessKind::ExclusiveWriteback
                )
            }) || (receiver_is_own
                && explicit_access.iter().any(|param| {
                    param.kind()
                        == witchy_types::access::AccessKind::ExclusiveWriteback
                }))
            {
                return unsupported(format!(
                    "RFC-0081 Wasm adapter for `{}`.{} with `own self` and `var` explicit parameters is not lowered yet",
                    slot.owner_trait, slot.method
                ));
            }
            let slot_index = u32::try_from(slot_index).map_err(|_| {
                LoweringFailure::Rejected(CodegenError {
                    message: "existential witness slot exceeds u32".to_string(),
                })
            })?;
            let table_index = index.table_index(witness, slot_index).ok_or_else(|| {
                LoweringFailure::Rejected(CodegenError {
                    message: "witness dispatch plan lost a valid static slot".to_string(),
                })
            })?;
            let name = format!("__dynw{}_{}", witness.id, slot_index);
            let ownership =
                cg.ownership_envelope_for_named_signature(&slot.adapter, access);
            let var_capacity_args: Vec<usize> = ownership
                .var_capacity_params
                .iter()
                .copied()
                .filter_map(|index| index.checked_sub(1))
                .collect();
            let unique_capacity_result = ownership.unique_capacity_result;
            let receiver_capacity = ownership.var_capacity_params.contains(&0);
            let mut params = vec![WirLocal {
                name: "receiver".to_string(),
                ty: WirTy::StructRef,
            }];
            for (argument_index, ty) in slot.params.iter().enumerate() {
                params.push(WirLocal {
                    name: format!("arg{argument_index}"),
                    ty: Codegen::wir_ty_for_kind(cg.kind_for_type(ty)),
                });
            }
            for index in &var_capacity_args {
                params.push(WirLocal {
                    name: format!("arg{index}__cap"),
                    ty: WirTy::Bool,
                });
            }
            let wrapped = E::RefCast {
                struct_id: EXISTENTIAL_WRAPPER_ID,
                value: Box::new(E::GetLocal("receiver".to_string())),
            };
            let erased_payload = E::StructGet {
                struct_id: EXISTENTIAL_WRAPPER_ID,
                field: 0,
                base: Box::new(wrapped),
            };
            let payload = E::StructGet {
                struct_id: payload_id,
                field: 0,
                base: Box::new(E::RefCast {
                    struct_id: payload_id,
                    value: Box::new(erased_payload),
                }),
            };
            let mut args = vec![payload];
            args.extend((0..slot.params.len()).map(|argument_index| {
                E::GetLocal(format!("arg{argument_index}"))
            }));
            if receiver_capacity {
                args.push(E::ConstI32(0));
            }
            args.extend(
                var_capacity_args
                    .iter()
                    .map(|index| E::GetLocal(format!("arg{index}__cap"))),
            );
            let result_ty = Codegen::wir_ty_for_kind(cg.kind_for_type(&slot.result));
            let mut locals = Vec::new();
            let mut body = Vec::new();
            let mut ret = vec![result_ty.clone()];
            let var_args: Vec<usize> = explicit_access
                .iter()
                .enumerate()
                .filter_map(|(index, param)| {
                    (param.kind()
                        == witchy_types::access::AccessKind::ExclusiveWriteback)
                    .then_some(index)
                })
                .collect();
            if !receiver_is_own
                && (receiver_is_var
                    || !var_args.is_empty()
                    || unique_capacity_result)
            {
                let result_local = format!("__dynw{0}_{1}_result", witness.id, slot_index);
                locals.push(WirLocal { name: result_local.clone(), ty: result_ty });
                let mut dests = vec![result_local.clone()];
                let unique_cap_local = unique_capacity_result.then(|| {
                    let local = format!("__dynw{0}_{1}_unique_cap", witness.id, slot_index);
                    locals.push(WirLocal { name: local.clone(), ty: WirTy::Bool });
                    dests.push(local.clone());
                    local
                });
                let payload_local = format!("__dynw{0}_{1}_payload", witness.id, slot_index);
                if receiver_is_var {
                    locals.push(WirLocal {
                        name: payload_local.clone(),
                        ty: Codegen::wir_ty_for_kind(cg.kind_for_type(&witness.concrete)),
                    });
                    dests.push(payload_local.clone());
                }
                let mut var_locals = Vec::new();
                for index in var_args {
                    let local = format!("__dynw{0}_{1}_arg{index}", witness.id, slot_index);
                    let ty = Codegen::wir_ty_for_kind(cg.kind_for_type(&slot.params[index]));
                    locals.push(WirLocal { name: local.clone(), ty: ty.clone() });
                    dests.push(local.clone());
                    var_locals.push((local, ty));
                }
                if receiver_capacity {
                    let local = format!("__dynw{0}_{1}_receiver_cap", witness.id, slot_index);
                    locals.push(WirLocal { name: local.clone(), ty: WirTy::Bool });
                    dests.push(local);
                }
                let mut cap_locals = Vec::new();
                for index in &var_capacity_args {
                    let local = format!("__dynw{0}_{1}_arg{index}_cap", witness.id, slot_index);
                    locals.push(WirLocal { name: local.clone(), ty: WirTy::Bool });
                    dests.push(local.clone());
                    cap_locals.push(local);
                }
                body.push(N::CallStoreMulti {
                    func: slot.adapter.clone(),
                    args,
                    dests,
                });
                body.push(N::Push(E::GetLocal(result_local)));
                if let Some(local) = unique_cap_local {
                    body.push(N::Push(E::GetLocal(local)));
                    ret.push(WirTy::Bool);
                }
                if receiver_is_var {
                    body.push(N::Push(E::StructNew {
                        struct_id: EXISTENTIAL_WRAPPER_ID,
                        args: vec![
                            E::StructNew {
                                struct_id: payload_id,
                                args: vec![E::GetLocal(payload_local)],
                            },
                            E::ConstI32(i32::try_from(witness.id).map_err(|_| {
                                LoweringFailure::Rejected(CodegenError {
                                    message: "existential witness id exceeds i32".to_string(),
                                })
                            })?),
                        ],
                    }));
                    ret.push(WirTy::GcRef(EXISTENTIAL_WRAPPER_ID));
                }
                for (local, ty) in var_locals {
                    body.push(N::Push(E::GetLocal(local)));
                    ret.push(ty);
                }
                for local in cap_locals {
                    body.push(N::Push(E::GetLocal(local)));
                    ret.push(WirTy::Bool);
                }
            } else if receiver_is_own {
                // The public existential ABI consumes just the erased receiver.
                // Its concrete own-ABI token is internal to the adapter: no
                // existential payload is reconstructed after an owning call.
                let has_own_state = cg.summaries.own_abi(&slot.adapter).is_some();
                if has_own_state || unique_capacity_result {
                    let result_local = format!("__dynw{0}_{1}_result", witness.id, slot_index);
                    locals.push(WirLocal { name: result_local.clone(), ty: result_ty });
                    let mut dests = vec![result_local.clone()];
                    let unique_cap_local = unique_capacity_result.then(|| {
                        let local = format!("__dynw{0}_{1}_unique_cap", witness.id, slot_index);
                        locals.push(WirLocal { name: local.clone(), ty: WirTy::Bool });
                        dests.push(local.clone());
                        local
                    });
                    if has_own_state {
                        args.push(E::ConstI32(0));
                        locals.push(WirLocal {
                            name: "__dynw_owncap".to_string(),
                            ty: WirTy::Bool,
                        });
                        dests.push("__dynw_owncap".to_string());
                    }
                    body.push(N::CallStoreMulti {
                        func: slot.adapter.clone(),
                        args,
                        dests,
                    });
                    body.push(N::Push(E::GetLocal(result_local)));
                    if let Some(local) = unique_cap_local {
                        body.push(N::Push(E::GetLocal(local)));
                        ret.push(WirTy::Bool);
                    }
                } else {
                    body.push(N::Push(E::Call { func: slot.adapter.clone(), args }));
                }
            } else {
                body.push(N::Push(E::Call { func: slot.adapter.clone(), args }));
            }
            funcs.push(WirFunc {
                name: name.clone(),
                params,
                ret,
                locals,
                body,
                raw_body: None,
            });
            entries[usize::try_from(table_index).expect("u32 table index fits usize")] = name;
        }
    }
    let Some(fallback) = funcs.first().map(|function| function.name.clone()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    for entry in &mut entries {
        if entry.is_empty() {
            // This cell belongs to a shorter, incompatible existential layout;
            // a well-typed static slot can never select it. It must still hold
            // a funcref so the dense table has stable arithmetic indexing.
            *entry = fallback.clone();
        }
    }
    Ok((funcs, entries))
}

#[derive(Debug)]
enum LoweringFailure {
    Unsupported(UnsupportedLowering),
    Rejected(CodegenError),
}

impl From<CodegenError> for LoweringFailure {
    fn from(error: CodegenError) -> Self {
        Self::Rejected(error)
    }
}

fn unsupported<T>(message: impl Into<String>) -> Result<T, LoweringFailure> {
    Err(LoweringFailure::Unsupported(UnsupportedLowering {
        message: message.into(),
    }))
}

fn public_outcome<T>(result: Result<T, LoweringFailure>) -> LoweringOutcome<T> {
    match result {
        Ok(value) => LoweringOutcome::Lowered(value),
        Err(LoweringFailure::Unsupported(reason)) => LoweringOutcome::Unsupported(reason),
        Err(LoweringFailure::Rejected(error)) => LoweringOutcome::Rejected(error),
    }
}

fn encode_validated(
    module: &witchy_wir::wir::WirModule,
    gc_structs: &[witchy_wir::wir::WirStructDef],
    gc_arrays: &[witchy_wir::wir::WirArrayDef],
) -> Result<Vec<u8>, CodegenError> {
    encode_validated_with_source_map(module, gc_structs, gc_arrays).map(|encoded| encoded.wasm)
}

fn encode_validated_with_source_map(
    module: &witchy_wir::wir::WirModule,
    gc_structs: &[witchy_wir::wir::WirStructDef],
    gc_arrays: &[witchy_wir::wir::WirArrayDef],
) -> Result<witchy_wir::wir_encode::EncodedModule, CodegenError> {
    let encoded = witchy_wir::wir_encode::try_encode_with_gc_source_map(
        module,
        gc_structs,
        gc_arrays,
    )
        .map_err(|error| CodegenError {
            message: format!("assembled WIR could not be encoded: {error}"),
        })?;
    wasmparser::validate(&encoded.wasm).map_err(|error| {
        let offset = error.offset();
        let mut body_index = 0_usize;
        let mut function = None;
        for payload in wasmparser::Parser::new(0).parse_all(&encoded.wasm) {
            if let Ok(wasmparser::Payload::CodeSectionEntry(body)) = payload {
                if body.range().contains(&offset) {
                    function = module.funcs.get(body_index).map(|func| func.name.as_str());
                    break;
                }
                body_index += 1;
            }
        }
        CodegenError {
            message: match function {
                Some(function) => format!(
                    "assembled WIR failed wasm validation in `{function}`: {error}"
                ),
                None => format!("assembled WIR failed wasm validation: {error}"),
            },
        }
    })?;
    Ok(encoded)
}

fn validated_module_outcome(
    module: witchy_wir::wir::WirModule,
    gc_structs: &[witchy_wir::wir::WirStructDef],
    gc_arrays: &[witchy_wir::wir::WirArrayDef],
) -> LoweringOutcome<witchy_wir::wir::WirModule> {
    match encode_validated(&module, gc_structs, gc_arrays) {
        Ok(_) => LoweringOutcome::Lowered(module),
        Err(error) => LoweringOutcome::Rejected(error),
    }
}

#[cfg(test)]
fn encoded_binary_outcome(
    module: &witchy_wir::wir::WirModule,
    gc_structs: &[witchy_wir::wir::WirStructDef],
    gc_arrays: &[witchy_wir::wir::WirArrayDef],
) -> LoweringOutcome<Vec<u8>> {
    match encode_validated(module, gc_structs, gc_arrays) {
        Ok(bytes) => LoweringOutcome::Lowered(bytes),
        Err(error) => LoweringOutcome::Rejected(error),
    }
}

fn append_layout_bundle(mut wasm: Vec<u8>, bundle: &witchy_wir::layout::LayoutBundle) -> Vec<u8> {
    const SECTION_NAME: &[u8] = b"witchy.layouts";
    fn push_u32_leb(bytes: &mut Vec<u8>, mut value: u32) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    let data = bundle.canonical_bytes();
    let mut payload = Vec::with_capacity(SECTION_NAME.len() + data.len() + 5);
    push_u32_leb(
        &mut payload,
        u32::try_from(SECTION_NAME.len()).expect("layout section name fits u32"),
    );
    payload.extend_from_slice(SECTION_NAME);
    payload.extend_from_slice(&data);
    wasm.push(0);
    push_u32_leb(
        &mut wasm,
        u32::try_from(payload.len()).expect("validated layout bundle payload fits u32"),
    );
    wasm.extend_from_slice(&payload);
    wasm
}

fn encoded_module_outcome(
    module: &witchy_wir::wir::WirModule,
    gc_structs: &[witchy_wir::wir::WirStructDef],
    gc_arrays: &[witchy_wir::wir::WirArrayDef],
) -> LoweringOutcome<witchy_wir::wir_encode::EncodedModule> {
    match encode_validated_with_source_map(module, gc_structs, gc_arrays) {
        Ok(encoded) => LoweringOutcome::Lowered(encoded),
        Err(error) => LoweringOutcome::Rejected(error),
    }
}

#[cfg(any(test, feature = "raw-module-test-api"))]
fn assemble_optimized_wir_with_structs(
    module: &Module,
) -> Result<
    (
        witchy_wir::wir::WirModule,
        Vec<witchy_wir::wir::WirStructDef>,
        Vec<witchy_wir::wir::WirArrayDef>,
        witchy_wir::layout::LayoutBundle,
    ),
    LoweringFailure,
> {
    assemble_optimized_wir_with_structs_mode(module, false, None, None, false)
}

fn assemble_optimized_wir_with_structs_mode(
    module: &Module,
    build_entrypoint: bool,
    runtime_catalog: Option<&witchy_types::runtime_type::RuntimeDeclarationCatalog>,
    glamour_development: Option<&GlamourDevelopmentMetadata>,
    collect_source_map: bool,
) -> Result<
    (
        witchy_wir::wir::WirModule,
        Vec<witchy_wir::wir::WirStructDef>,
        Vec<witchy_wir::wir::WirArrayDef>,
        witchy_wir::layout::LayoutBundle,
    ),
    LoweringFailure,
> {
    let (mut wir_module, gc_structs, gc_arrays, layouts) =
        assemble_wir_module_with_structs_mode(
            module,
            build_entrypoint,
            runtime_catalog,
            glamour_development,
            collect_source_map,
        )?;
    witchy_wir::wir_opt::lower_direct_tail_calls(&mut wir_module);
    witchy_wir::wir_opt::optimize(&mut wir_module);
    Ok((wir_module, gc_structs, gc_arrays, layouts))
}

/// Compile a module straight to a wasm **binary** via WIR + `wir_encode::encode`.
/// The result distinguishes a valid source construct that lacks a compiled
/// lowering from rejected input or malformed compiler output.
#[cfg(any(test, feature = "raw-module-test-api"))]
pub fn compile_module_binary(module: &Module) -> LoweringOutcome<Vec<u8>> {
    compile_module_binary_mode(module, false, None, None)
}

/// Compile a module that has crossed the canonical linked type-check boundary.
///
/// Production front ends should use this entrypoint. The raw-module variant is
/// retained for lowerer unit tests and compiler-synthesized modules whose
/// construction is intentionally below the source pipeline.
pub fn compile_checked_module_binary(
    checked: &witchy_types::pipeline::CheckedModule,
) -> LoweringOutcome<Vec<u8>> {
    let runtime_catalog = checked.runtime_declaration_catalog().ok();
    compile_module_binary_mode(checked.module(), false, runtime_catalog.as_ref(), None)
}

/// Compile one compiler-synthesized Glamour island adapter against the exact
/// linked and authenticated application module that selected it.
pub fn compile_checked_glamour_island_binary(
    checked: &witchy_types::pipeline::CheckedModule,
    generated: &Module,
) -> LoweringOutcome<Vec<u8>> {
    compile_checked_glamour_island_execution_binary(checked, checked.module(), generated)
}

/// Compile an authenticated compiler-rewritten application clone plus its
/// generated island adapter. The original CheckedModule remains the provenance
/// and runtime-declaration authority for every rewritten call.
pub fn compile_checked_glamour_island_execution_binary(
    checked: &witchy_types::pipeline::CheckedModule,
    application: &Module,
    generated: &Module,
) -> LoweringOutcome<Vec<u8>> {
    if !generated.imports.is_empty()
        || !generated.from_imports.is_empty()
        || generated.linked_entry.is_some()
        || !generated.compiler_item_syntax.is_empty()
        || !generated.compiler_expr_syntax.is_empty()
        || !generated.compiler_type_syntax.is_empty()
        || !generated.compiler_pattern_syntax.is_empty()
        || !generated.compiler_stmt_syntax.is_empty()
        || !generated.compiler_block_syntax.is_empty()
    {
        return LoweringOutcome::Rejected(CodegenError {
            message: "compiler-generated Glamour island adapter contains non-item module state"
                .into(),
        });
    }
    let mut module = application.clone();
    module.items.extend(generated.items.clone());
    module.item_lines.clear();
    let runtime_catalog = checked.runtime_declaration_catalog().ok();
    compile_module_binary_mode(&module, false, runtime_catalog.as_ref(), None)
}

pub fn compile_checked_development_module(
    checked: &witchy_types::pipeline::CheckedModule,
) -> LoweringOutcome<CompiledDevelopmentModule> {
    let metadata = match checked_glamour_development_metadata(checked) {
        Ok(metadata) => metadata,
        Err(error) => return LoweringOutcome::Rejected(error),
    };
    let generated_module = match metadata
        .as_ref()
        .and_then(|metadata| metadata.snapshot_codec.as_ref())
    {
        Some(codec) => match checked_glamour_development_codec_module(checked, codec) {
            Ok(module) => Some(module),
            Err(error) => return LoweringOutcome::Rejected(error),
        },
        None => None,
    };
    let runtime_catalog = checked.runtime_declaration_catalog().ok();
    match compile_module_binary_with_source_map_mode(
        generated_module.as_ref().unwrap_or_else(|| checked.module()),
        false,
        runtime_catalog.as_ref(),
        metadata.as_ref(),
        true,
    ) {
        LoweringOutcome::Lowered(encoded) => {
            LoweringOutcome::Lowered(CompiledDevelopmentModule {
                wasm: encoded.wasm,
                glamour: metadata,
                source_instructions: encoded.source_instructions,
                source_expressions: encoded.source_expressions,
            })
        }
        LoweringOutcome::Unsupported(reason) => LoweringOutcome::Unsupported(reason),
        LoweringOutcome::Rejected(error) => LoweringOutcome::Rejected(error),
    }
}

fn compile_module_binary_mode(
    module: &Module,
    build_entrypoint: bool,
    runtime_catalog: Option<&witchy_types::runtime_type::RuntimeDeclarationCatalog>,
    glamour_development: Option<&GlamourDevelopmentMetadata>,
) -> LoweringOutcome<Vec<u8>> {
    match compile_module_binary_with_source_map_mode(
        module,
        build_entrypoint,
        runtime_catalog,
        glamour_development,
        false,
    ) {
        LoweringOutcome::Lowered(encoded) => LoweringOutcome::Lowered(encoded.wasm),
        LoweringOutcome::Unsupported(reason) => LoweringOutcome::Unsupported(reason),
        LoweringOutcome::Rejected(error) => LoweringOutcome::Rejected(error),
    }
}

fn compile_module_binary_with_source_map_mode(
    module: &Module,
    build_entrypoint: bool,
    runtime_catalog: Option<&witchy_types::runtime_type::RuntimeDeclarationCatalog>,
    glamour_development: Option<&GlamourDevelopmentMetadata>,
    collect_source_map: bool,
) -> LoweringOutcome<witchy_wir::wir_encode::EncodedModule> {
    let (wir_module, gc_structs, gc_arrays, layouts) =
        match assemble_optimized_wir_with_structs_mode(
            module,
            build_entrypoint,
            runtime_catalog,
            glamour_development,
            collect_source_map,
        ) {
            Ok(assembled) => assembled,
            Err(failure) => return public_outcome(Err(failure)),
        };
    match encoded_module_outcome(&wir_module, &gc_structs, &gc_arrays) {
        LoweringOutcome::Lowered(mut encoded) => {
            encoded.wasm = append_layout_bundle(encoded.wasm, &layouts);
            match wasmparser::validate(&encoded.wasm) {
                Ok(_) => LoweringOutcome::Lowered(encoded),
                Err(error) => LoweringOutcome::Rejected(CodegenError {
                    message: format!("layout-annotated Wasm failed validation: {error}"),
                }),
            }
        }
        LoweringOutcome::Unsupported(reason) => LoweringOutcome::Unsupported(reason),
        LoweringOutcome::Rejected(error) => LoweringOutcome::Rejected(error),
    }
}

/// Assemble the complete pre-optimization `WirModule` for a program — the static
/// prelude raw-body helpers + the lowered user functions + the `run` export +
/// imports/globals/data/table. Split out from `compile_module_binary` so tests
/// can compare optimized and unoptimized encoding.
#[cfg(any(test, feature = "raw-module-test-api"))]
pub fn assemble_wir_module(module: &Module) -> LoweringOutcome<witchy_wir::wir::WirModule> {
    match assemble_wir_module_with_structs(module) {
        Ok((module, gc_structs, gc_arrays, _)) => {
            validated_module_outcome(module, &gc_structs, &gc_arrays)
        }
        Err(failure) => public_outcome(Err(failure)),
    }
}

/// Assemble and optimize the exact WIR module used by the binary backend, then
/// validate the transformed result before exposing it to diagnostic consumers
/// such as `emit-wat`.
pub fn assemble_checked_optimized_wir_module(
    checked: &witchy_types::pipeline::CheckedModule,
) -> LoweringOutcome<witchy_wir::wir::WirModule> {
    let runtime_catalog = checked.runtime_declaration_catalog().ok();
    match assemble_optimized_wir_with_structs_mode(
        checked.module(),
        false,
        runtime_catalog.as_ref(),
        None,
        false,
    ) {
        Ok((module, gc_structs, gc_arrays, _)) => {
            validated_module_outcome(module, &gc_structs, &gc_arrays)
        }
        Err(failure) => public_outcome(Err(failure)),
    }
}

#[cfg(any(test, feature = "raw-module-test-api"))]
pub fn assemble_optimized_wir_module(
    module: &Module,
) -> LoweringOutcome<witchy_wir::wir::WirModule> {
    match assemble_optimized_wir_with_structs(module) {
        Ok((module, gc_structs, gc_arrays, _)) => {
            validated_module_outcome(module, &gc_structs, &gc_arrays)
        }
        Err(failure) => public_outcome(Err(failure)),
    }
}

#[cfg(any(test, feature = "raw-module-test-api"))]
fn assemble_wir_module_with_structs(
    module: &Module,
) -> Result<
    (
        witchy_wir::wir::WirModule,
        Vec<witchy_wir::wir::WirStructDef>,
        Vec<witchy_wir::wir::WirArrayDef>,
        witchy_wir::layout::LayoutBundle,
    ),
    LoweringFailure,
> {
    assemble_wir_module_with_structs_mode(module, false, None, None, false)
}

fn assemble_wir_module_with_structs_mode(
    module: &Module,
    build_entrypoint: bool,
    runtime_catalog: Option<&witchy_types::runtime_type::RuntimeDeclarationCatalog>,
    glamour_development: Option<&GlamourDevelopmentMetadata>,
    collect_source_map: bool,
) -> Result<
    (
        witchy_wir::wir::WirModule,
        Vec<witchy_wir::wir::WirStructDef>,
        Vec<witchy_wir::wir::WirArrayDef>,
        witchy_wir::layout::LayoutBundle,
    ),
    LoweringFailure,
> {
    use witchy_wir::wir::{
        DataSegment, GlobalInit, Kind as WK, WirExpr, WirFunc, WirGlobal, WirImport, WirModule,
        WirNode, WirTable,
    };
    use witchy_wir::wir_prelude::WasmTy;
    // Front-end, identical to `compile_module_with`.
    let runtime_module = strip_compiler_syntax_items_for_runtime(module.clone());
    let checked = witchy_syntax::source_check::check(runtime_module)
        .map_err(|error| CodegenError { message: error.message })?;
    let checked = witchy_syntax::generators::lower(checked)
        .map_err(|message| CodegenError { message })?;
    let checked = witchy_syntax::async_lower::lower(checked)
        .map_err(|message| CodegenError { message })?;
    let recs = witchy_syntax::records::lower(checked)
        .map_err(|message| CodegenError { message })?
        .into_module();
    let eq_types = eq_impl_types(&recs);
    let witness_catalog = witchy_types::witness::WitnessCatalog::from_module(&recs);
    let (mut lowered, generic_specializations) =
        witchy_types::traits::lower_for_wasm_with_specializations(recs).into_parts();
    witchy_syntax::parser::lower_sugar_module(&mut lowered);
    alpha_rename_module(&mut lowered);
    let mut typed = if build_entrypoint {
        witchy_types::typeck::annotate_checked_build(lowered)
    } else {
        witchy_types::typeck::annotate_checked(lowered)
    }
        .map_err(|error| CodegenError { message: error.to_string() })?;
    // `e ? "msg"` desugar (`__try_ctx`) is type-directed: an `Option` operand lowers
    // via `option.ok_or`, a `Result` via `result.map_err`. Rewrite it here — after
    // annotation (so the operand's type is known) and before the string-`+` flip +
    // lowering (so the synthesized `map_err` lambda's `+` flips to `Concat` and its
    // nodes get typed). Re-annotate so the freshly minted calls/lambda are in the
    // type table.
    typed = typed.rewrite_and_reannotate(|table, module| {
        rewrite_try_ctx_module(module, table);
    });
    let prepared = match (build_entrypoint, runtime_catalog) {
        (true, Some(runtime_catalog)) => {
            witchy_types::existential::lower_explicit_packs_with_runtime_types_for_build(
                typed,
                &witness_catalog,
                runtime_catalog,
            )
        }
        (true, None) => {
            witchy_types::existential::lower_explicit_packs_for_build(typed, &witness_catalog)
        }
        (false, Some(runtime_catalog)) => {
            witchy_types::existential::lower_explicit_packs_with_runtime_types(
                typed,
                &witness_catalog,
                runtime_catalog,
            )
        }
        (false, None) => {
            witchy_types::existential::lower_explicit_packs(typed, &witness_catalog)
        }
    }
    .map_err(|message| CodegenError { message })?;
    let (mut module, type_table, witnesses) = prepared.into_parts();
    // This field-only rewrite deliberately happens after the typed owner has
    // been consumed. No public API can use unrestricted `&mut Module` access
    // while claiming that the address-keyed type proof remains valid.
    flip_string_add_module(&mut module, &type_table);
    let loan_facts = witchy_types::loans::facts_with_types(&module, &type_table)
        .map_err(|error| CodegenError { message: error.to_string() })?;
    let access_facts = witchy_types::access::checked_facts(&module, &type_table)
        .map_err(|error| CodegenError { message: error.to_string() })?;
    let glamour_exports = glamour_export_family(&module)?;
    // Witness adapters are ordinary monomorphized impl methods, but their only
    // callers are generated after source reachability has run. Seed them before
    // the transitive walk so their own callees are emitted too.
    let mut extra_roots = custom_eq_function_roots(&module);
    for witness in &witnesses.witnesses {
        for slot in &witness.slots {
            extra_roots.push(slot.adapter.clone());
        }
    }
    if let Some(family) = &glamour_exports {
        extra_roots.extend([
            family.init.clone(),
            family.dispatch.clone(),
            family.emit.clone(),
            family.release.clone(),
        ]);
    }
    if let Some(codec) = glamour_development.and_then(|metadata| metadata.snapshot_codec.as_ref()) {
        extra_roots.extend([codec.encoder.clone(), codec.decoder.clone()]);
        extra_roots.extend(codec.migrations.iter().map(|migration| migration.decoder.clone()));
    }
    let reachable = reachable_functions_with(&module, &extra_roots);
    let mut cg = Codegen::new(&module, &type_table, loan_facts, access_facts);
    cg.collect_wir = true;
    register_module_items(
        &mut cg,
        &module,
        &reachable,
        &witnesses,
        &generic_specializations,
    )?;
    cg.collect_source_map = collect_source_map;
    cg.eq_types = eq_types;
    cg.summaries = analysis::Summaries::of_module(&module);
    // (RFC-0110 step 5) Compute the normal-mode one-copy repair sites on the
    // exact `&module` codegen lowers, so the returned call-node pointers are live
    // for the counter check in `lower_var_call`/the owned-call arm. Keyed by the
    // checked access graph (`cg.access_facts`), hence lever-independent.
    cg.boundary_entry_selection =
        analysis::boundary_entry_selection(&module, Some(&cg.access_facts));

    // (RFC-0047) A custom-`PartialEq` type's `PartialEq__T__eq` may be called only
    // from a codegen-synthesized container eq helper (invisible to the AST walk), so
    // seed those impls as reachability roots — otherwise a `[CI] == [CI]` helper
    // calls an un-emitted function and the whole module reports `Unsupported`.
    // The exact `$name` functions this module emits — the discriminator
    // `lower_expr`'s call arm uses to tell a user call from an intrinsic/native.
    cg.emitted_funcs = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f)
                if reachable.contains(&f.name) && !witchy_types::typeck::intrinsic(&f.name) =>
            {
                Some(f.name.clone())
            }
            _ => None,
        })
        .collect();
    let mut has_main = false;
    let mut main_params = 0usize;
    let mut main_param_is_args: Vec<bool> = Vec::new();
    let mut main_param_is_dir: Vec<bool> = Vec::new();
    let mut main_param_is_file: Vec<bool> = Vec::new();
    let mut main_param_is_env: Vec<bool> = Vec::new();
    let mut main_param_is_exec: Vec<bool> = Vec::new();
    let mut main_param_is_net: Vec<bool> = Vec::new();
    let mut main_param_is_fetch: Vec<bool> = Vec::new();
    let mut main_param_is_secret: Vec<bool> = Vec::new();
    // RFC-0038: `Some((type_name, nfields))` for a grantable-capability `main` param
    // (its record is minted at the root); `None` otherwise.
    let mut main_param_user_cap: Vec<Option<(String, usize)>> = Vec::new();
    // Grantable capability name -> field count, to detect + size a grantable param.
    let grantable_caps: HashMap<&str, usize> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => {
                Some((t.name.as_str(), t.variants.first().map(|v| v.fields.len()).unwrap_or(0)))
            }
            _ => None,
        })
        .collect();
    let mut main_returns_int = false;
    let mut main_returns_float = false;
    let mut user_order: Vec<String> = Vec::new();
    // The JS-callable string exports (`pub fn f(String) -> String`); each gets an
    // `__export_f` wrapper and is an extra reachability root (above).
    let string_exports = string_export_functions(&module);
    // (RFC-0040) Cap-gated exports (`export_*(cap, String)`): (export name, cap type,
    // field count). Their `__export_*` wrapper mints the grantable cap host-side, so
    // register the record allocator arity now (while `cg` is mutable).
    let export_cap_info: Vec<(String, String, usize)> = string_exports
        .iter()
        .filter_map(|name| {
            let f = module.items.iter().find_map(|it| match it {
                Item::Function(fu) if &fu.name == name => Some(fu),
                _ => None,
            })?;
            export_cap_of(f, &module).map(|(c, n)| (name.clone(), c.to_string(), n))
        })
        .collect();
    for (_, _, nfields) in &export_cap_info {
        cg.mk_arities.insert(*nfields);
    }
    if let Some(family) = &glamour_exports {
        cg.mk_arities.insert(family.cap_fields);
        if family.state_constructor.is_some() {
            cg.mk_arities.insert(family.state_fields.len());
        }
    }
    for item in &module.items {
        if let Item::Function(f) = item {
            if f.name == "main" {
                has_main = true;
                main_params = f.params.len();
                main_returns_int = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Int");
                main_returns_float = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Float");
                for p in &f.params {
                    let is_args = matches!(&p.ty, Some(t) if witchy_types::typeck::is_args_type(t));
                    if is_args {
                        cg.uses_args = true;
                    }
                    main_param_is_args.push(is_args);
                    main_param_is_dir
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Dir"));
                    main_param_is_file
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "File"));
                    main_param_is_env
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Env"));
                    main_param_is_exec
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Exec"));
                    main_param_is_net
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Net"));
                    main_param_is_fetch
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Fetch"));
                    main_param_is_secret
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Secret"));
                    let uc = match &p.ty {
                        Some(Type::Named(n, _)) => {
                            grantable_caps.get(n.as_str()).map(|nf| (n.clone(), *nf))
                        }
                        _ => None,
                    };
                    if let Some((_, nfields)) = &uc {
                        cg.mk_arities.insert(*nfields); // the record allocator for the sealed cap
                    }
                    main_param_user_cap.push(uc);
                }
            }
            if reachable.contains(&f.name) && !witchy_types::typeck::intrinsic(&f.name) {
                // Compiled for its side effects: stashes a `WirFunc` in
                // `cg.wir_funcs` iff the whole body lowered, and sets the
                // `uses_*` import-gating flags.
                let instances = cg.generic_callable_instances.for_function(&f.name);
                if instances.is_empty() {
                    cg.compile_function(f)?;
                    user_order.push(f.name.clone());
                } else {
                    if cg
                        .generic_callable_instances
                        .needs_logical_fallback(&f.name)
                    {
                        cg.compile_function(f)?;
                        user_order.push(f.name.clone());
                    }
                    for instance in instances {
                        cg.compile_function_as(
                            f,
                            &instance.emitted_name,
                            Some(&instance.key),
                        )?;
                        user_order.push(instance.emitted_name);
                    }
                }
            }
        }
    }
    // A module needs an entry: either a `main` (the `run` export) or at least one
    // string export (a `__export_*` host entry). A library with neither has nothing
    // to instantiate against.
    if !has_main && string_exports.is_empty() && glamour_exports.is_none() {
        if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL no-main"); }
        return Err(LoweringFailure::Rejected(CodegenError {
            message: "module has neither a `main` entrypoint nor a string export, and has no RFC-0108 application export family".into(),
        }));
    }

    // Every reachable function must have fully lowered to WIR.
    if !user_order.iter().all(|n| cg.wir_funcs.contains_key(n)) {
        // Migration aid: `WIRDIAG=1` names the function(s) that didn't lower, so the
        // remaining WAT-fallback surface can be bisected. Inert otherwise.
        if std::env::var_os("WIRDIAG").is_some() {
            let missing: Vec<&String> =
                user_order.iter().filter(|n| !cg.wir_funcs.contains_key(*n)).collect();
            eprintln!("WIRBAIL user-fn-incomplete: {missing:?}");
        }
        let mut missing: Vec<&String> =
            user_order.iter().filter(|n| !cg.wir_funcs.contains_key(*n)).collect();
        missing.sort();
        return unsupported(format!(
            "reachable functions do not fully lower to WIR: {missing:?}"
        ));
    }
    let boundary_repair_adapters = build_boundary_repair_adapters(&cg, &user_order);
    let glamour_state_rcopy_helper = if let Some(family) = &glamour_exports {
        let scalar_only = family.state_constructor.is_some()
            && family.state_fields.iter().all(|field| {
                matches!(
                    field.unqualified(),
                    Type::Named(name, arguments)
                        if arguments.is_empty()
                            && matches!(name.as_str(), "Int" | "Duration" | "Float" | "Bool")
                )
            });
        if scalar_only {
            None
        } else {
            let state_name = match family.state_type.unqualified() {
                Type::Named(name, _) => name.as_str(),
                _ => "<state>",
            };
            let Some(shape) = cg.eq_shape_of_type(&family.state_type) else {
                return unsupported(format!(
                    "RFC-0108 application state `{state_name}` has no bounded deep-copy shape"
                ));
            };
            let Some(helper) = cg.ensure_rcopy_wir_helper(&shape) else {
                return unsupported(format!(
                    "RFC-0108 application state `{state_name}` cannot be copied into the stable arena"
                ));
            };
            cg.uses_region = true;
            Some(helper)
        }
    } else {
        None
    };
    let glamour_state_field_shapes = if glamour_development.is_some_and(|metadata| {
        metadata
            .state_fields
            .contains(&GlamourDevelopmentField::Aggregate)
    })
    {
        let family = glamour_exports
            .as_ref()
            .expect("Glamour development metadata requires an export family");
        let mut shapes = Vec::with_capacity(family.state_fields.len());
        for field in &family.state_fields {
            let Some(shape) = cg.eq_shape_of_type(field) else {
                return unsupported(
                    "Glamour aggregate development tracing requires bounded equality shapes",
                );
            };
            if let Some(custom) = cg.custom_eq_type_of_shape(&shape) {
                let function = format!("PartialEq__{custom}__eq");
                if !cg.wir_funcs.contains_key(&function) {
                    return unsupported(
                        "Glamour aggregate development tracing cannot introduce an unreachable custom equality method",
                    );
                }
            }
            if shape.is_compound()
                && cg.custom_eq_type_of_shape(&shape).is_none()
                && cg.ensure_eq_wir_helper(&shape).is_none()
            {
                return unsupported(
                    "Glamour aggregate development tracing could not build a field equality comparison",
                );
            }
            shapes.push(shape);
        }
        let mut helper_calls = HashSet::new();
        for helper in cg.eq_wir_helpers.values() {
            collect_called_funcs(&helper.body, &mut helper_calls);
        }
        if helper_calls.iter().any(|function| {
            function.starts_with("PartialEq__") && !cg.wir_funcs.contains_key(function)
        }) {
            return unsupported(
                "Glamour aggregate development tracing cannot introduce an unreachable nested custom equality method",
            );
        }
        Some(shapes)
    } else {
        None
    };
    // Generate the compiler-owned adapter functions only after their direct impl
    // targets have been lowered. They occupy the leading dense table cells;
    // lifted closures are offset by `existential_table_len` when constructed.
    let (existential_adapter_funcs, existential_table_entries) =
        build_existential_adapter_funcs(&cg, &witnesses)?;
    // Bail if the program needs program-specific helpers (not in the prelude) or
    // closure types beyond the reserved band. An Int/Float `main` is fine now —
    // the prelude declares `print_int`/`print_float` and the `run` wrapper prints
    // the result.
    // Structural `==` / generated render are fine when every legacy eq/ts helper
    // has a WIR twin; a shape the WIR generator couldn't build leaves its key
    // without a twin → bail to WAT.
    let eq_all_wir = cg.eq_helpers.keys().all(|k| cg.eq_wir_helpers.contains_key(k));
    let ts_all_wir = cg.ts_helpers.keys().all(|k| cg.ts_wir_helpers.contains_key(k));
    // Lambdas/closures are fine now: each lifted body is in `lambda_wir_funcs` and
    // the closure types are synthesized by the encoder from the `CallIndirect`
    // nodes. A lambda the WIR couldn't lower already bailed its enclosing function
    // at the lower stage (so the user_order check below catches it).
    if !eq_all_wir || !ts_all_wir || !cg.rcopy_helpers.is_empty() {
        if std::env::var_os("WIRDIAG").is_some() {
            eprintln!("WIRBAIL eq_ts_rcopy: eq={eq_all_wir} ts={ts_all_wir} rcopy={}", cg.rcopy_helpers.len());
        }
        return unsupported(format!(
            "program-specific helpers lack WIR lowering: equality={eq_all_wir}, \
             rendering={ts_all_wir}, region-copy={}",
            cg.rcopy_helpers.len()
        ));
    }
    let prelude = witchy_wir::wir_prelude::prelude();

    let wasmty_kind = |t: WasmTy| -> WK {
        match t {
            WasmTy::I32 => WK::I32,
            WasmTy::I64 => WK::I64,
            WasmTy::F64 | WasmTy::F32 => WK::F64,
            // (RFC-0005) A migrated capability import (mint_dir, dir_*,
            // mint_file, file_*, mint_net, net_*, mint_secret, crypto/secretstore)
            // takes/returns an unforgeable `externref`.
            WasmTy::ExternRef => WK::ExternRef,
        }
    };

    // --- Capability-minimal WIR-helper path (#35) -------------------------------
    // If every prelude helper the program reaches has a WIR-native form (the
    // `wir_helper` registry), build a PRUNED module that declares only those
    // helpers and imports only their authority — instead of splicing the full
    // "all features on" raw-body prelude (which would over-import and break the
    // capability model). Falls through to the raw-body path otherwise.
    {
        let helper_names: HashSet<&str> =
            prelude.funcs.iter().map(|f| f.name.as_str()).collect();
        let mut called = HashSet::new();
        let mut user_host_imports = HashSet::new();
        let mut uses_table = false;
        for name in &user_order {
            if let Some(wf) = cg.wir_funcs.get(name) {
                uses_table |= collect_called_funcs(&wf.body, &mut called);
                collect_called_host_imports(&wf.body, &mut user_host_imports);
            }
        }
        for function in cg.layout_wir_funcs.values() {
            uses_table |= collect_called_funcs(&function.body, &mut called);
            collect_called_host_imports(&function.body, &mut user_host_imports);
        }
        let custom_key_eq = cg.dict_key_eq_wir_helper();
        // The generated structural-eq / render helpers (included below) call
        // prelude helpers themselves — a Str field eq via `$str_eq`, a renderer via
        // `$concat`/`$int_to_string`. Pull those (and nested eq_*/ts_* calls) into
        // the reached set so the resolution loop declares them.
        for f in cg.eq_wir_helpers.values() {
            uses_table |= collect_called_funcs(&f.body, &mut called);
        }
        if let Some(f) = &custom_key_eq {
            uses_table |= collect_called_funcs(&f.body, &mut called);
        }
        for f in cg.ts_wir_helpers.values() {
            uses_table |= collect_called_funcs(&f.body, &mut called);
        }
        // Generated rcopy helpers call `$ensure`, `$rcopy_str`, and each other.
        // Only when a region actually reclaimed (so the `$rcopy_*` globals are
        // declared); a helper generated for a region that then fell back to a plain
        // block is an orphan and must not enter the module.
        if cg.uses_region {
            for f in cg.rcopy_wir_helpers.values() {
                uses_table |= collect_called_funcs(&f.body, &mut called);
            }
        }
        // Lifted lambda bodies call `$mkN`/`$ensure`/prelude helpers and each
        // other; pull their reached helpers into the resolution set.
        for f in &cg.lambda_wir_funcs {
            uses_table |= collect_called_funcs(&f.body, &mut called);
        }
        for f in &existential_adapter_funcs {
            uses_table |= collect_called_funcs(&f.body, &mut called);
        }
        // (RFC-0045) `__witchy_abort` is authority-free and always linked (like the
        // checked-heap `heap_register`/`heap_frontier`), so a direct `fail(msg)` in
        // user code calling it does NOT disqualify the capability-minimal pruned
        // path. Pull it out of the direct-host set before the gate, but remember to
        // declare the import below.
        let user_calls_abort = user_host_imports.remove("__witchy_abort");
        let layout_uses_heap_register = user_host_imports.remove("heap_register");
        // A direct host call in user code (e.g. `now`, `dir.subdir`, `recv_*`)
        // needs authority the capability-minimal helper registry can't account
        // for — report `Unsupported` for such programs. (Host access that goes
        // THROUGH a migrated helper is fine; its imports come from import_deps.)
        let no_direct_host =
            !called.iter().any(|n| n.starts_with("host:")) && user_host_imports.is_empty();
        if cg.uses_args {
            called.insert("build_args".to_string());
        }
        // RFC-0038: the `run` wrapper mints each grantable-cap param via
        // `mk{N}(build_user_cap_field(k, 0..N))`; those synthesized calls are in no
        // user body, so pull the helpers into the reached set explicitly.
        for (_, nfields) in main_param_user_cap.iter().flatten() {
            called.insert("build_user_cap_field".to_string());
            called.insert(format!("mk{nfields}"));
        }
        // (RFC-0040) cap-gated exports mint their grantable cap in the __export wrapper.
        for (_, _, nfields) in &export_cap_info {
            called.insert("build_user_cap_field".to_string());
            called.insert(format!("mk{nfields}"));
        }
        if let Some(family) = &glamour_exports {
            called.insert("build_user_cap_field".to_string());
            called.insert(format!("mk{}", family.cap_fields));
            if family.state_constructor.is_some() {
                called.insert(format!("mk{}", family.state_fields.len()));
            }
            if glamour_state_field_shapes
                .as_ref()
                .is_some_and(|shapes| {
                    shapes
                        .iter()
                        .any(|shape| matches!(shape, EqShape::Str | EqShape::Bytes))
                })
            {
                called.insert("str_eq".to_string());
            }
        }
        // The `__galloc` allocator the string-export wrappers expose delegates to
        // `$bump_alloc` (RFC-0051 I2 — the single ensure-prefixed allocator), so pull
        // it into the reached set (it brings `ensure` + the `$heap` global via its
        // registry deps). Harmless if a string-export body already reaches it.
        if !string_exports.is_empty() || glamour_exports.is_some() {
            called.insert("bump_alloc".to_string());
        }
        // Resolve every reached helper through the registry (transitively).
        let mut resolved: std::collections::BTreeMap<String, witchy_wir::wir_helpers::WirHelperSpec> =
            std::collections::BTreeMap::new();
        let mut all_registered = true;
        // A called name is a prelude helper to pull in if the static prelude
        // declares it OR the WIR registry resolves it — the latter covers helpers
        // migrated to WIR that have no static-prelude body (e.g. crypto_sha512).
        let mut queue: Vec<String> = called
            .iter()
            .filter(|n| helper_names.contains(n.as_str()) || witchy_wir::wir_helpers::wir_helper(n).is_some())
            .cloned()
            .collect();
        while let Some(h) = queue.pop() {
            if resolved.contains_key(&h) {
                continue;
            }
            match witchy_wir::wir_helpers::wir_helper(&h) {
                Some(spec) => {
                    for d in spec.helper_deps {
                        queue.push((*d).to_string());
                    }
                    resolved.insert(h, spec);
                }
                None => {
                    all_registered = false;
                    if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL unregistered-helper: {h}"); }
                    break;
                }
            }
        }
        if std::env::var_os("WIRDIAG").is_some() && !(no_direct_host && all_registered) {
            let hosts: Vec<&String> = called.iter().filter(|n| n.starts_with("host:")).collect();
            eprintln!("WIRBAIL prune-fail: no_direct_host={no_direct_host} all_registered={all_registered} user_host={user_host_imports:?} hosts={hosts:?}");
        }
        if no_direct_host && all_registered {
            // Host-backed helpers receive the caller's packed source site as a
            // final i64 argument. They publish it only immediately before the
            // host edge, so successful nested calls cannot stale the location.
            for (name, spec) in &mut resolved {
                prepare_diagnostic_helper(name, &mut spec.func);
            }
            let mut import_names: std::collections::BTreeSet<&str> =
                std::collections::BTreeSet::new();
            let mut uses_heap = false;
            for spec in resolved.values() {
                for i in spec.import_deps {
                    import_names.insert(i);
                }
                uses_heap |= spec.uses_heap;
                uses_table |= spec.uses_table;
            }
            // A watermarked loop in user code reads/writes `$heap` even when no
            // reached helper allocates, so the global must still be declared.
            uses_heap |= cg.uses_wm;
            uses_heap |= glamour_exports.is_some();
            // An Int/Float-returning `main` prints its result in the `run`
            // wrapper, so the corresponding host import must be declared.
            if main_returns_int {
                import_names.insert("print_int");
            } else if main_returns_float {
                import_names.insert("print_float");
            }
            if main_param_is_file.iter().any(|is_file| *is_file) {
                import_names.insert("mint_file");
            }
            if main_param_is_dir.iter().any(|is_dir| *is_dir) {
                import_names.insert("mint_dir");
            }
            if main_param_is_env.iter().any(|is_env| *is_env) {
                import_names.insert("mint_env");
            }
            if main_param_is_exec.iter().any(|is_exec| *is_exec) {
                import_names.insert("mint_exec");
            }
            if main_param_is_net.iter().any(|is_net| *is_net) {
                import_names.insert("mint_net");
            }
            if main_param_is_fetch.iter().any(|is_fetch| *is_fetch) {
                import_names.insert("mint_fetch");
            }
            if main_param_is_secret.iter().any(|is_secret| *is_secret) {
                import_names.insert("mint_secret");
            }
            // (RFC-0045) A user `fail(msg)` calls `__witchy_abort` directly (its
            // import_deps aren't consulted because it's not a registry helper), so
            // declare the import when user code reaches it.
            if user_calls_abort {
                import_names.insert("__witchy_abort");
            }
            if layout_uses_heap_register {
                import_names.insert("heap_register");
            }
            let pruned_imports: Vec<WirImport> = import_names
                .iter()
                .map(|iname| {
                    let pi = prelude
                        .imports
                        .iter()
                        .find(|p| p.name.as_str() == *iname)
                        .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                            message: format!(
                                "WIR helper references missing prelude import `{iname}`"
                            ),
                        }))?;
                    Ok(WirImport {
                        name: pi.name.clone(),
                        params: pi.params.iter().copied().map(wasmty_kind).collect(),
                        results: pi.results.iter().copied().map(wasmty_kind).collect(),
                    })
                })
                .collect::<Result<_, LoweringFailure>>()?;
            if custom_key_eq.is_some() {
                resolved.remove("key_eq");
            }
            let mut pruned_funcs: Vec<WirFunc> = resolved.into_values().map(|s| s.func).collect();
            pruned_funcs.extend(cg.layout_wir_funcs.values().cloned());
            let mut glamour_development_data = Vec::new();
            if let Some(f) = custom_key_eq {
                pruned_funcs.push(f);
            }
            // The program-specific structural-equality / render helpers reached by
            // user `==` / generated interpolation render.
            for f in cg.eq_wir_helpers.values() {
                pruned_funcs.push(f.clone());
            }
            for f in cg.ts_wir_helpers.values() {
                pruned_funcs.push(f.clone());
            }
            // Generated per-shape region copy-out helpers reached by a pointer
            // `region:` reclaim. Gated on `uses_region` so a helper generated for a
            // region that then fell back to a plain block stays out of the module
            // (it references `$rcopy_*` globals only declared when `uses_region`).
            if cg.uses_region {
                for f in cg.rcopy_wir_helpers.values() {
                    pruned_funcs.push(f.clone());
                }
            }
            // Closed existential witness adapters must precede closures: their
            // dense table indices are part of the RFC-0081 backend contract.
            for f in &existential_adapter_funcs {
                pruned_funcs.push(f.clone());
            }
            // Lifted lambda bodies, in table-index order (so `$__lamw{i}` lands at
            // table slot i, matching the code index baked into each closure object).
            for f in &cg.lambda_wir_funcs {
                pruned_funcs.push(f.clone());
            }
            // Repair entries retain the exact source callable envelope and
            // delegate to the proven body.  They must be emitted before user
            // functions so direct normal call targets are resolved in every
            // backend encoding mode.
            for f in &boundary_repair_adapters {
                pruned_funcs.push(f.clone());
            }
            for name in &user_order {
                let function = cg.wir_funcs.get(name).ok_or_else(|| {
                    LoweringFailure::Rejected(CodegenError {
                        message: format!("lowered WIR function `{name}` disappeared during assembly"),
                    })
                })?;
                pruned_funcs.push(function.clone());
            }
            // Each `Dir` param is minted from a distinct root grant in declaration
            // order as an unforgeable externref (RFC-0005 Stage 3).
            // Each `File` param is minted from the corresponding direct `--file`
            // grant as an unforgeable externref (RFC-0005 Stage 2). The root
            // `Secret` is minted from the host's signing-key grant as an opaque
            // externref; there is no guest-visible integer handle.
            let mut dir_grant_ord = 0i32;
            let mut file_grant_ord = 0i32;
            let mut net_grant_ord = 0i32;
            let mut fetch_grant_ord = 0i32;
            let mut user_cap_ord = 0i32;
            let mut main_args: Vec<WirExpr> = Vec::with_capacity(main_params);
            for i in 0..main_params {
                if main_param_is_args.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::Call { func: "build_args".into(), args: vec![] });
                } else if main_param_is_dir.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_dir".into(),
                        args: vec![WirExpr::ConstI32(dir_grant_ord)],
                    });
                    dir_grant_ord += 1;
                } else if main_param_is_file.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_file".into(),
                        args: vec![WirExpr::ConstI32(file_grant_ord)],
                    });
                    file_grant_ord += 1;
                } else if main_param_is_env.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_env".into(),
                        args: vec![],
                    });
                } else if main_param_is_exec.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_exec".into(),
                        args: vec![],
                    });
                } else if main_param_is_net.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_net".into(),
                        args: vec![WirExpr::ConstI32(net_grant_ord)],
                    });
                    net_grant_ord += 1;
                } else if main_param_is_fetch.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_fetch".into(),
                        args: vec![WirExpr::ConstI32(fetch_grant_ord)],
                    });
                    fetch_grant_ord += 1;
                } else if main_param_is_secret.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_secret".into(),
                        args: vec![WirExpr::ConstI32(0)],
                    });
                } else if let Some((tn, nfields)) = main_param_user_cap.get(i).cloned().flatten() {
                    // RFC-0038: mint the sealed record from the grant —
                    // `mk{N}(tag, build_user_cap_field(k, 0), …, build_user_cap_field(k, N-1))`.
                    // `k` is the grantable param's ordinal (indexing the host's
                    // `user_cap_fields`); the tag is the ctor's variant discriminant;
                    // each field is a separately-alloc'd String widened to the i64 slot.
                    let k = user_cap_ord;
                    user_cap_ord += 1;
                    let tag = cg.ctors.get(&tn).map(|c| c.0).unwrap_or(0) as i32;
                    let mut mk_args: Vec<WirExpr> = Vec::with_capacity(nfields + 1);
                    mk_args.push(WirExpr::ConstI32(tag));
                    for fi in 0..nfields as i32 {
                        mk_args.push(WirExpr::Convert {
                            from: witchy_wir::wir::Kind::I32,
                            to: witchy_wir::wir::Kind::I64,
                            arg: Box::new(WirExpr::Call {
                                func: "build_user_cap_field".into(),
                                args: vec![WirExpr::ConstI32(k), WirExpr::ConstI32(fi)],
                            }),
                        });
                    }
                    main_args.push(WirExpr::Call { func: format!("mk{nfields}"), args: mk_args });
                } else {
                    main_args.push(WirExpr::ConstI32(0));
                }
            }
            // The `run` export calls `main`; an Int/Float result is printed (the
            // exit-code convention), anything else is dropped — matching the WAT
            // sink's `run` tail. Only synthesized when the module has a `main`; a
            // pure string-export library (no `main`) exports only `__galloc` + the
            // `__export_*` wrappers.
            if has_main {
                let main_call = WirExpr::Call { func: "main".into(), args: main_args };
                let run_body = if main_returns_int {
                    vec![WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![main_call] })]
                } else if main_returns_float {
                    vec![WirNode::Do(WirExpr::CallHost { import: "print_float".into(), args: vec![main_call] })]
                } else {
                    vec![WirNode::Drop(main_call)]
                };
                pruned_funcs.push(WirFunc {
                    name: "run".into(),
                    params: Vec::new(),
                    ret: Vec::new(),
                    locals: Vec::new(),
                    body: run_body,
                    raw_body: None,
                });
            }
            // String-export glue (RFC-0007 §"Data marshaling" / RFC-0008 run loop):
            // a JS host writes a witchy `String` header `[i32 len][bytes]` into guest
            // memory at a `__galloc`-returned pointer, then calls `__export_f(ptr,
            // len)`; the wrapper passes the pointer straight to the witchy fn (whose
            // single `String` param IS that header) and returns the result String
            // pointer. No import, no authority — only guest-memory reads/writes.
            if !string_exports.is_empty() {
                // __galloc(len) -> ptr — (RFC-0051 I2) the shared WIR `$__galloc`
                // (which delegates to `$bump_alloc`, the single ensure-prefixed
                // allocator) rather than an inline ensure+bump twin.
                pruned_funcs.push(witchy_wir::wir_helpers::galloc_helper());
                // One `__export_f(in_ptr, in_len) -> out_ptr` per string export. The
                // `in_len` param is accepted for ABI symmetry (and a future bounds
                // check) but the String header is self-describing, so the wrapper
                // forwards `in_ptr` to the witchy fn directly.
                for name in &string_exports {
                    // (RFC-0040) A cap-gated export mints its grantable cap host-side
                    // (`mk{N}(tag, i64(build_user_cap_field(0, i))…)`, mirroring the `run`
                    // wrapper for `main`), prepended before the input String pointer.
                    let mut call_args: Vec<WirExpr> = Vec::new();
                    if let Some((_, cap_ty, nfields)) = export_cap_info.iter().find(|(n, _, _)| n == name) {
                        let tag = cg.ctors.get(cap_ty).map(|c| c.0).unwrap_or(0) as i32;
                        let mut mk_args: Vec<WirExpr> = Vec::with_capacity(nfields + 1);
                        mk_args.push(WirExpr::ConstI32(tag));
                        for fi in 0..*nfields as i32 {
                            mk_args.push(WirExpr::Convert {
                                from: witchy_wir::wir::Kind::I32,
                                to: witchy_wir::wir::Kind::I64,
                                arg: Box::new(WirExpr::Call {
                                    func: "build_user_cap_field".into(),
                                    args: vec![WirExpr::ConstI32(0), WirExpr::ConstI32(fi)],
                                }),
                            });
                        }
                        call_args.push(WirExpr::Call { func: format!("mk{nfields}"), args: mk_args });
                    }
                    call_args.push(WirExpr::GetLocal("in_ptr".into()));
                    pruned_funcs.push(WirFunc {
                        name: string_export_name(name),
                        params: vec![
                            witchy_wir::wir::WirLocal { name: "in_ptr".into(), ty: witchy_wir::wir::WirTy::Bool },
                            witchy_wir::wir::WirLocal { name: "in_len".into(), ty: witchy_wir::wir::WirTy::Bool },
                        ],
                        ret: vec![witchy_wir::wir::WirTy::Bool], // i32 result String pointer
                        locals: vec![],
                        body: vec![WirNode::Push(WirExpr::Call {
                            func: name.clone(),
                            args: call_args,
                        })],
                        raw_body: None,
                    });
                }
            }
            let mut glamour_state_field_kinds = None;
            if let Some(family) = &glamour_exports {
                use witchy_wir::wir::{BinOp, WirLocal, WirTy};
                let state_wir_ty = cg
                    .wir_funcs
                    .get(&family.init)
                    .and_then(|function| function.ret.first())
                    .cloned()
                    .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                        message: "`glamour_init` did not lower to one state result".into(),
                    }))?;
                if state_wir_ty.kind() != WK::I32 {
                    return unsupported(format!(
                        "RFC-0108 application state `{}` must use the scalar state arena",
                        match family.state_type.unqualified() {
                            Type::Named(name, _) => name,
                            _ => "<state>",
                        },
                    ));
                }
                let dispatch_state_ty = cg
                    .wir_funcs
                    .get(&family.dispatch)
                    .and_then(|function| function.ret.first())
                    .cloned();
                if dispatch_state_ty.as_ref() != Some(&state_wir_ty) {
                    return unsupported(
                        "RFC-0108 dispatch result representation differs from init state",
                    );
                }
                if cg.summaries.arg_leaks(&family.emit, 0, 1) {
                    return unsupported(
                        "RFC-0108 `glamour_emit` output may alias application state; \
                         emit must construct a fresh `Bytes` frame",
                    );
                }
                let aggregate_state = glamour_state_rcopy_helper.is_some();
                let state_field_kinds = if aggregate_state {
                    Vec::new()
                } else {
                    let mut kinds = Vec::with_capacity(family.state_fields.len());
                    for field in &family.state_fields {
                        let kind = match field.unqualified() {
                            Type::Named(name, arguments) if arguments.is_empty() => {
                                match name.as_str() {
                                    "Int" | "Duration" => WK::I64,
                                    "Float" => WK::F64,
                                    "Bool" => WK::I32,
                                    _ => unreachable!(
                                        "non-scalar Glamour state selected scalar storage"
                                    ),
                                }
                            }
                            _ => unreachable!("non-scalar Glamour state selected scalar storage"),
                        };
                        kinds.push(kind);
                    }
                    glamour_state_field_kinds = Some(kinds.clone());
                    kinds
                };
                let development_layout = if let Some(metadata) = glamour_development {
                    if metadata.snapshot_format()
                        == GlamourDevelopmentMetadata::SCALAR_SNAPSHOT_FORMAT
                    {
                        let metadata_kinds = metadata
                            .state_fields
                            .iter()
                            .map(|field| match field {
                                GlamourDevelopmentField::I64 => WK::I64,
                                GlamourDevelopmentField::F64 => WK::F64,
                                GlamourDevelopmentField::Bool => WK::I32,
                                GlamourDevelopmentField::Aggregate => unreachable!(
                                    "aggregate fields do not support scalar snapshots"
                                ),
                            })
                            .collect::<Vec<_>>();
                        if aggregate_state || metadata_kinds != state_field_kinds {
                            return Err(LoweringFailure::Rejected(CodegenError {
                                message: "compiler-owned Glamour snapshot metadata does not match the lowered state representation".into(),
                            }));
                        }
                    } else if !aggregate_state
                        || glamour_state_field_shapes
                            .as_ref()
                            .is_none_or(|shapes| shapes.len() != metadata.state_fields.len())
                    {
                        return Err(LoweringFailure::Rejected(CodegenError {
                            message: "compiler-owned Glamour aggregate tracing metadata does not match the lowered state representation".into(),
                        }));
                    }
                    let field_count = u16::try_from(metadata.state_fields.len()).map_err(|_| {
                        LoweringFailure::Rejected(CodegenError {
                            message: "Glamour development metadata has more than 65535 fields".into(),
                        })
                    })?;
                    let snapshot_layout = if metadata.snapshot_format()
                        == GlamourDevelopmentMetadata::SCALAR_SNAPSHOT_FORMAT
                    {
                        let snapshot_length = 40_u32
                            .checked_add(
                                u32::from(field_count).checked_mul(8).ok_or_else(|| {
                                    LoweringFailure::Rejected(CodegenError {
                                        message: "Glamour snapshot byte length overflows Wasm32".into(),
                                    })
                                })?,
                            )
                            .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                                message: "Glamour snapshot byte length overflows Wasm32".into(),
                            }))?;
                        let snapshot_header = cg.next_offset;
                        cg.next_offset = cg
                            .next_offset
                            .checked_add(4 + snapshot_length)
                            .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                                message: "Glamour snapshot arena exceeds Wasm32 address space".into(),
                            }))?;
                        let mut snapshot = Vec::with_capacity((4 + snapshot_length) as usize);
                        snapshot.extend_from_slice(&snapshot_length.to_le_bytes());
                        snapshot.extend_from_slice(b"WGST");
                        snapshot.extend_from_slice(
                            &GlamourDevelopmentMetadata::SCALAR_SNAPSHOT_FORMAT.to_le_bytes(),
                        );
                        snapshot.extend_from_slice(&field_count.to_le_bytes());
                        snapshot.extend_from_slice(&metadata.model_schema);
                        snapshot.resize((4 + snapshot_length) as usize, 0);
                        glamour_development_data.push(DataSegment {
                            offset: snapshot_header,
                            bytes: snapshot,
                        });
                        Some((snapshot_header, snapshot_length))
                    } else if metadata.snapshot_format()
                        == GlamourDevelopmentMetadata::AGGREGATE_SNAPSHOT_FORMAT
                    {
                        Some((0, 0))
                    } else {
                        None
                    };

                    let changes_length = u32::from(field_count);
                    let changes_pointer = cg.next_offset;
                    cg.next_offset = cg
                        .next_offset
                        .checked_add(changes_length)
                        .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                            message: "Glamour change bitmap exceeds Wasm32 address space".into(),
                        }))?;
                    glamour_development_data.push(DataSegment {
                        offset: changes_pointer,
                        bytes: vec![0; changes_length as usize],
                    });

                    let metadata_payload = metadata.wire_payload();
                    let metadata_length = u32::try_from(metadata_payload.len()).map_err(|_| {
                        LoweringFailure::Rejected(CodegenError {
                            message: "Glamour development metadata exceeds Wasm32".into(),
                        })
                    })?;
                    let metadata_header = cg.next_offset;
                    cg.next_offset = cg
                        .next_offset
                        .checked_add(4 + metadata_length)
                        .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                            message: "Glamour development metadata exceeds Wasm32 address space"
                                .into(),
                        }))?;
                    let mut metadata_record = Vec::with_capacity(4 + metadata_payload.len());
                    metadata_record.extend_from_slice(&metadata_length.to_le_bytes());
                    metadata_record.extend_from_slice(&metadata_payload);
                    glamour_development_data.push(DataSegment {
                        offset: metadata_header,
                        bytes: metadata_record,
                    });
                    Some((
                        snapshot_layout,
                        metadata_header,
                        metadata.model_schema,
                        changes_pointer,
                        changes_length,
                    ))
                } else {
                    None
                };
                // Keep the fixed input arena below the Phase 3 eight-page
                // budget. Larger frames are rejected by the authenticated
                // reserve export instead of forcing every island to pay for
                // an oversized linear-memory reservation.
                const GLAMOUR_INPUT_CAPACITY: u32 = 256 * 1024;
                let glamour_input_header = cg.next_offset;
                cg.next_offset = cg
                    .next_offset
                    .checked_add(4 + GLAMOUR_INPUT_CAPACITY)
                    .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                        message: "RFC-0108 input arena exceeds Wasm32 address space".into(),
                    }))?;
                let i32_local = |name: &str| WirLocal {
                    name: name.into(),
                    ty: WirTy::Bool,
                };
                let i32_binary = |op, left, right| WirExpr::Binary {
                    op,
                    kind: WK::I32,
                    lhs: Box::new(left),
                    rhs: Box::new(right),
                };
                let i64_binary = |op, left, right| WirExpr::Binary {
                    op,
                    kind: WK::I64,
                    lhs: Box::new(left),
                    rhs: Box::new(right),
                };
                let trap_when = |condition| WirNode::If {
                    cond: condition,
                    then_: vec![WirNode::Unreachable],
                    els: vec![],
                    result: None,
                };
                let global_ne = |name: &str, value: i32| {
                    i32_binary(
                        BinOp::Ne,
                        WirExpr::GetGlobal(name.into()),
                        WirExpr::ConstI32(value),
                    )
                };
                let input_header = || {
                    i32_binary(
                        BinOp::Sub,
                        WirExpr::GetLocal("input_ptr".into()),
                        WirExpr::ConstI32(4),
                    )
                };
                let output_payload = || {
                    i32_binary(
                        BinOp::Add,
                        WirExpr::GetLocal("output".into()),
                        WirExpr::ConstI32(4),
                    )
                };
                let state_tag = if aggregate_state {
                    0
                } else {
                    let state_constructor = family
                        .state_constructor
                        .as_deref()
                        .expect("scalar state has one constructor");
                    cg.ctors
                        .get(state_constructor)
                        .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                            message: format!(
                                "RFC-0108 application state constructor `{state_constructor}` is missing"
                            ),
                        }))?
                        .0 as i32
                };
                let state_value = || {
                    if aggregate_state {
                        return WirExpr::GetGlobal("__glamour_state".into());
                    }
                    let mut arguments = Vec::with_capacity(state_field_kinds.len() + 1);
                    arguments.push(WirExpr::ConstI32(state_tag));
                    arguments.extend(state_field_kinds.iter().enumerate().map(|(index, kind)| {
                        WirExpr::ToSlot(
                            Box::new(WirExpr::GetGlobal(format!("__glamour_state_{index}"))),
                            *kind,
                        )
                    }));
                    WirExpr::Call {
                        func: format!("mk{}", state_field_kinds.len()),
                        args: arguments,
                    }
                };
                let save_state = |local: &str| {
                    state_field_kinds
                        .iter()
                        .enumerate()
                        .map(|(index, kind)| WirNode::SetGlobal {
                            global: format!("__glamour_state_{index}"),
                            value: WirExpr::FromSlot(
                                Box::new(WirExpr::Load {
                                    ptr: Box::new(WirExpr::GetLocal(local.into())),
                                    kind: WK::I64,
                                    offset: 4 + (index as u32 * 8),
                                }),
                                *kind,
                            ),
                        })
                        .collect::<Vec<_>>()
                };
                let development_changes = development_layout
                    .as_ref()
                    .map(|(_, _, _, pointer, _)| *pointer);
                let record_state_changes = |local: &str, compare: bool| {
                    let Some(pointer) = development_changes else { return Vec::new() };
                    state_field_kinds
                        .iter()
                        .enumerate()
                        .map(|(index, kind)| {
                            let next = WirExpr::FromSlot(
                                Box::new(WirExpr::Load {
                                    ptr: Box::new(WirExpr::GetLocal(local.into())),
                                    kind: WK::I64,
                                    offset: 4 + (index as u32 * 8),
                                }),
                                *kind,
                            );
                            WirNode::Store8 {
                                ptr: WirExpr::ConstI32(pointer as i32),
                                value: if compare {
                                    WirExpr::Binary {
                                        op: BinOp::Ne,
                                        kind: *kind,
                                        lhs: Box::new(next),
                                        rhs: Box::new(WirExpr::GetGlobal(format!(
                                            "__glamour_state_{index}"
                                        ))),
                                    }
                                } else {
                                    WirExpr::ConstI32(1)
                                },
                                offset: index as u32,
                            }
                        })
                        .collect::<Vec<_>>()
                };
                let aggregate_initial_changes = development_changes
                    .zip(glamour_state_field_shapes.as_ref())
                    .map(|(pointer, shapes)| {
                        shapes
                            .iter()
                            .enumerate()
                            .map(|(index, _)| WirNode::Store8 {
                                ptr: WirExpr::ConstI32(pointer as i32),
                                value: WirExpr::ConstI32(1),
                                offset: index as u32,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let aggregate_dispatch_changes = if let (Some(pointer), Some(shapes)) =
                    (development_changes, glamour_state_field_shapes.as_ref())
                {
                    let mut changes = Vec::with_capacity(shapes.len());
                    for (index, shape) in shapes.iter().enumerate() {
                        let offset = 4 + (index as i32 * 8);
                        let address = |local: &str| {
                            i32_binary(
                                BinOp::Add,
                                WirExpr::GetLocal(local.into()),
                                WirExpr::ConstI32(offset),
                            )
                        };
                        let comparison = cg
                            .slot_cmp_wir(shape, address("old_state"), address("next_state"))
                            .ok_or_else(|| LoweringFailure::Rejected(CodegenError {
                                message: "Glamour aggregate development tracing could not compare one model field".into(),
                            }))?;
                        changes.push(WirNode::Store8 {
                            ptr: WirExpr::ConstI32(pointer as i32),
                            value: WirExpr::Unary {
                                op: witchy_wir::wir::UnOp::Not,
                                kind: WK::I32,
                                arg: Box::new(comparison),
                            },
                            offset: index as u32,
                        });
                    }
                    changes
                } else {
                    Vec::new()
                };
                let prepare_aggregate_state = |source: &str, base: &str| {
                    let helper = glamour_state_rcopy_helper
                        .as_ref()
                        .expect("aggregate state has a copy helper");
                    vec![
                        WirNode::SetGlobal {
                            global: "rcopy_wm".into(),
                            value: WirExpr::ConstI32(0),
                        },
                        WirNode::SetGlobal {
                            global: "rcopy_base".into(),
                            value: WirExpr::GetGlobal("heap".into()),
                        },
                        WirNode::SetGlobal {
                            global: "rcopy_delta".into(),
                            value: i32_binary(
                                BinOp::Sub,
                                WirExpr::GetGlobal("heap".into()),
                                WirExpr::GetLocal(base.into()),
                            ),
                        },
                        WirNode::SetGlobal {
                            global: "rc_freelist".into(),
                            value: WirExpr::ConstI32(0),
                        },
                        WirNode::SetLocal {
                            local: "stable_state".into(),
                            value: WirExpr::Call {
                                func: helper.clone(),
                                args: vec![WirExpr::GetLocal(source.into())],
                            },
                        },
                        WirNode::SetLocal {
                            local: "copied_length".into(),
                            value: i32_binary(
                                BinOp::Sub,
                                WirExpr::GetGlobal("heap".into()),
                                WirExpr::GetGlobal("rcopy_base".into()),
                            ),
                        },
                    ]
                };
                let finish_aggregate_state = |base: &str| {
                    let mut nodes = Vec::new();
                    if witchy_wir::wir_helpers::heap_check_enabled() {
                        nodes.push(WirNode::Do(WirExpr::Call {
                            func: "__heap_reclaim".into(),
                            args: vec![WirExpr::GetLocal(base.into())],
                        }));
                    }
                    nodes.extend([
                        WirNode::MemoryCopy {
                            dest: WirExpr::GetLocal(base.into()),
                            src: WirExpr::GetGlobal("rcopy_base".into()),
                            len: WirExpr::GetLocal("copied_length".into()),
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_state".into(),
                            value: WirExpr::GetLocal("stable_state".into()),
                        },
                        WirNode::SetGlobal {
                            global: "heap".into(),
                            value: i32_binary(
                                BinOp::Add,
                                WirExpr::GetLocal(base.into()),
                                WirExpr::GetLocal("copied_length".into()),
                            ),
                        },
                        WirNode::SetGlobal {
                            global: "rc_freelist".into(),
                            value: WirExpr::ConstI32(0),
                        },
                    ]);
                    nodes
                };
                let mut cap_args = vec![WirExpr::ConstI32(
                    cg.ctors.get(&family.cap_type).map(|constructor| constructor.0).unwrap_or(0)
                        as i32,
                )];
                for field in 0..family.cap_fields as i32 {
                    cap_args.push(WirExpr::Convert {
                        from: WK::I32,
                        to: WK::I64,
                        arg: Box::new(WirExpr::Call {
                            func: "build_user_cap_field".into(),
                            args: vec![WirExpr::ConstI32(0), WirExpr::ConstI32(field)],
                        }),
                    });
                }
                let root_cap = WirExpr::Call {
                    func: format!("mk{}", family.cap_fields),
                    args: cap_args,
                };

                pruned_funcs.push(WirFunc {
                    name: "__glamour_protocol_version".into(),
                    params: vec![],
                    ret: vec![WirTy::Bool],
                    locals: vec![],
                    body: vec![WirNode::Push(WirExpr::ConstI32((1_i32 << 16) | 4))],
                    raw_body: None,
                });
                pruned_funcs.push(WirFunc {
                    name: "__glamour_input_reserve".into(),
                    params: vec![i32_local("length")],
                    ret: vec![WirTy::Bool],
                    locals: vec![i32_local("header")],
                    body: vec![
                        trap_when(i32_binary(
                            BinOp::Lt,
                            WirExpr::GetLocal("length".into()),
                            WirExpr::ConstI32(0),
                        )),
                        trap_when(i32_binary(
                            BinOp::Gt,
                            WirExpr::GetLocal("length".into()),
                            WirExpr::ConstI32(GLAMOUR_INPUT_CAPACITY as i32),
                        )),
                        WirNode::SetLocal {
                            local: "header".into(),
                            value: WirExpr::ConstI32(glamour_input_header as i32),
                        },
                        WirNode::Store {
                            ptr: WirExpr::GetLocal("header".into()),
                            value: WirExpr::GetLocal("length".into()),
                            kind: WK::I32,
                            offset: 0,
                        },
                        WirNode::Push(i32_binary(
                            BinOp::Add,
                            WirExpr::GetLocal("header".into()),
                            WirExpr::ConstI32(4),
                        )),
                    ],
                    raw_body: None,
                });
                let mut init_body = vec![
                    trap_when(global_ne("__glamour_live", 0)),
                    trap_when(global_ne("__glamour_busy", 0)),
                    trap_when(global_ne("__glamour_output", 0)),
                    trap_when(i32_binary(
                        BinOp::Ne,
                        WirExpr::GetLocal("input_ptr".into()),
                        WirExpr::ConstI32((glamour_input_header + 4) as i32),
                    )),
                    trap_when(i32_binary(
                        BinOp::Ne,
                        WirExpr::Load {
                            ptr: Box::new(input_header()),
                            kind: WK::I32,
                            offset: 0,
                        },
                        WirExpr::GetLocal("input_length".into()),
                    )),
                    WirNode::SetGlobal {
                        global: "__glamour_busy".into(),
                        value: WirExpr::ConstI32(1),
                    },
                    WirNode::SetLocal {
                        local: "call_base".into(),
                        value: WirExpr::GetGlobal("heap".into()),
                    },
                ];
                init_body.push(WirNode::SetGlobal {
                    global: "__glamour_state_base".into(),
                    value: WirExpr::GetLocal("call_base".into()),
                });
                init_body.push(WirNode::SetLocal {
                    local: "state".into(),
                    value: WirExpr::Call {
                        func: family.init.clone(),
                        args: vec![root_cap, input_header()],
                    },
                });
                if aggregate_state {
                    init_body.extend(aggregate_initial_changes.clone());
                    init_body.extend(prepare_aggregate_state("state", "call_base"));
                    init_body.push(WirNode::Drop(WirExpr::Call {
                        func: family.release.clone(),
                        args: vec![WirExpr::GetLocal("state".into())],
                    }));
                    init_body.extend(finish_aggregate_state("call_base"));
                } else {
                    init_body.extend(record_state_changes("state", false));
                    init_body.extend(save_state("state"));
                    init_body.extend([
                        WirNode::Drop(WirExpr::Call {
                            func: family.release.clone(),
                            args: vec![WirExpr::GetLocal("state".into())],
                        }),
                        WirNode::SetGlobal {
                            global: "heap".into(),
                            value: WirExpr::GetLocal("call_base".into()),
                        },
                    ]);
                }
                let mut resume_body = init_body.clone();
                resume_body.extend([
                    WirNode::SetGlobal {
                        global: "__glamour_live".into(),
                        value: WirExpr::ConstI32(1),
                    },
                    WirNode::SetGlobal {
                        global: "__glamour_busy".into(),
                        value: WirExpr::ConstI32(0),
                    },
                    WirNode::Push(WirExpr::ConstI32(0)),
                ]);
                pruned_funcs.push(WirFunc {
                    name: "__glamour_resume".into(),
                    params: vec![i32_local("input_ptr"), i32_local("input_length")],
                    ret: vec![WirTy::Bool],
                    locals: vec![
                        WirLocal { name: "state".into(), ty: state_wir_ty.clone() },
                        i32_local("call_base"),
                        i32_local("stable_state"),
                        i32_local("copied_length"),
                    ],
                    body: resume_body,
                    raw_body: None,
                });
                init_body.extend([
                    WirNode::SetGlobal {
                        global: "__glamour_output_base".into(),
                        value: WirExpr::GetGlobal("heap".into()),
                    },
                    WirNode::SetLocal {
                        local: "emit_state".into(),
                        value: state_value(),
                    },
                    WirNode::SetLocal {
                        local: "output".into(),
                        value: WirExpr::Call {
                            func: family.emit.clone(),
                            args: vec![WirExpr::GetLocal("emit_state".into())],
                        },
                    },
                ]);
                if !aggregate_state {
                    init_body.push(WirNode::Drop(WirExpr::Call {
                        func: family.release.clone(),
                        args: vec![WirExpr::GetLocal("emit_state".into())],
                    }));
                }
                init_body.extend([
                    WirNode::SetGlobal {
                        global: "__glamour_output".into(),
                        value: WirExpr::GetLocal("output".into()),
                    },
                    WirNode::SetGlobal {
                        global: "__glamour_live".into(),
                        value: WirExpr::ConstI32(1),
                    },
                    WirNode::SetGlobal {
                        global: "__glamour_busy".into(),
                        value: WirExpr::ConstI32(0),
                    },
                    WirNode::Push(output_payload()),
                ]);
                pruned_funcs.push(WirFunc {
                    name: "__glamour_init".into(),
                    params: vec![i32_local("input_ptr"), i32_local("input_length")],
                    ret: vec![WirTy::Bool],
                    locals: vec![
                        WirLocal { name: "state".into(), ty: state_wir_ty.clone() },
                        WirLocal { name: "emit_state".into(), ty: state_wir_ty.clone() },
                        i32_local("call_base"),
                        i32_local("stable_state"),
                        i32_local("copied_length"),
                        i32_local("output"),
                    ],
                    body: init_body,
                    raw_body: None,
                });
                let mut dispatch_body = vec![
                    trap_when(global_ne("__glamour_live", 1)),
                    trap_when(global_ne("__glamour_busy", 0)),
                    trap_when(global_ne("__glamour_output", 0)),
                    trap_when(i32_binary(
                        BinOp::Ne,
                        WirExpr::GetLocal("input_ptr".into()),
                        WirExpr::ConstI32((glamour_input_header + 4) as i32),
                    )),
                    trap_when(i32_binary(
                        BinOp::Ne,
                        WirExpr::Load {
                            ptr: Box::new(input_header()),
                            kind: WK::I32,
                            offset: 0,
                        },
                        WirExpr::GetLocal("input_length".into()),
                    )),
                    WirNode::SetGlobal {
                        global: "__glamour_busy".into(),
                        value: WirExpr::ConstI32(1),
                    },
                    WirNode::SetLocal {
                        local: "call_base".into(),
                        value: WirExpr::GetGlobal("heap".into()),
                    },
                    WirNode::SetLocal {
                        local: "state_base".into(),
                        value: if aggregate_state {
                            WirExpr::GetGlobal("__glamour_state_base".into())
                        } else {
                            WirExpr::GetGlobal("heap".into())
                        },
                    },
                    WirNode::SetLocal {
                        local: "old_state".into(),
                        value: state_value(),
                    },
                    WirNode::SetLocal {
                        local: "next_state".into(),
                        value: WirExpr::Call {
                            func: family.dispatch.clone(),
                            args: vec![
                                WirExpr::GetLocal("old_state".into()),
                                input_header(),
                            ],
                        },
                    },
                ];
                if aggregate_state {
                    dispatch_body.extend(aggregate_dispatch_changes);
                    dispatch_body.extend(prepare_aggregate_state("next_state", "state_base"));
                    dispatch_body.extend([
                        WirNode::Drop(WirExpr::Call {
                            func: family.release.clone(),
                            args: vec![WirExpr::GetLocal("old_state".into())],
                        }),
                        WirNode::Drop(WirExpr::Call {
                            func: family.release.clone(),
                            args: vec![WirExpr::GetLocal("next_state".into())],
                        }),
                    ]);
                    dispatch_body.extend(finish_aggregate_state("state_base"));
                } else {
                    dispatch_body.extend(record_state_changes("next_state", true));
                    dispatch_body.extend(save_state("next_state"));
                    dispatch_body.extend([
                        WirNode::Drop(WirExpr::Call {
                            func: family.release.clone(),
                            args: vec![WirExpr::GetLocal("old_state".into())],
                        }),
                        WirNode::Drop(WirExpr::Call {
                            func: family.release.clone(),
                            args: vec![WirExpr::GetLocal("next_state".into())],
                        }),
                        WirNode::SetGlobal {
                            global: "heap".into(),
                            value: WirExpr::GetLocal("call_base".into()),
                        },
                    ]);
                }
                dispatch_body.extend([
                    WirNode::SetGlobal {
                        global: "__glamour_output_base".into(),
                        value: WirExpr::GetGlobal("heap".into()),
                    },
                    WirNode::SetLocal {
                        local: "emit_state".into(),
                        value: state_value(),
                    },
                    WirNode::SetLocal {
                        local: "output".into(),
                        value: WirExpr::Call {
                            func: family.emit.clone(),
                            args: vec![WirExpr::GetLocal("emit_state".into())],
                        },
                    },
                ]);
                if !aggregate_state {
                    dispatch_body.push(WirNode::Drop(WirExpr::Call {
                        func: family.release.clone(),
                        args: vec![WirExpr::GetLocal("emit_state".into())],
                    }));
                }
                dispatch_body.extend([
                    WirNode::SetGlobal {
                        global: "__glamour_output".into(),
                        value: WirExpr::GetLocal("output".into()),
                    },
                    WirNode::SetGlobal {
                        global: "__glamour_busy".into(),
                        value: WirExpr::ConstI32(0),
                    },
                    WirNode::Push(output_payload()),
                ]);
                pruned_funcs.push(WirFunc {
                    name: "__glamour_dispatch".into(),
                    params: vec![i32_local("input_ptr"), i32_local("input_length")],
                    ret: vec![WirTy::Bool],
                    locals: vec![
                        WirLocal { name: "old_state".into(), ty: state_wir_ty.clone() },
                        WirLocal { name: "next_state".into(), ty: state_wir_ty.clone() },
                        WirLocal { name: "emit_state".into(), ty: state_wir_ty.clone() },
                        i32_local("call_base"),
                        i32_local("state_base"),
                        i32_local("stable_state"),
                        i32_local("copied_length"),
                        i32_local("output"),
                    ],
                    body: dispatch_body,
                    raw_body: None,
                });
                pruned_funcs.push(WirFunc {
                    name: "__glamour_output_length".into(),
                    params: vec![],
                    ret: vec![WirTy::Bool],
                    locals: vec![],
                    body: vec![WirNode::Push(WirExpr::Control(Box::new(WirNode::If {
                        cond: global_ne("__glamour_output", 0),
                        then_: vec![WirNode::Push(WirExpr::Load {
                            ptr: Box::new(WirExpr::GetGlobal("__glamour_output".into())),
                            kind: WK::I32,
                            offset: 0,
                        })],
                        els: vec![WirNode::Push(WirExpr::ConstI32(0))],
                        result: Some(WirTy::Bool),
                    })))],
                    raw_body: None,
                });
                pruned_funcs.push(WirFunc {
                    name: "__glamour_output_release".into(),
                    params: vec![],
                    ret: vec![],
                    locals: vec![],
                    body: vec![WirNode::If {
                        cond: global_ne("__glamour_output", 0),
                        then_: vec![
                            WirNode::SetGlobal {
                                global: "__glamour_output".into(),
                                value: WirExpr::ConstI32(0),
                            },
                            WirNode::SetGlobal {
                                global: "heap".into(),
                                value: WirExpr::GetGlobal("__glamour_output_base".into()),
                            },
                        ],
                        els: vec![],
                        result: None,
                    }],
                    raw_body: None,
                });
                if let Some((
                    _,
                    metadata_header,
                    _,
                    changes_pointer,
                    changes_length,
                )) = development_layout.as_ref()
                {
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_metadata".into(),
                        params: vec![],
                        ret: vec![WirTy::Bool],
                        locals: vec![],
                        body: vec![WirNode::Push(WirExpr::ConstI32(
                            *metadata_header as i32,
                        ))],
                        raw_body: None,
                    });
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_changes".into(),
                        params: vec![],
                        ret: vec![WirTy::Bool],
                        locals: vec![],
                        body: vec![WirNode::Push(WirExpr::ConstI32(
                            *changes_pointer as i32,
                        ))],
                        raw_body: None,
                    });
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_changes_length".into(),
                        params: vec![],
                        ret: vec![WirTy::Bool],
                        locals: vec![],
                        body: vec![WirNode::Push(WirExpr::ConstI32(
                            *changes_length as i32,
                        ))],
                        raw_body: None,
                    });
                }
                let aggregate_development_layout = development_layout;
                if let Some((
                    Some((snapshot_header, snapshot_length)),
                    _,
                    model_schema,
                    changes_pointer,
                    _,
                )) = development_layout
                    && snapshot_length != 0
                {
                    let snapshot_payload = snapshot_header + 4;
                    let mut snapshot_body = vec![
                        trap_when(global_ne("__glamour_live", 1)),
                        trap_when(global_ne("__glamour_busy", 0)),
                        trap_when(global_ne("__glamour_output", 0)),
                    ];
                    snapshot_body.extend(state_field_kinds.iter().enumerate().map(
                        |(index, kind)| WirNode::Store {
                            ptr: WirExpr::ConstI32(snapshot_payload as i32),
                            value: WirExpr::GetGlobal(format!("__glamour_state_{index}")),
                            kind: *kind,
                            offset: 40 + (index as u32 * 8),
                        },
                    ));
                    snapshot_body.push(WirNode::Push(WirExpr::ConstI32(
                        snapshot_payload as i32,
                    )));
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_snapshot".into(),
                        params: vec![],
                        ret: vec![WirTy::Bool],
                        locals: vec![],
                        body: snapshot_body,
                        raw_body: None,
                    });
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_snapshot_length".into(),
                        params: vec![],
                        ret: vec![WirTy::Bool],
                        locals: vec![],
                        body: vec![WirNode::Push(WirExpr::ConstI32(
                            snapshot_length as i32,
                        ))],
                        raw_body: None,
                    });
                    let field_count = state_field_kinds.len() as u16;
                    let mut snapshot_prefix = [0_u8; 8];
                    snapshot_prefix[..4].copy_from_slice(b"WGST");
                    snapshot_prefix[4..6].copy_from_slice(
                        &GlamourDevelopmentMetadata::SCALAR_SNAPSHOT_FORMAT.to_le_bytes(),
                    );
                    snapshot_prefix[6..8].copy_from_slice(&field_count.to_le_bytes());
                    let mut restore_body = vec![
                        trap_when(global_ne("__glamour_live", 0)),
                        trap_when(global_ne("__glamour_busy", 0)),
                        trap_when(global_ne("__glamour_output", 0)),
                        trap_when(i32_binary(
                            BinOp::Ne,
                            WirExpr::GetLocal("input_ptr".into()),
                            WirExpr::ConstI32((glamour_input_header + 4) as i32),
                        )),
                        trap_when(i32_binary(
                            BinOp::Ne,
                            WirExpr::GetLocal("input_length".into()),
                            WirExpr::ConstI32(snapshot_length as i32),
                        )),
                        trap_when(i32_binary(
                            BinOp::Ne,
                            WirExpr::Load {
                                ptr: Box::new(input_header()),
                                kind: WK::I32,
                                offset: 0,
                            },
                            WirExpr::GetLocal("input_length".into()),
                        )),
                        trap_when(i64_binary(
                            BinOp::Ne,
                            WirExpr::Load {
                                ptr: Box::new(WirExpr::GetLocal("input_ptr".into())),
                                kind: WK::I64,
                                offset: 0,
                            },
                            WirExpr::ConstI64(i64::from_le_bytes(snapshot_prefix)),
                        )),
                    ];
                    for (index, chunk) in model_schema.chunks_exact(8).enumerate() {
                        restore_body.push(trap_when(i64_binary(
                            BinOp::Ne,
                            WirExpr::Load {
                                ptr: Box::new(WirExpr::GetLocal("input_ptr".into())),
                                kind: WK::I64,
                                offset: 8 + (index as u32 * 8),
                            },
                            WirExpr::ConstI64(i64::from_le_bytes(
                                chunk.try_into().expect("schema chunks are eight bytes"),
                            )),
                        )));
                    }
                    for (index, kind) in state_field_kinds.iter().enumerate() {
                        if *kind == WK::I32 {
                            restore_body.push(trap_when(i32_binary(
                                BinOp::GtU,
                                WirExpr::Load {
                                    ptr: Box::new(WirExpr::GetLocal("input_ptr".into())),
                                    kind: WK::I32,
                                    offset: 40 + (index as u32 * 8),
                                },
                                WirExpr::ConstI32(1),
                            )));
                        }
                    }
                    restore_body.push(WirNode::SetGlobal {
                        global: "__glamour_busy".into(),
                        value: WirExpr::ConstI32(1),
                    });
                    restore_body.extend(state_field_kinds.iter().enumerate().map(
                        |(index, _)| WirNode::Store8 {
                            ptr: WirExpr::ConstI32(changes_pointer as i32),
                            value: WirExpr::ConstI32(1),
                            offset: index as u32,
                        },
                    ));
                    restore_body.extend(state_field_kinds.iter().enumerate().map(
                        |(index, kind)| WirNode::SetGlobal {
                            global: format!("__glamour_state_{index}"),
                            value: WirExpr::Load {
                                ptr: Box::new(WirExpr::GetLocal("input_ptr".into())),
                                kind: *kind,
                                offset: 40 + (index as u32 * 8),
                            },
                        },
                    ));
                    restore_body.extend([
                        WirNode::SetGlobal {
                            global: "__glamour_state_base".into(),
                            value: WirExpr::GetGlobal("heap".into()),
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_output_base".into(),
                            value: WirExpr::GetGlobal("heap".into()),
                        },
                        WirNode::SetLocal {
                            local: "emit_state".into(),
                            value: state_value(),
                        },
                        WirNode::SetLocal {
                            local: "output".into(),
                            value: WirExpr::Call {
                                func: family.emit.clone(),
                                args: vec![WirExpr::GetLocal("emit_state".into())],
                            },
                        },
                        WirNode::Drop(WirExpr::Call {
                            func: family.release.clone(),
                            args: vec![WirExpr::GetLocal("emit_state".into())],
                        }),
                        WirNode::SetGlobal {
                            global: "__glamour_output".into(),
                            value: WirExpr::GetLocal("output".into()),
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_live".into(),
                            value: WirExpr::ConstI32(1),
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_busy".into(),
                            value: WirExpr::ConstI32(0),
                        },
                        WirNode::Push(output_payload()),
                    ]);
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_restore".into(),
                        params: vec![i32_local("input_ptr"), i32_local("input_length")],
                        ret: vec![WirTy::Bool],
                        locals: vec![
                            WirLocal {
                                name: "emit_state".into(),
                                ty: state_wir_ty.clone(),
                            },
                            i32_local("output"),
                        ],
                        body: restore_body,
                        raw_body: None,
                    });
                }
                if let Some((Some((_, 0)), _, _, changes_pointer, _)) =
                    aggregate_development_layout
                {
                    const MAXIMUM_SNAPSHOT_BYTES: i32 = 1024 * 1024 - 44;
                    let codec = glamour_development
                        .and_then(|metadata| metadata.snapshot_codec.as_ref())
                        .expect("format-2 snapshots have a typed codec");
                    let field_count = glamour_development
                        .map(|metadata| metadata.state_fields.len() as u16)
                        .unwrap_or(0);
                    let mut snapshot_prefix = [0_u8; 8];
                    snapshot_prefix[..4].copy_from_slice(b"WGST");
                    snapshot_prefix[4..6].copy_from_slice(
                        &GlamourDevelopmentMetadata::AGGREGATE_SNAPSHOT_FORMAT.to_le_bytes(),
                    );
                    snapshot_prefix[6..8].copy_from_slice(&field_count.to_le_bytes());
                    let mut snapshot_body = vec![
                        trap_when(global_ne("__glamour_live", 1)),
                        trap_when(global_ne("__glamour_busy", 0)),
                        trap_when(global_ne("__glamour_output", 0)),
                        WirNode::SetLocal {
                            local: "call_base".into(),
                            value: WirExpr::GetGlobal("heap".into()),
                        },
                        WirNode::SetLocal {
                            local: "wire".into(),
                            value: WirExpr::Call {
                                func: codec.encoder.clone(),
                                args: vec![state_value()],
                            },
                        },
                        WirNode::SetLocal {
                            local: "payload_length".into(),
                            value: WirExpr::Load {
                                ptr: Box::new(WirExpr::GetLocal("wire".into())),
                                kind: WK::I32,
                                offset: 0,
                            },
                        },
                        trap_when(i32_binary(
                            BinOp::GtU,
                            WirExpr::GetLocal("payload_length".into()),
                            WirExpr::ConstI32(MAXIMUM_SNAPSHOT_BYTES),
                        )),
                        WirNode::SetLocal {
                            local: "snapshot_length".into(),
                            value: i32_binary(
                                BinOp::Add,
                                WirExpr::GetLocal("payload_length".into()),
                                WirExpr::ConstI32(44),
                            ),
                        },
                        WirNode::SetLocal {
                            local: "snapshot".into(),
                            value: WirExpr::ConstI32((glamour_input_header + 4) as i32),
                        },
                        WirNode::Store {
                            ptr: WirExpr::GetLocal("snapshot".into()),
                            value: WirExpr::ConstI64(i64::from_le_bytes(snapshot_prefix)),
                            kind: WK::I64,
                            offset: 0,
                        },
                    ];
                    for (index, chunk) in glamour_development
                        .expect("development metadata")
                        .model_schema
                        .chunks_exact(8)
                        .enumerate()
                    {
                        snapshot_body.push(WirNode::Store {
                            ptr: WirExpr::GetLocal("snapshot".into()),
                            value: WirExpr::ConstI64(i64::from_le_bytes(
                                chunk.try_into().expect("schema chunk"),
                            )),
                            kind: WK::I64,
                            offset: 8 + index as u32 * 8,
                        });
                    }
                    snapshot_body.extend([
                        WirNode::Store {
                            ptr: WirExpr::GetLocal("snapshot".into()),
                            value: WirExpr::GetLocal("payload_length".into()),
                            kind: WK::I32,
                            offset: 40,
                        },
                        WirNode::MemoryCopy {
                            dest: i32_binary(
                                BinOp::Add,
                                WirExpr::GetLocal("snapshot".into()),
                                WirExpr::ConstI32(44),
                            ),
                            src: i32_binary(
                                BinOp::Add,
                                WirExpr::GetLocal("wire".into()),
                                WirExpr::ConstI32(4),
                            ),
                            len: WirExpr::GetLocal("payload_length".into()),
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_snapshot_length".into(),
                            value: WirExpr::GetLocal("snapshot_length".into()),
                        },
                        WirNode::SetGlobal {
                            global: "heap".into(),
                            value: WirExpr::GetLocal("call_base".into()),
                        },
                        WirNode::SetGlobal {
                            global: "rc_freelist".into(),
                            value: WirExpr::ConstI32(0),
                        },
                        WirNode::Push(WirExpr::GetLocal("snapshot".into())),
                    ]);
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_snapshot".into(),
                        params: vec![],
                        ret: vec![WirTy::Bool],
                        locals: vec![
                            i32_local("wire"),
                            i32_local("payload_length"),
                            i32_local("snapshot_length"),
                            i32_local("snapshot"),
                            i32_local("call_base"),
                        ],
                        body: snapshot_body,
                        raw_body: None,
                    });
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_snapshot_length".into(),
                        params: vec![],
                        ret: vec![WirTy::Bool],
                        locals: vec![],
                        body: vec![WirNode::Push(WirExpr::GetGlobal(
                            "__glamour_snapshot_length".into(),
                        ))],
                        raw_body: None,
                    });

                    let schema_matches = |schema: &[u8; 32]| {
                        schema.chunks_exact(8).enumerate().fold(
                            WirExpr::ConstI32(1),
                            |condition, (index, chunk)| i32_binary(
                                BinOp::And,
                                condition,
                                i64_binary(
                                    BinOp::Eq,
                                    WirExpr::Load {
                                        ptr: Box::new(WirExpr::GetLocal("input_ptr".into())),
                                        kind: WK::I64,
                                        offset: 8 + index as u32 * 8,
                                    },
                                    WirExpr::ConstI64(i64::from_le_bytes(
                                        chunk.try_into().expect("schema chunk"),
                                    )),
                                ),
                            ),
                        )
                    };
                    let decode = |decoder: String| WirNode::SetLocal {
                        local: "decoded_state".into(),
                        value: WirExpr::Call {
                            func: decoder,
                            args: vec![i32_binary(
                                BinOp::Add,
                                WirExpr::GetLocal("input_ptr".into()),
                                WirExpr::ConstI32(40),
                            )],
                        },
                    };
                    let mut decode_branch = vec![WirNode::Unreachable];
                    for migration in codec.migrations.iter().rev() {
                        decode_branch = vec![WirNode::If {
                            cond: schema_matches(&migration.model_schema),
                            then_: vec![decode(migration.decoder.clone())],
                            els: decode_branch,
                            result: None,
                        }];
                    }
                    decode_branch = vec![WirNode::If {
                        cond: schema_matches(
                            &glamour_development.expect("development metadata").model_schema,
                        ),
                        then_: vec![decode(codec.decoder.clone())],
                        els: decode_branch,
                        result: None,
                    }];
                    let mut restore_body = vec![
                        trap_when(global_ne("__glamour_live", 0)),
                        trap_when(global_ne("__glamour_busy", 0)),
                        trap_when(global_ne("__glamour_output", 0)),
                        trap_when(i32_binary(
                            BinOp::Ne,
                            WirExpr::GetLocal("input_ptr".into()),
                            WirExpr::ConstI32((glamour_input_header + 4) as i32),
                        )),
                        trap_when(i32_binary(
                            BinOp::Ne,
                            WirExpr::Load {
                                ptr: Box::new(input_header()),
                                kind: WK::I32,
                                offset: 0,
                            },
                            WirExpr::GetLocal("input_length".into()),
                        )),
                        trap_when(i32_binary(
                            BinOp::LtU,
                            WirExpr::GetLocal("input_length".into()),
                            WirExpr::ConstI32(44),
                        )),
                        trap_when(i32_binary(
                            BinOp::Ne,
                            WirExpr::Load {
                                ptr: Box::new(WirExpr::GetLocal("input_ptr".into())),
                                kind: WK::I32,
                                offset: 0,
                            },
                            WirExpr::ConstI32(i32::from_le_bytes(*b"WGST")),
                        )),
                        trap_when(i32_binary(
                            BinOp::Ne,
                            i32_binary(
                                BinOp::And,
                                WirExpr::Load {
                                    ptr: Box::new(WirExpr::GetLocal("input_ptr".into())),
                                    kind: WK::I32,
                                    offset: 4,
                                },
                                WirExpr::ConstI32(0xffff),
                            ),
                            WirExpr::ConstI32(
                                GlamourDevelopmentMetadata::AGGREGATE_SNAPSHOT_FORMAT as i32,
                            ),
                        )),
                        WirNode::SetLocal {
                            local: "payload_length".into(),
                            value: WirExpr::Load {
                                ptr: Box::new(WirExpr::GetLocal("input_ptr".into())),
                                kind: WK::I32,
                                offset: 40,
                            },
                        },
                        trap_when(i32_binary(
                            BinOp::GtU,
                            WirExpr::GetLocal("payload_length".into()),
                            WirExpr::ConstI32(MAXIMUM_SNAPSHOT_BYTES),
                        )),
                        trap_when(i32_binary(
                            BinOp::Ne,
                            WirExpr::GetLocal("input_length".into()),
                            i32_binary(
                                BinOp::Add,
                                WirExpr::GetLocal("payload_length".into()),
                                WirExpr::ConstI32(44),
                            ),
                        )),
                        WirNode::SetLocal {
                            local: "call_base".into(),
                            value: WirExpr::GetGlobal("heap".into()),
                        },
                    ];
                    restore_body.extend(decode_branch);
                    restore_body.extend([
                        WirNode::SetGlobal {
                            global: "__glamour_busy".into(),
                            value: WirExpr::ConstI32(1),
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_state_base".into(),
                            value: WirExpr::GetLocal("call_base".into()),
                        },
                    ]);
                    restore_body.extend(prepare_aggregate_state("decoded_state", "call_base"));
                    restore_body.push(WirNode::Drop(WirExpr::Call {
                        func: family.release.clone(),
                        args: vec![WirExpr::GetLocal("decoded_state".into())],
                    }));
                    restore_body.extend(finish_aggregate_state("call_base"));
                    restore_body.extend((0..field_count as usize).map(|index| {
                        WirNode::Store8 {
                            ptr: WirExpr::ConstI32(changes_pointer as i32),
                            value: WirExpr::ConstI32(1),
                            offset: index as u32,
                        }
                    }));
                    restore_body.extend([
                        WirNode::SetGlobal {
                            global: "__glamour_output_base".into(),
                            value: WirExpr::GetGlobal("heap".into()),
                        },
                        WirNode::SetLocal {
                            local: "emit_state".into(),
                            value: state_value(),
                        },
                        WirNode::SetLocal {
                            local: "output".into(),
                            value: WirExpr::Call {
                                func: family.emit.clone(),
                                args: vec![WirExpr::GetLocal("emit_state".into())],
                            },
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_output".into(),
                            value: WirExpr::GetLocal("output".into()),
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_live".into(),
                            value: WirExpr::ConstI32(1),
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_busy".into(),
                            value: WirExpr::ConstI32(0),
                        },
                        WirNode::Push(output_payload()),
                    ]);
                    pruned_funcs.push(WirFunc {
                        name: "__glamour_dev_restore".into(),
                        params: vec![i32_local("input_ptr"), i32_local("input_length")],
                        ret: vec![WirTy::Bool],
                        locals: vec![
                            i32_local("payload_length"),
                            WirLocal {
                                name: "decoded_state".into(),
                                ty: state_wir_ty.clone(),
                            },
                            WirLocal {
                                name: "emit_state".into(),
                                ty: state_wir_ty.clone(),
                            },
                            i32_local("call_base"),
                            i32_local("stable_state"),
                            i32_local("copied_length"),
                            i32_local("output"),
                        ],
                        body: restore_body,
                        raw_body: None,
                    });
                }
                let mut dispose_body = vec![WirNode::If {
                    cond: global_ne("__glamour_output", 0),
                    then_: vec![WirNode::SetGlobal {
                        global: "heap".into(),
                        value: WirExpr::GetGlobal("__glamour_output_base".into()),
                    }],
                    els: vec![],
                    result: None,
                }];
                if aggregate_state {
                    dispose_body.extend([
                        WirNode::If {
                            cond: global_ne("__glamour_live", 0),
                            then_: vec![WirNode::Drop(WirExpr::Call {
                                func: family.release.clone(),
                                args: vec![WirExpr::GetGlobal("__glamour_state".into())],
                            })],
                            els: vec![],
                            result: None,
                        },
                        WirNode::SetGlobal {
                            global: "__glamour_state".into(),
                            value: WirExpr::ConstI32(0),
                        },
                    ]);
                } else {
                    dispose_body.extend(state_field_kinds.iter().enumerate().map(
                        |(index, kind)| WirNode::SetGlobal {
                            global: format!("__glamour_state_{index}"),
                            value: match kind {
                                WK::I64 => WirExpr::ConstI64(0),
                                WK::F64 => WirExpr::ConstF64(0.0),
                                WK::I32 => WirExpr::ConstI32(0),
                                _ => unreachable!("state fields were restricted to scalar kinds"),
                            },
                        },
                    ));
                }
                dispose_body.extend([
                    WirNode::If {
                        cond: global_ne("__glamour_state_base", 0),
                        then_: vec![WirNode::SetGlobal {
                            global: "heap".into(),
                            value: WirExpr::GetGlobal("__glamour_state_base".into()),
                        }],
                        els: vec![],
                        result: None,
                    },
                    WirNode::SetGlobal {
                        global: "__glamour_state_base".into(),
                        value: WirExpr::ConstI32(0),
                    },
                    WirNode::SetGlobal {
                        global: "rc_freelist".into(),
                        value: WirExpr::ConstI32(0),
                    },
                    WirNode::SetGlobal {
                        global: "__glamour_output".into(),
                        value: WirExpr::ConstI32(0),
                    },
                    WirNode::SetGlobal {
                        global: "__glamour_live".into(),
                        value: WirExpr::ConstI32(0),
                    },
                    WirNode::SetGlobal {
                        global: "__glamour_busy".into(),
                        value: WirExpr::ConstI32(0),
                    },
                ]);
                pruned_funcs.push(WirFunc {
                    name: "__glamour_dispose".into(),
                    params: vec![],
                    ret: vec![],
                    locals: vec![],
                    body: dispose_body,
                    raw_body: None,
                });
            }
            // Static host-backed helpers already carry a source-site parameter,
            // and source/lambda bodies were instrumented statement by statement.
            // The remaining functions are compiler-synthesized after those
            // passes: run/export wrappers and per-shape render/equality helpers.
            // They have no lexical statement of their own, but must still satisfy
            // the augmented helper signatures. Thread the explicit "unknown"
            // site (0) through any host-backed path they contain. Existing precise
            // sites are idempotently preserved.
            for func in &mut pruned_funcs {
                if prepare_synthetic_diagnostic_sites(func) {
                    cg.uses_diagnostic_sites = true;
                }
            }
            let mut pruned_globals = if uses_heap {
                vec![
                    WirGlobal {
                        name: "heap".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(cg.next_offset as i32),
                        // Exported so a long-lived host (the glamour MVU run loop, which calls a
                        // `String -> String` export once per event) can RESET the bump allocator to
                        // its base after each call. Every `export_*` call is pure — its input,
                        // working, and output allocations are all dead once the host has read the
                        // result String out — so without a reset the never-freeing bump allocator
                        // leaks one call's allocations forever and eventually exhausts memory
                        // (`__galloc` returns an out-of-bounds pointer). The host reads the global's
                        // initial value as the base and restores it; see witchy-runtime.mjs.
                        export: Some("__heap".into()),
                    },
                    // (RFC-0035) The immutable heap base = the initial `$heap` value (the
                    // first byte past the static data segment). Every `$rc_alloc` object
                    // lives at an address >= this; scalars, nullary/immediate values,
                    // capability handles and static-data pointers all sit BELOW it. The
                    // gated `$rc_dup`/`$rc_drop` guard on `ptr >= heap_base`, so emitting
                    // them for any `i32`-kinded value is a sound over-approximation — only
                    // a real refcounted heap object (which alone has the `[rc]` header at
                    // `ptr-8`) is ever touched.
                    WirGlobal {
                        name: "heap_base".into(),
                        kind: WK::I32,
                        mutable: false,
                        init: GlobalInit::I32(cg.next_offset as i32),
                        export: None,
                    },
                    WirGlobal {
                        name: "__witchy_reowns".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_reowns".into()),
                    },
                    // (RFC-0016) Head of the RC-floor size-classed free-list (0 = empty).
                    // `$rc_alloc` pops it, `$rc_free` pushes; declared with `heap` since
                    // they share the allocation path. Empty (no effect) unless the
                    // codegen free-at-overwrite (gated `rc-floor`) emits `$rc_free`.
                    WirGlobal {
                        name: "rc_freelist".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: None,
                    },
                    // (RFC-0016) DoD counter: bytes handed back out of the free-list by
                    // `$rc_alloc` (reused rather than freshly bumped). 0 unless the
                    // free-at-overwrite codegen (gated `rc-floor`) populated the list, so
                    // `witchy stats` proves the optimization actually fired and recycled.
                    WirGlobal {
                        name: "__rc_reused_bytes".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__rc_reused_bytes".into()),
                    },
                    // (RFC-0035) Live-cell counter: `$rc_alloc` +1 (each call yields one live
                    // object), `$rc_free` -1 (each freed object). At exit it is the number of
                    // rc_alloc objects NOT returned to the free-list — a leak metric. For a
                    // fully-reclaiming rc-floor program it stays bounded (→ the reachable roots);
                    // an unbounded leak makes it grow with the input. 0 unless a `$rc_free` fires.
                    WirGlobal {
                        name: "__witchy_live_cells".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_live_cells".into()),
                    },
                    // (RFC-0089) Monotonic operation counts let FIP tests prove
                    // that recursive depth adds no heap work.
                    WirGlobal {
                        name: "__witchy_rc_alloc_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_rc_alloc_calls".into()),
                    },
                    // (RFC-0111) Descriptor-specific counters distinguish physical
                    // packed construction from the shared allocator's other traffic.
                    // Direct packed boundaries do not touch these: canonical
                    // descriptor allocation helpers increment calls and bytes.
                    // Box/reshape metrics are not exported until such adapters
                    // exist with real increment sites and nonzero fixtures.
                    WirGlobal {
                        name: "__witchy_packed_alloc_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_packed_alloc_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_packed_alloc_bytes".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_packed_alloc_bytes".into()),
                    },
                    WirGlobal {
                        name: "__witchy_rc_headers_emitted".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_rc_headers_emitted".into()),
                    },
                    WirGlobal {
                        name: "__witchy_rc_headers_elided".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_rc_headers_elided".into()),
                    },
                    WirGlobal {
                        name: "__witchy_destination_candidates_forwarded".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_destination_candidates_forwarded".into()),
                    },
                    WirGlobal {
                        name: "__witchy_bump_alloc_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_bump_alloc_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_rc_reuse_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_rc_reuse_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_rc_free_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_rc_free_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_region_rewind_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_region_rewind_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_active".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: None,
                    },
                    WirGlobal {
                        name: "__witchy_extract_searches".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_searches".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_key_comparisons".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_key_comparisons".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_copied_bytes".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_copied_bytes".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_retains".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_retains".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_drops".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_drops".into()),
                    },
                ]
            } else {
                Vec::new()
            };
            // RFC-0110: state-bearing calls that retain real table dispatch.
            // This global is independent of linear-memory use: a scalar `var`
            // write-back can still exercise the access envelope in a heap-free
            // module. Direct and devirtualized calls do not increment it.
            pruned_globals.push(WirGlobal {
                name: "__witchy_indirect_ownership_calls".into(),
                kind: WK::I64,
                mutable: true,
                init: GlobalInit::I64(0),
                export: Some("__witchy_indirect_ownership_calls".into()),
            });
            // (RFC-0110 criterion 9) Deterministic ownership counters, all
            // linear-memory-independent scalars like the call counter above.
            //   boundary_reown_copies    — one per normal-mode one-copy repair
            //                              (an unproven `unique` arg re-owned at
            //                              the call boundary via the zero-token
            //                              copy-on-write path).
            //   ownership_token_repairs  — logical repair events (a re-owned
            //                              boundary, or a var-result reconstructed
            //                              rather than direct-storage-forwarded).
            //   direct_storage_var_accesses — one per accepted direct-storage
            //                              `var` write-back (criterion 6).
            // The runtime already reads these (Vm::boundary_reown_copies etc.);
            // declaring them here flips those readers from None to a real 0 and
            // gives the increment sites a target.
            for counter in [
                "__witchy_boundary_reown_copies",
                "__witchy_ownership_token_repairs",
                "__witchy_direct_storage_var_accesses",
            ] {
                pruned_globals.push(WirGlobal {
                    name: counter.into(),
                    kind: WK::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some(counter.into()),
                });
            }
            if glamour_exports.is_some() {
                if let Some(state_field_kinds) = &glamour_state_field_kinds {
                    for (index, kind) in state_field_kinds.iter().enumerate() {
                        pruned_globals.push(WirGlobal {
                            name: format!("__glamour_state_{index}"),
                            kind: *kind,
                            mutable: true,
                            init: match kind {
                                WK::I64 => GlobalInit::I64(0),
                                WK::F64 => GlobalInit::F64(0.0),
                                WK::I32 => GlobalInit::I32(0),
                                _ => unreachable!("state fields were restricted to scalar kinds"),
                            },
                            export: None,
                        });
                    }
                }
                if glamour_state_rcopy_helper.is_some() {
                    pruned_globals.push(WirGlobal {
                        name: "__glamour_state".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: None,
                    });
                }
                for name in [
                    "__glamour_state_base",
                    "__glamour_output",
                    "__glamour_output_base",
                    "__glamour_live",
                    "__glamour_busy",
                ] {
                    pruned_globals.push(WirGlobal {
                        name: name.into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: None,
                    });
                }
                if glamour_development.is_some_and(|metadata| {
                    metadata.snapshot_format()
                        == GlamourDevelopmentMetadata::AGGREGATE_SNAPSHOT_FORMAT
                }) {
                    pruned_globals.push(WirGlobal {
                        name: "__glamour_snapshot_length".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: None,
                    });
                }
            }
            if cg.uses_diagnostic_sites {
                pruned_globals.push(WirGlobal {
                    name: "__witchy_diagnostic_site".into(),
                    kind: WK::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_diagnostic_site".into()),
                });
            }
            // Region copy-out scratch globals: the watermark / temp base / slide delta
            // the `$rcopy_*` helpers read, and the exported `$__region_copy_bytes`
            // counter. Declared only when a pointer `region:` reclaim is reached.
            if cg.uses_region {
                for (name, ex) in
                    [("rcopy_wm", false), ("rcopy_base", false), ("rcopy_delta", false)]
                {
                    pruned_globals.push(WirGlobal {
                        name: name.into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: if ex { Some(name.into()) } else { None },
                    });
                }
                pruned_globals.push(WirGlobal {
                    name: "__region_copy_bytes".into(),
                    kind: WK::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__region_copy_bytes".into()),
                });
            }
            // (RFC-0032) When `vm.par_map` (scalar or String) is linked, emit + export
            // the `__call_idx` trampoline the host (incl. fresh worker VMs) re-enters to
            // apply the mapped closure to each element by its table index. The String
            // variant also needs `__galloc` so the host can place input strings into a
            // worker's memory — emit it unless string-export wrappers already do.
            // The String/Bytes variants copy buffers in via `__galloc`; all variants
            // invoke the closure by index via `__call_idx`.
            let has_par_map_buf = pruned_funcs.iter().any(|f| f.name == "vm_par_map_bytes");
            // (RFC-0032) `vm.serve` uses the scalar `__call2` trampoline while
            // `vm.with_dir` uses an exact externref+Bytes trampoline. Both copy
            // buffers into a worker via `__galloc`.
            let has_call2 = pruned_funcs
                .iter()
                .any(|f| f.name == "vm_serve");
            let has_with_dir = pruned_funcs.iter().any(|f| f.name == "vm_with_dir");
            let exports_call_idx =
                has_par_map_buf || pruned_funcs.iter().any(|f| f.name == "vm_par_map");
            if exports_call_idx {
                pruned_funcs.push(witchy_wir::wir_helpers::call_idx_helper());
            }
            if has_call2 {
                pruned_funcs.push(witchy_wir::wir_helpers::call2_helper());
            }
            if has_with_dir {
                pruned_funcs.push(witchy_wir::wir_helpers::call_dir_bytes_helper());
            }
            // The String/Bytes par_map variants and `vm.with_dir` all copy a buffer into
            // a worker via `__galloc`.
            let needs_galloc = has_par_map_buf || has_call2 || has_with_dir;
            if needs_galloc && string_exports.is_empty() && glamour_exports.is_none() {
                pruned_funcs.push(witchy_wir::wir_helpers::galloc_helper());
            }
            let mut data: Vec<DataSegment> = cg
                .strings
                .iter()
                .map(|(text, off)| {
                    let mut bytes = (text.len() as u32).to_le_bytes().to_vec();
                    bytes.extend_from_slice(text.as_bytes());
                    DataSegment { offset: *off, bytes }
                })
                .collect();
            data.extend(glamour_development_data);
            let gc_structs = cg.gc_structs.clone();
            let gc_arrays = cg.gc_arrays.clone();
            let layout_roots = cg
                .specialized_type_ids
                .iter()
                .map(|(_, id)| *id)
                .chain(cg.callable_layouts.values().flat_map(|signature| {
                    signature
                        .parameters()
                        .iter()
                        .copied()
                        .flatten()
                        .chain(signature.result())
                }));
            let layout_bundle = witchy_wir::layout::LayoutBundle::from_interner(
                &cg.specialized_layouts,
                layout_roots,
            )
            .map_err(|error| CodegenError {
                message: format!("specialized layout bundle rejected: {error}"),
            })?;
            return Ok((WirModule {
                imports: pruned_imports,
                funcs: pruned_funcs,
                memory_pages: cg.next_offset.div_ceil(64 * 1024).max(1),
                data,
                globals: pruned_globals,
                table: if existential_table_entries.is_empty() && cg.lambda_wir_funcs.is_empty() {
                    if uses_table { Some(WirTable { funcs: Vec::new() }) } else { None }
                } else {
                    // Witness adapters occupy the leading cells. Lambda wrappers
                    // store that offset plus their local index, preserving the
                    // longstanding closure table contract.
                    let mut funcs = existential_table_entries.clone();
                    funcs.extend(cg.lambda_wir_funcs.iter().map(|f| f.name.clone()));
                    Some(WirTable { funcs })
                },
                exports: {
                    let mut exports: Vec<(String, String)> = Vec::new();
                    if has_main {
                        exports.push(("run".into(), "run".into()));
                    }
                    if exports_call_idx {
                        exports.push(("__call_idx".into(), "__call_idx".into()));
                    }
                    if has_call2 {
                        exports.push(("__call2".into(), "__call2".into()));
                    }
                    if has_with_dir {
                        exports.push((
                            "__call_dir_bytes".into(),
                            "__call_dir_bytes".into(),
                        ));
                    }
                    if !string_exports.is_empty() || needs_galloc {
                        exports.push(("__galloc".into(), "__galloc".into()));
                    }
                    if !string_exports.is_empty() {
                        for name in &string_exports {
                            let ex = string_export_name(name);
                            exports.push((ex.clone(), ex));
                        }
                    }
                    if glamour_exports.is_some() {
                        for name in [
                            "__glamour_protocol_version",
                            "__glamour_input_reserve",
                            "__glamour_init",
                            "__glamour_resume",
                            "__glamour_dispatch",
                            "__glamour_output_length",
                            "__glamour_output_release",
                            "__glamour_dispose",
                        ] {
                            exports.push((name.into(), name.into()));
                        }
                        if let Some(development) = glamour_development {
                            for name in [
                                "__glamour_dev_metadata",
                                "__glamour_dev_changes",
                                "__glamour_dev_changes_length",
                            ] {
                                exports.push((name.into(), name.into()));
                            }
                            if development.supports_snapshot() {
                                for name in [
                                    "__glamour_dev_snapshot",
                                    "__glamour_dev_snapshot_length",
                                    "__glamour_dev_restore",
                                ] {
                                    exports.push((name.into(), name.into()));
                                }
                            }
                        }
                    }
                    exports
                },
            }, gc_structs, gc_arrays, layout_bundle));
        }

        // Otherwise the program reaches a prelude helper not yet migrated to a
        // WIR-native form or directly calls an unaccounted host import.
        let mut direct_hosts: Vec<String> = user_host_imports.into_iter().collect();
        direct_hosts.sort();
        let mut unregistered: Vec<String> = called
            .into_iter()
            .filter(|name| {
                helper_names.contains(name.as_str())
                    && witchy_wir::wir_helpers::wir_helper(name).is_none()
            })
            .collect();
        unregistered.sort();
        unsupported(format!(
            "capability-correct WIR assembly is unavailable: direct host imports={direct_hosts:?}, \
             unregistered helpers={unregistered:?}"
        ))
    }
}

/// Collect every function name a `WirSeq` calls directly (`Call{func}`),
/// recursively, and report whether the sequence contains an indirect call.
/// Used by `assemble_wir_module` to find which prelude helpers and runtime
/// declarations a program reaches.
fn collect_called_funcs(
    seq: &[witchy_wir::wir::WirNode],
    out: &mut HashSet<String>,
) -> bool {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};
    fn expr(e: &E, out: &mut HashSet<String>, uses_table: &mut bool) {
        match e {
            E::Call { func, args } => {
                out.insert(func.clone());
                for a in args {
                    expr(a, out, uses_table);
                }
            }
            E::CallHost { args, .. } => {
                for a in args {
                    expr(a, out, uses_table);
                }
            }
            E::CallIndirect { args, index, .. } => {
                *uses_table = true;
                for a in args {
                    expr(a, out, uses_table);
                }
                expr(index, out, uses_table);
            }
            E::ToSlot(i, _)
            | E::FromSlot(i, _)
            | E::Unary { arg: i, .. }
            | E::Convert { arg: i, .. }
            | E::Load { ptr: i, .. }
            | E::Load8U { ptr: i, .. }
            | E::MemoryGrow(i) => expr(i, out, uses_table),
            E::Binary { lhs, rhs, .. } => {
                expr(lhs, out, uses_table);
                expr(rhs, out, uses_table);
            }
            E::Control(n) => node(n, out, uses_table),
            E::Seq(s) => {
                *uses_table |= collect_called_funcs(s, out);
            }
            E::StructNew { args, .. } => {
                for a in args {
                    expr(a, out, uses_table);
                }
            }
            E::ArrayNew { value, len, .. } => {
                expr(value, out, uses_table);
                expr(len, out, uses_table);
            }
            E::ArrayNewFixed { items, .. } => {
                for item in items {
                    expr(item, out, uses_table);
                }
            }
            E::ArrayGet { array, index, .. } => {
                expr(array, out, uses_table);
                expr(index, out, uses_table);
            }
            E::StructGet { base, .. }
            | E::RefCast { value: base, .. }
            | E::RefIsNull(base)
            | E::ArrayLen(base) => expr(base, out, uses_table),
            E::ConstI64(_) | E::ConstF64(_) | E::ConstI32(_) | E::StrPtr(_) | E::MemorySize
            | E::GetLocal(_) | E::GetGlobal(_) | E::RefNull(_) => {}
        }
    }
    fn node(n: &N, out: &mut HashSet<String>, uses_table: &mut bool) {
        match n {
            N::Source { body, .. } => {
                *uses_table |= collect_called_funcs(body, out);
            }
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } => {
                expr(value, out, uses_table)
            }
            N::Store { ptr, value, .. } | N::Store8 { ptr, value, .. } => {
                expr(ptr, out, uses_table);
                expr(value, out, uses_table);
            }
            N::CallStoreMulti { func, args, .. } => {
                out.insert(func.clone());
                for a in args {
                    expr(a, out, uses_table);
                }
            }
            N::CallIndirectStoreMulti { args, index, .. } => {
                *uses_table = true;
                for a in args {
                    expr(a, out, uses_table);
                }
                expr(index, out, uses_table);
            }
            N::MemoryCopy { dest, src, len } => {
                expr(dest, out, uses_table);
                expr(src, out, uses_table);
                expr(len, out, uses_table);
            }
            N::MemoryFill { dest, value, len } => {
                expr(dest, out, uses_table);
                expr(value, out, uses_table);
                expr(len, out, uses_table);
            }
            N::If { cond, then_, els, .. } => {
                expr(cond, out, uses_table);
                *uses_table |= collect_called_funcs(then_, out);
                *uses_table |= collect_called_funcs(els, out);
            }
            N::Block { body, .. } | N::Loop { body, .. } => {
                *uses_table |= collect_called_funcs(body, out);
            }
            N::StructSet { base, value, .. } => {
                expr(base, out, uses_table);
                expr(value, out, uses_table);
            }
            N::ArraySet { array, index, value, .. } => {
                expr(array, out, uses_table);
                expr(index, out, uses_table);
                expr(value, out, uses_table);
            }
            N::Br { cond: Some(c), .. } => expr(c, out, uses_table),
            N::Drop(e) | N::Do(e) | N::Push(e) | N::Return(Some(e)) => {
                expr(e, out, uses_table)
            }
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
    }
    let mut uses_table = false;
    for n in seq {
        node(n, out, &mut uses_table);
    }
    uses_table
}

/// Collect every host import a `WirSeq` calls directly (`CallHost{import}`),
/// recursively. Used by `assemble_wir_module` to detect direct host-authority
/// calls in USER code (e.g. `dir.subdir`, `now`, `recv_*`) — which the pruned
/// path can't account for, so such programs report `Unsupported`. (Helper
/// host calls are accounted for via the registry's `import_deps` instead.)
fn collect_called_host_imports(seq: &[witchy_wir::wir::WirNode], out: &mut HashSet<String>) {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};
    fn expr(e: &E, out: &mut HashSet<String>) {
        match e {
            E::CallHost { import, args } => {
                out.insert(import.clone());
                for a in args {
                    expr(a, out);
                }
            }
            E::Call { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            E::CallIndirect { args, index, .. } => {
                for a in args {
                    expr(a, out);
                }
                expr(index, out);
            }
            E::ToSlot(i, _)
            | E::FromSlot(i, _)
            | E::Unary { arg: i, .. }
            | E::Convert { arg: i, .. }
            | E::Load { ptr: i, .. }
            | E::Load8U { ptr: i, .. }
            | E::MemoryGrow(i) => expr(i, out),
            E::Binary { lhs, rhs, .. } => {
                expr(lhs, out);
                expr(rhs, out);
            }
            E::Control(n) => node(n, out),
            E::Seq(s) => collect_called_host_imports(s, out),
            E::StructNew { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            E::ArrayNew { value, len, .. } => {
                expr(value, out);
                expr(len, out);
            }
            E::ArrayNewFixed { items, .. } => {
                for item in items {
                    expr(item, out);
                }
            }
            E::ArrayGet { array, index, .. } => {
                expr(array, out);
                expr(index, out);
            }
            E::StructGet { base, .. }
            | E::RefCast { value: base, .. }
            | E::RefIsNull(base)
            | E::ArrayLen(base) => expr(base, out),
            E::ConstI64(_) | E::ConstF64(_) | E::ConstI32(_) | E::StrPtr(_) | E::MemorySize
            | E::GetLocal(_) | E::GetGlobal(_) | E::RefNull(_) => {}
        }
    }
    fn node(n: &N, out: &mut HashSet<String>) {
        match n {
            N::Source { body, .. } => collect_called_host_imports(body, out),
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } => expr(value, out),
            N::Store { ptr, value, .. } | N::Store8 { ptr, value, .. } => {
                expr(ptr, out);
                expr(value, out);
            }
            N::CallStoreMulti { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            N::CallIndirectStoreMulti { args, index, .. } => {
                for a in args {
                    expr(a, out);
                }
                expr(index, out);
            }
            N::MemoryCopy { dest, src, len } => {
                expr(dest, out);
                expr(src, out);
                expr(len, out);
            }
            N::MemoryFill { dest, value, len } => {
                expr(dest, out);
                expr(value, out);
                expr(len, out);
            }
            N::If { cond, then_, els, .. } => {
                expr(cond, out);
                collect_called_host_imports(then_, out);
                collect_called_host_imports(els, out);
            }
            N::Block { body, .. } | N::Loop { body, .. } => collect_called_host_imports(body, out),
            N::StructSet { base, value, .. } => {
                expr(base, out);
                expr(value, out);
            }
            N::ArraySet { array, index, value, .. } => {
                expr(array, out);
                expr(index, out);
                expr(value, out);
            }
            N::Br { cond: Some(c), .. } => expr(c, out),
            N::Drop(e) | N::Do(e) | N::Push(e) | N::Return(Some(e)) => expr(e, out),
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
    }
    for n in seq {
        node(n, out);
    }
}

/// Whether this lowered source statement can reach a host import. Helper calls
/// follow the WIR registry's declared dependency graph, the same source of truth
/// module assembly uses to link imports.
pub(super) fn wir_seq_needs_diagnostic_site(seq: &[witchy_wir::wir::WirNode]) -> bool {
    let mut imports = HashSet::new();
    collect_called_host_imports(seq, &mut imports);
    if !imports.is_empty() {
        return true;
    }

    let mut calls = HashSet::new();
    collect_called_funcs(seq, &mut calls);
    let mut seen = HashSet::new();
    calls.iter().any(|name| helper_needs_diagnostic_site(name, &mut seen))
}

const DIAGNOSTIC_SITE_PARAM: &str = "__witchy_diagnostic_site_arg";

fn helper_needs_diagnostic_site(name: &str, seen: &mut HashSet<String>) -> bool {
    if !seen.insert(name.to_string()) {
        return false;
    }
    let Some(spec) = witchy_wir::wir_helpers::wir_helper(name) else {
        return false;
    };
    !spec.import_deps.is_empty()
        || spec.helper_deps.iter().any(|dep| helper_needs_diagnostic_site(dep, seen))
}

fn registered_helper_needs_diagnostic_site(name: &str) -> bool {
    helper_needs_diagnostic_site(name, &mut HashSet::new())
}

fn append_helper_diagnostic_site(
    func: &str,
    args: &mut Vec<witchy_wir::wir::WirExpr>,
    site: &witchy_wir::wir::WirExpr,
) -> bool {
    if !registered_helper_needs_diagnostic_site(func) {
        return false;
    }
    let original_arity = witchy_wir::wir_helpers::wir_helper(func)
        .map(|spec| spec.func.params.len())
        .expect("a registered diagnostic helper must resolve");
    match args.len() {
        n if n == original_arity => args.push(site.clone()),
        n if n == original_arity + 1 => {}
        n => panic!("diagnostic helper `{func}` has {n} arguments; expected {original_arity}"),
    }
    true
}

/// Thread one packed source site into every host-backed helper call and publish
/// it only at the actual host edge. Passing the site as a normal argument is
/// compositional: nested calls evaluate first and cannot leave stale location
/// state for an outer operation.
pub(super) fn attach_diagnostic_sites(seq: &mut witchy_wir::wir::WirSeq, site: i64) -> bool {
    attach_diagnostic_site_expr(seq, &witchy_wir::wir::WirExpr::ConstI64(site))
}

fn attach_diagnostic_site_expr(
    seq: &mut witchy_wir::wir::WirSeq,
    site: &witchy_wir::wir::WirExpr,
) -> bool {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};

    fn expr(e: &mut E, site: &E) -> bool {
        let mut reaches_host = false;
        match e {
            E::Call { func, args } => {
                for arg in args.iter_mut() {
                    reaches_host |= expr(arg, site);
                }
                reaches_host |= append_helper_diagnostic_site(func, args, site);
            }
            E::CallHost { import: _, args } => {
                for arg in args {
                    let _ = expr(arg, site);
                }
                reaches_host = true;
            }
            E::CallIndirect { args, index, .. } => {
                for arg in args {
                    reaches_host |= expr(arg, site);
                }
                reaches_host |= expr(index, site);
            }
            E::ToSlot(inner, _)
            | E::FromSlot(inner, _)
            | E::Unary { arg: inner, .. }
            | E::Convert { arg: inner, .. }
            | E::Load { ptr: inner, .. }
            | E::Load8U { ptr: inner, .. }
            | E::MemoryGrow(inner) => reaches_host |= expr(inner, site),
            E::Binary { lhs, rhs, .. } => {
                reaches_host |= expr(lhs, site);
                reaches_host |= expr(rhs, site);
            }
            E::Control(node) => reaches_host |= node_expr(node, site),
            E::Seq(inner) => reaches_host |= attach_diagnostic_site_expr(inner, site),
            E::StructNew { args, .. } => {
                for arg in args {
                    reaches_host |= expr(arg, site);
                }
            }
            E::ArrayNew { value, len, .. } => {
                reaches_host |= expr(value, site);
                reaches_host |= expr(len, site);
            }
            E::ArrayNewFixed { items, .. } => {
                for item in items {
                    reaches_host |= expr(item, site);
                }
            }
            E::ArrayGet { array, index, .. } => {
                reaches_host |= expr(array, site);
                reaches_host |= expr(index, site);
            }
            E::StructGet { base, .. }
            | E::RefCast { value: base, .. }
            | E::RefIsNull(base)
            | E::ArrayLen(base) => reaches_host |= expr(base, site),
            E::ConstI64(_)
            | E::ConstF64(_)
            | E::ConstI32(_)
            | E::StrPtr(_)
            | E::GetLocal(_)
            | E::GetGlobal(_)
            | E::MemorySize
            | E::RefNull(_) => {}
        }
        reaches_host
    }

    fn node_expr(node: &mut N, site: &E) -> bool {
        let mut reaches_host = false;
        match node {
            N::Source { body, .. } => {
                reaches_host |= attach_diagnostic_site_expr(body, site);
            }
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } => {
                reaches_host |= expr(value, site);
            }
            N::Store { ptr, value, .. } | N::Store8 { ptr, value, .. } => {
                reaches_host |= expr(ptr, site);
                reaches_host |= expr(value, site);
            }
            N::CallStoreMulti { func, args, .. } => {
                for arg in args.iter_mut() {
                    reaches_host |= expr(arg, site);
                }
                reaches_host |= append_helper_diagnostic_site(func, args, site);
            }
            N::CallIndirectStoreMulti { args, index, .. } => {
                for arg in args.iter_mut() {
                    reaches_host |= expr(arg, site);
                }
                reaches_host |= expr(index, site);
            }
            N::MemoryCopy { dest, src, len } => {
                reaches_host |= expr(dest, site);
                reaches_host |= expr(src, site);
                reaches_host |= expr(len, site);
            }
            N::MemoryFill { dest, value, len } => {
                reaches_host |= expr(dest, site);
                reaches_host |= expr(value, site);
                reaches_host |= expr(len, site);
            }
            N::If { cond, then_, els, .. } => {
                reaches_host |= expr(cond, site);
                reaches_host |= attach_diagnostic_site_expr(then_, site);
                reaches_host |= attach_diagnostic_site_expr(els, site);
            }
            N::Block { body, .. } | N::Loop { body, .. } => {
                reaches_host |= attach_diagnostic_site_expr(body, site);
            }
            N::Br { cond: Some(cond), .. } => reaches_host |= expr(cond, site),
            N::Drop(value) | N::Do(value) | N::Push(value) | N::Return(Some(value)) => {
                reaches_host |= expr(value, site);
            }
            N::StructSet { base, value, .. } => {
                reaches_host |= expr(base, site);
                reaches_host |= expr(value, site);
            }
            N::ArraySet { array, index, value, .. } => {
                reaches_host |= expr(array, site);
                reaches_host |= expr(index, site);
                reaches_host |= expr(value, site);
            }
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
        reaches_host
    }

    let mut out = Vec::with_capacity(seq.len());
    let mut reaches_host = false;
    for mut node in std::mem::take(seq) {
        let is_host_edge = matches!(
            &node,
            N::SetLocal { value: E::CallHost { .. }, .. }
                | N::Push(E::CallHost { .. })
                | N::Do(E::CallHost { .. })
                | N::Return(Some(E::CallHost { .. }))
        );
        reaches_host |= node_expr(&mut node, site);
        let already_published = matches!(
            out.last(),
            Some(N::SetGlobal { global, .. }) if global == "__witchy_diagnostic_site"
        );
        if is_host_edge && !already_published {
            out.push(N::SetGlobal {
                global: "__witchy_diagnostic_site".into(),
                value: site.clone(),
            });
        }
        out.push(node);
    }
    *seq = out;
    reaches_host
}

fn prepare_diagnostic_helper(name: &str, func: &mut witchy_wir::wir::WirFunc) {
    use witchy_wir::wir::{WirExpr as E, WirLocal, WirTy};
    if !registered_helper_needs_diagnostic_site(name) {
        return;
    }
    if !func.params.iter().any(|param| param.name == DIAGNOSTIC_SITE_PARAM) {
        func.params.push(WirLocal { name: DIAGNOSTIC_SITE_PARAM.into(), ty: WirTy::Int });
    }
    let reached = attach_diagnostic_site_expr(
        &mut func.body,
        &E::GetLocal(DIAGNOSTIC_SITE_PARAM.into()),
    );
    debug_assert!(reached, "host-backed helper `{name}` has no diagnostic edge");
}

fn prepare_synthetic_diagnostic_sites(func: &mut witchy_wir::wir::WirFunc) -> bool {
    if func.params.iter().any(|param| param.name == DIAGNOSTIC_SITE_PARAM) {
        return false;
    }
    attach_diagnostic_sites(&mut func.body, 0)
}

#[cfg(test)]
mod checked_codegen_boundary_tests {
    use super::*;

    fn no_expand(
        _name: &str,
        _module: &mut witchy_syntax::ast::Module,
        _siblings: &[(String, witchy_syntax::ast::Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    fn authenticated_checked_result(
        source: &str,
    ) -> Result<witchy_types::pipeline::CheckedModule, witchy_types::pipeline::PipelineError> {
        use witchy_types::runtime_type::{
            AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
        };

        let module = witchy_syntax::parser::parse_module(source)
            .expect("parse authenticated checked-codegen fixture");
        let workspace = PackageCoordinate::new(
            PackageSource::Workspace,
            "example/dynamic-test",
            "0.1.0",
        )
        .expect("workspace coordinate");
        let toolchain = PackageCoordinate::new(
            PackageSource::Toolchain,
            "witchy/stdlib",
            "0.1.0",
        )
        .expect("toolchain coordinate");
        let mut assignments = vec![(
            "main".to_string(),
            ModuleLoadIdentity::new(workspace, ["main"]).expect("main owner"),
        )];
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|module| {
            (
                (*module).to_string(),
                ModuleLoadIdentity::new(toolchain.clone(), ["std", *module])
                    .expect("std owner"),
            )
        }));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
            .expect("authenticated owners");
        witchy_types::pipeline::link_checked_authenticated(
            vec![("main".into(), module)],
            "main",
            no_expand,
            owners,
        )
    }

    fn authenticated_checked(source: &str) -> witchy_types::pipeline::CheckedModule {
        authenticated_checked_result(source).expect("authenticated checked link")
    }

    #[test]
    fn authenticated_dynamic_descriptor_reaches_compiled_preparation() {
        let checked = authenticated_checked(
            "import dynamic\n\nfn main() -> Int:\n    let value = dynamic.dynamic(7)\n    0\n",
        );
        assert!(
            matches!(compile_checked_module_binary(&checked), LoweringOutcome::Lowered(_)),
            "authenticated Dynamic construction must lower"
        );
    }

    #[test]
    fn dynamic_construction_rejects_a_transitive_capability_before_lowering() {
        let error = authenticated_checked_result(
            "import dynamic\nimport reflect\n\ntype Holder:\n    Holder(Console)\n\nimpl Reflect for Holder:\n    fn reflect(self) -> reflect.Mirror:\n        reflect.MNil\n\nfn main(console: Console) -> Int:\n    let value = dynamic.dynamic(Holder(console))\n    0\n",
        )
        .expect_err("capability-retaining Dynamic construction must be rejected");
        let message = error.to_string();
        assert!(message.contains("Console"), "{message}");
        assert!(message.contains("Holder[0]"), "{message}");
    }

    #[test]
    fn checked_codegen_uses_the_authenticated_module() {
        let module = witchy_syntax::parser::parse_module("fn main() -> Int:\n    7\n")
            .expect("parse checked-codegen fixture");
        let checked = witchy_types::pipeline::link_checked(
            vec![("main".into(), module)],
            "main",
            no_expand,
        )
        .expect("link and check fixture");

        assert_eq!(
            compile_checked_module_binary(&checked),
            compile_module_binary(checked.module()),
        );
    }

    #[test]
    fn invalid_source_cannot_construct_checked_codegen_input() {
        let module = witchy_syntax::parser::parse_module(
            "fn main() -> Int:\n    \"not an int\"\n",
        )
        .expect("parse invalid checked-codegen fixture");
        let error = witchy_types::pipeline::link_checked(
            vec![("main".into(), module)],
            "main",
            no_expand,
        )
        .expect_err("type-invalid source must stop before checked codegen");

        assert!(error.to_string().contains("expected `Int`"), "{error}");
    }
}

#[cfg(test)]
mod diagnostic_site_tests {
    use super::{
        attach_diagnostic_sites, prepare_diagnostic_helper,
        prepare_synthetic_diagnostic_sites,
        wir_seq_needs_diagnostic_site, DIAGNOSTIC_SITE_PARAM,
    };
    use witchy_wir::wir::{WirExpr as E, WirNode as N};

    #[test]
    fn source_sites_follow_host_and_abort_dependencies() {
        let direct = [N::Do(E::CallHost {
            import: "__witchy_abort".into(),
            args: vec![],
        })];
        let helper = [N::Push(E::Call {
            func: "list_at".into(),
            args: vec![],
        })];
        let host_helper = [N::Push(E::Call {
            func: "net_listen".into(),
            args: vec![],
        })];
        let ordinary = [N::Push(E::Call {
            func: "concat".into(),
            args: vec![],
        })];

        assert!(wir_seq_needs_diagnostic_site(&direct));
        assert!(wir_seq_needs_diagnostic_site(&helper));
        assert!(wir_seq_needs_diagnostic_site(&host_helper));
        assert!(!wir_seq_needs_diagnostic_site(&ordinary));
    }

    #[test]
    fn source_sites_are_arguments_and_publish_only_at_host_edges() {
        let site = 0x1234_i64;
        let mut seq = vec![N::Push(E::Call {
            func: "list_at".into(),
            args: vec![E::ConstI32(8), E::ConstI64(1)],
        })];
        assert!(attach_diagnostic_sites(&mut seq, site));
        let N::Push(E::Call { args, .. }) = &seq[0] else { panic!("list_at call") };
        assert!(matches!(args.last(), Some(E::ConstI64(v)) if *v == site));
        assert!(!seq.iter().any(|node| matches!(node, N::SetGlobal { .. })));

        let mut helper = witchy_wir::wir_helpers::wir_helper("list_at").unwrap().func;
        prepare_diagnostic_helper("list_at", &mut helper);
        assert_eq!(helper.params.last().unwrap().name, DIAGNOSTIC_SITE_PARAM);
        let N::If { then_, .. } = &helper.body[0] else { panic!("list_at guard") };
        assert!(matches!(
            &then_[0],
            N::SetGlobal { global, value: E::GetLocal(local) }
                if global == "__witchy_diagnostic_site" && local == DIAGNOSTIC_SITE_PARAM
        ));

        let mut host_helper = witchy_wir::wir_helpers::wir_helper("net_listen").unwrap().func;
        prepare_diagnostic_helper("net_listen", &mut host_helper);
        assert_eq!(host_helper.params.last().unwrap().name, DIAGNOSTIC_SITE_PARAM);
        assert!(matches!(
            &host_helper.body[..],
            [
                N::SetGlobal { global, value: E::GetLocal(local) },
                N::Push(E::CallHost { import, .. })
            ] if global == "__witchy_diagnostic_site"
                && local == DIAGNOSTIC_SITE_PARAM
                && import == "net_listen"
        ));

        let mut direct = vec![
            N::SetLocal { local: "msg".into(), value: E::ConstI32(12) },
            N::Do(E::CallHost {
                import: "__witchy_abort".into(),
                args: vec![
                    E::ConstI32(5),
                    E::ConstI64(0),
                    E::ConstI64(0),
                    E::GetLocal("msg".into()),
                ],
            }),
        ];
        assert!(attach_diagnostic_sites(&mut direct, site));
        assert!(matches!(&direct[0], N::SetLocal { local, .. } if local == "msg"));
        assert!(matches!(&direct[1], N::SetGlobal { .. }));
        assert!(matches!(&direct[2], N::Do(E::CallHost { .. })));

        // An enclosing block walks the already-instrumented nested sequence
        // again. Its broader site must not overwrite the innermost statement.
        assert!(attach_diagnostic_sites(&mut direct, 0x9999));
        assert_eq!(direct.len(), 3);
        assert!(matches!(
            &direct[1],
            N::SetGlobal { value: E::ConstI64(v), .. } if *v == site
        ));
    }

    #[test]
    fn synthesized_callers_supply_an_unknown_site_to_host_backed_helpers() {
        let mut wrapper = witchy_wir::wir::WirFunc {
            name: "run".into(),
            params: Vec::new(),
            ret: Vec::new(),
            locals: Vec::new(),
            body: vec![N::Push(E::Call { func: "build_args".into(), args: Vec::new() })],
            raw_body: None,
        };
        assert!(prepare_synthetic_diagnostic_sites(&mut wrapper));
        assert!(matches!(
            &wrapper.body[..],
            [N::Push(E::Call { func, args })]
                if func == "build_args" && matches!(&args[..], [E::ConstI64(0)])
        ));
    }
}

#[cfg(test)]
mod table_discovery_tests {
    use super::*;
    use witchy_syntax::parser::parse_module;
    use witchy_wir::wir::{ClosureSignature, Kind, WirExpr as E, WirNode as N};

    #[test]
    fn indirect_expression_without_materialized_function_declares_table() {
        let module = parse_module(
            r#"fn invoke(callback: Option(fn(Int) -> Int)) -> Int:
    match callback:
        Some(f) -> f(41)
        None -> 0

fn main() -> Int:
    invoke(None)
"#,
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("an indirect call in an unreachable match arm still lowers");

        let table = wir.table.expect("CallIndirect requires a declared table");
        assert!(table.funcs.is_empty(), "the program materializes no function value");
    }

    #[test]
    fn indirect_store_multi_without_materialized_function_declares_table() {
        let module = parse_module(
            r#"fn invoke(callback: Option(fn(var Int) -> Int), var value: Int) -> Int:
    match callback:
        Some(f) -> f(value)
        None -> 0

fn main() -> Int:
    var value = 1
    invoke(None, value)
"#,
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("a multi-result indirect call in an unreachable arm still lowers");

        let table = wir.table.expect("CallIndirectStoreMulti requires a declared table");
        assert!(table.funcs.is_empty(), "the program materializes no function value");
    }

    #[test]
    fn recursive_wir_discovery_finds_both_indirect_call_forms() {
        let signature = ClosureSignature {
            params: vec![Kind::I64],
            results: vec![Kind::I64],
        };
        let expression = vec![N::Block {
            label: "nested".into(),
            result: None,
            body: vec![N::Drop(E::Seq(vec![N::Push(E::CallIndirect {
                signature: signature.clone(),
                args: vec![E::ConstI64(1)],
                index: Box::new(E::ConstI32(0)),
            })]))],
        }];
        let store_multi = vec![N::Loop {
            label: "nested".into(),
            body: vec![N::CallIndirectStoreMulti {
                signature,
                args: vec![E::ConstI64(1)],
                index: E::ConstI32(0),
                dests: vec!["result".into()],
            }],
        }];

        assert!(collect_called_funcs(&expression, &mut HashSet::new()));
        assert!(collect_called_funcs(&store_multi, &mut HashSet::new()));
    }
}

#[cfg(test)]
mod lowering_outcome_tests {
    use super::*;
    use witchy_wir::wir::{
        ClosureSignature, Kind, UnOp, WirArrayDef, WirExpr, WirFunc, WirLocal, WirModule,
        WirNode, WirTable, WirTy,
    };

    fn empty_function(name: &str) -> WirFunc {
        WirFunc {
            name: name.into(),
            params: Vec::new(),
            ret: Vec::new(),
            locals: Vec::new(),
            body: Vec::new(),
            raw_body: None,
        }
    }

    fn malformed_export_module() -> WirModule {
        WirModule {
            imports: Vec::new(),
            funcs: Vec::new(),
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: vec![("run".into(), "missing".into())],
        }
    }

    #[test]
    fn malformed_wir_is_rejected_by_module_and_binary_finalizers() {
        let module = malformed_export_module();
        let module_error = validated_module_outcome(module.clone(), &[], &[])
            .expect_rejected("public WIR assembly must reject malformed output");
        assert!(module_error.message.contains("unknown func $missing"));

        let binary_error = encoded_binary_outcome(&module, &[], &[])
            .expect_rejected("public binary assembly must reject malformed output");
        assert!(binary_error.message.contains("unknown func $missing"));
    }

    #[test]
    fn internal_unsupported_failure_stays_distinct_from_rejection() {
        let outcome: LoweringOutcome<()> = public_outcome(unsupported("test coverage miss"));
        let reason = outcome.expect_unsupported("unsupported outcome must be preserved");
        assert_eq!(reason.message, "test coverage miss");
    }

    #[test]
    fn reference_slot_crossing_is_rejected_before_encoder_panics() {
        let module = WirModule {
            imports: Vec::new(),
            funcs: vec![WirFunc {
                name: "run".into(),
                params: Vec::new(),
                ret: Vec::new(),
                locals: Vec::new(),
                body: vec![WirNode::Do(WirExpr::ToSlot(
                    Box::new(WirExpr::RefNull(Kind::ExternRef)),
                    Kind::ExternRef,
                ))],
                raw_body: None,
            }],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        let error = encoded_binary_outcome(&module, &[], &[])
            .expect_rejected("reference values cannot cross the scalar slot ABI");
        assert!(error.message.contains("cannot cross the i64 slot boundary"));
    }

    #[test]
    fn duplicate_names_and_bad_indirect_signatures_are_rejected() {
        let duplicate = WirModule {
            imports: Vec::new(),
            funcs: vec![empty_function("run"), empty_function("run")],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        let duplicate_error = encoded_binary_outcome(&duplicate, &[], &[])
            .expect_rejected("duplicate function identities must not silently retarget calls");
        assert!(duplicate_error.message.contains("duplicate function name $run"));

        let mut run = empty_function("run");
        run.body.push(WirNode::Do(WirExpr::CallIndirect {
            signature: ClosureSignature {
                params: vec![Kind::I32],
                results: Vec::new(),
            },
            args: Vec::new(),
            index: Box::new(WirExpr::ConstI32(0)),
        }));
        let bad_indirect = WirModule {
            imports: Vec::new(),
            funcs: vec![run],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: Some(WirTable { funcs: Vec::new() }),
            exports: vec![("run".into(), "run".into())],
        };
        let indirect_error = encoded_binary_outcome(&bad_indirect, &[], &[])
            .expect_rejected("indirect-call signature mismatch must not reach the encoder");
        assert!(indirect_error.message.contains("signature has 1 parameters"));
    }

    #[test]
    fn invalid_unary_kind_is_a_hard_rejection() {
        let mut run = empty_function("run");
        run.body.push(WirNode::Do(WirExpr::Unary {
            op: UnOp::Not,
            kind: Kind::F64,
            arg: Box::new(WirExpr::ConstI32(0)),
        }));
        let module = WirModule {
            imports: Vec::new(),
            funcs: vec![run],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        let error = encoded_binary_outcome(&module, &[], &[])
            .expect_rejected("operator and kind mismatch must not be returned as lowered");
        assert!(error.message.contains("unary Not on F64"));
    }

    #[test]
    fn production_finalizer_encodes_gc_array_definitions() {
        let module = WirModule {
            imports: Vec::new(),
            funcs: vec![WirFunc {
                name: "run".into(),
                params: Vec::new(),
                ret: Vec::new(),
                locals: vec![WirLocal { name: "items".into(), ty: WirTy::GcRef(0) }],
                body: vec![
                    WirNode::SetLocal {
                        local: "items".into(),
                        value: WirExpr::ArrayNew {
                            array_id: 0,
                            value: Box::new(WirExpr::ConstI64(7)),
                            len: Box::new(WirExpr::ConstI32(2)),
                        },
                    },
                    WirNode::Drop(WirExpr::ArrayLen(Box::new(WirExpr::GetLocal(
                        "items".into(),
                    )))),
                ],
                raw_body: None,
            }],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        let arrays = [WirArrayDef { element: Kind::I64 }];
        let binary = encoded_binary_outcome(&module, &[], &arrays)
            .expect_lowered("production finalization must retain GC array definitions");
        wasmparser::validate(&binary).expect("the finalized module is valid Wasm GC");
    }
}

/// Compile a rune's build step to a WASM binary that runs in the zero-ambient
/// build sandbox. The `build` entrypoint is renamed to `main` so the whole
/// `compile_module_binary` pipeline (the `run` export, marshaling, helpers) is
/// reused verbatim. The only build-specific code is the `write_out`/`read_build`
/// host calls (the `build_out_write`/`build_read` WIR helpers), which never
/// appear in an ordinary program (so parity is untouched). The host links only
/// `build_out_write`/`build_read_len`, confined to the granted output sandbox
/// and read roots — nothing else exists for the guest to call.
#[cfg(any(test, feature = "raw-module-test-api"))]
pub fn compile_build_module(module: &Module) -> LoweringOutcome<Vec<u8>> {
    compile_build_module_mode(module, None)
}

/// Compile a build module that crossed the canonical linked type-check boundary.
pub fn compile_checked_build_module(
    checked: &witchy_types::pipeline::CheckedModule,
) -> LoweringOutcome<Vec<u8>> {
    let runtime_catalog = checked.runtime_declaration_catalog().ok();
    compile_build_module_mode(checked.module(), runtime_catalog.as_ref())
}

fn compile_build_module_mode(
    module: &Module,
    runtime_catalog: Option<&witchy_types::runtime_type::RuntimeDeclarationCatalog>,
) -> LoweringOutcome<Vec<u8>> {
    let mut m = module.clone();
    // A build module ships no `main`; promote its `build` entrypoint to `main`.
    m.items.retain(|it| !matches!(it, Item::Function(f) if f.name == "main"));
    for item in &mut m.items {
        if let Item::Function(f) = item {
            if f.name.rsplit('.').next() == Some("build") {
                f.name = "main".to_string();
            }
        }
    }
    compile_module_binary_mode(&m, true, runtime_catalog, None)
}
