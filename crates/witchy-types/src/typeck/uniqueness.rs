//! Duplicate-declaration / uniqueness checks.
//!
//! A self-contained cluster of pre-lowering validation passes extracted
//! verbatim from the main checker: reject two top-level functions, two
//! declarations (const/type/alias/constructor/impl/method), or two parameters
//! that share a name. None touch checker state — the parent module drives them
//! from `check_with_compiler_syntax` / `run_check_selected` — so they live here
//! to keep the checker file focused on inference.

use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

use witchy_syntax::ast::{self, Block, Expr, Function, Item, Module, Stmt};

use super::{terr, TypeError};

pub(super) struct UniquenessError {
    pub(super) error: TypeError,
    pub(super) item_index: usize,
}

impl UniquenessError {
    fn new(item_index: usize, error: TypeError) -> Self {
        Self { error, item_index }
    }

    pub(super) fn into_type_error(self) -> TypeError {
        self.error
    }
}

fn unique_err<T>(
    item_index: usize,
    message: impl Into<String>,
) -> Result<T, UniquenessError> {
    Err(UniquenessError {
        error: TypeError {
            message: message.into(),
        },
        item_index,
    })
}

/// Reject two top-level functions with the same name. Witchy has no
/// free-function overloading — a second definition silently overwrites the first
/// (in both the linker's and the checker's name tables), so the duplicate is
/// always a bug (a typo or a copy/paste). Methods live in `impl` blocks and are
/// dispatched by receiver type, so they are not affected. Names may be
/// module-qualified (`main.f`) after linking; the message shows the bare name.
pub(super) fn check_unique_functions(module: &Module) -> Result<(), UniquenessError> {
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for (idx, item) in module.items.iter().enumerate() {
        if let Item::Function(f) = item {
            let line = module.item_lines.get(idx).copied().unwrap_or(0);
            if let Some(&first) = seen.get(f.name.as_str()) {
                let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                let where_ = if first != 0 && line != 0 {
                    format!(" (lines {first} and {line})")
                } else {
                    String::new()
                };
                return unique_err(idx, format!(
                    "function `{bare}` is defined more than once{where_}; \
                     top-level function names must be unique"
                ));
            }
            seen.insert(f.name.as_str(), line);
        }
    }
    Ok(())
}

/// (BUG-230) Reject duplicate top-level declarations in the const/type/constructor/
/// method namespaces — the same "defined more than once" quality of error the
/// function namespace already gets from [`check_unique_functions`]. Runs
/// pre-lowering (while `const`/`alias`/`impl`/`type` items are still distinct) on
/// the merged module, whose type and constructor names are already
/// module-qualified, so a genuine cross-module name is distinct and only
/// same-module duplicates (a typo or copy-paste) collide.
pub(super) fn check_unique_declarations(
    module: &Module,
) -> Result<(), UniquenessError> {
    let bare = |n: &str| n.rsplit('.').next().unwrap_or(n).to_string();
    let impl_trait_display = |name: &str, args: &[String]| {
        let base = bare(name);
        if args.is_empty() {
            base
        } else {
            format!("{base}({})", args.join(", "))
        }
    };
    let impl_target_display = |name: &str, args: &[String]| {
        let base = bare(name);
        if args.is_empty() {
            base
        } else {
            format!("{base}({})", args.join(", "))
        }
    };
    let line_suffix = |first: u32, second: u32| {
        if first != 0 && second != 0 {
            format!(" (lines {first} and {second})")
        } else {
            String::new()
        }
    };
    let check_type_params = |context: String, params: &[String]| -> Result<(), TypeError> {
        let mut seen: HashSet<&str> = HashSet::new();
        for param in params {
            if !seen.insert(param.as_str()) {
                return terr(format!(
                    "type parameter `{param}` is declared more than once in {context}; \
                     type parameter names must be unique"
                ));
            }
        }
        Ok(())
    };
    let check_fields = |t: &witchy_syntax::ast::TypeDef, v: &witchy_syntax::ast::Variant| -> Result<(), TypeError> {
        let mut seen: HashSet<&str> = HashSet::new();
        for field in &v.field_names {
            if !seen.insert(field.as_str()) {
                let noun = if t.is_capability { "capability" } else { "type" };
                return terr(format!(
                    "field `{field}` is declared more than once in {noun} `{}`; \
                     record field names must be unique",
                    bare(&t.name)
                ));
            }
        }
        Ok(())
    };
    let mut consts: HashMap<String, u32> = HashMap::new();
    let mut aliases: HashMap<String, u32> = HashMap::new();
    let mut types: HashMap<String, &witchy_syntax::ast::TypeDef> = HashMap::new();
    // Constructor name -> its owning type, so a cross-type duplicate names both.
    let mut ctors: HashMap<String, String> = HashMap::new();
    for (idx, item) in module.items.iter().enumerate() {
        let line = module.item_lines.get(idx).copied().unwrap_or(0);
        match item {
            Item::Const { name, .. } => {
                if let Some(first) = consts.insert(name.clone(), line) {
                    return unique_err(idx, format!(
                        "constant `{}` is defined more than once{}; \
                         top-level constant names must be unique",
                        bare(name),
                        line_suffix(first, line)
                    ));
                }
            }
            Item::TypeAlias { name, params, ty } => {
                check_type_params(format!("type alias `{}`", bare(name)), params)
                    .map_err(|error| UniquenessError::new(idx, error))?;
                let used_params = ast::effective_type_params(&[], std::iter::once(ty));
                for param in used_params {
                    if !params.contains(&param) {
                        return unique_err(idx, format!(
                            "type alias `{}` uses type parameter `{param}` but does not declare it; \
                             declare it in the alias head, e.g. `type {}({param}) = ...`",
                            bare(name),
                            bare(name)
                        ));
                    }
                }
                if types.contains_key(name) {
                    return unique_err(idx, format!(
                        "type alias `{}` conflicts with type `{}`; \
                         top-level type names must be unique",
                        bare(name),
                        bare(name)
                    ));
                }
                if let Some(first) = aliases.insert(name.clone(), line) {
                    return unique_err(idx, format!(
                        "type alias `{}` is defined more than once{}; \
                         top-level type names must be unique",
                        bare(name),
                        line_suffix(first, line)
                    ));
                }
            }
            Item::Type(t) => {
                check_type_params(format!("type `{}`", bare(&t.name)), &t.params)
                    .map_err(|error| UniquenessError::new(idx, error))?;
                for v in &t.variants {
                    check_fields(t, v)
                        .map_err(|error| UniquenessError::new(idx, error))?;
                }
                if aliases.contains_key(&t.name) {
                    return unique_err(idx, format!(
                        "type `{}` conflicts with type alias `{}`; \
                         top-level type names must be unique",
                        bare(&t.name),
                        bare(&t.name)
                    ));
                }
                if types.insert(t.name.clone(), t).is_some() {
                    return unique_err(idx, format!(
                        "type `{}` is defined more than once; top-level type names must be unique",
                        bare(&t.name)
                    ));
                }
                for v in &t.variants {
                    if let Some(prev) = ctors.insert(v.name.clone(), t.name.clone()) {
                        let (a, b) = (bare(&prev), bare(&t.name));
                        let where_ = if a == b {
                            format!("in type `{a}`")
                        } else {
                            format!("in types `{a}` and `{b}`")
                        };
                        return unique_err(idx, format!(
                            "constructor `{}` is defined more than once ({where_}); \
                             constructor names must be unique",
                            bare(&v.name)
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    // Methods: no two methods with the same name in one `impl` block or `trait`,
    // and no duplicate inherent method (same receiver type, same name) across the
    // inherent `impl` blocks of a type.
    let mut inherent: HashSet<(String, String)> = HashSet::new();
    let mut trait_impls: HashSet<(String, Vec<String>, String, Vec<String>)> = HashSet::new();
    for (idx, item) in module.items.iter().enumerate() {
        match item {
            Item::Impl(im) => {
                if let Some(trait_name) = &im.trait_name {
                    let trait_args = im
                        .trait_args
                        .iter()
                        .map(witchy_syntax::format::type_str)
                        .collect::<Vec<_>>();
                    let target_args = im
                        .target_args
                        .iter()
                        .map(witchy_syntax::format::type_str)
                        .collect::<Vec<_>>();
                    if !trait_impls.insert((
                        trait_name.clone(),
                        trait_args.clone(),
                        im.type_name.clone(),
                        target_args.clone(),
                    )) {
                        return unique_err(idx, format!(
                            "impl `{}` for `{}` is defined more than once; \
                             trait impl heads must be unique",
                            impl_trait_display(trait_name, &trait_args),
                            impl_target_display(&im.type_name, &target_args)
                        ));
                    }
                }
                let mut here: HashSet<String> = HashSet::new();
                for m in &im.methods {
                    let name = bare(&m.name);
                    if !here.insert(name.clone()) {
                        return unique_err(idx, format!(
                            "method `{name}` is defined more than once in `impl {}`; \
                             method names must be unique within an impl",
                            im.trait_name.as_deref().map_or_else(
                                || im.type_name.clone(),
                                |tr| format!("{tr} for {}", im.type_name)
                            )
                        ));
                    }
                    // Inherent (trait-free) methods share one namespace per type.
                    if im.trait_name.is_none() && !inherent.insert((im.type_name.clone(), name.clone())) {
                        return unique_err(idx, format!(
                            "inherent method `{name}` is defined more than once on `{}`; \
                             method names must be unique per receiver type",
                            bare(&im.type_name)
                        ));
                    }
                }
            }
            Item::Trait(tr) => {
                check_type_params(format!("trait `{}`", bare(&tr.name)), &tr.typarams)
                    .map_err(|error| UniquenessError::new(idx, error))?;
                let mut here: HashSet<&str> = HashSet::new();
                for m in &tr.methods {
                    if !here.insert(m.name.as_str()) {
                        return unique_err(idx, format!(
                            "method `{}` is declared more than once in `trait {}`; \
                             trait method names must be unique",
                            m.name, tr.name
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Reject duplicate parameter names in every source and lowered callable. The
/// checker scopes parameters in a name map, so accepting duplicates would make a
/// later parameter silently hide an earlier one and would also make keyword labels
/// ambiguous at direct call sites.
pub(super) fn check_unique_parameters(
    module: &Module,
) -> Result<(), UniquenessError> {
    fn bare(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }

    fn check_params(context: String, params: &[ast::Param]) -> Result<(), TypeError> {
        let mut seen: HashSet<&str> = HashSet::new();
        for param in params {
            if !seen.insert(param.name.as_str()) {
                return terr(format!(
                    "parameter `{}` is declared more than once in {context}; \
                     parameter names must be unique",
                    param.name
                ));
            }
        }
        for param in params {
            if let Some(default) = &param.default {
                check_expr(default)?;
            }
        }
        Ok(())
    }

    fn check_function(context: &str, f: &Function) -> Result<(), TypeError> {
        check_params(format!("{context} `{}`", bare(&f.name)), &f.params)?;
        check_block(&f.body)
    }

    fn check_block(block: &Block) -> Result<(), TypeError> {
        for stmt in &block.stmts {
            check_stmt(stmt)?;
        }
        Ok(())
    }

    fn check_stmt(stmt: &Stmt) -> Result<(), TypeError> {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => check_expr(value),
            Stmt::Return(Some(value)) => check_expr(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
        }
    }

    fn check_expr(expr: &Expr) -> Result<(), TypeError> {
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => Ok(()),
            Expr::List(values) | Expr::Tuple(values) => {
                for value in values {
                    check_expr(value)?;
                }
                Ok(())
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. } => {
                for arg in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::LabeledCall { args, .. } => {
                for (_, arg) in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::MethodCall { receiver, args, .. } => {
                check_expr(receiver)?;
                for arg in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                check_expr(receiver)?;
                for arg in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::Apply { func, args } => {
                check_expr(func)?;
                for arg in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::Unary { expr, .. }
            | Expr::Field { base: expr, .. }
            | Expr::Try(expr)
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::As { expr, .. } => check_expr(expr),
            Expr::Lambda { params, body, .. } => {
                check_params("lambda".to_string(), params)?;
                check_block(body)
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                check_expr(base)?;
                for (_, value) in fields {
                    check_expr(value)?;
                }
                Ok(())
            }
            Expr::Record { fields, spread, .. } => {
                for (_, value) in fields {
                    check_expr(value)?;
                }
                if let Some(base) = spread {
                    check_expr(base)?;
                }
                Ok(())
            }
            Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } => {
                check_expr(lhs)?;
                check_expr(rhs)
            }
            Expr::If { cond, then_block, else_block } => {
                check_expr(cond)?;
                check_block(then_block)?;
                if let Some(block) = else_block {
                    check_block(block)?;
                }
                Ok(())
            }
            Expr::Match { scrutinee, arms } => {
                check_expr(scrutinee)?;
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        check_expr(guard)?;
                    }
                    check_expr(&arm.body)?;
                }
                Ok(())
            }
            Expr::Block(block) => check_block(block),
            Expr::While { cond, body } => {
                check_expr(cond)?;
                check_block(body)
            }
            Expr::For { iter, body, .. } => {
                check_expr(iter)?;
                check_block(body)
            }
            Expr::Index { base, index } => {
                check_expr(base)?;
                check_expr(index)
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                check_expr(scrutinee)?;
                check_block(body)
            }
        }
    }

    for (idx, item) in module.items.iter().enumerate() {
        let result = (|| -> Result<(), TypeError> {
            match item {
                Item::Function(f) => check_function("function", f),
                Item::Impl(im) => {
                    for method in &im.methods {
                        check_function("method", method)?;
                    }
                    Ok(())
                }
                Item::Trait(tr) => {
                    for method in &tr.methods {
                        check_params(
                            format!("trait method `{}`", method.name),
                            &method.params,
                        )?;
                        if let Some(default) = &method.default {
                            check_block(default)?;
                        }
                    }
                    Ok(())
                }
                Item::Const { value, .. } => check_expr(value),
                Item::Comptime(block) => check_block(block),
                Item::Type(_) | Item::TypeAlias { .. } => Ok(()),
            }
        })();
        result.map_err(|error| UniquenessError::new(idx, error))?;
    }
    Ok(())
}
