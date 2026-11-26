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
//! parameters that only ever receive one value:
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
//! parameters share one set, since two of them may be the same storage. A store to a `&`
//! parameter's region, or any access classified otherwise, leaves the whole function without
//! regions.
use cranelift_codegen::ir::{self, AliasRegionData, ExternalName, GlobalValueData, InstructionData, Opcode, Value};
use cranelift_codegen::dominator_tree::DominatorTree;
use cranelift_codegen::flowgraph::ControlFlowGraph;
use std::collections::{BTreeMap, BTreeSet};

/// The header's size: a payload begins here (`body/cells.rs` `HEADER`).
const HEADER: i64 = 40;

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
    let same = same(function, &cfg);
    let heap = heap_symbols(function);
    let roots: BTreeMap<Value, (u32, &Root)> = roots.iter().enumerate().map(|(i, (value, root))| (*value, (i as u32, root))).collect();
    let group = |index: u32, root: &Root| if root.mutable { Group::Unique(index) } else { Group::Shared };
    let analysis = Analysis { steps: std::cell::Cell::new(0), function, same: &same, heap: &heap };

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
            let Some(place) = analysis.pointer(address) else {
                return Err(format!("{inst}: an address with no single base"));
            };
            let constant = place.constant + offset;
            // Through a lane whose other arm put a filler there: on that arm this access is at
            // the filler (below 4096) plus what was added after the join, which must be a
            // constant below 4096, so the access would fault without any region. The pass
            // relies on the zero page being unmapped (it is the first 4 GiB on macOS ARM64).
            if let Some((after, variable)) = place.filler
                && (variable || !(0..FILLER_LIMIT).contains(&(after + offset)))
            {
                return Err(format!("{inst}: an access through a lane with a filler, at more than a small constant past it"));
            }
            let region = match analysis.base(place.base) {
                Base::Frame => continue,
                Base::Literal if !store => continue,
                Base::Literal => return Err(format!("{inst}: a store to data")),
                Base::Heap => Region::Slots,
                Base::Param(value) => match roots.get(&value) {
                    Some(&(index, root)) => Region::Record(group(index, root)),
                    None => return Err(format!("{inst}: a parameter the loan rule does not describe")),
                },
                Base::Buffer(handle) => {
                    // The handle is a word read from `owner + load_offset`.
                    let (handle, through) = analysis.resolve(handle);
                    if through {
                        return Err(format!("{inst}: a handle through a lane with a filler"));
                    }
                    let read = match function.dfg.value_def(handle) {
                        ir::ValueDef::Result(read, _) => match function.dfg.insts[read] {
                            InstructionData::Load { opcode: Opcode::Load, arg, offset, .. } => Some((arg, i64::from(i32::from(offset)))),
                            _ => None,
                        },
                        _ => None,
                    };
                    let Some((field, load_offset)) = read else {
                        return Err(format!("{inst}: a handle that is not a word read from memory"));
                    };
                    let Some(mut owner) = analysis.pointer(field) else {
                        return Err(format!("{inst}: a handle with no single base"));
                    };
                    if owner.filler.is_some() {
                        return Err(format!("{inst}: a handle read through a lane with a filler"));
                    }
                    owner.load_offset = load_offset;
                    let Base::Param(value) = analysis.base(owner.base) else {
                        let def = function.dfg.value_def(analysis.resolve(owner.base).0);
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

    // Each inlined callee brought its own copy of the heap's name; with one, the same slot is
    // the same address value and Cranelift can see a second load of it is the first.
    if let Some(&first) = heap.iter().next() {
        for block in function.layout.blocks().collect::<Vec<_>>() {
            for inst in function.layout.block_insts(block).collect::<Vec<_>>() {
                if let InstructionData::UnaryGlobalValue { opcode: Opcode::SymbolValue, global_value } = &mut function.dfg.insts[inst]
                    && heap.contains(global_value)
                {
                    *global_value = first;
                }
            }
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

/// The largest filler, and the most an access may add past one: both stay in the zero page.
const FILLER_LIMIT: i64 = 4096;

/// An address: a base, a constant added to it, and whether anything else was added.
struct Place {
    base: Value,
    constant: i64,
    variable: bool,
    /// When the address passed through a lane joined with a filler: the constant and whether
    /// anything else was added after that join.
    filler: Option<(i64, bool)>,
    /// When the base is a loaded value: the offset of the load that gave it (for a handle).
    load_offset: i64,
}

enum Base {
    Param(Value),
    Heap,
    Literal,
    Frame,
    /// A buffer's pointer, read from the slot the given handle names.
    Buffer(Value),
    Unknown(String),
}

struct Analysis<'a> {
    /// Steps taken, so that no shape of address can make the walk go on without end.
    steps: std::cell::Cell<u32>,
    function: &'a ir::Function,
    same: &'a BTreeMap<Value, (Value, bool)>,
    heap: &'a BTreeSet<ir::GlobalValue>,
}

impl Analysis<'_> {
    /// Whether this function's walk has gone on long enough to give up on it.
    fn spent(&self) -> bool {
        self.steps.set(self.steps.get() + 1);
        self.steps.get() > 1_000_000
    }

    /// `value` through the block parameters that receive one value, and whether a filler was
    /// passed over on the way.
    fn resolve(&self, value: Value) -> (Value, bool) {
        let mut value = self.function.dfg.resolve_aliases(value);
        let mut through = false;
        for _ in 0..self.same.len() {
            match self.same.get(&value) {
                Some(&(next, filled)) => {
                    through |= filled;
                    value = self.function.dfg.resolve_aliases(next);
                }
                None => break,
            }
        }
        (value, through)
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

    /// `value` as a base plus a constant (and whether a non-constant part was added): the base
    /// is the one operand of an addition that is itself an address.
    fn pointer(&self, value: Value) -> Option<Place> {
        if self.spent() {
            return None;
        }
        let (value, through) = self.resolve(value);
        let mut place = self.decompose(value)?;
        if through {
            // What was found below this join is before it; nothing is added after it yet. A
            // second filler on the way is more than this argument covers.
            if place.filler.is_some() {
                return None;
            }
            place.filler = Some((0, false));
        }
        Some(place)
    }

    fn decompose(&self, value: Value) -> Option<Place> {
        let dfg = &self.function.dfg;
        let add = |mut place: Place, constant: i64, variable: bool| {
            place.constant += constant;
            place.variable |= variable;
            if let Some((after, more)) = place.filler.as_mut() {
                *after += constant;
                *more |= variable;
            }
            place
        };
        if let ir::ValueDef::Result(inst, _) = dfg.value_def(value) {
            match dfg.insts[inst] {
                InstructionData::Binary { opcode: Opcode::Iadd, args } => {
                    for (this, other) in [(args[0], args[1]), (args[1], args[0])] {
                        if let Some(c) = self.constant(other) {
                            return Some(add(self.pointer(this)?, c, false));
                        }
                    }
                    let based = |v: Value| self.pointer(v).filter(|p| !matches!(self.base(p.base), Base::Unknown(_)));
                    return match (based(args[0]), based(args[1])) {
                        (Some(place), None) | (None, Some(place)) => Some(add(place, 0, true)),
                        _ => None,
                    };
                }
                InstructionData::Load { opcode: Opcode::Load, offset, .. } => {
                    return Some(Place { base: value, constant: 0, variable: false, filler: None, load_offset: i64::from(i32::from(offset)) });
                }
                _ => {}
            }
        }
        Some(Place { base: value, constant: 0, variable: false, filler: None, load_offset: 0 })
    }

    fn base(&self, value: Value) -> Base {
        if self.spent() {
            return Base::Unknown("an address too deep to follow".into());
        }
        let (value, through) = self.resolve(value);
        if through {
            return Base::Unknown("a base through a lane with a filler".into());
        }
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
                    let Some(slot) = self.pointer(arg) else {
                        return Base::Unknown("a loaded pointer with no single base".into());
                    };
                    if slot.filler.is_some() || !matches!(self.base(slot.base), Base::Heap) || i32::from(offset) != 0 || slot.constant != 0 || !slot.variable {
                        return Base::Unknown("a loaded pointer that is not a buffer's".into());
                    }
                    match self.handle(arg) {
                        Some(handle) => Base::Buffer(handle),
                        None => Base::Unknown("a slot not computed from one handle".into()),
                    }
                }
                _ => Base::Unknown(format!("a pointer made by {}", dfg.insts[inst].opcode())),
            },
            _ => Base::Unknown("a value of no definition".into()),
        }
    }

    /// The one loaded value a slot address is computed from, besides the heap and constants.
    fn handle(&self, slot: Value) -> Option<Value> {
        let mut leaves = BTreeSet::new();
        let mut work = vec![slot];
        let mut steps = 0;
        while let Some(value) = work.pop() {
            steps += 1;
            if steps > 64 {
                return None;
            }
            let (value, through) = self.resolve(value);
            if through {
                return None;
            }
            let ir::ValueDef::Result(inst, _) = self.function.dfg.value_def(value) else {
                return None;
            };
            match self.function.dfg.insts[inst] {
                InstructionData::UnaryImm { .. } => {}
                InstructionData::UnaryGlobalValue { global_value, .. } if self.heap.contains(&global_value) => {}
                InstructionData::Load { opcode: Opcode::Load, .. } => {
                    leaves.insert(value);
                }
                InstructionData::Binary { opcode: Opcode::Iadd | Opcode::Imul | Opcode::Ishl | Opcode::Ushr | Opcode::Band, args } => work.extend(args),
                _ => return None,
            }
        }
        (leaves.len() == 1).then(|| leaves.into_iter().next().expect("one leaf"))
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

/// Block parameters that only ever receive one value (or themselves), mapped to it.
///
/// A parameter never resolves to a constant: two arms' constants are not one value.
///
/// A constant (below 4096) received besides that one value is passed over when it is the filler of a lane
/// that only means something on another arm: a `Result` of a reference is a tag and an
/// address, and its `Err` arm puts a constant where the address goes. It is a filler only if
/// it comes from a block that does not dominate the join (so a path to the join need not pass
/// it), on an edge that is not a back edge, and the value it stands beside comes on forward
/// edges only. A loop counter that starts at a constant is therefore not a filler. An access
/// through the lane runs on the arm where it is the address; one made with the filler would be
/// an access to a constant address, already wrong without any region. Native lowering makes no
/// access through a constant address (an empty text has a data symbol of its own).
fn same(function: &ir::Function, cfg: &ControlFlowGraph) -> BTreeMap<Value, (Value, bool)> {
    let dfg = &function.dfg;
    let domtree = DominatorTree::with_function(function, cfg);
    // For each parameter: each value it receives, and whether that edge may pass it over as a
    // filler (forward, from a block that does not dominate the join) or is a back edge.
    let mut incoming: BTreeMap<Value, Vec<(Value, bool, bool)>> = BTreeMap::new();
    // Parameters with an argument the pass cannot see: never resolved.
    let mut blind = BTreeSet::new();
    for block in function.layout.blocks() {
        for predecessor in cfg.pred_iter(block) {
            let back = domtree.block_dominates(block, predecessor.block);
            let aside = !back && !domtree.block_dominates(predecessor.block, block);
            let data = &dfg.insts[predecessor.inst];
            for call in data.branch_destination(&dfg.jump_tables, &dfg.exception_tables) {
                if call.block(&dfg.value_lists) != block {
                    continue;
                }
                for (param, argument) in dfg.block_params(block).iter().zip(call.args(&dfg.value_lists)) {
                    match argument.as_value() {
                        Some(argument) => incoming.entry(*param).or_default().push((dfg.resolve_aliases(argument), aside, back)),
                        None => {
                            blind.insert(*param);
                        }
                    }
                }
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
    let mut same = BTreeMap::new();
    loop {
        let mut changed = false;
        for (&param, sources) in &incoming {
            if same.contains_key(&param) || blind.contains(&param) {
                continue;
            }
            let (mut values, mut fillers, mut backward) = (BTreeSet::new(), 0, false);
            for &(source, aside, back) in sources {
                let mut value = source;
                for _ in 0..=same.len() {
                    match same.get(&value) {
                        Some(&(next, _)) => value = next,
                        None => break,
                    }
                }
                if value == param {
                    continue;
                }
                if aside && constant(value).is_some_and(|c| (0..FILLER_LIMIT).contains(&c)) {
                    fillers += 1;
                    continue;
                }
                backward |= back;
                values.insert(value);
            }
            // Never to a constant: a constant is no storage, and two arms' constants are not one.
            let one = (values.len() == 1).then(|| *values.iter().next().expect("one source")).filter(|&value| constant(value).is_none());
            if let Some(value) = one
                && !(fillers > 0 && backward)
            {
                same.insert(param, (value, fillers > 0));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    same
}

#[cfg(test)]
#[path = "alias_tests.rs"]
mod tests;
