#![deny(unsafe_code)]

fn main() {
    if let Err(error) = witchy_compiler_worker::serve_stdio() {
        eprintln!("witchy compiler worker refused request: {error}");
        std::process::exit(70);
    }
}
