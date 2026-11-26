//! Borrow-derived alias regions (prototype; `WITCHY_NATIVE_ALIAS=1` when compiling).
//!
//! Cranelift's alias analysis lets a store to one region leave loads of every other region
//! alone. It forwards a load from an earlier identical load or store when no store to that
//! region comes between, and at a loop's header when no store in the loop is to that
//! region. Its contract is the frontend's: one memory location is only ever accessed under
//! one region. In 0.136.3 a store with no region is a barrier to every region (`classify`),
//! but a load with no region is in a region of its own and is forwarded across stores tagged
//! with any region. For that reason a function gets regions on all of its heap accesses or on
//! none. Its own frame's accesses stay untagged, so a store to the frame ends all forwarding.
//!
//! The regions here come from the loan rule. A `&mut` parameter's storage is reached through
//! no other parameter while the call runs, and a `&` parameter's storage is not written while
//! it is lent. Only parameters whose type is a record (or a fixed array) of plain data and
//! dense buffers of plain data are trusted. A dense buffer issues no key, link or weak
//! reference (`body/storage.rs`), so its owner field is the only way to it. Everything such a
//! parameter reaches is then one hop away: the record itself, and the header and payload of
//! each buffer its owner fields name.
//!
//! Each access is classified by where its address comes from, looking through block
//! parameters that only ever receive one value, and through the lane of a tagged join (a
//! `Result` of a reference: a tag and an address) where a test of the tag that excludes every
//! other arm stands between the join and each use of the lane (see `same`). No address is
//! classified from what an access would do on a path where it does not run.
//!
//! * `param + c` for a trusted parameter: that parameter's record.
//! * `heap + f(handle)`: the runtime's slot table, one region. Nothing in a function writes
//!   it; only calls do, and a call is a barrier to every region.
//! * `buffer + c` with `c` below the header's size, or `buffer + HEADER + i`: the header or
//!   payload of the buffer whose pointer `load(slot)` read. It belongs to the parameter whose
//!   owner field, at a known offset, gave the handle that `slot` is computed from.
//! * `stack_addr`: the function's own frame, left with no region (no parameter reaches it).
//!
//! Each `&mut` parameter's record, headers and payloads are regions of its own. All `&`
//! parameters share one set, since two of them may be the same storage (`f(v.at(0), &v)` gives
//! one location as `Record(Shared)` and `Payload(Shared)`): that is harmless only because no store
//! to a shared region is admitted. A buffer belongs to the parameter whose owner field named it,
//! so a buffer must not change owner between two fences: a store that may write an owner field
//! (as `memory.exchange` of two owners does, with plain loads and stores) is refused. A buffer
//! may change owner inside a call (freeing, allocating, growing and moving are all runtime
//! calls), since a call fences every region. A store to a `&` parameter's region, or any access
//! classified otherwise, also leaves the whole function without regions.
//!
//! Cranelift 0.136.3 uses the regions for redundant-load elimination and also for dead-store
//! elimination (a tagged store is dead when no load of its region comes before the next store to
//! the same address), so the regions must be disjoint between fences for both. A record written at
//! a variable offset (a fixed array) relies on the index being in bounds: checked by
//! `index_address`, or proved. The entry block is never a branch target (the verifier refuses it),
//! so an entry-block parameter is the caller's value throughout.
use cranelift_codegen::ir::{self, AliasRegionData, ExternalName, GlobalValueData, InstructionData, Opcode, Value};
use cranelift_codegen::dominator_tree::DominatorTree;
use cranelift_codegen::flowgraph::ControlFlowGraph;
use std::collections::{BTreeMap, BTreeSet};

/// The header's size: a payload begins here (`body/cells.rs` `HEADER`).
const HEADER: i64 = 40;
/// An owner field's size (a handle), and the widest store: a store overlapping an owner field
/// is refused.
const OWNER: i64 = 8;
const WIDEST: i64 = 16;

pub(super) fn enabled() -> bool {
    std::env::var_os("WITCHY_NATIVE_ALIAS").is_some()
}

/// A parameter whose storage the loan rule describes: the byte offsets of its dense owner fields.
#[derive(Clone, Debug)]
pub(super) struct Root {
    pub mutable: bool,
    pub owners: BTreeSet<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Shared,
    Unique(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Region {
    Slots,
    Record(Group),
    Header(Group),
    Payload(Group),
}

/// Why a function was left without regions, or how many accesses it tagged.
pub(super) type Outcome = std::result::Result<usize, String>;

/// Tag every heap access of `function` (after inlining) with its region, or none of them.
pub(super) fn annotate(function: &mut ir::Function, roots: &[(Value, Root)]) -> Outcome {
    let cfg = ControlFlowGraph::with_function(function);
    let domtree = DominatorTree::with_function(function, &cfg);
    let same = same(function, &cfg);
    let heap = heap_symbols(function);
    let roots: BTreeMap<Value, (u32, &Root)> = roots.iter().enumerate().map(|(i, (value, root))| (*value, (i as u32, root))).collect();
    let group = |index: u32, root: &Root| if root.mutable { Group::Unique(index) } else { Group::Shared };
    let analysis = Analysis { steps: std::cell::Cell::new(0), function, cfg: &cfg, domtree: &domtree, same: &same, heap: &heap, tested: Default::default() };

    let mut tagged = Vec::new();
    for block in function.layout.blocks() {
        for inst in function.layout.block_insts(block) {
            let data = &function.dfg.insts[inst];
            let opcode = data.opcode();
            // `bitcast` carries flags (for lane order) and touches no memory.
            if data.memflags().is_none() || !(opcode.can_load() || opcode.can_store()) {
                continue;
            }
            let store = opcode.can_store();
            if !matches!(data, InstructionData::Load { .. } | InstructionData::Store { .. }) {
                return Err(format!("{opcode} is not a plain load or store"));
            }
            let (address, offset) = match *data {
                InstructionData::Load { arg, offset, .. } => (arg, i64::from(i32::from(offset))),
                InstructionData::Store { args, offset, .. } => (args[1], i64::from(i32::from(offset))),
                _ => unreachable!(),
            };
            let Some(place) = analysis.pointer(address, block) else {
                return Err(format!("{inst}: an address with no single base"));
            };
            let constant = place.constant + offset;
            let region = match analysis.base(place.base, place.at) {
                Base::Frame => continue,
                Base::Literal if !store => continue,
                Base::Literal => return Err(format!("{inst}: a store to data")),
                Base::Heap => Region::Slots,
                Base::Param(value) => match roots.get(&value) {
                    // A buffer's header and payload are given to the parameter whose owner field
                    // the handle was read from, which holds only while no owner field is written:
                    // `memory.exchange` of two owners is plain loads and stores, and would put one
                    // buffer under two parameters' regions.
                    Some(&(_, root)) if store && root.owners.iter().any(|&owner| place.variable || (constant < owner + OWNER && owner < constant + WIDEST)) => {
                        return Err(format!("{inst}: a store that may write an owner field"));
                    }
                    Some(&(index, root)) => Region::Record(group(index, root)),
                    None => return Err(format!("{inst}: a parameter the loan rule does not describe")),
                },
                Base::Buffer(handle) => {
                    // The handle is a word read from `owner + load_offset`.
                    let (handle, at) = handle;
                    let handle = analysis.resolve(handle, at);
                    let read = match function.dfg.value_def(handle) {
                        ir::ValueDef::Result(read, _) => match function.dfg.insts[read] {
                            InstructionData::Load { opcode: Opcode::Load, arg, offset, .. } => Some((arg, i64::from(i32::from(offset)))),
                            _ => None,
                        },
                        _ => None,
                    };
                    let Some((field, load_offset, read)) = read.map(|(field, offset)| (field, offset, function.dfg.value_def(handle).unwrap_inst())) else {
                        return Err(format!("{inst}: a handle that is not a word read from memory"));
                    };
                    let read_block = function.layout.inst_block(read).expect("in the layout");
                    let Some(mut owner) = analysis.pointer(field, read_block) else {
                        return Err(format!("{inst}: a handle with no single base"));
                    };
                    owner.load_offset = load_offset;
                    let Base::Param(value) = analysis.base(owner.base, owner.at) else {
                        let def = function.dfg.value_def(analysis.resolve(owner.base, owner.at));
                        let shown = match def {
                            ir::ValueDef::Result(i, _) => format!("{}", function.dfg.display_inst(i)),
                            other => format!("{other:?}"),
                        };
                        return Err(format!("{inst}: a handle not read from a parameter (its base: {shown})"));
                    };
                    let Some(&(index, root)) = roots.get(&value) else {
                        return Err(format!("{inst}: a handle in a parameter the loan rule does not describe"));
                    };
                    if owner.variable || !root.owners.contains(&(owner.constant + owner.load_offset)) {
                        return Err(format!("{inst}: a handle not read from an owner field"));
                    }
                    let group = group(index, root);
                    if !place.variable && constant < HEADER && constant >= 0 {
                        Region::Header(group)
                    } else if constant >= HEADER {
                        Region::Payload(group)
                    } else {
                        return Err(format!("{inst}: a buffer address that is neither header nor payload"));
                    }
                }
                Base::Unknown(what) => return Err(format!("{inst}: {what}")),
            };
            if store && matches!(region, Region::Record(Group::Shared) | Region::Header(Group::Shared) | Region::Payload(Group::Shared)) {
                return Err(format!("{inst}: a store through a shared borrow"));
            }
            if store && region == Region::Slots {
                return Err(format!("{inst}: a store to the slot table"));
            }
            tagged.push((inst, region));
        }
    }

    let mut ids = BTreeMap::new();
    for &(_, region) in &tagged {
        let next = ids.len() as u32;
        ids.entry(region).or_insert(next);
    }
    let regions: BTreeMap<Region, ir::AliasRegion> = ids
        .into_iter()
        .map(|(region, id)| (region, function.dfg.alias_regions.insert(AliasRegionData { user_id: id, description: format!("{region:?}").into() })))
        .collect();
    for &(inst, region) in &tagged {
        let flags = function.dfg.insts[inst].memflags().expect("a memory access");
        let data = function.dfg.mem_flags[flags].clone().with_alias_region(Some(regions[&region]));
        let flags = function.dfg.mem_flags.insert(data).map_err(|_| "too many memory flags".to_string())?;
        *function.dfg.insts[inst].memflags_mut().expect("a memory access") = flags;
    }
    Ok(tagged.len())
}

/// An address: a base, a constant added to it, and whether anything else was added.
struct Place {
    base: Value,
    constant: i64,
    variable: bool,
    /// The block where `base` is used: where a tagged join's lane must have been tested.
    at: ir::Block,
    /// When the base is a loaded value: the offset of the load that gave it (for a handle).
    load_offset: i64,
}

enum Base {
    Param(Value),
    Heap,
    Literal,
    Frame,
    /// A buffer's pointer, read from the slot the given handle names (used in that block).
    Buffer((Value, ir::Block)),
    Unknown(String),
}

struct Analysis<'a> {
    /// Steps taken, so that no shape of address can make the walk go on without end.
    steps: std::cell::Cell<u32>,
    function: &'a ir::Function,
    cfg: &'a ControlFlowGraph,
    domtree: &'a DominatorTree,
    same: &'a BTreeMap<Value, Join>,
    heap: &'a BTreeSet<ir::GlobalValue>,
    /// For a tagged join's lane: the blocks where the tag has been tested (computed once).
    tested: std::cell::RefCell<BTreeMap<Value, BTreeSet<ir::Block>>>,
}

impl Analysis<'_> {
    /// Whether this function's walk has gone on long enough to give up on it.
    fn spent(&self) -> bool {
        self.steps.set(self.steps.get() + 1);
        self.steps.get() > 1_000_000
    }

    /// `value` used in block `at`, through the block parameters it stands for there.
    fn resolve(&self, value: Value, at: ir::Block) -> Value {
        let dfg = &self.function.dfg;
        let mut value = dfg.resolve_aliases(value);
        // Only the first step may be a tagged lane (no parameter is resolved through one).
        if let Some(Join::Tagged { value: next, .. }) = self.same.get(&value) {
            if !self.tested(value, at) {
                return value;
            }
            value = dfg.resolve_aliases(*next);
        }
        for _ in 0..self.same.len() {
            match self.same.get(&value) {
                Some(Join::One(next)) => value = dfg.resolve_aliases(*next),
                _ => break,
            }
        }
        value
    }

    /// Whether the tagged lane `lane`, used in block `at`, is the value it was resolved to: on
    /// every path from the join to `at` (that does not enter the join again), the tag was tested
    /// equal to a value no other arm gives it. The tag is the join's own parameter, so the test
    /// saw the arm of the same entry into the join that gave the lane.
    fn tested(&self, lane: Value, at: ir::Block) -> bool {
        let Some(Join::Tagged { join, tags, .. }) = self.same.get(&lane) else { return false };
        if !self.domtree.block_dominates(*join, at) || at == *join {
            return false;
        }
        let mut cache = self.tested.borrow_mut();
        let passed = cache.entry(lane).or_insert_with(|| self.tests(*join, tags));
        if passed.contains(&at) {
            return true;
        }
        // Every block reached from the join without going through a tested block or the join again.
        let mut seen = BTreeSet::new();
        let mut work: Vec<ir::Block> = self.cfg.succ_iter(*join).collect();
        while let Some(block) = work.pop() {
            if block == *join || passed.contains(&block) || !seen.insert(block) {
                continue;
            }
            if block == at || self.spent() {
                return false;
            }
            work.extend(self.cfg.succ_iter(block));
        }
        true
    }

    /// The blocks entered only when one of `tags` (a tag of `join`, and the values the arms the
    /// lane was not resolved from give it) tested equal to something else: each has one
    /// predecessor, which ends in a two-way branch on that test.
    fn tests(&self, join: ir::Block, tags: &[(Value, BTreeSet<i64>)]) -> BTreeSet<ir::Block> {
        let dfg = &self.function.dfg;
        let mut found = BTreeSet::new();
        for block in self.function.layout.blocks() {
            if !self.domtree.block_dominates(join, block) {
                continue;
            }
            let Some(last) = self.function.layout.last_inst(block) else { continue };
            let InstructionData::Brif { arg, blocks, .. } = dfg.insts[last] else { continue };
            let (then, other) = (blocks[0].block(&dfg.value_lists), blocks[1].block(&dfg.value_lists));
            if then == other {
                continue;
            }
            // (tested value, the constant it equals on the way to `then`, or to `other`)
            let condition = self.condition(arg);
            for (tested, equal, target) in condition.into_iter().flat_map(|(v, k, eq)| [(v, k, if eq { then } else { other })]) {
                let only = self.cfg.pred_iter(target).count() == 1;
                if only && tags.iter().any(|(tag, others)| *tag == tested && !others.contains(&equal)) {
                    found.insert(target);
                }
            }
        }
        found
    }

    /// `brif arg`: the value tested, the constant, and whether `then` is taken when they are
    /// equal (`brif v` is `v != 0`; `icmp eq/ne v, k`, through `uextend`).
    fn condition(&self, arg: Value) -> Option<(Value, i64, bool)> {
        let dfg = &self.function.dfg;
        let arg = dfg.resolve_aliases(arg);
        if let ir::ValueDef::Result(inst, _) = dfg.value_def(arg) {
            match dfg.insts[inst] {
                InstructionData::Unary { opcode: Opcode::Uextend, arg } => return self.condition(arg),
                InstructionData::IntCompare { cond, args, .. } if matches!(cond, ir::condcodes::IntCC::Equal | ir::condcodes::IntCC::NotEqual) => {
                    let eq = cond == ir::condcodes::IntCC::Equal;
                    for (v, k) in [(args[0], args[1]), (args[1], args[0])] {
                        if let Some(k) = self.constant(k) {
                            return Some((dfg.resolve_aliases(v), k, eq));
                        }
                    }
                    return None;
                }
                _ => {}
            }
        }
        // `brif v`: `then` when `v != 0`, so `other` when `v == 0`.
        Some((arg, 0, false))
    }

    fn constant(&self, value: Value) -> Option<i64> {
        let ir::ValueDef::Result(inst, _) = self.function.dfg.value_def(self.function.dfg.resolve_aliases(value)) else {
            return None;
        };
        match self.function.dfg.insts[inst] {
            InstructionData::UnaryImm { opcode: Opcode::Iconst, imm } => Some(imm.bits()),
            _ => None,
        }
    }

    /// `value`, used in block `at`, as a base plus a constant (and whether a non-constant part
    /// was added): the base is the one operand of an addition that is itself an address.
    fn pointer(&self, value: Value, at: ir::Block) -> Option<Place> {
        if self.spent() {
            return None;
        }
        let value = self.resolve(value, at);
        let dfg = &self.function.dfg;
        let add = |mut place: Place, constant: i64, variable: bool| {
            place.constant += constant;
            place.variable |= variable;
            place
        };
        if let ir::ValueDef::Result(inst, _) = dfg.value_def(value) {
            // The operands are used where this instruction is.
            let here = self.function.layout.inst_block(inst).expect("in the layout");
            match dfg.insts[inst] {
                InstructionData::Binary { opcode: Opcode::Iadd, args } => {
                    for (this, other) in [(args[0], args[1]), (args[1], args[0])] {
                        if let Some(c) = self.constant(other) {
                            return Some(add(self.pointer(this, here)?, c, false));
                        }
                    }
                    let based = |v: Value| self.pointer(v, here).filter(|p| !matches!(self.base(p.base, p.at), Base::Unknown(_)));
                    return match (based(args[0]), based(args[1])) {
                        (Some(place), None) => Some(add(place, 0, true)),
                        (None, Some(place)) => Some(add(place, 0, true)),
                        _ => None,
                    };
                }
                InstructionData::Load { opcode: Opcode::Load, offset, .. } => {
                    return Some(Place { base: value, constant: 0, variable: false, at, load_offset: i64::from(i32::from(offset)) });
                }
                _ => {}
            }
        }
        Some(Place { base: value, constant: 0, variable: false, at, load_offset: 0 })
    }

    fn base(&self, value: Value, at: ir::Block) -> Base {
        if self.spent() {
            return Base::Unknown("an address too deep to follow".into());
        }
        let value = self.resolve(value, at);
        let dfg = &self.function.dfg;
        match dfg.value_def(value) {
            ir::ValueDef::Param(block, _) if Some(block) == self.function.layout.entry_block() => Base::Param(value),
            ir::ValueDef::Param(..) => Base::Unknown("a block parameter with several values".into()),
            ir::ValueDef::Result(inst, _) => match dfg.insts[inst] {
                InstructionData::UnaryGlobalValue { opcode: Opcode::SymbolValue, global_value } if self.heap.contains(&global_value) => Base::Heap,
                InstructionData::UnaryGlobalValue { opcode: Opcode::SymbolValue, .. } => Base::Literal,
                InstructionData::StackAddr { opcode: Opcode::StackAddr, .. } => Base::Frame,
                InstructionData::Load { opcode: Opcode::Load, arg, offset, .. } => {
                    // A buffer's pointer is the first word of its slot, `heap + (handle - 1) * 16`.
                    let here = self.function.layout.inst_block(inst).expect("in the layout");
                    let Some(slot) = self.pointer(arg, here) else {
                        return Base::Unknown("a loaded pointer with no single base".into());
                    };
                    if !matches!(self.base(slot.base, slot.at), Base::Heap) || i32::from(offset) != 0 || slot.constant != 0 || !slot.variable {
                        return Base::Unknown("a loaded pointer that is not a buffer's".into());
                    }
                    match self.handle(arg, here) {
                        Some(handle) => Base::Buffer(handle),
                        None => Base::Unknown("a slot not computed from one handle".into()),
                    }
                }
                _ => Base::Unknown(format!("a pointer made by {}", dfg.insts[inst].opcode())),
            },
            _ => Base::Unknown("a value of no definition".into()),
        }
    }

    /// The one loaded value a slot address (used in block `at`) is computed from, besides the
    /// heap and constants, and the block where it is used.
    fn handle(&self, slot: Value, at: ir::Block) -> Option<(Value, ir::Block)> {
        let mut leaves = BTreeSet::new();
        let mut work = vec![(slot, at)];
        let mut steps = 0;
        while let Some((value, at)) = work.pop() {
            steps += 1;
            if steps > 64 {
                return None;
            }
            let value = self.resolve(value, at);
            let ir::ValueDef::Result(inst, _) = self.function.dfg.value_def(value) else {
                return None;
            };
            let here = self.function.layout.inst_block(inst).expect("in the layout");
            match self.function.dfg.insts[inst] {
                InstructionData::UnaryImm { .. } => {}
                InstructionData::UnaryGlobalValue { global_value, .. } if self.heap.contains(&global_value) => {}
                InstructionData::Load { opcode: Opcode::Load, .. } => {
                    leaves.insert((value, at));
                }
                InstructionData::Binary { opcode: Opcode::Iadd | Opcode::Imul | Opcode::Ishl | Opcode::Ushr | Opcode::Band, args } => work.extend(args.map(|a| (a, here))),
                _ => return None,
            }
        }
        // One loaded value (it may be used in more than one block).
        let values: BTreeSet<Value> = leaves.iter().map(|&(v, _)| v).collect();
        (values.len() == 1).then(|| leaves.into_iter().next().expect("one leaf"))
    }
}

/// The global values that name the runtime's heap (the inliner copies the name by index).
fn heap_symbols(function: &ir::Function) -> BTreeSet<ir::GlobalValue> {
    let first = function.global_values.iter().find_map(|(gv, data)| match data {
        GlobalValueData::Symbol { name: ExternalName::User(reference), .. } => Some((gv, function.params.user_named_funcs()[*reference].clone())),
        _ => None,
    });
    let Some((_, heap)) = first else { return BTreeSet::new() };
    function
        .global_values
        .iter()
        .filter(|(_, data)| matches!(data, GlobalValueData::Symbol { name: ExternalName::User(reference), .. } if function.params.user_named_funcs()[*reference] == heap))
        .map(|(gv, _)| gv)
        .collect()
}

/// What a block parameter stands for.
#[derive(Debug)]
enum Join {
    /// It only ever receives this value (or itself).
    One(Value),
    /// The lane of a tagged join: some arms give it `value`, and on each other arm
    /// each of `tags` (other parameters of `join`) receives a constant. Where the use of the lane
    /// is reached only through a test that a tag equals none of its other arms' constants, the
    /// lane is `value` (`Analysis::tested`). Nothing is assumed of the lane on the other arms.
    Tagged { value: Value, join: ir::Block, tags: Vec<(Value, BTreeSet<i64>)> },
}

/// Block parameters that only ever receive one value (or themselves), and the lanes of tagged
/// joins, mapped to what they stand for.
///
/// A parameter never stands for a constant: two arms' constants are not one value. Lanes are
/// classified only after the single-value parameters, so no parameter is resolved *through* a
/// lane. A parameter may still stand for a lane (it receives the lane itself); `resolve` then
/// stops there, and the lane is taken further only by a later `resolve` of it where it is used,
/// with its own test. That use is dominated by the parameter's block, which is dominated by the
/// lane's join, so every path from the lane's last entry to the use passes the test.
fn same(function: &ir::Function, cfg: &ControlFlowGraph) -> BTreeMap<Value, Join> {
    let dfg = &function.dfg;
    // For each block: each incoming edge's arguments (`None` where the pass cannot see one).
    let mut edges: BTreeMap<ir::Block, Vec<Vec<Option<Value>>>> = BTreeMap::new();
    for block in function.layout.blocks() {
        for predecessor in cfg.pred_iter(block) {
            let data = &dfg.insts[predecessor.inst];
            for call in data.branch_destination(&dfg.jump_tables, &dfg.exception_tables) {
                if call.block(&dfg.value_lists) != block {
                    continue;
                }
                let arguments = call.args(&dfg.value_lists).map(|a| a.as_value().map(|v| dfg.resolve_aliases(v))).collect();
                edges.entry(block).or_default().push(arguments);
            }
        }
    }
    let constant = |value: Value| match dfg.value_def(value) {
        ir::ValueDef::Result(inst, _) => match dfg.insts[inst] {
            InstructionData::UnaryImm { opcode: Opcode::Iconst, imm } => Some(imm.bits()),
            _ => None,
        },
        _ => None,
    };
    // What each edge into `block` gives its parameter `index`, through parameters that receive
    // one value; `None` when an edge's argument is unseen (or, after phase 1, a tagged lane).
    let given = |same: &BTreeMap<Value, Join>, block: ir::Block, index: usize| -> Option<Vec<Value>> {
        edges[&block]
            .iter()
            .map(|arguments| {
                let mut value = arguments.get(index).copied().flatten()?;
                for _ in 0..=same.len() {
                    match same.get(&value) {
                        Some(Join::One(next)) => value = *next,
                        Some(Join::Tagged { .. }) => return None,
                        None => break,
                    }
                }
                Some(value)
            })
            .collect()
    };
    // First every parameter that receives one value, to a fixed point; only then tagged lanes,
    // so that a parameter is never taken for a lane while what it receives is still unresolved
    // (a loop header's parameter receives itself, through others, on its back edge).
    let mut same: BTreeMap<Value, Join> = BTreeMap::new();
    loop {
        let mut changed = false;
        for &block in edges.keys() {
            for (index, &param) in dfg.block_params(block).iter().enumerate() {
                if same.contains_key(&param) {
                    continue;
                }
                let Some(given) = given(&same, block, index) else { continue };
                let values: BTreeSet<Value> = given.into_iter().filter(|&v| v != param).collect();
                if values.len() == 1 {
                    let value = *values.iter().next().expect("one");
                    if constant(value).is_none() {
                        same.insert(param, Join::One(value));
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    // A tagged lane: one value that is no constant, and on every other arm a constant for some
    // other parameter of the join.
    let mut lanes = BTreeMap::new();
    for (&block, arrivals) in &edges {
        let params = dfg.block_params(block);
        for (index, &param) in params.iter().enumerate() {
            if same.contains_key(&param) {
                continue;
            }
            let Some(given) = given(&same, block, index) else { continue };
            let values: BTreeSet<Value> = given.iter().copied().filter(|&v| v != param).collect();
            let mut found = None;
            // The value may not be a block parameter of the body: one entry into the join can
            // copy it, and its own block can be entered again before the lane is used, giving it
            // another value (and another lane's test something else to pass) while the lane keeps
            // the copy. A value an instruction computed is fixed, and anything it was computed
            // from is checked where that instruction is.
            let fixed = |v: Value| match dfg.value_def(v) {
                ir::ValueDef::Param(block, _) => Some(block) == function.layout.entry_block(),
                _ => true,
            };
            for &value in values.iter().filter(|&&v| constant(v).is_none() && fixed(v)) {
                // Every edge that does not give `value` is another arm, including one that carries
                // the lane back unchanged: it may set the tag afresh while the lane keeps what an
                // earlier entry gave it. Only an edge that carries the tag back unchanged as well
                // keeps the two together, and needs no constant.
                let others: Vec<usize> = (0..arrivals.len()).filter(|&e| given[e] != value).collect();
                let tags: Vec<(Value, BTreeSet<i64>)> = params
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i != index)
                    .filter_map(|(i, &tag)| {
                        let constants: Option<BTreeSet<i64>> = others
                            .iter()
                            .filter(|&&e| !(given[e] == param && arrivals[e].get(i).copied().flatten() == Some(tag)))
                            .map(|&e| arrivals[e].get(i).copied().flatten().and_then(constant))
                            .collect();
                        constants.map(|c| (tag, c))
                    })
                    .collect();
                if !tags.is_empty() {
                    // Two values could each be "the" value: then neither is.
                    if found.is_some() {
                        found = None;
                        break;
                    }
                    found = Some(Join::Tagged { value, join: block, tags });
                }
            }
            if let Some(join) = found {
                lanes.insert(param, join);
            }
        }
    }
    same.extend(lanes);
    same
}

#[cfg(test)]
#[path = "alias_tests.rs"]
mod tests;
