use std::time::Instant;

struct Stack {
    items: Vec<i64>,
    total: i64,
}

fn build(n: i64) -> Stack {
    let mut s = Stack { items: Vec::new(), total: 0 };
    let mut i = 0;
    while i < n {
        s.items.push(i);
        s.total += i;
        i += 1;
    }
    s
}

fn main() {
    let t0 = Instant::now();
    let s = build(500000);
    println!("{}", s.total + s.items.len() as i64);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}
