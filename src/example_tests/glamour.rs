use super::*;
use crate::{codegen, interpreter, parser, typeck};

    /// RFC-0006: a `tag"…${e}…"` tagged literal expands at COMPILE TIME — the tag
    /// (`twice`, a self-contained `fn(parts, holes) -> String` returning witchy
    /// EXPRESSION SOURCE) runs once in the compiler and its result is parsed and
    /// SPLICED over the literal. The literal is gone before either backend sees
    /// the program, so the interpreter and the compiled-WASM backend must produce
    /// IDENTICAL output. `twice"a${v}b"` expands to source `"a" + v + v + "b"`,
    /// which at the call site (`v = "X"`) evaluates to `"aXXb"`.
    #[test]
    fn tagged_literal_expands_identically_on_both_backends() {
        let src = "import list\n\
                   \n\
                   import meta\n\
                   \n\
                   comptime fn twice(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
                   \x20   let a = list.at(parts, 0)\n\
                   \x20   let b = list.at(parts, 1)\n\
                   \x20   let h = list.at(holes, 0)\n\
                   \x20   meta.expr_raw(\"\\\"\" + a + \"\\\" + \" + h + \" + \" + h + \" + \\\"\" + b + \"\\\"\")\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let v = \"X\"\n\
                   \x20   console.print(twice\"a${v}b\")\n";
        // Interpreter and compiled WASM agree, and both yield the spliced value.
        let interp = link_run(src);
        let wasm = wasm_run(src);
        assert_eq!(interp, vec!["aXXb".to_string()], "interpreter output");
        assert_eq!(wasm, interp, "compiled WASM must match the interpreter");
    }

    /// RFC-0107 Phase 2: a three-parameter tag receives compiler-owned source
    /// metadata. The tag can use it for diagnostics while deriving semantic IDs
    /// from `parts`; the legacy two-parameter ABI remains valid above.
    #[test]
    fn tagged_literal_origin_metadata_has_backend_parity() {
        let src = "import meta\n\
                   \n\
                   comptime fn located(parts: List(String), holes: List(String), origin: String) -> meta.ExprSyntax:\n\
                   \x20   meta.expr_raw(\"\\\"\" + origin + \"\\\"\")\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   console.print(located\"here\")\n";
        let interp = link_run(src);
        let wasm = wasm_run(src);
        assert_eq!(wasm, interp, "compiled origin metadata must match the interpreter");
        assert_eq!(interp.len(), 1);
        assert!(interp[0].starts_with("main:"), "origin should name the invocation module: {interp:?}");
    }

    /// RFC-0107 Phase 2: the checked linker retains a structured inventory for
    /// library-defined tags. Tooling does not have to recover template origins
    /// from generated source strings, and hole locations remain call-site spans.
    #[test]
    fn glamour_tags_retain_checked_definition_and_invocation_origins() {
        let src = "import glamour\n\
                   from glamour import VNode\n\
                   \n\
                   fn views(name: String) -> (VNode(Int), VNode(Int)):\n\
                   \x20   let first = html\"<p>${name}</p>\"\n\
                   \x20   let second = jsx\"<p>${name}</p>\"\n\
                   \x20   (first, second)\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let linked = crate::pipeline::link_with_origins(
            vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
            "main",
        )
        .expect("link tagged templates with origins");
        let tagged = linked
            .origins()
            .tagged_literals()
            .iter()
            .filter(|origin| matches!(origin.tag.as_str(), "glamour.html" | "glamour.jsx"))
            .collect::<Vec<_>>();

        assert_eq!(tagged.len(), 2);
        assert_eq!(tagged[0].tag, "glamour.html");
        assert_eq!(tagged[1].tag, "glamour.jsx");
        for origin in tagged {
            assert_eq!(origin.id.module, "main");
            assert_eq!(origin.definition.module, "glamour");
            assert_eq!(origin.invocation.module, "main");
            assert!(origin.definition.start.line > 0);
            assert!(origin.invocation.start.line > 0);
            assert_eq!(origin.holes.len(), 1);
            assert_eq!(origin.holes[0].module, "main");
            assert!(origin.holes[0].start.column > 0);
        }
    }

    #[test]
    fn checked_glamour_template_inventory_deduplicates_html_and_jsx_plans() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "witchy-glamour-template-metadata-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create template metadata fixture");
        let source = root.join("main.witchy");
        std::fs::write(
            &source,
            "import glamour\n\
             from glamour import VNode\n\
             \n\
             fn views(name: String) -> (VNode(Int), VNode(Int)):\n\
             \x20 let first = html\"<p>${name}</p>\"\n\
             \x20 let second = jsx\"<p>${name}</p>\"\n\
             \x20 (first, second)\n\
             \n\
             type Message:\n\
             \x20 Pressed\n\
             \n\
             fn rich(label: String) -> VNode(Message):\n\
             \x20 jsx\"<button class=\\\"card\\\" aria-label=${label} on:click=${Pressed}>${label}</button>\"\n\
             \n\
             fn main() -> Int:\n\
             \x20 0\n",
        )
        .expect("write template metadata fixture");
        let (checked, _) =
            crate::link_file_checked(source.to_str().expect("UTF-8 fixture path"))
                .expect("authenticated checked template fixture");
        let plans = witchy_lower::codegen::checked_glamour_templates(&checked)
            .expect("compiler template inventory");

        assert_eq!(plans.len(), 2, "identical html/jsx skeletons share one plan");
        let simple = plans
            .iter()
            .find(|plan| plan.slots.len() == 1)
            .expect("simple text plan");
        assert!(simple.identity.starts_with("glamour-tp1-"));
        assert_eq!(
            simple.slots,
            [witchy_lower::codegen::GlamourTemplateSlotMetadata {
                index: 0,
                wire_id: 1,
                node: 2,
                kind: "child".into(),
                name: String::new(),
            }]
        );
        assert_ne!(simple.wire_id, 0);
        assert_eq!(
            simple.root,
            witchy_lower::codegen::GlamourTemplateNodeMetadata::Element {
                node: 1,
                tag: "p".into(),
                attributes: Vec::new(),
                children: vec![witchy_lower::codegen::GlamourTemplateNodeMetadata::Text {
                    node: 2,
                    text: String::new(),
                }],
            }
        );
        assert_eq!(simple.origins.len(), 2);
        assert_eq!(simple.origins[0].tag.name(), "html");
        assert_eq!(simple.origins[1].tag.name(), "jsx");
        assert!(simple
            .origins
            .iter()
            .all(|origin| origin.tag.package().name() == "witchy/glamour"));
        let rich = plans
            .iter()
            .find(|plan| plan.slots.len() == 3)
            .expect("rich attribute/event/text plan");
        assert_eq!(
            rich.slots
                .iter()
                .map(|slot| (slot.wire_id, slot.node, slot.kind.as_str(), slot.name.as_str()))
                .collect::<Vec<_>>(),
            [
                (1, 1, "aria", "aria-label"),
                (2, 1, "event", "click"),
                (3, 2, "child", ""),
            ]
        );
        let witchy_lower::codegen::GlamourTemplateNodeMetadata::Element {
            attributes,
            ..
        } = &rich.root
        else {
            panic!("rich template root must be an element");
        };
        assert_eq!(
            attributes,
            &[witchy_lower::codegen::GlamourTemplateAttributeMetadata {
                name: "class".into(),
                value: "card".into(),
            }]
        );
        std::fs::remove_dir_all(root).expect("remove template metadata fixture");
    }

    #[test]
    fn target_available_entrypoint_has_backend_parity() {
        let src = "@browser\n\
                   fn browser_value() -> String:\n\
                   \x20   \"browser\"\n\
                   \n\
                   @browser\n\
                   fn main(console: Console):\n\
                   \x20   console.print(browser_value())\n";
        let interp = link_run(src);
        let wasm = wasm_run(src);
        assert_eq!(interp, vec!["browser".to_string()]);
        assert_eq!(wasm, interp, "target metadata must not change backend semantics");
    }

    /// Link a program that `import glamour` against the embedded glamour source
    /// (and, transitively, the bundled std), then run it on BOTH backends and
    /// assert they agree — the parity oracle for the framework rune. Returns the
    /// agreed output. The `html` tag is a COMPILE-TIME literal (RFC-0006): it is
    /// expanded by the linker before either backend sees the program, so this is a
    /// genuine differential test of the *expanded* `VNode`-constructing AST.
    fn glamour_run_both(src: &str) -> Vec<String> {
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let modules = vec![("main".to_string(), entry), ("glamour".to_string(), glamour)];
        let linked = crate::pipeline::link(modules, "main").expect("link glamour consumer");
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp run");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm = crate::run_wasm_bytes(&bytes).expect("wasm run");
        assert_eq!(wasm, interp, "compiled WASM must match the interpreter");
        interp
    }

    #[test]
    fn glamour_completion_codecs_have_backend_parity() {
        let src = "import bytes\n\
                   import glamour\n\
                   from glamour import HttpResult, NavigationResult, PortResult\n\
                   \n\
                   fn payload(values: List(Int)) -> Bytes:\n\
                   \x20   match bytes.from_list(values):\n\
                   \x20       Ok(value) -> value\n\
                   \x20       Err(_) -> bytes.from_string(\"\")\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let http = payload([1, 0, 0, 0, 204, 0, 0, 0, 4, 0, 0, 0, 100, 111, 110, 101])\n\
                   \x20   match glamour.island_http_completion(http, 0):\n\
                   \x20       HttpResponse(status, body) -> console.print(\"http:${status}:${body}\")\n\
                   \x20       _ -> console.print(\"wrong-http\")\n\
                   \x20   let navigation = payload([2, 0, 0, 0, 6, 0, 0, 0, 100, 101, 110, 105, 101, 100])\n\
                   \x20   match glamour.island_navigation_completion(navigation, 1):\n\
                   \x20       NavigationFailure(problem) -> console.print(\"navigation:${problem}\")\n\
                   \x20       _ -> console.print(\"wrong-navigation\")\n\
                   \x20   let port = payload([1, 0, 0, 0, 2, 0, 0, 0, 111, 107])\n\
                   \x20   match glamour.island_port_completion(port, 0):\n\
                   \x20       PortResponse(value) -> console.print(\"port:${value}\")\n\
                   \x20       _ -> console.print(\"wrong-port\")\n";
        let output = glamour_run_both(src);
        assert_eq!(
            output,
            ["http:204:done", "navigation:denied", "port:ok"],
        );
    }

    /// RFC-0107: `jsx"..."` is a library-defined compile-time tagged literal,
    /// not a Witchy parser mode. It delegates to Glamour's checked `html` tag,
    /// so both spellings expand to the same typed VNode AST on both backends.
    #[test]
    fn glamour_jsx_tag_is_a_parity_checked_html_alias() {
        let src = "import glamour\n\
                   from glamour import VNode\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let a: VNode(Int) = html\"<p>same</p>\"\n\
                   \x20   let b: VNode(Int) = jsx\"<p>same</p>\"\n\
                   \x20   console.print(glamour.to_html(a))\n\
                   \x20   console.print(glamour.to_html(b))\n\
                   \x20   match glamour.template_id(a):\n\
                   \x20       Some(aid) ->\n\
                   \x20           match glamour.template_id(b):\n\
                   \x20               Some(bid) -> console.print(if aid == bid: \"stable-plan\" else: \"different-plan\")\n\
                   \x20               None -> console.print(\"missing-jsx-plan\")\n\
                   \x20       None -> console.print(\"missing-html-plan\")\n";
        let out = glamour_run_both(src);
        assert_eq!(
            out,
            vec![
                "<p>same</p>".to_string(),
                "<p>same</p>".to_string(),
                "stable-plan".to_string(),
            ],
        );
    }

    /// Like [`glamour_run_both`] but also links `std/markdown` — for the docs renderer.
    fn markdown_run_both(src: &str) -> Vec<String> {
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let markdown = markdown_module_cached();
        let modules = vec![
            ("main".to_string(), entry),
            ("glamour".to_string(), glamour),
            ("markdown".to_string(), markdown),
        ];
        let linked = crate::pipeline::link(modules, "main").expect("link markdown consumer");
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp run");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm = crate::run_wasm_bytes(&bytes).expect("wasm run");
        assert_eq!(wasm, interp, "compiled WASM must match the interpreter");
        interp
    }

    /// (RFC-0041) `markdown.to_vnode` preserves a fenced block's INFO STRING as a
    /// `language-<lang>` class on the `<code>` — the hook a host uses to find runnable
    /// `witchy` blocks and to highlight by language — while the code stays inert, escaped
    /// text (never an HTML sink). Identical on both backends. A bare ``` fence gets no class.
    #[test]
    fn markdown_code_fence_carries_its_language_class_on_both_backends() {
        let src = "import glamour\n\
                   import markdown\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   console.print(glamour.to_html(markdown.to_vnode(\"```witchy\\nfn f():\\n    pass\\n```\")))\n\
                   \x20   console.print(glamour.to_html(markdown.to_vnode(\"```\\nplain\\n```\")))\n";
        let out = markdown_run_both(src);
        assert!(
            out[0].contains("<code class=\"language-witchy\">"),
            "a ```witchy fence tags the code with its language:\n{}",
            out[0]
        );
        assert!(out[0].contains("fn f():") && !out[0].contains("<script"), "the code is inert escaped text");
        assert!(
            out[1].contains("<code>plain</code>") && !out[1].contains("language-"),
            "a bare ``` fence carries no language class:\n{}",
            out[1]
        );
    }

    /// `markdown.to_vnode` renders `*emphasis*` as an `<em>` element of inert text
    /// nodes (never an HTML sink), and `**bold**` still wins over a lone `*` when
    /// their markers coincide, so `**b**` is one `<strong>` — not `<em>*b*</em>`.
    /// An unclosed `*` renders as a literal asterisk. Identical on both backends.
    #[test]
    fn markdown_emphasis_renders_em_and_yields_to_bold_on_both_backends() {
        let src = "import glamour\n\
                   import markdown\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   console.print(glamour.to_html(markdown.to_vnode(\"an *italic* word\")))\n\
                   \x20   console.print(glamour.to_html(markdown.to_vnode(\"a **bold** word\")))\n\
                   \x20   console.print(glamour.to_html(markdown.to_vnode(\"one *star\")))\n";
        let out = markdown_run_both(src);
        assert!(
            out[0].contains("<em>italic</em>") && !out[0].contains('*'),
            "a *span* becomes <em> with no literal asterisks:\n{}",
            out[0]
        );
        assert!(
            out[1].contains("<strong>bold</strong>") && !out[1].contains("<em>"),
            "** wins over a lone * so bold stays <strong>:\n{}",
            out[1]
        );
        assert!(
            out[2].contains("one *star"),
            "an unclosed * renders as a literal asterisk:\n{}",
            out[2]
        );
    }

    /// (RFC-0041) A host `Slot` is pure data on the wire — `{"slot": kind, "data": payload}`
    /// (the DOM host mounts the widget) with an inert escaped-code fallback for `to_html`.
    /// Both serializations are IDENTICAL on the interpreter and compiled WASM.
    #[test]
    fn glamour_slot_wire_is_identical_on_both_backends() {
        let src = "import glamour\n\
                   from glamour import VNode\n\
                   import json\n\
                   from json import Json\n\
                   \n\
                   fn mj(m: Int) -> Json:\n\
                   \x20   JsonInt(0)\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let v: VNode(Int) = glamour.slot(\"witchy-runnable\", \"fn main():\\n    pass\")\n\
                   \x20   console.print(glamour.to_json(v, mj))\n\
                   \x20   console.print(glamour.to_html(v))\n";
        let out = glamour_run_both(src);
        assert_eq!(
            out,
            vec![
                "{\"slot\":\"witchy-runnable\",\"data\":\"fn main():\\n    pass\"}".to_string(),
                "<pre class=\"glamour-slot\" data-glamour-slot-kind=\"witchy-runnable\"><code>fn main():\n    pass</code></pre>".to_string(),
            ],
            "the slot wire + the inert code fallback"
        );
    }

    /// (RFC-0039) The secret effect's WIRE FORMAT is pure data, so it serializes IDENTICALLY
    /// on both backends. A `SecretField` VNode and a `SubmitSecret` Cmd carry ONLY their
    /// host-slot coordinates and port name — read out of the sealed `SecretInput`/`SecretRef`/
    /// `CredentialPort` tokens (minted here from a granted `UiRoot`), never a value — and the
    /// interpreter and compiled WASM agree byte-for-byte. This is the parity half of the
    /// host-custody guarantee: the description the rune emits is inert, identical data.
    #[test]
    fn glamour_secret_wire_is_identical_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import glamour\n\
                   from glamour import UiRoot, VNode\n\
                   import json\n\
                   from json import Json\n\
                   \n\
                   type Msg:\n\
                   \x20   Done(String)\n\
                   \n\
                   fn mj(m: Msg) -> Json:\n\
                   \x20   JsonString(\"\")\n\
                   \n\
                   fn main(console: Console, ui: UiRoot):\n\
                   \x20   let input = glamour.secret_field(ui, \"login\", \"password\")\n\
                   \x20   let cred = glamour.credential_port(ui, \"passkeyLogin\")\n\
                   \x20   let cmd = glamour.submit_secret_compat(glamour.secret_ref(input), cred, \"Done\")\n\
                   \x20   let node: VNode(Msg) = glamour.secret_input(input, \"PwStatus\")\n\
                   \x20   console.print(glamour.to_json(node, mj))\n\
                   \x20   console.print(json.encode(glamour.cmd_to_json(cmd, mj)))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let modules = vec![("main".to_string(), entry), ("glamour".to_string(), glamour)];
        let linked = crate::pipeline::link(modules, "main").expect("link glamour consumer");
        typeck::check(&linked).expect("typecheck");

        // Interpreter: grant a single-field `UiRoot` keyed by the param name `ui`.
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("policy".to_string(), "login".to_string());
        let mut grants = std::collections::BTreeMap::new();
        grants.insert("ui".to_string(), fields);
        let interp = interpreter::run_module_user_caps(linked.clone(), ".", vec![], vec![], vec![], grants)
            .expect("interp");

        // Compiled: stage the one field host-side (declaration order).
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    quiet: true,
                    user_cap_fields: vec![vec!["login".to_string()]],
                    ..Default::default()
                },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), interp, "compiled WASM must match the interpreter");

        // And the shape is exactly the host-shell protocol: slot + port, no value.
        assert_eq!(
            interp,
            vec![
                "{\"secret\":{\"form\":\"login\",\"field\":\"password\"},\"on_ready\":\"PwStatus\"}".to_string(),
                "{\"cmd\":\"submit_secret\",\"slot\":\"login/password\",\"port\":\"passkeyLogin\",\"tag\":\"Done\"}".to_string(),
            ],
            "the secret wire carries only slot + port names (from tokens), never a value"
        );
    }

    /// RFC-0008: glamour's `html` tag (RFC-0006 compile-time literal) builds a
    /// `VNode(msg)` tree, and the serializer renders it IDENTICALLY on both
    /// backends. The headline property is structural XSS-immunity: a text-position
    /// hole carrying `<script>x</script>` becomes a `Text` NODE, never markup, so
    /// the serializer escapes it to `&lt;script&gt;…` — proven observable here.
    #[test]
    fn glamour_html_tag_renders_and_is_xss_immune_on_both_backends() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let cls = glamour.classes([\"card\"])\n\
                   \x20   let title = \"Witchy\"\n\
                   \x20   let body = \"<script>x</script>\"\n\
                   \x20   let view = html\"<div class=${cls}><h2>${title}</h2><span class=\\\"cap\\\">${body}</span></div>\"\n\
                   \x20   console.print(glamour.to_html(view))\n";
        let out = glamour_run_both(src);
        assert_eq!(
            out,
            vec![
                "<div class=\"card\"><h2>Witchy</h2><span class=\"cap\">\
                 &lt;script&gt;x&lt;/script&gt;</span></div>"
                    .to_string()
            ],
            "static class -> prop, text holes -> text nodes, and the <script> \
             payload renders ESCAPED — XSS-immune by construction"
        );
        // The escaped payload is present; the raw executable form is NOT.
        let rendered = &out[0];
        assert!(rendered.contains("&lt;script&gt;"), "the payload must be escaped");
        assert!(
            !rendered.contains("<script>"),
            "no raw <script> may reach the output — that would be an injection"
        );
    }

    /// RFC-0008: events are DATA. `on:click=${Inc}` in attribute position lowers
    /// to `on("click", Inc)` carrying a `msg` VALUE (not a closure), and the same
    /// expanded AST runs identically on both backends.
    #[test]
    fn glamour_event_binding_is_a_msg_value_on_both_backends() {
        let src = "import glamour\n\
                   \n\
                   type Msg:\n\
                   \x20   Inc\n\
                   \x20   Dec\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let view = html\"<button on:click=${Inc}>+</button>\"\n\
                   \x20   console.print(glamour.to_html(view))\n";
        let out = glamour_run_both(src);
        assert_eq!(out, vec!["<button>+</button>".to_string()]);
    }

    /// RFC-0008: glamour's `to_json` serializes a VNode tree to the wire format the
    /// JS DOM host shell (`web/witchy-runtime/glamour-dom.mjs`) consumes —
    /// `{"el":tag,"attrs":[["prop",k,v]|["on",evt,<msg-json>]],"kids":[...]}` /
    /// `{"text":"..."}`. The `On` binding embeds the msg via a caller-supplied
    /// `msg_to_json` (here `json.from_value`), so an event handler round-trips as its
    /// message value. The serialized string must be IDENTICAL on both backends.
    #[test]
    fn glamour_detached_commands_map_with_backend_parity() {
        let src = r#"import glamour
from glamour import Cmd
import json
from json import Json

type Child:
    Tick

type Parent:
    Wrapped(Child)

fn message_json(_message: Parent) -> Json:
    JsonString("done")

fn main(console: Console):
    let command: Cmd(Child) = glamour.detach(NoCmd)
    let mapped: Cmd(Parent) = command.map(fn(message: Child): Wrapped(message))
    console.print(json.encode(glamour.cmd_to_json(mapped, message_json)))
"#;
        assert_eq!(glamour_run_both(src), vec![r#"{"cmd":"none"}"#.to_string()]);
    }

    #[test]
    fn glamour_to_json_serializes_the_wire_format_on_both_backends() {
        let src = "import glamour\n\
                   import json\n\
                   from json import Json\n\
                   import reflect\n\
                   \n\
                   type Msg derive(Reflect):\n\
                   \x20   Inc\n\
                   \x20   Dec\n\
                   \n\
                   fn msg_to_json(m: Msg) -> Json:\n\
                   \x20   json.from_value(m)\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let view = glamour.element(\"div\", [glamour.prop(\"class\", \"c\")], [\n\
                   \x20       glamour.element(\"button\", [glamour.on(\"click\", Inc)], [glamour.text(\"+\")]),\n\
                   \x20       glamour.text(\"hi\"),\n\
                   \x20   ])\n\
                   \x20   console.print(glamour.to_json(view, msg_to_json))\n";
        let out = glamour_run_both(src);
        assert_eq!(
            out,
            vec![
                "{\"el\":\"div\",\"attrs\":[[\"prop\",\"class\",\"c\"]],\"kids\":\
                 [{\"el\":\"button\",\"attrs\":[[\"on\",\"click\",\
                 {\"$variant\":\"Inc\",\"$values\":[]}]],\"kids\":[{\"text\":\"+\"}]},\
                 {\"text\":\"hi\"}]}"
                    .to_string()
            ],
            "to_json must emit the documented wire shape: el/attrs/kids, prop/on \
             attrs, and the On msg embedded as its reflected JSON"
        );
    }

    /// RFC-0008 §1 / RFC-0007: a `pub fn export_*(String) -> String` compiles to a
    /// JS-callable export. The module must export the `__galloc` allocator and the
    /// `__export_<name>` wrapper (so the host can write the input String header and
    /// call the function), keep the existing `run`/`memory` exports intact, and add
    /// NO import (the call path grants no authority). This is the codegen contract
    /// the JS `callString` (and the spike's round-trip) depend on.
    #[test]
    fn string_export_emits_galloc_and_wrapper_with_no_extra_import() {
        let src = "pub fn export_echo(s: String) -> String:\n\
                   \x20   \"echo: \" + s\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   console.print(export_echo(\"hi\"))\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");

        let mut exports: Vec<String> = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::ExportSection(s) = payload.expect("parse") {
                for e in s {
                    exports.push(e.expect("export").name.to_string());
                }
            }
        }
        for want in ["memory", "run", "__galloc", "__export_export_echo"] {
            assert!(
                exports.contains(&want.to_string()),
                "module must export `{want}`; got {exports:?}"
            );
        }
        // The module validates (the synthesized wrappers are well-formed wasm). The
        // spike (`tests/browser_shim.rs`) proves it round-trips through the JS shim
        // and that the wrappers add NO host import (the rune stays instantiable
        // under the deny-all pure-compute host).
        assert!(
            validates_wasm_gc(&bytes),
            "a module with string-export wrappers must validate"
        );
    }

    /// (RFC-0040) A `pub fn export_*(cap: <grantable>, String) -> String` is a
    /// browser app root: the leading bare grantable cap is host-minted per call, so
    /// the module compiles, validates, and exports its `__export_*` wrapper (which
    /// mints the cap via `mk{N}(build_user_cap_field…)`, mirroring the `run` wrapper).
    #[test]
    fn cap_gated_string_export_compiles_and_validates() {
        let src = "grantable capability UiRoot:\n    policy: String\n\npub fn export_step(ui: UiRoot, input: String) -> String:\n    match ui:\n        UiRoot(p) -> p + \":\" + input\n\nfn main(console: Console):\n    console.print(\"ok\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut exports: Vec<String> = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::ExportSection(s) = payload.expect("parse") {
                for e in s {
                    exports.push(e.expect("export").name.to_string());
                }
            }
        }
        assert!(
            exports.contains(&"__export_export_step".to_string()),
            "the cap-gated export's wrapper must be exported; got {exports:?}"
        );
        assert!(
            validates_wasm_gc(&bytes),
            "a cap-gated export module must validate (the minting wrapper is well-formed)"
        );
    }

    /// RFC-0008 acceptance criterion: the glamour rune has an EMPTY runtime
    /// footprint — no Net, no Dir, no Clock, nothing. coven's own analyzer
    /// (`capabilities::analyze`, the engine behind `witchy caps`) proves it from
    /// source. This is the headline: a UI framework whose authority is provably
    /// nil. The `witchy.toml` declares the same (`runtime = []`).
    #[test]
    fn glamour_rune_has_an_empty_capability_footprint() {
        let fp = crate::capabilities::analyze(
            &glamour_module_cached(),
        );
        // `show_caps` renders the empty set as the literal `(none)`.
        assert_eq!(
            crate::capabilities::show_caps(&fp.total),
            "(none)",
            "glamour must demand NO capability — an empty footprint is RFC-0008's headline"
        );
        assert!(fp.total.is_empty(), "the footprint map itself must be empty");
        // And the manifest agrees: `runtime = []`.
        let toml = include_str!("../../projects/glamour/witchy.toml");
        assert!(
            toml.contains("runtime = []"),
            "witchy.toml must declare an empty runtime footprint"
        );
    }

    /// RFC-0008: a hole in a FORBIDDEN position is a COMPILE error, not a runtime
    /// surprise. A `${hole}` used as a tag NAME makes the `html` tag `fail` at
    /// comptime with a message naming the problem — so the program never links.
    #[test]
    fn glamour_html_rejects_a_hole_in_tag_name_position() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let t = \"div\"\n\
                   \x20   let view = html\"<${t}>hi</${t}>\"\n\
                   \x20   console.print(glamour.to_html(view))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let modules = vec![("main".to_string(), entry), ("glamour".to_string(), glamour)];
        let err = crate::pipeline::link(modules, "main")
            .expect_err("a tag-name hole must be a compile error");
        assert!(
            err.to_string().contains("a tag NAME may not be a"),
            "the compile error must name the forbidden position, got: {err}"
        );
    }

    /// RFC-0006 hole-precise diagnostics: a type-wrong hole (an `Int` in TEXT
    /// position, which the `html` tag wraps in `glamour.text(…)` expecting a
    /// `String`) must report a type error whose LINE points INTO the literal — the
    /// `${5}` lives on the literal's line, not on the tag-emitted constructor or
    /// the desugared call. The marker-substitution machinery stamps each spliced
    /// hole with its captured source position so the diagnostic lands here.
    #[test]
    fn glamour_html_wrong_typed_hole_points_into_the_literal() {
        // The `html"…"` literal (with the `${5}` text hole) is on line 4.
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let view = html\"<span>${5}</span>\"\n\
                   \x20   console.print(glamour.to_html(view))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let modules = vec![("main".to_string(), entry), ("glamour".to_string(), glamour)];
        let linked = crate::pipeline::link(modules, "main").expect("link (expansion succeeds)");
        let err = typeck::check(&linked)
            .expect_err("an Int in text position must be a type error (text holes need String)");
        let msg = err.to_string();
        assert!(
            msg.contains("line 4"),
            "the type error must point INTO the literal (line 4, where the `${{5}}` \
             hole lives), got: {msg}"
        );
    }

    /// RFC-0107 Phase 2: template holes select typed sinks. URL positions
    /// require a kinded SafeUrl, class positions require token lists, boolean
    /// positions require Bool, and ARIA remains escaped text.
    #[test]
    fn glamour_template_typed_sinks_have_backend_parity() {
        let src = "import glamour\n\
                   from glamour import VNode\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let destination = glamour.navigation_url_or_root(\"/docs\")\n\
                   \x20   let view: VNode(Int) = html\"<a href=${destination} aria-label=${\"Docs\"} class=${glamour.classes([\"nav\", \"active\"])} hidden=${false}>Go</a>\"\n\
                   \x20   console.print(glamour.to_html(view))\n";
        let out = glamour_run_both(src);
        assert_eq!(
            out,
            vec!["<a href=\"/docs\" aria-label=\"Docs\" class=\"nav active\">Go</a>".to_string()],
        );
    }

    #[test]
    fn glamour_template_rejects_plain_strings_at_url_holes() {
        let src = "import glamour\n\
                   from glamour import VNode\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let destination = \"/docs\"\n\
                   \x20   let view: VNode(Int) = html\"<a href=${destination}>Go</a>\"\n\
                   \x20   console.print(glamour.to_html(view))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let linked = crate::pipeline::link(
            vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
            "main",
        )
        .expect("link");
        let err = typeck::check(&linked)
            .expect_err("a String must not reach a typed href sink")
            .to_string();
        assert!(
            err.contains("line 6") && err.contains("SafeUrl"),
            "URL sink diagnostics should point to the literal hole and name SafeUrl: {err}",
        );
    }

    #[test]
    fn glamour_template_rejects_unsafe_static_urls_during_expansion() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   console.print(glamour.to_html(html\"<a href=\\\"javascript:alert(1)\\\">bad</a>\"))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let err = crate::pipeline::link(
            vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
            "main",
        )
        .expect_err("unsafe static URL must fail at compile time")
        .to_string();
        assert!(err.contains("unsafe static URL") && err.contains("href"), "got: {err}");
    }

    #[test]
    fn glamour_css_literal_scopes_and_extracts_typed_classes_with_parity() {
        let src = "import glamour\n\
                   from glamour import CssSheet, VNode\n\
                   \n\
                   fn styles() -> CssSheet:\n\
                   \x20   css\".card, .active { color: rebeccapurple; }\"\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let sheet = styles()\n\
                   \x20   console.print(glamour.css_sheet_id(sheet))\n\
                   \x20   console.print(glamour.css_text(sheet))\n\
                   \x20   match glamour.css_class(sheet, \"card\"):\n\
                   \x20       Err(_) -> console.print(\"missing\")\n\
                   \x20       Ok(card) ->\n\
                   \x20           let node: VNode(Int) = glamour.element(\"section\", [glamour.css_scope(sheet), glamour.class_attribute(glamour.css_classes([card]))], [glamour.text(\"styled\")])\n\
                   \x20           console.print(glamour.to_html(node))\n";
        let out = glamour_run_both(src);
        assert_eq!(out.len(), 3, "sheet id, extracted CSS, and HTML");
        assert!(
            out[0].starts_with("glamour-css1-") && out[0].len() == "glamour-css1-".len() + 64,
            "stable CSS identity: {out:?}",
        );
        assert!(
            out[1].contains("[data-glamour-scope=\"") &&
                out[1].contains(".card") &&
                out[1].contains(".active"),
            "CSS selectors should be deterministically scoped: {out:?}",
        );
        assert!(
            out[2].contains("data-glamour-scope=") && out[2].contains("class=\"card\""),
            "typed class and scope attributes should reach static HTML: {out:?}",
        );
    }

    #[test]
    fn glamour_global_css_is_explicit_owned_and_unscoped_with_parity() {
        let src = "import glamour\n\
                   from glamour import CssSheet\n\
                   \n\
                   fn styles() -> CssSheet:\n\
                   \x20   global_css\"html, body { color: rebeccapurple; } .focus-ring { outline-color: red; }\"\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let sheet = styles()\n\
                   \x20   console.print(glamour.css_text(sheet))\n\
                   \x20   console.print(glamour.css_origin(sheet))\n";
        let out = glamour_run_both(src);
        assert_eq!(out.len(), 2, "global CSS text plus source origin: {out:?}");
        assert!(out[0].contains("html, body { color: rebeccapurple;}"), "{out:?}");
        assert!(out[0].contains(".focus-ring { outline-color: red;}"), "{out:?}");
        assert!(!out[0].contains("data-glamour-scope"), "{out:?}");
        assert!(!out[1].is_empty(), "global CSS keeps compiler source ownership: {out:?}");
    }

    #[test]
    fn glamour_css_literal_rejects_interpolation() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let color = \"red\"\n\
                   \x20   let sheet = css\".card { color: ${color}; }\"\n\
                   \x20   console.print(glamour.css_text(sheet))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = glamour_module_cached();
        let linked = crate::pipeline::link(
            vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
            "main",
        )
        .expect("link untyped CSS value");
        let err = typeck::check(&linked)
            .expect_err("plain strings must not enter typed CSS properties")
            .to_string();
        assert!(
            err.contains("expected `glamour.CssValue(glamour.CssColorKind)`")
                && err.contains("found `String`"),
            "got: {err}"
        );

        let direct = "import glamour\n\
                      \n\
                      fn main(console: Console):\n\
                      \x20   console.print(glamour.css_text(css\".card { background-image: url('/logo.svg'); }\"))\n";
        let entry = parser::parse_module(direct).expect("parse direct CSS URL");
        let glamour = glamour_module_cached();
        let err = crate::pipeline::link(
            vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
            "main",
        )
        .expect_err("direct CSS URLs must use typed assets")
        .to_string();
        assert!(err.contains("typed glamour.css_asset interpolation"), "got: {err}");
    }

    #[test]
    fn glamour_media_policy_css_and_loader_share_one_query_corpus() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../web/witchy-runtime/glamour-media-query-corpus.json"
        ))
        .expect("media-query corpus");
        for example in corpus.as_array().expect("media-query cases") {
            let query = example["query"].as_str().expect("query");
            let valid = example["valid"].as_bool().expect("valid flag");
            let escaped = query.replace('\\', "\\\\").replace('"', "\\\"");
            let source = format!(
                "import glamour\n\nfn main(console: Console):\n    let _condition = media\"{escaped}\"\n    let sheet = css\"@media {escaped} {{ .card {{ color: red; }} }}\"\n    let global = global_css\"@media {escaped} {{ body {{ color: red; }} }}\"\n    console.print(glamour.css_text(sheet) + glamour.css_text(global))\n"
            );
            if valid {
                let output = glamour_run_both(&source);
                assert!(
                    output[0].contains(&format!("@media {query}"))
                        && output[0].contains("[data-glamour-scope=")
                        && output[0].contains("body { color: red;}"),
                    "accepted media query did not survive checked scoped CSS: {query:?}"
                );
            } else {
                let entry = parser::parse_module(&source).expect("parse invalid media fixture");
                let glamour = glamour_module_cached();
                let error = crate::pipeline::link(
                    vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
                    "main",
                )
                .expect_err("rejected media corpus entry must fail compile-time expansion");
                assert!(
                    error.to_string().contains("glamour media")
                        || error.to_string().contains("glamour css"),
                    "unexpected media rejection for {query:?}: {error}"
                );
            }
        }
        for stylesheet in [
            "@supports (display: grid) { .card { color: red; } }",
            "@media print { @media (color) { .card { color: red; } } }",
        ] {
            let source = format!(
                "import glamour\n\nfn main(console: Console):\n    let sheet = css\"{stylesheet}\"\n    console.print(glamour.css_text(sheet))\n"
            );
            let entry = parser::parse_module(&source).expect("parse rejected at-rule fixture");
            let glamour = glamour_module_cached();
            let linked = crate::pipeline::link(
                vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
                "main",
            )
            .expect("link rejected at-rule fixture");
            typeck::check(&linked).expect("typecheck rejected at-rule fixture");
            let interpreter_error = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("non-media and nested at-rules must abort in the interpreter");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("rejected at-rule fixture lowers");
            let wasm_error = crate::run_wasm_bytes(&bytes)
                .expect_err("non-media and nested at-rules must abort in compiled Wasm");
            assert!(interpreter_error.message.contains("glamour css"));
            assert!(wasm_error.to_string().contains("glamour css"));
        }
    }

    #[test]
    fn glamour_css_literal_accepts_typed_asset_images_with_parity() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   match glamour.css_asset(glamour.asset_url_or_empty(\"/logo.svg\")):\n\
                   \x20       Err(_) -> console.print(\"invalid\")\n\
                   \x20       Ok(image) ->\n\
                   \x20           let sheet = css\".hero { background-image: ${image}; }\"\n\
                   \x20           console.print(glamour.css_text(sheet))\n";
        let out = glamour_run_both(src);
        assert_eq!(out.len(), 1, "typed CSS asset trace: {out:?}");
        assert!(
            out[0].contains("background-image: url(\"/logo.svg\")"),
            "typed CSS asset should remain inert and scoped: {out:?}"
        );
    }

    #[test]
    fn glamour_css_custom_properties_are_typed_and_have_backend_parity() {
        let src = "import glamour\n\
                   from glamour import VNode\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   match glamour.css_color_property(\"accent\"):\n\
                   \x20       Err(_) -> console.print(\"invalid-property\")\n\
                   \x20       Ok(accent) ->\n\
                   \x20           match glamour.css_color(\"#663399\"):\n\
                   \x20               Err(_) -> console.print(\"invalid-color\")\n\
                   \x20               Ok(color) ->\n\
                   \x20                   let sheet = css\".card { color: ${glamour.css_var(accent)}; }\"\n\
                   \x20                   let node: VNode(Int) = glamour.element(\"section\", [glamour.css_scope(sheet), glamour.css_custom_properties([glamour.css_assign(accent, color)])], [glamour.text(\"themed\")])\n\
                   \x20                   console.print(glamour.css_text(sheet))\n\
                   \x20                   console.print(glamour.to_html(node))\n";
        let out = glamour_run_both(src);
        assert_eq!(out.len(), 2, "typed custom-property trace: {out:?}");
        assert!(out[0].contains("color: var(--glamour-accent)"), "{out:?}");
        assert!(
            out[1].contains("style=\"--glamour-accent:#663399\""),
            "{out:?}"
        );

        let mismatched = "import glamour\n\
                          \n\
                          fn main(console: Console):\n\
                          \x20   match glamour.css_px(12):\n\
                          \x20       Err(_) -> console.print(\"invalid\")\n\
                          \x20       Ok(length) -> console.print(glamour.css_text(css\".card { color: ${length}; }\"))\n";
        let entry = parser::parse_module(mismatched).expect("parse mismatched CSS value");
        let glamour = glamour_module_cached();
        let linked = crate::pipeline::link(
            vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
            "main",
        )
        .expect("link mismatched CSS value");
        let error = typeck::check(&linked)
            .expect_err("a length must not enter a color property")
            .to_string();
        assert!(
            error.contains("expected `glamour.CssColorKind`")
                && error.contains("found `glamour.CssLengthKind`"),
            "{error}"
        );
    }

    #[test]
    fn glamour_css_extended_categories_are_typed_and_have_backend_parity() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   match glamour.css_percent(25):\n\
                   \x20       Err(_) -> console.print(\"invalid-percent\")\n\
                   \x20       Ok(value) -> console.print(glamour.css_text(css\".card { background-position-x: ${value}; }\"))\n\
                   \x20   match glamour.css_deg(90):\n\
                   \x20       Err(_) -> console.print(\"invalid-angle\")\n\
                   \x20       Ok(value) -> console.print(glamour.css_text(css\".card { rotate: ${value}; }\"))\n\
                   \x20   match glamour.css_ms(250):\n\
                   \x20       Err(_) -> console.print(\"invalid-time\")\n\
                   \x20       Ok(value) -> console.print(glamour.css_text(css\".card { transition-duration: ${value}; }\"))\n";
        let out = glamour_run_both(src);
        assert_eq!(out.len(), 3, "extended CSS category trace: {out:?}");
        assert!(out[0].contains("background-position-x: 25%"), "{out:?}");
        assert!(out[1].contains("rotate: 90deg"), "{out:?}");
        assert!(out[2].contains("transition-duration: 250ms"), "{out:?}");

        let mismatched = "import glamour\n\
                          \n\
                          fn main(console: Console):\n\
                          \x20   match glamour.css_deg(90):\n\
                          \x20       Err(_) -> console.print(\"invalid\")\n\
                          \x20       Ok(angle) -> console.print(glamour.css_text(css\".card { transition-duration: ${angle}; }\"))\n";
        let entry = parser::parse_module(mismatched).expect("parse mismatched CSS category");
        let glamour = glamour_module_cached();
        let linked = crate::pipeline::link(
            vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
            "main",
        )
        .expect("link mismatched CSS category");
        let error = typeck::check(&linked)
            .expect_err("an angle must not enter a time property")
            .to_string();
        assert!(
            error.contains("expected `glamour.CssTimeKind`")
                && error.contains("found `glamour.CssAngleKind`"),
            "{error}"
        );
    }

    #[test]
    fn glamour_routes_and_progressive_forms_have_backend_parity() {
        let src = "import glamour\n\
                   from glamour import FormFieldKind, FormValue, RouteDef, RouteGraph, RouteId, RouteMatch, VNode\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let fallback = RouteDef(RouteId(\"fallback\"), \"/\", [])\n\
                   \x20   let home = glamour.route(\"/\").unwrap_or(fallback)\n\
                   \x20   let rune = glamour.route(\"/runes/:name\").unwrap_or(fallback)\n\
                   \x20   let graph = glamour.route_graph([home, rune]).unwrap_or(RouteGraph([]))\n\
                   \x20   console.print(glamour.static_route_paths(graph).join(\",\"))\n\
                   \x20   match glamour.route_url(rune, [(\"name\", \"witchy/core\")]):\n\
                   \x20       Err(error) -> console.print(glamour.route_error_message(error))\n\
                   \x20       Ok(destination) ->\n\
                   \x20           let link: VNode(Int) = glamour.element(\"a\", [glamour.navigation_url_attribute(\"href\", destination)], [glamour.text(\"Rune\")])\n\
                   \x20           console.print(glamour.to_html(link))\n\
                   \x20   match glamour.match_route(graph, \"/runes/witchy\"):\n\
                   \x20       None -> console.print(\"no-match\")\n\
                   \x20       Some(found) ->\n\
                   \x20           match found:\n\
                   \x20               glamour.RouteMatch(id, values) -> console.print(glamour.route_id_string(id) + \":\" + values.at(0).1)\n\
                   \x20   let fields = [glamour.form_field(\"name\", \"Name\", FormText, true), glamour.form_field(\"email\", \"Email\", FormEmail, true)]\n\
                   \x20   match glamour.form_schema(\"signup\", \"post\", glamour.form_url_or_root(\"/signup\"), fields):\n\
                   \x20       Err(problem) -> console.print(glamour.form_problem_message(problem))\n\
                   \x20       Ok(schema) ->\n\
                   \x20           let form: VNode(Int) = glamour.element(\"form\", glamour.form_attributes(schema), [glamour.text(\"Sign up\")])\n\
                   \x20           console.print(glamour.to_html(form))\n\
                   \x20           let problems = glamour.validate_form(schema, [FormValue(\"name\", \"Ada\"), FormValue(\"email\", \"invalid\")])\n\
                   \x20           console.print(problems.map(glamour.form_problem_message).join(\",\"))\n\
                   \x20   match glamour.form_schema(\"lookup\", \"GET\", glamour.form_url_or_root(\"/lookup\"), [glamour.form_field(\"token\", \"Token\", FormSecret, true)]):\n\
                   \x20       Ok(_) -> console.print(\"unsafe-secret-get\")\n\
                   \x20       Err(problem) -> console.print(glamour.form_problem_message(problem))\n";
        let out = glamour_run_both(src);
        assert_eq!(out.len(), 6, "route and form trace: {out:?}");
        assert_eq!(out[0], "/");
        assert_eq!(out[1], "<a href=\"/runes/witchy%2Fcore\">Rune</a>");
        assert!(out[2].starts_with("glamour-route1-") && out[2].ends_with(":witchy"), "{out:?}");
        assert!(out[3].starts_with("<form data-glamour-form=\"glamour-form1-"));
        assert!(out[3].ends_with("\" method=\"POST\" action=\"/signup\">Sign up</form>"));
        assert_eq!(out[4], "invalid value for form field `email`");
        assert_eq!(out[5], "secret form field `token` requires POST");
    }

    #[test]
    fn glamour_client_action_frames_decode_to_typed_messages_on_both_backends() {
        let src = r#"import bytes
import glamour
from glamour import ClientActionFrame, ClientActionValue, FormFieldKind, FormSchema

fn schema() -> FormSchema:
    let fallback = FormSchema("invalid", "POST", glamour.form_url_or_root("/"), [])
    glamour.form_schema("signup", "POST", glamour.form_url_or_root("/signup"), [
        glamour.form_field("email", "Email", FormEmail, true),
        glamour.form_field("password", "Password", FormSecret, true),
        glamour.form_field("updates", "Updates", FormCheckbox, false),
    ]).unwrap_or(fallback)

fn put_u8(var output: List(Int), value: Int):
    output.push(value % 256)

fn put_u16(var output: List(Int), value: Int):
    put_u8(output, value)
    put_u8(output, value / 256)

fn put_u32(var output: List(Int), value: Int):
    put_u16(output, value % 65536)
    put_u16(output, value / 65536)

fn put_header(var output: List(Int), kind: Int, total: Int, sequence: Int, payload: Int):
    for value in [71, 76, 77, 82]:
        put_u8(output, value)
    put_u16(output, 1)
    put_u16(output, 4)
    put_u8(output, kind)
    put_u8(output, 0)
    put_u16(output, 48)
    put_u32(output, total)
    put_u32(output, 1)
    put_u32(output, 7)
    put_u32(output, 11)
    put_u32(output, 22)
    put_u32(output, sequence)
    put_u32(output, 0)
    put_u32(output, payload)
    put_u32(output, 0)

fn input_frame(schema_id: Int) -> Bytes:
    let email = bytes.from_string("ada@example.test")
    let updates = bytes.from_string("true")
    var output: List(Int) = []
    put_header(output, 4, 124, 9, 104)
    put_u16(output, 1)
    put_u16(output, 0)
    put_u32(output, 56)
    put_u32(output, schema_id)
    put_u32(output, 2)
    put_u32(output, 2)
    put_u32(output, 0)
    put_u16(output, 0)
    put_u8(output, 2)
    put_u8(output, 0)
    put_u32(output, 104)
    put_u32(output, email.length())
    put_u32(output, 0)
    put_u16(output, 2)
    put_u8(output, 4)
    put_u8(output, 0)
    put_u32(output, 120)
    put_u32(output, updates.length())
    put_u32(output, 0)
    for value in email.to_list():
        output.push(value)
    for value in updates.to_list():
        output.push(value)
    bytes.from_list(output).unwrap_or(bytes.from_string(""))

fn completion_frame(schema_id: Int) -> Bytes:
    var output: List(Int) = []
    put_header(output, 5, 80, 10, 0)
    put_u16(output, 1)
    put_u16(output, 0)
    put_u32(output, 32)
    put_u32(output, schema_id)
    put_u32(output, 2)
    put_u32(output, 0)
    put_u32(output, 204)
    put_u32(output, 0)
    put_u32(output, 0)
    bytes.from_list(output).unwrap_or(bytes.from_string(""))

fn main(console: Console):
    let form = schema()
    let input_schema = glamour.form_input_schema(form)
    let result_schema = glamour.form_result_schema(form)
    console.print("${input_schema}|${result_schema}")
    match glamour.decode_client_action_frame(form, input_frame(input_schema), 7, 11, 22, 9):
        glamour.ClientActionInput(generation, values) ->
            match (values.at(0), values.at(1)):
                (glamour.ClientActionEmail(email_name, email), glamour.ClientActionCheckbox(check_name, checked)) ->
                    console.print("input|${generation}|${email_name}|${email}|${check_name}|${checked == Some(true)}")
                _ -> console.print("wrong-values")
        _ -> console.print("wrong-input")
    match glamour.decode_client_action_frame(form, completion_frame(result_schema), 7, 11, 22, 10):
        glamour.ClientActionCompletion(generation, glamour.ClientActionSucceeded, status) -> console.print("done|${generation}|${status}")
        _ -> console.print("wrong-completion")
"#;
        let out = glamour_run_both(src);
        assert_eq!(out.len(), 3, "typed action trace: {out:?}");
        assert_eq!(out[0], "2859606054|4195218877");
        assert_eq!(out[1], "input|2|email|ada@example.test|updates|true");
        assert_eq!(out[2], "done|2|204");
    }

    #[test]
    fn glamour_form_submission_decoder_has_server_backend_parity() {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../../projects/glamour/form-decoder-fixtures.json"
        ))
        .expect("form decoder fixtures");
        let action = &fixtures["action"];
        let quoted = |value: &str| serde_json::to_string(value).expect("quoted Witchy string");
        let fields = action["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .map(|field| {
                let kind = match field["kind"].as_str().expect("field kind") {
                    "text" => "FormText",
                    "email" => "FormEmail",
                    "number" => "FormNumber",
                    "checkbox" => "FormCheckbox",
                    "secret" => "FormSecret",
                    kind => panic!("unsupported fixture kind {kind}"),
                };
                format!(
                    "glamour.form_field({}, {}, {kind}, {})",
                    quoted(field["name"].as_str().expect("field name")),
                    quoted(field["label"].as_str().expect("field label")),
                    field["required"].as_bool().expect("required"),
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let cases = fixtures["cases"].as_array().expect("cases");
        let accepted = cases
            .iter()
            .find(|case| case["name"] == "accepted")
            .expect("accepted case");
        let rejected = cases
            .iter()
            .find(|case| case["name"] == "rejected")
            .expect("rejected case");
        let entries = |case: &serde_json::Value| {
            case["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .map(|entry| {
                    format!(
                        "({}, {})",
                        quoted(entry[0].as_str().expect("entry name")),
                        quoted(entry[1].as_str().expect("entry value")),
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let secret_name = accepted["secrets"][0][0].as_str().expect("secret name");
        let src = format!(
            "import glamour\n\
             from glamour import FormFieldKind, FormSchema\n\
             \n\
             fn schema() -> FormSchema:\n\
             \x20   let fallback = FormSchema(\"invalid\", \"POST\", glamour.form_url_or_root(\"/\"), [])\n\
             \x20   glamour.form_schema(\"signup\", {}, glamour.form_url_or_root({}), [{fields}]).unwrap_or(fallback)\n\
             \n\
             @server\n\
             fn main(console: Console):\n\
             \x20   let form = schema()\n\
             \x20   match glamour.decode_form_entries(form, [{}]):\n\
             \x20       Err(_) -> console.print(\"valid-rejected\")\n\
             \x20       Ok(submission) ->\n\
             \x20           console.print(\"public:${{glamour.form_submission_values(submission).length()}}\")\n\
             \x20           console.print(glamour.form_submission_secret(submission, {}) ?? \"missing-secret\")\n\
             \x20   match glamour.decode_form_entries(form, [{}]):\n\
             \x20       Ok(_) -> console.print(\"invalid-accepted\")\n\
             \x20       Err(problems) -> console.print(problems.map(glamour.form_problem_message).join(\",\"))\n",
            quoted(action["method"].as_str().expect("method")),
            quoted(action["action"].as_str().expect("action")),
            entries(accepted),
            quoted(secret_name),
            entries(rejected),
        );
        let out = glamour_run_both(&src);
        let expected_problems = rejected["problems"]
            .as_array()
            .expect("problems")
            .iter()
            .map(|problem| problem.as_str().expect("problem"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            out,
            vec![
                format!("public:{}", accepted["public"].as_array().expect("public").len()),
                accepted["secrets"][0][1].as_str().expect("secret value").to_string(),
                expected_problems,
            ],
        );
    }

    #[test]
    fn glamour_route_graph_rejects_duplicate_patterns() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   match glamour.route(\"/same\"):\n\
                   \x20       Err(_) -> console.print(\"route-error\")\n\
                   \x20       Ok(route) ->\n\
                   \x20           match glamour.route_graph([route, route]):\n\
                   \x20               Ok(_) -> console.print(\"accepted\")\n\
                   \x20               Err(error) -> console.print(glamour.route_error_message(error))\n";
        let out = glamour_run_both(src);
        assert_eq!(out, vec!["duplicate route pattern `/same`".to_string()]);
    }

    #[test]
    fn glamour_wildcard_routes_preserve_and_encode_path_segments() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   match glamour.route(\"/book/*slug\"):\n\
                   \x20       Err(error) -> console.print(glamour.route_error_message(error))\n\
                   \x20       Ok(route) ->\n\
                   \x20           match glamour.route_url(route, [(\"slug\", \"guide/first page\")]):\n\
                   \x20               Err(error) -> console.print(glamour.route_error_message(error))\n\
                   \x20               Ok(destination) -> console.print(glamour.safe_url_string(destination))\n";
        let out = glamour_run_both(src);
        assert_eq!(out, vec!["/book/guide/first%20page".to_string()]);
    }

    #[test]
    fn glamour_accessible_primitives_have_backend_parity() {
        let src = "import glamour\n\
                   from glamour import VNode\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let save: VNode(Int) = glamour.button(\"Save\", 1)\n\
                   \x20   let logo: VNode(Int) = glamour.image(glamour.asset_url_or_empty(\"/logo.png\"), \"Witchy\")\n\
                   \x20   console.print(glamour.to_html(glamour.element(\"div\", [], [save, logo])))\n\
                   \x20   let form: VNode(Int) = html\"<div><label for=\\\"name\\\">Name</label><input id=\\\"name\\\" value=${\"Ada\"}></input></div>\"\n\
                   \x20   console.print(glamour.to_html(form))\n";
        let out = glamour_run_both(src);
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("<button type=\"button\"") && out[0].contains("alt=\"Witchy\""), "{out:?}");
        assert!(out[1].contains("<label for=\"name\">Name</label>") && out[1].contains("value=\"Ada\""), "{out:?}");
    }

    #[test]
    fn glamour_template_accessibility_diagnostics_fail_at_expansion() {
        for (literal, expected) in [
            ("<img src=\"/logo.png\"/>", "<img> requires an alt"),
            ("<button></button>", "<button> requires text content"),
            ("<div><input id=\"name\"></input></div>", "needs a matching <label"),
            ("<div><span id=\"same\">a</span><span id=\"same\">b</span></div>", "duplicate static id"),
        ] {
            let src = format!(
                "import glamour\n\nfn main(console: Console):\n    console.print(glamour.to_html(html\"{}\"))\n",
                literal.replace('"', "\\\""),
            );
            let entry = parser::parse_module(&src).expect("parse entry");
            let glamour = glamour_module_cached();
            let err = crate::pipeline::link(
                vec![("main".to_string(), entry), ("glamour".to_string(), glamour)],
                "main",
            )
            .expect_err("inaccessible static template must fail during expansion")
            .to_string();
            assert!(err.contains(expected), "expected `{expected}` in: {err}");
        }
    }

    /// RFC-0136: ergonomic program constructors (simple_program, command_program) have backend parity.
    #[test]
    fn glamour_rfc0136_ergonomic_programs_have_backend_parity() {
        let src = "import glamour\n\
                   from glamour import Ui\n\
                   \n\
                   type Msg:\n\
                   \x20   Inc\n\
                   \x20   Dec\n\
                   \n\
                   fn update_simple(count: Int, msg: Msg) -> Int:\n\
                   \x20   match msg:\n\
                   \x20       Inc -> count + 1\n\
                   \x20       Dec -> count - 1\n\
                   \n\
                   fn view_simple(count: Int) -> Ui(Msg):\n\
                   \x20   glamour.ui(glamour.element(\"div\", [], [glamour.text(\"count: ${count}\")]))\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let app = glamour.simple_program(0, update_simple, view_simple)\n\
                   \x20   let (model, cmd, sub, ui) = glamour.program_update(app, Nil, 0, Inc)\n\
                   \x20   console.print(\"after Inc: ${model}\")\n\
                   \x20   console.print(glamour.to_html(glamour.ui_node(ui)))\n";
        let out = glamour_run_both(src);
        assert_eq!(out, vec!["after Inc: 1".to_string(), "<div>count: 1</div>".to_string()]);
    }

