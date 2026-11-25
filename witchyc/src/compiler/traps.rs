//! `witchyc wasm-traps program.wy`: how each reachable function can stop the run with a
//! trap, from the program as it is emitted (instances resolved, so a trait method is the
//! implementation that runs). A trap is a program's fault: an operation that has no answer
//! (`U64` overflow, division by zero, an index past the end), `unreachable` reached, or a
//! task used wrongly. Running out of stack, fuel or time is the run's limit, not the
//! program's fault, and is not listed. Nothing here is trusted by a build.
use super::ir::{Expr, Program, Stmt, Value};
use super::operations::{Binary, Signed};
use super::pause::{children, places};
use super::place::{Place, Projection};
use std::collections::{BTreeMap, BTreeSet};

/// The traps a value can raise itself, not through a call.
/// What the function's accepted proof removed the checks of: its unsigned `+ - *`, its fixed
/// array indexes (`proved_index`), and its slice reads (`proved_slices`, review of proved slice
/// reads: those were still listed as traps).
#[derive(Clone, Copy)]
struct Proved { arithmetic: bool, index: bool, slices: bool, unreachable: bool }
/// Whether an array index is in range as written: a number below the length (`values[4]` of
/// `[U64; 5]`), the index site the coverage scan skips (`scalar_vc/scan.rs`).
fn literal_below(index: &Expr, length: u32) -> bool { matches!(index.kind, Value::U64(k) if k < length as u64) }
/// A place reached through an array index is tested against the length wherever it is used
/// (read, written, moved out of, lent, exchanged, printed through: emit.rs `checked_index`, the
/// one site that emits the test for a place). Review 2026-11-21: only stores counted it, so
/// `return arr[n]`, `&arr[n]` and `memory.exchange(&mut arr[n], ..)` passed `nopanic` and trapped.
/// It cannot trap only when the function's every index is proved (`proved_index`, which is also
/// exactly when the test is not emitted) or each index is a number below the length.
fn place(place: &Place, proved: Proved, out: &mut BTreeSet<&'static str>, calls: &mut BTreeSet<u32>) {
    for projection in &place.projections {
        let Projection::Index { value, length, .. } = projection else { continue };
        if !proved.index && !literal_below(value, *length) { out.insert("index out of range"); }
        own(value, proved, out, calls);
    }
}
fn own(e: &Expr, proved: Proved, out: &mut BTreeSet<&'static str>, calls: &mut BTreeSet<u32>) {
    match &e.kind {
        Value::Binary(Binary::Add | Binary::Sub | Binary::Mul, ..) if proved.arithmetic => {}
        Value::Index(..) if proved.index => {}
        Value::Index(_, at, length) if literal_below(at, *length) => {}
        Value::SliceIndex(..) | Value::SliceWithin(..) | Value::SliceReadLe64(..) if proved.slices => {}
        // Below the length by construction (a counted `for` up to the slice's length).
        Value::SliceIndex(_, _, _, false) => {}
        Value::Binary(Binary::Add | Binary::Sub | Binary::Mul, ..) => { out.insert("overflow"); }
        // By a number other than zero, written as such: no answer is missing.
        Value::Binary(Binary::Div | Binary::Rem, _, by) if matches!(by.kind, Value::U64(n) if n != 0) => {}
        Value::Binary(Binary::Div | Binary::Rem, ..) => { out.insert("division by zero"); }
        Value::Signed(Signed::Add | Signed::Sub | Signed::Mul, _) => { out.insert("overflow"); }
        Value::Float(super::ir::Float::ToI64, _) => { out.insert("overflow"); }
        // A shift by the width or more. By a number below the width, written as such, there is
        // always an answer (as for a division by a number other than zero).
        Value::Bits(super::ir::Bits::Shl | super::ir::Bits::Shr, operands) if matches!((&operands[1].kind, e.ty), (Value::U64(by), super::ir::Ty::Unsigned(scalar)) if *by < 64 - u64::from(scalar.layout().maximum().leading_zeros())) => {}
        Value::Bits(super::ir::Bits::Shl | super::ir::Bits::Shr, _) => { out.insert("overflow"); }
        Value::Signed(Signed::Div | Signed::Rem, _) => { out.insert("division by zero"); out.insert("overflow"); }
        Value::Index(..) | Value::SliceIndex(..) | Value::SlicePart(..) | Value::SliceWithin(..) | Value::SliceCopy(..) | Value::SliceReadLe64(..) | Value::SliceAt(_, _, _, super::logical_body::Reach::Trap) => { out.insert("index out of range"); }
        Value::TaskSpawn { entry, .. } => {
            out.insert("task misuse");
            if let Value::Call(f, _) = &entry.kind { calls.insert(*f); }
        }
        Value::TaskStep(..) | Value::TaskFinish(..) => { out.insert("task misuse"); }
        // Host operations the host refuses for what the program passed: a group slot that holds
        // no socket, more entries than the host waits on (review round 32).
        Value::Host(super::host::HostOp::GroupTake | super::host::HostOp::NetPoll | super::host::HostOp::NetWait, ..) => { out.insert("host refusal"); }
        Value::Call(f, _) => { calls.insert(*f); }
        _ => {}
    }
    for child in children(&e.kind) { own(child, proved, out, calls); }
    for at in places(&e.kind) { place(at, proved, out, calls); }
}
fn block(body: &[Stmt], proved: Proved, out: &mut BTreeSet<&'static str>, calls: &mut BTreeSet<u32>) {
    for stmt in body {
        match stmt {
            Stmt::Set(_, e) | Stmt::Eval(e) | Stmt::Return(e, _) => own(e, proved, out, calls),
            Stmt::StorePlace(target, _, e) | Stmt::Replace(_, target, e) => {
                // A compound assignment checks its arithmetic, and narrows into a smaller width.
                if let Stmt::StorePlace(target, Some(op), _) = stmt {
                    if matches!(op, Binary::Div | Binary::Rem) { out.insert("division by zero"); }
                    else if !(proved.arithmetic && matches!(target.ty, super::ir::Ty::Unsigned(_))) { out.insert("overflow"); }
                }
                place(target, proved, out, calls);
                own(e, proved, out, calls);
            }
            Stmt::If(c, a, b) => { own(c, proved, out, calls); block(a, proved, out, calls); block(b, proved, out, calls); }
            Stmt::While(c, body) => { own(c, proved, out, calls); block(body, proved, out, calls); }
            Stmt::Match { value, arms } | Stmt::MatchRef { value, arms, .. } => { own(value, proved, out, calls); for arm in arms { block(&arm.body, proved, out, calls); } }
            Stmt::Trap => { if !proved.unreachable { out.insert("unreachable"); } }
            Stmt::Expire(_) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

type Traps = Vec<BTreeSet<&'static str>>;
/// Per function: the traps it raises itself, what it calls, and every trap it can raise
/// through its calls too, to a fixed point (recursion included).
fn analysis(program: &Program) -> (Traps, Vec<BTreeSet<u32>>, Traps) {
    let n = program.functions.len();
    let mut direct = vec![BTreeSet::new(); n];
    let mut calls = vec![BTreeSet::new(); n];
    for (i, f) in program.functions.iter().enumerate() {
        let flag = |v: &Vec<bool>| v.get(i).copied().unwrap_or(false);
        let proved = Proved { arithmetic: flag(&program.proved_arithmetic), index: flag(&program.proved_index), slices: flag(&program.proved_slices), unreachable: flag(&program.proved_unreachable) };
        block(&f.body, proved, &mut direct[i], &mut calls[i]);
    }
    let mut all = direct.clone();
    loop {
        let mut changed = false;
        for f in 0..n {
            for g in calls[f].clone() {
                let more: Vec<&'static str> = all[g as usize].difference(&all[f]).copied().collect();
                if !more.is_empty() { all[f].extend(more); changed = true; }
            }
        }
        if !changed { break; }
    }
    (direct, calls, all)
}

/// `nopanic fn`: each function marked (`marked`, per function as emitted) must have no way to
/// trap, itself or through a call. The first that has one is named, with how.
pub(super) fn nopanic(program: &Program, marked: &[bool]) -> Result<(), (usize, String)> {
    let (direct, _, all) = analysis(program);
    for (f, traps) in all.iter().enumerate() {
        if !marked.get(f).copied().unwrap_or(false) || traps.is_empty() { continue; }
        let name = program.names.get(f).cloned().unwrap_or_default();
        let own: Vec<&str> = direct[f].iter().copied().collect();
        let through: Vec<&str> = traps.difference(&direct[f]).copied().collect();
        let mut how = Vec::new();
        if !own.is_empty() { how.push(format!("{} in its own body", own.join(", "))); }
        if !through.is_empty() { how.push(format!("{} through what it calls", through.join(", "))); }
        return Err((f, format!("`{name}` is `nopanic`, but it can trap: {} (see `wasm-traps`; a scalar contract whose proof shows its arithmetic in range removes overflow)", how.join("; "))));
    }
    Ok(())
}

pub(super) fn report(program: &Program) -> String {
    let (direct, calls, all) = analysis(program);
    // Only what `main` reaches, a task's function from where it is spawned.
    let mut reached = BTreeSet::from([program.main]);
    let mut queue = vec![program.main];
    while let Some(f) = queue.pop() { for g in &calls[f as usize] { if reached.insert(*g) { queue.push(*g); } } }
    let name = |f: u32| program.names.get(f as usize).cloned().unwrap_or_else(|| format!("#{f}"));
    let mut lines: BTreeMap<String, String> = BTreeMap::new();
    let mut never = 0;
    for f in &reached {
        let (own, through) = (&direct[*f as usize], &all[*f as usize]);
        if through.is_empty() { never += 1; continue; }
        let mut text = own.iter().copied().collect::<Vec<_>>().join(", ");
        let inherited: Vec<&str> = through.difference(own).copied().collect();
        if !inherited.is_empty() {
            // Only the calls that pass on one of these.
            let via: BTreeSet<String> = calls[*f as usize].iter().filter(|g| inherited.iter().any(|t| all[**g as usize].contains(t))).map(|g| name(*g)).collect();
            if !text.is_empty() { text.push_str("; "); }
            text.push_str(&format!("{} through {}", inherited.join(", "), via.into_iter().collect::<Vec<_>>().join(", ")));
        }
        lines.insert(name(*f), text);
    }
    let main = lines.remove("main");
    let mut out = format!("main: {}\n", main.as_deref().unwrap_or("never traps"));
    for (f, text) in &lines { out.push_str(&format!("{f}: {text}\n")); }
    out.push_str(&format!("{never} reachable functions never trap\n"));
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn each_function_says_how_it_can_trap_and_through_what() {
        let source = "fn add(a: U64, b: U64) -> U64:\n    return a + b\n\nfn half(a: U64) -> U64:\n    return a / 2\n\nfn share(a: U64, b: U64) -> U64:\n    return a / b\n\nfn total(a: U64) -> U64:\n    return add(a, half(a))\n\nfn main():\n    let x = total(3)\n    let y = share(x, 2)\n    let z = half(y)\n";
        let syntax = crate::compiler::parse(source).expect("parses");
        let logical = super::super::check::program(&syntax).expect("checks");
        let program = super::super::concrete::program(logical).expect("lowers");
        let report = super::report(&program);
        assert!(report.contains("add: overflow\n") && report.contains("share: division by zero\n"), "{report}");
        assert!(report.contains("total: overflow through add\n"), "{report}");
        assert!(!report.contains("half:") && report.contains("1 reachable functions never trap"), "{report}");
        assert!(report.starts_with("main: division by zero, overflow through share, total\n"), "{report}");
    }

    #[test]
    fn counted_reads_use_the_checked_type_of_a_local_slice() {
        let source = r#"fn fold() -> U64:
    let data = [1, 2, 4]
    let values = data.slice()
    var checksum: U64 = 0
    for i in 0..values.len():
        checksum = checksum ^ values[i]
    return checksum

fn main():
    let checksum = fold()
"#;
        let syntax = crate::compiler::parse(source).expect("parses");
        let logical = super::super::check::program(&syntax).expect("checks");
        let program = super::super::concrete::program(logical).expect("lowers");
        let report = super::report(&program);
        assert!(!report.contains("index out of range"), "{report}");
    }

    /// A function whose proof shows every `+ - *` in range is emitted without their checks, and
    /// is not listed; one whose proof misses one (here an array element's `+=` at a computed index; one at a
    /// literal index is followed) keeps them all.
    #[test]
    fn a_proof_of_range_removes_overflow() {
        let source = "fn bump(x: U64) -> U64:\n    requires x < 100\n    ensures result <= 200\n    proof:\n        auto\n    var y = x + 1\n    y += x\n    return y\n\nfn poke(x: U64) -> U64:\n    requires x < 100\n    ensures true\n    proof:\n        auto\n    var a: [U64; 2] = [0, 0]\n    var i: U64 = 0\n    a[i] += x\n    return a[i]\n\nfn main():\n    let x = bump(3)\n    let y = poke(4)\n";
        let syntax = crate::compiler::parse(source).expect("parses");
        let logical = super::super::check::program(&syntax).expect("checks");
        let proved = super::super::source_vc::prepare_with_sites(&logical).expect("goals").proved_arithmetic;
        let mut program = super::super::concrete::program(logical).expect("lowers");
        program.proved_arithmetic = program.definitions.iter().map(|d| proved.contains(d)).collect();
        let report = super::report(&program);
        assert!(!report.contains("bump:") && report.contains("poke: index out of range, overflow\n"), "{report}");
    }
    /// What `nopanic` answers for a program, as a build asks it; `proofs` says whether the goals
    /// made for it are taken as accepted (no prover runs here), and so what they cover as proved.
    fn promise(source: &str, proofs: bool) -> Result<(), String> {
        let syntax = crate::compiler::parse(source).expect("parses");
        let logical = super::super::check::program(&syntax).unwrap_or_else(|e| panic!("checks: {e:?}\n{source}"));
        let marked: BTreeSet<u32> = logical.functions.iter().filter(|f| f.nopanic.is_some()).map(|f| f.definition.0).collect();
        let proved = if proofs { super::super::source_vc::prepare_with_sites(&logical).expect("goals").proved_arithmetic } else { Default::default() };
        let mut program = super::super::concrete::program(logical).expect("lowers");
        program.proved_arithmetic = program.definitions.iter().map(|d| proved.contains(d)).collect();
        program.proved_index = program.definitions.iter().map(|d| proved.contains(&(d | super::super::scalar_vc::INDEX_PROVED))).collect();
        let marked: Vec<bool> = program.definitions.iter().map(|d| marked.contains(d)).collect();
        super::nopanic(&program, &marked).map_err(|(_, why)| why)
    }
    const TYPES: &str = "type Pick:\n    Yes(U64)\n    No\n\ntype Pair:\n    xs: [U64; 3]\n    k: U64\n\nnopanic fn id(x: U64) -> U64:\n    return x\n\nnopanic fn get(x: &U64) -> U64:\n    return *x\n\nfn make() -> [U64; 3]:\n    return [1, 2, 3]\n\n";
    fn f(parameters: &str, body: &[&str]) -> String { g(parameters, "", body) }
    fn g(parameters: &str, contract: &str, body: &[&str]) -> String {
        format!("{TYPES}nopanic fn f({parameters}) -> U64:\n{contract}    var arr: [U64; 3] = [1, 2, 3]\n    var x: U64 = 9\n{}\n\nfn main():\n    let unused = 0\n", body.iter().map(|line| format!("    {line}\n")).collect::<String>())
    }
    use std::collections::BTreeSet;
    /// Review 2026-11-21: an index past the end through a place was counted only for stores, so
    /// `return arr[n]`, `&arr[n]` and `memory.exchange(&mut arr[n], ..)` passed `nopanic` and
    /// trapped. Every form that reaches a place through a computed index is a trap.
    #[test]
    fn an_index_through_a_place_is_a_trap_in_every_position() {
        let forms: &[(&str, &[&str])] = &[
            ("read", &["return arr[n]"]),
            ("store", &["arr[n] = 4", "return 1"]),
            ("compound assignment", &["arr[n] /= 2", "return 1"]),
            ("shared reference", &["let r = &arr[n]", "return *r"]),
            ("unique reference", &["let r = &mut arr[n]", "*r = 5", "return 1"]),
            ("exchange, first operand", &["memory.exchange(&mut arr[n], &mut x)", "return x"]),
            ("exchange, second operand", &["memory.exchange(&mut x, &mut arr[n])", "return x"]),
            ("call argument", &["return id(arr[n])"]),
            ("reference argument", &["return get(&arr[n])"]),
            ("while condition", &["while arr[n] == x:", "    x = 7", "return x"]),
            ("if condition", &["if arr[n] == 2:", "    return 1", "return 0"]),
            ("index of an index", &["return arr[arr[n]]"]),
            ("operand", &["return arr[n] / 2"]),
            ("let", &["let y = arr[n]", "return y"]),
            ("field then index, store", &["var p = Pair(xs: [1, 2, 3], k: 4)", "p.xs[n] = 3", "return 1"]),
            ("field then index, read", &["let p = Pair(xs: [1, 2, 3], k: 4)", "return p.xs[n]"]),
            ("element of an element", &["let grid: [[U64; 2]; 2] = [[1, 2], [3, 4]]", "return grid[1][n]"]),
            ("outer element", &["let grid: [[U64; 2]; 2] = [[1, 2], [3, 4]]", "return grid[n][1]"]),
            ("match scrutinee", &["let picks: [Pick; 2] = [Pick.Yes(1), Pick.No]", "match picks[n]:", "    Yes(y):", "        return y", "    No:", "        return 0"]),
            ("match through a reference", &["let picks: [Pick; 2] = [Pick.Yes(1), Pick.No]", "match &picks[n]:", "    Yes(y):", "        return *y", "    No:", "        return 0"]),
            ("if let", &["let picks: [Pick; 2] = [Pick.Yes(1), Pick.No]", "if let Yes(y) = picks[n]:", "    return y", "return 0"]),
            ("inside a match arm", &["match Pick.No:", "    Yes(y):", "        return y", "    No:", "        return arr[n]"]),
            ("inside a loop body", &["while x > 100:", "    x = arr[n]", "return x"]),
            ("a value's element", &["return make()[n]"]),
            ("a number past the end", &["return arr[3]"]),
        ];
        for (name, body) in forms {
            let refusal = promise(&f("n: U64", body), false).expect_err(name);
            assert!(refusal.contains("`f` is `nopanic`") && refusal.contains("index out of range in its own body"), "{name}: {refusal}");
        }
        // Through a reference to the array, read and written.
        for body in ["return (*a)[n]", "(*a)[n] = 1\n    return 1"] {
            let source = format!("nopanic fn g(a: &mut [U64; 3], n: U64) -> U64:\n    {body}\n\nfn main():\n    let unused = 0\n");
            assert!(promise(&source, false).expect_err(body).contains("index out of range in its own body"), "{body}");
        }
        // And a caller of one inherits it.
        let source = "fn at(n: U64) -> U64:\n    let arr: [U64; 3] = [1, 2, 3]\n    return arr[n]\n\nnopanic fn g(n: U64) -> U64:\n    return at(n)\n\nfn main():\n    let unused = 0\n";
        assert!(promise(source, false).expect_err("through").contains("index out of range through what it calls"));
    }
    /// A number below the length is in range as written, in a place or a value; and a function
    /// whose proof bounds its every index (which is when its tests are not emitted) cannot trap.
    /// One index the proof does not bound keeps every test, and the promise is refused.
    #[test]
    fn an_index_in_range_as_written_or_proved_is_not_a_trap() {
        let literal = f("n: U64", &["arr[1] = n", "memory.exchange(&mut arr[2], &mut x)", "x = get(&arr[0])", "x = make()[2]", "return arr[2]"]);
        assert_eq!(promise(&literal, false), Ok(()));
        let source = |body: &[&str]| g("n: U64, m: U64", "    requires n < 3\n    ensures true\n    proof:\n        auto\n", body);
        let proved = source(&["memory.exchange(&mut arr[n], &mut x)", "let r = &arr[n]", "return *r"]);
        assert_eq!(promise(&proved, true), Ok(()));
        // Without the accepted proof the same body keeps its tests.
        assert!(promise(&proved, false).expect_err("unproved").contains("index out of range"));
        let partly = source(&["let y = arr[n]", "return arr[m]"]);
        assert!(promise(&partly, true).expect_err("one index unbounded").contains("index out of range in its own body"));
    }
}
