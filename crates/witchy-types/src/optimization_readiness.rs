//! Checked facts retained for possible future optimization.
//!
//! This is an authority carrier, not an optimizer.  In particular, a `pure`
//! callable promises no observable callable effect; it does *not* prove that
//! invocation terminates or cannot trap.  Discard facts therefore retain a
//! conservative execution barrier even where a call has a pure contract.

use std::collections::HashMap;
use std::fmt;

use witchy_syntax::ast::{Block, Expr, Item, Module, Pattern, Stmt};

use witchy_canonical_source::access::{AccessIdentityKey, AccessQualifier, AccessSignature};
use crate::pipeline::CanonicalCheckedSource;
use crate::typeck::{CheckedAccessFactsError, TypedModule};

/// The checked observable-effect promise carried by one exact invocation.
///
/// `Pure` does not include a totality or no-trap proof.  A future elimination
/// pass must obtain those facts independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckedCallEffects {
    Pure,
    MayHaveObservableEffects,
}

/// Facts retained from one direct, indirect, generic, closure, or witness call
/// that type checking selected at an exact expression allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallOptimizationFacts {
    access_identity: AccessIdentityKey,
    frozen_parameters: Vec<usize>,
    frozen_result: bool,
    effects: CheckedCallEffects,
}

impl CallOptimizationFacts {
    pub fn access_identity(&self) -> &AccessIdentityKey { &self.access_identity }
    pub fn frozen_parameters(&self) -> &[usize] { &self.frozen_parameters }
    pub fn frozen_result(&self) -> bool { self.frozen_result }
    pub fn effects(&self) -> CheckedCallEffects { self.effects }
}

/// Facts retained for the only source-level discard form, `let _ = value`.
///
/// A checked program cannot put a live `must` value here.  That is a proof
/// that the discard did not discharge an obligation, not permission for a
/// later pass to discard a must-bearing value after a rewrite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscardOptimizationFacts {
    call_access_identity: Option<AccessIdentityKey>,
    effects: CheckedCallEffects,
    /// Unknown is deliberately represented as `true`: an optimizer must keep
    /// the evaluation unless it obtains a separate no-trap proof.
    may_trap: bool,
    /// Unknown is deliberately represented as `true`: purity never supplies a
    /// termination proof.
    may_diverge: bool,
    no_live_must_obligation: bool,
}

impl DiscardOptimizationFacts {
    pub fn call_access_identity(&self) -> Option<&AccessIdentityKey> {
        self.call_access_identity.as_ref()
    }

    pub fn effects(&self) -> CheckedCallEffects { self.effects }
    pub fn may_trap(&self) -> bool { self.may_trap }
    pub fn may_diverge(&self) -> bool { self.may_diverge }
    pub fn no_live_must_obligation(&self) -> bool { self.no_live_must_obligation }

    /// This carrier alone can never authorize removal of a discard expression.
    pub fn permits_elision(&self) -> bool { false }
}

/// Why a readiness query refused to use a fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptimizationReadinessError {
    MissingAccessFacts(CheckedAccessFactsError),
    StaleCheckedSource,
    MissingCallFact,
    MissingDiscardFact,
    AccessIdentityMismatch,
}

impl fmt::Display for OptimizationReadinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAccessFacts(error) => {
                write!(formatter, "optimization readiness lacks checked access facts: {error}")
            }
            Self::StaleCheckedSource => formatter.write_str(
                "optimization readiness belongs to a different checked source identity",
            ),
            Self::MissingCallFact => formatter.write_str(
                "optimization readiness lacks an exact checked call fact",
            ),
            Self::MissingDiscardFact => formatter.write_str(
                "optimization readiness lacks an exact checked discard fact",
            ),
            Self::AccessIdentityMismatch => formatter.write_str(
                "optimization readiness call access identity no longer matches checked source",
            ),
        }
    }
}

impl std::error::Error for OptimizationReadinessError {}

/// Sealed facts whose address keys belong to one checked canonical source.
///
/// This product is created only while the canonical checked source is built.
/// Queries re-check both that source identity and the live access identity, so
/// copying an expression, replacing a typed module, or changing a selected
/// indirect/generic signature fails closed.
#[derive(Debug)]
pub struct OptimizationReadiness {
    typed_owner: *const Module,
    source_bundle_digest: [u8; 32],
    calls: HashMap<usize, CallOptimizationFacts>,
    discards: HashMap<usize, DiscardOptimizationFacts>,
}

// Address keys are an authority check at query time, not semantic source
// content.  Canonical checked-source equality has always compared equivalent
// source products across independently allocated typed modules, so it must not
// turn allocation identity into observable equality.
impl PartialEq for OptimizationReadiness {
    fn eq(&self, other: &Self) -> bool {
        self.source_bundle_digest == other.source_bundle_digest
            && self.calls == other.calls
            && self.discards == other.discards
    }
}

impl OptimizationReadiness {
    pub(crate) fn from_typed(
        typed: &TypedModule,
        source_bundle_digest: [u8; 32],
    ) -> Result<Self, OptimizationReadinessError> {
        let module = typed.module();
        let access = typed
            .access_facts()
            .map_err(OptimizationReadinessError::MissingAccessFacts)?;
        let calls = access
            .call_contracts()
            .map(|(site, signature)| (site, call_facts(signature)))
            .collect();
        let mut discards = HashMap::new();
        visit_module_discards(module, &mut |expression| {
            // A source-level discard records the selected root-call contract
            // when it has one. Every other expression stays conservative.
            let call = access.call_at(module, expression);
            let facts = DiscardOptimizationFacts {
                call_access_identity: call.map(AccessSignature::identity_key),
                effects: call.map(call_effects).unwrap_or(CheckedCallEffects::MayHaveObservableEffects),
                // Neither ordinary nor pure checked effects proves these.
                may_trap: true,
                may_diverge: true,
                // The type checker rejected any live must obligation before
                // this canonical product was minted.
                no_live_must_obligation: true,
            };
            discards.insert(expression as *const Expr as usize, facts);
        });
        Ok(Self {
            typed_owner: std::ptr::from_ref(module),
            source_bundle_digest,
            calls,
            discards,
        })
    }

    /// Check that this product remains paired with its exact canonical source.
    /// Production lowering calls this before it exposes the carrier to any
    /// later stage.
    pub fn validate_source(
        &self,
        source: &CanonicalCheckedSource,
    ) -> Result<(), OptimizationReadinessError> {
        let typed = source.typed_module();
        (std::ptr::eq(self.typed_owner, typed.module())
            && self.source_bundle_digest == source.source_witness().source_bundle_digest())
            .then_some(())
            .ok_or(OptimizationReadinessError::StaleCheckedSource)
    }

    pub fn call_at<'a>(
        &'a self,
        source: &CanonicalCheckedSource,
        expression: &Expr,
    ) -> Result<&'a CallOptimizationFacts, OptimizationReadinessError> {
        self.validate_source(source)?;
        let facts = self
            .calls
            .get(&(expression as *const Expr as usize))
            .ok_or(OptimizationReadinessError::MissingCallFact)?;
        let access = source
            .access_facts()
            .map_err(OptimizationReadinessError::MissingAccessFacts)?;
        let current = access
            .call_at(source.typed_module().module(), expression)
            .ok_or(OptimizationReadinessError::MissingCallFact)?;
        (current.identity_key() == facts.access_identity)
            .then_some(facts)
            .ok_or(OptimizationReadinessError::AccessIdentityMismatch)
    }

    pub fn discard_at<'a>(
        &'a self,
        source: &CanonicalCheckedSource,
        expression: &Expr,
    ) -> Result<&'a DiscardOptimizationFacts, OptimizationReadinessError> {
        self.validate_source(source)?;
        let facts = self
            .discards
            .get(&(expression as *const Expr as usize))
            .ok_or(OptimizationReadinessError::MissingDiscardFact)?;
        let Some(expected) = &facts.call_access_identity else {
            return Ok(facts);
        };
        let access = source
            .access_facts()
            .map_err(OptimizationReadinessError::MissingAccessFacts)?;
        let current = access
            .call_at(source.typed_module().module(), expression)
            .ok_or(OptimizationReadinessError::MissingCallFact)?;
        (current.identity_key() == *expected)
            .then_some(facts)
            .ok_or(OptimizationReadinessError::AccessIdentityMismatch)
    }

    pub fn call_count(&self) -> usize { self.calls.len() }
    pub fn discard_count(&self) -> usize { self.discards.len() }
}

fn call_facts(signature: &AccessSignature) -> CallOptimizationFacts {
    CallOptimizationFacts {
        access_identity: signature.identity_key(),
        frozen_parameters: signature
            .params()
            .iter()
            .enumerate()
            .filter_map(|(position, parameter)| {
                parameter.qualifiers().contains(&AccessQualifier::Frozen).then_some(position)
            })
            .collect(),
        frozen_result: signature.result().qualifiers().contains(&AccessQualifier::Frozen),
        effects: call_effects(signature),
    }
}

fn call_effects(signature: &AccessSignature) -> CheckedCallEffects {
    if signature.callable_contract().pure {
        CheckedCallEffects::Pure
    } else {
        CheckedCallEffects::MayHaveObservableEffects
    }
}

fn visit_module_discards(module: &Module, visitor: &mut impl FnMut(&Expr)) {
    for item in &module.items {
        match item {
            Item::Function(function) => visit_block_discards(&function.body, visitor),
            Item::Trait(definition) => {
                for method in &definition.methods {
                    if let Some(default) = &method.default {
                        visit_block_discards(default, visitor);
                    }
                }
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    visit_block_discards(&method.body, visitor);
                }
            }
            Item::Const { value, .. } => visit_expr_discards(value, visitor),
            Item::Comptime(block) => visit_block_discards(block, visitor),
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
}

fn visit_block_discards(block: &Block, visitor: &mut impl FnMut(&Expr)) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { name, value, .. } => {
                if name == "_" {
                    visitor(value);
                }
                visit_expr_discards(value, visitor);
            }
            Stmt::LetPattern { pattern: Pattern::Wildcard, value } => {
                visitor(value);
                visit_expr_discards(value, visitor);
            }
            Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => visit_expr_discards(value, visitor),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn visit_expr_discards(expression: &Expr, visitor: &mut impl FnMut(&Expr)) {
    match expression {
        Expr::List(items)
        | Expr::Tuple(items)
        | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items { visit_expr_discards(item, visitor); }
        }
        Expr::Call { args, .. } | Expr::MethodCall { args, .. } => {
            if let Expr::MethodCall { receiver, .. } = expression {
                visit_expr_discards(receiver, visitor);
            }
            for argument in args { visit_expr_discards(argument, visitor); }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_expr_discards(receiver, visitor);
            for (_, argument) in args { visit_expr_discards(argument, visitor); }
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            visit_expr_discards(receiver, visitor);
            for argument in args { visit_expr_discards(argument, visitor); }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args { visit_expr_discards(argument, visitor); }
        }
        Expr::Apply { func, args } => {
            visit_expr_discards(func, visitor);
            for argument in args { visit_expr_discards(argument, visitor); }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => visit_expr_discards(expr, visitor),
        Expr::Lambda { body, .. } | Expr::Block(body) => visit_block_discards(body, visitor),
        Expr::RecordUpdate { base, fields, .. } => {
            visit_expr_discards(base, visitor);
            for (_, value) in fields { visit_expr_discards(value, visitor); }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields { visit_expr_discards(value, visitor); }
            if let Some(spread) = spread { visit_expr_discards(spread, visitor); }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit_expr_discards(lhs, visitor);
            visit_expr_discards(rhs, visitor);
        }
        Expr::If { cond, then_block, else_block } => {
            visit_expr_discards(cond, visitor);
            visit_block_discards(then_block, visitor);
            if let Some(else_block) = else_block { visit_block_discards(else_block, visitor); }
        }
        Expr::Match { scrutinee, arms } => {
            visit_expr_discards(scrutinee, visitor);
            for arm in arms {
                if let Some(guard) = &arm.guard { visit_expr_discards(guard, visitor); }
                visit_expr_discards(&arm.body, visitor);
            }
        }
        Expr::While { cond, body } => {
            visit_expr_discards(cond, visitor);
            visit_block_discards(body, visitor);
        }
        Expr::For { iter, body, .. } => {
            visit_expr_discards(iter, visitor);
            visit_block_discards(body, visitor);
        }
        Expr::Range { lo, hi, .. } => {
            visit_expr_discards(lo, visitor);
            visit_expr_discards(hi, visitor);
        }
        Expr::Index { base, index } => {
            visit_expr_discards(base, visitor);
            visit_expr_discards(index, visitor);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            visit_expr_discards(scrutinee, visitor);
            visit_block_discards(body, visitor);
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
