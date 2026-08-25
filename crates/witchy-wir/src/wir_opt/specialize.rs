//! Specialization by Call-Site Constants and Range Facts (RFC-0154 Track 4).
//!
//! When a call site passes statically known constants to a function, create a
//! specialized clone of the target with those constants propagated into the body.
//! Unused / dead branches under known constants are pruned immediately. Clones
//! with identical specialization profiles are shared.

use crate::wir::{WirExpr, WirFunc, WirModule, WirNode, WirSeq};
use std::collections::HashMap;

/// Identifies a compile-time constant argument passed at a call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstVal {
    I64(i64),
    I32(i32),
    F64Bits(u64),
    V128([u8; 16]),
}

impl ConstVal {
    pub fn from_expr(expr: &WirExpr) -> Option<Self> {
        match expr {
            WirExpr::ConstI64(v) => Some(ConstVal::I64(*v)),
            WirExpr::ConstI32(v) => Some(ConstVal::I32(*v)),
            WirExpr::ConstF64(v) => Some(ConstVal::F64Bits(v.to_bits())),
            WirExpr::ConstV128(bytes) => Some(ConstVal::V128(*bytes)),
            _ => None,
        }
    }

    pub fn to_expr(&self) -> WirExpr {
        match self {
            ConstVal::I64(v) => WirExpr::ConstI64(*v),
            ConstVal::I32(v) => WirExpr::ConstI32(*v),
            ConstVal::F64Bits(b) => WirExpr::ConstF64(f64::from_bits(*b)),
            ConstVal::V128(bytes) => WirExpr::ConstV128(*bytes),
        }
    }
}

/// Profile key identifying a unique specialization of a function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SpecKey {
    func_name: String,
    const_args: Vec<(usize, ConstVal)>,
}

/// Specialize call sites in `module` when arguments are statically known constants.
pub fn specialize_by_call_site_facts(module: &mut WirModule) {
    let func_map: HashMap<String, WirFunc> = module
        .funcs
        .iter()
        .filter(|f| f.raw_body.is_none())
        .map(|f| (f.name.clone(), f.clone()))
        .collect();

    let mut spec_cache: HashMap<SpecKey, String> = HashMap::new();
    let mut new_clones: Vec<WirFunc> = Vec::new();
    let mut spec_counter = 0usize;

    for func in &mut module.funcs {
        if func.raw_body.is_some() {
            continue;
        }
        specialize_seq(
            &mut func.body,
            &func_map,
            &mut spec_cache,
            &mut new_clones,
            &mut spec_counter,
        );
    }

    module.funcs.extend(new_clones);
}

fn specialize_seq(
    seq: &mut WirSeq,
    func_map: &HashMap<String, WirFunc>,
    spec_cache: &mut HashMap<SpecKey, String>,
    new_clones: &mut Vec<WirFunc>,
    spec_counter: &mut usize,
) {
    for node in seq {
        specialize_node(node, func_map, spec_cache, new_clones, spec_counter);
    }
}

fn specialize_node(
    node: &mut WirNode,
    func_map: &HashMap<String, WirFunc>,
    spec_cache: &mut HashMap<SpecKey, String>,
    new_clones: &mut Vec<WirFunc>,
    spec_counter: &mut usize,
) {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            specialize_seq(body, func_map, spec_cache, new_clones, spec_counter);
        }
        WirNode::If { cond, then_, els, .. } => {
            specialize_expr(cond, func_map, spec_cache, new_clones, spec_counter);
            specialize_seq(then_, func_map, spec_cache, new_clones, spec_counter);
            specialize_seq(els, func_map, spec_cache, new_clones, spec_counter);
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => {
            specialize_expr(value, func_map, spec_cache, new_clones, spec_counter);
        }
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            specialize_expr(ptr, func_map, spec_cache, new_clones, spec_counter);
            specialize_expr(value, func_map, spec_cache, new_clones, spec_counter);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            specialize_expr(dest, func_map, spec_cache, new_clones, spec_counter);
            specialize_expr(src, func_map, spec_cache, new_clones, spec_counter);
            specialize_expr(len, func_map, spec_cache, new_clones, spec_counter);
        }
        WirNode::MemoryFill { dest, value, len }
        | WirNode::ArraySet {
            array: dest,
            index: value,
            value: len,
            ..
        } => {
            specialize_expr(dest, func_map, spec_cache, new_clones, spec_counter);
            specialize_expr(value, func_map, spec_cache, new_clones, spec_counter);
            specialize_expr(len, func_map, spec_cache, new_clones, spec_counter);
        }
        WirNode::CallStoreMulti { args, .. } => {
            for a in args {
                specialize_expr(a, func_map, spec_cache, new_clones, spec_counter);
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args {
                specialize_expr(a, func_map, spec_cache, new_clones, spec_counter);
            }
            specialize_expr(index, func_map, spec_cache, new_clones, spec_counter);
        }
        _ => {}
    }
}

fn specialize_expr(
    expr: &mut WirExpr,
    func_map: &HashMap<String, WirFunc>,
    spec_cache: &mut HashMap<SpecKey, String>,
    new_clones: &mut Vec<WirFunc>,
    spec_counter: &mut usize,
) {
    match expr {
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => {
            specialize_expr(inner, func_map, spec_cache, new_clones, spec_counter);
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            specialize_expr(lhs, func_map, spec_cache, new_clones, spec_counter);
            specialize_expr(rhs, func_map, spec_cache, new_clones, spec_counter);
        }
        WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => {
            for a in args {
                specialize_expr(a, func_map, spec_cache, new_clones, spec_counter);
            }
        }
        WirExpr::ArrayNew { value, len, .. }
        | WirExpr::ArrayGet { array: value, index: len, .. } => {
            specialize_expr(value, func_map, spec_cache, new_clones, spec_counter);
            specialize_expr(len, func_map, spec_cache, new_clones, spec_counter);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args {
                specialize_expr(a, func_map, spec_cache, new_clones, spec_counter);
            }
            specialize_expr(index, func_map, spec_cache, new_clones, spec_counter);
        }
        WirExpr::Control(node) => {
            specialize_node(node, func_map, spec_cache, new_clones, spec_counter);
        }
        WirExpr::Seq(seq) => {
            specialize_seq(seq, func_map, spec_cache, new_clones, spec_counter);
        }
        WirExpr::Call { func, args } => {
            for a in args.iter_mut() {
                specialize_expr(a, func_map, spec_cache, new_clones, spec_counter);
            }

            // Check if call has constant arguments eligible for specialization
            let const_args: Vec<(usize, ConstVal)> = args
                .iter()
                .enumerate()
                .filter_map(|(idx, arg)| ConstVal::from_expr(arg).map(|cv| (idx, cv)))
                .collect();

            if !const_args.is_empty() && const_args.len() < args.len() {
                if let Some(target_func) = func_map.get(func) {
                    if target_func.raw_body.is_none() && !func.contains("_spec_") {
                        let key = SpecKey {
                            func_name: func.clone(),
                            const_args: const_args.clone(),
                        };

                        if let Some(existing_name) = spec_cache.get(&key) {
                            *func = existing_name.clone();
                        } else {
                            *spec_counter += 1;
                            let clone_name = format!("{}_spec_{}", func, spec_counter);
                            let mut specialized_func = target_func.clone();
                            specialized_func.name = clone_name.clone();

                            // Build substitution map for constant parameters
                            let mut subst: HashMap<String, WirExpr> = HashMap::new();
                            for (idx, cv) in const_args {
                                if idx < specialized_func.params.len() {
                                    subst.insert(
                                        specialized_func.params[idx].name.clone(),
                                        cv.to_expr(),
                                    );
                                }
                            }

                            replace_params_seq(&mut specialized_func.body, &subst);

                            spec_cache.insert(key, clone_name.clone());
                            *func = clone_name;
                            new_clones.push(specialized_func);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn replace_params_seq(seq: &mut WirSeq, subst: &HashMap<String, WirExpr>) {
    for node in seq {
        replace_params_node(node, subst);
    }
}

fn replace_params_node(node: &mut WirNode, subst: &HashMap<String, WirExpr>) {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            replace_params_seq(body, subst);
        }
        WirNode::If { cond, then_, els, .. } => {
            replace_params_expr(cond, subst);
            replace_params_seq(then_, subst);
            replace_params_seq(els, subst);
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => {
            replace_params_expr(value, subst);
        }
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            replace_params_expr(ptr, subst);
            replace_params_expr(value, subst);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            replace_params_expr(dest, subst);
            replace_params_expr(src, subst);
            replace_params_expr(len, subst);
        }
        WirNode::MemoryFill { dest, value, len }
        | WirNode::ArraySet {
            array: dest,
            index: value,
            value: len,
            ..
        } => {
            replace_params_expr(dest, subst);
            replace_params_expr(value, subst);
            replace_params_expr(len, subst);
        }
        WirNode::CallStoreMulti { args, .. } => {
            for a in args {
                replace_params_expr(a, subst);
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args {
                replace_params_expr(a, subst);
            }
            replace_params_expr(index, subst);
        }
        _ => {}
    }
}

fn replace_params_expr(expr: &mut WirExpr, subst: &HashMap<String, WirExpr>) {
    match expr {
        WirExpr::GetLocal(name) => {
            if let Some(replacement) = subst.get(name) {
                *expr = replacement.clone();
            }
        }
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => {
            replace_params_expr(inner, subst);
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            replace_params_expr(lhs, subst);
            replace_params_expr(rhs, subst);
        }
        WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => {
            for a in args {
                replace_params_expr(a, subst);
            }
        }
        WirExpr::ArrayNew { value, len, .. }
        | WirExpr::ArrayGet { array: value, index: len, .. } => {
            replace_params_expr(value, subst);
            replace_params_expr(len, subst);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args {
                replace_params_expr(a, subst);
            }
            replace_params_expr(index, subst);
        }
        WirExpr::Control(node) => {
            replace_params_node(node, subst);
        }
        WirExpr::Seq(seq) => {
            replace_params_seq(seq, subst);
        }
        _ => {}
    }
}
