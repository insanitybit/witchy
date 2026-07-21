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
                   fn twice(parts: List(String), holes: List(String)) -> String:\n\
                   \x20   let a = list.at(parts, 0)\n\
                   \x20   let b = list.at(parts, 1)\n\
                   \x20   let h = list.at(holes, 0)\n\
                   \x20   \"\\\"\" + a + \"\\\" + \" + h + \" + \" + h + \" + \\\"\" + b + \"\\\"\"\n\
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

    /// Link a program that `import glamour` against the embedded glamour source
    /// (and, transitively, the bundled std), then run it on BOTH backends and
    /// assert they agree — the parity oracle for the framework rune. Returns the
    /// agreed output. The `html` tag is a COMPILE-TIME literal (RFC-0006): it is
    /// expanded by the linker before either backend sees the program, so this is a
    /// genuine differential test of the *expanded* `VNode`-constructing AST.
    fn glamour_run_both(src: &str) -> Vec<String> {
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
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

    /// Like [`glamour_run_both`] but also links `std/markdown` — for the docs renderer.
    fn markdown_run_both(src: &str) -> Vec<String> {
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
        let markdown = parser::parse_module(MARKDOWN_SRC).expect("parse markdown");
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
                "<pre class=\"glamour-slot\"><code>fn main():\n    pass</code></pre>".to_string(),
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
                   \x20   let cmd = glamour.submit_secret(glamour.secret_ref(input), cred, \"Done\")\n\
                   \x20   let node: VNode(Msg) = glamour.secret_input(input, \"PwStatus\")\n\
                   \x20   console.print(glamour.to_json(node, mj))\n\
                   \x20   console.print(json.encode(glamour.cmd_to_json(cmd, mj)))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
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
                   \x20   let cls = \"card\"\n\
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
        assert_eq!(out, vec!["<button data-on-click=\"[msg]\">+</button>".to_string()]);
    }

    /// RFC-0008: glamour's `to_json` serializes a VNode tree to the wire format the
    /// JS DOM host shell (`web/witchy-runtime/glamour-dom.mjs`) consumes —
    /// `{"el":tag,"attrs":[["prop",k,v]|["on",evt,<msg-json>]],"kids":[...]}` /
    /// `{"text":"..."}`. The `On` binding embeds the msg via a caller-supplied
    /// `msg_to_json` (here `json.from_value`), so an event handler round-trips as its
    /// message value. The serialized string must be IDENTICAL on both backends.
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
            &parser::parse_module(GLAMOUR_SRC).expect("parse glamour"),
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
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
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
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
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
