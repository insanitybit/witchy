    use super::*;

    #[test]
    fn every_cataloged_string_operation_has_runtime_dispatch() {
        let module = witchy_syntax::parser::parse_module("fn main() -> Int:\n    0\n")
            .expect("parse minimal module");
        let mut interpreter = Interpreter::new(module);

        for name in intrinsics::STRING_OPERATIONS {
            let args = match *name {
                intrinsics::STRING_FROM_CODE => vec![Value::Int(65)],
                intrinsics::STRING_REPLACE => vec![
                    Value::str("12"),
                    Value::str("1"),
                    Value::str("x"),
                ],
                intrinsics::STRING_SUBSTRING => {
                    vec![Value::str("12"), Value::Int(0), Value::Int(1)]
                }
                intrinsics::STRING_SPLIT
                | intrinsics::STRING_CONTAINS
                | intrinsics::STRING_STARTS_WITH
                | intrinsics::STRING_ENDS_WITH
                | intrinsics::STRING_FIND => {
                    vec![Value::str("12"), Value::str("1")]
                }
                _ => vec![Value::str("12")],
            };
            let result = interpreter
                .call_builtin(name, &args)
                .unwrap_or_else(|error| panic!("{} failed: {}", name, error.message));
            assert!(result.is_some(), "{} fell through runtime dispatch", name);
        }
    }

    #[test]
    fn every_cataloged_math_operation_has_runtime_dispatch() {
        let module = witchy_syntax::parser::parse_module("fn main() -> Int:\n    0\n")
            .expect("parse minimal module");
        let mut interpreter = Interpreter::new(module);

        for name in intrinsics::MATH_OPERATIONS {
            let args = match *name {
                intrinsics::MATH_TO_FLOAT => vec![Value::Int(4)],
                intrinsics::MATH_TO_INT | intrinsics::MATH_SQRT => vec![Value::Float(4.0)],
                _ => unreachable!("uncataloged math operation"),
            };
            let result = interpreter
                .call_builtin(name, &args)
                .unwrap_or_else(|error| panic!("{} failed: {}", name, error.message));
            assert!(result.is_some(), "{} fell through runtime dispatch", name);
        }
    }

    #[test]
    fn cataloged_regex_operation_has_interpreter_native_dispatch() {
        let module = witchy_syntax::parser::parse_module("fn main() -> Int:\n    0\n")
            .expect("parse minimal module");
        let mut interpreter = Interpreter::new(module);
        let result = interpreter
            .call_builtin(
                intrinsics::REGEX_MATCH_SPANS,
                &[Value::str("a+"), Value::str("caaat")],
            )
            .expect("regex dispatch");
        assert_eq!(result, Some(Value::str("1,4")));
    }

    fn run_exit(source: &str) -> i32 {
        let module = witchy_syntax::parser::parse_module(source).expect("parse runtime source");
        run_module_exit(module, ".", Vec::new(), Vec::new(), None)
            .expect("run runtime source")
            .1
    }

    #[test]
    fn existential_dispatch_uses_the_closed_witness_plan() {
        let source = r#"
trait Render:
    fn render(let self) -> Int

type Label:
    Label(Int)

type Badge:
    Badge(Int)

impl Render for Label:
    fn render(let self) -> Int:
        match self:
            Label(value) -> value

impl Render for Badge:
    fn render(let self) -> Int:
        match self:
            Badge(value) -> value + 10

fn main() -> Int:
    let items: List(dyn Render) = [Label(2), Badge(3)]
    items[0].render() + items[1].render()
"#;
        assert_eq!(run_exit(source), 15);
    }

    #[test]
    fn existential_supertrait_upcast_switches_to_the_base_witness() {
        let source = r#"
trait Base:
    fn base(let self) -> Int

trait Render: Base:
    fn render(let self) -> Int

type Label:
    Label(Int)

impl Base for Label:
    fn base(let self) -> Int:
        match self:
            Label(value) -> value

impl Render for Label:
    fn render(let self) -> Int:
        match self:
            Label(value) -> value + 10

fn main() -> Int:
    let rendered: dyn Render = Label(2)
    let base: dyn Base = rendered
    base.base()
"#;
        assert_eq!(run_exit(source), 2);
    }

    #[test]
    fn existential_var_receiver_commits_after_each_structured_return() {
        let source = r#"
trait Tick:
    fn tick(var self) -> Int

type Counter:
    Counter(Int)

impl Tick for Counter:
    fn tick(var self) -> Int:
        let Counter(value) = self
        self = Counter(value + 1)
        value + 1

fn main() -> Int:
    var counter: dyn Tick = Counter(4)
    counter.tick() + counter.tick()
"#;
        assert_eq!(run_exit(source), 11);
    }

    #[test]
    fn existential_var_receiver_commits_on_callee_try_return() {
        let source = r#"
trait Tick:
    fn tick(var self) -> Result(Int, String)
    fn value(let self) -> Int

type Counter:
    Counter(Int)

impl Tick for Counter:
    fn tick(var self) -> Result(Int, String):
        let Counter(value) = self
        self = Counter(value + 1)
        Err("stopped")?
    fn value(let self) -> Int:
        match self:
            Counter(value) -> value

fn main() -> Int:
    var counter: dyn Tick = Counter(4)
    let ignored = counter.tick()
    counter.value()
"#;
        assert_eq!(run_exit(source), 5);
    }

    #[test]
    fn existential_values_stay_opaque_when_the_oracle_skips_source_checking() {
        let module = witchy_syntax::parser::parse_module("fn main() -> Nil:\n    Nil\n")
            .expect("parse minimal module");
        let mut interpreter = Interpreter::new(module);
        let opaque = Value::Existential {
            payload: Box::new(Value::Int(1)),
            witness: 0,
        };
        let error = interpreter
            .values_equal(&Value::list(vec![opaque]), &Value::list(vec![Value::Int(1)]))
            .expect_err("existential equality must stay unavailable");
        assert!(error.message.contains("do not support equality"), "{error:?}");
    }

    #[test]
    fn every_cataloged_list_operation_has_runtime_dispatch() {
        let module = witchy_syntax::parser::parse_module("fn main() -> Int:\n    0\n")
            .expect("parse minimal module");
        let mut interpreter = Interpreter::new(module);

        let list = || Value::list(vec![Value::Int(1)]);
        let some_one = || Value::ctor("Some", vec![Value::Int(1)]);
        let cases = vec![
            (intrinsics::LIST_LENGTH, vec![list()], Value::Int(1)),
            (intrinsics::LIST_AT, vec![list(), Value::Int(0)], Value::Int(1)),
            (
                intrinsics::LIST_PUSH,
                vec![list(), Value::Int(2)],
                Value::list(vec![Value::Int(1), Value::Int(2)]),
            ),
            (
                intrinsics::LIST_SET_AT,
                vec![list(), Value::Int(0), Value::Int(2)],
                Value::list(vec![Value::Int(2)]),
            ),
            (
                intrinsics::LIST_CONCAT,
                vec![list(), list()],
                Value::list(vec![Value::Int(1), Value::Int(1)]),
            ),
            (
                intrinsics::LIST_POP_EXTRACT,
                vec![list()],
                Value::tuple(vec![Value::list(Vec::new()), some_one()]),
            ),
        ];
        for (name, args, expected) in cases {
            let result = interpreter
                .call_builtin(name, &args)
                .unwrap_or_else(|error| panic!("{} failed: {}", name, error.message));
            assert_eq!(result, Some(expected), "{} runtime semantics drifted", name);
        }

        let special = match interpreter
            .call_interpreter_special(intrinsics::LIST_POP_EXTRACT, &[list()])
        {
            Ok(Some(outcome)) => outcome,
            Ok(None) => panic!("pop special dispatch fell through"),
            Err(_) => panic!("pop special dispatch failed"),
        };
        assert_eq!(special, (some_one(), vec![Value::list(Vec::new())]));

        for index in [-1, 1] {
            for (name, args) in [
                (intrinsics::LIST_AT, vec![list(), Value::Int(index)]),
                (
                    intrinsics::LIST_SET_AT,
                    vec![list(), Value::Int(index), Value::Int(2)],
                ),
            ] {
                let error = interpreter.call_builtin(name, &args).expect_err("out of bounds");
                assert!(error.message.contains("out of bounds"), "{}: {}", name, error.message);
            }
        }
    }

    #[test]
    fn every_cataloged_dict_operation_has_runtime_dispatch() {
        let module = witchy_syntax::parser::parse_module(
            "fn inc(n: Int) -> Int:\n    n + 1\n\nfn main() -> Int:\n    0\n",
        )
        .expect("parse dict runtime probe");
        let inc = module.items.iter().find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if function.name == "inc" => {
                Some(Value::Closure {
                    function: closure_function(
                        function.name.clone(),
                        function.params.clone(),
                        function.body.clone(),
                    ),
                    env: Box::new(Env::new()),
                })
            }
            _ => None,
        });
        let inc = inc.expect("inc closure");
        let mut interpreter = Interpreter::new(module);
        let dict = || Value::dict(vec![(Value::Int(1), Value::Int(10))]);
        let some_ten = || Value::ctor("Some", vec![Value::Int(10)]);
        let cases = vec![
            (intrinsics::DICT_NEW, vec![], Value::dict(Vec::new())),
            (
                intrinsics::DICT_INSERT,
                vec![dict(), Value::Int(1), Value::Int(20)],
                Value::dict(vec![(Value::Int(1), Value::Int(20))]),
            ),
            (
                intrinsics::DICT_INSERT_EXTRACT,
                vec![dict(), Value::Int(1), Value::Int(20)],
                Value::tuple(vec![
                    Value::dict(vec![(Value::Int(1), Value::Int(20))]),
                    some_ten(),
                ]),
            ),
            (
                intrinsics::DICT_GET_OR,
                vec![dict(), Value::Int(1), Value::Int(0)],
                Value::Int(10),
            ),
            (
                intrinsics::DICT_AT,
                vec![dict(), Value::Int(1)],
                Value::Int(10),
            ),
            (
                intrinsics::DICT_CONTAINS_KEY,
                vec![dict(), Value::Int(1)],
                Value::Bool(true),
            ),
            (
                intrinsics::DICT_REMOVE,
                vec![dict(), Value::Int(1)],
                Value::dict(Vec::new()),
            ),
            (
                intrinsics::DICT_REMOVE_EXTRACT,
                vec![dict(), Value::Int(1)],
                Value::tuple(vec![Value::dict(Vec::new()), some_ten()]),
            ),
            (
                intrinsics::DICT_KEYS,
                vec![dict()],
                Value::list(vec![Value::Int(1)]),
            ),
            (
                intrinsics::DICT_VALUES,
                vec![dict()],
                Value::list(vec![Value::Int(10)]),
            ),
            (
                intrinsics::DICT_PAIRS,
                vec![dict()],
                Value::list(vec![Value::tuple(vec![Value::Int(1), Value::Int(10)])]),
            ),
            (intrinsics::DICT_LENGTH, vec![dict()], Value::Int(1)),
        ];
        for (name, args, expected) in cases {
            let result = interpreter
                .call_builtin(name, &args)
                .unwrap_or_else(|error| panic!("{} failed: {}", name, error.message));
            assert_eq!(result, Some(expected), "{} runtime semantics drifted", name);
        }

        for (name, args, expected) in [
            (
                intrinsics::DICT_INSERT_EXTRACT,
                vec![dict(), Value::Int(1), Value::Int(20)],
                (
                    some_ten(),
                    vec![Value::dict(vec![(Value::Int(1), Value::Int(20))])],
                ),
            ),
            (
                intrinsics::DICT_REMOVE_EXTRACT,
                vec![dict(), Value::Int(1)],
                (some_ten(), vec![Value::dict(Vec::new())]),
            ),
            (
                intrinsics::DICT_UPDATE,
                vec![dict(), Value::Int(1), Value::Int(0), inc],
                (
                    Value::dict(vec![(Value::Int(1), Value::Int(11))]),
                    Vec::new(),
                ),
            ),
        ] {
            let actual = match interpreter.call_interpreter_special(name, &args) {
                Ok(Some(outcome)) => outcome,
                Ok(None) => panic!("{} special dispatch fell through", name),
                Err(_) => panic!("{} special dispatch failed", name),
            };
            assert_eq!(actual, expected, "{} write-back semantics drifted", name);
        }
    }

    #[test]
    fn evaluates_arithmetic_and_precedence() {
        let out = run(r#"
fn main(console: Console):
    console.print("${(1 + (2 * 3))}")
"#)
            .unwrap();
        assert_eq!(out, vec!["7"]);
    }

    #[test]
    fn mints_a_grantable_user_cap() {
        // (RFC-0038) a `main` binding a bare grantable cap gets a sealed record
        // minted from the `[user_caps]` grant fields, readable in its own module.
        let src = "grantable capability UiRoot:\n    policy: String\n\nfn policy_of(u: UiRoot) -> String:\n    match u:\n        UiRoot(p) -> p\n\nfn main(console: Console, ui: UiRoot):\n    console.print(policy_of(ui))\n";
        let module = witchy_syntax::parser::parse_module(src).unwrap();
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("policy".to_string(), "coven-web".to_string());
        let mut grants: UserCapGrants = std::collections::BTreeMap::new();
        grants.insert("ui".to_string(), fields);
        let out = run_module_user_caps(module, ".", vec![], vec![], vec![], grants).unwrap();
        assert_eq!(out, vec!["coven-web".to_string()]);

        // Without the grant, minting fails loudly (an under-grant).
        let module2 = witchy_syntax::parser::parse_module(src).unwrap();
        let err = run_module_user_caps(module2, ".", vec![], vec![], vec![], Default::default())
            .unwrap_err();
        assert!(err.message.contains("UiRoot") && err.message.contains("user_caps"), "{}", err.message);
    }

    #[test]
    fn build_step_generates_source_through_confined_caps() {
        // A build step reads a schema (BuildRead) and writes generated source
        // (BuildOut). Its authority is exactly the confined grants minted here.
        let dir = std::env::temp_dir().join(format!("witchy_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src_root = dir.join("proj");
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&src_root).unwrap();
        std::fs::write(src_root.join("api.proto"), "service Foo").unwrap();

        let module = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    out.write_out(\"api.witchy\", \"// generated from: \" + schema.read_build(\"api.proto\"))\n",
        )
        .expect("parse");
        let grants = BuildGrants {
            out_dir: out_dir.clone(),
            read_roots: vec![src_root.clone()],
            ..Default::default()
        };
        let generated = run_build_step(module, grants).expect("build step runs");
        assert_eq!(generated, vec!["api.witchy".to_string()]);
        let body = std::fs::read_to_string(out_dir.join("api.witchy")).unwrap();
        assert_eq!(body, "// generated from: service Foo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_step_cannot_escape_or_demand_ungranted_caps() {
        let dir = std::env::temp_dir().join(format!("witchy_build_esc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // BuildRead demanded but not granted ⇒ refused before running.
        let m = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    out.write_out(\"x\", schema.read_build(\"a\"))\n",
        )
        .unwrap();
        let g = BuildGrants { out_dir: dir.join("out"), ..Default::default() };
        let err = run_build_step(m, g).expect_err("ungranted BuildRead must be refused");
        assert!(err.message.contains("no read grant"), "{}", err.message);
        // A confined BuildOut cannot write outside its sandbox.
        let m2 = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut):\n    out.write_out(\"../escape.txt\", \"nope\")\n",
        )
        .unwrap();
        let g2 = BuildGrants { out_dir: dir.join("out2"), ..Default::default() };
        let err = run_build_step(m2, g2).expect_err("a `..` write must be refused");
        assert!(err.message.contains("escapes the Dir capability"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_generated_source_compiles_and_runs() {
        // The whole point: a build step emits real witchy source, which then flows
        // into the normal compile and runs. Here `build` writes a `greet` module,
        // and a consumer imports and calls it.
        let dir = std::env::temp_dir().join(format!("witchy_build_e2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let build_mod = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    out.write_out(\"greet.witchy\", \"pub fn greeting() -> String:\" + nl + \"    \\\"hi from generated code\\\"\" + nl)\n",
        )
        .expect("parse build module");
        let gen_dir = dir.join("gen");
        let files = run_build_step(build_mod, BuildGrants { out_dir: gen_dir.clone(), ..Default::default() })
            .expect("build step runs");
        assert_eq!(files, vec!["greet.witchy".to_string()]);
        let generated = std::fs::read_to_string(gen_dir.join("greet.witchy")).unwrap();
        // The generated source links with a consumer and runs.
        let consumer = "import greet\nfn main(console: Console):\n    console.print(greet.greeting())\n";
        let out = run_program(&[("greet", generated.as_str()), ("main", consumer)], "main")
            .expect("generated source compiles and runs");
        assert_eq!(out, vec!["hi from generated code"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_read_spans_multiple_granted_roots() {
        // A BuildRead grant can name several confined roots; `read_build` resolves
        // a path against the first root that holds it — and still nothing else.
        let dir = std::env::temp_dir().join(format!("witchy_build_mr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("from_a.txt"), "ALPHA").unwrap();
        std::fs::write(b.join("from_b.txt"), "BETA").unwrap();

        let module = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, src: BuildRead):\n    out.write_out(\"g.txt\", src.read_build(\"from_a.txt\") + \"/\" + src.read_build(\"from_b.txt\"))\n",
        )
        .unwrap();
        let grants = BuildGrants {
            out_dir: dir.join("out"),
            read_roots: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        run_build_step(module, grants).expect("reads across both roots");
        assert_eq!(std::fs::read_to_string(dir.join("out/g.txt")).unwrap(), "ALPHA/BETA");

        // A file in neither root is refused.
        let m2 = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, src: BuildRead):\n    out.write_out(\"g.txt\", src.read_build(\"nope.txt\"))\n",
        )
        .unwrap();
        let g2 = BuildGrants { out_dir: dir.join("out2"), read_roots: vec![a, b], ..Default::default() };
        let e = run_build_step(m2, g2).expect_err("a path in no granted root must fail");
        assert!(e.message.contains("not found in any granted read root"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_env_reads_only_named_variables() {
        // A build step never sees the whole environment: `BuildEnv` carries an
        // allow-list of *named* keys, and reading anything else is refused —
        // even a variable that exists in the process env.
        let dir = std::env::temp_dir().join(format!("witchy_build_env_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let granted = witchy_syntax::parser::parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match env.get_build_env(\"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"g.txt\", v)\n",
        )
        .unwrap();
        let g = BuildGrants {
            out_dir: dir.join("out"),
            env: [("WITCHY_BUILD_ALLOWED".to_string(), Some("yes".to_string()))].into(),
            ..Default::default()
        };
        run_build_step(granted, g).expect("a named key reads fine");
        assert_eq!(std::fs::read_to_string(dir.join("out/g.txt")).unwrap(), "yes");

        // The same grant cannot read a key it didn't name.
        let denied = witchy_syntax::parser::parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match env.get_build_env(\"WITCHY_BUILD_SECRET\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"g.txt\", v)\n",
        )
        .unwrap();
        let g2 = BuildGrants {
            out_dir: dir.join("out2"),
            env: [("WITCHY_BUILD_ALLOWED".to_string(), Some("yes".to_string()))].into(),
            ..Default::default()
        };
        let err = run_build_step(denied, g2).expect_err("an unlisted key must be refused");
        assert!(err.message.contains("not in this BuildEnv grant's allow-list"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_net_fetches_only_allow_listed_hosts() {
        // A local one-shot HTTP listener stands in for "the network": the build
        // step may fetch from it only because the grant allow-lists exactly that
        // host:port; any other destination is refused before a packet moves.
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            let body = "schema-v1";
            let _ = sock.write_all(
                format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len())
                    .as_bytes(),
            );
        });

        let dir = std::env::temp_dir().join(format!("witchy_build_net_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let module = witchy_syntax::parser::parse_module(
            &format!(
                "fn build(out: BuildOut, dl: BuildNet):\n    out.write_out(\"got.txt\", dl.fetch_build(\"{addr}\", \"/schema\"))\n"
            ),
        )
        .unwrap();
        let grants = BuildGrants {
            out_dir: dir.join("out"),
            net_hosts: vec![addr.clone()],
            ..Default::default()
        };
        run_build_step(module, grants).expect("allow-listed fetch runs");
        assert_eq!(std::fs::read_to_string(dir.join("out/got.txt")).unwrap(), "schema-v1");
        server.join().unwrap();

        // A host NOT on the allow-list is refused — even one that exists.
        let m2 = witchy_syntax::parser::parse_module(
            &format!(
                "fn build(out: BuildOut, dl: BuildNet):\n    out.write_out(\"x\", dl.fetch_build(\"{addr}\", \"/\"))\n"
            ),
        )
        .unwrap();
        let g2 = BuildGrants {
            out_dir: dir.join("out2"),
            net_hosts: vec!["allowed.example:80".to_string()],
            ..Default::default()
        };
        let e = run_build_step(m2, g2).expect_err("an un-allow-listed host must be refused");
        assert!(e.message.contains("not in this BuildNet grant's allow-list"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_exec_runs_only_allow_listed_tools() {
        // `cat` echoes its stdin, so the generated file is exactly the input —
        // deterministic. The grant allow-lists `cat`; anything else is refused.
        let dir = std::env::temp_dir().join(format!("witchy_build_exec_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let module = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, cc: BuildExec):\n    out.write_out(\"x.txt\", cc.run_tool(\"cat\", \"piped-input\"))\n",
        )
        .unwrap();
        let grants = BuildGrants {
            out_dir: dir.join("out"),
            exec_tools: vec!["cat".to_string()],
            ..Default::default()
        };
        let generated = run_build_step(module, grants).expect("cat is allow-listed");
        assert_eq!(generated, vec!["x.txt".to_string()]);
        assert_eq!(std::fs::read_to_string(dir.join("out/x.txt")).unwrap(), "piped-input");

        // A tool NOT on the allow-list is refused before it runs.
        let m2 = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, cc: BuildExec):\n    out.write_out(\"x.txt\", cc.run_tool(\"rm\", \"-rf /\"))\n",
        )
        .unwrap();
        let g2 = BuildGrants {
            out_dir: dir.join("out2"),
            exec_tools: vec!["cat".to_string()],
            ..Default::default()
        };
        let err = run_build_step(m2, g2).expect_err("an un-allow-listed tool must be refused");
        assert!(err.message.contains("not in this BuildExec grant's allow-list"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_capability_rejects_path_traversal() {
        // A Dir capability is confined to its subtree. `resolve` must reject any
        // path that would escape it, so a holder (e.g. an untrusted library
        // handed a narrow Dir) can read within the subtree but never above it.
        use std::path::Path;
        let base = Path::new(".");
        // Positive control: a path inside the subtree resolves (Cargo.toml is at
        // the crate root, the CWD for tests).
        assert!(resolve(base, "Cargo.toml").is_ok());
        // `..` is rejected lexically, before any filesystem access.
        assert!(resolve(base, "../secret").is_err());
        assert!(resolve(base, "src/../../etc/passwd").is_err());
        // Absolute paths are rejected: the capability is a subtree, not root.
        assert!(resolve(base, "/etc/passwd").is_err());
    }

    #[test]
    fn calls_user_functions_and_concats_strings() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    console.print(("doubled: " + "${double(21)}"))
"#;
        assert_eq!(run(src).unwrap(), vec!["doubled: 42"]);
    }

    #[test]
    fn pipelines_thread_left_to_right() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    let result = "${double(4)}"
    console.print(result)
"#;
        assert_eq!(run(src).unwrap(), vec!["8"]);
    }

    #[test]
    fn match_with_constructors_and_guards() {
        let src = r#"
fn describe(e: Event) -> String:
    match e:
        Click(x, _) if (x > 0) -> "right click"
        Click(_, _) -> "other click"
        Closed -> "closed"
        _ -> "unknown"

fn main(console: Console):
    console.print(describe(Click(5, 9)))
    console.print(describe(Click((-1), 0)))
    console.print(describe(Closed))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["right click", "other click", "closed"]
        );
    }

    #[test]
    fn if_else_and_let_bindings() {
        let src = r#"
fn sign(n: Int) -> String:
    let label = if (n > 0): "positive" else: "non-positive"
    label

fn main(console: Console):
    console.print(sign(3))
    console.print(sign((-2)))
"#;
        assert_eq!(run(src).unwrap(), vec!["positive", "non-positive"]);
    }

    #[test]
    fn recursion_works() {
        let src = r#"
fn fact(n: Int) -> Int:
    match n:
        0 -> 1
        _ -> (n * fact((n - 1)))

fn main(console: Console):
    console.print("${fact(5)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["120"]);
    }

    #[test]
    fn reports_unknown_function() {
        let e = run(r#"
fn main():
    nope()
"#).unwrap_err();
        assert!(e.message.contains("unknown function"));
    }

    /// The capability thesis at the language level: a function that was never
    /// handed the Console capability cannot print, even though `print` exists.
    #[test]
    fn function_without_capability_cannot_print() {
        let src = r#"
fn leak(secret: String) -> Nil:
    print(secret)

fn main(console: Console):
    leak("password")
"#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("Console capability"),
            "expected a capability error, got: {}",
            e.message
        );
    }

    /// Holding the capability, the same effect succeeds — capabilities
    /// propagate by being passed explicitly.
    #[test]
    fn capability_can_be_threaded_to_a_helper() {
        let src = r#"
fn announce(console: Console, who: String) -> Nil:
    console.print(("hello, " + who))

fn main(console: Console):
    announce(console, "witchy")
"#;
        assert_eq!(run(src).unwrap(), vec!["hello, witchy"]);
    }

    #[test]
    fn dir_capability_reads_attenuates_and_confines() {
        let root = std::env::temp_dir().join(format!("witchy_fs_{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/hi.txt"), "hi!").unwrap();

        // Attenuate to a subdir and read a file within it.
        let ok = r#"
fn main(console: Console, root: Dir):
    let d = root.subtree("sub")
    console.print(d.read("hi.txt"))
"#;
        assert_eq!(run_in(ok, &root).unwrap(), vec!["hi!"]);

        // Confinement: `..` cannot escape the granted subtree.
        let escape = r#"
fn main(console: Console, root: Dir):
    console.print(root.read("../secret"))
"#;
        assert!(run_in(escape, &root).is_err());

        // A function with no Dir cannot read (no way to obtain the capability).
        let no_cap = r#"
fn sneaky() -> String:
    root.read("sub/hi.txt")

fn main(console: Console, root: Dir):
    console.print(sneaky())
"#;
        assert!(run_in(no_cap, &root).is_err());

        // Confinement holds against symlinks: a link inside the subtree pointing
        // outside it must not be followable.
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!("witchy_outside_{}", std::process::id()));
            std::fs::write(&outside, "secret").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("sub/escape")).ok();
            let via_symlink = r#"
fn main(console: Console, root: Dir):
    let d = root.subtree("sub")
    console.print(d.read("escape"))
"#;
            assert!(run_in(via_symlink, &root).is_err());
            std::fs::remove_file(&outside).ok();
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn dir_list_rejects_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;

        let root = std::env::temp_dir().join(format!(
            "witchy_nonutf8_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("normal.txt"), "ok").unwrap();
        let bad = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![
            0xbd, 0xb2, b'=', 0xbc,
        ]));
        if std::fs::write(root.join(bad), "hidden").is_err() {
            // Some Unix filesystems (notably macOS APFS/HFS configurations)
            // reject non-UTF-8 names at creation time; the runtime bug is only
            // observable where the host filesystem can contain such an entry.
            std::fs::remove_dir_all(&root).ok();
            return;
        }

        let src = r#"
fn main(console: Console, root: Dir):
    let names = root.list()
    console.print("${list.length(names)}")
"#;
        let err = run_in(src, &root).expect_err("non-UTF-8 names must be loud");
        assert!(
            err.message.contains("not valid UTF-8"),
            "unexpected error: {}",
            err.message
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn net_capability_connects_attenuates_and_denies() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        // One-shot loopback echo server.
        let server = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut r = BufReader::new(stream);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let _ = r.get_mut().write_all(line.as_bytes());
            }
        });

        // Attenuate to the one held address, connect, send, receive the echo.
        let (host, port) = addr.rsplit_once(':').expect("addr is host:port");
        let ok = format!(
            r#"
fn main(console: Console, net: Net):
    let only = net.only(Net.tcp("{host}", {port}))
    let s = only.connect("{addr}")
    s.send_line("ping")
    console.print(s.recv_line())
"#
        );
        // Link in the bundled std (`policy` is preluded), then run.
        let linked_ok = crate::pipeline::link(
            vec![("main".to_string(), witchy_syntax::parser::parse_module(&ok).expect("parse"))],
            "main",
        )
        .expect("link");
        assert_eq!(run_module(linked_ok, ".", vec![addr.clone()]).unwrap(), vec!["ping"]);
        server.join().ok();

        // Denied: connecting to an address not in the allow-list.
        let denied = r#"
fn main(console: Console, net: Net):
    let s = net.connect("10.255.255.1:80")
    s.send_line("x")
"#;
        assert!(run_with(denied, ".", vec![addr.clone()]).is_err());

        // Denied: cannot attenuate to an address not already held.
        let bad_restrict = r#"
fn main(console: Console, net: Net):
    let bad = net.only(Net.tcp("10.255.255.1", 80))
    console.print("unreachable")
"#;
        let linked_bad = crate::pipeline::link(
            vec![("main".to_string(), witchy_syntax::parser::parse_module(bad_restrict).expect("parse"))],
            "main",
        )
        .expect("link");
        assert!(run_module(linked_bad, ".", vec![addr]).is_err());
    }

    #[test]
    fn net_server_listen_accept_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        // A free port to hand the witchy server (bind+drop to discover it).
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
fn main(console: Console, net: Net):
    let server = net.listen("{addr}")
    let sock = server.accept()
    let line = sock.recv_line()
    console.print(line)
    sock.send_bytes("HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello witchy")
    sock.close()
"#
        );
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_with(&src, ".", allow));

        // Connect once the server has bound (retry through the bind race).
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect to witchy server");
        stream.write_all(b"GET /hi HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK"), "resp: {resp}");
        assert!(resp.ends_with("hello witchy"), "resp: {resp}");

        let out = server.join().unwrap().unwrap();
        assert_eq!(out, vec!["GET /hi HTTP/1.1\r"]);
    }

    #[test]
    fn recv_bytes_does_not_preallocate_attacker_count() {
        // (BUG-065) `sock.recv_bytes(n)` must NOT pre-allocate `n` bytes up front —
        // `n` is an attacker-controlled count (an HTTP Content-Length up to i64::MAX),
        // so `vec![0u8; n]` before reading a single byte is a remote OOM. The fix reads
        // in bounded chunks: a huge `n` against a peer that sends only a few bytes then
        // closes returns exactly the bytes received, without allocating the claimed
        // count. (The compiled backend already reads chunked — parity.)
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(b"hi");
                // dropping `stream` closes the connection => EOF for the reader.
            }
        });

        let (host, port) = addr.rsplit_once(':').expect("addr is host:port");
        // Claim ~2 billion bytes but the peer sends 2 then closes.
        let src = format!(
            r#"
fn main(console: Console, net: Net):
    let only = net.only(Net.tcp("{host}", {port}))
    let s = only.connect("{addr}")
    console.print(s.recv_bytes(2000000000))
"#
        );
        let linked = crate::pipeline::link(
            vec![("main".to_string(), witchy_syntax::parser::parse_module(&src).expect("parse"))],
            "main",
        )
        .expect("link");
        assert_eq!(run_module(linked, ".", vec![addr.clone()]).unwrap(), vec!["hi"]);
        server.join().ok();
    }

    #[test]
    fn serve_loopback_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router()
        .get("/", fn(req: Request): server.text(200, "home"))
        .get("/users/:id", fn(req: Request): server.text(200, "user " + server.param_or(req, "id", "")))
        .post("/echo", fn(req: Request): server.text(201, server.request_body(req)))
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        // Link in the bundled std (http + its deps), then run on a thread.
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let r1 = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("200 OK") && r1.ends_with("home"), "r1: {r1}");
        let r2 = request("GET /users/42 HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.ends_with("user 42"), "r2: {r2}");
        let r3 = request("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello");
        assert!(r3.contains("201 ") && r3.ends_with("hello"), "r3: {r3}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn http_client_builder_loopback() {
        // The reqwest-style client builder (get_request().with_header(...).send(net))
        // against a raw TCP server: it sends the method/path/header and parses
        // the response status and body.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let srv = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nhello!!")
                .unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });

        let src = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let req = http.get_request("http://{addr}/path")
        .with_header("X-Test", "abc")
        .with_query("q", "hi")
    match req.send(net):
        Ok(resp) ->
            console.print("${{http.status(resp)}}")
            console.print(http.body(resp))
            console.print("${{http.is_success(resp)}}")
        Err(e) -> console.print("err: " + e)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let out = run_module(linked, ".", vec![addr.clone()]).expect("run");
        assert_eq!(out, vec!["200", "hello!!", "true"]);
        let req = srv.join().unwrap();
        assert!(req.contains("GET /path?q=hi HTTP/1.1"), "req: {req}");
        assert!(req.contains("X-Test: abc"), "req: {req}");
    }

    #[test]
    fn serve_status_constructors_roundtrip() {
        // The status-named response constructors (created/bad_request/
        // unauthorized/no_content) render the right status line and reason.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router().post("/make", fn(req: Request): server.created("made")).get("/bad", fn(req: Request): server.bad_request("nope")).get("/secret", fn(req: Request): server.unauthorized("auth")).delete("/item", fn(req: Request): server.no_content())
    server.serve_n(net, "{addr}", app, 4)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let r1 = request("POST /make HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(r1.contains("201 Created") && r1.ends_with("made"), "r1: {r1}");
        let r2 = request("GET /bad HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.contains("400 Bad Request") && r2.ends_with("nope"), "r2: {r2}");
        let r3 = request("GET /secret HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r3.contains("401 Unauthorized") && r3.ends_with("auth"), "r3: {r3}");
        let r4 = request("DELETE /item HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r4.contains("204 No Content"), "r4: {r4}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_method_not_allowed_vs_not_found() {
        // A known path with the wrong method is a 405; an unknown path is a 404.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router().post("/items", fn(req: Request): server.created("ok"))
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let ok = request("POST /items HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(ok.contains("201 Created"), "ok: {ok}");
        let wrong = request("GET /items HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(wrong.contains("405 Method Not Allowed"), "wrong: {wrong}");
        let missing = request("GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(missing.contains("404 Not Found"), "missing: {missing}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_var_receiver_method_resolution() {
        // Method calls on a variable receiver (`var app = router(); app = app.get(...)`)
        // resolve the overloaded `get`/`post` by the tracked variable type (Router),
        // even though http/server/json all export `get`.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
from http import Request, Response
fn main(console: Console, net: Net):
    var app = server.router()
    app = app.get("/", fn(req: Request): server.ok("home"))
    app = app.post("/items", fn(req: Request): server.created("made"))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let g = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(g.contains("200 OK") && g.ends_with("home"), "g: {g}");
        let p = request("POST /items HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(p.contains("201 Created") && p.ends_with("made"), "p: {p}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_any_method_route_roundtrip() {
        // An `any` route answers every verb (the `*` wildcard method).
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router().any("/ping", fn(req: Request): server.ok(server.method(req)))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let g = request("GET /ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(g.contains("200 OK") && g.ends_with("GET"), "g: {g}");
        let d = request("DELETE /ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(d.contains("200 OK") && d.ends_with("DELETE"), "d: {d}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_middleware_nest_and_notfound_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response

// A tower-style Layer that tags every response with a header.
fn tagger(next: fn(Request) -> Response) -> fn(Request) -> Response:
    fn(req: Request): tag(next(req))

fn tag(resp: Response) -> Response:
    server.with_header(resp, "x-by", "witchy")

fn main(console: Console, net: Net):
    let api = server.router().get("/ping", fn(req: Request): server.text(200, "pong"))
    let app = server.router().get("/", fn(req: Request): server.text(200, "root")).nest("/api", api).layer(tagger)
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        // Middleware tagged the response; root handler ran.
        let r1 = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("x-by: witchy") && r1.ends_with("root"), "r1: {r1}");
        // Nested route under /api.
        let r2 = request("GET /api/ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.ends_with("pong"), "r2: {r2}");
        // Unknown path -> 404 (still tagged by the layer).
        let r3 = request("GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r3.contains("404 ") && r3.contains("x-by: witchy"), "r3: {r3}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_json_handler_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
from http import Request, Response
from json import Json
fn greet(req: Request) -> Response:
    server.json_value(200, JsonObject([("hello", JsonString(server.param_or(req, "name", "")))]))
fn main(console: Console, net: Net):
    let app = server.router().get("/hello/:name", greet)
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        stream.write_all(b"GET /hello/witchy HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("application/json"), "resp: {resp}");
        assert!(resp.contains("\"hello\"") && resp.contains("\"witchy\""), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_json_body_decode_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
import option
from http import Request, Response
from json import Json
fn name_of(doc: Json) -> String:
    match json.get(doc, "name"):
        Some(v) -> option.unwrap_or(json.as_string(v), "?")
        None -> "?"
fn echo_name(req: Request) -> Response:
    match server.json_body(req):
        Ok(doc) -> server.text(200, name_of(doc))
        Err(e) -> server.text(400, e)
fn main(console: Console, net: Net):
    let app = server.router().post("/", echo_name)
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        let body = "{\"name\":\"witchy\"}";
        let req = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK") && resp.ends_with("witchy"), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_form_field_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router().post("/", fn(req: Request): server.text(200, server.form_field_or(req, "name", "")))
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        let body = "name=witchy&lang=rust";
        let req = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK") && resp.ends_with("witchy"), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_static_files_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        // The handler captures a Dir rooted at examples/data and serves from it.
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn file_server(dir: Dir) -> fn(Request) -> Response:
    fn(req: Request): serve_file(dir, server.param_or(req, "path", ""))
fn serve_file(dir: Dir, p: String) -> Response:
    if dir.exists(p):
        server.text(200, dir.read(p))
    else:
        server.not_found()
fn main(console: Console, net: Net, root: Dir):
    let examples = root.subtree("examples")
    let data = subtree(examples, "data")
    let app = server.router().get("/files/*path", file_server(data))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, concat!(env!("CARGO_MANIFEST_DIR"), "/../.."), allow));

        let request = |raw: &str| -> String {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                assert!(!server.is_finished(), "server exited before accepting a request");
                assert!(
                    std::time::Instant::now() < deadline,
                    "server did not accept a request within 10 seconds",
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };

        let r1 = request("GET /files/greeting.txt HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("200 OK") && r1.contains("sandboxed Dir"), "r1: {r1}");
        let r2 = request("GET /files/nope.txt HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.contains("404 "), "r2: {r2}");

        drop(request);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn handlers_cannot_reach_the_network() {
        // The capability guarantee: a pure handler has no Net, so even trying to
        // open a socket is a compile-time (type) error — it can't be written.
        let src = r#"
import server
from http import Request, Response
fn evil(req: Request) -> Response:
    let s = net.connect("10.0.0.1:80")
    server.text(200, "leaked")
fn main(console: Console, net: Net):
    let app = server.router().get("/", evil)
    server.serve_n(net, "127.0.0.1:0", app, 0)
"#;
        let parsed = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        // Type-check the linked program: `connect` needs a Net the handler lacks.
        assert!(witchy_types::typeck::check(&linked).is_err());
    }

    #[test]
    fn nonexhaustive_match_diagnostic_renders_home_type_bare() {
        // BUG-292: a home-module type/variant renders bare (the spelling the reader
        // wrote) in a non-exhaustive-match diagnostic — never the `prog.Color`
        // file-stem qualifier — and the missing-variant list is backticked.
        let src = r#"
type Color:
    Red
    Blue

fn pick(c: Color) -> Int:
    match c:
        Red -> 1

fn main(console: Console):
    console.print("${pick(Red)}")
"#;
        let parsed = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("prog".to_string(), parsed)], "prog").expect("link");
        let err = witchy_types::typeck::check(&linked).expect_err("non-exhaustive match");
        assert!(err.message.contains("non-exhaustive match on `Color`"), "{}", err.message);
        assert!(err.message.contains("missing `Blue`"), "{}", err.message);
        assert!(
            !err.message.contains("prog.Color") && !err.message.contains("prog.Blue"),
            "home-module file stem leaked: {}",
            err.message
        );
    }

    #[test]
    fn modules_qualified_calls() {
        let strutil = r#"
pub fn shout(name: String) -> String:
    ("HELLO, " + name)
"#;
        let app = r#"
import strutil

fn main(console: Console):
    console.print(strutil.shout("witchy"))
"#;
        assert_eq!(
            run_program(&[("strutil", strutil), ("app", app)], "app").unwrap(),
            vec!["HELLO, witchy"]
        );
    }

    #[test]
    fn run_program_rejects_reserved_std_module_replacement() {
        let fake_show = "pub fn render(n: Int) -> String:\n    \"fake\"\n";
        let app = "import show\n\nfn main(console: Console):\n    console.print(show.render(1))\n";
        let err = run_program(&[("show", fake_show), ("app", app)], "app").unwrap_err();
        assert!(
            err.message.contains("module `show` uses a reserved standard-library name"),
            "{}",
            err.message
        );
    }

    #[test]
    fn library_uses_only_passed_capabilities() {
        // The app chooses to hand the logger its Console.
        let logger = r#"
pub fn log(console: Console, msg: String):
    console.print(("[log] " + msg))
"#;
        let app = r#"
import logger

fn main(console: Console):
    logger.log(console, "hi")
"#;
        assert_eq!(
            run_program(&[("logger", logger), ("app", app)], "app").unwrap(),
            vec!["[log] hi"]
        );
    }

    #[test]
    fn library_cannot_fabricate_a_capability() {
        // `steal` references `console` it was never given — caught at compile
        // time as an unbound variable (no ambient authority to grab).
        let evil = r#"
pub fn steal(secret: String) -> String:
    console.print(secret)
"#;
        let app = r#"
import evil

fn main(console: Console):
    console.print(evil.steal("data"))
"#;
        let linked = crate::pipeline::link(
            vec![
                ("evil".into(), parse_module(evil).unwrap()),
                ("app".into(), parse_module(app).unwrap()),
            ],
            "app",
        )
        .unwrap();
        assert!(witchy_types::typeck::check(&linked).is_err());
    }

    #[test]
    fn calling_unimported_module_is_a_link_error() {
        let app = r#"
fn main(console: Console):
    console.print(other.foo())
"#;
        assert!(run_program(&[("app", app)], "app").is_err());
    }

    #[test]
    fn float_arithmetic() {
        let src = r#"
fn half(x: Float) -> Float:
    (x / 2.0)

fn main(console: Console):
    console.print("${half(7.0)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["3.5"]);
    }

    #[test]
    fn boolean_operators() {
        let src = r#"
fn classify(n: Int) -> String:
    if ((n > 0) && (n < 10)):
        "small positive"
    else if ((n <= 0) || (n >= 100)):
        "out of range"
    else:
        "other"

fn main(console: Console):
    console.print(classify(5))
    console.print(classify((-1)))
    console.print(classify(50))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["small positive", "out of range", "other"]
        );
    }

    #[test]
    fn tuples_destructure_and_match() {
        let src = r#"
fn divmod(a: Int, b: Int) -> (Int, Int):
    ((a / b), (a % b))

fn main(console: Console):
    let (q, r) = divmod(17, 5)
    console.print("${q}")
    console.print("${r}")
    let pair = (1, "one")
    match pair:
        (n, name) -> console.print((("${n}" + "=") + name))
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "2", "1=one"]);
    }

    #[test]
    fn generic_identity_runs() {
        let src = r#"
fn id(x: a) -> a:
    x

fn main(console: Console):
    console.print(id("hi"))
    console.print("${id(5)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["hi", "5"]);
    }

    #[test]
    fn generic_adt_runs() {
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn show(r: Result(Int, String)) -> String:
    match r:
        Ok(n) -> ("ok " + "${n}")
        Err(msg) -> ("err " + msg)

fn main(console: Console):
    console.print(show(Ok(7)))
    console.print(show(Err("boom")))
"#;
        assert_eq!(run(src).unwrap(), vec!["ok 7", "err boom"]);
    }

    /// Run a no-parameter `main` with a small step ceiling, so an infinite loop
    /// is caught quickly instead of hanging the test.
    fn run_capped(src: &str, limit: u64) -> Result<Vec<String>, RuntimeError> {
        let module = parse_module(src).map_err(|e| RuntimeError { message: e.to_string() })?;
        let mut interp = Interpreter::new(module);
        interp.step_limit = limit;
        interp.call("main", vec![])?;
        Ok(interp.output)
    }

    #[test]
    fn early_return_exits_function_and_loop() {
        let src = r#"
fn first_even(xs: List(Int)) -> Int:
    for x in xs:
        if ((x % 2) == 0):
            return x
    (0 - 1)

fn main(console: Console):
    console.print("${first_even([1, 3, 8, 5])}")
    console.print("${first_even([1, 3, 5])}")
"#;
        assert_eq!(run(src).unwrap(), vec!["8", "-1"]);
    }

    #[test]
    fn negative_int_patterns_match() {
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        -1 -> "neg one"
        0 -> "zero"
        _ -> "other"

fn main(console: Console):
    console.print(classify((-1)))
    console.print(classify(0))
    console.print(classify(3))
"#;
        assert_eq!(run(src).unwrap(), vec!["neg one", "zero", "other"]);
    }

    #[test]
    fn deep_self_tail_recursion_uses_constant_stack() {
        let src = r#"
fn rec(n: Int) -> Int:
    if (n == 0):
        0
    else:
        rec((n - 1))

fn main(console: Console):
    console.print("${rec(5000000)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["0"]);
    }

    #[test]
    fn deep_mutual_tail_recursion_uses_constant_stack() {
        let src = r#"
fn even(n: Int) -> Bool:
    if n == 0:
        true
    else:
        odd(n - 1)

fn odd(n: Int) -> Bool:
    if n == 0:
        false
    else:
        even(n - 1)

fn main(console: Console):
    console.print("${even(250000)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["true"]);
    }

    #[test]
    fn mutual_tail_edges_stage_arguments_and_honor_explicit_return() {
        let src = r#"
fn left(n: Int, a: Int, b: Int) -> Int:
    if n == 0:
        return a * 10 + b
    return right(n - 1, b, a)

fn right(n: Int, a: Int, b: Int) -> Int:
    if n == 0:
        return a * 10 + b
    return left(n - 1, b, a)

fn main(console: Console):
    console.print("${left(1001, 2, 7)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["72"]);
    }

    #[test]
    fn non_tail_recursion_still_reports_the_depth_guard() {
        let src = r#"
fn rec(n: Int) -> Int:
    if (n == 0):
        0
    else:
        (1 + rec((n - 1)))

fn main(console: Console):
    console.print("${rec(5000000)}")
"#;
        let error = run(src).unwrap_err();
        assert!(error.message.contains("too deep"), "got: {}", error.message);
    }

    #[test]
    fn self_tail_arguments_rebind_simultaneously() {
        let src = r#"
fn swap_down(n: Int, a: Int, b: Int) -> Int:
    if (n == 0):
        ((a * 10) + b)
    else:
        swap_down((n - 1), b, a)

fn main(console: Console):
    console.print("${swap_down(1000001, 2, 7)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["72"]);
    }

    #[test]
    fn nested_explicit_return_is_a_tail_position() {
        let src = r#"
fn down(n: Int) -> Int:
    if (n > 0):
        return down((n - 1))
    9

fn main(console: Console):
    console.print("${down(5000000)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["9"]);
    }

    #[test]
    fn self_tail_calls_still_consume_the_evaluation_budget() {
        let src = r#"
fn forever(n: Int) -> Int:
    forever((n + 1))

fn main():
    forever(0)
"#;
        let error = run_capped(src, 100).unwrap_err();
        assert!(error.message.contains("step budget"), "got: {}", error.message);
    }

    #[test]
    fn local_closure_shadowing_function_name_is_not_self_recursion() {
        let src = r#"
fn apply_once(n: Int) -> Int:
    let apply_once = fn(x: Int): (x + 1)
    apply_once(n)

fn main(console: Console):
    console.print("${apply_once(41)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    #[test]
    fn indirect_closure_cycle_uses_one_callable_boundary() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn drive(bounce: Bounce, n: Int) -> Int:
    match bounce:
        Bounce(f) -> f(bounce, n)

fn step(bounce: Bounce, n: Int) -> Int:
    if n == 0:
        5000000007
    else:
        drive(bounce, n - 1)

fn main(console: Console):
    let bounce = Bounce(step)
    console.print("${drive(bounce, 250000)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["5000000007"]);
    }

    #[test]
    fn moderate_recursion_succeeds() {
        // Recursion well within the limit still works.
        let src = r#"
fn rec(n: Int) -> Int:
    if (n == 0):
        0
    else:
        rec((n - 1))

fn main(console: Console):
    console.print("${rec(10000)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["0"]);
    }

    #[test]
    fn integer_overflow_wraps_like_the_wasm_backend() {
        // Multiplication that overflows i64 WRAPS (two's complement), identical to
        // the WASM backend's `i64.mul`, never panicking the host.
        let src = r#"
fn main(console: Console):
    let big = 9999999999
    console.print("${(big * big)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["7766279611452241921"]);
    }

    #[test]
    fn negating_int_min_wraps_not_panics() {
        // -(i64::MIN) wraps back to i64::MIN (matching the WASM backend), never a
        // host panic.
        let src = r#"
fn main(console: Console):
    let lo = ((0 - 9223372036854775807) - 1)
    console.print("${(-lo)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["-9223372036854775808"]);
    }

    #[test]
    fn runtime_errors_report_their_source_line() {
        // Division by zero happens on the third line.
        let src = "fn main(console: Console):\n    let a = 1\n    console.print(\"${a / 0}\")\n";
        let e = run(src).unwrap_err();
        assert!(e.message.contains("line 3"), "got: {}", e.message);
    }

    #[test]
    fn runtime_errors_name_the_innermost_function() {
        // The error must be attributed to `risky`, not the caller `main`.
        let src = r#"
fn risky(n: Int) -> Int:
    (n / 0)

fn main(console: Console):
    console.print("${risky(5)}")
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("risky"), "got: {}", e.message);
    }

    #[test]
    fn assertion_failures_report_the_user_call_site_not_stdlib() {
        // Regression (M6): a failed `std/testing` assertion used to report the
        // `fail` line buried inside std/testing (always the same line, for every
        // failure). It must instead point at the user's call site — and at the
        // call STATEMENT's line even when an argument is a nested call that moves
        // the line cursor (`helper(1)` here is on line 3, the assertion on line 5).
        let src = "import testing\nfn helper(n: Int) -> Int:\n    n + 1\nfn main(console: Console):\n    testing.assert_int_eq(helper(1), 5)\n";
        let parsed = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let e = run_module(linked, ".", vec![]).unwrap_err();
        assert!(e.message.contains("`main`, line 5"), "got: {}", e.message);
        assert!(!e.message.contains("testing."), "should not name the stdlib frame: {}", e.message);
        assert!(e.message.contains("got 2, want 5"), "got: {}", e.message);
    }

    #[test]
    fn runaway_loop_is_bounded_not_hung() {
        let src = r#"
fn main() -> Int:
    var i = 0
    while true:
        i = (i + 1)
    i
"#;
        let e = run_capped(src, 100_000).unwrap_err();
        assert!(e.message.contains("step budget"), "got: {}", e.message);
    }

    #[test]
    fn normal_program_runs_within_budget() {
        // A finite loop well under the ceiling completes normally.
        let src = r#"
fn main() -> Int:
    var sum = 0
    var i = 0
    while (i < 1000):
        sum = (sum + i)
        i = (i + 1)
    sum
"#;
        assert!(run_capped(src, 100_000).is_ok());
    }

    #[test]
    fn dict_values_and_pairs_iterate() {
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.__insert(d, "a", 10)
    d = dict.__insert(d, "b", 20)
    var sum = 0
    for v in dict.values(d):
        sum = (sum + v)
    console.print("${sum}")
    var report = ""
    for e in dict.pairs(d):
        let (k, v) = e
        report = ((((report + k) + "=") + "${v}") + ";")
    console.print(report)
"#;
        assert_eq!(run(src).unwrap(), vec!["30", "a=10;b=20;"]);
    }

    #[test]
    fn dict_insert_get_has_keys_and_immutability() {
        let src = r#"
fn main(console: Console):
    let a = dict.__insert(dict.new(), "x", 1)
    let b = dict.__insert(a, "y", 2)
    let c = dict.__insert(b, "x", 9)
    console.print("${dict.get_or(c, "x", 0)}")
    console.print("${dict.get_or(c, "y", 0)}")
    console.print("${dict.get_or(c, "z", 0)}")
    console.print("${dict.length(c)}")
    console.print("${dict.get_or(a, "x", 0)}")
    console.print("${dict.contains_key(c, "y")}")
    console.print("${list.length(dict.keys(c))}")
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["9", "2", "0", "2", "1", "true", "2"]
        );
    }

    #[test]
    fn sqrt_builtin_computes() {
        let src = r#"
fn main(console: Console):
    console.print("${math.sqrt(2.0)}")
    console.print("${math.to_int(math.sqrt(144.0))}")
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["1.4142135623730951", "12"]
        );
    }

    #[test]
    fn string_slicing_and_search() {
        let src = r#"
fn main(console: Console):
    let s = "abcdef"
    console.print(string.substring(s, 1, 4))
    console.print(string.substring(s, 4, 100))
    console.print(string.substring(s, 3, 1))
    console.print("${string.find(s, "cd")}")
    console.print("${string.find(s, "z")}")
    console.print("${string.ends_with(s, "ef")}")
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["bcd", "ef", "", "2", "-1", "true"]
        );
    }

    #[test]
    fn substring_is_char_based_not_byte_based() {
        // A multi-byte char must count as one position, not its byte length.
        let src = r#"
fn main(console: Console):
    let s = "héllo"
    console.print(string.substring(s, 0, 2))
"#;
        assert_eq!(run(src).unwrap(), vec!["hé"]);
    }

    #[test]
    fn string_split_contains_replace() {
        let src = r#"
fn main(console: Console):
    let parts = string.split("a,b,c", ",")
    console.print("${list.length(parts)}")
    console.print(list.at(parts, 1))
    console.print(string.replace("a,b,c", ",", "-"))
    console.print("${string.contains("hello", "ell")}")
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "b", "a-b-c", "true"]);
    }

    #[test]
    fn push_is_immutable_and_concat_joins() {
        let src = r#"
fn main(console: Console):
    let a = [1, 2]
    let b = list.__push(a, 3)
    console.print("${list.length(a)}")
    console.print("${list.length(b)}")
    let c = list.concat(a, [9, 9])
    console.print("${list.at(c, 3)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["2", "3", "9"]);
    }

    #[test]
    fn closure_captures_environment() {
        let src = r#"
fn adder(n: Int) -> fn(Int) -> Int:
    fn(x: Int): (x + n)

fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let inc = adder(1)
    let plus100 = adder(100)
    console.print("${apply(inc, 5)}")
    console.print("${apply(plus100, 5)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["6", "105"]);
    }

    #[test]
    fn try_inside_lambda_returns_from_lambda() {
        // `?` inside a lambda short-circuits the lambda, not the outer function.
        let src = r#"
type Option:
    Some(a)
    None
fn run(f: fn(Option(Int)) -> Option(Int), o: Option(Int)) -> Option(Int):
    f(o)
fn render(o: Option(Int)) -> String:
    match o:
        Some(n) -> "${n}"
        None -> "none"
fn main(console: Console):
    let g = fn(o: Option(Int)):
        let n = o?
        Some(n + 1)
    console.print(render(run(g, Some(7))))
    console.print(render(run(g, None)))
"#;
        assert_eq!(run(src).unwrap(), vec!["8", "none"]);
    }

    #[test]
    fn record_update_does_not_mutate_the_original() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(1, 2)
    let q = Point(x: 10, y: ((p).y + 1), ..p)
    console.print(("${(p).x}" + "${(p).y}"))
    console.print(("${(q).x}" + "${(q).y}"))
"#;
        assert_eq!(run(src).unwrap(), vec!["12", "103"]);
    }

    #[test]
    fn record_field_access_runs() {
        let src = r#"
type Person:
    name: String
    age: Int

fn main(console: Console):
    let p = Person("witchy", 7)
    console.print((((p).name + " is ") + "${(p).age}"))
"#;
        assert_eq!(run(src).unwrap(), vec!["witchy is 7"]);
    }

    #[test]
    fn list_pattern_head_tail() {
        let src = r#"
fn len(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [_, ..tail] -> (1 + len(tail))

fn main(console: Console):
    console.print("${len([5, 6, 7, 8])}")
"#;
        assert_eq!(run(src).unwrap(), vec!["4"]);
    }

    #[test]
    fn for_in_accumulates() {
        let src = r#"
fn main(console: Console):
    var total = 0
    for n in [10, 20, 30]:
        total = (total + n)
    console.print("${total}")
"#;
        assert_eq!(run(src).unwrap(), vec!["60"]);
    }

    #[test]
    fn try_option_short_circuits() {
        // `?` on `None` returns `None` from `first_word`; on `Some` it unwraps.
        let src = r#"
type Option:
    Some(a)
    None

fn head(o: Option(Int)) -> Option(Int):
    let n = (o)?
    Some((n + 100))

fn render(o: Option(Int)) -> String:
    match o:
        Some(n) -> "${n}"
        None -> "none"

fn main(console: Console):
    console.print(render(head(Some(1))))
    console.print(render(head(None)))
"#;
        assert_eq!(run(src).unwrap(), vec!["101", "none"]);
    }

    #[test]
    fn conversions() {
        let src = r#"
fn main(console: Console):
    console.print("${math.to_float(7)}")
    console.print("${math.to_int(3.9)}")
    console.print("${string.to_int("42")}")
"#;
        assert_eq!(run(src).unwrap(), vec!["7.0", "3", "42"]);
    }

    #[test]
    fn string_stdlib() {
        let src = r#"
fn main(console: Console):
    console.print(string.to_upper("witchy"))
    console.print("${string.length("hello")}")
    console.print(string.trim("  hi  "))
    if string.starts_with("witchy", "wit"):
        console.print("yes")
    else:
        console.print("no")
"#;
        assert_eq!(run(src).unwrap(), vec!["WITCHY", "5", "hi", "yes"]);
    }

    #[test]
    fn while_loop_and_modulo() {
        let src = r#"
fn main(console: Console):
    var i = 1
    var total = 0
    while (i <= 5):
        total = (total + i)
        i = (i + 1)
    console.print("${total}")
    console.print("${(10 % 3)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["15", "1"]);
    }

    #[test]
    fn boolean_not_and_short_circuit() {
        let src = r#"
fn is_zero(n: Int) -> Bool:
    (n == 0)

fn main(console: Console):
    if (!is_zero(5)):
        console.print("nonzero")
    else:
        console.print("zero")
"#;
        assert_eq!(run(src).unwrap(), vec!["nonzero"]);
    }

    #[test]
    fn lists_length_and_index() {
        let src = r#"
fn main(console: Console):
    let xs = [10, 20, 30]
    console.print("${list.length(xs)}")
    console.print("${list.at(xs, 1)}")
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "20"]);
    }

    #[test]
    fn let_bindings_are_immutable() {
        let src = r#"
fn main(console: Console):
    let x = 1
    x = 2
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("immutable"), "got: {}", e.message);
    }

    #[test]
    fn var_bindings_are_mutable() {
        let src = r#"
fn main(console: Console):
    var x = 1
    x = (x + 41)
    console.print("${x}")
"#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    /// Hylo-style mutable value semantics: an `var` parameter mutates the
    /// caller's variable in place — easy mutability, no pointers.
    #[test]
    fn var_parameter_writes_back_to_caller() {
        let src = r#"
fn bump(var n: Int):
    n = (n + 1)

fn main(console: Console):
    var x = 41
    bump(x)
    console.print("${x}")
"#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    #[test]
    fn var_requires_a_mutable_variable() {
        let src = r#"
fn bump(var n: Int):
    n = (n + 1)

fn main(console: Console):
    let x = 41
    bump(x)
"#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("var") || e.message.contains("immutable"),
            "got: {}",
            e.message
        );
    }

    // --- Drift guard for the `eval_call` builtin fast path -------------------
    //
    // `eval_call` (and the tail-call site) skip both `call_builtin` and
    // `call_interpreter_special` whenever `is_interpreter_builtin(name)` is
    // false — the win is not re-scanning the intrinsic table on every plain
    // user-function call. Correctness requires the predicate to be a SUPERSET
    // of the names those two functions can dispatch: if a builtin name is not
    // covered, the fast path skips it and the call falls through to "unknown
    // function". This test scans the two functions' source for their
    // dispatch-position string literals and asserts every one is covered.
    //
    // Coverage of the intrinsic table and cap-ops is by construction (the
    // predicate calls `intrinsics::lookup` / `cap_ops::is_op_name`). The only
    // drift-prone arms are hand-written string literals — which this catches.
    // NOTE: a `name == intrinsics::SOME_CONST` arm is covered only if that
    // const is a real cataloged intrinsic; adding a const arm for a name that
    // is NOT in the table would need a matching `matches!` entry (and this
    // test can't see the const's value to check it).

    fn scan_fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing `{sig}` in interpreter.rs"));
        let tail = &src[start + sig.len()..];
        let end = tail.find("\n    fn ").unwrap_or(tail.len());
        &tail[..end]
    }

    fn is_name_shaped(s: &str) -> bool {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if c.is_ascii_lowercase() || c == '_' => {}
            _ => return false,
        }
        s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
    }

    /// Name-shaped string literals in dispatch position: `name == "x"`, an
    /// `"x" =>` match arm, or an `"x" |` / `| "x"` arm alternative.
    fn dispatch_literals(body: &str) -> Vec<String> {
        let bytes = body.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'"' {
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                // A backslash escape means this is not a bare name literal.
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            let lit = &body[i + 1..j];
            if is_name_shaped(lit) {
                let before = body[..i].trim_end();
                let after = body[j + 1..].trim_start();
                let dispatch = before.ends_with("==")
                    || before.ends_with('|')
                    || after.starts_with("=>")
                    || after.starts_with('|');
                if dispatch {
                    out.push(lit.to_string());
                }
            }
            i = j + 1;
        }
        out
    }

    #[test]
    fn interpreter_builtin_names_are_covered() {
        let src = include_str!("interpreter.rs");
        let mut names: Vec<String> = ["    fn call_builtin(", "    fn call_interpreter_special("]
            .into_iter()
            .flat_map(|sig| dispatch_literals(scan_fn_body(src, sig)))
            .collect();
        names.sort();
        names.dedup();
        // Sanity: the scan actually found the known dispatch arms (guards against
        // a refactor that moves the functions or changes their shape so the scan
        // silently matches nothing and the guard becomes vacuous).
        assert!(
            names.contains(&"print".to_string()) && names.contains(&"fail".to_string()),
            "dispatch-literal scan found nothing recognizable ({} names) — the \
             scan is stale, fix scan_fn_body/dispatch_literals",
            names.len()
        );
        let uncovered: Vec<&String> = names
            .iter()
            .filter(|name| !is_interpreter_builtin(name))
            .collect();
        assert!(
            uncovered.is_empty(),
            "these builtin dispatch names are not covered by `is_interpreter_builtin` — the \
             eval_call fast path would skip them and report `unknown function`: {uncovered:?}. \
             Add each to the `matches!` list in `is_interpreter_builtin` (or, if it is a real \
             intrinsic/cap-op, ensure it is in the intrinsic table / cap_ops::OPS).",
        );
    }
