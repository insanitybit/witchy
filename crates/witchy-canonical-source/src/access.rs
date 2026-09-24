//! Canonical ownership/access contracts for checked callable types.
//!
//! This module is the representation boundary introduced by RFC-0110 stage 1.
//! It deliberately derives contracts from checked [`Type`] and [`Convention`]
//! values. Later call-shape and lowering work consumes this representation
//! instead of rediscovering ownership from syntax, rendered names, or individual
//! container operations. The checker's access-flow verifier, which needs the
//! typing tables, stays in `witchy_types::access` (RFC-0170).

use std::fmt;

// foldhash (not SipHash): all keys are compiler-internal names/ids, never
// attacker-controlled.
use foldhash::{HashMap, HashMapExt as _};

use witchy_cap_model::CapabilityKind;
use witchy_syntax::ast::{
    Block, CallableQualifiers, Convention, Expr, Function, Item, Module, Stmt, Type, TypeDef,
    TypeQual, effective_nominal_type_def_params,
};
use witchy_syntax::intrinsics;

/// The source-level access granted to one callable parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    /// A bare parameter: an owned value which is immutable in the callee.
    OwnedImmutable,
    /// An explicit `let` parameter: call-scoped shared access.
    SharedBorrow,
    /// A `var` parameter: exclusive move-in/write-back access.
    ExclusiveWriteback,
    /// An `own` parameter: consuming ownership transfer.
    Consuming,
}

impl From<Convention> for AccessKind {
    fn from(convention: Convention) -> Self {
        match convention {
            Convention::Let => Self::OwnedImmutable,
            Convention::Borrow => Self::SharedBorrow,
            Convention::Var => Self::ExclusiveWriteback,
            Convention::Own => Self::Consuming,
        }
    }
}

/// A checked qualifier in its declared outer-to-inner order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessQualifier {
    Frozen,
    Unique,
    /// RFC-0167 rooted immutable snapshot carrier.  Unlike `Borrow`, it has
    /// no lifetime relation and cannot become an exclusive write-back lane.
    View,
    Borrow(String),
    BorrowMut(String),
}

impl From<&TypeQual> for AccessQualifier {
    fn from(qualifier: &TypeQual) -> Self {
        match qualifier {
            TypeQual::Frozen => Self::Frozen,
            TypeQual::Unique => Self::Unique,
            TypeQual::View => Self::View,
            TypeQual::Borrow(lifetime) => Self::Borrow(lifetime.clone()),
            TypeQual::LegacyBorrow(lifetime) => Self::Borrow(lifetime.clone()),
            TypeQual::BorrowMut(lifetime) => Self::BorrowMut(lifetime.clone()),
        }
    }
}

/// One statically checked step from an owner root to borrowed storage.
///
/// This representation is shared by access signatures and loan facts so a
/// callable cannot preserve lifetime names while erasing the field, tuple, or
/// fixed-range relation those lifetimes govern.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LoanProjectionStep {
    Field(String),
    Tuple(usize),
    Index(i64),
    /// One element of a homogeneous list. This is a signature-level path, not
    /// a runtime offset: an actual `values[index]` resolves it to that element
    /// without projecting through the owner that supplied the view.
    AnyIndex,
    Range { lo: i64, hi: i64, inclusive: bool },
}

/// A fixed projection relative to an owning root. Empty means the whole value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct LoanProjection {
    pub steps: Vec<LoanProjectionStep>,
}

/// One checked projection from a mutable caller binding to a `var` place.
///
/// A dynamic index remains explicit instead of being treated as a fixed path:
/// overlap and ownership consumers must fail closed when its coordinate is not
/// statically known.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CheckedPlaceStep {
    Field(String),
    Index(i64),
    /// A literal slice projection. Dynamic range endpoints are retained in a
    /// `CheckedViewFact`; this structural path never re-reads source syntax.
    Range { lo: i64, hi: i64, inclusive: bool },
    DynamicIndex,
}

/// The canonical checked identity of an assignable caller place.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CheckedPlace {
    root: String,
    steps: Vec<CheckedPlaceStep>,
}

impl CheckedPlace {
    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn steps(&self) -> &[CheckedPlaceStep] {
        &self.steps
    }

    pub fn has_dynamic_index(&self) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step, CheckedPlaceStep::DynamicIndex))
    }

    /// Whether two simultaneously reserved write-back places may designate
    /// common storage. A dynamic or unlike projection pair fails closed.
    pub fn overlaps(&self, other: &Self) -> bool {
        if self.root != other.root {
            return false;
        }
        for (left, right) in self.steps.iter().zip(&other.steps) {
            if left == right {
                continue;
            }
            return match (left, right) {
                (CheckedPlaceStep::Field(left), CheckedPlaceStep::Field(right)) => left == right,
                (CheckedPlaceStep::Index(left), CheckedPlaceStep::Index(right)) => left == right,
                _ => true,
            };
        }
        true
    }
}

/// Derive a place through the syntax forms authenticated by type checking.
/// Consumers outside type checking query the finalized [`CheckedAccessFacts`]
/// table instead of rediscovering this path from the AST.
pub fn checked_place(expression: &Expr) -> Option<CheckedPlace> {
    checked_place_from(expression, None)
}

/// RFC-0167 view formation through a view: the place's base may be exactly
/// one dereference of a named binding (`&(*b).f`, `&(*v)[i]`).  The returned
/// root is that binding, whose rooted carrier retains the original root; the
/// flag says the base was dereferenced.  This is never an assignable place,
/// so write-back consumers keep using [`checked_place`].
pub fn checked_view_place(expression: &Expr) -> Option<(CheckedPlace, bool)> {
    let mut through_view = false;
    let place = checked_place_from(expression, Some(&mut through_view))?;
    Some((place, through_view))
}

fn checked_place_from(expression: &Expr, mut view_base: Option<&mut bool>) -> Option<CheckedPlace> {
    fn walk(
        expression: &Expr,
        steps: &mut Vec<CheckedPlaceStep>,
        view_base: &mut Option<&mut bool>,
    ) -> Option<String> {
        match expression {
            Expr::Var(root) => Some(root.clone()),
            Expr::Unary { op: witchy_syntax::ast::UnOp::Deref, expr } if view_base.is_some() => {
                let Expr::Var(root) = expr.as_ref() else { return None };
                if let Some(through) = view_base.as_deref_mut() {
                    *through = true;
                }
                Some(root.clone())
            }
            Expr::Field { base, field } => {
                let root = walk(base, steps, view_base)?;
                steps.push(CheckedPlaceStep::Field(field.clone()));
                Some(root)
            }
            Expr::Index { base, index } => {
                let root = walk(base, steps, view_base)?;
                steps.push(match index.as_ref() {
                    Expr::Int(value) => CheckedPlaceStep::Index(*value),
                    Expr::Range { lo, hi, inclusive }
                        if matches!(lo.as_ref(), Expr::Int(_)) && matches!(hi.as_ref(), Expr::Int(_)) =>
                    {
                        let Expr::Int(lo) = lo.as_ref() else { unreachable!() };
                        let Expr::Int(hi) = hi.as_ref() else { unreachable!() };
                        CheckedPlaceStep::Range { lo: *lo, hi: *hi, inclusive: *inclusive }
                    }
                    _ => CheckedPlaceStep::DynamicIndex,
                });
                Some(root)
            }
            Expr::Call { name, args }
                if matches!(name.as_str(), intrinsics::LIST_AT | intrinsics::DICT_AT)
                    && args.len() == 2 =>
            {
                let root = walk(&args[0], steps, view_base)?;
                steps.push(match &args[1] {
                    Expr::Int(value) => CheckedPlaceStep::Index(*value),
                    _ => CheckedPlaceStep::DynamicIndex,
                });
                Some(root)
            }
            _ => None,
        }
    }

    let mut steps = Vec::new();
    let root = walk(expression, &mut steps, &mut view_base)?;
    Some(CheckedPlace { root, steps })
}

/// Recover the destination preserved by [`Stmt::Assign::place_depth`] after
/// RFC-0022 desugaring. This is deliberately narrower than recognizing an
/// arbitrary right-hand-side shape: each level must be one canonical rebuild
/// emitted by `desugar_place_assign`, otherwise the root-only place fails
/// closed for overlap checks.
pub fn checked_assignment_place(
    root: &str,
    value: &Expr,
    place_depth: u32,
) -> CheckedPlace {
    let mut steps = Vec::new();
    let mut current = value;
    for _ in 0..place_depth {
        match current {
            Expr::RecordUpdate { fields, .. } if fields.len() == 1 => {
                steps.push(CheckedPlaceStep::Field(fields[0].0.clone()));
                current = &fields[0].1;
            }
            Expr::Call { name, args }
                if name == intrinsics::LIST_SET_AT && args.len() == 3 =>
            {
                steps.push(match &args[1] {
                    Expr::Int(value) => CheckedPlaceStep::Index(*value),
                    _ => CheckedPlaceStep::DynamicIndex,
                });
                current = &args[2];
            }
            Expr::Call { name, args }
                if name == intrinsics::DICT_INSERT && args.len() == 3 =>
            {
                // A dictionary key is never a statically disjoint place, but
                // the enclosing value projection remains authenticated.  The
                // caller can therefore continue checking a nested field for a
                // frozen contract; loan overlap still treats this as dynamic.
                steps.push(CheckedPlaceStep::DynamicIndex);
                current = &args[2];
            }
            Expr::MethodCall { method, args, .. }
                if method == "__set_at" && args.len() == 2 =>
            {
                steps.push(match &args[0] {
                    Expr::Int(value) => CheckedPlaceStep::Index(*value),
                    _ => CheckedPlaceStep::DynamicIndex,
                });
                current = &args[1];
            }
            _ => {
                steps.clear();
                break;
            }
        }
    }
    CheckedPlace { root: root.to_string(), steps }
}

/// Canonical assignable-place facts keyed to one exact module allocation.
///
/// The producer recognizes only the place forms accepted by type checking. A
/// later access-verification error does not erase these identities, allowing
/// fail-closed diagnostic consumers to retain roots without re-parsing syntax.
#[derive(Debug)]
pub struct CheckedPlaceFacts {
    owner: *const Module,
    places: HashMap<usize, CheckedPlace>,
}

impl CheckedPlaceFacts {
    fn owns(&self, module: &Module) -> bool {
        std::ptr::eq(self.owner, module)
    }

    pub fn place_at(&self, module: &Module, expression: &Expr) -> Option<&CheckedPlace> {
        if !self.owns(module) {
            return None;
        }
        self.places.get(&(expression as *const Expr as usize))
    }
}

/// Publish assignable-place facts for one exact module. Full callable-access
/// verification embeds this authority, while diagnostic-only consumers may
/// retain it independently when another access contract rejects.
pub fn checked_place_facts(module: &Module) -> CheckedPlaceFacts {
    let mut places = HashMap::new();
    visit_place_module(module, &mut |expression| {
        if let Some(place) = checked_place(expression) {
            places.insert(expression as *const Expr as usize, place);
        }
    });
    CheckedPlaceFacts { owner: std::ptr::from_ref(module), places }
}

fn visit_place_module(module: &Module, visitor: &mut impl FnMut(&Expr)) {
    for item in &module.items {
        match item {
            Item::Function(function) => visit_place_block(&function.body, visitor),
            Item::Trait(definition) => {
                for method in &definition.methods {
                    if let Some(default) = &method.default {
                        visit_place_block(default, visitor);
                    }
                }
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    visit_place_block(&method.body, visitor);
                }
            }
            Item::Const { value, .. } => visit_place_expr(value, visitor),
            Item::Comptime(block) => visit_place_block(block, visitor),
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
}

fn visit_place_block(block: &Block, visitor: &mut impl FnMut(&Expr)) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => visit_place_expr(value, visitor),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn visit_place_expr(expression: &Expr, visitor: &mut impl FnMut(&Expr)) {
    visitor(expression);
    match expression {
        Expr::List(items)
        | Expr::Tuple(items)
        | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                visit_place_expr(item, visitor);
            }
        }
        Expr::Call { args, .. } | Expr::MethodCall { args, .. } => {
            if let Expr::MethodCall { receiver, .. } = expression {
                visit_place_expr(receiver, visitor);
            }
            for argument in args {
                visit_place_expr(argument, visitor);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_place_expr(receiver, visitor);
            for (_, argument) in args {
                visit_place_expr(argument, visitor);
            }
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            visit_place_expr(receiver, visitor);
            for argument in args {
                visit_place_expr(argument, visitor);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                visit_place_expr(argument, visitor);
            }
        }
        Expr::Apply { func, args } => {
            visit_place_expr(func, visitor);
            for argument in args {
                visit_place_expr(argument, visitor);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => visit_place_expr(expr, visitor),
        Expr::Lambda { body, .. } | Expr::Block(body) => visit_place_block(body, visitor),
        Expr::RecordUpdate { base, fields, .. } => {
            visit_place_expr(base, visitor);
            for (_, value) in fields {
                visit_place_expr(value, visitor);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_place_expr(value, visitor);
            }
            if let Some(spread) = spread {
                visit_place_expr(spread, visitor);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit_place_expr(lhs, visitor);
            visit_place_expr(rhs, visitor);
        }
        Expr::If { cond, then_block, else_block } => {
            visit_place_expr(cond, visitor);
            visit_place_block(then_block, visitor);
            if let Some(else_block) = else_block {
                visit_place_block(else_block, visitor);
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_place_expr(scrutinee, visitor);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_place_expr(guard, visitor);
                }
                visit_place_expr(&arm.body, visitor);
            }
        }
        Expr::While { cond, body } => {
            visit_place_expr(cond, visitor);
            visit_place_block(body, visitor);
        }
        Expr::For { iter, body, .. } => {
            visit_place_expr(iter, visitor);
            visit_place_block(body, visitor);
        }
        Expr::Range { lo, hi, .. } => {
            visit_place_expr(lo, visitor);
            visit_place_expr(hi, visitor);
        }
        Expr::Index { base, index } => {
            visit_place_expr(base, visitor);
            visit_place_expr(index, visitor);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            visit_place_expr(scrutinee, visitor);
            visit_place_block(body, visitor);
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
}

impl LoanProjection {
    pub fn extended(&self, suffix: &Self) -> Self {
        let mut steps = self.steps.clone();
        steps.extend(suffix.steps.iter().cloned());
        Self { steps }
    }

    pub fn prefixed(&self, step: LoanProjectionStep) -> Self {
        let mut steps = Vec::with_capacity(self.steps.len() + 1);
        steps.push(step);
        steps.extend(self.steps.iter().cloned());
        Self { steps }
    }
}

/// The capability carried by one lifetime-bearing reference slot.
///
/// This is deliberately part of the callable relation rather than inferred
/// from a storage type: a shared result may be derived from an exclusive input,
/// but an exclusive result may only be derived from exclusive input storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorrowKind {
    Shared,
    Exclusive,
}

impl BorrowKind {
    fn can_supply(self, requested: Self) -> bool {
        matches!(
            (self, requested),
            (Self::Shared, Self::Shared)
                | (Self::Exclusive, Self::Shared | Self::Exclusive)
        )
    }
}

#[derive(Clone)]
pub struct BorrowSlot {
    pub lifetime: String,
    pub kind: BorrowKind,
    pub projection: LoanProjection,
    pub storage_type: Type,
}

/// Nominal declarations needed to derive exact borrowed slots from checked
/// types. The catalog is the single relation authority shared by access-flow
/// verification and the loan checker.
#[derive(Clone, Default)]
pub struct BorrowRelationCatalog {
    definitions: HashMap<String, TypeDef>,
    constructors: HashMap<String, String>,
}

impl BorrowRelationCatalog {
    pub fn from_module(module: &Module) -> Self {
        let mut catalog = Self::default();
        for item in &module.items {
            let Item::Type(definition) = item else { continue };
            catalog
                .definitions
                .insert(definition.name.clone(), definition.clone());
            for variant in &definition.variants {
                catalog
                    .constructors
                    .insert(variant.name.clone(), definition.name.clone());
            }
        }
        catalog
    }

    pub fn slots(&self, ty: &Type) -> Vec<BorrowSlot> {
        self.slots_with(ty, &HashMap::new(), &HashMap::new(), &mut Vec::new())
    }

    fn borrow_lifetimes(&self, ty: &Type) -> Vec<String> {
        slot_lifetimes(&self.slots(ty))
    }

    fn slots_with(
        &self,
        ty: &Type,
        lifetimes: &HashMap<String, String>,
        types: &HashMap<String, Type>,
        active_nominals: &mut Vec<(String, Type)>,
    ) -> Vec<BorrowSlot> {
        match ty {
            Type::Qualified(TypeQual::Borrow(lifetime), inner)
            | Type::Qualified(TypeQual::LegacyBorrow(lifetime), inner)
            | Type::Qualified(TypeQual::BorrowMut(lifetime), inner) => {
                let nested = self.slots_with(inner, lifetimes, types, active_nominals);
                if !nested.is_empty() {
                    nested
                } else {
                    vec![BorrowSlot {
                        lifetime: lifetimes
                            .get(lifetime)
                            .cloned()
                            .unwrap_or_else(|| lifetime.clone()),
                        kind: if matches!(ty, Type::Qualified(TypeQual::BorrowMut(_), _)) {
                            BorrowKind::Exclusive
                        } else {
                            BorrowKind::Shared
                        },
                        projection: LoanProjection::default(),
                        storage_type: substitute_borrow_slot_type(inner, types),
                    }]
                }
            }
            Type::Qualified(_, inner) => {
                self.slots_with(inner, lifetimes, types, active_nominals)
            }
            Type::Tuple(items) => items
                .iter()
                .enumerate()
                .flat_map(|(index, item)| {
                    self.slots_with(item, lifetimes, types, active_nominals)
                        .into_iter()
                        .map(move |mut slot| {
                            slot.projection = slot
                                .projection
                                .prefixed(LoanProjectionStep::Tuple(index));
                            slot
                        })
                })
                .collect(),
            Type::Slice(elem) => self
                .slots_with(elem, lifetimes, types, active_nominals)
                .into_iter()
                .map(|mut slot| {
                    slot.projection = slot.projection.prefixed(LoanProjectionStep::AnyIndex);
                    slot
                })
                .collect(),
            Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => self
                .slots_with(&arguments[0], lifetimes, types, active_nominals)
                .into_iter()
                .map(|mut slot| {
                    slot.projection = slot.projection.prefixed(LoanProjectionStep::AnyIndex);
                    slot
                })
                .collect(),
            Type::Named(name, arguments)
                if name == "Option" && arguments.len() == 1 =>
            {
                self.slots_with(&arguments[0], lifetimes, types, active_nominals)
                    .into_iter()
                    .map(|mut slot| {
                        slot.projection = slot
                            .projection
                            .prefixed(self.constructor_step("Some", 0));
                        slot
                    })
                    .collect()
            }
            Type::Named(name, arguments)
                if name == "Result" && arguments.len() == 2 =>
            {
                self.slots_with(&arguments[0], lifetimes, types, active_nominals)
                    .into_iter()
                    .map(|mut slot| {
                        slot.projection = slot
                            .projection
                            .prefixed(self.constructor_step("Ok", 0));
                        slot
                    })
                    .collect()
            }
            Type::Named(name, arguments) => {
                if arguments.is_empty()
                    && let Some(substituted) = types.get(name)
                {
                    let substituted = substitute_borrow_slot_type(substituted, types);
                    if &substituted == ty {
                        return coarse_borrow_slots(ty, lifetimes, types);
                    }
                    return self.slots_with(
                        &substituted,
                        lifetimes,
                        types,
                        active_nominals,
                    );
                }
                let Some(definition) = self.definitions.get(name) else {
                    return coarse_borrow_slots(ty, lifetimes, types);
                };
                let mut nested_lifetimes = lifetimes.clone();
                let mut nested_types = types.clone();
                for (parameter, argument) in
                    effective_nominal_type_def_params(definition).iter().zip(arguments)
                {
                    if parameter.starts_with('\'') {
                        if let Type::Named(argument, arguments) = argument
                            && arguments.is_empty()
                            && argument.starts_with('\'')
                        {
                            let parameter = parameter
                                .strip_prefix('\'')
                                .expect("guarded lifetime parameter");
                            let argument = argument
                                .strip_prefix('\'')
                                .expect("guarded lifetime argument");
                            nested_lifetimes.insert(
                                parameter.to_string(),
                                lifetimes
                                    .get(argument)
                                    .cloned()
                                    .unwrap_or_else(|| argument.to_string()),
                            );
                        }
                    } else {
                        nested_types.insert(
                            parameter.clone(),
                            substitute_borrow_slot_type(argument, types),
                        );
                    }
                }
                let instantiated = Type::Named(
                    name.clone(),
                    arguments
                        .iter()
                        .map(|argument| substitute_borrow_slot_type(argument, types))
                        .collect(),
                );
                if active_nominals.iter().any(|(_, active)| active == &instantiated) {
                    return Vec::new();
                }
                if active_nominals.iter().rev().any(|(active_name, active)| {
                    active_name == name
                        && type_node_count(&instantiated) >= type_node_count(active)
                }) {
                    // A recursive declaration which does not consume a finite
                    // type argument cannot reveal a new finite field path. Keep
                    // any lifetime relation visible in its instantiated
                    // arguments as a conservative root relation and terminate.
                    return coarse_borrow_slots(&instantiated, &nested_lifetimes, &nested_types);
                }
                active_nominals.push((name.clone(), instantiated));
                let [variant] = definition.variants.as_slice() else {
                    active_nominals.pop();
                    return Vec::new();
                };
                let slots = variant
                    .fields
                    .iter()
                    .enumerate()
                    .flat_map(|(index, field)| {
                        let step = variant
                            .field_names
                            .get(index)
                            .cloned()
                            .map(LoanProjectionStep::Field)
                            .unwrap_or(LoanProjectionStep::Tuple(index));
                        self.slots_with(
                            field,
                            &nested_lifetimes,
                            &nested_types,
                            active_nominals,
                        )
                        .into_iter()
                        .map(move |mut slot| {
                            slot.projection = slot.projection.prefixed(step.clone());
                            slot
                        })
                    })
                    .collect();
                active_nominals.pop();
                slots
            }
            Type::Dyn(_, _) => coarse_borrow_slots(ty, lifetimes, types),
            Type::RecordCompose { .. } | Type::Fn(..) => Vec::new(),
        }
    }

    pub fn borrowed_constructor(&self, constructor: &str) -> bool {
        self.constructors
            .get(constructor)
            .and_then(|name| self.definitions.get(name))
            .is_some_and(|definition| {
                definition.params.iter().any(|parameter| parameter.starts_with('\''))
            })
    }

    pub fn borrowed_record(&self, name: &str) -> bool {
        self.definitions
            .get(name)
            .is_some_and(|definition| {
                definition.params.iter().any(|parameter| parameter.starts_with('\''))
            })
    }

    /// Recover a nominal field's declared AST type, including reference
    /// qualifiers that the ordinary checker `Ty` intentionally erases. Loan
    /// checking uses this for callable fields because a first-class function
    /// value must retain the field's complete borrow contract when projected.
    pub fn field_type(&self, base: &Type, field: &str) -> Option<Type> {
        let base = match base {
            Type::Qualified(_, inner) => return self.field_type(inner, field),
            other => other,
        };
        let Type::Named(name, arguments) = base else { return None };
        let definition = self.definitions.get(name)?;
        let substitutions = effective_nominal_type_def_params(definition)
            .iter()
            .zip(arguments)
            .map(|(parameter, argument)| (parameter.clone(), argument.clone()))
            .collect::<HashMap<_, _>>();
        definition
            .variants
            .iter()
            .find_map(|variant| {
                let index = variant.field_names.iter().position(|candidate| candidate == field)?;
                variant
                    .fields
                    .get(index)
                    .map(|ty| substitute_borrow_slot_type(ty, &substitutions))
            })
    }

    pub fn constructor_step(
        &self,
        constructor: &str,
        index: usize,
    ) -> LoanProjectionStep {
        self.constructors
            .get(constructor)
            .and_then(|name| self.definitions.get(name))
            .and_then(|definition| {
                definition
                    .variants
                    .iter()
                    .find(|variant| variant.name == constructor)
            })
            .and_then(|variant| variant.field_names.get(index))
            .cloned()
            .map(LoanProjectionStep::Field)
            .unwrap_or(LoanProjectionStep::Tuple(index))
    }
}

fn slot_lifetimes(slots: &[BorrowSlot]) -> Vec<String> {
    let mut lifetimes = Vec::new();
    for slot in slots {
        if !lifetimes.contains(&slot.lifetime) {
            lifetimes.push(slot.lifetime.clone());
        }
    }
    lifetimes
}

/// Preserve the coarse lifetime relation available from a type alone when no
/// declaration catalog can authenticate nominal field paths. Every discovered
/// lifetime conservatively refers to the nominal root.
fn coarse_borrow_slots(
    ty: &Type,
    substitutions: &HashMap<String, String>,
    types: &HashMap<String, Type>,
) -> Vec<BorrowSlot> {
    fn collect(
        ty: &Type,
        substitutions: &HashMap<String, String>,
        found: &mut Vec<(String, BorrowKind)>,
    ) {
        match ty {
            Type::Qualified(TypeQual::Borrow(lifetime), inner)
            | Type::Qualified(TypeQual::LegacyBorrow(lifetime), inner)
            | Type::Qualified(TypeQual::BorrowMut(lifetime), inner) => {
                let lifetime = substitutions
                    .get(lifetime)
                    .cloned()
                    .unwrap_or_else(|| lifetime.clone());
                let kind = if matches!(ty, Type::Qualified(TypeQual::BorrowMut(_), _)) {
                    BorrowKind::Exclusive
                } else {
                    BorrowKind::Shared
                };
                if !found.iter().any(|(known, _)| known == &lifetime) {
                    found.push((lifetime, kind));
                }
                collect(inner, substitutions, found);
            }
            Type::Qualified(_, inner) => collect(inner, substitutions, found),
            Type::Named(name, arguments) if arguments.is_empty() && name.starts_with('\'') => {
                let lifetime = name
                    .strip_prefix('\'')
                    .expect("guarded lifetime marker");
                let lifetime = substitutions
                    .get(lifetime)
                    .cloned()
                    .unwrap_or_else(|| lifetime.to_string());
                if !found.iter().any(|(known, _)| known == &lifetime) {
                    // A bare nominal lifetime argument records a relation but
                    // carries no mutable-reference capability by itself.
                    found.push((lifetime, BorrowKind::Shared));
                }
            }
            Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
                for argument in arguments {
                    collect(argument, substitutions, found);
                }
            }
            Type::Slice(elem) => collect(elem, substitutions, found),
            Type::RecordCompose { base, fields } => {
                collect(base, substitutions, found);
                for (_, field) in fields {
                    collect(field, substitutions, found);
                }
            }
            Type::Fn(..) => {}
        }
    }

    let mut lifetimes = Vec::new();
    collect(ty, substitutions, &mut lifetimes);
    let storage_type = substitute_borrow_slot_type(ty, types);
    lifetimes
        .into_iter()
        .map(|(lifetime, kind)| BorrowSlot {
            lifetime,
            kind,
            projection: LoanProjection::default(),
            storage_type: storage_type.clone(),
        })
        .collect()
}

fn type_node_count(ty: &Type) -> usize {
    let children = match ty {
        Type::Qualified(_, inner) => type_node_count(inner),
        Type::Slice(inner) => type_node_count(inner),
        Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => arguments
            .iter()
            .fold(0usize, |count, argument| count.saturating_add(type_node_count(argument))),
        Type::Fn(params, result, _, _) => params
            .iter()
            .fold(type_node_count(result), |count, parameter| {
                count.saturating_add(type_node_count(parameter))
            }),
        Type::RecordCompose { base, fields } => fields
            .iter()
            .fold(type_node_count(base), |count, (_, field)| {
                count.saturating_add(type_node_count(field))
            }),
    };
    1usize.saturating_add(children)
}

fn substitute_borrow_slot_type(ty: &Type, substitutions: &HashMap<String, Type>) -> Type {
    substitute_borrow_slot_type_with(ty, substitutions, &mut Vec::new())
}

fn substitute_borrow_slot_type_with(
    ty: &Type,
    substitutions: &HashMap<String, Type>,
    active: &mut Vec<String>,
) -> Type {
    match ty {
        Type::Named(name, arguments) if arguments.is_empty() => {
            let Some(substitution) = substitutions.get(name) else { return ty.clone() };
            if active.contains(name) {
                return ty.clone();
            }
            active.push(name.clone());
            let substituted = substitute_borrow_slot_type_with(substitution, substitutions, active);
            active.pop();
            substituted
        }
        Type::Named(name, arguments) => Type::Named(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_borrow_slot_type_with(argument, substitutions, active))
                .collect(),
        ),
        Type::Qualified(qualifier, inner) => Type::Qualified(
            qualifier.clone(),
            Box::new(substitute_borrow_slot_type_with(inner, substitutions, active)),
        ),
        Type::Slice(inner) => Type::Slice(Box::new(substitute_borrow_slot_type_with(
            inner,
            substitutions,
            active,
        ))),
        Type::Tuple(items) => Type::Tuple(
            items
                .iter()
                .map(|item| substitute_borrow_slot_type_with(item, substitutions, active))
                .collect(),
        ),
        Type::Fn(params, result, conventions, qualifiers) => Type::Fn(
            params
                .iter()
                .map(|param| substitute_borrow_slot_type_with(param, substitutions, active))
                .collect(),
            Box::new(substitute_borrow_slot_type_with(result, substitutions, active)),
            conventions.clone(),
            *qualifiers,
        ),
        Type::Dyn(name, arguments) => Type::Dyn(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_borrow_slot_type_with(argument, substitutions, active))
                .collect(),
        ),
        Type::RecordCompose { base, fields } => Type::RecordCompose {
            base: Box::new(substitute_borrow_slot_type_with(base, substitutions, active)),
            fields: fields
                .iter()
                .map(|(name, field)| {
                    (
                        name.clone(),
                        substitute_borrow_slot_type_with(field, substitutions, active),
                    )
                })
                .collect(),
        },
    }
}

/// The logical ownership state associated with a physical representation.
///
/// `LayoutDependent` is intentional: a checked nominal type alone does not say
/// whether RFC-0111 selected a boxed, packed, or destination-passed layout. The
/// layout planner must refine that case rather than this layer guessing from a
/// container name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnershipStateClass {
    /// RC/capacity state belonging to an owning linear-memory object.
    LinearMemoryObject,
    /// Static access/uniqueness state for a Wasm reference value.
    GcReference,
    /// An authenticated immutable root-plus-projection carrier.  The type
    /// layer records only this requirement; lowering must consume a matching
    /// checked view fact before it may construct or transport the carrier.
    RootedViewCarrier,
    /// An owner-root relation, never an owning-object token for the viewed data.
    BorrowedOwnerRoot { lifetime: String },
    /// A tuple, nominal, or generic representation that must be refined by
    /// layout facts. Child requirements remain positional even though this
    /// layer does not guess the outer physical layout.
    LayoutDependent { children: Vec<Option<OwnershipStateClass>> },
}

/// Ownership components entering and leaving one parameter access.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnershipStateFlow {
    input: Option<OwnershipStateClass>,
    writeback: Option<OwnershipStateClass>,
}

impl OwnershipStateFlow {
    pub fn input(&self) -> Option<&OwnershipStateClass> {
        self.input.as_ref()
    }

    pub fn writeback(&self) -> Option<&OwnershipStateClass> {
        self.writeback.as_ref()
    }
}

/// One parameter in a canonical access signature.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessParam {
    ty: Type,
    kind: AccessKind,
    qualifiers: Vec<AccessQualifier>,
    borrow_lifetimes: Vec<String>,
    ownership: OwnershipStateFlow,
}

impl AccessParam {
    pub fn ty(&self) -> &Type {
        &self.ty
    }

    pub fn kind(&self) -> AccessKind {
        self.kind
    }

    pub fn qualifiers(&self) -> &[AccessQualifier] {
        &self.qualifiers
    }

    pub fn borrow_lifetimes(&self) -> &[String] {
        &self.borrow_lifetimes
    }

    pub fn ownership(&self) -> &OwnershipStateFlow {
        &self.ownership
    }
}

/// One input slot which may own a borrowed result slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorrowOwnerRelation {
    position: usize,
    input_projection: LoanProjection,
}

impl BorrowOwnerRelation {
    pub fn position(&self) -> usize {
        self.position
    }

    pub fn input_projection(&self) -> &LoanProjection {
        &self.input_projection
    }
}

/// An exact borrowed result slot and the input slots which can own it.
#[derive(Clone, Debug, PartialEq)]
pub struct BorrowRelation {
    lifetime: String,
    kind: BorrowKind,
    output_projection: LoanProjection,
    owners: Vec<BorrowOwnerRelation>,
    storage_type: Type,
}

impl BorrowRelation {
    pub fn lifetime(&self) -> &str {
        &self.lifetime
    }

    pub fn kind(&self) -> BorrowKind {
        self.kind
    }

    pub fn output_projection(&self) -> &LoanProjection {
        &self.output_projection
    }

    pub fn owners(&self) -> &[BorrowOwnerRelation] {
        &self.owners
    }

    pub fn storage_type(&self) -> &Type {
        &self.storage_type
    }
}

/// The result component of a canonical access signature.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessResult {
    ty: Type,
    qualifiers: Vec<AccessQualifier>,
    borrow_lifetimes: Vec<String>,
    ownership_output: Option<OwnershipStateClass>,
}

impl AccessResult {
    pub fn ty(&self) -> &Type {
        &self.ty
    }

    pub fn qualifiers(&self) -> &[AccessQualifier] {
        &self.qualifiers
    }

    pub fn borrow_lifetimes(&self) -> &[String] {
        &self.borrow_lifetimes
    }

    pub fn ownership_output(&self) -> Option<&OwnershipStateClass> {
        self.ownership_output.as_ref()
    }
}

/// The exact checked ownership/access identity of one callable.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessSignature {
    callable_qualifiers: Vec<AccessQualifier>,
    callable_contract: CallableQualifiers,
    params: Vec<AccessParam>,
    result: AccessResult,
    borrow_relations: Vec<BorrowRelation>,
}

/// Encoding version for [`AccessIdentityKey`]. Increment this whenever the
/// canonical byte representation changes.
pub const ACCESS_IDENTITY_SCHEMA_VERSION: u32 = 2;

/// Stable, alpha-normalized identity exported by the checked RFC-0110 access
/// authority. Its bytes are intentionally opaque to lowering; consumers may
/// compare/order/hash the identity but cannot reconstruct access facts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessIdentityKey(Vec<u8>);

impl AccessIdentityKey {
    /// The canonical, alpha-normalized checked access bytes.  Export metadata
    /// may bind these bytes to a signed declaration envelope; it must not
    /// reconstruct them from an import-site type.
    pub fn as_bytes(&self) -> &[u8] { &self.0 }
}

impl AccessSignature {
    /// Return the stable identity of this exact checked access signature.
    /// Lifetime spellings are normalized by first occurrence across the entire
    /// signature; access kind, qualifiers, ownership flow, and projected borrow
    /// relations remain exact.
    pub fn identity_key(&self) -> AccessIdentityKey {
        AccessIdentityKey(encode_access_identity(self))
    }

    #[doc(hidden)]
    pub fn as_type(&self) -> Type {
        let params = self.params.iter().map(|param| param.ty.clone()).collect();
        let conventions = self
            .params
            .iter()
            .map(|param| match param.kind {
                AccessKind::OwnedImmutable => Convention::Let,
                AccessKind::SharedBorrow => Convention::Borrow,
                AccessKind::ExclusiveWriteback => Convention::Var,
                AccessKind::Consuming => Convention::Own,
            })
            .collect();
        let mut ty = Type::Fn(
            params,
            Box::new(self.result.ty.clone()),
            conventions,
            self.callable_contract,
        );
        for qualifier in self.callable_qualifiers.iter().rev() {
            let qualifier = match qualifier {
                AccessQualifier::Frozen => TypeQual::Frozen,
                AccessQualifier::Unique => TypeQual::Unique,
                AccessQualifier::View => TypeQual::View,
                AccessQualifier::Borrow(lifetime) => TypeQual::Borrow(lifetime.clone()),
                AccessQualifier::BorrowMut(lifetime) => TypeQual::BorrowMut(lifetime.clone()),
            };
            ty = Type::Qualified(qualifier, Box::new(ty));
        }
        ty
    }

    /// Derive the signature of a checked declaration.
    ///
    /// Checked declarations with inferred parameter types still retain `None`
    /// in the AST. Callers must first obtain their finalized checked [`Type`]
    /// and use [`Self::from_function_type`] instead of inventing a type here.
    pub fn from_function(function: &Function) -> Result<Self, AccessSignatureError> {
        Self::from_function_with_catalog(function, &BorrowRelationCatalog::default())
    }

    pub fn from_function_with_catalog(
        function: &Function,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        let params = function
            .params
            .iter()
            .enumerate()
            .map(|(position, param)| {
                param
                    .ty
                    .clone()
                    .ok_or(AccessSignatureError::MissingParameterType { position })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = function
            .ret
            .clone()
            .ok_or(AccessSignatureError::MissingResultType)?;
        let conventions = function.params.iter().map(|param| param.convention).collect();
        let mut signature = Self::from_parts_with_catalog(params, result, conventions, catalog)?;
        signature.callable_contract = CallableQualifiers::new(function.pure, false);
        Ok(signature)
    }

    /// Derive the signature carried by a checked first-class function type.
    pub fn from_function_type(ty: &Type) -> Result<Self, AccessSignatureError> {
        Self::from_function_type_with_catalog(ty, &BorrowRelationCatalog::default())
    }

    pub fn from_function_type_with_catalog(
        ty: &Type,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        let callable_qualifiers = leading_qualifiers(ty);
        let Type::Fn(params, result, conventions, callable_contract) = ty.unqualified() else {
            return Err(AccessSignatureError::NotFunctionType);
        };
        let conventions = normalized_conventions(params.len(), conventions)?;
        let mut signature = Self::from_parts_with_catalog(
            params.clone(),
            result.as_ref().clone(),
            conventions,
            catalog,
        )?;
        signature.callable_qualifiers = callable_qualifiers;
        signature.callable_contract = *callable_contract;
        Ok(signature)
    }

    /// Derive a declaration's access contract using its finalized checked
    /// function type. Source qualifiers and lifetime names remain authoritative;
    /// inferred holes and generic leaves take their concrete checked shape from
    /// `resolved`.
    pub fn from_resolved_function(
        function: &Function,
        resolved: &Type,
    ) -> Result<Self, AccessSignatureError> {
        Self::from_resolved_function_with_catalog(
            function,
            resolved,
            &BorrowRelationCatalog::default(),
        )
    }

    #[doc(hidden)]
    pub fn from_resolved_function_with_catalog(
        function: &Function,
        resolved: &Type,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        let params = function.params.iter().map(|param| param.ty.as_ref()).collect();
        let conventions = function.params.iter().map(|param| param.convention).collect();
        Self::from_resolved_parts_with_catalog(
            params,
            function.ret.as_ref(),
            conventions,
            resolved,
            catalog,
        )
    }

    /// Derive a lambda or generated callable contract from finalized checked
    /// parameter/result types without fabricating placeholder type names.
    pub fn from_resolved_parts(
        declared_params: Vec<Option<&Type>>,
        declared_result: Option<&Type>,
        conventions: Vec<Convention>,
        resolved: &Type,
    ) -> Result<Self, AccessSignatureError> {
        Self::from_resolved_parts_with_catalog(
            declared_params,
            declared_result,
            conventions,
            resolved,
            &BorrowRelationCatalog::default(),
        )
    }

    #[doc(hidden)]
    pub fn from_resolved_parts_with_catalog(
        declared_params: Vec<Option<&Type>>,
        declared_result: Option<&Type>,
        conventions: Vec<Convention>,
        resolved: &Type,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        let Type::Fn(resolved_params, resolved_result, resolved_conventions, callable_contract) =
            resolved.unqualified()
        else {
            return Err(AccessSignatureError::NotFunctionType);
        };
        if declared_params.len() != resolved_params.len() {
            return Err(AccessSignatureError::ConventionArity {
                params: resolved_params.len(),
                conventions: declared_params.len(),
            });
        }
        let conventions = if conventions.is_empty() {
            normalized_conventions(resolved_params.len(), resolved_conventions)?
        } else {
            normalized_conventions(resolved_params.len(), &conventions)?
        };
        let params = declared_params
            .into_iter()
            .zip(resolved_params)
            .map(|(declared, resolved)| {
                declared.map_or_else(|| resolved.clone(), |declared| {
                    apply_declared_contract(declared, resolved)
                })
            })
            .collect();
        let result = declared_result.map_or_else(
            || resolved_result.as_ref().clone(),
            |declared| apply_declared_contract(declared, resolved_result),
        );
        let mut signature = Self::from_parts_with_catalog(params, result, conventions, catalog)?;
        signature.callable_contract = *callable_contract;
        Ok(signature)
    }

    /// Derive a signature from finalized checked parameter and result types.
    pub fn from_parts(
        params: Vec<Type>,
        result: Type,
        conventions: Vec<Convention>,
    ) -> Result<Self, AccessSignatureError> {
        Self::from_parts_with_catalog(
            params,
            result,
            conventions,
            &BorrowRelationCatalog::default(),
        )
    }

    pub fn from_parts_with_catalog(
        params: Vec<Type>,
        result: Type,
        conventions: Vec<Convention>,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        if params.len() != conventions.len() {
            return Err(AccessSignatureError::ConventionArity {
                params: params.len(),
                conventions: conventions.len(),
            });
        }

        let mut access_params = Vec::with_capacity(params.len());
        for (ty, convention) in params.into_iter().zip(conventions) {
            let qualifiers = leading_qualifiers(&ty);
            // An explicit `&'a mut T` carries the same write-back effect as a
            // `var T` parameter in the current forced-copy ABI. Keeping that
            // effect in the canonical access signature lets every consumer
            // (interpreter, Wasm, adapters) agree while direct-place lowering
            // is introduced incrementally.
            let explicit_exclusive_reference = qualifiers
                .iter()
                .any(|qualifier| matches!(qualifier, AccessQualifier::BorrowMut(_)));
            let kind = if explicit_exclusive_reference && convention != Convention::Own {
                AccessKind::ExclusiveWriteback
            } else {
                AccessKind::from(convention)
            };
            // Conventions govern the value/reference handle, while `frozen`
            // governs deep mutation of its payload. A `var frozen T` may replace
            // the whole value and `var &'a T` may replace the shared handle; both
            // retain their qualifier identity below.

            let borrow_lifetimes = catalog.borrow_lifetimes(&ty);
            let state = ownership_state_class(&ty)?;
            let requires_state = matches!(kind, AccessKind::ExclusiveWriteback | AccessKind::Consuming)
                || type_requires_ownership_state(&ty);
            let input = requires_state.then_some(state.clone()).flatten();
            let writeback = matches!(kind, AccessKind::ExclusiveWriteback)
                .then_some(state)
                .flatten();
            access_params.push(AccessParam {
                ty,
                kind,
                qualifiers,
                borrow_lifetimes,
                ownership: OwnershipStateFlow { input, writeback },
            });
        }

        let result_qualifiers = leading_qualifiers(&result);
        let result_slots = catalog.slots(&result);
        let result_lifetimes = slot_lifetimes(&result_slots);
        let result_state = ownership_state_class(&result)?;
        let returns_state = type_requires_ownership_state(&result);
        let ownership_output = returns_state.then_some(result_state).flatten();

        let mut borrow_relations = Vec::with_capacity(result_slots.len());
        for slot in result_slots {
            let mut owners = Vec::new();
            for (position, param) in access_params.iter().enumerate() {
                for input in catalog.slots(&param.ty) {
                    if input.lifetime == slot.lifetime && input.kind.can_supply(slot.kind) {
                        owners.push(BorrowOwnerRelation {
                            position,
                            input_projection: input.projection,
                        });
                    }
                }
            }
            if owners.is_empty() {
                return Err(AccessSignatureError::UnboundResultLifetime {
                    lifetime: slot.lifetime,
                });
            }
            borrow_relations.push(BorrowRelation {
                lifetime: slot.lifetime,
                kind: slot.kind,
                output_projection: slot.projection,
                owners,
                storage_type: slot.storage_type,
            });
        }

        Ok(Self {
            callable_qualifiers: Vec::new(),
            callable_contract: CallableQualifiers::ORDINARY,
            params: access_params,
            result: AccessResult {
                ty: result,
                qualifiers: result_qualifiers,
                borrow_lifetimes: result_lifetimes,
                ownership_output,
            },
            borrow_relations,
        })
    }

    pub fn params(&self) -> &[AccessParam] {
        &self.params
    }

    /// This signature with `source`'s callable qualifiers and contract: the
    /// checker rebuilds an inferred lambda signature from its parts, and those
    /// are not among them.
    #[doc(hidden)]
    pub fn with_callable_contract_of(mut self, source: &Self) -> Self {
        self.callable_qualifiers = source.callable_qualifiers.clone();
        self.callable_contract = source.callable_contract;
        self
    }

    pub fn callable_qualifiers(&self) -> &[AccessQualifier] {
        &self.callable_qualifiers
    }

    pub fn callable_contract(&self) -> CallableQualifiers {
        self.callable_contract
    }

    pub fn result(&self) -> &AccessResult {
        &self.result
    }

    pub fn borrow_relations(&self) -> &[BorrowRelation] {
        &self.borrow_relations
    }

    /// Verify that `candidate` preserves this signature without erasing or
    /// changing any access component. Lifetime names are alpha-renamable; their
    /// occurrence structure and result-to-owner positions are not.
    pub fn verify_exact(&self, candidate: &Self) -> Result<(), AccessMismatch> {
        if self.callable_contract != candidate.callable_contract {
            return Err(AccessMismatch::new(None, AccessMismatchKind::TypeShape));
        }
        if self.params.len() != candidate.params.len() {
            return Err(AccessMismatch::new(None, AccessMismatchKind::ParameterCount));
        }
        for (position, (required, found)) in
            self.params.iter().zip(&candidate.params).enumerate()
        {
            if required.kind != found.kind {
                return Err(AccessMismatch::new(
                    Some(SignaturePosition::Parameter(position)),
                    AccessMismatchKind::AccessKind,
                ));
            }
        }

        let mut lifetimes = LifetimeBijection::default();
        compare_qualifiers(
            &self.callable_qualifiers,
            &candidate.callable_qualifiers,
            &mut lifetimes,
        )
        .map_err(|kind| AccessMismatch::new(None, kind))?;
        for (position, (required, found)) in
            self.params.iter().zip(&candidate.params).enumerate()
        {
            compare_type(&required.ty, &found.ty, &mut lifetimes).map_err(|kind| {
                AccessMismatch::new(Some(SignaturePosition::Parameter(position)), kind)
            })?;
        }
        compare_type(&self.result.ty, &candidate.result.ty, &mut lifetimes)
            .map_err(|kind| AccessMismatch::new(Some(SignaturePosition::Result), kind))?;

        for (position, (required, found)) in
            self.params.iter().zip(&candidate.params).enumerate()
        {
            if !state_flow_compatible(&required.ownership, &found.ownership, &lifetimes) {
                return Err(AccessMismatch::new(
                    Some(SignaturePosition::Parameter(position)),
                    AccessMismatchKind::OwnershipState,
                ));
            }
        }
        if !optional_state_compatible(
            self.result.ownership_output.as_ref(),
            candidate.result.ownership_output.as_ref(),
            &lifetimes,
        ) {
            return Err(AccessMismatch::new(
                Some(SignaturePosition::Result),
                AccessMismatchKind::OwnershipState,
            ));
        }

        if self.borrow_relations.len() != candidate.borrow_relations.len()
            || self
                .borrow_relations
                .iter()
                .zip(&candidate.borrow_relations)
                .any(|(required, candidate)| {
                    !borrow_relation_compatible(required, candidate, &lifetimes)
                })
        {
            return Err(AccessMismatch::new(
                Some(SignaturePosition::Result),
                AccessMismatchKind::BorrowRelation,
            ));
        }
        Ok(())
    }

    /// Verify a directed callable conversion. The ordinary type checker permits
    /// one callable-identity relaxation: a reusable or once `pure fn` may widen
    /// to the corresponding ordinary callable. Every ownership, access, nested
    /// type, cardinality, and borrow-relation component remains exact.
    #[doc(hidden)]
    pub fn verify_directed(&self, expected: &Self) -> Result<(), AccessMismatch> {
        let actual_contract = self.callable_contract;
        let expected_contract = expected.callable_contract;
        let purity_widening = actual_contract.once == expected_contract.once
            && actual_contract.pure
            && !expected_contract.pure;
        if actual_contract != expected_contract && !purity_widening {
            return Err(AccessMismatch::new(None, AccessMismatchKind::TypeShape));
        }
        let mut actual = self.clone();
        actual.callable_contract = expected_contract;
        actual.verify_exact(expected)
    }

    /// Join two branch callable identities. Purity is the sole widening axis:
    /// a branch is pure only when both inputs are pure. Invocation cardinality
    /// and all access/ownership relations must agree exactly.
    #[doc(hidden)]
    pub fn join(&self, other: &Self) -> Result<Self, AccessMismatch> {
        if self.callable_contract.once != other.callable_contract.once {
            return Err(AccessMismatch::new(None, AccessMismatchKind::TypeShape));
        }
        let contract = CallableQualifiers::new(
            self.callable_contract.pure && other.callable_contract.pure,
            self.callable_contract.once,
        );
        let mut left = self.clone();
        let mut right = other.clone();
        left.callable_contract = contract;
        right.callable_contract = contract;
        left.verify_exact(&right)?;
        Ok(left)
    }

    /// Compare the canonical output-slot to input-slot graph independently of
    /// surface type specialization. The type checker already owns surface type
    /// compatibility at callable-value sites; this preserves the relation
    /// identity while allowing a generic callable and its specialization.
    pub fn has_same_projected_borrow_relations(&self, candidate: &Self) -> bool {
        self.borrow_relations.len() == candidate.borrow_relations.len()
            && self
                .borrow_relations
                .iter()
                .zip(&candidate.borrow_relations)
                .all(|(left, right)| {
                    left.output_projection == right.output_projection
                        && left.owners.len() == right.owners.len()
                        && left.owners.iter().zip(&right.owners).all(
                            |(left_owner, right_owner)| {
                                left_owner.position == right_owner.position
                                    && left_owner.input_projection
                                        == right_owner.input_projection
                            },
                        )
                })
    }
}

fn encode_access_identity(signature: &AccessSignature) -> Vec<u8> {
    #[derive(Default)]
    struct Encoder {
        bytes: Vec<u8>,
        lifetimes: std::collections::BTreeMap<String, u32>,
    }

    impl Encoder {
        fn tag(&mut self, tag: u8) {
            self.bytes.push(tag);
        }

        fn u32(&mut self, value: u32) {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        }

        fn usize(&mut self, value: usize) {
            self.u32(u32::try_from(value).expect("access identity exceeds u32::MAX items"));
        }

        fn i64(&mut self, value: i64) {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        }

        fn text(&mut self, value: &str) {
            self.usize(value.len());
            self.bytes.extend_from_slice(value.as_bytes());
        }

        fn lifetime(&mut self, name: &str) {
            let id = if let Some(id) = self.lifetimes.get(name) {
                *id
            } else {
                let id = u32::try_from(self.lifetimes.len())
                    .expect("access identity exceeds u32::MAX lifetime binders");
                self.lifetimes.insert(name.to_string(), id);
                id
            };
            self.u32(id);
        }

        fn qualifier(&mut self, qualifier: &AccessQualifier) {
            match qualifier {
                AccessQualifier::Frozen => self.tag(0),
                AccessQualifier::Unique => self.tag(1),
                AccessQualifier::View => self.tag(5),
                // Tag 2 is retired; surviving qualifier identities remain stable.
                AccessQualifier::Borrow(lifetime) => {
                    self.tag(3);
                    self.lifetime(lifetime);
                }
                AccessQualifier::BorrowMut(lifetime) => {
                    self.tag(4);
                    self.lifetime(lifetime);
                }
            }
        }

        fn qualifiers(&mut self, qualifiers: &[AccessQualifier]) {
            self.usize(qualifiers.len());
            for qualifier in qualifiers {
                self.qualifier(qualifier);
            }
        }

        fn ty(&mut self, ty: &Type) {
            match ty {
                Type::Named(name, arguments) => {
                    if let Some(lifetime) = witchy_syntax::ast::lifetime_param_name(name) {
                        self.tag(6);
                        self.lifetime(lifetime);
                    } else {
                        self.tag(0);
                        self.text(name);
                        self.usize(arguments.len());
                        for argument in arguments {
                            self.ty(argument);
                        }
                    }
                }
                Type::Tuple(items) => {
                    self.tag(1);
                    self.usize(items.len());
                    for item in items {
                        self.ty(item);
                    }
                }
                Type::Fn(parameters, result, conventions, qualifiers) => {
                    self.tag(2);
                    // Nested callables bind their own lifetime namespace. This
                    // mirrors `compare_type`: reusing an outer spelling inside
                    // a callback does not relate the two scopes.
                    let outer_lifetimes = std::mem::take(&mut self.lifetimes);
                    self.usize(parameters.len());
                    for (position, parameter) in parameters.iter().enumerate() {
                        self.tag(match conventions.get(position).copied().unwrap_or_default() {
                            Convention::Let => 0,
                            Convention::Borrow => 1,
                            Convention::Var => 2,
                            Convention::Own => 3,
                        });
                        self.ty(parameter);
                    }
                    self.ty(result);
                    self.tag(u8::from(qualifiers.pure));
                    self.tag(u8::from(qualifiers.once));
                    self.lifetimes = outer_lifetimes;
                }
                Type::Qualified(qualifier, inner) => {
                    self.tag(3);
                    self.qualifier(&AccessQualifier::from(qualifier));
                    self.ty(inner);
                }
                Type::Dyn(name, arguments) => {
                    self.tag(4);
                    self.text(name);
                    self.usize(arguments.len());
                    for argument in arguments {
                        self.ty(argument);
                    }
                }
                Type::RecordCompose { base, fields } => {
                    self.tag(5);
                    self.ty(base);
                    self.usize(fields.len());
                    for (name, field) in fields {
                        self.text(name);
                        self.ty(field);
                    }
                }
                Type::Slice(elem) => {
                    self.tag(7);
                    self.ty(elem);
                }
            }
        }

        fn ownership_state(&mut self, state: &OwnershipStateClass) {
            match state {
                OwnershipStateClass::LinearMemoryObject => self.tag(0),
                OwnershipStateClass::GcReference => self.tag(1),
                OwnershipStateClass::RootedViewCarrier => self.tag(4),
                OwnershipStateClass::BorrowedOwnerRoot { lifetime } => {
                    self.tag(2);
                    self.lifetime(lifetime);
                }
                OwnershipStateClass::LayoutDependent { children } => {
                    self.tag(3);
                    self.usize(children.len());
                    for child in children {
                        self.optional_ownership_state(child.as_ref());
                    }
                }
            }
        }

        fn optional_ownership_state(&mut self, state: Option<&OwnershipStateClass>) {
            match state {
                Some(state) => {
                    self.tag(1);
                    self.ownership_state(state);
                }
                None => self.tag(0),
            }
        }

        fn projection(&mut self, projection: &LoanProjection) {
            self.usize(projection.steps.len());
            for step in &projection.steps {
                match step {
                    LoanProjectionStep::Field(name) => {
                        self.tag(0);
                        self.text(name);
                    }
                    LoanProjectionStep::Tuple(position) => {
                        self.tag(1);
                        self.usize(*position);
                    }
                    LoanProjectionStep::Index(index) => {
                        self.tag(2);
                        self.i64(*index);
                    }
                    LoanProjectionStep::AnyIndex => self.tag(4),
                    LoanProjectionStep::Range { lo, hi, inclusive } => {
                        self.tag(3);
                        self.i64(*lo);
                        self.i64(*hi);
                        self.tag(u8::from(*inclusive));
                    }
                }
            }
        }
    }

    let mut encoder = Encoder::default();
    encoder.u32(ACCESS_IDENTITY_SCHEMA_VERSION);
    encoder.qualifiers(&signature.callable_qualifiers);
    encoder.tag(u8::from(signature.callable_contract.pure));
    encoder.tag(u8::from(signature.callable_contract.once));
    encoder.usize(signature.params.len());
    for parameter in &signature.params {
        encoder.tag(match parameter.kind {
            AccessKind::OwnedImmutable => 0,
            AccessKind::SharedBorrow => 1,
            AccessKind::ExclusiveWriteback => 2,
            AccessKind::Consuming => 3,
        });
        encoder.ty(&parameter.ty);
        encoder.qualifiers(&parameter.qualifiers);
        encoder.optional_ownership_state(parameter.ownership.input.as_ref());
        encoder.optional_ownership_state(parameter.ownership.writeback.as_ref());
    }
    encoder.ty(&signature.result.ty);
    encoder.qualifiers(&signature.result.qualifiers);
    encoder.optional_ownership_state(signature.result.ownership_output.as_ref());
    encoder.usize(signature.borrow_relations.len());
    for relation in &signature.borrow_relations {
        encoder.lifetime(&relation.lifetime);
        encoder.tag(match relation.kind {
            BorrowKind::Shared => 0,
            BorrowKind::Exclusive => 1,
        });
        encoder.projection(&relation.output_projection);
        encoder.usize(relation.owners.len());
        for owner in &relation.owners {
            encoder.usize(owner.position);
            encoder.projection(&owner.input_projection);
        }
        encoder.ty(&relation.storage_type);
    }
    encoder.bytes
}

fn borrow_relation_compatible(
    required: &BorrowRelation,
    candidate: &BorrowRelation,
    lifetimes: &LifetimeBijection,
) -> bool {
    lifetimes.matches(&required.lifetime, &candidate.lifetime)
        && required.kind == candidate.kind
        && required.output_projection == candidate.output_projection
        && required.owners.len() == candidate.owners.len()
        && required
            .owners
            .iter()
            .zip(&candidate.owners)
            .all(|(required, candidate)| {
                required.position == candidate.position
                    && required.input_projection == candidate.input_projection
            })
}

/// Classify the ownership state which a type's current representation can need.
///
/// This query is structural. It intentionally leaves nominal layouts opaque and
/// contains no `List`/`Dict`/operation-specific rules.
pub fn ownership_state_class(
    ty: &Type,
) -> Result<Option<OwnershipStateClass>, AccessSignatureError> {
    match ty {
        Type::Qualified(TypeQual::View, _) => Ok(Some(OwnershipStateClass::RootedViewCarrier)),
        Type::Qualified(TypeQual::Borrow(lifetime) | TypeQual::LegacyBorrow(lifetime), _) => {
            Ok(Some(OwnershipStateClass::BorrowedOwnerRoot {
                lifetime: lifetime.clone(),
            }))
        }
        Type::Qualified(_, inner) => ownership_state_class(inner),
        Type::RecordCompose { .. } => Err(AccessSignatureError::UnnormalizedRecordCompose),
        Type::Tuple(fields) if fields.is_empty() => Ok(None),
        Type::Tuple(fields) => {
            let states = fields
                .iter()
                .map(ownership_state_class)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(OwnershipStateClass::LayoutDependent { children: states }))
        }
        Type::Fn(..) | Type::Dyn(_, _) => Ok(Some(OwnershipStateClass::GcReference)),
        Type::Slice(inner) => ownership_state_class(inner),
        Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "Int" | "Float" | "Duration" | "Bool" | "Nil" | "()" => Ok(None),
            "String" | "str" | "Bytes" | "__Msg" => {
                Ok(Some(OwnershipStateClass::LinearMemoryObject))
            }
            _ if CapabilityKind::from_name(name).is_some() => {
                if crate::capabilities::externref_cap_name(name).is_some() {
                    Ok(Some(OwnershipStateClass::GcReference))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(Some(OwnershipStateClass::LayoutDependent {
                children: Vec::new(),
            })),
        },
        Type::Named(_, arguments) => Ok(Some(OwnershipStateClass::LayoutDependent {
            children: arguments
                .iter()
                .map(ownership_state_class)
                .collect::<Result<Vec<_>, _>>()?,
        })),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessSignatureError {
    NotFunctionType,
    MissingParameterType { position: usize },
    MissingResultType,
    ConventionArity { params: usize, conventions: usize },
    UnboundResultLifetime { lifetime: String },
    UnnormalizedRecordCompose,
}

impl fmt::Display for AccessSignatureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFunctionType => write!(formatter, "expected a checked function type"),
            Self::MissingParameterType { position } => write!(
                formatter,
                "parameter {position} has no finalized checked type"
            ),
            Self::MissingResultType => {
                write!(formatter, "function has no finalized checked result type")
            }
            Self::ConventionArity { params, conventions } => write!(
                formatter,
                "function type has {params} parameter(s) but {conventions} convention(s)"
            ),
            Self::UnboundResultLifetime { lifetime } => write!(
                formatter,
                "borrowed result lifetime '{lifetime} has no owner parameter"
            ),
            Self::UnnormalizedRecordCompose => write!(
                formatter,
                "structural record composition reached access-signature derivation before normalization"
            ),
        }
    }
}

impl std::error::Error for AccessSignatureError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignaturePosition {
    Parameter(usize),
    Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMismatchKind {
    ParameterCount,
    AccessKind,
    TypeShape,
    Qualifier,
    BorrowRelation,
    OwnershipState,
    NestedConvention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessMismatch {
    position: Option<SignaturePosition>,
    kind: AccessMismatchKind,
}

impl AccessMismatch {
    fn new(position: Option<SignaturePosition>, kind: AccessMismatchKind) -> Self {
        Self { position, kind }
    }

    pub fn position(&self) -> Option<SignaturePosition> {
        self.position
    }

    pub fn kind(&self) -> AccessMismatchKind {
        self.kind
    }
}

impl fmt::Display for AccessMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let position = match self.position {
            Some(SignaturePosition::Parameter(index)) => format!("parameter {index}"),
            Some(SignaturePosition::Result) => "result".to_string(),
            None => "signature".to_string(),
        };
        write!(formatter, "{position} does not preserve {:?}", self.kind)
    }
}

impl std::error::Error for AccessMismatch {}

fn normalized_conventions(
    params: usize,
    conventions: &[Convention],
) -> Result<Vec<Convention>, AccessSignatureError> {
    if conventions.is_empty() {
        return Ok(vec![Convention::Let; params]);
    }
    if params != conventions.len() {
        return Err(AccessSignatureError::ConventionArity {
            params,
            conventions: conventions.len(),
        });
    }
    Ok(conventions.to_vec())
}

fn leading_qualifiers(ty: &Type) -> Vec<AccessQualifier> {
    let mut qualifiers = Vec::new();
    let mut current = ty;
    while let Type::Qualified(qualifier, inner) = current {
        qualifiers.push(AccessQualifier::from(qualifier));
        current = inner;
    }
    qualifiers
}

fn apply_declared_contract(declared: &Type, resolved: &Type) -> Type {
    match declared {
        Type::Qualified(qualifier, inner) => {
            // Finalized checker types ordinarily erase declaration access
            // qualifiers, but a checked expression context may retain one.
            // Apply the source contract exactly once: recursively feeding the
            // same retained qualifier to a generic leaf would manufacture
            // `&&a` from a declared `&a`.
            let resolved = match resolved {
                Type::Qualified(resolved_qualifier, resolved_inner)
                    if resolved_qualifier == qualifier => resolved_inner.as_ref(),
                _ => resolved,
            };
            Type::Qualified(
                qualifier.clone(),
                Box::new(apply_declared_contract(inner, resolved)),
            )
        }
        // Lifetime markers are declaration binders, not checker type holes. Keep
        // one alpha-consistent spelling across qualifiers and nominal arguments.
        Type::Named(name, arguments) if arguments.is_empty() && name.starts_with('\'') => {
            declared.clone()
        }
        Type::Named(name, arguments)
            if arguments.is_empty()
                && !name.contains('.')
                && name.chars().next().is_some_and(char::is_lowercase) =>
        {
            resolved.clone()
        }
        Type::Named(_, declared_arguments) => {
            let Type::Named(resolved_name, resolved_arguments) = resolved.unqualified() else {
                return resolved.clone();
            };
            if declared_arguments.len() != resolved_arguments.len() {
                return resolved.clone();
            }
            Type::Named(
                resolved_name.clone(),
                declared_arguments
                    .iter()
                    .zip(resolved_arguments)
                    .map(|(declared, resolved)| apply_declared_contract(declared, resolved))
                    .collect(),
            )
        }
        Type::Dyn(_, declared_arguments) => {
            let Type::Dyn(resolved_name, resolved_arguments) = resolved.unqualified() else {
                return resolved.clone();
            };
            if declared_arguments.len() != resolved_arguments.len() {
                return resolved.clone();
            }
            Type::Dyn(
                resolved_name.clone(),
                declared_arguments
                    .iter()
                    .zip(resolved_arguments)
                    .map(|(declared, resolved)| apply_declared_contract(declared, resolved))
                    .collect(),
            )
        }
        Type::Tuple(declared_fields) => {
            let Type::Tuple(resolved_fields) = resolved.unqualified() else {
                return resolved.clone();
            };
            if declared_fields.len() != resolved_fields.len() {
                return resolved.clone();
            }
            Type::Tuple(
                declared_fields
                    .iter()
                    .zip(resolved_fields)
                    .map(|(declared, resolved)| apply_declared_contract(declared, resolved))
                    .collect(),
            )
        }
        Type::Fn(declared_params, declared_result, declared_conventions, declared_qualifiers) => {
            let Type::Fn(resolved_params, resolved_result, resolved_conventions, _) =
                resolved.unqualified()
            else {
                return resolved.clone();
            };
            if declared_params.len() != resolved_params.len() {
                return resolved.clone();
            }
            let conventions = if declared_conventions.is_empty() {
                resolved_conventions.clone()
            } else {
                declared_conventions.clone()
            };
            Type::Fn(
                declared_params
                    .iter()
                    .zip(resolved_params)
                    .map(|(declared, resolved)| apply_declared_contract(declared, resolved))
                    .collect(),
                Box::new(apply_declared_contract(declared_result, resolved_result)),
                conventions,
                *declared_qualifiers,
            )
        }
        Type::Slice(declared_elem) => {
            let Type::Slice(resolved_elem) = resolved.unqualified() else {
                return resolved.clone();
            };
            Type::Slice(Box::new(apply_declared_contract(declared_elem, resolved_elem)))
        }
        Type::RecordCompose { .. } => resolved.clone(),
    }
}

/// Whether crossing this access boundary must preserve representation state.
///
/// This is deliberately distinct from affine/unique-storage classification.
/// A canonical view is copyable, but every copied carrier still retains an
/// authenticated root and therefore needs shared retain/drop state at ordinary
/// input and output boundaries.
pub fn type_requires_ownership_state(ty: &Type) -> bool {
    match ty {
        Type::Qualified(
            TypeQual::Unique
            | TypeQual::Borrow(_)
            | TypeQual::LegacyBorrow(_)
            | TypeQual::BorrowMut(_),
            _,
        ) => true,
        Type::Qualified(TypeQual::View, _) => true,
        Type::Qualified(TypeQual::Frozen, inner) => type_requires_ownership_state(inner),
        Type::Slice(inner) => type_requires_ownership_state(inner),
        Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
            arguments.iter().any(type_requires_ownership_state)
        }
        Type::RecordCompose { base, fields } => {
            type_requires_ownership_state(base)
                || fields
                    .iter()
                    .any(|(_, field)| type_requires_ownership_state(field))
        }
        // A nested callable's parameter/result ownership belongs to that
        // callable's own access signature, not to the function value which
        // contains it.
        Type::Fn(..) => false,
    }
}

#[derive(Default)]
struct LifetimeBijection {
    forward: Vec<(String, String)>,
}

impl LifetimeBijection {
    fn relate(&mut self, required: &str, candidate: &str) -> bool {
        if let Some((_, mapped)) = self.forward.iter().find(|(from, _)| from == required) {
            return mapped == candidate;
        }
        if self.forward.iter().any(|(_, to)| to == candidate) {
            return false;
        }
        self.forward.push((required.to_string(), candidate.to_string()));
        true
    }

    fn matches(&self, required: &str, candidate: &str) -> bool {
        self.forward
            .iter()
            .find(|(from, _)| from == required)
            .is_some_and(|(_, mapped)| mapped == candidate)
    }
}

fn compare_type(
    required: &Type,
    candidate: &Type,
    lifetimes: &mut LifetimeBijection,
) -> Result<(), AccessMismatchKind> {
    match (required, candidate) {
        (Type::Qualified(TypeQual::View, left_inner), Type::Qualified(TypeQual::View, right_inner)) => {
            compare_type(left_inner, right_inner, lifetimes)
        }
        (
            Type::Qualified(TypeQual::Borrow(left) | TypeQual::LegacyBorrow(left), left_inner),
            Type::Qualified(TypeQual::Borrow(right) | TypeQual::LegacyBorrow(right), right_inner),
        ) => {
            if !lifetimes.relate(left, right) {
                return Err(AccessMismatchKind::BorrowRelation);
            }
            compare_type(left_inner, right_inner, lifetimes)
        }
        (
            Type::Qualified(TypeQual::BorrowMut(left), left_inner),
            Type::Qualified(TypeQual::BorrowMut(right), right_inner),
        ) => {
            if !lifetimes.relate(left, right) {
                return Err(AccessMismatchKind::BorrowRelation);
            }
            compare_type(left_inner, right_inner, lifetimes)
        }
        (Type::Qualified(left, left_inner), Type::Qualified(right, right_inner)) => {
            if left != right {
                return Err(AccessMismatchKind::Qualifier);
            }
            compare_type(left_inner, right_inner, lifetimes)
        }
        (Type::Qualified(_, _), _) | (_, Type::Qualified(_, _)) => {
            Err(AccessMismatchKind::Qualifier)
        }
        (Type::Named(left, left_args), Type::Named(right, right_args))
            if left_args.is_empty()
                && right_args.is_empty()
                && left.starts_with('\'')
                && right.starts_with('\'') =>
        {
            let left = left.strip_prefix('\'').expect("guarded lifetime marker");
            let right = right.strip_prefix('\'').expect("guarded lifetime marker");
            if lifetimes.relate(left, right) {
                Ok(())
            } else {
                Err(AccessMismatchKind::BorrowRelation)
            }
        }
        (Type::Named(left_name, left_args), Type::Named(right_name, right_args))
        | (Type::Dyn(left_name, left_args), Type::Dyn(right_name, right_args)) => {
            if left_name != right_name || left_args.len() != right_args.len() {
                return Err(AccessMismatchKind::TypeShape);
            }
            compare_type_lists(left_args, right_args, lifetimes)
        }
        (Type::Tuple(left), Type::Tuple(right)) => compare_type_lists(left, right, lifetimes),
        (
            Type::RecordCompose { base: left_base, fields: left_fields },
            Type::RecordCompose { base: right_base, fields: right_fields },
        ) => {
            if left_fields.len() != right_fields.len()
                || left_fields
                    .iter()
                    .zip(right_fields)
                    .any(|((left, _), (right, _))| left != right)
            {
                return Err(AccessMismatchKind::TypeShape);
            }
            compare_type(left_base, right_base, lifetimes)?;
            for ((_, left), (_, right)) in left_fields.iter().zip(right_fields) {
                compare_type(left, right, lifetimes)?;
            }
            Ok(())
        }
        (
            Type::Fn(left_params, left_result, left_conventions, left_qualifiers),
            Type::Fn(right_params, right_result, right_conventions, right_qualifiers),
        ) => {
            if left_qualifiers != right_qualifiers {
                return Err(AccessMismatchKind::TypeShape);
            }
            if left_params.len() != right_params.len() {
                return Err(AccessMismatchKind::TypeShape);
            }
            let left_conventions = normalized_conventions(left_params.len(), left_conventions)
                .map_err(|_| AccessMismatchKind::NestedConvention)?;
            let right_conventions = normalized_conventions(right_params.len(), right_conventions)
                .map_err(|_| AccessMismatchKind::NestedConvention)?;
            if left_conventions != right_conventions {
                return Err(AccessMismatchKind::NestedConvention);
            }
            let mut nested_lifetimes = LifetimeBijection::default();
            compare_type_lists(left_params, right_params, &mut nested_lifetimes)?;
            compare_type(left_result, right_result, &mut nested_lifetimes)
        }
        _ => Err(AccessMismatchKind::TypeShape),
    }
}

fn compare_qualifiers(
    required: &[AccessQualifier],
    candidate: &[AccessQualifier],
    lifetimes: &mut LifetimeBijection,
) -> Result<(), AccessMismatchKind> {
    if required.len() != candidate.len() {
        return Err(AccessMismatchKind::Qualifier);
    }
    for (required, candidate) in required.iter().zip(candidate) {
        match (required, candidate) {
            (AccessQualifier::Borrow(required), AccessQualifier::Borrow(candidate)) => {
                if !lifetimes.relate(required, candidate) {
                    return Err(AccessMismatchKind::BorrowRelation);
                }
            }
            (AccessQualifier::BorrowMut(required), AccessQualifier::BorrowMut(candidate)) => {
                if !lifetimes.relate(required, candidate) {
                    return Err(AccessMismatchKind::BorrowRelation);
                }
            }
            _ if required == candidate => {}
            _ => return Err(AccessMismatchKind::Qualifier),
        }
    }
    Ok(())
}

fn compare_type_lists(
    required: &[Type],
    candidate: &[Type],
    lifetimes: &mut LifetimeBijection,
) -> Result<(), AccessMismatchKind> {
    if required.len() != candidate.len() {
        return Err(AccessMismatchKind::TypeShape);
    }
    for (required, candidate) in required.iter().zip(candidate) {
        compare_type(required, candidate, lifetimes)?;
    }
    Ok(())
}

fn state_flow_compatible(
    required: &OwnershipStateFlow,
    candidate: &OwnershipStateFlow,
    lifetimes: &LifetimeBijection,
) -> bool {
    optional_state_compatible(required.input.as_ref(), candidate.input.as_ref(), lifetimes)
        && optional_state_compatible(
            required.writeback.as_ref(),
            candidate.writeback.as_ref(),
            lifetimes,
        )
}

fn optional_state_compatible(
    required: Option<&OwnershipStateClass>,
    candidate: Option<&OwnershipStateClass>,
    lifetimes: &LifetimeBijection,
) -> bool {
    match (required, candidate) {
        (None, None) => true,
        (Some(required), Some(candidate)) => {
            state_compatible(required, candidate, lifetimes)
        }
        _ => false,
    }
}

fn state_compatible(
    required: &OwnershipStateClass,
    candidate: &OwnershipStateClass,
    lifetimes: &LifetimeBijection,
) -> bool {
    match (required, candidate) {
        (
            OwnershipStateClass::BorrowedOwnerRoot { lifetime: required },
            OwnershipStateClass::BorrowedOwnerRoot { lifetime: candidate },
        ) => lifetimes.matches(required, candidate),
        (
            OwnershipStateClass::LayoutDependent { children: required },
            OwnershipStateClass::LayoutDependent { children: candidate },
        ) => {
            required.len() == candidate.len()
                && required.iter().zip(candidate).all(|(required, candidate)| {
                    optional_state_compatible(
                        required.as_ref(),
                        candidate.as_ref(),
                        lifetimes,
                    )
                })
        }
        _ => required == candidate,
    }
}
