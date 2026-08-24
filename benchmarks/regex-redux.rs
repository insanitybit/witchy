use regex::Regex;
use std::fmt::Write;
use std::time::Instant;

fn main() {
    let t0 = Instant::now();

    let seq = "agggtaaa cgtgggtaaa aactggtaaa agactgtaaa aggacttaaa agggcgaaa agggtcaa agggtaca agggtaac aatggtaaa agagtaaa aggataaa agggcaaa tHaNt aND caN HaD WaS aNt BY <header> |word| ";

    let mut sb = String::new();
    for i in 0..250 {
        write!(&mut sb, ">Sequence_{}\n{}\n", i, seq).unwrap();
    }
    let mut text = sb;
    let original_len = text.len();

    let clean_re = Regex::new(r"(>[^\n]+)?\n").unwrap();
    text = clean_re.replace_all(&text, "").into_owned();
    let cleaned_len = text.len();

    let variants = vec![
        "agggtaaa|tttaccct",
        "[cgt]gggtaaa|tttaccc[acg]",
        "a[act]ggtaaa|tttacc[agt]t",
        "ag[act]gtaaa|tttac[agt]ct",
        "agg[act]taaa|ttta[agt]cct",
        "aggg[acg]aaa|ttt[cgt]ccct",
        "agggt[cgt]aa|tt[acg]accct",
        "agggta[cgt]a|t[acg]taccct",
        "agggtaa[cgt]|[acg]ttaccct",
    ];

    for v in variants {
        let re = Regex::new(v).unwrap();
        let count = re.find_iter(&text).count();
        println!("{} {}", v, count);
    }

    let substitutions = vec![
        ("tHa[Nt]", "<4>"),
        ("aND|caN|Ha[DS]|WaS", "<3>"),
        ("a[NSt]|BY", "<2>"),
        ("<[^>]*>", "|"),
        (r"\|[^|][^|]*\|", "-"),
    ];

    for (re_str, sub) in substitutions {
        let re = Regex::new(re_str).unwrap();
        text = re.replace_all(&text, sub).into_owned();
    }

    println!("\n{}\n{}\n{}", original_len, cleaned_len, text.len());
    let duration = t0.elapsed();
    println!("bench_ns={}", duration.as_nanos());
}
