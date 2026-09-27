//! Immutable typed payloads and deterministic structural clone costs. This is
//! data, not authority: only admission binds a catalogue to an executable.
//! Serde visits every AST field, including embedded types/defaults. No source
//! text, Debug output, parser invocation, or type-shape whitelist is involved.
use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};
use crate::ast::{Expr, Module};
use super::ComptimeSyntaxOrigin;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyntaxCost { pub nodes: u64, pub bytes: u64 }

impl SyntaxCost {
    pub fn work(self) -> Option<u64> { self.nodes.checked_add(self.bytes) }
}

#[derive(Debug)]
pub struct ExpressionSeed { expression: Expr, cost: SyntaxCost, encoded: Vec<u8>, sites: BTreeSet<u32> }
impl ExpressionSeed {
    pub fn expression(&self) -> &Expr { &self.expression }
    pub fn cost(&self) -> SyntaxCost { self.cost }
    pub fn allows_site(&self, site: u32) -> bool { self.sites.contains(&site) }
}
// Exact serialized payload equality preserves float bit patterns, including
// NaNs. The private AST and these bytes are created together and never mutate.
impl PartialEq for ExpressionSeed {
    fn eq(&self, other: &Self) -> bool { self.encoded == other.encoded && self.cost == other.cost && self.sites == other.sites }
}
impl Eq for ExpressionSeed {}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CompilerSyntaxSeeds { expressions: BTreeMap<String, Arc<ExpressionSeed>> }
impl CompilerSyntaxSeeds {
    /// One bounded preparation pass before retaining any payload. Duplicate
    /// handles preserve the interpreter's last-payload-wins rule.
    pub fn from_module(module: &Module) -> Result<Self, String> {
        #[cfg(target_arch = "wasm32")]
        { let _ = module; Err("canonical compiler syntax seeds require the native compiler".into()) }
        #[cfg(not(target_arch = "wasm32"))]
        {
        use serde::Serialize;
        use measurement::Meter;
        let mut meter = Meter { cost: SyntaxCost::default(), depth: 0 };
        let mut costs = Vec::new();
        for seed in &module.compiler_expr_syntax {
            let before = meter.cost;
            seed.handle.serialize(&mut meter).map_err(|e| e.to_string())?;
            let payload_start = meter.cost;
            seed.expr.serialize(&mut meter).map_err(|e| e.to_string())?;
            costs.push(SyntaxCost { nodes: meter.cost.nodes - payload_start.nodes,
                bytes: meter.cost.bytes - payload_start.bytes });
            debug_assert!(meter.cost.nodes > before.nodes);
        }
        let mut expressions = BTreeMap::<String, ExpressionSeed>::new();
        for (seed, cost) in module.compiler_expr_syntax.iter().zip(costs) {
            let encoded = postcard::to_allocvec(&seed.expr).map_err(|error| error.to_string())?;
            let entry = expressions.entry(seed.handle.clone()).or_insert_with(|| ExpressionSeed {
                expression: Expr::Int(0), cost, encoded: Vec::new(), sites: BTreeSet::new(),
            });
            entry.expression = seed.expr.clone();
            entry.cost = cost;
            entry.encoded = encoded;
            entry.sites.insert(seed.definition_line);
        }
        Ok(Self { expressions: expressions.into_iter().map(|(key, value)| (key, Arc::new(value))).collect() })
        }
    }
    pub fn expression(&self, handle: &str) -> Option<&Arc<ExpressionSeed>> { self.expressions.get(handle) }
}

/// An emitted value keeps its diagnostic provenance beside the typed AST.
/// Callers must still check this AST in its destination context.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedExpression { pub expression: Expr, pub origin: ComptimeSyntaxOrigin }

#[cfg(not(target_arch = "wasm32"))]
mod measurement {
use super::SyntaxCost;
use serde::{Serialize, ser::{self, SerializeSeq}};
#[derive(Debug)]
pub(super) struct Limit;
impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("compiler syntax seed structural limit exceeded") }
}
impl std::error::Error for Limit {}
impl ser::Error for Limit { fn custom<T: std::fmt::Display>(_: T) -> Self { Self } }
pub(super) struct Meter { pub cost: SyntaxCost, pub depth: usize }
impl Meter {
    fn charge(&mut self, bytes: usize) -> Result<(), Limit> {
        self.cost.nodes = self.cost.nodes.checked_add(1).ok_or(Limit)?;
        self.cost.bytes = self.cost.bytes.checked_add(bytes as u64).ok_or(Limit)?;
        if self.cost.nodes > 1_000_000 || self.cost.bytes > 64 * 1024 * 1024 { return Err(Limit); }
        Ok(())
    }
    fn compound(&mut self) -> Result<Compound<'_>, Limit> {
        self.charge(0)?;
        if self.depth >= 128 { return Err(Limit); }
        self.depth += 1;
        Ok(Compound(self))
    }
    fn nested<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Limit> {
        let compound = self.compound()?;
        value.serialize(&mut *compound.0)
    }
}
pub(super) struct Compound<'a>(&'a mut Meter);
impl Drop for Compound<'_> { fn drop(&mut self) { self.0.depth -= 1; } }
macro_rules! scalars {
    ($($method:ident: $ty:ty),* $(,)?) => { $(fn $method(self, _: $ty) -> Result<(), Limit> { self.charge(std::mem::size_of::<$ty>()) })* };
}
impl<'a> ser::Serializer for &'a mut Meter {
    type Ok = (); type Error = Limit;
    type SerializeSeq = Compound<'a>; type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>; type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>; type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;
    scalars!(serialize_bool: bool, serialize_i8: i8, serialize_i16: i16, serialize_i32: i32,
        serialize_i64: i64, serialize_i128: i128, serialize_u8: u8, serialize_u16: u16,
        serialize_u32: u32, serialize_u64: u64, serialize_u128: u128,
        serialize_f32: f32, serialize_f64: f64, serialize_char: char);
    fn serialize_str(self, value: &str) -> Result<(), Limit> { self.charge(value.len()) }
    fn serialize_bytes(self, value: &[u8]) -> Result<(), Limit> { self.charge(value.len()) }
    fn serialize_none(self) -> Result<(), Limit> { self.charge(0) }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Limit> { self.nested(value) }
    fn serialize_unit(self) -> Result<(), Limit> { self.charge(0) }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Limit> { self.charge(0) }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<(), Limit> { self.charge(0) }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Result<(), Limit> { self.nested(value) }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, _: u32, _: &'static str, value: &T) -> Result<(), Limit> { self.nested(value) }
    fn serialize_seq(self, _: Option<usize>) -> Result<Compound<'a>, Limit> { self.compound() }
    fn serialize_tuple(self, _: usize) -> Result<Compound<'a>, Limit> { self.compound() }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Compound<'a>, Limit> { self.compound() }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Compound<'a>, Limit> { self.compound() }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound<'a>, Limit> { self.compound() }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Compound<'a>, Limit> { self.compound() }
    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Compound<'a>, Limit> { self.compound() }
}
macro_rules! sequence {
    ($trait:ident, $method:ident) => {
        impl ser::$trait for Compound<'_> {
            type Ok = (); type Error = Limit;
            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Limit> { value.serialize(&mut *self.0) }
            fn end(self) -> Result<(), Limit> { Ok(()) }
        }
    };
}
sequence!(SerializeSeq, serialize_element);
sequence!(SerializeTuple, serialize_element);
sequence!(SerializeTupleStruct, serialize_field);
sequence!(SerializeTupleVariant, serialize_field);
impl ser::SerializeMap for Compound<'_> {
    type Ok = (); type Error = Limit;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Limit> { self.serialize_element(key) }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Limit> { self.serialize_element(value) }
    fn end(self) -> Result<(), Limit> { Ok(()) }
}
macro_rules! fields {
    ($trait:ident) => {
        impl ser::$trait for Compound<'_> {
            type Ok = (); type Error = Limit;
            fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, value: &T) -> Result<(), Limit> { self.serialize_element(value) }
            fn end(self) -> Result<(), Limit> { Ok(()) }
        }
    };
}
fields!(SerializeStruct);
fields!(SerializeStructVariant);
}
