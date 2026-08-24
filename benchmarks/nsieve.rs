use std::time::Instant;

fn nsieve(n: usize) -> usize {
    let mut flags = vec![true; n];
    let mut count = 0;
    for i in 2..n {
        if flags[i] {
            count += 1;
            let mut j = i + i;
            while j < n {
                flags[j] = false;
                j += i;
            }
        }
    }
    count
}

fn main() {
    let t0 = Instant::now();
    let mut total = 0;
    total += nsieve(800000);
    total += nsieve(400000);
    total += nsieve(200000);
    println!("{}", total);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}
