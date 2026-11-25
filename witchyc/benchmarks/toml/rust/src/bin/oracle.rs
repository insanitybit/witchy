//! oracle DIR COUNT: one line, `1` for each of DIR/m0 .. that the `toml` crate reads as a
//! table, `0` for each it refuses (text that is not UTF-8 among them).
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let count: u64 = args[2].parse().expect("a count");
    let mut out = String::new();
    for number in 0..count {
        let bytes = std::fs::read(format!("{}/m{number}", args[1])).expect("a mutant");
        let accepted = String::from_utf8(bytes).is_ok_and(|text| text.parse::<toml::Table>().is_ok());
        out.push(if accepted { '1' } else { '0' });
    }
    println!("{out}");
}
