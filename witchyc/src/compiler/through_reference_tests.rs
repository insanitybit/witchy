//! A field or element reached through a reference by the reference's name alone: `r.field`,
//! `r[i]`. What must hold: it is the program written with `(*r)`, nothing more. Each case is
//! compiled both ways; accepted, the two modules are the same bytes; refused, the same refusal.
const TYPES: &str = "type Pair:\n    left: U64\n    right: U64\n\ntype Wide:\n    pair: Pair\n    cells: [U64; 3]\n\nfn sum(pair: &Pair) -> U64:\n    return (*pair).left + (*pair).right\n\nfn bump(value: &mut U64):\n    *value += 1\n\n";
const MAIN: &str = "\nfn main(console: Console<Write>):\n    var wide = Wide(Pair(1, 2), [3, 4, 5])\n    console.print_int(run(&mut wide))\n";
/// The short form of a case, and the same text with every `@` name dereferenced.
fn both(case: &str) -> (Result<Vec<u8>, String>, Result<Vec<u8>, String>) {
    let build = |text: String| super::compile(&format!("{TYPES}{text}{MAIN}")).map_err(|refusal| refusal.message.to_string());
    let mut short = String::new();
    let mut long = String::new();
    let mut rest = case;
    while let Some(at) = rest.find('@') {
        let name: String = rest[at + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        short.push_str(&rest[..at]); short.push_str(&name);
        long.push_str(&rest[..at]); long.push_str(&format!("(*{name})"));
        rest = &rest[at + 1 + name.len()..];
    }
    short.push_str(rest); long.push_str(rest);
    assert_ne!(short, long, "{case}");
    (build(short), build(long))
}
fn same(case: &str) { let (short, long) = both(case); assert!(short.is_ok(), "{case}: {short:?}"); assert_eq!(short, long, "{case}"); }
fn refused(case: &str, why: &str) {
    let (short, long) = both(case);
    let message = short.clone().expect_err(case);
    assert!(message.contains(why), "{case}: {message}");
    assert_eq!(short, long, "{case}");
}
#[test]
fn reading_and_writing_through_a_name_is_the_dereference_written_out() {
    same("fn run(wide: &mut Wide) -> U64:\n    return @wide.pair.left + @wide.cells[1]\n");
    same("fn run(wide: &mut Wide) -> U64:\n    @wide.pair.left += 4\n    @wide.cells[2] = @wide.pair.left * 2\n    return @wide.cells[2] + sum(&@wide.pair)\n");
    same("fn run(wide: &mut Wide) -> U64:\n    bump(&mut @wide.pair.right)\n    bump(&mut @wide.cells[0])\n    return @wide.pair.right + @wide.cells[0]\n");
    same("fn run(wide: &mut Wide) -> U64:\n    let index: U64 = 2\n    let pair = &@wide.pair\n    return @pair.left + @wide.cells[index]\n");
    same("fn run(wide: &mut Wide) -> U64:\n    let pair = &mut @wide.pair\n    @pair.left = 9\n    @pair.right += @pair.left\n    return @wide.pair.right\n");
    same("fn inner(pair: &Pair) -> U64:\n    return @pair.left * 10 + @pair.right\n\nfn run(wide: &mut Wide) -> U64:\n    return inner(&@wide.pair)\n");
    same("fn total(cells: &[U64; 3]) -> U64:\n    var all: U64 = 0\n    for index in 0..3:\n        all += @cells[index]\n    return all\n\nfn run(wide: &mut Wide) -> U64:\n    return total(&@wide.cells)\n");
    same("impl Pair:\n    fn larger(self: &Pair) -> U64:\n        if @self.left > @self.right:\n            return @self.left\n        return @self.right\n\nfn run(wide: &mut Wide) -> U64:\n    return @wide.pair.larger()\n");
}
#[test]
fn what_the_dereference_refuses_the_name_alone_refuses() {
    refused("fn poke(pair: &Pair):\n    @pair.left = 1\n\nfn run(wide: &mut Wide) -> U64:\n    poke(&@wide.pair)\n    return 0\n", "");
    refused("fn run(wide: &mut Wide) -> U64:\n    let pair = &mut @wide.pair\n    @wide.pair.left = 3\n    @pair.right = 1\n    return 0\n", "this value is lent");
    refused("fn run(wide: &mut Wide) -> U64:\n    let pair = &@wide.pair\n    bump(&mut @wide.pair.left)\n    return @pair.left\n", "");
    refused("fn run(wide: &mut Wide) -> U64:\n    return @wide.pair.missing\n", "unknown record field");
    refused("fn run(wide: &mut Wide) -> U64:\n    return @wide.cells.left\n", "");
    // Moving out of what a reference borrows is refused either way, in different words.
    let (short, long) = both("fn run(wide: &mut Wide) -> U64:\n    let pair: Pair = move @wide.pair\n    return 0\n");
    assert!(short.is_err() && long.is_err(), "{short:?} {long:?}");
}
