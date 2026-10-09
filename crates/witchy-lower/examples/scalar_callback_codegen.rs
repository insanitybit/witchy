use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 2 {
        return Err("usage: scalar_callback_codegen INPUT.wasm OUTPUT_PREFIX".into());
    }
    let wasm = fs::read(&arguments[0])?;
    let prefix = PathBuf::from(&arguments[1]);
    let mut config = wasmtime::Config::new();
    config.cranelift_opt_level(wasmtime::OptLevel::Speed);
    config.compiler_inlining(wasmtime::Inlining::Yes);
    config.wasm_reference_types(true);
    config.wasm_function_references(true);
    config.wasm_gc(true);
    config.wasm_simd(true);
    config.wasm_relaxed_simd(true);
    config.wasm_memory64(true);
    config.memory_may_move(false);
    let engine = wasmtime::Engine::new(&config)?;
    let module = wasmtime::Module::new(&engine, &wasm)?;
    fs::write(prefix.with_extension("text"), module.text())?;
    fs::write(prefix.with_extension("cwasm"), module.serialize()?)?;
    println!("wasm_bytes={} native_text_bytes={}", wasm.len(), module.text().len());
    Ok(())
}
