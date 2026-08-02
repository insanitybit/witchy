use std::hint::black_box;
use std::time::Instant;

enum Token {
    Skip,
    Value(i64),
}

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn witchy_rust_class_kernel() -> i64 {
    let count = black_box(500_000_i64);
    let mut total = 0_i64;
    for i in 0..count {
        let token = if i % 3 == 0 {
            Token::Skip
        } else {
            Token::Value((i * 7 + 3) % 101)
        };
        total += match token {
            Token::Skip => 1,
            Token::Value(value) => value,
        };
    }
    black_box(total)
}

fn main() {
    let start = Instant::now();
    let result = witchy_rust_class_kernel();
    let elapsed = start.elapsed().as_nanos();
    println!("result={result}");
    println!("bench_ns={elapsed}");
}
