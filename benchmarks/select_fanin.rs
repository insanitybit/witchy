use std::sync::mpsc;
use std::thread;
use std::time::Instant;

const PER_SOURCE: i32 = 2048;

fn expected_checksum() -> i32 {
    let mut checksum = 0i64;
    for i in 0..PER_SOURCE {
        checksum = (checksum * 257 + (i * 2 + 1) as i64) % 1000000007;
    }
    for i in 0..PER_SOURCE {
        checksum = (checksum * 257 + (i * 2 + 3) as i64) % 1000000007;
    }
    checksum as i32
}

fn main() {
    let (tx_a, rx_a) = mpsc::sync_channel(PER_SOURCE as usize);
    let (tx_b, rx_b) = mpsc::sync_channel(PER_SOURCE as usize);
    
    let handle_a = thread::spawn(move || {
        for i in 0..PER_SOURCE {
            tx_a.send(i * 2).unwrap();
        }
    });
    
    let handle_b = thread::spawn(move || {
        for i in 0..PER_SOURCE {
            tx_b.send(1 + i * 2).unwrap();
        }
    });
    
    handle_a.join().unwrap();
    handle_b.join().unwrap();

    let mut seen_a = 0;
    let mut seen_b = 0;
    let mut checksum = 0i64;
    let mut order_ok = true;
    
    let t0 = Instant::now();
    
    // In Rust we can just try to receive from a, then if empty try b, etc.
    // However mpsc::select! is deprecated/removed in std, so we use try_recv
    while seen_a + seen_b < PER_SOURCE * 2 {
        if let Ok(value) = rx_a.try_recv() {
            if seen_b != 0 || value != seen_a * 2 {
                order_ok = false;
            }
            checksum = (checksum * 257 + value as i64 + 1) % 1000000007;
            seen_a += 1;
        } else if let Ok(value) = rx_b.try_recv() {
            if seen_a != PER_SOURCE || value != seen_b * 2 + 1 {
                order_ok = false;
            }
            checksum = (checksum * 257 + value as i64 + 2) % 1000000007;
            seen_b += 1;
        }
    }
    
    let bench_ns = t0.elapsed().as_nanos();
    let expected = expected_checksum();
    
    if order_ok && seen_a == PER_SOURCE && seen_b == PER_SOURCE && checksum == expected as i64 {
        println!("count={} checksum={} order=ok", seen_a + seen_b, checksum);
    } else {
        println!("count={} checksum={} expected={} order=bad", seen_a + seen_b, checksum, expected);
    }
    println!("bench_ns={}", bench_ns);
}
