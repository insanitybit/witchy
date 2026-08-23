struct Tree {
    l: Option<Box<Tree>>,
    r: Option<Box<Tree>>,
}

fn build(depth: i32) -> Option<Box<Tree>> {
    if depth == 0 {
        return Some(Box::new(Tree { l: None, r: None }));
    }
    Some(Box::new(Tree {
        l: build(depth - 1),
        r: build(depth - 1),
    }))
}

fn check(t: &Option<Box<Tree>>) -> i32 {
    match t {
        None => 0,
        Some(node) => 1 + check(&node.l) + check(&node.r),
    }
}

fn main() {
    let t0 = std::time::Instant::now();
    let mut total = 0;
    for _ in 0..50 {
        let t = build(16);
        total += check(&t);
    }
    let duration = t0.elapsed();
    println!("{}", total);
    println!("bench_ns={}", duration.as_nanos());
}
