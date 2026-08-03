    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("witchy-web-{name}-{}-{nanos}", std::process::id()))
    }

    fn glamour_output_operations(frame: &[u8]) -> Vec<&[u8]> {
        let count = u32::from_le_bytes(frame[16..20].try_into().expect("operation count"));
        let mut cursor = 48_usize;
        let mut operations = Vec::new();
        for _ in 0..count {
            let length = u32::from_le_bytes(
                frame[cursor + 4..cursor + 8]
                    .try_into()
                    .expect("operation length"),
            ) as usize;
            assert!(length >= 8 && cursor + length <= frame.len());
            operations.push(&frame[cursor..cursor + length]);
            cursor += length;
        }
        operations
    }

    fn glamour_operation_tag(operation: &[u8]) -> u16 {
        u16::from_le_bytes(operation[..2].try_into().expect("operation tag"))
    }

    fn glamour_operation_u32(operation: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(
            operation[offset..offset + 4]
                .try_into()
                .expect("operation field"),
        )
    }

    fn glamour_payload_text(frame: &[u8], offset: u32, length: u32) -> &str {
        let start = offset as usize;
        let end = start + length as usize;
        std::str::from_utf8(&frame[start..end]).expect("operation payload UTF-8")
    }

    fn instantiate_glamour_wasm(wasm: &[u8]) -> (wasmtime::Store<()>, wasmtime::Instance) {
        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("Wasm GC engine");
        let module = wasmtime::Module::new(&engine, wasm).expect("Glamour Wasm");
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap("witchy", "fill_pending", |_pointer: i32| {})
            .expect("fill_pending");
        linker
            .func_wrap(
                "witchy",
                "user_cap_field_len",
                |_capability: i32, _field: i32| -> i32 { 0 },
            )
            .expect("user_cap_field_len");
        linker
            .func_wrap(
                "witchy",
                "__witchy_abort",
                |_template: i32,
                 _first: i64,
                 _second: i64,
                 _text: i32|
                 -> wasmtime::Result<()> { Err(wasmtime::Error::msg("Witchy guest aborted")) },
            )
            .expect("abort");
        linker
            .func_wrap(
                "witchy",
                "encoding",
                |mut caller: wasmtime::Caller<'_, ()>,
                 _operation: i32,
                 input: i32,
                 output: i32|
                 -> wasmtime::Result<i32> {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .ok_or_else(|| wasmtime::Error::msg("missing memory"))?;
                    let data = memory.data(&caller);
                    let start = input as usize;
                    let length = u32::from_le_bytes(
                        data.get(start..start + 4)
                            .ok_or_else(|| wasmtime::Error::msg("input header"))?
                            .try_into()
                            .expect("four-byte header"),
                    ) as usize;
                    let bytes = data
                        .get(start + 4..start + 4 + length)
                        .ok_or_else(|| wasmtime::Error::msg("input bytes"))?
                        .to_vec();
                    memory.write(&mut caller, output as usize, &bytes)?;
                    Ok(length as i32)
                },
            )
            .expect("encoding");
        linker
            .func_wrap(
                "witchy",
                "string_from_code",
                |_codepoint: i64, _output: i32| -> i32 { 0 },
            )
            .expect("string_from_code");
        linker
            .func_wrap(
                "witchy",
                "crypto.sha256",
                |mut caller: wasmtime::Caller<'_, ()>, input: i32, output: i32| -> wasmtime::Result<()> {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .ok_or_else(|| wasmtime::Error::msg("missing memory"))?;
                    let data = memory.data(&caller);
                    let start = input as usize;
                    let length = u32::from_le_bytes(
                        data.get(start..start + 4)
                            .ok_or_else(|| wasmtime::Error::msg("SHA-256 input header"))?
                            .try_into()
                            .expect("four-byte header"),
                    ) as usize;
                    let digest = sha256(
                        data.get(start + 4..start + 4 + length)
                            .ok_or_else(|| wasmtime::Error::msg("SHA-256 input bytes"))?,
                    );
                    memory.write(&mut caller, output as usize, digest.as_bytes())?;
                    Ok(())
                },
            )
            .expect("SHA-256");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module).expect("Glamour instance");
        (store, instance)
    }

    fn embedded_mount_grant(wasm: &[u8]) -> Value {
        let sections = wasmparser::Parser::new(0)
            .parse_all(wasm)
            .filter_map(|payload| match payload.expect("valid Wasm") {
                wasmparser::Payload::CustomSection(section)
                    if section.name() == "witchy.web.mount-grant" =>
                {
                    Some(
                        serde_json::from_slice(section.data())
                            .expect("mount grant section JSON"),
                    )
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(sections.len(), 1);
        sections.into_iter().next().expect("mount grant section")
    }

    #[test]
    fn starter_uses_the_compiler_generated_static_island_boundary() {
        let root = temp_path("starter");
        let files = starter_files("web-check", "web_check");
        assert!(files[Path::new("witchy.toml")].contains("delivery = \"static\""));
        assert!(!files.contains_key(Path::new("web/index.html")));
        assert!(!files.contains_key(Path::new("web/glamour-manifest.json")));
        create_project_atomically(&root, &files).expect("create static starter");

        let checked = check_static_project(load_project(&root).expect("starter project"))
            .expect("check static starter");
        assert_eq!(checked.pages.len(), 1);
        assert_eq!(checked.islands.len(), 1);
        assert_eq!(checked.island_plans.len(), 1);
        assert_eq!(checked.islands[0].diagnostic_name.as_deref(), Some("counter"));
        assert_eq!(checked.islands[0].activation, "interaction");
        assert_eq!(checked.islands[0].mode, "resume");
        assert_eq!(checked.islands[0].state.as_deref(), Some("0"));

        let publication = checked
            .island_publication
            .as_ref()
            .expect("compiler-generated island publication");
        assert_eq!(publication.artifacts.len(), 1);
        let islands = publication.manifest["islands"]
            .as_array()
            .expect("island manifest records");
        assert_eq!(islands.len(), 1);
        assert!(islands[0]["events"].as_array().is_some_and(|events| events.len() == 1));

        let output = root.join("dist");
        write_static_production(&checked, &output).expect("publish static starter");
        static_site::audit_static_island_artifacts(&output, &checked)
            .expect("audit compiler-generated starter graph");
        let index = std::fs::read_to_string(output.join("index.html"))
            .expect("published starter route");
        assert!(index.contains("<h1>Witchy app</h1>"));
        assert!(index.contains("data-glamour-island="));
        assert!(!index.contains("id=\"app\""));
        std::fs::remove_dir_all(root).expect("cleanup");

        let default_root = temp_path("starter-default-delivery");
        let mut default_files = starter_files("default-web", "default_web");
        let manifest = default_files
            .get_mut(Path::new("witchy.toml"))
            .expect("starter manifest");
        *manifest = manifest.replace("delivery = \"static\"\n", "");
        create_project_atomically(&default_root, &default_files)
            .expect("create default-delivery starter");
        assert_eq!(
            load_project(&default_root).expect("default-delivery project").delivery,
            Delivery::Static,
        );
        std::fs::remove_dir_all(default_root).expect("cleanup default-delivery starter");
    }

    #[test]
    fn legacy_client_fixture_is_complete_and_deterministic() {
        let root = temp_path("starter");
        let files = client_fixture_files("web-check", "web_check");
        create_project_atomically(&root, &files).expect("create");
        let checked = check_project(&root).expect("check");
        assert_eq!(checked.grant.capability, "UiRoot");
        assert_eq!(checked.grant.policy, "web-check");
        assert_eq!(checked.grant.digest.len(), 64);
        assert!(checked.development.is_none());
        for name in FORBIDDEN_PRODUCTION_EXPORTS {
            assert!(!checked.exports.contains(*name));
        }
        assert_eq!(checked.wasm, compile_project(&checked.project).expect("second compile"));
        let output = root.join("dist");
        let first = write_production(&checked, &output).expect("first build");
        let first_bytes = snapshot(&output);
        let second = write_production(&checked, &output).expect("second build");
        assert_eq!(first, second);
        assert_eq!(first_bytes, snapshot(&output));
        assert!(output.join("witchy-build-report.json").is_file());
        assert!(output.join("witchy-sbom.cdx.json").is_file());
        assert!(output.join("_headers").is_file());
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-web-manifest.json")).expect("manifest"),
        )
        .expect("manifest JSON");
        let report: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-build-report.json")).expect("report"),
        )
        .expect("report JSON");
        assert_eq!(manifest["protocolVersion"], json!({"major": 1, "minor": 4}));
        assert_eq!(manifest["mountGrant"]["digest"], checked.grant.digest);
        assert_eq!(report["protocol"], json!({"major": 1, "minor": 4}));
        let runtime = std::fs::read_to_string(
            std::fs::read_dir(output.join("assets"))
                .expect("assets")
                .map(Result::unwrap)
                .find(|entry| {
                    entry.file_name().to_string_lossy().starts_with("glamour-runtime-")
                })
                .expect("runtime")
                .path(),
        )
        .expect("runtime text");
        assert!(!runtime.contains("__WITCHY_GLAMOUR_DEV__"));
        assert!(runtime.contains("function encodeCompletionResult"));
        assert!(runtime.contains("function encodeActivationFrame"));
        let emitted_wasm = std::fs::read(
            std::fs::read_dir(output.join("assets"))
                .expect("assets")
                .map(Result::unwrap)
                .find(|entry| entry.file_name().to_string_lossy().starts_with("app-"))
                .expect("application Wasm")
                .path(),
        )
        .expect("application Wasm bytes");
        let embedded = embedded_mount_grant(&emitted_wasm);
        assert_eq!(embedded["grant"]["digest"], checked.grant.digest);
        assert_eq!(embedded["artifact"], Value::Null);
        assert_eq!(embedded["artifactGrant"], Value::Null);
        let sbom: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-sbom.cdx.json")).expect("SBOM"),
        )
        .expect("SBOM JSON");
        assert!(!checked.packages.is_empty());
        assert_eq!(
            sbom["components"].as_array().expect("components").len(),
            checked.packages.len() + 2
        );
        audit_generated_artifacts(&output, &checked, "production").expect("production audit");
        std::fs::write(
            output.join("index.html"),
            format!("leaked path: {}", checked.project.root.display()),
        )
        .expect("inject audit fixture");
        let error = audit_generated_artifacts(&output, &checked, "production")
            .expect_err("absolute source path must fail audit");
        assert!(error.to_string().contains("absolute project path"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn runnable_web_grants_are_explicit_public_and_unambiguous() {
        let missing_root = temp_path("missing-web-grant");
        let mut missing = client_fixture_files("missing-grant", "missing_grant");
        let manifest = missing
            .get_mut(Path::new("witchy.toml"))
            .expect("project manifest");
        *manifest = manifest.replace("grants = \"web/grants.toml\"\n", "");
        missing.remove(Path::new("web/grants.toml"));
        create_project_atomically(&missing_root, &missing).expect("create missing fixture");
        let error = check_project(&missing_root).expect_err("implicit grant must fail");
        assert!(error.message.contains("requires `web.grants`"));
        std::fs::remove_dir_all(missing_root).expect("cleanup missing fixture");

        let broad_root = temp_path("broad-web-grant");
        let mut broad = client_fixture_files("broad-grant", "broad_grant");
        broad.insert(
            PathBuf::from("web/grants.toml"),
            "[fetch]\napi = [\"https://example.com\"]\n[user_caps]\nui = { type = \"UiRoot\", policy = \"broad\" }\n".into(),
        );
        create_project_atomically(&broad_root, &broad).expect("create broad fixture");
        let error = check_project(&broad_root).expect_err("host authority must fail");
        assert!(error.message.contains("may contain only one public"));
        std::fs::remove_dir_all(broad_root).expect("cleanup broad fixture");

        let ambiguous_root = temp_path("ambiguous-web-grant");
        let mut ambiguous = client_fixture_files("ambiguous-grant", "ambiguous_grant");
        ambiguous.insert(
            PathBuf::from("web/grants.toml"),
            "[user_caps]\nfirst = { type = \"UiRoot\", policy = \"one\" }\nsecond = { type = \"UiRoot\", policy = \"two\" }\n".into(),
        );
        create_project_atomically(&ambiguous_root, &ambiguous)
            .expect("create ambiguous fixture");
        let error = check_project(&ambiguous_root).expect_err("ambiguous grant must fail");
        assert!(error.message.contains("exactly one `[user_caps]` entry"));
        std::fs::remove_dir_all(ambiguous_root).expect("cleanup ambiguous fixture");
    }

    #[test]
    fn nested_keyed_island_templates_retain_authenticated_regions() {
        let root = temp_path("nested-keyed-island-template");
        let mut files = static_fixture_files("nested-island", "nested_island");
        files.insert(
            PathBuf::from("src/nested_island.witchy"),
            r#"from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg:
    Tick

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn start(_auth: UiRoot, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: UiRoot, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn render(_model: Int) -> Ui(Msg):
    let nested = [glamour.keyed("x", glamour.element("li", [], [glamour.text("X")])), glamour.keyed("y", glamour.element("li", [], [glamour.text("Y")]))]
    let outer = glamour.keyed("a", glamour.element("li", [glamour.on_event("nested.tick", "click", glamour.event_msg(Tick))], [glamour.element("ol", [], nested)]))
    glamour.ui(glamour.element("ul", [], [outer]))

fn subscriptions(_auth: UiRoot, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let island = glamour.island("nested", app(), 0, static_view, glamour.OnInteraction)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(island)))]),
        [island],
    )
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create nested island project");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("nested keyed island declaration");
        let plan = &checked.island_plans[0];
        assert_eq!(plan.auth_type, "glamour.UiRoot");
        assert_eq!(plan.regions.len(), 2);
        let outer = plan
            .regions
            .iter()
            .find(|region| region.path == [0])
            .expect("outer keyed region");
        let nested = plan
            .regions
            .iter()
            .find(|region| region.path == [0, 0, 0])
            .expect("nested keyed region");
        assert_ne!(outer.keys[0].template, 0);
        assert!(outer.dynamic.is_none(), "event-bearing nested templates stay closed-world");
        assert!(nested.dynamic.is_some(), "flat homogeneous nested keys authenticate a prototype");
        let artifact = &checked
            .island_publication
            .as_ref()
            .expect("island publication")
            .artifact_manifest["artifacts"][0];
        let template = artifact["templates"]
            .as_array()
            .expect("template registry")
            .iter()
            .find(|template| template["id"] == outer.keys[0].template)
            .expect("outer insertion template");
        let nested_record = &template["regions"][nested.id.to_string()];
        assert_eq!(nested_record["parent"], nested.parent);
        assert_eq!(
            nested_record["keys"]
                .as_array()
                .expect("nested template keys")
                .len(),
            2,
        );
        assert_eq!(template["events"][0]["node"], plan.events[0].node);
        assert_eq!(
            template["events"][0]["eventPlan"],
            plan.events[0].plan,
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn compiler_authenticates_structural_region_liveness() {
        let root = temp_path("branch-island-template");
        let mut files = static_fixture_files("branch-island", "branch_island");
        files.insert(
            PathBuf::from("src/branch_island.witchy"),
            r#"from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg:
    Tick

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn start(_auth: UiRoot, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: UiRoot, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn render(model: Int) -> Ui(Msg):
    let toggle = glamour.element("button", [glamour.on_event("branch.tick", "click", glamour.event_msg(Tick))], [glamour.text("toggle")])
    let details = glamour.branch("details", model % 2 == 1, glamour.element("button", [glamour.on_event("details.tick", "click", glamour.event_msg(Tick))], [glamour.text("count-${model}")]))
    let summary_template = glamour.element("button", [glamour.on_event("summary.tick", "click", glamour.event_msg(Tick))], [glamour.text("summary-${model}")])
    let summary = glamour.optional_child("summary", summary_template, if model > 0: Some(glamour.element("button", [glamour.on_event("summary.tick", "click", glamour.event_msg(Tick))], [glamour.text("summary-${model}")])) else: None)
    glamour.ui(glamour.element("div", [], [toggle, details, summary]))

fn subscriptions(_auth: UiRoot, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let island = glamour.island("branching", app(), 0, static_view, glamour.OnInteraction)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(island)))]),
        [island],
    )
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create branch island project");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("branch island declaration");
        let plan = &checked.island_plans[0];
        assert_eq!(plan.regions.len(), 2);
        let branch = plan
            .regions
            .iter()
            .find(|region| region.kind == StaticIslandRegionKind::Branch)
            .expect("branch region");
        let optional = plan
            .regions
            .iter()
            .find(|region| region.kind == StaticIslandRegionKind::Child)
            .expect("optional-child region");
        assert!(branch.keys.is_empty());
        assert!(optional.keys.is_empty());
        let branch_child = branch.child.as_ref().expect("authenticated branch child");
        let optional_child = optional.child.as_ref().expect("authenticated optional child");
        assert!(!branch_child.mounted);
        assert!(!optional_child.mounted);
        assert_ne!(branch_child.template, 0);
        assert_ne!(optional_child.template, 0);
        assert_eq!(branch_child.path, vec![0, 1]);
        assert_eq!(optional_child.path, vec![0, 2]);
        assert_eq!(branch.before, vec![optional_child.root]);
        assert!(optional.before.is_empty());
        assert!(!plan.html.contains("count-0"));
        assert!(!plan.html.contains("summary-0"));
        assert_eq!(plan.events.len(), 3);
        let toggle_event = plan
            .events
            .iter()
            .find(|event| event.id == "branch.tick")
            .expect("live toggle event");
        let details_event = plan
            .events
            .iter()
            .find(|event| event.id == "details.tick")
            .expect("branch template event");
        let summary_event = plan
            .events
            .iter()
            .find(|event| event.id == "summary.tick")
            .expect("child template event");
        let artifact = &checked
            .island_publication
            .as_ref()
            .expect("island publication")
            .artifact_manifest["artifacts"][0];
        let artifact_regions = artifact["regions"].as_array().expect("region registry");
        let declared_branch = artifact_regions
            .iter()
            .find(|region| region["kind"] == "branch")
            .expect("declared branch");
        assert_eq!(declared_branch["nodes"], json!(branch_child.nodes));
        assert!(artifact_regions.iter().any(|region| region["kind"] == "child"));
        let resume_regions = artifact["resume"]["regions"]
            .as_array()
            .expect("resume regions");
        assert!(resume_regions
            .iter()
            .any(|region| region["id"] == branch.id && region["child"].is_null()));
        assert!(resume_regions
            .iter()
            .any(|region| region["id"] == optional.id && region["child"].is_null()));
        let live_nodes = artifact["resume"]["nodes"]
            .as_array()
            .expect("live resume nodes");
        assert!(branch_child.nodes.iter().all(|id| {
            live_nodes.iter().all(|node| node["id"] != *id)
        }));
        assert!(optional_child.nodes.iter().all(|id| {
            live_nodes.iter().all(|node| node["id"] != *id)
        }));
        let template = artifact["templates"]
            .as_array()
            .expect("template registry")
            .iter()
            .find(|template| template["id"] == branch_child.template)
            .expect("branch template");
        assert_eq!(template["root"]["kind"], "element");
        assert_eq!(template["root"]["tag"], "button");
        assert_eq!(template["events"][0]["node"], details_event.node);
        assert_eq!(template["events"][0]["eventPlan"], details_event.plan);
        let optional_template = artifact["templates"]
            .as_array()
            .expect("template registry")
            .iter()
            .find(|template| template["id"] == optional_child.template)
            .expect("optional-child template");
        assert_eq!(optional_template["root"]["tag"], "button");
        assert_eq!(optional_template["events"][0]["node"], summary_event.node);
        assert_eq!(optional_template["events"][0]["eventPlan"], summary_event.plan);
        let artifact_event_plans = artifact["eventPlans"]
            .as_array()
            .expect("event-plan registry");
        let event_owner = |event: &StaticIslandEventRecord| {
            artifact_event_plans
                .iter()
                .find(|plan| plan["id"] == event.plan)
                .expect("published event plan")["instance"]
                .as_u64()
                .expect("event owner instance") as u32
        };
        assert_eq!(event_owner(toggle_event), plan.registry_id);
        assert_eq!(event_owner(details_event), branch.id);
        assert_eq!(event_owner(summary_event), optional.id);
        let resume_events = artifact["resume"]["events"]
            .as_array()
            .expect("live resume events");
        assert_eq!(resume_events.len(), 1);
        assert_eq!(resume_events[0]["eventPlan"], toggle_event.plan);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn optimized_islands_teardown_only_the_departed_structural_owner() {
        let root = temp_path("structural-effect-owners");
        let mut files = static_fixture_files("structural-owners", "structural_owners");
        files.insert(
            PathBuf::from("src/structural_owners.witchy"),
            r#"import reflect
from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    StartA
    StartDetached
    StartB
    RemoveA
    Done

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn start(_auth: UiRoot, _model: Int) -> Cmd(Msg):
    NoCmd

fn delayed(auth: UiRoot) -> Cmd(Msg):
    glamour.schedule("shared", glamour.timer_scope(auth, 10), 20, Done)

fn update(auth: UiRoot, model: Int, message: Msg) -> (Int, Cmd(Msg)):
    match message:
        StartA -> (model, delayed(auth))
        StartDetached -> (model, glamour.detach(delayed(auth)))
        StartB -> (model, delayed(auth))
        RemoveA -> (1, NoCmd)
        Done -> (model + 10, NoCmd)

fn view(model: Int) -> Ui(Msg):
    let items = if model == 0:
        [
            glamour.keyed("a", glamour.element("div", [], [
                glamour.element("button", [glamour.on_event("owner.start.a", "click", glamour.event_msg(StartA))], [glamour.text("A")]),
                glamour.element("button", [glamour.on_event("owner.start.detached", "click", glamour.event_msg(StartDetached))], [glamour.text("detached")])
            ])),
            glamour.keyed("b", glamour.element("button", [glamour.on_event("owner.start.b", "click", glamour.event_msg(StartB))], [glamour.text("B")]))
        ]
    else:
        [glamour.keyed("b", glamour.element("button", [glamour.on_event("owner.start.b", "click", glamour.event_msg(StartB))], [glamour.text("B")]))]
    glamour.ui(glamour.element("main", [], [
        glamour.element("button", [glamour.on_event("owner.remove.a", "click", glamour.event_msg(RemoveA))], [glamour.text("remove")]),
        glamour.text("${model}"),
        glamour.element("div", [], items)
    ]))

fn clock(auth: UiRoot, id: String) -> Sub(Msg):
    glamour.every(id, glamour.timer_scope(auth, 10), 1000, Done)

fn subscriptions(auth: UiRoot, model: Int) -> Sub(Msg):
    if model == 0:
        glamour.batch_sub([clock(auth, "left"), clock(auth, "right")])
    else:
        clock(auth, "right")

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, view, subscriptions)

pub fn web() -> Site:
    let owners = glamour.interactive(app(), 0).activate(glamour.OnLoad)
    glamour.site([glamour.static_page("/", glamour.ui(glamour.element("main", [], [glamour.embed(owners)])))])
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create structural-owner fixture");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("compile structural-owner fixture");
        let plan = &checked.island_plans[0];
        let region = plan
            .regions
            .iter()
            .find(|region| region.kind == StaticIslandRegionKind::List)
            .expect("keyed owner region");
        let owner_a = region
            .keys
            .iter()
            .find(|key| key.source == "a")
            .expect("owner a")
            .id;
        let owner_b = region
            .keys
            .iter()
            .find(|key| key.source == "b")
            .expect("owner b")
            .id;
        assert_ne!(owner_a, owner_b);
        let event_a = plan
            .events
            .iter()
            .find(|event| event.id == "owner.start.a")
            .expect("owner-a event");
        let event_b = plan
            .events
            .iter()
            .find(|event| event.id == "owner.start.b")
            .expect("owner-b event");
        let event_detached = plan
            .events
            .iter()
            .find(|event| event.id == "owner.start.detached")
            .expect("detached owner-a event");
        let remove_a = plan
            .events
            .iter()
            .find(|event| event.id == "owner.remove.a")
            .expect("root removal event");
        let descriptor = plan
            .effect_descriptors
            .iter()
            .find(|descriptor| descriptor.semantic == "timer")
            .expect("timer descriptor");
        assert_ne!(descriptor.owner_scope, 0);
        let publication = checked
            .island_publication
            .as_ref()
            .expect("structural-owner publication");
        let artifact = &publication.artifact_manifest["artifacts"][0];
        assert_eq!(
            artifact["effectDescriptors"][descriptor.id.to_string()]["ownerScope"],
            descriptor.owner_scope,
        );
        let executable = &publication.artifacts[0];
        let (mut store, instance) = instantiate_glamour_wasm(&executable.wasm);
        let memory = instance.get_memory(&mut store, "memory").expect("memory");
        let reserve = instance
            .get_typed_func::<i32, i32>(&mut store, "__glamour_input_reserve")
            .expect("input reserve");
        let init = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_init")
            .expect("init");
        let dispatch = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_dispatch")
            .expect("dispatch");
        let output_length = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_output_length")
            .expect("output length");
        let output_release = instance
            .get_typed_func::<(), ()>(&mut store, "__glamour_output_release")
            .expect("output release");
        let app_id = artifact["appId"].as_u64().expect("app id") as u32;
        let build_id = u64::from_str_radix(
            artifact["buildId"]
                .as_str()
                .expect("build id")
                .trim_start_matches("0x"),
            16,
        )
        .expect("hex build id");
        let mut start_frame = vec![0_u8; 49];
        start_frame[..4].copy_from_slice(b"GLMR");
        start_frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
        start_frame[8] = 1;
        start_frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
        start_frame[12..16].copy_from_slice(&49_u32.to_le_bytes());
        start_frame[20..24].copy_from_slice(&app_id.to_le_bytes());
        start_frame[24..32].copy_from_slice(&build_id.to_le_bytes());
        start_frame[40..44].copy_from_slice(&48_u32.to_le_bytes());
        start_frame[48] = b'0';
        let pointer = reserve
            .call(&mut store, start_frame.len() as i32)
            .expect("reserve start");
        memory
            .write(&mut store, pointer as usize, &start_frame)
            .expect("write start");
        let initial_output = init
            .call(&mut store, (pointer, start_frame.len() as i32))
            .expect("initialize structural-owner adapter");
        let initial_length = output_length
            .call(&mut store, ())
            .expect("initial output length") as usize;
        let initial_bytes = memory.data(&store)
            [initial_output as usize..initial_output as usize + initial_length]
            .to_vec();
        let initial_subscriptions = glamour_output_operations(&initial_bytes)
            .into_iter()
            .filter(|operation| glamour_operation_tag(operation) == 258)
            .map(|operation| glamour_operation_u32(operation, 8))
            .collect::<Vec<_>>();
        assert_eq!(initial_subscriptions.len(), 2);
        let left_subscription = initial_subscriptions[0];
        let right_subscription = initial_subscriptions[1];
        assert_ne!(left_subscription, right_subscription);
        output_release.call(&mut store, ()).expect("release mount");

        let event_frame = |event: &StaticIslandEventRecord,
                           owner: u32,
                           sequence: u32|
         -> Vec<u8> {
            let mut frame = vec![0_u8; 96];
            frame[..4].copy_from_slice(b"GLMR");
            frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
            frame[8] = 2;
            frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
            frame[12..16].copy_from_slice(&96_u32.to_le_bytes());
            frame[16..20].copy_from_slice(&1_u32.to_le_bytes());
            frame[20..24].copy_from_slice(&app_id.to_le_bytes());
            frame[24..32].copy_from_slice(&build_id.to_le_bytes());
            frame[32..36].copy_from_slice(&sequence.to_le_bytes());
            frame[40..44].copy_from_slice(&96_u32.to_le_bytes());
            frame[48..50].copy_from_slice(&1_u16.to_le_bytes());
            frame[52..56].copy_from_slice(&48_u32.to_le_bytes());
            frame[56..60].copy_from_slice(&event.plan.to_le_bytes());
            frame[60..64].copy_from_slice(&owner.to_le_bytes());
            frame[64..68].copy_from_slice(&event.event_class.to_le_bytes());
            frame[72..76].copy_from_slice(&96_u32.to_le_bytes());
            frame[80..84].copy_from_slice(&96_u32.to_le_bytes());
            frame
        };
        let mut dispatch_event = |frame: Vec<u8>| -> Vec<u8> {
            let pointer = reserve
                .call(&mut store, frame.len() as i32)
                .expect("reserve event");
            memory
                .write(&mut store, pointer as usize, &frame)
                .expect("write event");
            let output = dispatch
                .call(&mut store, (pointer, frame.len() as i32))
                .expect("dispatch event");
            let length = output_length.call(&mut store, ()).expect("event output") as usize;
            let emitted = memory.data(&store)[output as usize..output as usize + length].to_vec();
            output_release
                .call(&mut store, ())
                .expect("release dispatched output");
            emitted
        };

        let started_detached = dispatch_event(event_frame(event_detached, owner_a, 0));
        let assert_root_subscriptions = |output: &[u8], expected: &[u32]| {
            let operations = glamour_output_operations(output);
            assert!(!operations
                .iter()
                .any(|operation| glamour_operation_tag(operation) == 259));
            let subscriptions = operations
                .into_iter()
                .filter(|operation| glamour_operation_tag(operation) == 258)
                .map(|operation| glamour_operation_u32(operation, 8))
                .collect::<Vec<_>>();
            assert_eq!(subscriptions, expected);
        };
        assert_root_subscriptions(&started_detached, &[left_subscription, right_subscription]);
        let effect_detached = glamour_output_operations(&started_detached)
            .into_iter()
            .find(|operation| glamour_operation_tag(operation) == 256)
            .expect("detached owner-a command starts root-owned work");
        let effect_detached_instance = glamour_operation_u32(effect_detached, 8);

        let started_a = dispatch_event(event_frame(event_a, owner_a, 1));
        assert_root_subscriptions(&started_a, &[left_subscription, right_subscription]);
        let started_a_operations = glamour_output_operations(&started_a);
        let effect_a = started_a_operations
            .iter()
            .copied()
            .find(|operation| glamour_operation_tag(operation) == 256)
            .unwrap_or_else(|| {
                panic!(
                    "owner a starts its effect; operations={:?}",
                    started_a_operations
                        .iter()
                        .map(|operation| glamour_operation_tag(operation))
                        .collect::<Vec<_>>()
                )
            });
        let effect_a_instance = glamour_operation_u32(effect_a, 8);
        assert_eq!(glamour_operation_u32(effect_a, 12), effect_a_instance);

        let started_b = dispatch_event(event_frame(event_b, owner_b, 2));
        assert_root_subscriptions(&started_b, &[left_subscription, right_subscription]);
        let operations_b = glamour_output_operations(&started_b);
        assert!(!operations_b
            .iter()
            .any(|operation| glamour_operation_tag(operation) == 257));
        let effect_b = operations_b
            .into_iter()
            .find(|operation| glamour_operation_tag(operation) == 256)
            .expect("owner b starts its independent effect");
        let effect_b_instance = glamour_operation_u32(effect_b, 8);
        assert_ne!(effect_a_instance, effect_b_instance);

        let removed_a = dispatch_event(event_frame(remove_a, plan.registry_id, 3));
        let removed_subscription_operations = glamour_output_operations(&removed_a);
        assert!(removed_subscription_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 259
                && glamour_operation_u32(operation, 8) == left_subscription
        }));
        assert!(!removed_subscription_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 259
                && glamour_operation_u32(operation, 8) == right_subscription
        }));
        assert!(removed_subscription_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 258
                && glamour_operation_u32(operation, 8) == right_subscription
        }));
        let removal_operations = glamour_output_operations(&removed_a);
        assert!(removal_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 257
                && glamour_operation_u32(operation, 8) == effect_a_instance
        }));
        assert!(!removal_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 257
                && glamour_operation_u32(operation, 8) == effect_b_instance
        }));
        assert!(!removal_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 257
                && glamour_operation_u32(operation, 8) == effect_detached_instance
        }));

        let completion_frame = |instance: u32, sequence: u32| -> Vec<u8> {
            let mut frame = vec![0_u8; 88];
            frame[..4].copy_from_slice(b"GLMR");
            frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
            frame[8] = 3;
            frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
            frame[12..16].copy_from_slice(&88_u32.to_le_bytes());
            frame[16..20].copy_from_slice(&1_u32.to_le_bytes());
            frame[20..24].copy_from_slice(&app_id.to_le_bytes());
            frame[24..32].copy_from_slice(&build_id.to_le_bytes());
            frame[32..36].copy_from_slice(&sequence.to_le_bytes());
            frame[40..44].copy_from_slice(&88_u32.to_le_bytes());
            frame[48..50].copy_from_slice(&1_u16.to_le_bytes());
            frame[52..56].copy_from_slice(&40_u32.to_le_bytes());
            frame[56..60].copy_from_slice(&1_u32.to_le_bytes());
            frame[60..64].copy_from_slice(&instance.to_le_bytes());
            frame[64..68].copy_from_slice(&1_u32.to_le_bytes());
            frame[68..72].copy_from_slice(&descriptor.id.to_le_bytes());
            frame[72..76].copy_from_slice(&descriptor.result_schema.to_le_bytes());
            frame[80..84].copy_from_slice(&88_u32.to_le_bytes());
            frame
        };
        let completed_b = dispatch_event(completion_frame(effect_b_instance, 4));
        assert_root_subscriptions(&completed_b, &[right_subscription]);
        let text = glamour_output_operations(&completed_b)
            .into_iter()
            .find(|operation| glamour_operation_tag(operation) == 2)
            .expect("live sibling completion updates the model");
        assert_eq!(
            glamour_payload_text(
                &completed_b,
                glamour_operation_u32(text, 12),
                glamour_operation_u32(text, 16),
            ),
            "11",
        );
        let completed_detached = dispatch_event(completion_frame(effect_detached_instance, 5));
        assert_root_subscriptions(&completed_detached, &[right_subscription]);
        let detached_text = glamour_output_operations(&completed_detached)
            .into_iter()
            .find(|operation| glamour_operation_tag(operation) == 2)
            .expect("root-owned detached completion updates the model");
        assert_eq!(
            glamour_payload_text(
                &completed_detached,
                glamour_operation_u32(detached_text, 12),
                glamour_operation_u32(detached_text, 16),
            ),
            "21",
        );
        let late_a = completion_frame(effect_a_instance, 6);
        let pointer = reserve
            .call(&mut store, late_a.len() as i32)
            .expect("reserve stale owner completion");
        memory
            .write(&mut store, pointer as usize, &late_a)
            .expect("write stale owner completion");
        assert!(
            dispatch
                .call(&mut store, (pointer, late_a.len() as i32))
                .is_err(),
            "a departed structural owner has no private callback entry",
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn identical_islands_share_one_content_addressed_module() {
        let root = temp_path("shared-island-module");
        let mut files = static_fixture_files("shared-islands", "shared_islands");
        files.insert(
            PathBuf::from("src/shared_islands.witchy"),
            r#"from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg:
    Increment

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn initial(_start: Start) -> Int:
    0

fn start(_auth: Nil, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: Nil, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn render(model: Int) -> Ui(Msg):
    glamour.ui(glamour.element("button", [glamour.on_event("counter.increment", "click", glamour.event_msg(Increment))], [glamour.text("${model}")]))

fn subscriptions(_auth: Nil, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(Nil, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let left = glamour.island("left", app(), 0, static_view, glamour.OnInteraction)
    let right = glamour.island("right", app(), 0, static_view, glamour.OnInteraction)
    glamour.with_islands(
        glamour.site([
            glamour.static_page("/left", glamour.ui(glamour.island_node(left))),
            glamour.static_page("/right", glamour.ui(glamour.island_node(right))),
        ]),
        [left, right],
    )
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create shared-island project");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("shared island declarations");
        let publication = checked
            .island_publication
            .as_ref()
            .expect("island publication");
        let records = publication.artifact_manifest["artifacts"]
            .as_array()
            .expect("artifact plans");
        assert_eq!(records.len(), 1);
        assert_eq!(publication.manifest["islands"].as_array().expect("placements").len(), 2);
        assert_eq!(publication.artifacts.len(), 1);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn compiler_authenticates_every_scalar_template_slot_kind() {
        let root = temp_path("island-template-slots");
        let mut files = static_fixture_files("island-slots", "island_slots");
        files.insert(
            PathBuf::from("src/island_slots.witchy"),
            r#"from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg:
    Tick

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn initial(_start: Start) -> Int:
    0

fn start(_auth: Nil, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: Nil, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn render(model: Int) -> Ui(Msg):
    var attrs = [glamour.attribute("title", "title-${model}"), glamour.boolean_attribute("hidden", model == 0), glamour.property("value", "property-${model}"), glamour.static_url_attribute("href", "/${model}"), glamour.static_class_attribute(["item-${model}"]), glamour.aria_attribute("aria-label", "label-${model}"), glamour.prop("data-value", "compat-${model}")]
    match glamour.css_length_property("gap"):
        Err(_) -> Nil
        Ok(gap) ->
            match glamour.css_px(model):
                Err(_) -> Nil
                Ok(gap_value) ->
                    match glamour.css_percentage_property("position"):
                        Err(_) -> Nil
                        Ok(position) ->
                            match glamour.css_percent(model):
                                Err(_) -> Nil
                                Ok(position_value) ->
                                    match glamour.css_angle_property("turn"):
                                        Err(_) -> Nil
                                        Ok(turn) ->
                                            match glamour.css_deg(model):
                                                Err(_) -> Nil
                                                Ok(turn_value) ->
                                                    match glamour.css_time_property("delay"):
                                                        Err(_) -> Nil
                                                        Ok(delay) ->
                                                            match glamour.css_ms(model):
                                                                Err(_) -> Nil
                                                                Ok(delay_value) -> attrs.push(glamour.css_custom_properties([glamour.css_assign(gap, gap_value), glamour.css_assign(position, position_value), glamour.css_assign(turn, turn_value), glamour.css_assign(delay, delay_value)]))
    let item = glamour.element("a", attrs, [glamour.text("text-${model}")])
    glamour.ui(glamour.element("div", [], [glamour.keyed("item", item)]))

fn subscriptions(_auth: Nil, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(Nil, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let plan = glamour.island("slots", app(), 0, static_view, glamour.OnLoad)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(plan)))]),
        [plan],
    )
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create scalar-slot project");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("checked scalar-slot project");
        let slots = &checked.island_plans[0].regions[0].keys[0].slots;
        assert_eq!(slots.len(), 12);
        assert_eq!(
            slots
                .iter()
                .map(|slot| (slot.source_kind.as_str(), slot.kind.as_str()))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                ("aria", "aria"),
                ("attribute", "attribute"),
                ("boolean", "boolean"),
                ("class", "class"),
                ("css-angle", "custom-property"),
                ("css-length", "custom-property"),
                ("css-percentage", "custom-property"),
                ("css-time", "custom-property"),
                ("prop", "attribute"),
                ("property", "property"),
                ("text", "text"),
                ("url", "attribute"),
            ]),
        );
        assert!(slots.iter().all(|slot| slot.id != 0));
        let artifact = &checked
            .island_publication
            .as_ref()
            .expect("scalar-slot publication")
            .artifact_manifest["artifacts"][0];
        let custom_properties = artifact["customProperties"]
            .as_array()
            .expect("custom-property registry");
        assert_eq!(custom_properties.len(), 4);
        assert_eq!(
            custom_properties
                .iter()
                .map(|property| {
                    (
                        property["name"].as_str().expect("custom-property name"),
                        property["category"]
                            .as_str()
                            .expect("custom-property category"),
                    )
                })
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                ("--glamour-delay", "time"),
                ("--glamour-gap", "length"),
                ("--glamour-position", "percentage"),
                ("--glamour-turn", "angle"),
            ]),
        );
        assert_eq!(
            artifact["templates"][0]["root"]["attributes"]["style"],
            "--glamour-gap:0px;--glamour-position:0%;--glamour-turn:0deg;--glamour-delay:0ms"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn compiler_rejects_unproven_structural_shapes() {
        let source = r#"from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg:
    Tick

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn initial(_start: Start) -> Int:
    0

fn start(_auth: Nil, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: Nil, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn render(_model: Int) -> Ui(Msg):
    glamour.ui(glamour.element("div", [], [STRUCTURAL_NODE]))

fn subscriptions(_auth: Nil, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(Nil, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let island = glamour.island("branching", app(), 0, static_view, glamour.OnInteraction)
    glamour.with_islands(glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(island)))]), [island])
"#;
        for (name, structural_node, expected) in [
            (
                "inactive-nested-branch",
                "glamour.branch(\"outer\", false, glamour.element(\"section\", [], [glamour.branch(\"inner\", true, glamour.element(\"p\", [], [glamour.text(\"details\")]))]))",
                "contains a nested region",
            ),
            (
                "mismatched-child-template",
                "glamour.optional_child(\"details\", glamour.element(\"p\", [], [glamour.text(\"details\")]), Some(glamour.element(\"span\", [], [glamour.text(\"details\")])))",
                "template graph disagrees with its authenticated resume graph",
            ),
        ] {
            let root = temp_path(name);
            let mut files = static_fixture_files(name, "branch_check");
            files.insert(
                PathBuf::from("src/branch_check.witchy"),
                source.replace("STRUCTURAL_NODE", structural_node),
            );
            create_project_atomically(&root, &files).expect("create rejected branch project");
            let error = check_static_project(load_project(&root).expect("project"))
                .expect_err("unproven branch shape must be rejected");
            assert!(error.to_string().contains(expected), "{error}");
            std::fs::remove_dir_all(root).expect("cleanup");
        }
    }

    #[test]
    fn compiler_template_schema_ignores_source_line_movement() {
        let root = temp_path("compiler-template-schema");
        let mut files = client_fixture_files("template-check", "template_check");
        let source = files
            .get_mut(Path::new("src/template_check.witchy"))
            .expect("starter source");
        source.insert_str(0, "import glamour\n");
        source.push_str(
            "\nfn checked_view(name: String) -> glamour.VNode(Int):\n\
             \x20   jsx\"<p>${name}</p>\"\n",
        );
        create_project_atomically(&root, &files).expect("create");
        let first = check_project_development(&root).expect("first checked build");
        assert_eq!(first.templates.len(), 1);
        assert_eq!(first.templates[0].slots.len(), 1);
        assert_eq!(first.templates[0].slots[0].kind, "child");
        let first_schema = template_schema(&first).expect("first template schema");
        let first_map = compiler_template_registry(&first.templates, true);
        let first_operations = compiler_operation_source_registry(&first.templates);
        assert_eq!(first_operations.as_array().map(Vec::len), Some(2));
        assert_eq!(first_operations[0]["operation"], "mount");
        assert_eq!(first_operations[1]["operation"], "slot");
        assert_eq!(first_operations[1]["slotIndex"], 0);
        assert_eq!(first_operations[1]["kind"], "child");

        let source_path = root.join("src/template_check.witchy");
        let source = std::fs::read_to_string(&source_path).expect("read source");
        std::fs::write(
            &source_path,
            source.replace(
                "\nfn checked_view",
                "\n// Movement outside the literal changes diagnostics only.\n\nfn checked_view",
            ),
        )
        .expect("move source line");
        let second = check_project_development(&root).expect("second checked build");
        let second_schema = template_schema(&second).expect("second template schema");
        let second_map = compiler_template_registry(&second.templates, true);
        let second_operations = compiler_operation_source_registry(&second.templates);

        assert_eq!(first.templates[0].identity, second.templates[0].identity);
        assert_eq!(first_schema, second_schema);
        assert_eq!(first_map, second_map, "semantic origin IDs must remain stable");
        assert_ne!(
            first_operations, second_operations,
            "operation source spans must follow the edit"
        );
        assert_ne!(
            first.tagged_origins, second.tagged_origins,
            "private source spans must follow the edit"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn development_source_map_follows_only_loaded_local_imports() {
        let root = temp_path("development-source-functions");
        let mut files = client_fixture_files("source-functions", "source_functions");
        files
            .get_mut(Path::new("src/source_functions.witchy"))
            .expect("starter source")
            .insert_str(0, "import helper\n");
        let entry = files
            .get_mut(Path::new("src/source_functions.witchy"))
            .expect("starter source");
        *entry = entry.replace(
            "BrowserState(true)",
            "BrowserState(helper.Meter(1).value() == 1)",
        );
        files.insert(
            PathBuf::from("src/helper.witchy"),
            "fn first() -> Int:\n    1\n\ntype Meter:\n    Meter(Int)\n\nimpl Meter:\n    pub fn value(self) -> Int:\n        match self:\n            Meter(number) -> number\n\nfn second() -> Int:\n    2\n".into(),
        );
        files.insert(
            PathBuf::from("src/unloaded.witchy"),
            "fn hidden() -> Int:\n    3\n".into(),
        );
        create_project_atomically(&root, &files).expect("create source-map project");
        let project = load_project(&root).expect("load project");
        let graph = development_source_graph(&project).expect("source graph");
        let functions = graph.functions;
        let first = functions
            .iter()
            .find(|function| {
                function.module == "src/helper.witchy" && function.name == "first"
            })
            .expect("loaded helper function");
        assert_eq!((first.start_line, first.end_line), (1, 3));
        assert_eq!(first.module, "src/helper.witchy");
        assert!(functions
            .iter()
            .any(|function| {
                function.module == "src/helper.witchy" && function.name == "second"
            }));
        let method = functions
            .iter()
            .find(|function| {
                function.module == "src/helper.witchy" && function.name == "value"
            })
            .expect("loaded helper method");
        assert_eq!((method.start_line, method.end_line), (8, 11));
        assert_eq!(method.compiled_names, ["Meter__value", "helper.Meter__value"]);
        assert!(!functions
            .iter()
            .any(|function| function.module == "src/unloaded.witchy"));
        let checked = check_project_development(&root).expect("checked method source map");
        let source_method = checked
            .source_functions
            .as_array()
            .expect("source functions")
            .iter()
            .find(|function| {
                function["module"] == "src/helper.witchy" && function["name"] == "value"
            })
            .expect("source method inventory");
        let source_expressions = source_method["expressionSpans"]
            .as_array()
            .expect("source expression spans");
        assert!(source_expressions.iter().any(|span| span["start"]["line"] == 9));
        assert!(source_expressions.iter().any(|span| span["start"]["line"] == 10));
        let wasm_functions = checked.wasm_functions.as_array().expect("Wasm functions");
        let mapped_method = wasm_functions
            .iter()
            .find(|function| {
                function["source"]["module"] == "src/helper.witchy"
                    && function["source"]["start"]["line"] == 8
            })
            .expect("compiled method source map");
        let offsets = mapped_method["instructionOffsets"]
            .as_array()
            .expect("instruction offsets");
        let statements = mapped_method["statementMappings"]
            .as_array()
            .expect("statement mappings");
        let expressions = mapped_method["expressionSpans"]
            .as_array()
            .expect("mapped source expression spans");
        assert_eq!(expressions.len(), source_expressions.len());
        assert!(expressions.iter().all(|span| {
            span["module"] == "src/helper.witchy"
                && span.get("byteStart").is_none()
                && span.get("byteEnd").is_none()
        }));
        let mapped_expression = expressions
            .iter()
            .find(|span| span["statementMapping"].is_object())
            .expect("expression with an exact containing statement mapping");
        let containing = &mapped_expression["statementMapping"];
        let expression_start = containing["instructionStart"]
            .as_u64()
            .expect("expression statement instruction start") as usize;
        let expression_end = containing["instructionEnd"]
            .as_u64()
            .expect("expression statement instruction end") as usize;
        assert!(expression_start < expression_end);
        assert_eq!(offsets[expression_start], containing["byteStart"]);
        assert_eq!(offsets[expression_end], containing["byteEnd"]);
        assert!(!statements.is_empty());
        for statement in statements {
            let start = statement["instructionStart"].as_u64().expect("instruction start") as usize;
            let end = statement["instructionEnd"].as_u64().expect("instruction end") as usize;
            let line = statement["source"]["start"]["line"].as_u64().expect("source line");
            assert!((8..=11).contains(&line));
            assert!(start < end);
            assert_eq!(offsets[start], statement["byteStart"]);
            assert_eq!(offsets[end], statement["byteEnd"]);
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn typed_static_site_emits_routes_without_browser_runtime() {
        let root = temp_path("static-site");
        let mut files = static_fixture_files("static-check", "static_check");
        files.insert(
            PathBuf::from("src/static_check.witchy"),
            r##"from glamour import CssSheet, FormFieldKind, FormSchema, Site, StaticPreloadKind

type Message:
    Unused

fn signup() -> FormSchema:
    let fallback = FormSchema("invalid", "POST", glamour.form_url_or_root("#"), [])
    glamour.form_schema(
        "signup",
        "POST",
        glamour.form_url_or_root("/signup"),
        [
            glamour.form_field("email", "Email", FormEmail, true),
            glamour.form_field("password", "Password", FormSecret, true),
        ],
    ).unwrap_or(fallback)

fn styles() -> CssSheet:
    match glamour.css_color_property("accent"):
        Err(_) -> css".card { color: rebeccapurple; }"
        Ok(accent) ->
            match glamour.css_asset(glamour.asset_url_or_empty("/logo.svg")):
                Err(_) -> css".card { color: ${glamour.css_var(accent)}; }"
                Ok(logo) -> css".card { color: ${glamour.css_var(accent)}; background-image: ${logo}; }"

fn home(styles: CssSheet) -> glamour.Ui(Message):
    var attrs = [
        glamour.css_scope(styles),
        glamour.class_attribute(glamour.classes(["card"])),
    ]
    match glamour.css_color_property("accent"):
        Err(_) -> Nil
        Ok(accent) ->
            match glamour.css_color("#663399"):
                Err(_) -> Nil
                Ok(color) -> attrs.push(glamour.css_custom_properties([glamour.css_assign(accent, color)]))
    glamour.ui(glamour.element("main", attrs, [
        glamour.element("form", glamour.form_attributes(signup()), [
            glamour.text("Sign up"),
        ]),
        glamour.image(glamour.asset_url_or_empty("/logo.svg"), "Logo"),
    ]))

fn page(text: String) -> glamour.Ui(Message):
    glamour.ui(glamour.element("main", [], [
        glamour.text(text),
    ]))

pub fn web() -> Site:
    let styles = styles()
    glamour.site_with_assets(
        [
            glamour.static_page("/", home(styles)),
            glamour.static_page("/about", page("About")),
        ],
        [signup()],
        [glamour.routed_stylesheet(styles, ["/", "/about"], ["/"])],
        [glamour.static_asset(glamour.asset_url_or_empty("/logo.svg"))],
        [glamour.static_preload("/", glamour.asset_url_or_empty("/logo.svg"), PreloadImage)],
    )
"##
            .into(),
        );
        files.insert(
            PathBuf::from("web/public/logo.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>".into(),
        );
        create_project_atomically(&root, &files).expect("create");
        let project = load_project(&root).expect("load static project");
        assert_eq!(project.delivery, Delivery::Static);
        assert_eq!(project.hosting, HostingProfile::Portable);
        let checked = check_static_project(project).expect("check static site");
        assert_eq!(
            checked
                .pages
                .iter()
                .map(|page| page.route.as_str())
                .collect::<Vec<_>>(),
            ["/", "/about"]
        );
        assert_eq!(checked.actions.len(), 1);
        assert_eq!(checked.styles.len(), 1);
        assert_eq!(checked.styles[0].routes, ["/", "/about"]);
        assert_eq!(checked.styles[0].critical_routes, ["/"]);
        assert_eq!(checked.preloads.len(), 1);
        assert_eq!(checked.assets.len(), 1);
        assert_eq!(checked.actions[0].method, "POST");
        assert_eq!(checked.actions[0].action, "/signup");
        assert_ne!(checked.actions[0].input_schema, 0);
        assert_ne!(checked.actions[0].result_schema, 0);
        assert_ne!(checked.actions[0].input_schema, checked.actions[0].result_schema);
        assert_eq!(
            checked.actions[0]
                .fields
                .iter()
                .map(|field| field.kind.as_str())
                .collect::<Vec<_>>(),
            ["email", "secret"]
        );
        let mut mismatched_actions = checked.actions.clone();
        mismatched_actions[0].action = "/different".into();
        let error = static_site::validate_action_bindings(
            &checked.pages,
            &mismatched_actions,
        )
        .expect_err("rendered form and action manifest must agree");
        assert!(error.to_string().contains("disagrees with its method or action"));
        assert!(static_site::valid_static_form_url("/signup"));
        assert!(static_site::valid_static_form_url("https://example.test/signup"));
        assert!(!static_site::valid_static_form_url("ftp://example.test/upload"));
        assert!(!static_site::valid_static_form_url("javascript:alert(1)"));
        assert!(static_site::valid_static_custom_properties(
            "--glamour-accent:#663399;--glamour-gap:12px"
        ));
        assert!(static_site::valid_static_custom_properties(
            "--glamour-position:25%;--glamour-turn:90deg;--glamour-delay:250ms"
        ));
        assert!(!static_site::valid_static_custom_properties(
            "--glamour-accent:url(https://evil.test/x)"
        ));
        assert!(!static_site::valid_static_custom_properties(
            "--glamour-delay:3600001ms"
        ));

        let output = root.join("dist");
        let first = write_static_production(&checked, &output).expect("first static build");
        let first_bytes = snapshot(&output);
        let second = write_static_production(&checked, &output).expect("second static build");
        assert_eq!(first, second);
        assert_eq!(first_bytes, snapshot(&output));
        let index = std::fs::read_to_string(output.join("index.html")).expect("index");
        assert!(index.contains("data-glamour-form=\"glamour-form1-"));
        assert!(index.contains("<style data-glamour-style=\"glamour-css1-"));
        assert!(index.contains("<link rel=\"preload\" href=\"/assets/logo-"));
        assert!(index.contains("src=\"/assets/logo-"));
        assert!(index.contains("background-image: url(\"/assets/logo-"));
        assert!(index.contains("color: var(--glamour-accent)"));
        assert!(index.contains("style=\"--glamour-accent:#663399\""));
        assert!(!output.join("logo.svg").exists());
        let about =
            std::fs::read_to_string(output.join("about/index.html")).expect("about route");
        assert!(about.contains("<link rel=\"stylesheet\" href=\"/assets/style-"));
        assert!(!about.contains("<style data-glamour-style="));
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-web-manifest.json")).expect("manifest"),
        )
        .expect("manifest JSON");
        assert_eq!(manifest["actions"].as_array().expect("actions").len(), 1);
        assert_eq!(manifest["actions"][0]["fields"][1]["kind"], "secret");
        let policies = manifest["browserPolicy"]
            .as_array()
            .expect("route browser policies");
        assert_eq!(policies.len(), 2);
        let home_policy = policies
            .iter()
            .find(|policy| policy["route"] == "/")
            .expect("home route browser policy");
        assert_eq!(home_policy["trustedTypes"]["required"], true);
        assert_eq!(
            home_policy["authority"]["staticControls"]["actions"][0]["fields"][1]["kind"],
            "secret",
        );
        assert_eq!(
            home_policy["authority"]["capabilities"]["secretFields"][0]["field"],
            "password",
        );
        let csp = home_policy["contentSecurityPolicy"]
            .as_str()
            .expect("home CSP");
        let meta_csp = home_policy["metaContentSecurityPolicy"]
            .as_str()
            .expect("home meta CSP");
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("script-src 'none'"));
        assert!(csp.contains("form-action 'self'"));
        assert!(csp.contains("require-trusted-types-for 'script'"));
        assert!(csp.contains("trusted-types 'none'"));
        assert!(csp.contains("'sha256-"));
        assert!(csp.contains("style-src-attr 'unsafe-hashes' 'sha256-"));
        assert!(!csp.contains("'unsafe-inline'"));
        assert!(!meta_csp.contains("frame-ancestors"));
        assert_eq!(home_policy["unavailableInMeta"], json!(["frame-ancestors", "permissions-policy"]));
        assert_eq!(home_policy["hosting"], json!({
            "profile": "portable",
            "responseHeadersRequired": false,
            "enforcement": "degraded",
        }));
        assert!(home_policy["permissionsPolicy"]
            .as_str()
            .expect("home permissions policy")
            .contains("publickey-credentials-get=()"));
        assert!(index.contains(&format!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"{}\">",
            meta_csp.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;"),
        )));
        assert_eq!(
            manifest["actions"][0]["inputSchema"],
            checked.actions[0].input_schema
        );
        assert_eq!(
            manifest["actions"][0]["resultSchema"],
            checked.actions[0].result_schema
        );
        assert_eq!(manifest["styles"].as_array().expect("styles").len(), 1);
        assert_eq!(manifest["preloads"].as_array().expect("preloads").len(), 1);
        assert_eq!(
            manifest["publicAssets"].as_array().expect("public assets").len(),
            1
        );
        assert!(manifest["publicAssets"][0]["emitted"]
            .as_str()
            .expect("emitted asset URL")
            .starts_with("/assets/logo-"));
        let extracted_style = std::fs::read_to_string(
            output
                .join("assets")
                .join(manifest["styles"][0]["asset"].as_str().expect("style asset")),
        )
        .expect("extracted style");
        assert!(extracted_style.contains("background-image: url(\"/assets/logo-"));
        let report: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-build-report.json")).expect("report"),
        )
        .expect("report JSON");
        assert_eq!(report["actions"]["secretFields"], 1);
        assert_eq!(report["styles"]["criticalRouteBindings"], 1);
        assert_eq!(report["styles"]["extractedAssets"], 1);
        assert_eq!(report["preloads"]["count"], 1);
        assert_eq!(report["publicAssets"]["count"], 1);
        assert_eq!(report["browserPolicy"], manifest["browserPolicy"]);
        let headers = std::fs::read_to_string(output.join("_headers")).expect("headers");
        assert!(headers.contains(&format!("Content-Security-Policy: {csp}")));
        assert!(headers.contains("Permissions-Policy: camera=(), microphone=(), geolocation=()"));
        assert!(output.join("about/index.html").is_file());
        assert!(snapshot(&output).iter().all(|(path, _)| {
            !path.ends_with(".js")
                && !path.ends_with(".mjs")
                && !path.ends_with(".wasm")
        }));
        static_site::audit_static_artifacts(&output, &checked).expect("static audit");
        std::fs::write(output.join("injected.js"), "alert(1)").expect("inject executable");
        let error = static_site::audit_static_artifacts(&output, &checked)
            .expect_err("static executable artifact must fail audit");
        assert!(error.to_string().contains("executable browser artifact"));

        use witchy_interp::interpreter::CompilerValue;
        let forged = CompilerValue::Constructor {
            name: "glamour.Site".into(),
            fields: vec![
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticPage".into(),
                    fields: vec![
                        CompilerValue::String("/forged".into()),
                        CompilerValue::String(
                            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"></head><body><script>alert(1)</script></body></html>".into(),
                        ),
                        CompilerValue::List(vec![]),
                        CompilerValue::List(vec![]),
                    ],
                }]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
            ],
        };
        let error = static_site::static_site_from_value(forged)
            .expect_err("direct constructor must not bypass static sink validation");
        assert!(error.to_string().contains("disallowed element `<script>`"));

        let forged_secret_get = CompilerValue::Constructor {
            name: "glamour.Site".into(),
            fields: vec![
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticPage".into(),
                    fields: vec![
                        CompilerValue::String("/about".into()),
                        CompilerValue::String(
                            checked
                                .pages
                                .iter()
                                .find(|page| page.route == "/about")
                                .expect("about page")
                                .html
                                .clone(),
                        ),
                        CompilerValue::List(vec![]),
                        CompilerValue::List(vec![]),
                    ],
                }]),
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticAction".into(),
                    fields: vec![
                        CompilerValue::String(format!(
                            "glamour-form1-{}",
                            "0".repeat(64)
                        )),
                        CompilerValue::String("GET".into()),
                        CompilerValue::String("/lookup".into()),
                        CompilerValue::List(vec![CompilerValue::Constructor {
                            name: "glamour.StaticActionField".into(),
                            fields: vec![
                                CompilerValue::String("token".into()),
                                CompilerValue::String("Token".into()),
                                CompilerValue::String("secret".into()),
                                CompilerValue::Bool(true),
                            ],
                        }]),
                    ],
                }]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
            ],
        };
        let error = static_site::static_site_from_value(forged_secret_get)
            .expect_err("direct constructors must not put secrets in GET URLs");
        assert!(error.to_string().contains("secret field `token` requires POST"));

        let forged_style = CompilerValue::Constructor {
            name: "glamour.Site".into(),
            fields: vec![
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticPage".into(),
                    fields: vec![
                        CompilerValue::String("/about".into()),
                        CompilerValue::String(
                            checked
                                .pages
                                .iter()
                                .find(|page| page.route == "/about")
                                .expect("about page")
                                .html
                                .clone(),
                        ),
                        CompilerValue::List(vec![]),
                        CompilerValue::List(vec![]),
                    ],
                }]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticStyle".into(),
                    fields: vec![
                        CompilerValue::Constructor {
                            name: "glamour.CssSheet".into(),
                            fields: vec![
                                CompilerValue::String(format!(
                                    "glamour-css1-{}",
                                    "0".repeat(64)
                                )),
                                CompilerValue::String("000000000000".into()),
                                CompilerValue::String("main:1".into()),
                                CompilerValue::String(
                                    "[data-glamour-scope=\"000000000000\"] .card {color:red;}\n"
                                        .into(),
                                ),
                                CompilerValue::List(vec![CompilerValue::String(
                                    "card".into(),
                                )]),
                            ],
                        },
                        CompilerValue::List(vec![CompilerValue::String("/about".into())]),
                        CompilerValue::List(vec![]),
                    ],
                }]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
            ],
        };
        let error = static_site::static_site_from_value(forged_style)
            .expect_err("forged CSS identity must fail at the publication boundary");
        assert!(
            error
                .to_string()
                .contains("disagrees with its checked representation"),
            "{error}"
        );

        let asset_scope = "111111111111";
        let asset_text = format!(
            "[data-glamour-scope=\"{asset_scope}\"] .card {{background-image: url(\"/missing.svg\");}}\n"
        );
        let asset_style_id = format!(
            "glamour-css1-{}",
            sha256(format!("{asset_scope}|{asset_text}|card").as_bytes())
        );
        let forged_css_asset = CompilerValue::Constructor {
            name: "glamour.Site".into(),
            fields: vec![
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticPage".into(),
                    fields: vec![
                        CompilerValue::String("/about".into()),
                        CompilerValue::String(
                            checked
                                .pages
                                .iter()
                                .find(|page| page.route == "/about")
                                .expect("about page")
                                .html
                                .clone(),
                        ),
                        CompilerValue::List(vec![]),
                        CompilerValue::List(vec![]),
                    ],
                }]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticStyle".into(),
                    fields: vec![
                        CompilerValue::Constructor {
                            name: "glamour.CssSheet".into(),
                            fields: vec![
                                CompilerValue::String(asset_style_id),
                                CompilerValue::String(asset_scope.into()),
                                CompilerValue::String("main:1".into()),
                                CompilerValue::String(asset_text),
                                CompilerValue::List(vec![CompilerValue::String("card".into())]),
                            ],
                        },
                        CompilerValue::List(vec![CompilerValue::String("/about".into())]),
                        CompilerValue::List(vec![]),
                    ],
                }]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
            ],
        };
        let error = static_site::static_site_from_value(forged_css_asset)
            .expect_err("CSS assets must be declared in the typed asset graph");
        assert!(error
            .to_string()
            .contains("references undeclared typed asset `/missing.svg`"));

        let forged_asset = CompilerValue::Constructor {
            name: "glamour.Site".into(),
            fields: vec![
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticPage".into(),
                    fields: vec![
                        CompilerValue::String("/about".into()),
                        CompilerValue::String(
                            checked
                                .pages
                                .iter()
                                .find(|page| page.route == "/about")
                                .expect("about page")
                                .html
                                .clone(),
                        ),
                        CompilerValue::List(vec![]),
                        CompilerValue::List(vec![]),
                    ],
                }]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![]),
                CompilerValue::List(vec![CompilerValue::Constructor {
                    name: "glamour.StaticAsset".into(),
                    fields: vec![CompilerValue::String("/../secret".into())],
                }]),
                CompilerValue::List(vec![]),
            ],
        };
        let error = static_site::static_site_from_value(forged_asset)
            .expect_err("forged asset paths must fail at the publication boundary");
        assert!(error.to_string().contains("non-local or non-canonical"));

        let mut missing_preload = checked.clone();
        missing_preload.preloads[0].href = "/missing.png".into();
        let error = write_static_production(&missing_preload, &root.join("missing-dist"))
            .expect_err("preloads must name emitted assets");
        assert!(error.to_string().contains("does not name an emitted public asset"));
        let mut missing_asset = checked.clone();
        missing_asset.assets[0].href = "/missing.png".into();
        let error = write_static_production(&missing_asset, &root.join("missing-asset-dist"))
            .expect_err("typed assets must name source files");
        assert!(error.to_string().contains("typed public asset") && error.to_string().contains("missing"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn static_hosting_profile_is_closed_and_explicit() {
        let root = temp_path("static-hosting-profile");
        let mut files = static_fixture_files("static-hosting", "static_hosting");
        files
            .get_mut(Path::new("witchy.toml"))
            .expect("manifest")
            .push_str("hosting = \"headers-required\"\n");
        create_project_atomically(&root, &files).expect("create hosting fixture");
        let project = load_project(&root).expect("load headers-required project");
        assert_eq!(project.hosting, HostingProfile::HeadersRequired);

        let mut unsupported = std::fs::read_to_string(root.join("witchy.toml"))
            .expect("hosting manifest");
        unsupported = unsupported.replace("headers-required", "guess");
        std::fs::write(root.join("witchy.toml"), unsupported).expect("write unsupported profile");
        let error = load_project(&root).expect_err("unknown hosting profile must fail");
        assert!(error.to_string().contains("web.hosting must be"));

        let mut unavailable = std::fs::read_to_string(root.join("witchy.toml"))
            .expect("hosting manifest");
        unavailable = unavailable.replace("hosting = \"guess\"", "hosting = \"portable\"\n[web.ports]\nlogin = \"unsafe.js\"");
        std::fs::write(root.join("witchy.toml"), unavailable).expect("write unavailable ports");
        let error = load_project(&root).expect_err("unimplemented production ports must fail");
        assert!(error.to_string().contains("typed host-custody registry"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn explicit_global_css_retains_rule_ownership_in_publication() {
        let root = temp_path("global-css");
        let mut files = static_fixture_files("global-css", "global_css");
        files.insert(
            PathBuf::from("src/global_css.witchy"),
            r#"import glamour
from glamour import CssSheet, Site

type Message:
    Unused

fn styles() -> CssSheet:
    global_css"html, body { color: rebeccapurple; } .app { margin-top: 0px; }"

pub fn web() -> Site:
    let sheet = styles()
    glamour.site_with_resources(
        [
            glamour.static_page(
                "/",
                glamour.ui(glamour.element("main", [glamour.class_attribute(glamour.classes(["app"]))], [glamour.text("Global")])),
            ),
        ],
        [],
        [glamour.stylesheet(sheet, ["/"])],
        [],
    )
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create global CSS project");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("check global CSS project");
        assert_eq!(checked.styles.len(), 1);
        let style = &checked.styles[0];
        assert_eq!(style.scope, "global");
        assert!(!style.origin.is_empty());
        assert_eq!(style.routes, ["/"]);
        assert!(style.text.contains("html, body { color: rebeccapurple;}"));
        assert!(!style.text.contains("data-glamour-scope"));

        let output = root.join("dist");
        write_static_production(&checked, &output).expect("publish global CSS project");
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-web-manifest.json")).expect("manifest"),
        )
        .expect("manifest JSON");
        assert_eq!(manifest["styles"][0]["scope"], "global");
        assert_eq!(manifest["styles"][0]["global"], true);
        assert_eq!(manifest["styles"][0]["origin"], style.origin);
        let asset = manifest["styles"][0]["asset"]
            .as_str()
            .expect("global CSS asset");
        let css = std::fs::read_to_string(output.join("assets").join(asset))
            .expect("global CSS text");
        assert_eq!(css, style.text);
        let report: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-build-report.json")).expect("report"),
        )
        .expect("report JSON");
        let rules = report["styles"]["globalRules"]
            .as_array()
            .expect("global rule ownership");
        assert_eq!(rules.len(), 3);
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule["selector"].as_str().expect("global selector"))
                .collect::<Vec<_>>(),
            ["html", "body", ".app"],
        );
        assert!(rules.iter().all(|rule| {
            rule["sheet"] == style.id
                && rule["owner"] == style.origin
                && rule["routes"] == serde_json::json!(["/"])
        }));
        static_site::audit_static_artifacts(&output, &checked).expect("audit global CSS output");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn declared_static_content_is_closed_deterministic_build_data() {
        let root = temp_path("static-content");
        let mut files = static_fixture_files("content-check", "content_check");
        files
            .get_mut(Path::new("witchy.toml"))
            .expect("manifest")
            .push_str("content = \"content\"\n");
        files.insert(
            PathBuf::from("src/content_check.witchy"),
            r#"from glamour import Site, StaticContent

type Message:
    Unused

fn page(path: String, body: String) -> glamour.StaticPage:
    glamour.static_page(path, glamour.ui(glamour.text(body)))

pub fn web(content: StaticContent) -> Site:
    var pages = []
    for file in glamour.static_content_files(content):
        let source = glamour.static_content_path(file)
        if source.ends_with(".md"):
            pages.push(page("/" + source.strip_suffix(".md"), glamour.static_content_text(file)))
    glamour.site(pages)
"#
            .into(),
        );
        files.insert(PathBuf::from("content/z.md"), "<unsafe>".into());
        files.insert(PathBuf::from("content/nested/a.md"), "safe".into());
        create_project_atomically(&root, &files).expect("create");

        let checked = check_static_project(load_project(&root).expect("load"))
            .expect("check content site");
        assert_eq!(
            checked
                .pages
                .iter()
                .map(|page| page.route.as_str())
                .collect::<Vec<_>>(),
            ["/nested/a", "/z"]
        );
        assert_eq!(
            checked
                .content_inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["nested/a.md", "z.md"]
        );
        assert!(checked.pages[1].html.contains("&lt;unsafe&gt;"));

        let output = root.join("dist");
        write_static_production(&checked, &output).expect("publish content site");
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-web-manifest.json")).expect("manifest"),
        )
        .expect("manifest JSON");
        assert_eq!(
            manifest["contentInputs"]
                .as_array()
                .expect("content inputs")
                .len(),
            2
        );
        assert_eq!(manifest["runtime"]["javascript"], false);
        assert_eq!(manifest["runtime"]["wasm"], false);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn static_islands_publish_compiler_authenticated_work_descriptors() {
        let root = temp_path("static-island-work-descriptors");
        let mut files = static_fixture_files("island-work", "island_work");
        let source = r#"import reflect
from glamour import Cmd, FormFieldKind, FormSchema, IslandPlan, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Tick
    Fetched(Int, glamour.HttpResult)
    Navigated(glamour.NavigationResult)
    Stored(glamour.StorageResult)

type Auth:
    Auth(glamour.UiTimer, glamour.UiFetch, glamour.UiRoute, glamour.UiStorage)

fn authorize(root: UiRoot) -> Auth:
    Auth(
        glamour.timer_scope(root, 10),
        glamour.fetch_scope(root, "book", "GET", "/content/"),
        glamour.route_scope(root, "/chapter/", "push"),
        glamour.storage_scope(root, "local", "preferences", "book.", 4096),
    )

fn initial(_start: Start) -> Int:
    0

fn start(auth: Auth, _model: Int) -> Cmd(Msg):
    match auth:
        Auth(timer, _fetch, _route, _storage) -> glamour.schedule("clock", timer, 20, Tick)

fn fetch_page(fetch: glamour.UiFetch, model: Int) -> Cmd(Msg):
    glamour.http_get("page", fetch, "/content/page.md", fn(result: glamour.HttpResult): Fetched(model, result))

fn visit_intro(route: glamour.UiRoute) -> Cmd(Msg):
    glamour.navigate("chapter", route, "/chapter/intro", fn(result: glamour.NavigationResult): Navigated(result))

fn load_preference(storage: glamour.UiStorage) -> Cmd(Msg):
    glamour.storage_get("preference", storage, "book.theme", fn(result: glamour.StorageResult): Stored(result))

fn update(auth: Auth, model: Int, message: Msg) -> (Int, Cmd(Msg)):
    match auth:
        Auth(_timer, fetch, route, storage) ->
            match message:
                Tick ->
                    (model + 1, Batch([
                        fetch_page(fetch, model),
                        visit_intro(route),
                    ]))
                Fetched(captured, _result) -> (captured + 10, NoCmd)
                Navigated(_result) -> (model, load_preference(storage))
                Stored(_result) -> (model, NoCmd)

fn render(model: Int) -> Ui(Msg):
    let form = signup()
    glamour.ui(glamour.element("main", [], [
        glamour.element("button", [glamour.on_event("clock.tick", "click", glamour.event_msg(Tick))], [glamour.text("${model}")]),
        glamour.element("form", glamour.form_attributes(form), [glamour.text("Sign up")]),
    ]))

fn subscriptions(auth: Auth, _model: Int) -> Sub(Msg):
    match auth:
        Auth(timer, _fetch, _route, _storage) -> glamour.every("pulse", timer, 20, Tick)

fn app() -> Program(Auth, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

fn signup() -> FormSchema:
    let fallback = FormSchema("invalid", "POST", glamour.form_url_or_root("/"), [])
    glamour.form_schema(
        "signup",
        "POST",
        glamour.form_url_or_root("/signup"),
        [glamour.form_field("password", "Password", FormSecret, true)],
    ).unwrap_or(fallback)

pub fn web() -> Site:
    let clock = glamour.island("clock", app(), 0, static_view, glamour.OnLoad)
    let form = signup()
    glamour.with_islands(
        glamour.site_with_forms(
            [glamour.static_page("/", glamour.ui(glamour.island_node(clock)))],
            [form],
        ),
        [clock],
    )
"#;
        files.insert(PathBuf::from("src/island_work.witchy"), source.into());
        create_project_atomically(&root, &files).expect("create descriptor fixture");
        let entry = root.join("src/island_work.witchy");
        let (compiler_checked, _) = crate::link_file_checked(
            entry.to_str().expect("UTF-8 fixture path"),
        )
        .expect("link descriptor fixture");
        let compiler_metadata = witchy_lower::codegen::checked_glamour_islands(
            &compiler_checked,
        )
        .expect("authenticate descriptor fixture");
        let request_metadata = compiler_metadata[0]
            .work
            .iter()
            .find(|work| work.kind == "http")
            .expect("HTTP work metadata");
        let route_metadata = compiler_metadata[0]
            .work
            .iter()
            .find(|work| work.kind == "navigation")
            .expect("navigation work metadata");
        assert_ne!(request_metadata.owner_scope_id, 0);
        assert_ne!(route_metadata.owner_scope_id, 0);
        assert_ne!(request_metadata.owner_scope_id, route_metadata.owner_scope_id);
        assert_eq!(request_metadata.completion_captures.len(), 1);
        assert_eq!(request_metadata.completion_captures[0].name, "model");
        assert_eq!(
            request_metadata.completion_captures[0].ty,
            witchy_syntax::ast::Type::Named("Int".into(), Vec::new()),
        );
        let execution = witchy_lower::codegen::checked_glamour_island_execution_module(
            &compiler_checked,
            &compiler_metadata[0],
        )
        .expect("rewrite descriptor fixture");
        let rewritten = execution
            .items
            .iter()
            .filter_map(|item| match item {
                witchy_syntax::ast::Item::Function(function)
                    if function.name.ends_with(".start")
                        || function.name.ends_with(".update")
                        || function.name.ends_with(".fetch_page")
                        || function.name.ends_with(".visit_intro")
                        || function.name.ends_with(".load_preference")
                        || function.name.ends_with(".subscriptions") =>
                {
                    Some(witchy_syntax::format::block_str(&function.body))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rewritten.contains("glamour.island_cmd_schedule("));
        assert!(rewritten.contains("glamour.island_cmd_http("));
        assert!(rewritten.contains("glamour.island_cmd_navigate("));
        assert!(rewritten.contains("glamour.island_cmd_storage_get("));
        assert!(rewritten.contains(&format!(
            "glamour.island_cmd_http({}, {}, {},",
            request_metadata.descriptor_id,
            request_metadata.completion_id,
            request_metadata.owner_scope_id,
        )));
        assert!(rewritten.contains(&format!(
            "glamour.island_cmd_navigate({}, {}, {},",
            route_metadata.descriptor_id,
            route_metadata.completion_id,
            route_metadata.owner_scope_id,
        )));
        assert!(rewritten.contains("glamour.island_sub_every("));
        assert!(rewritten.contains("glamour.IslandCaptureList("));
        assert!(rewritten.contains("glamour_island_capture_encode_"));
        assert!(!rewritten.contains("glamour.http_get("));
        assert!(!rewritten.contains("glamour.schedule("));
        assert!(!rewritten.contains("glamour.every("));
        assert!(!rewritten.contains("fn("));
        let generated_codecs = execution
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item,
                    witchy_syntax::ast::Item::Function(function)
                        if function.name.contains(".glamour_island_capture_")
                )
            })
            .count();
        assert!(generated_codecs >= 6);
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("checked work descriptors");
        let plan = &checked.island_plans[0];
        assert_eq!(plan.effect_descriptors.len(), 4);
        assert_eq!(plan.subscription_descriptors.len(), 1);
        let effect = plan
            .effect_descriptors
            .iter()
            .find(|descriptor| descriptor.semantic == "timer")
            .expect("timer descriptor");
        let request = plan
            .effect_descriptors
            .iter()
            .find(|descriptor| descriptor.semantic == "http")
            .expect("HTTP descriptor");
        let route = plan
            .effect_descriptors
            .iter()
            .find(|descriptor| descriptor.semantic == "navigation")
            .expect("navigation descriptor");
        let storage = plan
            .effect_descriptors
            .iter()
            .find(|descriptor| descriptor.semantic == "storage-get")
            .expect("storage descriptor");
        let subscription = &plan.subscription_descriptors[0];
        assert_eq!(effect.handler, "timer");
        assert_eq!(effect.semantic, "timer");
        assert_eq!(effect.policy, json!({"kind": "timer", "minimum": 10}));
        assert_ne!(effect.owner_scope, 0);
        assert!(effect.completion_source.ends_with(".Tick"));
        assert!(effect.completion_captures.is_empty());
        assert_eq!(request.handler, "request");
        assert_eq!(request.policy, json!({
            "kind": "fetch",
            "scope": "book",
            "methods": ["GET"],
            "prefix": "/content/",
        }));
        assert_ne!(request.owner_scope, 0);
        assert!(request.completion_source.contains("Fetched"));
        assert_eq!(request.completion_captures, ["model"]);
        assert_eq!(route.handler, "navigation");
        assert_eq!(route.policy, json!({
            "kind": "navigation",
            "base": "/chapter/",
            "rights": "push",
        }));
        assert_ne!(route.owner_scope, 0);
        assert_ne!(request.owner_scope, route.owner_scope);
        assert!(route.completion_source.contains("Navigated"));
        assert!(route.completion_captures.is_empty());
        assert_eq!(storage.handler, "storage");
        assert_eq!(storage.policy, json!({
            "kind": "storage",
            "provider": "local",
            "namespace": "preferences",
            "keyPrefix": "book.",
            "maxValueBytes": 4096,
        }));
        assert!(storage.completion_source.contains("Stored"));
        assert!(storage.completion_captures.is_empty());
        assert_eq!(subscription.handler, "interval");
        assert_eq!(subscription.semantic, "interval");
        assert_eq!(subscription.policy, json!({"kind": "timer", "minimum": 10}));
        assert_ne!(subscription.owner_scope, 0);
        for id in [
            effect.id,
            effect.result_schema,
            effect.completion_id,
            request.id,
            request.result_schema,
            request.completion_id,
            route.id,
            route.result_schema,
            route.completion_id,
            storage.id,
            storage.result_schema,
            storage.completion_id,
            subscription.id,
            subscription.result_schema,
            subscription.completion_id,
        ] {
            assert_ne!(id, 0);
        }
        let publication = checked
            .island_publication
            .as_ref()
            .expect("island publication");
        let mount_grant = &publication.manifest["mountGrant"];
        assert_eq!(mount_grant["capability"], "UiRoot");
        assert_eq!(mount_grant["policy"], "island-work");
        assert_eq!(mount_grant["digest"].as_str().expect("grant digest").len(), 64);
        let artifact = &publication.artifact_manifest["artifacts"][0];
        assert_ne!(artifact["artifact"], compiler_metadata[0].identity);
        assert_eq!(
            artifact["grantDigest"],
            publication.artifact_manifest["grantDigest"],
        );
        let projected_effects = artifact["grantProjection"]["effects"]
            .as_object()
            .expect("projected effects");
        assert_eq!(projected_effects.len(), 4);
        for id in [effect.id, request.id, route.id, storage.id] {
            assert!(projected_effects.contains_key(&id.to_string()));
        }
        assert_eq!(
            artifact["grantProjection"]["subscriptions"][subscription.id.to_string()]["policy"],
            subscription.policy,
        );
        assert_eq!(artifact["actions"].as_array().expect("artifact actions").len(), 1);
        assert_eq!(artifact["actions"][0]["fields"][0]["kind"], "secret");
        assert_eq!(
            artifact["grantProjection"]["staticControls"]["actions"],
            artifact["actions"],
        );
        assert_eq!(
            artifact["browserPolicy"]["secretFields"][0],
            json!({
                "form": artifact["actions"][0]["id"],
                "field": "password",
            }),
        );
        assert_eq!(
            artifact["browserPolicy"]["storage"][0],
            json!({
                "provider": "local",
                "namespace": "preferences",
                "keyPrefix": "book.",
                "maxValueBytes": 4096,
            }),
        );
        assert_eq!(
            publication.manifest["islands"][0]["grantDigest"]
                .as_str()
                .expect("instance grant digest")
                .len(),
            64,
        );
        assert_eq!(
            artifact["effectDescriptors"][effect.id.to_string()]["handler"],
            "timer"
        );
        assert_eq!(
            artifact["effectDescriptors"][effect.id.to_string()]["ownerScope"],
            effect.owner_scope,
        );
        assert_eq!(
            artifact["effectDescriptors"][request.id.to_string()]["handler"],
            "request"
        );
        assert_eq!(
            artifact["effectDescriptors"][request.id.to_string()]["ownerScope"],
            request.owner_scope,
        );
        assert_eq!(
            artifact["effectDescriptors"][route.id.to_string()]["handler"],
            "navigation"
        );
        assert_eq!(
            artifact["effectDescriptors"][route.id.to_string()]["ownerScope"],
            route.owner_scope,
        );
        assert_eq!(
            artifact["effectDescriptors"][storage.id.to_string()]["handler"],
            "storage"
        );
        assert_eq!(
            artifact["effectDescriptors"][storage.id.to_string()]["policy"],
            storage.policy,
        );
        assert_eq!(
            artifact["subscriptionDescriptors"][subscription.id.to_string()]["handler"],
            "interval"
        );
        assert_eq!(
            artifact["subscriptionDescriptors"][subscription.id.to_string()]["ownerScope"],
            subscription.owner_scope,
        );
        let public_artifact = serde_json::to_string(artifact).expect("artifact JSON");
        assert!(!public_artifact.contains("completionSource"));
        assert!(!public_artifact.contains("completionCaptures"));
        assert!(!public_artifact.contains("Fetched"));
        assert_eq!(artifact["limits"]["maxCaptureBytes"], 64 * 1024);
        assert_eq!(artifact["limits"]["maxPendingEffects"], 128);
        assert_eq!(artifact["limits"]["maxSubscriptions"], 64);

        let executable = &publication.artifacts[0];
        let embedded = embedded_mount_grant(&executable.wasm);
        assert_eq!(embedded["grant"]["digest"], mount_grant["digest"]);
        assert_eq!(embedded["artifact"], artifact["artifact"]);
        assert_eq!(embedded["artifactGrant"], artifact["grantProjection"]);
        let (mut store, instance) = instantiate_glamour_wasm(&executable.wasm);
        let memory = instance.get_memory(&mut store, "memory").expect("memory");
        let reserve = instance
            .get_typed_func::<i32, i32>(&mut store, "__glamour_input_reserve")
            .expect("input reserve");
        let init = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_init")
            .expect("init");
        let dispatch = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_dispatch")
            .expect("dispatch");
        let output_length = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_output_length")
            .expect("output length");
        let output_release = instance
            .get_typed_func::<(), ()>(&mut store, "__glamour_output_release")
            .expect("output release");
        let app_id = artifact["appId"].as_u64().expect("app id") as u32;
        let build_id = u64::from_str_radix(
            artifact["buildId"]
                .as_str()
                .expect("build id")
                .trim_start_matches("0x"),
            16,
        )
        .expect("hex build id");
        let mut start_frame = vec![0_u8; 49];
        start_frame[..4].copy_from_slice(b"GLMR");
        start_frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
        start_frame[8] = 1;
        start_frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
        start_frame[12..16].copy_from_slice(&49_u32.to_le_bytes());
        start_frame[20..24].copy_from_slice(&app_id.to_le_bytes());
        start_frame[24..32].copy_from_slice(&build_id.to_le_bytes());
        start_frame[40..44].copy_from_slice(&48_u32.to_le_bytes());
        start_frame[48] = b'0';
        let start_pointer = reserve
            .call(&mut store, start_frame.len() as i32)
            .expect("reserve start");
        memory
            .write(&mut store, start_pointer as usize, &start_frame)
            .expect("write start");
        let output = init
            .call(&mut store, (start_pointer, start_frame.len() as i32))
            .expect("initialize work adapter");
        let output_len = output_length.call(&mut store, ()).expect("startup length") as usize;
        let startup = memory.data(&store)[output as usize..output as usize + output_len].to_vec();
        let startup_operations = glamour_output_operations(&startup);
        assert_eq!(
            startup_operations
                .iter()
                .map(|operation| glamour_operation_tag(operation))
                .collect::<Vec<_>>(),
            [256, 258],
        );
        assert_eq!(glamour_operation_u32(startup_operations[0], 16), effect.id);
        assert_eq!(
            glamour_operation_u32(startup_operations[1], 12),
            subscription.id,
        );
        output_release.call(&mut store, ()).expect("release startup");

        let mut timer_completion = vec![0_u8; 88];
        timer_completion[..4].copy_from_slice(b"GLMR");
        timer_completion[4..6].copy_from_slice(&1_u16.to_le_bytes());
        timer_completion[8] = 3;
        timer_completion[10..12].copy_from_slice(&48_u16.to_le_bytes());
        timer_completion[12..16].copy_from_slice(&88_u32.to_le_bytes());
        timer_completion[16..20].copy_from_slice(&1_u32.to_le_bytes());
        timer_completion[20..24].copy_from_slice(&app_id.to_le_bytes());
        timer_completion[24..32].copy_from_slice(&build_id.to_le_bytes());
        timer_completion[40..44].copy_from_slice(&88_u32.to_le_bytes());
        timer_completion[48..50].copy_from_slice(&1_u16.to_le_bytes());
        timer_completion[52..56].copy_from_slice(&40_u32.to_le_bytes());
        timer_completion[56..60].copy_from_slice(&1_u32.to_le_bytes());
        timer_completion[60..64].copy_from_slice(&1_u32.to_le_bytes());
        timer_completion[64..68].copy_from_slice(&1_u32.to_le_bytes());
        timer_completion[68..72].copy_from_slice(&effect.id.to_le_bytes());
        timer_completion[72..76].copy_from_slice(&effect.result_schema.to_le_bytes());
        timer_completion[80..84].copy_from_slice(&88_u32.to_le_bytes());
        let completion_pointer = reserve
            .call(&mut store, timer_completion.len() as i32)
            .expect("reserve timer completion");
        memory
            .write(&mut store, completion_pointer as usize, &timer_completion)
            .expect("write timer completion");
        let output = dispatch
            .call(
                &mut store,
                (completion_pointer, timer_completion.len() as i32),
            )
            .expect("dispatch timer completion");
        let output_len = output_length.call(&mut store, ()).expect("completion length") as usize;
        let completed = memory.data(&store)[output as usize..output as usize + output_len].to_vec();
        let completed_operations = glamour_output_operations(&completed);
        assert!(completed_operations
            .iter()
            .any(|operation| glamour_operation_tag(operation) == 2));
        assert!(completed_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 256
                && glamour_operation_u32(operation, 16) == request.id
        }));
        assert!(completed_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 256
                && glamour_operation_u32(operation, 16) == route.id
        }));
        assert!(completed_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 258
                && glamour_operation_u32(operation, 12) == subscription.id
        }));
        output_release
            .call(&mut store, ())
            .expect("release timer completion");

        let mut http_payload = vec![1, 0, 0, 0];
        http_payload.extend_from_slice(&200_u32.to_le_bytes());
        http_payload.extend_from_slice(&2_u32.to_le_bytes());
        http_payload.extend_from_slice(b"ok");
        let mut http_completion = vec![0_u8; 88 + http_payload.len()];
        http_completion[..88].copy_from_slice(&timer_completion);
        let http_length = http_completion.len() as u32;
        http_completion[12..16].copy_from_slice(&http_length.to_le_bytes());
        http_completion[32..36].copy_from_slice(&1_u32.to_le_bytes());
        http_completion[60..64].copy_from_slice(&2_u32.to_le_bytes());
        http_completion[68..72].copy_from_slice(&request.id.to_le_bytes());
        http_completion[72..76].copy_from_slice(&request.result_schema.to_le_bytes());
        http_completion[84..88].copy_from_slice(&(http_payload.len() as u32).to_le_bytes());
        http_completion[88..].copy_from_slice(&http_payload);
        let completion_pointer = reserve
            .call(&mut store, http_completion.len() as i32)
            .expect("reserve HTTP completion");
        memory
            .write(&mut store, completion_pointer as usize, &http_completion)
            .expect("write HTTP completion");
        let output = dispatch
            .call(
                &mut store,
                (completion_pointer, http_completion.len() as i32),
            )
            .expect("dispatch HTTP completion");
        let output_len = output_length.call(&mut store, ()).expect("HTTP length") as usize;
        let completed = memory.data(&store)[output as usize..output as usize + output_len].to_vec();
        let text = glamour_output_operations(&completed)
            .into_iter()
            .find(|operation| glamour_operation_tag(operation) == 2)
            .expect("captured model changes text");
        assert_eq!(
            glamour_payload_text(
                &completed,
                glamour_operation_u32(text, 12),
                glamour_operation_u32(text, 16),
            ),
            "10",
        );
        output_release
            .call(&mut store, ())
            .expect("release HTTP completion");
        http_completion[32..36].copy_from_slice(&2_u32.to_le_bytes());
        let duplicate_pointer = reserve
            .call(&mut store, http_completion.len() as i32)
            .expect("reserve duplicate completion");
        memory
            .write(&mut store, duplicate_pointer as usize, &http_completion)
            .expect("write duplicate completion");
        let duplicate = dispatch
            .call(
                &mut store,
                (duplicate_pointer, http_completion.len() as i32),
            )
            .is_err();
        assert!(duplicate, "one-shot callback environments are consumed exactly once");

        let (mut resumed_store, resumed_instance) =
            instantiate_glamour_wasm(&executable.wasm);
        let resumed_memory = resumed_instance
            .get_memory(&mut resumed_store, "memory")
            .expect("resumed memory");
        let resumed_reserve = resumed_instance
            .get_typed_func::<i32, i32>(&mut resumed_store, "__glamour_input_reserve")
            .expect("resumed input reserve");
        let resume = resumed_instance
            .get_typed_func::<(i32, i32), i32>(&mut resumed_store, "__glamour_resume")
            .expect("resume");
        let resumed_dispatch = resumed_instance
            .get_typed_func::<(i32, i32), i32>(&mut resumed_store, "__glamour_dispatch")
            .expect("resumed dispatch");
        let resumed_output_length = resumed_instance
            .get_typed_func::<(), i32>(&mut resumed_store, "__glamour_output_length")
            .expect("resumed output length");
        let mut resume_frame = start_frame;
        resume_frame[9] = 1;
        let resume_pointer = resumed_reserve
            .call(&mut resumed_store, resume_frame.len() as i32)
            .expect("reserve resume");
        resumed_memory
            .write(&mut resumed_store, resume_pointer as usize, &resume_frame)
            .expect("write resume");
        assert_eq!(
            resume
                .call(
                    &mut resumed_store,
                    (resume_pointer, resume_frame.len() as i32),
                )
                .expect("install resumed startup"),
            0,
        );
        assert_eq!(
            resumed_output_length
                .call(&mut resumed_store, ())
                .expect("resume has no output"),
            0,
        );
        let event = &checked.island_plans[0].events[0];
        let mut activation_event = vec![0_u8; 96];
        activation_event[..4].copy_from_slice(b"GLMR");
        activation_event[4..6].copy_from_slice(&1_u16.to_le_bytes());
        activation_event[8] = 2;
        activation_event[10..12].copy_from_slice(&48_u16.to_le_bytes());
        activation_event[12..16].copy_from_slice(&96_u32.to_le_bytes());
        activation_event[16..20].copy_from_slice(&1_u32.to_le_bytes());
        activation_event[20..24].copy_from_slice(&app_id.to_le_bytes());
        activation_event[24..32].copy_from_slice(&build_id.to_le_bytes());
        activation_event[40..44].copy_from_slice(&96_u32.to_le_bytes());
        activation_event[48..50].copy_from_slice(&1_u16.to_le_bytes());
        activation_event[52..56].copy_from_slice(&48_u32.to_le_bytes());
        activation_event[56..60].copy_from_slice(&event.plan.to_le_bytes());
        activation_event[60..64]
            .copy_from_slice(&checked.island_plans[0].registry_id.to_le_bytes());
        activation_event[64..68].copy_from_slice(&event.event_class.to_le_bytes());
        activation_event[72..76].copy_from_slice(&96_u32.to_le_bytes());
        activation_event[80..84].copy_from_slice(&96_u32.to_le_bytes());
        let event_pointer = resumed_reserve
            .call(&mut resumed_store, activation_event.len() as i32)
            .expect("reserve activation event");
        resumed_memory
            .write(
                &mut resumed_store,
                event_pointer as usize,
                &activation_event,
            )
            .expect("write activation event");
        let output = resumed_dispatch
            .call(
                &mut resumed_store,
                (event_pointer, activation_event.len() as i32),
            )
            .expect("dispatch activation event");
        let output_len = resumed_output_length
            .call(&mut resumed_store, ())
            .expect("activation output length") as usize;
        let activated = resumed_memory.data(&resumed_store)
            [output as usize..output as usize + output_len]
            .to_vec();
        assert_eq!(
            u64::from_le_bytes(activated[32..40].try_into().expect("output sequence")),
            1,
        );
        let work = glamour_output_operations(&activated)
            .into_iter()
            .filter(|operation| glamour_operation_tag(operation) >= 256)
            .collect::<Vec<_>>();
        assert_eq!(
            work.iter()
                .map(|operation| glamour_operation_tag(operation))
                .collect::<Vec<_>>(),
            [256, 256, 256, 258],
        );
        assert_eq!(glamour_operation_u32(work[0], 16), effect.id);
        assert_eq!(glamour_operation_u32(work[1], 16), request.id);
        assert_eq!(glamour_operation_u32(work[2], 16), route.id);
        assert_eq!(glamour_operation_u32(work[3], 12), subscription.id);
        std::fs::remove_dir_all(root).expect("cleanup");

        let dynamic_root = temp_path("static-island-dynamic-browser-policy");
        let mut dynamic_files = static_fixture_files("island-work", "island_work");
        dynamic_files.insert(
            PathBuf::from("src/island_work.witchy"),
            source
                .replacen(
                    "fn authorize(root: UiRoot) -> Auth:",
                    "fn dynamic_minimum(_root: UiRoot) -> Int:\n    10\n\nfn authorize(root: UiRoot) -> Auth:",
                    1,
                )
                .replacen(
                    "glamour.timer_scope(root, 10)",
                    "glamour.timer_scope(root, dynamic_minimum(root))",
                    1,
                ),
        );
        create_project_atomically(&dynamic_root, &dynamic_files)
            .expect("create dynamic policy fixture");
        let dynamic_entry = dynamic_root.join("src/island_work.witchy");
        let (dynamic_checked, _) = crate::link_file_checked(
            dynamic_entry.to_str().expect("UTF-8 fixture path"),
        )
        .expect("link dynamic policy fixture");
        let error = witchy_lower::codegen::checked_glamour_islands(&dynamic_checked)
            .expect_err("dynamic narrowed browser policy must fail publication");
        assert!(
            error.message.contains("bounded by compiler-visible literals"),
            "unexpected dynamic policy diagnostic: {}",
            error.message,
        );
        std::fs::remove_dir_all(dynamic_root).expect("cleanup dynamic policy fixture");
    }

    #[test]
    fn optimized_islands_fuse_cmd_and_sub_maps_into_final_callbacks() {
        let root = temp_path("static-island-mapped-work");
        let mut files = static_fixture_files("island-mapped-work", "island_mapped_work");
        files.insert(
            PathBuf::from("src/island_mapped_work.witchy"),
            r#"import reflect
from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type ChildMsg derive(Reflect):
    Tick
    Fetched(glamour.HttpResult)

type MiddleMsg derive(Reflect):
    Middle(ChildMsg)

type Msg derive(Reflect):
    Child(Int, MiddleMsg)

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn child_start(auth: UiRoot) -> Cmd(ChildMsg):
    glamour.schedule("clock", glamour.timer_scope(auth, 10), 20, Tick)

fn child_fetch(auth: UiRoot) -> Cmd(ChildMsg):
    let fetch = glamour.fetch_scope(auth, "book", "GET", "/content/")
    glamour.http_get("page", fetch, "/content/page.md", fn(result: glamour.HttpResult): Fetched(result))

fn map_child(command: Cmd(ChildMsg), offset: Int) -> Cmd(Msg):
    command
        .map(fn(message: ChildMsg): Middle(message))
        .map(fn(message: MiddleMsg): Child(offset, message))

fn start(auth: UiRoot, model: Int) -> Cmd(Msg):
    map_child(child_start(auth), model + 2)

fn update(auth: UiRoot, model: Int, message: Msg) -> (Int, Cmd(Msg)):
    match message:
        Child(offset, Middle(Tick)) -> (model + offset, map_child(child_fetch(auth), model + 5))
        Child(offset, Middle(Fetched(_result))) -> (model + offset, NoCmd)

fn view(model: Int) -> Ui(Msg):
    glamour.ui(glamour.element("button", [glamour.on_event("clock.tick", "click", glamour.event_msg(Child(1, Middle(Tick))))], [glamour.text("${model}")]))

fn child_subscriptions(auth: UiRoot) -> Sub(ChildMsg):
    glamour.every("pulse", glamour.timer_scope(auth, 10), 20, Tick)

fn subscriptions(auth: UiRoot, model: Int) -> Sub(Msg):
    child_subscriptions(auth)
        .map(fn(message: ChildMsg): Middle(message))
        .map(fn(message: MiddleMsg): Child(model + 3, message))

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, view, subscriptions)

pub fn web() -> Site:
    let clock = glamour.interactive(app(), 0).activate(glamour.OnLoad)
    glamour.site([glamour.static_page("/", glamour.ui(glamour.element("main", [], [glamour.embed(clock)])))])
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create mapped-work fixture");
        let entry = root.join("src/island_mapped_work.witchy");
        let (compiler_checked, _) = crate::link_file_checked(
            entry.to_str().expect("UTF-8 fixture path"),
        )
        .expect("link mapped-work fixture");
        let compiler_metadata = witchy_lower::codegen::checked_glamour_islands(
            &compiler_checked,
        )
        .expect("authenticate mapped-work fixture");
        let metadata = &compiler_metadata[0];
        assert_eq!(metadata.work_maps.len(), 4);
        assert_eq!(metadata.mapped_work.len(), 6);
        let mapped_effect = metadata
            .mapped_work
            .iter()
            .find(|work| work.kind == "timer" && work.composition.len() == 2)
            .expect("mapped effect callback");
        let mapped_request = metadata
            .mapped_work
            .iter()
            .find(|work| work.kind == "http" && work.composition.len() == 2)
            .expect("mapped HTTP callback");
        let mapped_subscription = metadata
            .mapped_work
            .iter()
            .find(|work| work.channel == "subscription" && work.composition.len() == 2)
            .expect("mapped subscription callback");
        let base_effect = metadata
            .work
            .iter()
            .find(|work| work.kind == "timer")
            .expect("base effect callback");
        let base_request = metadata
            .work
            .iter()
            .find(|work| work.kind == "http")
            .expect("base HTTP callback");
        let base_subscription = metadata
            .work
            .iter()
            .find(|work| work.channel == "subscription")
            .expect("base subscription callback");
        assert_eq!(mapped_effect.owner_scope_id, base_effect.owner_scope_id);
        assert_eq!(mapped_request.owner_scope_id, base_request.owner_scope_id);
        assert_eq!(mapped_effect.browser_policy, base_effect.browser_policy);
        assert_eq!(mapped_request.browser_policy, base_request.browser_policy);
        assert_eq!(
            mapped_subscription.owner_scope_id,
            base_subscription.owner_scope_id,
        );
        assert_eq!(
            mapped_subscription.browser_policy,
            base_subscription.browser_policy,
        );
        assert_eq!(mapped_effect.mapper_captures[0].name, "offset");
        assert_eq!(mapped_request.mapper_captures[0].name, "offset");
        assert_eq!(mapped_subscription.mapper_captures[0].name, "model");
        assert_ne!(mapped_effect.descriptor_id, mapped_effect.previous_descriptor_id);
        assert_ne!(mapped_effect.completion_id, mapped_effect.previous_completion_id);
        assert_ne!(
            mapped_subscription.descriptor_id,
            mapped_subscription.previous_descriptor_id,
        );
        assert_ne!(
            mapped_subscription.completion_id,
            mapped_subscription.previous_completion_id,
        );
        let execution = witchy_lower::codegen::checked_glamour_island_execution_module(
            &compiler_checked,
            metadata,
        )
        .expect("rewrite mapped-work fixture");
        let rewritten = execution
            .items
            .iter()
            .filter_map(|item| match item {
                witchy_syntax::ast::Item::Function(function)
                    if function.name.ends_with(".start")
                        || function.name.ends_with(".map_child")
                        || function.name.ends_with(".subscriptions") =>
                {
                    Some(witchy_syntax::format::block_str(&function.body))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rewritten.contains("glamour_island_capture_cmd_map_"));
        assert!(rewritten.contains("glamour_island_capture_sub_map_"));

        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("compile mapped Cmd/Sub callbacks");
        let plan = &checked.island_plans[0];
        assert!(plan
            .effect_descriptors
            .iter()
            .any(|descriptor| descriptor.id == mapped_effect.descriptor_id));
        assert!(plan
            .effect_descriptors
            .iter()
            .any(|descriptor| descriptor.id == mapped_request.descriptor_id));
        assert!(plan
            .subscription_descriptors
            .iter()
            .any(|descriptor| descriptor.id == mapped_subscription.descriptor_id));
        let publication = checked
            .island_publication
            .as_ref()
            .expect("mapped-work publication");
        let artifact = &publication.artifact_manifest["artifacts"][0];
        let public_artifact = serde_json::to_string(artifact).expect("mapped artifact JSON");
        assert!(!public_artifact.contains("mapperSource"));
        assert!(!public_artifact.contains("mapperCaptures"));
        assert!(!public_artifact.contains("offset"));
        let executable = &publication.artifacts[0];
        let (mut store, instance) = instantiate_glamour_wasm(&executable.wasm);
        let memory = instance.get_memory(&mut store, "memory").expect("memory");
        let reserve = instance
            .get_typed_func::<i32, i32>(&mut store, "__glamour_input_reserve")
            .expect("input reserve");
        let init = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_init")
            .expect("init");
        let dispatch = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_dispatch")
            .expect("dispatch");
        let output_length = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_output_length")
            .expect("output length");
        let output_release = instance
            .get_typed_func::<(), ()>(&mut store, "__glamour_output_release")
            .expect("output release");
        let app_id = artifact["appId"].as_u64().expect("app id") as u32;
        let build_id = u64::from_str_radix(
            artifact["buildId"]
                .as_str()
                .expect("build id")
                .trim_start_matches("0x"),
            16,
        )
        .expect("hex build id");
        let mut start_frame = vec![0_u8; 49];
        start_frame[..4].copy_from_slice(b"GLMR");
        start_frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
        start_frame[8] = 1;
        start_frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
        start_frame[12..16].copy_from_slice(&49_u32.to_le_bytes());
        start_frame[20..24].copy_from_slice(&app_id.to_le_bytes());
        start_frame[24..32].copy_from_slice(&build_id.to_le_bytes());
        start_frame[40..44].copy_from_slice(&48_u32.to_le_bytes());
        start_frame[48] = b'0';
        let start_pointer = reserve
            .call(&mut store, start_frame.len() as i32)
            .expect("reserve start");
        memory
            .write(&mut store, start_pointer as usize, &start_frame)
            .expect("write start");
        let output = init
            .call(&mut store, (start_pointer, start_frame.len() as i32))
            .expect("initialize mapped-work adapter");
        let output_len = output_length.call(&mut store, ()).expect("startup length") as usize;
        let startup = memory.data(&store)[output as usize..output as usize + output_len].to_vec();
        let startup_operations = glamour_output_operations(&startup);
        let effect = startup_operations
            .iter()
            .find(|operation| glamour_operation_tag(operation) == 256)
            .expect("mapped startup effect");
        let subscription = startup_operations
            .iter()
            .find(|operation| glamour_operation_tag(operation) == 258)
            .expect("mapped startup subscription");
        assert_eq!(glamour_operation_u32(effect, 16), mapped_effect.descriptor_id);
        assert_eq!(
            glamour_operation_u32(subscription, 12),
            mapped_subscription.descriptor_id,
        );
        output_release.call(&mut store, ()).expect("release startup");

        let mut completion = vec![0_u8; 88];
        completion[..4].copy_from_slice(b"GLMR");
        completion[4..6].copy_from_slice(&1_u16.to_le_bytes());
        completion[8] = 3;
        completion[10..12].copy_from_slice(&48_u16.to_le_bytes());
        completion[12..16].copy_from_slice(&88_u32.to_le_bytes());
        completion[16..20].copy_from_slice(&1_u32.to_le_bytes());
        completion[20..24].copy_from_slice(&app_id.to_le_bytes());
        completion[24..32].copy_from_slice(&build_id.to_le_bytes());
        completion[40..44].copy_from_slice(&88_u32.to_le_bytes());
        completion[48..50].copy_from_slice(&1_u16.to_le_bytes());
        completion[52..56].copy_from_slice(&40_u32.to_le_bytes());
        completion[56..60].copy_from_slice(&1_u32.to_le_bytes());
        completion[60..64].copy_from_slice(&1_u32.to_le_bytes());
        completion[64..68].copy_from_slice(&1_u32.to_le_bytes());
        completion[68..72].copy_from_slice(&mapped_effect.descriptor_id.to_le_bytes());
        completion[72..76].copy_from_slice(&mapped_effect.result_schema_id.to_le_bytes());
        completion[80..84].copy_from_slice(&88_u32.to_le_bytes());
        let completion_pointer = reserve
            .call(&mut store, completion.len() as i32)
            .expect("reserve completion");
        memory
            .write(&mut store, completion_pointer as usize, &completion)
            .expect("write completion");
        let output = dispatch
            .call(&mut store, (completion_pointer, completion.len() as i32))
            .expect("dispatch mapped completion");
        let output_len = output_length.call(&mut store, ()).expect("completion length") as usize;
        let completed = memory.data(&store)[output as usize..output as usize + output_len].to_vec();
        let completed_operations = glamour_output_operations(&completed);
        let text = completed_operations
            .iter()
            .find(|operation| glamour_operation_tag(operation) == 2)
            .expect("mapped completion changes text");
        assert_eq!(
            glamour_payload_text(
                &completed,
                glamour_operation_u32(text, 12),
                glamour_operation_u32(text, 16),
            ),
            "2",
        );
        assert!(completed_operations.iter().any(|operation| {
            glamour_operation_tag(operation) == 256
                && glamour_operation_u32(operation, 16) == mapped_request.descriptor_id
        }));
        output_release
            .call(&mut store, ())
            .expect("release mapped completion");

        let mut http_payload = vec![1, 0, 0, 0];
        http_payload.extend_from_slice(&200_u32.to_le_bytes());
        http_payload.extend_from_slice(&2_u32.to_le_bytes());
        http_payload.extend_from_slice(b"ok");
        let mut http_completion = vec![0_u8; 88 + http_payload.len()];
        http_completion[..88].copy_from_slice(&completion);
        let http_length = http_completion.len() as u32;
        http_completion[12..16].copy_from_slice(&http_length.to_le_bytes());
        http_completion[32..36].copy_from_slice(&1_u32.to_le_bytes());
        http_completion[60..64].copy_from_slice(&2_u32.to_le_bytes());
        http_completion[68..72].copy_from_slice(&mapped_request.descriptor_id.to_le_bytes());
        http_completion[72..76]
            .copy_from_slice(&mapped_request.result_schema_id.to_le_bytes());
        http_completion[84..88].copy_from_slice(&(http_payload.len() as u32).to_le_bytes());
        http_completion[88..].copy_from_slice(&http_payload);
        let completion_pointer = reserve
            .call(&mut store, http_completion.len() as i32)
            .expect("reserve mapped HTTP completion");
        memory
            .write(&mut store, completion_pointer as usize, &http_completion)
            .expect("write mapped HTTP completion");
        let output = dispatch
            .call(
                &mut store,
                (completion_pointer, http_completion.len() as i32),
            )
            .expect("dispatch mapped HTTP completion");
        let output_len = output_length
            .call(&mut store, ())
            .expect("mapped HTTP completion length") as usize;
        let completed =
            memory.data(&store)[output as usize..output as usize + output_len].to_vec();
        let text = glamour_output_operations(&completed)
            .into_iter()
            .find(|operation| glamour_operation_tag(operation) == 2)
            .expect("mapped HTTP completion changes text");
        assert_eq!(
            glamour_payload_text(
                &completed,
                glamour_operation_u32(text, 12),
                glamour_operation_u32(text, 16),
            ),
            "7",
        );
        output_release
            .call(&mut store, ())
            .expect("release mapped HTTP completion");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn optimized_island_maps_reject_dynamic_and_capability_bearing_mappers() {
        let dynamic_root = temp_path("static-island-dynamic-map");
        let mut dynamic_files = static_fixture_files("island-dynamic-map", "island_dynamic_map");
        dynamic_files.insert(
            PathBuf::from("src/island_dynamic_map.witchy"),
            r#"import reflect
from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type ChildMsg derive(Reflect):
    Tick

type Msg derive(Reflect):
    Child(ChildMsg)

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn first(message: ChildMsg) -> Msg:
    Child(message)

fn second(message: ChildMsg) -> Msg:
    Child(message)

fn start(auth: UiRoot, model: Int) -> Cmd(Msg):
    let mapper: fn(ChildMsg) -> Msg = if model == 0: first else: second
    glamour.schedule("clock", glamour.timer_scope(auth, 10), 20, Tick).map(mapper)

fn update(_auth: UiRoot, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model, NoCmd)

fn view(model: Int) -> Ui(Msg):
    glamour.ui(glamour.text("${model}"))

fn subscriptions(_auth: UiRoot, _model: Int) -> Sub(Msg):
    NoSub

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, view, subscriptions)

pub fn web() -> Site:
    let clock = glamour.interactive(app(), 0).activate(glamour.OnLoad)
    glamour.site([glamour.static_page("/", glamour.ui(glamour.embed(clock)))])
"#
            .into(),
        );
        create_project_atomically(&dynamic_root, &dynamic_files)
            .expect("create dynamic-map fixture");
        let dynamic_entry = dynamic_root.join("src/island_dynamic_map.witchy");
        let (dynamic_checked, _) = crate::link_file_checked(
            dynamic_entry.to_str().expect("UTF-8 fixture path"),
        )
        .expect("link dynamic-map fixture");
        let dynamic_error = witchy_lower::codegen::checked_glamour_islands(&dynamic_checked)
            .expect_err("dynamically selected mapper must be rejected");
        assert!(dynamic_error.message.contains("dynamically selected mapper"));
        std::fs::remove_dir_all(dynamic_root).expect("cleanup dynamic-map fixture");

        let capability_root = temp_path("static-island-capability-map");
        let mut capability_files =
            static_fixture_files("island-capability-map", "island_capability_map");
        capability_files.insert(
            PathBuf::from("src/island_capability_map.witchy"),
            r#"import reflect
from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type ChildMsg derive(Reflect):
    Tick

type Msg derive(Reflect):
    Child(ChildMsg)

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn retain(_fetch: glamour.UiFetch, message: ChildMsg) -> Msg:
    Child(message)

fn start(auth: UiRoot, _model: Int) -> Cmd(Msg):
    let fetch = glamour.fetch_scope(auth, "book", "GET", "/content/")
    glamour.schedule("clock", glamour.timer_scope(auth, 10), 20, Tick)
        .map(fn(message: ChildMsg): retain(fetch, message))

fn update(_auth: UiRoot, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model, NoCmd)

fn view(model: Int) -> Ui(Msg):
    glamour.ui(glamour.text("${model}"))

fn subscriptions(_auth: UiRoot, _model: Int) -> Sub(Msg):
    NoSub

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, view, subscriptions)

pub fn web() -> Site:
    let clock = glamour.interactive(app(), 0).activate(glamour.OnLoad)
    glamour.site([glamour.static_page("/", glamour.ui(glamour.embed(clock)))])
"#
            .into(),
        );
        create_project_atomically(&capability_root, &capability_files)
            .expect("create capability-map fixture");
        let capability_entry = capability_root.join("src/island_capability_map.witchy");
        let (capability_checked, _) = crate::link_file_checked(
            capability_entry.to_str().expect("UTF-8 fixture path"),
        )
        .expect("link capability-map fixture");
        let capability_error = witchy_lower::codegen::checked_glamour_islands(
            &capability_checked,
        )
        .expect_err("capability-bearing mapper must be rejected");
        assert!(capability_error.message.contains("cannot persist capture `fetch`"));
        assert!(capability_error.message.contains("capability"));
        std::fs::remove_dir_all(capability_root).expect("cleanup capability-map fixture");
    }

    #[test]
    fn static_island_completion_environments_reject_capability_captures() {
        let root = temp_path("static-island-capability-capture");
        let mut files = static_fixture_files("island-capture", "island_capture");
        let source = r#"import reflect
from glamour import Cmd, IslandPlan, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Fetched(glamour.HttpResult)

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn start(_auth: UiRoot, _model: Int) -> Cmd(Msg):
    NoCmd

fn finish(_fetch: glamour.UiFetch, result: glamour.HttpResult) -> Msg:
    Fetched(result)

fn update(auth: UiRoot, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    let fetch = glamour.fetch_scope(auth, "book", "GET", "/content/")
    (model, glamour.http_get("page", fetch, "/content/page.md", fn(result: glamour.HttpResult): finish(fetch, result)))

fn render(model: Int) -> Ui(Msg):
    glamour.ui(glamour.text("${model}"))

fn subscriptions(_auth: UiRoot, _model: Int) -> Sub(Msg):
    NoSub

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let page = glamour.island("page", app(), 0, static_view, glamour.OnLoad)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(page)))]),
        [page],
    )
"#;
        files.insert(PathBuf::from("src/island_capture.witchy"), source.into());
        create_project_atomically(&root, &files).expect("create capture fixture");
        let entry = root.join("src/island_capture.witchy");
        let (checked, _) = crate::link_file_checked(
            entry.to_str().expect("UTF-8 fixture path"),
        )
        .expect("link capture fixture");
        let error = witchy_lower::codegen::checked_glamour_islands(&checked)
            .expect_err("capability capture must be rejected");
        assert!(error.message.contains("cannot persist capture `fetch`"));
        assert!(error.message.contains("capability"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn typed_island_plan_erases_only_after_program_model_and_view_agree() {
        let root = temp_path("static-island-plan");
        let mut files = static_fixture_files("island-check", "island_check");
        let source = r#"import reflect
from glamour import Cmd, IslandPlan, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Increment

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn start(_auth: UiRoot, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: UiRoot, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn render(model: Int) -> Ui(Msg):
    let event = if model == 11: "keydown" else: "click"
    let items = if model == 7:
        [glamour.keyed("a", glamour.element("li", [], [glamour.text("A")])), glamour.keyed("b", glamour.element("li", [], [glamour.text("B")])), glamour.keyed("c", glamour.element("li", [], [glamour.text("C")]))]
    else if model == 8:
        [glamour.keyed("c", glamour.element("li", [], [glamour.text("C")])), glamour.keyed("a", glamour.element("li", [], [glamour.text("A")])), glamour.keyed("b", glamour.element("li", [], [glamour.text("B")]))]
    else if model == 10:
        [glamour.keyed("new-${model}", glamour.element("li", [], [glamour.text("B-${model}")])), glamour.keyed("c", glamour.element("li", [], [glamour.text("C")])), glamour.keyed("a", glamour.element("li", [], [glamour.text("A")]))]
    else:
        [glamour.keyed("c", glamour.element("li", [], [glamour.text("C")])), glamour.keyed("a", glamour.element("li", [], [glamour.text("A")]))]
    glamour.ui(glamour.element("div", [], [glamour.element("button", [glamour.attribute("title", "count-${model}"), glamour.boolean_attribute("disabled", model == 7), glamour.property("value", "${model}"), glamour.static_url_attribute("href", "/${model}"), glamour.static_class_attribute(["count-${model}"]), glamour.aria_attribute("aria-label", "${model}"), glamour.on_event("counter.increment", event, glamour.prevent_default(glamour.event_msg(Increment)))], [glamour.text("${model}")]), glamour.element("ul", [], items)]))

fn subscriptions(_auth: UiRoot, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let counter = glamour.island("counter", app(), 7, static_view, glamour.OnInteraction)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(counter)))]),
        [counter],
    )
"#;
        files.insert(PathBuf::from("src/island_check.witchy"), source.into());
        create_project_atomically(&root, &files).expect("create");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("typed island declaration");
        assert_eq!(checked.islands.len(), 1);
        assert_eq!(checked.islands[0].key, "counter");
        assert_eq!(checked.islands[0].activation, "interaction");
        assert_eq!(checked.islands[0].mode, "resume");
        assert_eq!(checked.islands[0].state.as_deref(), Some("7"));
        assert_eq!(
            checked.islands[0].html,
            "<div><button title=\"count-7\" disabled=\"\" value=\"7\" href=\"/7\" class=\"count-7\" aria-label=\"7\" data-glamour-events=\"counter.increment\">7</button><ul><li>A</li><li>B</li><li>C</li></ul></div>"
        );
        assert_eq!(
            serde_json::to_value(&checked.islands[0].resume).expect("resume graph JSON"),
            serde_json::json!({
                "kind": "element",
                "tag": "div",
                "attributes": [],
                "events": [],
                "children": [{
                    "kind": "element",
                    "tag": "button",
                    "attributes": [
                        {"kind": "attribute", "name": "title"},
                        {"kind": "boolean", "name": "disabled"},
                        {"kind": "property", "name": "value"},
                        {"kind": "url", "name": "href"},
                        {"kind": "class", "name": "class"},
                        {"kind": "aria", "name": "aria-label"},
                    ],
                    "events": [{
                        "id": "counter.increment",
                        "event": "click",
                        "kind": "msg",
                        "preventDefault": true,
                        "stopPropagation": false,
                    }],
                    "children": [{"kind": "text"}],
                }, {
                    "kind": "element",
                    "tag": "ul",
                    "attributes": [],
                    "events": [],
                    "children": [
                        {"kind": "keyed", "key": "a", "child": {"kind": "element", "tag": "li", "attributes": [], "events": [], "children": [{"kind": "text"}]}},
                        {"kind": "keyed", "key": "b", "child": {"kind": "element", "tag": "li", "attributes": [], "events": [], "children": [{"kind": "text"}]}},
                        {"kind": "keyed", "key": "c", "child": {"kind": "element", "tag": "li", "attributes": [], "events": [], "children": [{"kind": "text"}]}},
                    ],
                }],
            })
        );
        assert_eq!(checked.island_plans.len(), 1);
        assert_eq!(checked.island_plans[0].key, "counter");
        assert_ne!(checked.island_plans[0].registry_id, 0);
        assert_ne!(checked.island_plans[0].registry_id, checked.island_plans[0].wire_id);
        assert!(checked.island_plans[0]
            .artifact
            .starts_with("glamour-island1-"));
        assert_eq!(checked.island_plans[0].auth_type, "glamour.UiRoot");
        assert_eq!(checked.island_plans[0].model_type, "Int");
        assert!(checked.island_plans[0].message_type.ends_with("Msg"));
        assert_eq!(
            checked.island_plans[0]
                .nodes
                .iter()
                .map(|node| node.path.clone())
                .collect::<Vec<_>>(),
            vec![
                vec![0],
                vec![0, 0],
                vec![0, 0, 0],
                vec![0, 1],
                vec![0, 1, 0],
                vec![0, 1, 0, 0],
                vec![0, 1, 1],
                vec![0, 1, 1, 0],
                vec![0, 1, 2],
                vec![0, 1, 2, 0],
            ],
        );
        assert_eq!(checked.island_plans[0].events.len(), 1);
        assert_eq!(checked.island_plans[0].attributes.len(), 6);
        assert_eq!(checked.island_plans[0].attributes[4].id, 0);
        assert_eq!(checked.island_plans[0].attributes[4].kind, "class");
        assert!(checked.island_plans[0]
            .attributes
            .iter()
            .enumerate()
            .all(|(index, attribute)| index == 4 || attribute.id != 0));
        assert_eq!(checked.island_plans[0].events[0].id, "counter.increment");
        assert_eq!(checked.island_plans[0].events[0].name, "click");
        assert!(checked.island_plans[0].events[0].prevent_default);
        assert_ne!(checked.island_plans[0].events[0].node, 0);
        assert_ne!(checked.island_plans[0].events[0].plan, 0);
        assert_ne!(checked.island_plans[0].events[0].event_class, 0);
        assert_eq!(
            checked.island_plans[0].html,
            format!(
                "<div><button title=\"count-7\" disabled=\"\" value=\"7\" href=\"/7\" class=\"count-7\" aria-label=\"7\" data-glamour-node=\"{}\">7</button><ul><li>A</li><li>B</li><li>C</li></ul></div>",
                checked.island_plans[0].events[0].node,
            )
        );
        assert_eq!(checked.island_plans[0].regions.len(), 1);
        assert_eq!(checked.island_plans[0].regions[0].keys.len(), 3);
        assert!(checked.island_plans[0].regions[0]
            .keys
            .iter()
            .all(|key| key.template != 0));
        let dynamic = checked.island_plans[0].regions[0]
            .dynamic
            .as_ref()
            .expect("homogeneous keyed region has a dynamic prototype");
        assert_eq!(dynamic.source, "a");
        assert_eq!(dynamic.template, checked.island_plans[0].regions[0].keys[0].template);
        assert_eq!(checked.island_plans[0].regions[0].path, vec![0, 1]);
        assert_eq!(
            checked.island_plans[0].regions[0]
                .keys
                .iter()
                .map(|key| key.source.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"],
        );
        let publication = checked
            .island_publication
            .as_ref()
            .expect("checked island publication");
        assert_eq!(publication.build_identity.len(), 64);
        assert_eq!(
            publication.manifest["schema"],
            "witchy.glamour.islands.v1"
        );
        assert_eq!(
            publication.artifact_manifest["schema"],
            "witchy.glamour.island-artifacts.v1"
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["programTypes"]["model"],
            "Int"
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["properties"]
                .as_array()
                .expect("property registry")
                .len(),
            1
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["attributes"]
                .as_array()
                .expect("attribute registry")
                .len(),
            3
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["aria"]
                .as_array()
                .expect("ARIA registry")
                .len(),
            1
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["regions"][0]["id"],
            checked.island_plans[0].regions[0].id,
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["registryId"],
            checked.island_plans[0].registry_id,
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["eventPlans"][0]["instance"],
            checked.island_plans[0].registry_id,
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["regions"][0]["dynamicTemplate"],
            dynamic.template,
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["resume"]["regions"][0]["keys"]
                .as_array()
                .expect("resumed keyed entries")
                .len(),
            3,
        );
        assert_eq!(
            publication.artifact_manifest["artifacts"][0]["resume"]["regions"][0]["keys"][0]["source"],
            "a",
        );
        let templates = publication.artifact_manifest["artifacts"][0]["templates"]
            .as_array()
            .expect("authenticated insertion templates");
        assert_eq!(templates.len(), 3);
        assert_eq!(
            templates[1]["id"],
            checked.island_plans[0].regions[0].keys[1].template,
        );
        assert_eq!(templates[1]["root"]["kind"], "element");
        assert_eq!(templates[1]["root"]["tag"], "li");
        assert_eq!(templates[1]["root"]["children"][0]["text"], "B");
        assert_eq!(templates[1]["slots"].as_array().expect("template slots").len(), 1);
        assert_eq!(
            templates[1]["slots"][0]["id"],
            checked.island_plans[0].regions[0].keys[1].slots[0].id,
        );
        assert_eq!(templates[1]["slots"][0]["kind"], "text");
        let published_island = &publication.manifest["islands"][0];
        assert!(published_island["id"]
            .as_str()
            .expect("instance id")
            .starts_with("glamour-instance1-"));
        assert_eq!(
            published_island["events"][0]["node"],
            checked.island_plans[0].events[0].node,
        );
        let published_page = publication.pages.get("/").expect("rewritten route");
        assert!(published_page.contains("data-glamour-island=\"glamour-instance1-"));
        assert!(published_page.contains(&format!(
            "data-glamour-node=\"{}\"",
            checked.island_plans[0].events[0].node,
        )));
        assert!(!published_page.contains("data-glamour-island-key"));
        assert!(!published_page.contains("data-glamour-events"));
        assert_eq!(publication.artifacts.len(), 1);
        let artifact = &publication.artifacts[0];
        assert_ne!(artifact.identity, checked.island_plans[0].artifact);
        assert_eq!(
            artifact.identity,
            publication.artifact_manifest["artifacts"][0]["artifact"],
        );
        assert!(artifact.file.starts_with("island-"));
        assert!(artifact.file.ends_with(".wasm"));

        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("Wasm GC engine");
        let module = wasmtime::Module::new(&engine, &artifact.wasm).expect("island Wasm");
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap("witchy", "fill_pending", |_pointer: i32| {})
            .expect("fill_pending");
        linker
            .func_wrap(
                "witchy",
                "user_cap_field_len",
                |_capability: i32, _field: i32| -> i32 { 0 },
            )
            .expect("user_cap_field_len");
        linker
            .func_wrap(
                "witchy",
                "__witchy_abort",
                |_template: i32, _first: i64, _second: i64, _text: i32| -> wasmtime::Result<()> {
                    Err(wasmtime::Error::msg("Witchy guest aborted"))
                },
            )
            .expect("abort");
        linker
            .func_wrap(
                "witchy",
                "encoding",
                |mut caller: wasmtime::Caller<'_, ()>, _operation: i32, input: i32, output: i32| -> wasmtime::Result<i32> {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .ok_or_else(|| wasmtime::Error::msg("missing memory"))?;
                    let data = memory.data(&caller);
                    let start = input as usize;
                    let length = u32::from_le_bytes(
                        data.get(start..start + 4)
                            .ok_or_else(|| wasmtime::Error::msg("input header"))?
                            .try_into()
                            .expect("four-byte header"),
                    ) as usize;
                    let bytes = data
                        .get(start + 4..start + 4 + length)
                        .ok_or_else(|| wasmtime::Error::msg("input bytes"))?
                        .to_vec();
                    memory.write(&mut caller, output as usize, &bytes)?;
                    Ok(length as i32)
                },
            )
            .expect("encoding");
        linker
            .func_wrap(
                "witchy",
                "string_from_code",
                |_codepoint: i64, _output: i32| -> i32 { 0 },
            )
            .expect("string_from_code");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module).expect("island instance");
        let memory = instance.get_memory(&mut store, "memory").expect("memory");
        let reserve = instance
            .get_typed_func::<i32, i32>(&mut store, "__glamour_input_reserve")
            .expect("input reserve");
        let resume = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_resume")
            .expect("resume");
        let dispatch = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_dispatch")
            .expect("dispatch");
        let output_length = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_output_length")
            .expect("output length");
        let output_release = instance
            .get_typed_func::<(), ()>(&mut store, "__glamour_output_release")
            .expect("output release");
        let artifact_record = &publication.artifact_manifest["artifacts"][0];
        let app_id = artifact_record["appId"].as_u64().expect("app id") as u32;
        let build_id = u64::from_str_radix(
            artifact_record["buildId"]
                .as_str()
                .expect("build id")
                .trim_start_matches("0x"),
            16,
        )
        .expect("hex build id");
        let mut state = vec![0_u8; 49];
        state[..4].copy_from_slice(b"GLMR");
        state[4..6].copy_from_slice(&1_u16.to_le_bytes());
        state[8] = 1;
        state[10..12].copy_from_slice(&48_u16.to_le_bytes());
        state[12..16].copy_from_slice(&49_u32.to_le_bytes());
        state[20..24].copy_from_slice(&app_id.to_le_bytes());
        state[24..32].copy_from_slice(&build_id.to_le_bytes());
        state[40..44].copy_from_slice(&48_u32.to_le_bytes());
        state[48] = b'7';
        let state_pointer = reserve.call(&mut store, state.len() as i32).expect("reserve state");
        memory
            .write(&mut store, state_pointer as usize, &state)
            .expect("write state");
        assert_eq!(
            resume
                .call(&mut store, (state_pointer, state.len() as i32))
                .expect("resume public state"),
            0
        );
        assert_eq!(output_length.call(&mut store, ()).expect("resume output"), 0);
        let event = &checked.island_plans[0].events[0];
        let mut frame = vec![0_u8; 96];
        frame[..4].copy_from_slice(b"GLMR");
        frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
        frame[8] = 2;
        frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
        frame[12..16].copy_from_slice(&96_u32.to_le_bytes());
        frame[16..20].copy_from_slice(&1_u32.to_le_bytes());
        frame[20..24].copy_from_slice(&app_id.to_le_bytes());
        frame[24..32].copy_from_slice(&build_id.to_le_bytes());
        frame[40..44].copy_from_slice(&96_u32.to_le_bytes());
        frame[48..50].copy_from_slice(&1_u16.to_le_bytes());
        frame[52..56].copy_from_slice(&48_u32.to_le_bytes());
        frame[56..60].copy_from_slice(&event.plan.to_le_bytes());
        frame[60..64].copy_from_slice(&checked.island_plans[0].registry_id.to_le_bytes());
        frame[64..68].copy_from_slice(&event.event_class.to_le_bytes());
        frame[72..76].copy_from_slice(&96_u32.to_le_bytes());
        frame[80..84].copy_from_slice(&96_u32.to_le_bytes());
        let event_pointer = reserve.call(&mut store, frame.len() as i32).expect("reserve event");
        memory
            .write(&mut store, event_pointer as usize, &frame)
            .expect("write event");
        let output = dispatch
            .call(&mut store, (event_pointer, frame.len() as i32))
            .expect("dispatch typed event");
        let output_len = output_length.call(&mut store, ()).expect("patch length") as usize;
        let patch = memory
            .data(&store)
            .get(output as usize..output as usize + output_len)
            .expect("patch bounds");
        assert_eq!(&patch[..4], b"GLMR");
        assert_eq!(u16::from_le_bytes(patch[6..8].try_into().unwrap()), 3);
        assert_eq!(patch[8], 17);
        assert_eq!(u32::from_le_bytes(patch[16..20].try_into().unwrap()), 8);
        assert_eq!(u64::from_le_bytes(patch[32..40].try_into().unwrap()), 1);
        let operations = glamour_output_operations(patch);
        assert_eq!(
            operations.iter().map(|operation| glamour_operation_tag(operation)).collect::<Vec<_>>(),
            vec![4, 6, 3, 4, 16, 17, 20, 2],
        );
        assert_eq!(
            glamour_operation_u32(operations[6], 8),
            checked.island_plans[0].regions[0].id,
        );
        assert_eq!(
            glamour_payload_text(
                patch,
                glamour_operation_u32(operations[6], 12),
                glamour_operation_u32(operations[6], 16),
            ),
            "c",
        );
        assert_eq!(
            glamour_payload_text(
                patch,
                glamour_operation_u32(operations[6], 20),
                glamour_operation_u32(operations[6], 24),
            ),
            "a",
        );
        assert_eq!(
            glamour_operation_u32(operations[7], 8),
            checked.island_plans[0].text_nodes[0].id,
        );
        assert_eq!(
            glamour_operation_u32(operations[0], 12),
            checked.island_plans[0].attributes[0].id,
        );
        assert_eq!(
            glamour_operation_u32(operations[1], 12),
            checked.island_plans[0].attributes[1].id,
        );
        assert_eq!(
            glamour_operation_u32(operations[2], 12),
            checked.island_plans[0].attributes[2].id,
        );
        assert_eq!(
            glamour_operation_u32(operations[3], 12),
            checked.island_plans[0].attributes[3].id,
        );
        assert_eq!(
            glamour_operation_u32(operations[5], 12),
            checked.island_plans[0].attributes[5].id,
        );
        let payload_offset = u32::from_le_bytes(patch[40..44].try_into().unwrap()) as usize;
        assert_eq!(
            std::str::from_utf8(&patch[payload_offset..]).expect("patch payload UTF-8"),
            "count-88/8count-88ca8"
        );
        assert_eq!(patch.last(), Some(&b'8'));
        output_release.call(&mut store, ()).expect("release move patch");

        frame[32..40].copy_from_slice(&1_u64.to_le_bytes());
        let removal_pointer = reserve
            .call(&mut store, frame.len() as i32)
            .expect("reserve removal event");
        memory
            .write(&mut store, removal_pointer as usize, &frame)
            .expect("write removal event");
        let removal_output = dispatch
            .call(&mut store, (removal_pointer, frame.len() as i32))
            .expect("dispatch keyed removal");
        let removal_len = output_length.call(&mut store, ()).expect("removal patch length") as usize;
        let removal = memory
            .data(&store)
            .get(removal_output as usize..removal_output as usize + removal_len)
            .expect("removal patch bounds");
        assert_eq!(u32::from_le_bytes(removal[16..20].try_into().unwrap()), 7);
        let removal_operations = glamour_output_operations(removal);
        assert_eq!(
            removal_operations
                .iter()
                .map(|operation| glamour_operation_tag(operation))
                .collect::<Vec<_>>(),
            vec![4, 3, 4, 16, 17, 21, 2],
        );
        assert_eq!(
            glamour_operation_u32(removal_operations[5], 8),
            checked.island_plans[0].regions[0].id,
        );
        assert_eq!(
            glamour_payload_text(
                removal,
                glamour_operation_u32(removal_operations[5], 12),
                glamour_operation_u32(removal_operations[5], 16),
            ),
            "b",
        );
        output_release.call(&mut store, ()).expect("release removal patch");

        frame[32..40].copy_from_slice(&2_u64.to_le_bytes());
        let insertion_pointer = reserve
            .call(&mut store, frame.len() as i32)
            .expect("reserve insertion event");
        memory
            .write(&mut store, insertion_pointer as usize, &frame)
            .expect("write insertion event");
        let insertion_output = dispatch
            .call(&mut store, (insertion_pointer, frame.len() as i32))
            .expect("dispatch keyed insertion");
        let insertion_len = output_length
            .call(&mut store, ())
            .expect("insertion patch length") as usize;
        let insertion = memory
            .data(&store)
            .get(insertion_output as usize..insertion_output as usize + insertion_len)
            .expect("insertion patch bounds");
        let insertion_operations = glamour_output_operations(insertion);
        assert_eq!(
            insertion_operations
                .iter()
                .map(|operation| glamour_operation_tag(operation))
                .collect::<Vec<_>>(),
            vec![4, 3, 4, 16, 17, 19, 2],
        );
        let insertion_operation = insertion_operations[5];
        let dynamic = checked.island_plans[0].regions[0]
            .dynamic
            .as_ref()
            .expect("homogeneous keyed region has a dynamic prototype");
        assert_eq!(
            glamour_operation_u32(insertion_operation, 8),
            checked.island_plans[0].regions[0].id,
        );
        assert_eq!(
            glamour_payload_text(
                insertion,
                glamour_operation_u32(insertion_operation, 12),
                glamour_operation_u32(insertion_operation, 16),
            ),
            "new-10",
        );
        assert_eq!(
            glamour_payload_text(
                insertion,
                glamour_operation_u32(insertion_operation, 20),
                glamour_operation_u32(insertion_operation, 24),
            ),
            "c",
        );
        assert_eq!(glamour_operation_u32(insertion_operation, 28), dynamic.template);
        assert_eq!(glamour_operation_u32(insertion_operation, 32), 1);
        assert_eq!(
            glamour_operation_u32(insertion_operation, 36),
            dynamic.slots[0].id,
        );
        assert_eq!(
            glamour_payload_text(
                insertion,
                glamour_operation_u32(insertion_operation, 40),
                glamour_operation_u32(insertion_operation, 44),
            ),
            "B-10",
        );
        let insertion_payload_offset =
            u32::from_le_bytes(insertion[40..44].try_into().unwrap()) as usize;
        assert_eq!(
            std::str::from_utf8(&insertion[insertion_payload_offset..])
                .expect("insertion payload UTF-8"),
            "count-1010/10count-1010new-10cB-1010"
        );
        output_release
            .call(&mut store, ())
            .expect("release insertion patch");

        frame[32..40].copy_from_slice(&3_u64.to_le_bytes());
        let invalid_pointer = reserve
            .call(&mut store, frame.len() as i32)
            .expect("reserve metadata-drift event");
        memory
            .write(&mut store, invalid_pointer as usize, &frame)
            .expect("write metadata-drift event");
        let error = dispatch
            .call(&mut store, (invalid_pointer, frame.len() as i32))
            .expect_err("live event metadata drift must abort before update");
        let backtrace = error.to_string();
        assert!(backtrace.contains("find_island_decoder"), "{backtrace}");
        assert!(backtrace.contains("island_validate_events"), "{backtrace}");

        let output = root.join("dist");
        write_static_production(&checked, &output)
            .expect("publish the atomically audited island graph");
        assert!(output.join("witchy-islands-manifest.json").is_file());
        assert!(output.join("witchy-island-artifacts.json").is_file());
        let published = std::fs::read_to_string(output.join("index.html"))
            .expect("published island page");
        assert_eq!(
            published
                .matches(" data-witchy-islands data-witchy-islands-manifest=")
                .count(),
            1,
        );
        assert!(published.contains("data-glamour-island="));
        static_site::audit_static_island_artifacts(&output, &checked)
            .expect("re-audit published island graph");

        let int_artifact = checked.island_plans[0].artifact.clone();
        let string_model = source
            .replace(
                "fn initial(_start: Start) -> Int:\n    0",
                "fn initial(_start: Start) -> String:\n    \"0\"",
            )
            .replace("fn start(_auth: UiRoot, _model: Int)", "fn start(_auth: UiRoot, _model: String)")
            .replace(
                "fn update(_auth: UiRoot, model: Int, _message: Msg) -> (Int, Cmd(Msg)):\n    (model + 1, NoCmd)",
                "fn update(_auth: UiRoot, model: String, _message: Msg) -> (String, Cmd(Msg)):\n    (model, NoCmd)",
            )
            .replace("fn render(model: Int)", "fn render(model: String)")
            .replace("fn subscriptions(_auth: UiRoot, _model: Int)", "fn subscriptions(_auth: UiRoot, _model: String)")
            .replace("Program(UiRoot, Int, Msg)", "Program(UiRoot, String, Msg)")
            .replace("fn static_view(model: Int)", "fn static_view(model: String)")
            .replace("model == 7", "model == \"7\"")
            .replace("model == 8", "model == \"8\"")
            .replace("model == 10", "model == \"10\"")
            .replace("model == 11", "model == \"11\"")
            .replace("app(), 7, static_view", "app(), \"7\", static_view");
        std::fs::write(root.join("src/island_check.witchy"), string_model)
            .expect("write specialized model fixture");
        let string_checked = check_static_project(load_project(&root).expect("project"))
            .expect("string model island declaration");
        assert_eq!(string_checked.island_plans[0].model_type, "String");
        assert_ne!(string_checked.island_plans[0].artifact, int_artifact);

        let nominal_model = r#"import json
import public_state
import reflect
from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Increment

type Model derive(PublicState, Reflect, Deserialize):
    count: Int

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn initial(_start: Start) -> Model:
    Model(0)

fn start(_auth: Nil, _model: Model) -> Cmd(Msg):
    NoCmd

fn update(_auth: Nil, model: Model, _message: Msg) -> (Model, Cmd(Msg)):
    (Model(model.count + 1), NoCmd)

fn render(model: Model) -> Ui(Msg):
    glamour.ui(glamour.element("button", [glamour.on_event("counter.increment", "click", glamour.event_msg(Increment))], [glamour.text("${model.count}")]))

fn subscriptions(_auth: Nil, _model: Model) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(Nil, Model, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Model) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let counter = glamour.island("counter", app(), Model(7), static_view, glamour.OnInteraction)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(counter)))]),
        [counter],
    )
"#;
        std::fs::write(root.join("src/island_check.witchy"), nominal_model)
            .expect("write nominal model fixture");
        let nominal_checked = check_static_project(load_project(&root).expect("project"))
            .expect("nominal model island declaration");
        assert!(nominal_checked.island_plans[0]
            .model_type
            .ends_with("Model"));
        assert!(!nominal_checked
            .island_publication
            .as_ref()
            .expect("nominal island publication")
            .artifacts[0]
            .wasm
            .is_empty());

        let unsafe_sink = source.replace(
            "glamour.attribute(\"title\"",
            "glamour.attribute(\"onclick\"",
        );
        std::fs::write(root.join("src/island_check.witchy"), unsafe_sink)
            .expect("write unsafe sink fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("event-handler attribute sinks must fail before code generation");
        assert!(
            error.to_string().contains("disallowed attribute `onclick`"),
            "{error}"
        );

        let mixed_keys = source.replacen(
            "glamour.keyed(\"b\", glamour.element(\"li\", [], [glamour.text(\"B\")]))",
            "glamour.element(\"li\", [], [glamour.text(\"B\")])",
            1,
        );
        std::fs::write(root.join("src/island_check.witchy"), mixed_keys)
            .expect("write mixed keyed fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("mixed keyed and unkeyed siblings must fail before code generation");
        assert!(error.to_string().contains("mixes keyed and unkeyed"));

        let duplicate_key = source.replacen(
            "glamour.keyed(\"b\", glamour.element(\"li\", [], [glamour.text(\"B\")]))",
            "glamour.keyed(\"a\", glamour.element(\"li\", [], [glamour.text(\"B\")]))",
            1,
        );
        std::fs::write(root.join("src/island_check.witchy"), duplicate_key)
            .expect("write duplicate keyed fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("duplicate keyed siblings must fail before code generation");
        assert!(error.to_string().contains("repeats keyed child `a`"));

        let legacy_event = source.replace(
            "glamour.on_event(\"counter.increment\", event, glamour.prevent_default(glamour.event_msg(Increment)))",
            "glamour.on(\"click\", Increment)",
        );
        std::fs::write(root.join("src/island_check.witchy"), legacy_event)
            .expect("write legacy event fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("resumable islands must use typed event decoders");
        assert!(error.to_string().contains("must use typed glamour.on_event"));

        let indirect = source.replace(
            "pub fn web() -> Site:\n    let counter = glamour.island(\"counter\", app(), 7, static_view, glamour.OnInteraction)",
            "fn wrapped(key: String) -> IslandPlan:\n    glamour.island(key, app(), 7, static_view, glamour.OnInteraction)\n\npub fn web() -> Site:\n    let counter = wrapped(\"counter\")",
        );
        std::fs::write(root.join("src/island_check.witchy"), indirect)
            .expect("write indirect island fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("dynamic island declarations must not select executable identity");
        assert!(error.to_string().contains("literal key"), "{error}");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn interactive_embeds_one_program_view_without_manual_island_plumbing() {
        let root = temp_path("interactive-authoring");
        let mut files = static_fixture_files("interactive-authoring", "interactive_authoring");
        files.insert(
            PathBuf::from("src/interactive_authoring.witchy"),
            r#"import reflect
from glamour import Cmd, MediaQuery, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Increment

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn initial(_start: Start) -> Int:
    0

fn start(_auth: Nil, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: Nil, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn view(model: Int) -> Ui(Msg):
    glamour.ui(glamour.element("div", [], [glamour.element("a", [glamour.static_url_attribute("href", "/next"), glamour.on_event("counter.increment", "click", glamour.prevent_default(glamour.event_msg(Increment)))], [glamour.text("${model}")]), glamour.element("form", [glamour.static_url_attribute("action", "/save"), glamour.attribute("method", "post"), glamour.on_event("counter.submit", "submit", glamour.prevent_default(glamour.event_msg(Increment)))], [glamour.element("button", [glamour.attribute("type", "submit")], [glamour.text("Save")])])]))

fn subscriptions(_auth: Nil, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(Nil, Int, Msg):
    glamour.program(authorize, initial, start, update, view, subscriptions)

fn wide() -> MediaQuery:
    media"(min-width: 40rem)"

pub fn web() -> Site:
    let counter = glamour.interactive(app(), 7)
    let tuned = glamour.interactive(app(), 8).named("tuned").activate(glamour.OnInteraction).prefetch(glamour.PrefetchVisible)
    let media = glamour.interactive(app(), 9).named("media").activate(glamour.OnMedia(wide())).prefetch(glamour.PrefetchMedia(wide()))
    glamour.site([glamour.static_page("/", glamour.ui(html"<main>${glamour.embed(counter)}${glamour.embed(tuned)}${glamour.embed(media)}</main>"))])
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create");

        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("checked Interactive authoring boundary");
        assert_eq!(checked.islands.len(), 3);
        assert_eq!(checked.island_plans.len(), 3);
        let default = checked
            .islands
            .iter()
            .find(|island| island.diagnostic_name.is_none())
            .expect("default Interactive");
        assert!(default.key.starts_with("interactive-"));
        assert_eq!(default.activation, "visible");
        assert_eq!(default.prefetch, "none");
        assert_eq!(default.mode, "resume");
        assert_eq!(default.state.as_deref(), Some("7"));
        let tuned = checked
            .islands
            .iter()
            .find(|island| island.diagnostic_name.as_deref() == Some("tuned"))
            .expect("tuned Interactive");
        assert_eq!(tuned.activation, "interaction");
        assert_eq!(tuned.prefetch, "visible");
        assert_eq!(tuned.state.as_deref(), Some("8"));
        let media = checked
            .islands
            .iter()
            .find(|island| island.diagnostic_name.as_deref() == Some("media"))
            .expect("media Interactive");
        assert_eq!(media.activation, "media");
        assert_eq!(media.media.as_deref(), Some("(min-width: 40rem)"));
        assert_eq!(media.prefetch, "media");
        assert_eq!(media.prefetch_media.as_deref(), Some("(min-width: 40rem)"));
        assert_eq!(media.state.as_deref(), Some("9"));
        assert_eq!(checked.pages[0].island_keys.len(), 3);
        assert!(checked.island_plans.iter().all(|plan| {
            plan.events.iter().any(|event| {
                event.fallback
                    == Some(StaticIslandProgressiveFallback::Navigate {
                        href: "/next".into(),
                    })
            })
        }));
        assert!(checked.island_plans.iter().all(|plan| {
            plan.events.iter().any(|event| {
                event.fallback
                    == Some(StaticIslandProgressiveFallback::Submit {
                        action: "/save".into(),
                        method: "post".into(),
                    })
            })
        }));
        for island in &checked.islands {
            assert!(checked.pages[0]
                .html
                .contains(&format!("data-glamour-island-key=\"{}\"", island.key)));
        }
        assert!(checked
            .island_plans
            .iter()
            .all(|plan| plan.program_name == "interactive_authoring.app"));
        let output = root.join("dist");
        write_static_production(&checked, &output)
            .expect("publish preferred Interactive authoring graph");
        static_site::audit_static_island_artifacts(&output, &checked)
            .expect("audit preferred Interactive authoring graph");
        let published = std::fs::read_to_string(output.join("index.html"))
            .expect("published Interactive page");
        assert_eq!(
            published
                .matches(" data-witchy-islands data-witchy-islands-manifest=")
                .count(),
            1,
        );
        let headers = std::fs::read_to_string(output.join("_headers"))
            .expect("published Interactive headers");
        assert!(headers.contains("script-src 'self'"));
        assert!(headers.contains("'wasm-unsafe-eval'"));
        assert!(headers.contains("connect-src 'self'"));
        assert!(headers.contains("form-action 'self'"));
        assert!(headers.contains("frame-src 'none'"));
        assert!(headers.contains("worker-src 'none'"));
        assert!(headers.contains("require-trusted-types-for 'script'"));
        assert!(headers.contains("trusted-types 'none'"));
        assert_eq!(
            checked
                .island_publication
                .as_ref()
                .expect("Interactive publication")
                .artifacts
                .len(),
            1,
        );
        let published_islands = checked
            .island_publication
            .as_ref()
            .expect("Interactive publication")
            .manifest["islands"]
            .as_array()
            .expect("published islands");
        assert!(published_islands.iter().all(|island| {
            let events = island["events"].as_array().expect("published events");
            events.iter().any(|event| {
                event["fallback"]
                    == serde_json::json!({"kind": "navigate", "href": "/next"})
            }) && events.iter().any(|event| {
                event["fallback"]
                    == serde_json::json!({"kind": "submit", "action": "/save", "method": "post"})
            })
        }));

        let source_path = root.join("src/interactive_authoring.witchy");
        let original = std::fs::read_to_string(&source_path).expect("read Interactive source");
        let invalid_child = original.replace("glamour.embed(counter)", "40");
        std::fs::write(&source_path, invalid_child).expect("write invalid child-hole fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("arbitrary child-hole values must fail type-directed lowering");
        assert!(error.to_string().contains(
            "Glamour child holes accept only `String`, `glamour.VNode(msg)`, or `glamour.Ui(msg)`"
        ));

        let interpolated = original.replace(
            "media\"(min-width: 40rem)\"",
            "media\"(min-width: ${40}rem)\"",
        );
        std::fs::write(&source_path, interpolated).expect("write dynamic media fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("media conditions must remain compiler-checked static values");
        assert!(error.to_string().contains("interpolations are not allowed"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn client_region_keeps_fallback_inert_and_serializes_no_model() {
        let root = temp_path("client-region-authoring");
        let mut files = static_fixture_files("client-region-authoring", "client_region_authoring");
        files.insert(
            PathBuf::from("src/client_region_authoring.witchy"),
            r#"from glamour import Cmd, Program, Site, Start, StaticUi, Sub, Ui, UiRoot

type Msg:
    Activated

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn initial(start: Start) -> Int:
    match start:
        Start("/", "") -> 42
        _ -> 0

fn start(_auth: Nil, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: Nil, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn view(model: Int) -> Ui(Msg):
    let root_events = if model % 2 == 0: [glamour.on_event("editor.optional", "keydown", glamour.event_msg(Activated))] else: []
    glamour.ui(glamour.element("div", root_events, [glamour.element("button", [glamour.on_event("editor.activated", "click", glamour.event_msg(Activated))], [glamour.text("Toggle-${model}")]), glamour.branch("editor-details", model > 42, glamour.element("button", [glamour.on_event("editor.details", "click", glamour.event_msg(Activated))], [glamour.text("Detail-${model}")]))]))

fn subscriptions(_auth: Nil, _model: Int) -> Sub(Msg):
    NoSub

fn app() -> Program(Nil, Int, Msg):
    glamour.program(authorize, initial, start, update, view, subscriptions)

fn fallback() -> StaticUi:
    glamour.static_ui(html"<p>Editor loads on demand</p>")

pub fn web() -> Site:
    let editor = glamour.client_region(app(), glamour.static_ui(html"<p>Editor loads on demand</p>")).activate(glamour.OnInteraction)
    glamour.site([glamour.static_page("/", glamour.ui(html"<main>${glamour.embed(editor)}</main>"))])
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create");

        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("checked fresh client region");
        assert_eq!(checked.islands.len(), 1);
        assert_eq!(checked.islands[0].mode, "fresh");
        assert_eq!(checked.islands[0].state, None);
        assert_eq!(checked.island_plans[0].mode, "fresh");
        assert_eq!(checked.island_plans[0].events.len(), 3);
        let manifest = &checked
            .island_publication
            .as_ref()
            .expect("fresh publication")
            .manifest;
        assert_eq!(manifest["islands"][0]["mode"], "fresh");
        assert_eq!(manifest["islands"][0]["state"], serde_json::Value::Null);
        assert_eq!(manifest["islands"][0]["events"], serde_json::json!([]));
        let publication = checked
            .island_publication
            .as_ref()
            .expect("fresh publication");
        let artifact_record = &publication.artifact_manifest["artifacts"][0];
        let fresh = &artifact_record["fresh"];
        assert_eq!(fresh["route"], "/");
        assert_eq!(fresh["bootstrap"], "");
        assert_ne!(fresh["template"].as_u64().expect("fresh template"), 0);
        assert_ne!(fresh["instance"].as_u64().expect("fresh instance"), 0);
        assert_eq!(artifact_record["templates"].as_array().expect("templates").len(), 2);
        assert_eq!(artifact_record["eventPlans"].as_array().expect("event plans").len(), 3);
        assert_eq!(artifact_record["fresh"]["template"], artifact_record["templates"][1]["id"]);
        assert_eq!(artifact_record["templates"][1]["events"].as_array().expect("fresh events").len(), 2);
        let published_page = publication.pages.get("/").expect("fresh page");
        assert!(published_page.contains("<p>Editor loads on demand</p>"));
        assert!(!published_page.contains("Toggle-42"));

        let artifact = &publication.artifacts[0];
        let (mut store, instance) = instantiate_glamour_wasm(&artifact.wasm);
        let memory = instance.get_memory(&mut store, "memory").expect("memory");
        let reserve = instance
            .get_typed_func::<i32, i32>(&mut store, "__glamour_input_reserve")
            .expect("reserve");
        let init = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_init")
            .expect("fresh init");
        let dispatch = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_dispatch")
            .expect("dispatch");
        let output_length = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_output_length")
            .expect("output length");
        let output_release = instance
            .get_typed_func::<(), ()>(&mut store, "__glamour_output_release")
            .expect("output release");
        let app_id = artifact_record["appId"].as_u64().expect("app id") as u32;
        let build_id = u64::from_str_radix(
            artifact_record["buildId"]
                .as_str()
                .expect("build id")
                .trim_start_matches("0x"),
            16,
        )
        .expect("hex build id");
        let start_payload = br#"{"route":"/","bootstrap":""}"#;
        let mut start_frame = vec![0_u8; 48 + start_payload.len()];
        start_frame[..4].copy_from_slice(b"GLMR");
        start_frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
        start_frame[8] = 1;
        start_frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
        let start_length = start_frame.len() as u32;
        start_frame[12..16].copy_from_slice(&start_length.to_le_bytes());
        start_frame[20..24].copy_from_slice(&app_id.to_le_bytes());
        start_frame[24..32].copy_from_slice(&build_id.to_le_bytes());
        start_frame[40..44].copy_from_slice(&48_u32.to_le_bytes());
        start_frame[48..].copy_from_slice(start_payload);
        let start_pointer = reserve
            .call(&mut store, start_frame.len() as i32)
            .expect("reserve fresh Start");
        memory
            .write(&mut store, start_pointer as usize, &start_frame)
            .expect("write fresh Start");
        let output = init
            .call(&mut store, (start_pointer, start_frame.len() as i32))
            .expect("initialize fresh client region");
        let output_len = output_length.call(&mut store, ()).expect("mount length") as usize;
        let mount = memory.data(&store)[output as usize..output as usize + output_len].to_vec();
        assert_eq!(&mount[..4], b"GLMR");
        assert_eq!(mount[8], 16);
        assert_eq!(u64::from_le_bytes(mount[32..40].try_into().unwrap()), 0);
        let operations = glamour_output_operations(&mount);
        assert_eq!(operations.len(), 1);
        assert_eq!(glamour_operation_tag(operations[0]), 1);
        assert_eq!(glamour_operation_u32(operations[0], 8), fresh["template"].as_u64().unwrap() as u32);
        assert_eq!(glamour_operation_u32(operations[0], 12), fresh["instance"].as_u64().unwrap() as u32);
        assert_eq!(glamour_operation_u32(operations[0], 24), 1);
        assert_eq!(
            glamour_payload_text(
                &mount,
                glamour_operation_u32(operations[0], 32),
                glamour_operation_u32(operations[0], 36),
            ),
            "Toggle-42",
        );
        output_release.call(&mut store, ()).expect("release mount");

        let event = checked.island_plans[0]
            .events
            .iter()
            .find(|event| event.id == "editor.activated")
            .expect("initially live event");
        let mut event_frame = vec![0_u8; 96];
        event_frame[..4].copy_from_slice(b"GLMR");
        event_frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
        event_frame[8] = 2;
        event_frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
        event_frame[12..16].copy_from_slice(&96_u32.to_le_bytes());
        event_frame[16..20].copy_from_slice(&1_u32.to_le_bytes());
        event_frame[20..24].copy_from_slice(&app_id.to_le_bytes());
        event_frame[24..32].copy_from_slice(&build_id.to_le_bytes());
        event_frame[40..44].copy_from_slice(&96_u32.to_le_bytes());
        event_frame[48..50].copy_from_slice(&1_u16.to_le_bytes());
        event_frame[52..56].copy_from_slice(&48_u32.to_le_bytes());
        event_frame[56..60].copy_from_slice(&event.plan.to_le_bytes());
        event_frame[60..64].copy_from_slice(&checked.island_plans[0].registry_id.to_le_bytes());
        event_frame[64..68].copy_from_slice(&event.event_class.to_le_bytes());
        event_frame[72..76].copy_from_slice(&96_u32.to_le_bytes());
        event_frame[80..84].copy_from_slice(&96_u32.to_le_bytes());
        let event_pointer = reserve
            .call(&mut store, event_frame.len() as i32)
            .expect("reserve fresh event");
        memory
            .write(&mut store, event_pointer as usize, &event_frame)
            .expect("write fresh event");
        let output = dispatch
            .call(&mut store, (event_pointer, event_frame.len() as i32))
            .expect("dispatch fresh event");
        let output_len = output_length.call(&mut store, ()).expect("patch length") as usize;
        let patch = &memory.data(&store)[output as usize..output as usize + output_len];
        assert_eq!(patch[8], 17);
        let operations = glamour_output_operations(patch);
        assert_eq!(
            operations
                .iter()
                .map(|operation| glamour_operation_tag(operation))
                .collect::<Vec<_>>(),
            vec![15, 7, 2, 2],
        );
        let text = operations
            .iter()
            .find(|operation| {
                glamour_operation_tag(operation) == 2
                    && glamour_payload_text(
                        patch,
                        glamour_operation_u32(operation, 12),
                        glamour_operation_u32(operation, 16),
                    ) == "Toggle-43"
            })
            .expect("fresh stable text patch");
        assert_eq!(
            glamour_payload_text(
                patch,
                glamour_operation_u32(text, 12),
                glamour_operation_u32(text, 16),
            ),
            "Toggle-43",
        );
        output_release.call(&mut store, ()).expect("release patch");

        event_frame[32..40].copy_from_slice(&1_u64.to_le_bytes());
        let event_pointer = reserve
            .call(&mut store, event_frame.len() as i32)
            .expect("reserve second fresh event");
        memory
            .write(&mut store, event_pointer as usize, &event_frame)
            .expect("write second fresh event");
        let output = dispatch
            .call(&mut store, (event_pointer, event_frame.len() as i32))
            .expect("dispatch second fresh event");
        let output_len = output_length.call(&mut store, ()).expect("second patch length") as usize;
        let patch = &memory.data(&store)[output as usize..output as usize + output_len];
        assert_eq!(patch[8], 17);
        let operations = glamour_output_operations(patch);
        assert_eq!(
            operations
                .iter()
                .map(|operation| glamour_operation_tag(operation))
                .collect::<Vec<_>>(),
            vec![14, 2, 2],
        );
        output_release.call(&mut store, ()).expect("release second patch");
        let output = root.join("dist");
        write_static_production(&checked, &output)
            .expect("publish fresh client-region graph");
        static_site::audit_static_island_artifacts(&output, &checked)
            .expect("audit fresh client-region graph");

        let source_path = root.join("src/client_region_authoring.witchy");
        let original = std::fs::read_to_string(&source_path).expect("read client-region source");
        let eventful = original.replace(
            "glamour.static_ui(html\"<p>Editor loads on demand</p>\")",
            "glamour.static_ui(html\"<button on:click=${Nil}>Editor</button>\")",
        );
        std::fs::write(&source_path, eventful).expect("write eventful StaticUi fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("StaticUi event bindings must fail closed");
        assert!(error
            .to_string()
            .contains("glamour StaticUi: event bindings are not allowed"));

        let property = original.replace(
            "glamour.static_ui(html\"<p>Editor loads on demand</p>\")",
            "glamour.static_ui(glamour.element(\"input\", [glamour.property(\"value\", \"draft\")], []))",
        );
        std::fs::write(&source_path, property).expect("write property StaticUi fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("StaticUi browser properties must fail closed");
        assert!(error
            .to_string()
            .contains("glamour StaticUi: browser property sinks are not static"));

        let forged = original.replace(
            "glamour.static_ui(html\"<p>Editor loads on demand</p>\")",
            "StaticUi(html\"<p>Editor loads on demand</p>\")",
        );
        std::fs::write(&source_path, forged).expect("write forged StaticUi fixture");
        check_static_project(load_project(&root).expect("project"))
            .expect_err("applications must not name the sealed StaticUi constructor");

        let indirect = original.replace(
            "glamour.client_region(app(), glamour.static_ui(html\"<p>Editor loads on demand</p>\"))",
            "glamour.client_region(app(), fallback())",
        );
        std::fs::write(&source_path, indirect).expect("write indirect StaticUi fixture");
        let error = check_static_project(load_project(&root).expect("project"))
            .expect_err("client-region fallback provenance must be direct");
        assert!(error
            .to_string()
            .contains("fallback must be a direct `glamour.static_ui(...)` value"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn interactive_metadata_joins_by_authenticated_origin_not_registry_order() {
        let root = temp_path("interactive-origin-join");
        let mut files = static_fixture_files("interactive-origin-join", "interactive_origin_join");
        files.insert(
            PathBuf::from("src/interactive_origin_join.witchy"),
            r#"import reflect
from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Changed

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn int_initial(_start: Start) -> Int:
    0

fn int_start(_auth: Nil, _model: Int) -> Cmd(Msg):
    NoCmd

fn int_update(_auth: Nil, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn int_view(model: Int) -> Ui(Msg):
    glamour.ui(glamour.text("${model}"))

fn int_subscriptions(_auth: Nil, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn int_app() -> Program(Nil, Int, Msg):
    glamour.program(authorize, int_initial, int_start, int_update, int_view, int_subscriptions)

fn string_initial(_start: Start) -> String:
    ""

fn string_start(_auth: Nil, _model: String) -> Cmd(Msg):
    NoCmd

fn string_update(_auth: Nil, model: String, _message: Msg) -> (String, Cmd(Msg)):
    (model + "!", NoCmd)

fn string_view(model: String) -> Ui(Msg):
    glamour.ui(glamour.text(model))

fn string_subscriptions(_auth: Nil, _model: String) -> Sub(Msg):
    glamour.no_sub()

fn string_app() -> Program(Nil, String, Msg):
    glamour.program(authorize, string_initial, string_start, string_update, string_view, string_subscriptions)

pub fn web() -> Site:
    let numbers = glamour.interactive(int_app(), 1).named("numbers")
    let words = glamour.interactive(string_app(), "one").named("words")
    glamour.site([glamour.static_page("/", glamour.ui(glamour.element("main", [], [glamour.embed(words), glamour.embed(numbers)])))])
"#
            .into(),
        );
        create_project_atomically(&root, &files).expect("create");

        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("identity-bound heterogeneous Interactive publication");
        for (name, program, model) in [
            ("numbers", "interactive_origin_join.int_app", "Int"),
            ("words", "interactive_origin_join.string_app", "String"),
        ] {
            let island = checked
                .islands
                .iter()
                .find(|island| island.diagnostic_name.as_deref() == Some(name))
                .expect("named Interactive instance");
            let plan = checked
                .island_plans
                .iter()
                .find(|plan| plan.key == island.key)
                .expect("compiled Interactive plan");
            assert_eq!(plan.program_name, program);
            assert_eq!(plan.model_type, model);
        }
        let second = evaluate_static_site(&load_project(&root).expect("reload project"))
            .expect("deterministic Interactive evaluation keeps compiler origins");
        assert_eq!(second.0, checked.pages);
        assert_eq!(second.5, checked.islands);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn static_site_requires_loader_authenticated_glamour_identity() {
        let root = temp_path("static-identity");
        let mut files = static_fixture_files("static-spoof", "spoof");
        files.insert(
            PathBuf::from("src/spoof.witchy"),
            "type Site:\n    Site(List(String))\n\npub fn web() -> Site:\n    Site([])\n"
                .into(),
        );
        create_project_atomically(&root, &files).expect("create");
        let project = load_project(&root).expect("load");
        let error = check_static_project(project)
            .expect_err("workspace type named glamour.Site must not authenticate");
        assert!(error
            .to_string()
            .contains("must return toolchain `glamour.Site`"), "{error}");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn development_scalar_snapshot_restores_atomically_and_production_omits_it() {
        let root = temp_path("development-snapshot");
        let mut files = client_fixture_files("snapshot-check", "snapshot_check");
        let source = files
            .get_mut(Path::new("src/snapshot_check.witchy"))
            .expect("starter source");
        *source = source
            .replace(
                "type BrowserState:\n    BrowserState(Bool)",
                "type BrowserState:\n    count: Int\n    ratio: Float\n    active: Bool",
            )
            .replace("BrowserState(true)", "BrowserState(7, 1.5, true)")
            .replace(
                "pub fn glamour_dispatch(state: BrowserState, _input: Bytes) -> BrowserState:\n    state",
                "pub fn glamour_dispatch(_state: BrowserState, _input: Bytes) -> BrowserState:\n    BrowserState(8, 2.5, false)",
            )
            .replace("BrowserState(_) -> Nil", "BrowserState(_, _, _) -> Nil");
        create_project_atomically(&root, &files).expect("create");
        let checked = check_project_development(&root).expect("development check");
        let metadata = checked.development.as_ref().expect("scalar metadata");
        assert_eq!(
            metadata.state_fields,
            [
                witchy_lower::codegen::GlamourDevelopmentField::I64,
                witchy_lower::codegen::GlamourDevelopmentField::F64,
                witchy_lower::codegen::GlamourDevelopmentField::Bool,
            ]
        );
        assert_eq!(metadata.state_field_names, ["count", "ratio", "active"]);
        for name in FORBIDDEN_PRODUCTION_EXPORTS {
            assert!(checked.exports.contains(*name), "missing {name}");
        }

        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("Wasm GC engine");
        let module = wasmtime::Module::new(&engine, &checked.wasm).expect("development Wasm");
        let imports = module
            .imports()
            .map(|import| format!("{}.{}", import.module(), import.name()))
            .collect::<Vec<_>>();
        assert_eq!(
            imports,
            ["witchy.fill_pending", "witchy.user_cap_field_len"]
        );
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap("witchy", "fill_pending", |_pointer: i32| {})
            .expect("fill_pending");
        linker
            .func_wrap(
                "witchy",
                "user_cap_field_len",
                |_capability: i32, _field: i32| -> i32 { 0 },
            )
            .expect("user_cap_field_len");
        let mut old_store = wasmtime::Store::new(&engine, ());
        let old = linker
            .instantiate(&mut old_store, &module)
            .expect("old instance");
        let old_memory = old.get_memory(&mut old_store, "memory").expect("memory");
        let reserve = old
            .get_typed_func::<i32, i32>(&mut old_store, "__glamour_input_reserve")
            .expect("reserve");
        let init = old
            .get_typed_func::<(i32, i32), i32>(&mut old_store, "__glamour_init")
            .expect("init");
        let dispatch = old
            .get_typed_func::<(i32, i32), i32>(&mut old_store, "__glamour_dispatch")
            .expect("dispatch");
        let output_length = old
            .get_typed_func::<(), i32>(&mut old_store, "__glamour_output_length")
            .expect("output length");
        let changes = old
            .get_typed_func::<(), i32>(&mut old_store, "__glamour_dev_changes")
            .expect("model changes");
        let changes_length = old
            .get_typed_func::<(), i32>(&mut old_store, "__glamour_dev_changes_length")
            .expect("model changes length");
        let release = old
            .get_typed_func::<(), ()>(&mut old_store, "__glamour_output_release")
            .expect("release");
        let input = reserve.call(&mut old_store, 0).expect("reserve empty input");
        let output = init.call(&mut old_store, (input, 0)).expect("initialize");
        let changes_ptr = changes.call(&mut old_store, ()).expect("changes pointer") as usize;
        let changes_len = changes_length
            .call(&mut old_store, ())
            .expect("changes length") as usize;
        assert_eq!(changes_len, metadata.state_fields.len());
        assert_eq!(
            old_memory
                .data(&old_store)
                .get(changes_ptr..changes_ptr + changes_len)
                .expect("changes bounds"),
            [1, 1, 1],
        );
        let output_len = output_length.call(&mut old_store, ()).expect("length") as usize;
        let old_output = old_memory
            .data(&old_store)
            .get(output as usize..output as usize + output_len)
            .expect("output bounds")
            .to_vec();
        release.call(&mut old_store, ()).expect("release output");
        for expected in [[1, 1, 1], [0, 0, 0]] {
            let input = reserve.call(&mut old_store, 0).expect("reserve dispatch input");
            dispatch
                .call(&mut old_store, (input, 0))
                .expect("dispatch scalar update");
            assert_eq!(
                old_memory
                    .data(&old_store)
                    .get(changes_ptr..changes_ptr + changes_len)
                    .expect("dispatch changes bounds"),
                expected,
            );
            release.call(&mut old_store, ()).expect("release dispatch output");
        }
        let snapshot_ptr = old
            .get_typed_func::<(), i32>(&mut old_store, "__glamour_dev_snapshot")
            .expect("snapshot")
            .call(&mut old_store, ())
            .expect("snapshot pointer");
        let snapshot_len = old
            .get_typed_func::<(), i32>(&mut old_store, "__glamour_dev_snapshot_length")
            .expect("snapshot length")
            .call(&mut old_store, ())
            .expect("snapshot length") as usize;
        let snapshot = old_memory
            .data(&old_store)
            .get(snapshot_ptr as usize..snapshot_ptr as usize + snapshot_len)
            .expect("snapshot bounds")
            .to_vec();
        assert_eq!(&snapshot[..4], b"WGST");
        assert_eq!(&snapshot[8..40], metadata.model_schema.as_slice());

        let mut new_store = wasmtime::Store::new(&engine, ());
        let new = linker
            .instantiate(&mut new_store, &module)
            .expect("new instance");
        let new_memory = new.get_memory(&mut new_store, "memory").expect("memory");
        let new_reserve = new
            .get_typed_func::<i32, i32>(&mut new_store, "__glamour_input_reserve")
            .expect("reserve");
        let restore = new
            .get_typed_func::<(i32, i32), i32>(&mut new_store, "__glamour_dev_restore")
            .expect("restore");
        let restore_ptr = new_reserve
            .call(&mut new_store, snapshot_len as i32)
            .expect("reserve snapshot");
        let mut corrupt = snapshot.clone();
        corrupt[8] ^= 1;
        new_memory
            .write(&mut new_store, restore_ptr as usize, &corrupt)
            .expect("write corrupt snapshot");
        assert!(
            restore
                .call(&mut new_store, (restore_ptr, snapshot_len as i32))
                .is_err(),
            "schema mismatch must trap"
        );
        new_memory
            .write(&mut new_store, restore_ptr as usize, &snapshot)
            .expect("write valid snapshot");
        let restored_output = restore
            .call(&mut new_store, (restore_ptr, snapshot_len as i32))
            .expect("restore after rejected candidate");
        let restored_len = new
            .get_typed_func::<(), i32>(&mut new_store, "__glamour_output_length")
            .expect("restored output length")
            .call(&mut new_store, ())
            .expect("restored length") as usize;
        assert_eq!(
            new_memory
                .data(&new_store)
                .get(restored_output as usize..restored_output as usize + restored_len)
                .expect("restored output bounds"),
            old_output,
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn stateful_browser_abi_accepts_private_bytes_state() {
        let root = temp_path("bytes-browser-state");
        let mut files = client_fixture_files("bytes-state", "bytes_state");
        let source = files
            .get_mut(Path::new("src/bytes_state.witchy"))
            .expect("starter source");
        *source = source
            .replace("BrowserState(Bool)", "BrowserState(Bytes)")
            .replace("BrowserState(true)", "BrowserState(_input)");
        create_project_atomically(&root, &files).expect("create bytes state project");
        let checked = check_project_development(&root).expect("compile private Bytes state");
        let metadata = checked.development.as_ref().expect("development metadata");
        assert_eq!(
            metadata.state_fields,
            [witchy_lower::codegen::GlamourDevelopmentField::Aggregate],
        );
        assert_eq!(metadata.snapshot_format(), 0);
        assert!(!checked.exports.contains("__glamour_dev_snapshot"));
        assert!(checked.exports.contains("__glamour_dispatch"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn aggregate_public_state_snapshots_restore_and_trace_exactly() {
        let root = temp_path("aggregate-snapshot");
        create_project_atomically(
            &root,
            &client_fixture_files("aggregate-check", "aggregate_check"),
        )
        .expect("create");
        let source = root.join("src/aggregate_check.witchy");
        let aggregate = std::fs::read_to_string(&source)
            .expect("source")
            .replace("import bytes\n", "import bytes\nimport public_state\n")
            .replace(
                "type BrowserState:\n    BrowserState(Bool)",
                "type BrowserState derive(PublicState):\n    BrowserState(String, List(String))",
            )
            .replace(
                "BrowserState(true)",
                "BrowserState(\"private\", [\"stable\"])",
            )
            .replace(
                "pub fn glamour_dispatch(state: BrowserState, _input: Bytes) -> BrowserState:\n    state",
                "pub fn glamour_dispatch(_state: BrowserState, _input: Bytes) -> BrowserState:\n    BrowserState(\"changed\", [\"stable\"])",
            )
            .replace("BrowserState(_) -> Nil", "BrowserState(_, _) -> Nil");
        std::fs::write(&source, aggregate).expect("aggregate source");
        let checked = check_project_development(&root).expect("development check");
        let metadata = checked.development.as_ref().expect("aggregate metadata");
        assert_eq!(metadata.snapshot_format(), 2);
        assert_eq!(metadata.state_fields, [
            witchy_lower::codegen::GlamourDevelopmentField::Aggregate,
            witchy_lower::codegen::GlamourDevelopmentField::Aggregate,
        ]);
        for name in [
            "__glamour_dev_metadata",
            "__glamour_dev_changes",
            "__glamour_dev_changes_length",
        ] {
            assert!(checked.exports.contains(name), "missing {name}");
        }
        for name in [
            "__glamour_dev_snapshot",
            "__glamour_dev_snapshot_length",
            "__glamour_dev_restore",
        ] {
            assert!(checked.exports.contains(name), "aggregate snapshot omitted {name}");
        }
        let output = root.join("development-dist");
        write_development(&checked, &output).expect("write aggregate development build");
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(output.join("witchy-web-manifest.json"))
                .expect("development manifest"),
        )
        .expect("development manifest JSON");
        assert_eq!(manifest["development"]["snapshotFormat"], 2);
        assert_eq!(manifest["development"]["maxSnapshotBytes"], 1024 * 1024);
        assert_eq!(manifest["features"]["hotSwap"], true);
        assert_eq!(manifest["features"]["developmentExports"], true);

        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("Wasm GC engine");
        let module = wasmtime::Module::new(&engine, &checked.wasm).expect("development Wasm");
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap("witchy", "fill_pending", |_pointer: i32| {})
            .expect("fill_pending");
        linker
            .func_wrap(
                "witchy",
                "user_cap_field_len",
                |_capability: i32, _field: i32| -> i32 { 0 },
            )
            .expect("user_cap_field_len");
        linker
            .func_wrap(
                "witchy",
                "__witchy_abort",
                |_template: i32, _first: i64, _second: i64, _text: i32| -> wasmtime::Result<()> {
                    Err(wasmtime::Error::msg("Witchy guest aborted"))
                },
            )
            .expect("abort");
        linker
            .func_wrap(
                "witchy",
                "float_to_str",
                |_value: f64, _output: i32| -> i32 { 0 },
            )
            .expect("float_to_str");
        linker
            .func_wrap(
                "witchy",
                "string_from_code",
                |_codepoint: i64, _output: i32| -> i32 { 0 },
            )
            .expect("string_from_code");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module).expect("instance");
        let memory = instance.get_memory(&mut store, "memory").expect("memory");
        let reserve = instance
            .get_typed_func::<i32, i32>(&mut store, "__glamour_input_reserve")
            .expect("reserve");
        let init = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_init")
            .expect("init");
        let dispatch = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__glamour_dispatch")
            .expect("dispatch");
        let release = instance
            .get_typed_func::<(), ()>(&mut store, "__glamour_output_release")
            .expect("release");
        let snapshot = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_dev_snapshot")
            .expect("snapshot");
        let snapshot_length = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_dev_snapshot_length")
            .expect("snapshot length");
        let changes = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_dev_changes")
            .expect("changes");
        let changes_length = instance
            .get_typed_func::<(), i32>(&mut store, "__glamour_dev_changes_length")
            .expect("changes length");
        let changes_ptr = changes.call(&mut store, ()).expect("changes pointer") as usize;
        assert_eq!(
            changes_length.call(&mut store, ()).expect("changes length"),
            2,
        );
        let input = reserve.call(&mut store, 0).expect("reserve init input");
        init.call(&mut store, (input, 0)).expect("initialize");
        assert_eq!(
            &memory.data(&store)[changes_ptr..changes_ptr + 2],
            [1, 1],
            "initial aggregate fields are new",
        );
        release.call(&mut store, ()).expect("release init output");

        let input = reserve.call(&mut store, 0).expect("reserve first dispatch");
        dispatch.call(&mut store, (input, 0)).expect("first dispatch");
        assert_eq!(
            &memory.data(&store)[changes_ptr..changes_ptr + 2],
            [1, 0],
            "only the field with different aggregate content changes",
        );
        release.call(&mut store, ()).expect("release first output");

        let input = reserve.call(&mut store, 0).expect("reserve second dispatch");
        dispatch.call(&mut store, (input, 0)).expect("second dispatch");
        assert_eq!(
            &memory.data(&store)[changes_ptr..changes_ptr + 2],
            [0, 0],
            "equal nested aggregate content is unchanged",
        );
        release.call(&mut store, ()).expect("release second output");
        let snapshot_ptr = snapshot.call(&mut store, ()).expect("snapshot state") as usize;
        let snapshot_len = snapshot_length.call(&mut store, ()).expect("snapshot length") as usize;
        let snapshot_bytes = memory.data(&store)[snapshot_ptr..snapshot_ptr + snapshot_len].to_vec();
        assert_eq!(&snapshot_bytes[..4], b"WGST");
        assert_eq!(u16::from_le_bytes(snapshot_bytes[4..6].try_into().unwrap()), 2);

        let mut candidate_store = wasmtime::Store::new(&engine, ());
        let candidate = linker
            .instantiate(&mut candidate_store, &module)
            .expect("candidate instance");
        let candidate_memory = candidate
            .get_memory(&mut candidate_store, "memory")
            .expect("candidate memory");
        let candidate_reserve = candidate
            .get_typed_func::<i32, i32>(&mut candidate_store, "__glamour_input_reserve")
            .expect("candidate reserve");
        let restore = candidate
            .get_typed_func::<(i32, i32), i32>(&mut candidate_store, "__glamour_dev_restore")
            .expect("restore");
        let candidate_release = candidate
            .get_typed_func::<(), ()>(&mut candidate_store, "__glamour_output_release")
            .expect("candidate release");
        let candidate_changes = candidate
            .get_typed_func::<(), i32>(&mut candidate_store, "__glamour_dev_changes")
            .expect("candidate changes")
            .call(&mut candidate_store, ())
            .expect("candidate changes pointer") as usize;
        let restore_ptr = candidate_reserve
            .call(&mut candidate_store, snapshot_len as i32)
            .expect("reserve snapshot");
        let mut corrupt = snapshot_bytes.clone();
        corrupt[8] ^= 1;
        candidate_memory
            .write(&mut candidate_store, restore_ptr as usize, &corrupt)
            .expect("write corrupt snapshot");
        assert!(
            restore
                .call(&mut candidate_store, (restore_ptr, snapshot_len as i32))
                .is_err(),
            "aggregate schema mismatch must reject the detached candidate",
        );
        candidate_memory
            .write(&mut candidate_store, restore_ptr as usize, &snapshot_bytes)
            .expect("write snapshot");
        restore
            .call(&mut candidate_store, (restore_ptr, snapshot_len as i32))
            .expect("restore aggregate state");
        assert_eq!(
            &candidate_memory.data(&candidate_store)[candidate_changes..candidate_changes + 2],
            [1, 1],
            "restored aggregate fields are emitted as changed",
        );
        candidate_release
            .call(&mut candidate_store, ())
            .expect("release restored output");
        let candidate_dispatch = candidate
            .get_typed_func::<(i32, i32), i32>(&mut candidate_store, "__glamour_dispatch")
            .expect("candidate dispatch");
        let input = candidate_reserve
            .call(&mut candidate_store, 0)
            .expect("reserve post-restore dispatch");
        candidate_dispatch
            .call(&mut candidate_store, (input, 0))
            .expect("dispatch restored state");
        assert_eq!(
            &candidate_memory.data(&candidate_store)[candidate_changes..candidate_changes + 2],
            [0, 0],
            "typed restoration preserves nested aggregate content",
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn aggregate_snapshot_uses_a_typed_generated_migration() {
        let root = temp_path("aggregate-migration");
        create_project_atomically(
            &root,
            &client_fixture_files("aggregate-migration", "aggregate_migration"),
        )
        .expect("create");
        let source_path = root.join("src/aggregate_migration.witchy");
        let old_source = std::fs::read_to_string(&source_path)
            .expect("source")
            .replace("import bytes\n", "import bytes\nimport public_state\n")
            .replace(
                "type BrowserState:\n    BrowserState(Bool)",
                "type BrowserState derive(PublicState):\n    BrowserState(String, List(String))",
            )
            .replace(
                "BrowserState(true)",
                "BrowserState(\"before\", [\"migration\"])",
            )
            .replace("BrowserState(_) -> Nil", "BrowserState(_, _) -> Nil");
        std::fs::write(&source_path, &old_source).expect("old source");
        let old_checked = check_project_development(&root).expect("old development build");
        let old_schema = old_checked
            .development
            .as_ref()
            .expect("old metadata")
            .model_schema_hex();

        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("Wasm GC engine");
        let old_module = wasmtime::Module::new(&engine, &old_checked.wasm).expect("old Wasm");
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap("witchy", "fill_pending", |_pointer: i32| {})
            .expect("fill_pending");
        linker
            .func_wrap(
                "witchy",
                "user_cap_field_len",
                |_capability: i32, _field: i32| -> i32 { 0 },
            )
            .expect("user_cap_field_len");
        linker
            .func_wrap(
                "witchy",
                "__witchy_abort",
                |_template: i32, _first: i64, _second: i64, _text: i32| -> wasmtime::Result<()> {
                    Err(wasmtime::Error::msg("Witchy guest aborted"))
                },
            )
            .expect("abort");
        linker
            .func_wrap(
                "witchy",
                "float_to_str",
                |_value: f64, _output: i32| -> i32 { 0 },
            )
            .expect("float_to_str");
        linker
            .func_wrap(
                "witchy",
                "string_from_code",
                |_codepoint: i64, _output: i32| -> i32 { 0 },
            )
            .expect("string_from_code");
        let mut old_store = wasmtime::Store::new(&engine, ());
        let old = linker
            .instantiate(&mut old_store, &old_module)
            .expect("old instance");
        let old_memory = old.get_memory(&mut old_store, "memory").expect("old memory");
        let old_reserve = old
            .get_typed_func::<i32, i32>(&mut old_store, "__glamour_input_reserve")
            .expect("old reserve");
        let old_init = old
            .get_typed_func::<(i32, i32), i32>(&mut old_store, "__glamour_init")
            .expect("old init");
        let old_release = old
            .get_typed_func::<(), ()>(&mut old_store, "__glamour_output_release")
            .expect("old release");
        let input = old_reserve.call(&mut old_store, 0).expect("old input");
        old_init.call(&mut old_store, (input, 0)).expect("old init");
        old_release.call(&mut old_store, ()).expect("old output release");
        let snapshot_ptr = old
            .get_typed_func::<(), i32>(&mut old_store, "__glamour_dev_snapshot")
            .expect("old snapshot")
            .call(&mut old_store, ())
            .expect("snapshot") as usize;
        let snapshot_len = old
            .get_typed_func::<(), i32>(&mut old_store, "__glamour_dev_snapshot_length")
            .expect("old snapshot length")
            .call(&mut old_store, ())
            .expect("snapshot length") as usize;
        let snapshot = old_memory.data(&old_store)
            [snapshot_ptr..snapshot_ptr + snapshot_len]
            .to_vec();

        let new_source = old_source
            .replace(
                "type BrowserState derive(PublicState):\n    BrowserState(String, List(String))",
                "type BrowserState derive(PublicState):\n    BrowserState(String, List(String))\n\ntype NextState derive(PublicState):\n    NextState(String, List(String), Int)",
            )
            .replace(
                "pub fn glamour_init(_root: UiRoot, _input: Bytes) -> BrowserState:\n    BrowserState(\"before\", [\"migration\"])",
                "pub fn glamour_init(_root: UiRoot, _input: Bytes) -> NextState:\n    NextState(\"fresh\", [], 0)",
            )
            .replace(
                "pub fn glamour_dispatch(state: BrowserState, _input: Bytes) -> BrowserState:\n    state",
                "pub fn glamour_dispatch(state: NextState, _input: Bytes) -> NextState:\n    state",
            )
            .replace(
                "pub fn glamour_emit(_state: BrowserState) -> Bytes:",
                "pub fn glamour_emit(_state: NextState) -> Bytes:",
            )
            .replace(
                "pub fn glamour_release(own state: BrowserState):\n    match state:\n        BrowserState(_, _) -> Nil",
                "pub fn glamour_release(own state: NextState):\n    match state:\n        NextState(_, _, _) -> Nil",
            )
            + "\nfn glamour_migrate(previous: BrowserState) -> NextState:\n    match previous:\n        BrowserState(name, items) -> NextState(name, items, 7)\n";
        std::fs::write(&source_path, new_source).expect("new source");
        let new_checked = check_project_development(&root).expect("new development build");
        let metadata = new_checked.development.as_ref().expect("new metadata");
        assert_eq!(metadata.snapshot_format(), 2);
        assert_eq!(metadata.migration_schema_hexes(), [old_schema]);

        let new_module = wasmtime::Module::new(&engine, &new_checked.wasm).expect("new Wasm");
        let mut new_store = wasmtime::Store::new(&engine, ());
        let new = linker
            .instantiate(&mut new_store, &new_module)
            .expect("new instance");
        let new_memory = new.get_memory(&mut new_store, "memory").expect("new memory");
        let new_reserve = new
            .get_typed_func::<i32, i32>(&mut new_store, "__glamour_input_reserve")
            .expect("new reserve");
        let restore = new
            .get_typed_func::<(i32, i32), i32>(&mut new_store, "__glamour_dev_restore")
            .expect("new restore");
        let restore_ptr = new_reserve
            .call(&mut new_store, snapshot_len as i32)
            .expect("reserve old snapshot");
        new_memory
            .write(&mut new_store, restore_ptr as usize, &snapshot)
            .expect("write old snapshot");
        restore
            .call(&mut new_store, (restore_ptr, snapshot_len as i32))
            .expect("typed migration restores old state into new model");
        let changes_ptr = new
            .get_typed_func::<(), i32>(&mut new_store, "__glamour_dev_changes")
            .expect("new changes")
            .call(&mut new_store, ())
            .expect("new changes pointer") as usize;
        assert_eq!(&new_memory.data(&new_store)[changes_ptr..changes_ptr + 3], [1, 1, 1]);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn development_source_names_are_project_relative_or_redacted() {
        let root = Path::new("/private/project");
        assert_eq!(
            source_module_name(root, "/private/project/src/main.witchy"),
            "src/main.witchy"
        );
        assert_eq!(
            source_module_name(root, "/Users/someone/secret.witchy"),
            "<external>/secret.witchy"
        );
        assert_eq!(
            source_module_name(root, "../outside.witchy"),
            "<external>/outside.witchy"
        );
        assert_eq!(source_module_name(root, "witchy/glamour"), "witchy/glamour");
    }

    #[test]
    fn creation_and_public_copy_refuse_overwrites_and_symlinks() {
        let existing = temp_path("existing");
        std::fs::create_dir(&existing).expect("mkdir");
        assert!(validate_destination(&existing).is_err());
        std::fs::remove_dir_all(existing).expect("cleanup");
    }

    #[test]
    fn static_import_stripping_keeps_dynamic_import_expressions() {
        let source = "import {\n  x,\n} from \"./x.mjs\";\nconst y = await import(\"node:y\");\n";
        assert_eq!(
            strip_static_imports(source),
            "const y = await import(\"node:y\");\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_inputs_cannot_escape_through_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temp_path("confined");
        create_project_atomically(&root, &client_fixture_files("confined-web", "confined_web"))
            .expect("create");
        let outside = temp_path("outside-index");
        std::fs::write(&outside, CLIENT_FIXTURE_INDEX).expect("outside");
        let index = root.join("web/index.html");
        std::fs::remove_file(&index).expect("remove generated index");
        symlink(&outside, &index).expect("link");
        let error = load_project(&root).expect_err("escape must fail");
        assert!(error.to_string().contains("resolves outside the project"));
        std::fs::remove_dir_all(root).expect("cleanup project");
        std::fs::remove_file(outside).expect("cleanup outside");
    }

    #[cfg(unix)]
    #[test]
    fn declared_static_content_rejects_symlinked_files() {
        use std::os::unix::fs::symlink;

        let root = temp_path("content-symlink");
        let mut files = static_fixture_files("content-symlink", "content_symlink");
        files
            .get_mut(Path::new("witchy.toml"))
            .expect("manifest")
            .push_str("content = \"content\"\n");
        files.insert(
            PathBuf::from("src/content_symlink.witchy"),
            "from glamour import Site, StaticContent\n\npub fn web(_content: StaticContent) -> Site:\n    glamour.site([glamour.static_page(\"/\", glamour.ui(glamour.text(\"safe\")))])\n"
                .into(),
        );
        files.insert(PathBuf::from("content/direct.md"), "safe".into());
        create_project_atomically(&root, &files).expect("create");
        let outside = temp_path("outside-content");
        std::fs::write(&outside, "not declared directly").expect("outside");
        symlink(&outside, root.join("content/linked.md")).expect("link content");

        let error = check_static_project(load_project(&root).expect("load"))
            .expect_err("content symlink must fail");
        assert!(error.to_string().contains("is a symlink"));
        std::fs::remove_dir_all(root).expect("cleanup project");
        std::fs::remove_file(outside).expect("cleanup outside");
    }

    #[test]
    fn protocol_manifest_drops_unrecognized_build_metadata() {
        let path = temp_path("manifest");
        std::fs::write(
            &path,
            CLIENT_FIXTURE_MANIFEST.replace(
                "\n}",
                ",\n  \"absoluteSource\": \"/private/source.witchy\",\n  \"credential\": \"not-an-artifact\"\n}",
            ),
        )
        .expect("write");
        let manifest = read_protocol_manifest(&path).expect("normalize");
        assert!(manifest.get("absoluteSource").is_none());
        assert!(manifest.get("credential").is_none());
        std::fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn protocol_manifest_checks_progressive_actions() {
        let path = temp_path("action-manifest");
        let mut source: Value =
            serde_json::from_str(CLIENT_FIXTURE_MANIFEST).expect("client fixture manifest");
        source["actions"] = json!([{
            "id": "glamour-form1-00260bc33b35b90bb6dce5d11da82aa2f1fc273d789f581676bbb5254880aba2",
            "method": "POST",
            "action": "/signup",
            "inputSchema": 1,
            "resultSchema": 1,
            "fields": [
                {"name": "email", "label": "Email", "kind": "email", "required": true},
                {"name": "password", "label": "Password", "kind": "secret", "required": true},
                {"name": "updates", "label": "Updates", "kind": "checkbox", "required": false},
            ],
        }]);
        std::fs::write(&path, pretty_json(&source).expect("JSON")).expect("write");
        let error = read_protocol_manifest(&path)
            .expect_err("client manifests cannot author transport schema identities");
        assert!(error.to_string().contains("unknown field `inputSchema`"));
        source["actions"][0]
            .as_object_mut()
            .expect("action")
            .remove("inputSchema");
        source["actions"][0]
            .as_object_mut()
            .expect("action")
            .remove("resultSchema");
        std::fs::write(&path, pretty_json(&source).expect("JSON")).expect("write");
        let manifest = read_protocol_manifest(&path).expect("checked action manifest");
        assert_eq!(manifest["actions"][0]["fields"][1]["kind"], "secret");
        assert_eq!(manifest["actions"][0]["inputSchema"], 2_859_606_054_u64);
        assert_eq!(manifest["actions"][0]["resultSchema"], 4_195_218_877_u64);
        assert_ne!(
            manifest["actions"][0]["inputSchema"],
            manifest["actions"][0]["resultSchema"]
        );

        source["actions"][0]["method"] = json!("GET");
        std::fs::write(&path, pretty_json(&source).expect("JSON")).expect("write");
        let error = read_protocol_manifest(&path)
            .expect_err("client manifests must not place secrets in GET URLs");
        assert!(error.to_string().contains("secret field `password` requires POST"));
        std::fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn static_islands_publish_closed_content_addressed_workers() {
        let root = temp_path("static-island-worker");
        let mut files = static_fixture_files("island-worker", "island_worker");
        files.insert(
            PathBuf::from("src/island_worker.witchy"),
            r#"import reflect
from glamour import Cmd, IslandPlan, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Finished(Result(Int, String))

type Auth:
    Auth(glamour.UiWorker)

fn authorize(root: UiRoot) -> Auth:
    Auth(glamour.worker_scope(root, "double", 4096, 4096, 2, 5000))

fn initial(_start: Start) -> Int:
    3

fn double(value: Int) -> Int:
    value * 2

fn start(auth: Auth, model: Int) -> Cmd(Msg):
    match auth:
        Auth(worker) -> glamour.worker("double", worker, double, model, fn(result: Result(Int, String)): Finished(result))

fn update(_auth: Auth, model: Int, message: Msg) -> (Int, Cmd(Msg)):
    match message:
        Finished(result) ->
            match result:
                Ok(value) -> (value, NoCmd)
                Err(_problem) -> (model, NoCmd)

fn render(model: Int) -> Ui(Msg):
    glamour.ui(glamour.element("p", [], [glamour.text("${model}")]))

fn subscriptions(_auth: Auth, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(Auth, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

fn static_view(model: Int) -> Ui(Msg):
    render(model)

pub fn web() -> Site:
    let plan = glamour.island("worker", app(), 3, static_view, glamour.OnLoad)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(plan)))]),
        [plan],
    )
"#.into(),
        );
        create_project_atomically(&root, &files).expect("create worker fixture");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("check worker fixture");
        let publication = checked.island_publication.as_ref().expect("publication");
        assert_eq!(publication.workers.len(), 1);
        let worker = &publication.workers[0];
        assert!(worker.file.starts_with("worker-"));
        assert!(worker.file.ends_with(".wasm"));
        assert!(worker.identity.starts_with("glamour-worker1-"));
        let artifact = &publication.artifact_manifest["artifacts"][0];
        let descriptor = artifact["effectDescriptors"]
            .as_object()
            .expect("effect descriptors")
            .values()
            .find(|descriptor| descriptor["semantic"] == "worker")
            .expect("worker descriptor");
        assert_eq!(descriptor["handler"], "worker");
        assert_eq!(descriptor["policy"]["artifact"], worker.identity);
        assert_eq!(descriptor["policy"]["url"], format!("/assets/{}", worker.file));
        assert_eq!(artifact["browserPolicy"]["workers"].as_array().expect("workers").len(), 1);
        assert_eq!(publication.artifact_manifest["workers"].as_array().expect("workers").len(), 1);
        let output = root.join("dist");
        write_static_production(&checked, &output).expect("publish worker site");
        let headers = std::fs::read_to_string(output.join("_headers")).expect("headers");
        assert!(headers.contains("worker-src 'self'"));
        assert!(output.join("assets").join(&worker.file).is_file());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn static_islands_authenticate_generic_host_port_types_and_registry_policy() {
        let root = temp_path("static-island-host-port");
        let mut files = static_fixture_files("island-host-port", "island_host_port");
        files.insert(
            PathBuf::from("src/island_host_port.witchy"),
            r#"import reflect
from glamour import Cmd, CredentialExchangeOutcome, CredentialExchangeRequest, HostPort, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Finished(Result(CredentialExchangeOutcome, String))

type Auth:
    Auth(HostPort(CredentialExchangeRequest, CredentialExchangeOutcome))

fn authorize(root: UiRoot) -> Auth:
    Auth(glamour.credential_get_exchange(root, "/auth/passkey/exchange"))

fn initial(_start: Start) -> Int:
    0

fn start(auth: Auth, _model: Int) -> Cmd(Msg):
    match auth:
        Auth(port) -> glamour.host_port("passkey.get", port, glamour.credential_exchange_request("{\"challenge\":\"AA\"}"), fn(result: Result(CredentialExchangeOutcome, String)): Finished(result))

fn update(_auth: Auth, model: Int, message: Msg) -> (Int, Cmd(Msg)):
    match message:
        Finished(result) ->
            match result:
                Ok(glamour.CredentialExchangeOutcome(status, succeeded)) -> (if succeeded: status else: model, NoCmd)
                Err(_problem) -> (model, NoCmd)

fn render(model: Int) -> Ui(Msg):
    glamour.ui(glamour.element("p", [], [glamour.text("${model}")]))

fn subscriptions(_auth: Auth, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(Auth, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

pub fn web() -> Site:
    let plan = glamour.island("host-port", app(), 0, render, glamour.OnLoad)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(plan)))]),
        [plan],
    )
"#.into(),
        );
        create_project_atomically(&root, &files).expect("create host-port fixture");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("check host-port fixture");
        let publication = checked.island_publication.as_ref().expect("publication");
        let artifact = &publication.artifact_manifest["artifacts"][0];
        let descriptor = artifact["effectDescriptors"]
            .as_object()
            .expect("effect descriptors")
            .values()
            .find(|descriptor| descriptor["semantic"] == "host-port")
            .expect("host-port descriptor");
        assert_eq!(descriptor["handler"], "port");
        assert_eq!(descriptor["policy"]["adapter"], "credential.get-exchange.v1");
        assert_eq!(descriptor["policy"]["endpoint"], "/auth/passkey/exchange");
        assert_eq!(descriptor["policy"]["maxRequestBytes"], 61_440);
        assert_eq!(descriptor["policy"]["maxResultBytes"], 512);
        assert_eq!(artifact["browserPolicy"]["ports"], json!(["credential.get-exchange.v1"]));
        let output = root.join("dist");
        write_static_production(&checked, &output).expect("publish host-port site");
        let headers = std::fs::read_to_string(output.join("_headers")).expect("headers");
        assert!(headers.contains("publickey-credentials-get=()"));
        let scripts = std::fs::read_dir(output.join("assets"))
            .expect("host-port assets")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "mjs"))
            .map(|entry| std::fs::read_to_string(entry.path()).expect("host-port runtime module"))
            .collect::<Vec<_>>();
        assert!(scripts.iter().any(|source| source.contains(
            "credential exchange requires an approved host-custody implementation",
        )));
        assert!(scripts.iter().all(|source| {
            !source.contains("navigator.credentials") && !source.contains("credential.toJSON")
        }));
        static_site::audit_static_island_artifacts(&output, &checked)
            .expect("audit host-port site");
        let source_path = root.join("src/island_host_port.witchy");
        let original = std::fs::read_to_string(&source_path).expect("host-port source");
        let dynamic = original
            .replace(
                "fn authorize(root: UiRoot) -> Auth:",
                "fn exchange_endpoint() -> String:\n    \"/auth/passkey/exchange\"\n\nfn authorize(root: UiRoot) -> Auth:",
            )
            .replace(
                "glamour.credential_get_exchange(root, \"/auth/passkey/exchange\")",
                "glamour.credential_get_exchange(root, exchange_endpoint())",
            );
        std::fs::write(&source_path, dynamic).expect("write dynamic host-port fixture");
        let error = check_static_project(load_project(&root).expect("dynamic project"))
            .expect_err("dynamic host-port endpoints must fail closed");
        assert!(error
            .to_string()
            .contains("must be bounded by compiler-visible literals"));
        std::fs::write(
            &source_path,
            original.replace("credential_get_exchange", "credential_create_exchange"),
        )
        .expect("write create-exchange fixture");
        let create = check_static_project(load_project(&root).expect("create project"))
            .expect("check create-exchange fixture");
        let create_descriptor = create
            .island_publication
            .as_ref()
            .expect("create publication")
            .artifact_manifest["artifacts"][0]["effectDescriptors"]
            .as_object()
            .expect("create descriptors")
            .values()
            .find(|descriptor| descriptor["semantic"] == "host-port")
            .expect("create host-port descriptor");
        assert_eq!(
            create_descriptor["policy"]["adapter"],
            "credential.create-exchange.v1",
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn static_islands_publish_typed_opaque_frame_compartments() {
        let root = temp_path("static-island-frame");
        let mut files = static_fixture_files("island-frame", "island_frame");
        files.insert(
            PathBuf::from("src/island_frame.witchy"),
            r#"import reflect
from glamour import Cmd, FrameEvent, Program, Site, Start, Sub, Ui, UiRoot

type Msg derive(Reflect):
    Activated(FrameEvent)

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn initial(_start: Start) -> Int:
    0

fn start(_auth: Nil, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: Nil, model: Int, message: Msg) -> (Int, Cmd(Msg)):
    match message:
        Activated(event) ->
            match event:
                glamour.FrameMessage(_value) -> (model + 1, NoCmd)

fn render(model: Int) -> Ui(Msg):
    glamour.ui(glamour.frame(glamour.document_renderer(), glamour.frame_text("Document ${model}"), glamour.static_ui(html"<p>Document unavailable</p>"), "document.activate", fn(event: FrameEvent): Activated(event)))

fn subscriptions(_auth: Nil, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(Nil, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

pub fn web() -> Site:
    let plan = glamour.island("frame", app(), 0, render, glamour.OnLoad)
    glamour.with_islands(
        glamour.site([glamour.static_page("/", glamour.ui(glamour.island_node(plan)))]),
        [plan],
    )
"#.into(),
        );
        create_project_atomically(&root, &files).expect("create frame fixture");
        let checked = check_static_project(load_project(&root).expect("project"))
            .expect("check typed frame fixture");
        let publication = checked.island_publication.as_ref().expect("publication");
        assert_eq!(publication.frames.len(), 1);
        let frame = &publication.frames[0];
        assert!(frame.file.starts_with("frame-"));
        assert!(frame.file.ends_with(".html"));
        assert!(frame.identity.starts_with("glamour-frame1-"));
        let artifact = &publication.artifact_manifest["artifacts"][0];
        let frame_record = &artifact["frames"][0];
        assert_eq!(frame_record["renderer"], "document.v1");
        assert_eq!(frame_record["artifact"], frame.identity);
        assert_eq!(frame_record["url"], format!("/assets/{}", frame.file));
        assert_eq!(frame_record["grant"], "Document 0");
        assert!(frame_record["nonce"]
            .as_str()
            .is_some_and(|nonce| nonce.starts_with("glamour-frame-nonce1-")));
        assert_eq!(artifact["browserPolicy"]["frames"].as_array().expect("frames").len(), 1);
        assert_eq!(publication.artifact_manifest["frames"].as_array().expect("frames").len(), 1);
        let output = root.join("dist");
        write_static_production(&checked, &output).expect("publish frame site");
        let headers = std::fs::read_to_string(output.join("_headers")).expect("headers");
        assert!(headers.contains("frame-src 'self'"));
        assert!(headers.contains("/assets/frame-*.html"));
        assert!(headers.contains("frame-ancestors 'self'"));
        let published = std::fs::read_to_string(output.join("index.html")).expect("page");
        assert!(published.contains("sandbox=\"allow-scripts\""));
        assert!(!published.contains("allow-same-origin"));
        assert!(!published.contains("Document 0"));
        assert!(published.contains(&format!("src=\"/assets/{}\"", frame.file)));
        assert!(output.join("assets").join(&frame.file).is_file());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(root: &Path, current: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in std::fs::read_dir(current).expect("read dir") {
                let entry = entry.expect("entry");
                if entry.file_type().expect("type").is_dir() {
                    walk(root, &entry.path(), out);
                } else {
                    out.push((
                        slash_path(entry.path().strip_prefix(root).expect("relative")),
                        std::fs::read(entry.path()).expect("read"),
                    ));
                }
            }
        }
        let mut output = Vec::new();
        walk(root, root, &mut output);
        output.sort_by(|left, right| left.0.cmp(&right.0));
        output
    }
