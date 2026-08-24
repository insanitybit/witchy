use std::time::Instant;
use std::collections::HashMap;
use std::str;

fn main() {
    let t0 = Instant::now();
    let n = 200000;
    let k = 10;
    let mut cs = vec![0u8; n];
    let mut seed = 42i64;
    let chars = b"ACGT";
    
    for i in 0..n {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        cs[i] = chars[((seed / 65536) % 4) as usize];
    }
    
    let mut counts: HashMap<&[u8], i32> = HashMap::new();
    for j in 0..=(n - k) {
        *counts.entry(&cs[j..j+k]).or_insert(0) += 1;
    }
    
    println!("{}", counts.len());
    println!("bench_ns={}", t0.elapsed().as_nanos());
}
