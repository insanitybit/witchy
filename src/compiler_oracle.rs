//! Pure compiler-service adapter for the executable SafeWIR oracle.
//! This sits above WIR so the oracle never depends on the runtime kernel.

pub fn call(
    service: witchy_wir::canonical_interpreter::CompilerServiceKind,
    arguments: &[String],
) -> Result<String, String> {
    use witchy_runtime::value::NativeValue;
    use witchy_wir::canonical_interpreter::CompilerServiceKind;
    let arguments = arguments.iter().cloned().map(NativeValue::Str).collect::<Vec<_>>();
    let result = match service {
        CompilerServiceKind::Footprint => witchy_interp::compiler_natives::footprint(&arguments),
        CompilerServiceKind::Diff => witchy_interp::compiler_natives::diff(&arguments),
        CompilerServiceKind::Doc => witchy_interp::compiler_natives::doc(&arguments),
        CompilerServiceKind::DocResultJson => witchy_interp::compiler_natives::doc_result_json(&arguments),
    }.map_err(|error| error.message)?;
    match result {
        NativeValue::Str(result) => Ok(result),
        _ => Err("compiler service returned a non-string value".to_owned()),
    }
}
