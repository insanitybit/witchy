use std::hint::black_box;
use std::time::Instant;

enum Chain {
    End,
    Node(i64, Box<Chain>),
}

#[inline(never)]
fn build(length: i64) -> Chain {
    let mut chain = Chain::End;
    for value in 0..length {
        chain = Chain::Node(value % 101, Box::new(chain));
    }
    chain
}

#[inline(never)]
fn sum(chain: Chain) -> i64 {
    let mut total = 0_i64;
    let mut cursor = chain;
    loop {
        match cursor {
            Chain::End => return total,
            Chain::Node(value, next) => {
                total += value;
                cursor = *next;
            }
        }
    }
}

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn witchy_rust_class_kernel() -> i64 {
    let length = black_box(200_000_i64);
    black_box(sum(build(length)))
}

fn main() {
    let start = Instant::now();
    let result = witchy_rust_class_kernel();
    let elapsed = start.elapsed().as_nanos();
    println!("result={result}");
    println!("bench_ns={elapsed}");
}
