use std::{env, fs, process::ExitCode};

const USAGE: &str = "usage: witchyc build-object FILE -o OBJECT\n       witchyc <build|wasm|wasm-checked|wasm-measure> FILE -o MODULE.wasm [--heap-pages=N] [--slots=N] [--stack-bytes=N] [--profile=debug|release]\n       witchyc build FILE -o EXECUTABLE --target=<macos-arm64|linux-arm64> [--manifest=GRANTS.toml [--manifest-digest=SHA256]] [--measure] [--profile=debug|release]\n       witchyc build FILE -o MODULE.wasm --watch\n       witchyc build FILE -o MODULE.wasm --reviewed-input-key=KEY [--heap-pages=N] [--slots=N] [--stack-bytes=N]\n       witchyc build-session FILE -o OUTPUT [--target=<macos-arm64|linux-arm64> [--manifest=GRANTS.toml]]\n       witchyc compiler-session PRIVATE_SOCKET\n       witchyc wasm-session-diagnostic FILE\n       witchyc wasm-session-trusted-generation-diagnostic FILE SDK_SHA256\n       witchyc wasm-session-body-interface-diagnostic FILE SDK_SHA256\n       witchyc wasm-toolchain-id ABSOLUTE_SDK_ROOT\n       witchyc wasm-ir FILE\n       witchyc wasm-check-package FILE\n       witchyc wasm-grants FILE DOCUMENT [--document-pin=sha384:HEX] [--document-base=DIR] [--print-input-key]\n       witchyc native-plan FILE MANIFEST [--manifest-digest=SHA256]\n       witchyc wasm-caps [--by-file|--json] FILE\n       witchyc wasm-caps <--policy|--repin> POLICY FILE\n       witchyc <lex|syntax> FILE\nBuild emits direct core Wasm. The supported profile and host grants are documented in docs/wasm-cutover.md.";

fn main() -> ExitCode {
    // The front end recurses over the program's expressions and blocks. Its syntax limits bound
    // that, and this thread's stack is sized for them in any build: the main thread's 8 MiB was
    // not (a development build overflowed it on 60 nested calls).
    match std::thread::Builder::new().name("witchyc".into()).stack_size(witchy_two::compiler::COMPILER_STACK).spawn(run) {
        Ok(compiler) => compiler.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        Err(error) => { eprintln!("cannot start compiler worker: {error}"); ExitCode::FAILURE }
    }
}
fn run() -> ExitCode {
    let mut args: Vec<_> = env::args().skip(1).collect();
    if matches!(args.as_slice(), [flag] if flag == "--help" || flag == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if let [command, root] = args.as_slice() && command == "qualification-worker" {
        return match witchy_two::compiler::qualification_worker(root.as_ref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, socket] = args.as_slice() && command == "compiler-session" {
        return match witchy_two::compiler::serve(socket.as_ref()) {
            Ok(()) => ExitCode::SUCCESS, Err(e) => { eprintln!("{e}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, flag, output] = args.as_slice() && command == "build-object" && flag == "-o" {
        return match witchy_two::compiler::build_object(input.as_ref(), output.as_ref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => { eprintln!("{e}"); ExitCode::FAILURE }
        };
    }
    let profiles: Vec<_> = args.iter().filter_map(|arg| arg.strip_prefix("--profile=")).collect();
    if !profiles.is_empty() {
        if profiles.len() != 1 || !matches!(profiles[0], "debug" | "release")
            || !matches!(args.first().map(String::as_str), Some("build" | "wasm" | "wasm-checked" | "wasm-measure"))
            || args.iter().any(|arg| arg == "--watch")
        {
            eprintln!("--profile=debug or --profile=release is supported on direct builds");
            return ExitCode::FAILURE;
        }
        witchy_two::parser::configure_debug_build(profiles[0] == "debug");
        args.retain(|arg| !arg.starts_with("--profile="));
    }
    // Sizes of the emitted module's memory: `--heap-pages=N --slots=N --stack-bytes=N`, anywhere.
    let mut heap = witchy_two::compiler::Heap::DEFAULT;
    let mut sized = false;
    let mut invalid = None;
    args.retain(|arg| {
        let (target, text): (u8, &str) = if let Some(text) = arg.strip_prefix("--heap-pages=") { (0, text) }
            else if let Some(text) = arg.strip_prefix("--slots=") { (1, text) }
            else if let Some(text) = arg.strip_prefix("--stack-bytes=") { (2, text) }
            else { return true };
        match text.parse::<u64>() {
            Ok(value) if value <= u32::MAX as u64 => {
                match target { 0 => heap.pages = value as u32, 1 => heap.slots = value, _ => heap.stack = value as u32 }
                sized = true;
            }
            _ => invalid = Some(arg.clone()),
        }
        false
    });
    if let Some(arg) = invalid { eprintln!("invalid size: {arg}"); return ExitCode::FAILURE; }
    if sized && let Err(error) = witchy_two::compiler::configure_heap(heap) { eprintln!("{error}"); return ExitCode::FAILURE; }
    // `--manifest=PATH` binds a checked launch manifest into a native executable
    // (docs/native-trusted-executables.md); once, and only on native builds.
    let manifests: Vec<String> = args.iter().filter_map(|arg| arg.strip_prefix("--manifest=")).map(str::to_owned).collect();
    if !manifests.is_empty() {
        let native = args.iter().any(|arg| matches!(arg.as_str(), "--target=macos-arm64" | "--target=linux-arm64"));
        if manifests.len() != 1 || manifests[0].is_empty() || !native || !matches!(args.first().map(String::as_str), Some("build" | "build-session")) {
            eprintln!("--manifest=PATH is given once, on a native build or build-session (--target=macos-arm64)");
            return ExitCode::FAILURE;
        }
        args.retain(|arg| !arg.starts_with("--manifest="));
    }
    let manifest = manifests.first().map(std::path::Path::new);
    if let [command, input, flag, output, target, rest @ ..] = args.as_slice()
        && command == "build" && flag == "-o" && matches!(target.as_str(), "--target=macos-arm64" | "--target=linux-arm64") && !input.starts_with('-')
    {
        let mut measure = false;
        let mut digest = None;
        for option in rest {
            match option.as_str() {
                "--measure" if !measure => measure = true,
                flag if manifest.is_some() && digest.is_none()
                    && let Some(expected) = flag.strip_prefix("--manifest-digest=") => digest = Some(expected),
                _ => { eprintln!("{USAGE}"); return ExitCode::FAILURE; }
            }
        }
        if sized { eprintln!("{USAGE}"); return ExitCode::FAILURE; }
        return match witchy_two::compiler::build_native_with_manifest_digest(input.as_ref(), output.as_ref(),
            &target["--target=".len()..], measure, manifest, digest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, flag, output, watch] = args.as_slice()
        && command == "build" && flag == "-o" && watch == "--watch" && !input.starts_with('-') {
        return match witchy_two::compiler::watch_build(input.as_ref(), output.as_ref()) {
            Ok(()) => ExitCode::SUCCESS, Err(e) => { eprintln!("{e}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, flag, output, target] = args.as_slice()
        && command == "build-session" && flag == "-o" && !input.starts_with('-')
        && let Some(target) = target.strip_prefix("--target=") {
        return match witchy_two::compiler::build_target_session(input.as_ref(), output.as_ref(), Some(target), manifest) {
            Ok(()) => ExitCode::SUCCESS, Err(e) => { eprintln!("{e}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, flag, output] = args.as_slice()
        && command == "build-session" && flag == "-o" && !input.starts_with('-') {
        return match witchy_two::compiler::build_session(input.as_ref(), output.as_ref()) {
            Ok(()) => ExitCode::SUCCESS, Err(e) => { eprintln!("{e}"); ExitCode::FAILURE }
        };
    }
    if let [command, entry, root, out] = args.as_slice() && command == "package-test-consumer" {
        // The one command whose diagnostics a tool matches by their byte range.
        witchy_two::diagnostic::show_offsets(true);
        return match witchy_two::compiler::check_test_consumer(entry.as_ref(),root.as_ref(),out.as_ref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => { eprintln!("{e}"); ExitCode::FAILURE }
        };
    }
    if let [command, entry, out, rest @ ..] = args.as_slice() && matches!(command.as_str(), "package-tests" | "package-tests-coverage") {
        let production = match rest {
            [] => None, [flag,path] if flag=="--production" => Some(std::path::Path::new(path)),
            _ => { eprintln!("expected --production ENTRY"); return ExitCode::FAILURE }
        };
        return match witchy_two::compiler::build_package_tests(entry.as_ref(), out.as_ref(), command.ends_with("-coverage"), production) {
            Ok(report) => { println!("{report}"); ExitCode::SUCCESS }
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, root] = args.as_slice() && command == "wasm-toolchain-id" {
        return match witchy_two::compiler::toolchain_identity(root.as_ref()) {
            Ok(identity) => { println!("{identity}"); ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, expected] = args.as_slice() && command == "wasm-session-body-interface-diagnostic" {
        return match witchy_two::compiler::diagnostic_body_interface_loop(input.as_ref(), expected) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, expected] = args.as_slice() && command == "wasm-session-trusted-generation-diagnostic" {
        return match witchy_two::compiler::diagnostic_trusted_generation_loop(input.as_ref(), expected) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, input] = args.as_slice() && command == "wasm-session-diagnostic" {
        return match witchy_two::compiler::diagnostic_loop(input.as_ref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, flag, output, checked] = args.as_slice()
        && command == "build" && flag == "-o" && !input.starts_with('-')
        && let Some(key) = checked.strip_prefix("--reviewed-input-key=")
    {
        return match witchy_two::compiler::build_with_reviewed_input_key(input.as_ref(), output.as_ref(), key) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, flag, output] = args.as_slice()
        && (command == "build" || command == "wasm" || command == "wasm-checked") && flag == "-o"
        && (command != "build" || !input.starts_with('-'))
    {
        let result = witchy_two::compiler::build_report(input.as_ref(), output.as_ref());
        return match result {
            Ok(report) => { if command == "wasm-checked" { println!("{report}"); } ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, input, flag, output] = args.as_slice()
        && command == "wasm-measure" && flag == "-o"
    {
        return match witchy_two::compiler::build_measured(input.as_ref(), output.as_ref()) {
            Ok(measurements) => { println!("{}", measurements.json()); ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, input] = args.as_slice() && command == "wasm-check-package" {
        return match witchy_two::compiler::check_package(input.as_ref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    // The program's authority: what `main` is granted and which functions can use what.
    if let [command, input] = args.as_slice() && command == "wasm-caps" {
        return match witchy_two::compiler::caps_file(input.as_ref()) {
            Ok(report) => { print!("{report}"); ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    // How each reachable function can stop the run with a trap.
    if let [command, input] = args.as_slice() && command == "wasm-traps" {
        return match witchy_two::compiler::traps_file(input.as_ref()) {
            Ok(report) => { print!("{report}"); ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    // A launch manifest checked against `main`: the plan a manifest-bound native executable embeds.
    if let [command, input, document, options @ ..] = args.as_slice() && command == "native-plan" {
        let digest = match options {
            [] => None,
            [option] if let Some(digest) = option.strip_prefix("--manifest-digest=") => Some(digest),
            _ => { eprintln!("{USAGE}"); return ExitCode::FAILURE; }
        };
        return match witchy_two::compiler::native_plan_file_with_digest(input.as_ref(), document.as_ref(), digest) {
            Ok(review) => { for line in review { println!("{line}"); } ExitCode::SUCCESS }
            Err(errors) => { for e in errors { eprintln!("error: {e}"); } ExitCode::FAILURE }
        };
    }
    // A grant document checked against `main`: the engine's flags on stdout, the review on stderr.
    if let [command, input, document, options @ ..] = args.as_slice() && command == "wasm-grants" {
        let mut print_key = false;
        let mut pin = None;
        let mut base = None;
        for option in options {
            if pin.is_none() && let Some(value) = option.strip_prefix("--document-pin=") {
                pin = Some(value);
            } else if base.is_none() && let Some(value) = option.strip_prefix("--document-base=") {
                base = Some(std::path::Path::new(value));
            } else if !print_key && option == "--print-input-key" {
                print_key = true;
            } else {
                eprintln!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
        return match witchy_two::compiler::grants_file_review(input.as_ref(), document.as_ref(), pin, base, print_key) {
            Ok((flags, review, warnings, key)) => {
                if let Some(key) = key { println!("input-key: {key}"); }
                for line in review { eprintln!("grant: {line}"); }
                for line in warnings { eprintln!("warning: {line}"); }
                for flag in flags { println!("{flag}"); }
                ExitCode::SUCCESS
            }
            Err(errors) => { for e in errors { eprintln!("error: {e}"); } ExitCode::FAILURE }
        };
    }
    // A program held to a per-file authority policy: exit 1 and the violations if it does not fit.
    if let [command, flag, policy, input] = args.as_slice() && command == "wasm-caps" && flag == "--policy" {
        return match witchy_two::compiler::caps_policy(policy.as_ref(), input.as_ref()) {
            Ok(violations) if violations.is_empty() => { println!("fits the policy"); ExitCode::SUCCESS },
            Ok(violations) => { for v in violations { println!("{v}"); } ExitCode::FAILURE },
            Err(error) => { eprintln!("{error}"); ExitCode::from(2) }
        };
    }
    // After reviewing a package's update: its `; digest` lines set to the code they cover now.
    if let [command, flag, policy, input] = args.as_slice() && command == "wasm-caps" && flag == "--repin" {
        return match witchy_two::compiler::caps_repin(policy.as_ref(), input.as_ref()) {
            Ok(changed) if changed.is_empty() => { println!("every digest is current"); ExitCode::SUCCESS },
            Ok(changed) => { for line in changed { println!("repinned {line}"); } ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, flag, input] = args.as_slice() && command == "wasm-caps" && flag == "--draft-policy" {
        return match witchy_two::compiler::caps_draft_policy(input.as_ref()) {
            Ok(policy) => { print!("{policy}"); ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, flag, input] = args.as_slice() && command == "wasm-caps" && flag == "--by-file" {
        return match witchy_two::compiler::caps_by_file(input.as_ref()) {
            Ok(report) => { print!("{report}"); ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, flag, input] = args.as_slice() && command == "wasm-caps" && flag == "--json" {
        return match witchy_two::compiler::caps_json_file(input.as_ref()) {
            Ok(report) => { print!("{report}"); ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    // Whether a new version of a program can do more than the old one: exit 1 if it can.
    if let [command, old, new] = args.as_slice() && command == "wasm-caps-diff" {
        return match witchy_two::compiler::caps_diff_files(old.as_ref(), new.as_ref()) {
            Ok((report, widened)) => { print!("{report}"); if widened { ExitCode::FAILURE } else { ExitCode::SUCCESS } },
            Err(error) => { eprintln!("{error}"); ExitCode::from(2) }
        };
    }
    if let [command, input] = args.as_slice() && command == "wasm-ir" {
        return match witchy_two::compiler::inspect_file(input.as_ref()) {
            Ok(ir) => { println!("{ir}"); ExitCode::SUCCESS },
            Err(error) => { eprintln!("{error}"); ExitCode::FAILURE }
        };
    }
    if let [command, path] = args.as_slice() {
        if command == "lex" || command == "syntax" {
            let source = match fs::read_to_string(path) {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("{path}: {error}");
                    return ExitCode::FAILURE;
                }
            };
            if command == "syntax" {
                return match witchy_two::parser::parse(&source) {
                    Ok(program) => {
                        println!(
                            "{} declarations parsed; not type-checked",
                            program.declarations.len()
                        );
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!(
                            "{path}:{}..{} {}: {}",
                            error.span.start, error.span.end, error.code, error.message
                        );
                        ExitCode::FAILURE
                    }
                };
            }
            return match witchy_two::lexer::lex(&source) {
                Ok(tokens) => {
                    for token in tokens {
                        println!("{}..{} {:?}", token.span.start, token.span.end, token.kind);
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!(
                        "{path}:{}..{} {}: {}",
                        error.span.start, error.span.end, error.code, error.message
                    );
                    ExitCode::FAILURE
                }
            };
        }
    }
    eprintln!("{USAGE}");
    ExitCode::FAILURE
}
