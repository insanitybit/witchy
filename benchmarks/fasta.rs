use std::io::{self, Write, BufWriter};
use std::time::Instant;

static mut LAST: u32 = 42;
const A: u32 = 3877;
const C: u32 = 29573;
const M: u32 = 139968;

fn randf(max: f64) -> f64 {
    unsafe {
        LAST = (LAST * A + C) % M;
        max * (LAST as f64) / (M as f64)
    }
}

const ALU: &str = "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGG\
GAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGA\
CCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAAT\
ACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCA\
GCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGG\
AGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCC\
AGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA";

struct AA {
    p: f64,
    c: u8,
}

fn make_cumulative(table: &mut [AA]) {
    let mut cp = 0.0;
    for t in table.iter_mut() {
        cp += t.p;
        t.p = cp;
    }
}

fn fasta_repeat(out: &mut BufWriter<io::Stdout>, mut n: usize, seq: &[u8]) {
    let mut seqi = 0;
    while n > 0 {
        let len_out = if n < 60 { n } else { 60 };
        if seqi + len_out < seq.len() {
            out.write_all(&seq[seqi..seqi + len_out]).unwrap();
            seqi += len_out;
        } else {
            let s = &seq[seqi..];
            seqi = len_out - s.len();
            out.write_all(s).unwrap();
            out.write_all(&seq[0..seqi]).unwrap();
        }
        out.write_all(b"\n").unwrap();
        n -= len_out;
    }
}

fn fasta_random(out: &mut BufWriter<io::Stdout>, mut n: usize, table: &[AA]) {
    let mut line = vec![0u8; 60];
    while n > 0 {
        let len_out = if n < 60 { n } else { 60 };
        for i in 0..len_out {
            let r = randf(1.0);
            for t in table.iter() {
                if r < t.p {
                    line[i] = t.c;
                    break;
                }
            }
        }
        out.write_all(&line[0..len_out]).unwrap();
        out.write_all(b"\n").unwrap();
        n -= len_out;
    }
}

fn main() {
    let t0 = Instant::now();

    let mut iub = vec![
        AA { p: 0.27, c: b'a' }, AA { p: 0.12, c: b'c' }, AA { p: 0.12, c: b'g' }, AA { p: 0.27, c: b't' },
        AA { p: 0.02, c: b'B' }, AA { p: 0.02, c: b'D' }, AA { p: 0.02, c: b'H' }, AA { p: 0.02, c: b'K' },
        AA { p: 0.02, c: b'M' }, AA { p: 0.02, c: b'N' }, AA { p: 0.02, c: b'R' }, AA { p: 0.02, c: b'S' },
        AA { p: 0.02, c: b'V' }, AA { p: 0.02, c: b'W' }, AA { p: 0.02, c: b'Y' },
    ];

    let mut homo_sap = vec![
        AA { p: 0.3029549426680, c: b'a' },
        AA { p: 0.1979883004921, c: b'c' },
        AA { p: 0.1975473066391, c: b'g' },
        AA { p: 0.3015094502008, c: b't' },
    ];

    make_cumulative(&mut iub);
    make_cumulative(&mut homo_sap);

    let n = 2500000;
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout);

    out.write_all(b">ONE Homo sapiens alu\n").unwrap();
    fasta_repeat(&mut out, 2 * n, ALU.as_bytes());

    out.write_all(b">TWO IUB ambiguity codes\n").unwrap();
    fasta_random(&mut out, 3 * n, &iub);

    out.write_all(b">THREE Homo sapiens frequency\n").unwrap();
    fasta_random(&mut out, 5 * n, &homo_sap);

    out.flush().unwrap();
    let t1 = t0.elapsed();
    println!("bench_ns={}", t1.as_nanos());
}
