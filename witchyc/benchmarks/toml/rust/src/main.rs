//! toml-compare PARSER FILE COUNT: parse FILE COUNT times with one parser and print a sum.
//! PARSER is spanner, toml, toml_edit, or none (the file is read and nothing parsed).
use std::hint::black_box;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let source = std::fs::read_to_string(&args[2]).expect("readable input");
    let count: u64 = args[3].parse().expect("a count");
    let mut total = 0u64;
    for _ in 0..count {
        let source = black_box(source.as_str());
        match args[1].as_str() {
            "spanner" => {
                let arena = toml_spanner::Arena::new();
                let mut result = toml_spanner::parse(source, &arena);
                black_box(&mut result);
                total += result.is_ok() as u64;
            }
            "toml" => {
                let mut result = source.parse::<toml::Table>();
                black_box(&mut result);
                total += result.is_ok() as u64;
            }
            "toml_edit" => {
                let mut result = source.parse::<toml_edit::DocumentMut>();
                black_box(&mut result);
                total += result.is_ok() as u64;
            }
            _ => total += source.len() as u64,
        }
    }
    println!("{total}");
}
