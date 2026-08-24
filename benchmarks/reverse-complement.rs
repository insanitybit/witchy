use std::time::Instant;

fn main() {
    let alu = b"GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCAGCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA";
    let total = 3_000_000;
    
    let mut seq = vec![0u8; total];
    let alu_len = alu.len();
    for i in 0..total {
        seq[i] = alu[i % alu_len];
    }
    
    let mut comp = [0u8; 256];
    for i in 0..256 {
        comp[i] = i as u8;
    }
    comp[b'A' as usize] = b'T';
    comp[b'a' as usize] = b'T';
    comp[b'C' as usize] = b'G';
    comp[b'c' as usize] = b'G';
    comp[b'G' as usize] = b'C';
    comp[b'g' as usize] = b'C';
    comp[b'T' as usize] = b'A';
    comp[b't' as usize] = b'A';
    
    let t0 = Instant::now();
    
    for _ in 0..15 {
        let mut i = 0;
        let mut j = total - 1;
        while i <= j {
            let ci = seq[i];
            let cj = seq[j];
            seq[i] = comp[cj as usize];
            seq[j] = comp[ci as usize];
            if i == j {
                break;
            }
            i += 1;
            j -= 1;
        }
    }
    
    let t1 = Instant::now();
    
    println!(">ONE Homo sapiens alu");
    let mut k = 0;
    while k < total {
        let mut end = k + 60;
        if end > total {
            end = total;
        }
        let chunk = &seq[k..end];
        let s = unsafe { std::str::from_utf8_unchecked(chunk) };
        println!("{}", s);
        k += 60;
    }
    
    println!("bench_ns={}", t1.duration_since(t0).as_nanos());
}
