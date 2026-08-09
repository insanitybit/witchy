//! RFC-0008 ("A capability-pure frontend framework (MVU over VNode)") capstone
//! test: drive the FULL glamour MVU run loop headlessly.
//!
//! The committed Node driver (`web/witchy-runtime/glamour-dom.test.mjs`):
//!   1. compiles the `counter` demo rune to WASM via the real `witchy` binary
//!      (`witchy compile … --out …`), with glamour as a sibling module;
//!   2. mounts it through the DOM host shell (`web/witchy-runtime/glamour-dom.mjs`)
//!      into a self-contained fake DOM, asserts the initial render (a `<div>` with
//!      two buttons and a `<span>` showing 0), and that the + button carries a
//!      click handler (an `on` attr wired as `addEventListener`);
//!   3. simulates a `+` click — the handler dispatches the `Inc` message back into
//!      the pure rune, which folds it into count+1 — and asserts the `<span>`
//!      re-renders to 1, then 2; a `-` click decrements; and the differ patches
//!      the existing DOM in place (no wholesale replacement).
//!
//! That proves render + event -> update -> re-render end to end: the witchy core
//! stays pure (the `String -> String` `export_step` ABI), and the JS shell holds
//! all the authority (the DOM, the events). Node is the host engine; if `node` is
//! absent the test SKIPS cleanly so the suite stays green everywhere. The driver
//! is independently runnable: `node web/witchy-runtime/glamour-dom.test.mjs`.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

/// Whether a usable `node` is on PATH (>= the ESM/`node:` features the shell uses).
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_node_driver(relative_path: &str, success_marker: &str, label: &str) {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join(relative_path);
    assert!(driver.exists(), "the committed {label} driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .unwrap_or_else(|error| panic!("spawn {label} driver: {error}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the {label} test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains(success_marker),
        "{label} driver did not report {success_marker:?}:\n{stdout}"
    );
}

/// RFC-0108: malformed optimized-protocol frames fail before returning a
/// partial operation list, and sequence state advances only after successful
/// all-or-nothing application.
#[test]
fn glamour_binary_protocol_is_checked_before_application() {
    run_node_driver(
        "web/witchy-runtime/glamour-protocol.test.mjs",
        "GLAMOUR-PROTOCOL OK",
        "Glamour binary protocol",
    );
}

/// RFC-0108: a compiler-checked application export family keeps its private
/// state rooted in one Wasm instance and exposes only bounded byte-buffer
/// lifecycle calls to the host.
#[test]
fn glamour_stateful_wasm_abi_owns_the_application_model() {
    run_node_driver(
        "web/witchy-runtime/glamour-stateful-abi.test.mjs",
        "GLAMOUR-STATEFUL-ABI OK",
        "Glamour stateful Wasm ABI",
    );
}

/// RFC-0108: the optimized host mounts authenticated static templates, applies
/// changed slots, delegates one root listener per event class, and validates a
/// whole patch before its first live DOM mutation.
#[test]
fn glamour_optimized_host_mounts_patches_and_delegates_events() {
    run_node_driver(
        "web/witchy-runtime/glamour-optimized.test.mjs",
        "GLAMOUR-OPTIMIZED OK",
        "Glamour optimized DOM host",
    );
}

/// RFC-0109: a candidate restores against authenticated compiler metadata on a
/// detached root. Failure leaves the old application live; success disposes it
/// only after restoration and activates the candidate.
#[test]
fn glamour_development_swap_keeps_the_last_good_application() {
    run_node_driver(
        "web/witchy-runtime/glamour-development.test.mjs",
        "GLAMOUR-DEVELOPMENT OK",
        "Glamour development swap",
    );
}

/// RFC-0108: binary effect and subscription records resolve only
/// build-authenticated host descriptors. Stable generations cancel replaced
/// work and make late completions inert without exposing authority to Wasm.
#[test]
fn glamour_optimized_host_custodies_effects_and_subscriptions() {
    run_node_driver(
        "web/witchy-runtime/glamour-optimized-effects.test.mjs",
        "GLAMOUR-OPTIMIZED-EFFECTS OK",
        "Glamour optimized effect host",
    );
}

/// RFC-0108 end to end: compiled Witchy emits binary effect and subscription
/// plans, consumes typed completion records, and retains its private lifecycle
/// state while the host owns all asynchronous authority.
#[test]
fn glamour_optimized_wasm_effects_keep_authority_in_the_host() {
    run_node_driver(
        "web/witchy-runtime/glamour-optimized-effects-wasm.test.mjs",
        "GLAMOUR-OPTIMIZED-EFFECTS-WASM OK",
        "Glamour optimized compiled-Wasm effects",
    );
}

/// RFC-0108 differential oracle: one source runs through the native
/// interpreter plus JSON host, compiled Wasm plus JSON host, and compiled Wasm
/// plus optimized binary host. Normalized state agrees after every message.
#[test]
fn glamour_optimized_host_matches_the_json_reference_oracle() {
    run_node_driver(
        "web/witchy-runtime/glamour-differential.test.mjs",
        "GLAMOUR-DIFFERENTIAL OK",
        "Glamour optimized differential oracle",
    );
}

/// RFC-0108 Phase 3 performance evidence: the same keyed source runs for 30
/// measured samples through JSON and binary hosts while structural artifact,
/// memory, listener, and operation thresholds remain enforced.
#[test]
fn glamour_phase3_performance_report_enforces_structural_thresholds() {
    run_node_driver(
        "web/witchy-runtime/glamour-phase3-performance.mjs",
        "GLAMOUR-PHASE3-PERFORMANCE OK",
        "Glamour Phase 3 performance report",
    );
}

/// RFC-0108 end to end: a real compiled Witchy application retains its counter
/// model in Wasm and exchanges only binary event/mount/one-slot patch frames
/// with the optimized host.
#[test]
fn glamour_optimized_wasm_dispatch_has_no_model_or_vnode_json() {
    run_node_driver(
        "web/witchy-runtime/glamour-optimized-wasm.test.mjs",
        "GLAMOUR-OPTIMIZED-WASM OK",
        "Glamour optimized compiled-Wasm path",
    );
}

/// RFC-0108 keyed planning runs inside Witchy. Retained keys outside one LIS
/// produce exactly the minimum number of moves; removals precede right-to-left
/// inserts/moves so every `before` key is already live.
#[test]
fn glamour_keyed_plan_is_minimal_and_backends_agree() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-keyed-plan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    std::fs::write(
        work.join("keyed_plan.witchy"),
        r#"import glamour
from glamour import KeyedEdit

fn show(edit: KeyedEdit) -> String:
    match edit:
        RemoveKey(key) -> "remove ${key}"
        InsertKey(key, before) -> "insert ${key} before ${before}"
        MoveKey(key, before) -> "move ${key} before ${before}"

fn print_plan(console: Console, old_keys: List(Int), new_keys: List(Int)):
    match glamour.keyed_plan(old_keys, new_keys):
        Ok(edits) ->
            for edit in edits:
                console.print(show(edit))
        Err(message) -> console.print("error ${message}")

fn main(console: Console):
    print_plan(console, [1, 2, 3], [3, 1, 2])
    print_plan(console, [1, 2, 3], [2, 4, 3])
    print_plan(console, [1, 2, 3, 4], [4, 3, 2, 1])
    print_plan(console, [1, 1], [1])
"#,
    )
    .unwrap();

    let program = work.join("keyed_plan.witchy");
    let parity = Command::new(BIN)
        .arg("parity")
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run keyed planner parity");
    let parity_stdout = String::from_utf8_lossy(&parity.stdout);
    let parity_stderr = String::from_utf8_lossy(&parity.stderr);
    assert!(
        parity.status.success() && parity_stdout.contains("agree (7 line(s) of output)"),
        "keyed planner backends must agree:\n{parity_stdout}\n{parity_stderr}"
    );

    let run = Command::new(BIN)
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run keyed planner");
    let stdout = String::from_utf8(run.stdout).expect("keyed planner output is UTF-8");
    let _ = std::fs::remove_dir_all(&work);
    assert!(run.status.success(), "keyed planner run failed: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        stdout,
        "move 3 before 1\n\
         remove 1\n\
         insert 4 before 3\n\
         move 2 before 1\n\
         move 3 before 2\n\
         move 4 before 3\n\
         error glamour keyed plan: duplicate old key 1\n"
    );
}

/// RFC-0107 structural regions are planned in Witchy itself. Branches and
/// optional children leave with tags 8/13, enter from authenticated dormant
/// templates with tags 7/12, then apply changed scalar slots.
#[test]
fn glamour_structural_patch_frames_agree_on_both_backends() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work =
        std::env::temp_dir().join(format!("glamour-structural-patch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    std::fs::write(
        work.join("branch_patch.witchy"),
        r#"import glamour
from glamour import Ui

fn view(active: Bool, value: String) -> Ui(Int):
    glamour.ui(glamour.element("div", [], [glamour.branch("details", active, glamour.element("button", [glamour.on_event("details.tick", "click", glamour.event_msg(1))], [glamour.text(value)]))]))

fn child_view(active: Bool, value: String) -> Ui(Int):
    let child = if active: Some(glamour.element("button", [glamour.on_event("details.tick", "click", glamour.event_msg(1))], [glamour.text(value)])) else: None
    let template = glamour.element("button", [glamour.on_event("details.tick", "click", glamour.event_msg(1))], [glamour.text(value)])
    glamour.ui(glamour.element("div", [], [glamour.optional_child("summary", template, child)]))

fn event_view(active: Bool) -> Ui(Int):
    let attrs = if active: [glamour.on_event("details.tick", "click", glamour.event_msg(1))] else: []
    glamour.ui(glamour.element("button", attrs, [glamour.text("stable")]))

fn alternate_event_view() -> Ui(Int):
    glamour.ui(glamour.element("button", [glamour.on_event("details.alt", "click", glamour.event_msg(1))], [glamour.text("stable")]))

fn main(console: Console):
    let nodes = [glamour.island_text_node([0, 0, 0], 30)]
    let slots = [glamour.island_template_text_slot(70, [0, 0, 0])]
    let events = [glamour.island_event_descriptor(90, 20, 80, "details.tick", "click", "msg", false, false), glamour.island_event_descriptor(91, 20, 80, "details.alt", "click", "msg", false, false)]
    let regions = [glamour.island_branch_region([0, 0], 40, 10, 50, 60, [0, 0], slots)]
    let leave = glamour.island_patch(view(true, "zero"), view(true, "zero"), view(false, "one"), nodes, [], events, regions, 1, 2, 3, 1)
    console.print("${leave.length()},${leave.at(16)},${leave.at(48)},${leave.at(56)}")
    let enter = glamour.island_patch(view(false, "zero"), view(false, "one"), view(true, "two"), nodes, [], events, regions, 1, 2, 3, 2)
    console.print("${enter.length()},${enter.at(16)},${enter.at(48)},${enter.at(56)},${enter.at(60)},${enter.at(64)},${enter.at(68)},${enter.at(72)},${enter.at(84)},${enter.at(92)}")
    let child_regions = [glamour.island_child_region([0, 0], 41, 10, 51, 61, [0, 0], slots)]
    let unmount = glamour.island_patch(child_view(false, "zero"), child_view(true, "zero"), child_view(false, "one"), nodes, [], events, child_regions, 1, 2, 3, 3)
    console.print("${unmount.length()},${unmount.at(16)},${unmount.at(48)},${unmount.at(56)}")
    let mount = glamour.island_patch(child_view(false, "zero"), child_view(false, "one"), child_view(true, "two"), nodes, [], events, child_regions, 1, 2, 3, 4)
    console.print("${mount.length()},${mount.at(16)},${mount.at(48)},${mount.at(56)},${mount.at(60)},${mount.at(64)},${mount.at(68)},${mount.at(72)},${mount.at(84)},${mount.at(92)}")
    let add_event = glamour.island_patch(event_view(false), event_view(false), event_view(true), [glamour.island_text_node([0, 0], 30)], [], events, [], 1, 2, 3, 5)
    console.print("${add_event.length()},${add_event.at(16)},${add_event.at(48)},${add_event.at(56)},${add_event.at(60)},${add_event.at(64)}")
    let remove_event = glamour.island_patch(event_view(false), event_view(true), event_view(false), [glamour.island_text_node([0, 0], 30)], [], events, [], 1, 2, 3, 6)
    console.print("${remove_event.length()},${remove_event.at(16)},${remove_event.at(48)},${remove_event.at(56)},${remove_event.at(60)}")
    let replace_event = glamour.island_patch(event_view(true), event_view(true), alternate_event_view(), [glamour.island_text_node([0, 0], 30)], [], events, [], 1, 2, 3, 7)
    console.print("${replace_event.length()},${replace_event.at(16)},${replace_event.at(48)},${replace_event.at(56)},${replace_event.at(60)},${replace_event.at(64)}")
"#,
    )
    .unwrap();

    let program = work.join("branch_patch.witchy");
    let parity = Command::new(BIN)
        .arg("parity")
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run branch patch parity");
    let parity_stdout = String::from_utf8_lossy(&parity.stdout);
    let parity_stderr = String::from_utf8_lossy(&parity.stderr);
    assert!(
        parity.status.success() && parity_stdout.contains("agree (7 line(s) of output)"),
        "branch patch backends must agree:\n{parity_stdout}\n{parity_stderr}"
    );

    let run = Command::new(BIN)
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run branch patch planner");
    let stdout = String::from_utf8(run.stdout).expect("branch patch output is UTF-8");
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        run.status.success(),
        "branch patch run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        stdout,
        "60,1,8,40\n110,2,7,40,50,60,1,70,2,30\n60,1,13,41\n110,2,12,41,51,61,1,70,2,30\n68,1,14,20,80,90\n64,1,15,20,80\n68,1,14,20,80,91\n"
    );
}

/// RFC-0108 host work shares the same authenticated frame as DOM patches. The
/// Witchy encoder owns operation sizes, absolute payload references, and stable
/// ordering before JavaScript observes any effect or subscription request.
#[test]
fn glamour_island_host_work_frames_agree_on_both_backends() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work =
        std::env::temp_dir().join(format!("glamour-host-work-frame-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    std::fs::write(
        work.join("host_work.witchy"),
        r#"import glamour
from glamour import Ui

fn view() -> Ui(Int):
    glamour.ui(glamour.element("p", [], [glamour.text("stable")]))

fn main(console: Console):
    let work = [glamour.island_start_effect(11, 12, 13, "go"), glamour.island_cancel_effect(12), glamour.island_sync_subscription(21, 22, "50"), glamour.island_remove_subscription(21)]
    let frame = glamour.island_patch_with_work(view(), view(), view(), [], [], [], [], work, 7, 2, 3, 4)
    console.print("${frame.length()},${frame.at(16)},${frame.at(40)},${frame.at(48)},${frame.at(49)},${frame.at(56)},${frame.at(60)},${frame.at(64)},${frame.at(68)},${frame.at(72)},${frame.at(76)},${frame.at(77)},${frame.at(84)},${frame.at(88)},${frame.at(89)},${frame.at(96)},${frame.at(100)},${frame.at(104)},${frame.at(108)},${frame.at(112)},${frame.at(113)},${frame.at(120)},${frame.at(124)},${frame.at(125)},${frame.at(126)},${frame.at(127)}")
    let mount = glamour.island_mount_with_work(view(), 31, 32, [], [], [glamour.island_start_effect(41, 42, 43, "")], 7, 2, 3)
    console.print("${mount.length()},${mount.at(16)},${mount.at(40)},${mount.at(48)},${mount.at(56)},${mount.at(60)},${mount.at(76)},${mount.at(77)},${mount.at(84)},${mount.at(88)},${mount.at(92)},${mount.at(96)},${mount.at(100)}")
"#,
    )
    .unwrap();

    let program = work.join("host_work.witchy");
    let parity = Command::new(BIN)
        .arg("parity")
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run host-work parity");
    let parity_stdout = String::from_utf8_lossy(&parity.stdout);
    let parity_stderr = String::from_utf8_lossy(&parity.stderr);
    assert!(
        parity.status.success() && parity_stdout.contains("agree (2 line(s) of output)"),
        "host-work frame backends must agree:\n{parity_stdout}\n{parity_stderr}"
    );

    let run = Command::new(BIN)
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run host-work frame encoder");
    let stdout = String::from_utf8(run.stdout).expect("host-work output is UTF-8");
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        run.status.success(),
        "host-work frame run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        stdout,
        "128,4,124,0,1,11,12,13,124,2,1,1,12,2,1,21,22,126,2,3,1,21,103,111,53,48\n104,2,104,1,31,32,0,1,41,42,43,104,0\n"
    );
}

/// RFC-0107 effect completions cross the browser boundary as one bounded,
/// identity-checked binary record. Witchy performs the same validation on both
/// backends before a generated adapter may turn the result into a typed message.
#[test]
fn glamour_island_completion_frames_agree_on_both_backends() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work =
        std::env::temp_dir().join(format!("glamour-completion-frame-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    let mut frame = Vec::from([0_u8; 92]);
    frame[..4].copy_from_slice(b"GLMR");
    frame[4..6].copy_from_slice(&1_u16.to_le_bytes());
    frame[8] = 3;
    frame[10..12].copy_from_slice(&48_u16.to_le_bytes());
    frame[12..16].copy_from_slice(&92_u32.to_le_bytes());
    frame[16..20].copy_from_slice(&1_u32.to_le_bytes());
    frame[20..24].copy_from_slice(&7_u32.to_le_bytes());
    frame[24..28].copy_from_slice(&2_u32.to_le_bytes());
    frame[28..32].copy_from_slice(&3_u32.to_le_bytes());
    frame[32..36].copy_from_slice(&4_u32.to_le_bytes());
    frame[40..44].copy_from_slice(&88_u32.to_le_bytes());
    frame[48..50].copy_from_slice(&1_u16.to_le_bytes());
    frame[52..56].copy_from_slice(&40_u32.to_le_bytes());
    frame[56..60].copy_from_slice(&2_u32.to_le_bytes());
    frame[60..64].copy_from_slice(&11_u32.to_le_bytes());
    frame[64..68].copy_from_slice(&12_u32.to_le_bytes());
    frame[68..72].copy_from_slice(&13_u32.to_le_bytes());
    frame[72..76].copy_from_slice(&14_u32.to_le_bytes());
    frame[76..80].copy_from_slice(&1_u32.to_le_bytes());
    frame[80..84].copy_from_slice(&88_u32.to_le_bytes());
    frame[84..88].copy_from_slice(&4_u32.to_le_bytes());
    frame[88..].copy_from_slice(b"done");
    let source = format!(
        "import bytes\nimport glamour\n\nfn main(console: Console):\n    match bytes.from_list([{}]):\n        Err(_) -> console.print(\"bytes error\")\n        Ok(frame) ->\n            match glamour.island_completion_input(frame, 7, 2, 3, 4):\n                glamour.IslandCompletionInput(source, instance, generation, descriptor, schema, status, value) ->\n                    match value.decode_utf8_string():\n                        Ok(text) -> console.print(\"${{source}},${{instance}},${{generation}},${{descriptor}},${{schema}},${{status}},${{text}}\")\n                        Err(_) -> console.print(\"invalid payload\")\n",
        frame
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let program = work.join("completion.witchy");
    std::fs::write(&program, source).unwrap();
    let parity = Command::new(BIN)
        .arg("parity")
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run completion-frame parity");
    let parity_stdout = String::from_utf8_lossy(&parity.stdout);
    let parity_stderr = String::from_utf8_lossy(&parity.stderr);
    assert!(
        parity.status.success() && parity_stdout.contains("agree (1 line(s) of output)"),
        "completion-frame backends must agree:\n{parity_stdout}\n{parity_stderr}"
    );
    let run = Command::new(BIN)
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run completion-frame decoder");
    let _ = std::fs::remove_dir_all(&work);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "2,11,12,13,14,1,done\n");
}

/// Typed CSS assignments stay distinct from generic style strings and lower to
/// one authenticated custom-property operation in a protocol-1.3 frame on both
/// backends.
#[test]
fn glamour_custom_property_patch_agrees_on_both_backends() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work =
        std::env::temp_dir().join(format!("glamour-custom-property-patch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    std::fs::write(
        work.join("custom_property_patch.witchy"),
        r#"import glamour
from glamour import Ui

fn view(value: Int) -> Ui(Int):
    var attrs = []
    match glamour.css_length_property("gap"):
        Err(_) -> Nil
        Ok(gap) ->
            match glamour.css_px(value):
                Err(_) -> Nil
                Ok(current) -> attrs.push(glamour.css_custom_properties([glamour.css_assign(gap, current)]))
    glamour.ui(glamour.element("div", attrs, []))

fn main(console: Console):
    let sinks = [glamour.island_attribute_sink([0], 10, 0, "css-length", "--glamour-gap", 27)]
    let patch = glamour.island_patch(view(1), view(1), view(2), [], sinks, [], [], 1, 2, 3, 1)
    console.print("${patch.length()},${patch.at(6)},${patch.at(16)},${patch.at(48)},${patch.at(56)},${patch.at(60)},${patch.at(72)},${patch.at(73)},${patch.at(74)}")
"#,
    )
    .unwrap();

    let program = work.join("custom_property_patch.witchy");
    let parity = Command::new(BIN)
        .arg("parity")
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run custom-property patch parity");
    let parity_stdout = String::from_utf8_lossy(&parity.stdout);
    let parity_stderr = String::from_utf8_lossy(&parity.stderr);
    assert!(
        parity.status.success() && parity_stdout.contains("agree (1 line(s) of output)"),
        "custom-property patch backends must agree:\n{parity_stdout}\n{parity_stderr}"
    );

    let run = Command::new(BIN)
        .arg(&program)
        .current_dir(&work)
        .output()
        .expect("run custom-property patch");
    let stdout = String::from_utf8(run.stdout).expect("custom-property output is UTF-8");
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        run.status.success(),
        "custom-property patch failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(stdout, "75,3,1,18,10,27,50,112,120\n");
}

/// RFC-0107 Phase 0: the proposed Program/Site/Route/IslandPlan declaration
/// shape must compile using ordinary Witchy values and agree on both backends.
/// This is the evidence required before proposing framework-specific syntax.
#[test]
fn glamour_next_api_shape_needs_no_language_keyword() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-next-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/examples/next_api/src/next_api.witchy"),
        work.join("next_api.witchy"),
    )
    .unwrap();

    let out = Command::new(BIN)
        .arg("parity")
        .arg(work.join("next_api.witchy"))
        .current_dir(&work)
        .output()
        .expect("run parity on the RFC-0107 API prototype");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        out.status.success() && stdout.contains("agree (5 line(s) of output)"),
        "the RFC-0107 API prototype must compile and agree on both backends:\n\
         --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}

/// RFC-0107 Phase 1: rendering is a deterministic projection of public model
/// data. A Program cannot pass its authorization record into the view function.
#[test]
fn glamour_program_view_cannot_receive_authority() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!(
        "glamour-capability-free-view-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    std::fs::write(
        work.join("bad.witchy"),
        r#"import glamour
from glamour import Cmd, Program, Start, Sub, Ui, UiRoot

type Msg:
    Tick

fn authorize(root: UiRoot) -> UiRoot:
    root

fn initial(_start: Start) -> Int:
    0

fn start(_auth: UiRoot, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: UiRoot, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model, NoCmd)

fn authority_view(_auth: UiRoot, model: Int) -> Ui(Msg):
    glamour.ui(glamour.text("${model}"))

fn subscriptions(_auth: UiRoot, _model: Int) -> Sub(Msg):
    glamour.no_sub()

fn app() -> Program(UiRoot, Int, Msg):
    glamour.program(authorize, initial, start, update, authority_view, subscriptions)

fn main():
    let _app = app()
"#,
    )
    .unwrap();

    let out = Command::new(BIN)
        .args(["compile", "bad.witchy", "--out", "bad.wasm"])
        .current_dir(&work)
        .output()
        .expect("compile a Program with an authority-bearing view");
    let produced = work.join("bad.wasm").exists();
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        !out.status.success() && !produced,
        "a Program view that receives authority must not compile:\n{output}"
    );
}

/// RFC-0107 Phase 0: capture a reproducible before-measurement for the current
/// full-model/full-VNode JSON transport and structural DOM differ.
#[test]
fn glamour_reference_baseline_has_checked_transport_and_dom_counts() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-baseline.mjs");
    let out = Command::new("node")
        .arg(driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("run Glamour Phase 0 baseline");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the Glamour baseline failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("baseline output must be one JSON document");
    assert_eq!(report["schema"], "witchy.glamour.baseline.v1");
    assert_eq!(report["workload"]["finalModel"], 100);
    assert_eq!(report["transport"]["steps"], 101);
    assert_eq!(report["domOperations"]["setText"], 100);
    assert_eq!(report["domOperations"]["replaceChild"], serde_json::Value::Null);
    assert!(
        report["artifact"]["wasmBytes"].as_u64().unwrap_or(0) > 0,
        "baseline must measure a real compiled artifact: {report}"
    );
    assert!(
        report["memory"]["wasmPages"].as_u64().is_some_and(|pages| pages <= 2),
        "the counter baseline should remain within two Wasm pages: {report}"
    );
}

/// RFC-0107 Phase 1: Program initialization, declarative typed events, stable
/// subscription reconciliation, and unmount cancellation execute through the
/// unchanged JSON host boundary.
#[test]
fn glamour_program_typed_events_and_subscriptions_are_deterministic() {
    run_node_driver(
        "web/witchy-runtime/glamour-program.test.mjs",
        "GLAMOUR-PROGRAM OK",
        "Glamour Program runtime",
    );
}

/// (RFC-0039) The capability gate, made structural: a component WITHOUT a `UiFetch`
/// cannot construct `Cmd.Http`. `glamour.http_get` REQUIRES the token as its leading
/// argument, so a fetch attempt without one FAILS TO COMPILE — the unauthorized
/// effect is unrepresentable, not merely rejected by a host at runtime.
#[test]
fn glamour_http_effect_requires_a_uifetch_token() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    // Try to build an HTTP effect WITHOUT holding a `UiFetch`.
    std::fs::write(
        work.join("bad.witchy"),
        "import glamour\nfrom glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort\n\ntype Msg:\n    Got(Int, String)\n\npub fn go() -> Cmd(Msg):\n    glamour.http_get_compat(\"/x\", \"Got\")\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["compile", "bad.witchy", "--out", "bad.wasm"])
        .current_dir(&work)
        .output()
        .expect("spawn witchy compile");
    let produced = work.join("bad.wasm").exists();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        !out.status.success() && !produced,
        "an HTTP effect without a UiFetch MUST NOT compile (the capability gate). output:\n{msg}"
    );
}

/// (RFC-0039) The same gate on the credential-port effect: a component WITHOUT a
/// `CredentialPort` cannot invoke a host port. `glamour.port` REQUIRES the token as its
/// leading argument (the port NAME is carried by the token, not a free string), so an
/// attempt to run e.g. `promote` without holding that authority FAILS TO COMPILE. This is
/// the sharp case from the RFC's motivation — `Cmd.Port("promote", …)` from any component —
/// made unrepresentable rather than merely denied by host policy at runtime.
#[test]
fn glamour_port_effect_requires_a_credential_token() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-port-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    // Try to invoke the `promote` port WITHOUT holding a `CredentialPort` — passing the
    // port name as a bare string the way the old ambient API allowed.
    std::fs::write(
        work.join("bad.witchy"),
        "import glamour\nfrom glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort\n\ntype Msg:\n    Done(String)\n\npub fn go() -> Cmd(Msg):\n    glamour.port_compat(\"promote\", \"acme/charts\", \"Done\")\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["compile", "bad.witchy", "--out", "bad.wasm"])
        .current_dir(&work)
        .output()
        .expect("spawn witchy compile");
    let produced = work.join("bad.wasm").exists();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        !out.status.success() && !produced,
        "a port effect without a CredentialPort MUST NOT compile (the capability gate). output:\n{msg}"
    );
}

/// (RFC-0039) The DoD differential: a component CANNOT read another component's password.
/// A password entered into a `secret_input` stays in host custody; the rune holds only an
/// opaque `SecretRef`. `SecretRef`/`SecretInput` are sealed Glamour capabilities, so a
/// consuming module may HOLD and PASS one but cannot DESTRUCTURE it — an attempt to unwrap a
/// `SecretRef` to recover the host slot (the password's locator) FAILS TO COMPILE. Combined
/// with the secret never becoming a msg/model `String` (see the node driver below), a
/// sibling cannot observe another component's secret, by construction.
#[test]
fn glamour_password_is_unreadable_by_a_sibling_component() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-secret-seal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    // A component tries to read the secret behind an opaque `SecretRef` by destructuring the
    // sealed capability to recover the host slot.
    std::fs::write(
        work.join("bad.witchy"),
        "import glamour\nfrom glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort\n\nfn steal(r: SecretRef) -> String:\n    match r:\n        SecretRef(slot) -> slot\n\nfn main(console: Console, ui: UiRoot):\n    let input = glamour.secret_field(ui, \"login\", \"password\")\n    console.print(steal(glamour.secret_ref(input)))\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["compile", "bad.witchy", "--out", "bad.wasm"])
        .current_dir(&work)
        .output()
        .expect("spawn witchy compile");
    let produced = work.join("bad.wasm").exists();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        !out.status.success() && !produced,
        "destructuring another component's SecretRef MUST NOT compile (the secret seal). output:\n{msg}"
    );
}

/// (RFC-0039) The runnable proof of host secret custody. The committed Node driver
/// (`web/witchy-runtime/glamour-secret.test.mjs`) mounts a login rune whose `view` renders a
/// `secret_input` and whose `update` emits `submit_secret`, types a password, and submits.
/// It asserts the host credential port receives the REAL password while the rune's
/// model/view only ever hold a non-sensitive status and the port's result — the secret bytes
/// go host -> port and never enter the WASM.
#[test]
fn glamour_secret_input_keeps_the_password_host_side() {
    run_node_driver("web/witchy-runtime/glamour-secret.test.mjs", "GLAMOUR-SECRET OK", "glamour secret-custody");
}

/// RFC-0107 Phase 5: browser form entries are decoded against the published
/// action schema with the same problem order as Witchy. The delegated
/// same-origin lifecycle cancels stale work and never publishes secret values.
#[test]
fn glamour_progressive_forms_keep_secrets_host_side() {
    run_node_driver(
        "web/witchy-runtime/glamour-forms.test.mjs",
        "GLAMOUR-FORMS OK",
        "Glamour progressive form decoder",
    );
}

#[test]
fn glamour_dom_run_loop_renders_and_updates_on_events() {
    run_node_driver("web/witchy-runtime/glamour-dom.test.mjs", "GLAMOUR-DOM OK", "glamour DOM run-loop");
}

/// RFC-0008 EFFECTS-AS-DATA capstone: prove the rune -> `Cmd` -> shell-performs ->
/// msg -> update loop end to end, headlessly, with a FAKE CLOCK.
///
/// The committed Node driver (`web/witchy-runtime/glamour-dom-timer.test.mjs`)
/// compiles the `autocounter` demo — whose `update` returns `After(1000, Tick)` —
/// mounts it through the shell with an INJECTED fake timer (`opts.setTimeout`),
/// then advances the fake clock and asserts the count auto-increments WITHOUT any
/// user click. The rune holds no `Clock`: it only DESCRIBES the timer as a `Cmd`
/// value; the capability-HOLDING shell performs the `setTimeout` and dispatches the
/// deferred `Tick` back as a msg. That is "authority lives at the edge" made
/// observable: the effect runs, but only the shell could run it.
#[test]
fn glamour_dom_timer_effect_dispatches_msg_via_fake_clock() {
    run_node_driver("web/witchy-runtime/glamour-dom-timer.test.mjs", "GLAMOUR-DOM TIMER OK", "glamour timer");
}

/// (RFC-0040) The committed Node driver (`user-cap-export.test.mjs`) compiles a
/// cap-gated export `export_step(ui: UiRoot, input: String)`, instantiates it in the
/// pure browser host with a `[user_caps]` grant, and asserts the host-minted
/// `UiRoot`'s policy round-trips into the rune — and that a missing grant traps
/// (parity with the wasmtime host). This is the browser end of the app-root ABI,
/// proving the minted VALUE, not just that the wrapper is well-formed.
#[test]
fn user_cap_export_mints_uiroot_in_the_browser_host() {
    run_node_driver("web/witchy-runtime/user-cap-export.test.mjs", "RFC-0040", "RFC-0040 user-cap export");
}

/// The headline RFC-0008 property, asserted directly: the effects-using demo rune
/// has an EMPTY capability footprint. The `After(1000, Tick)` timer is the SHELL's
/// authority; the rune only describes it as data, so `witchy caps` must report no
/// `Net`/`Dir`/`Clock` for the app's view/update core. (`main`'s `Console` is
/// output, not authority — the per-export breakdown shows `export_step` is `(none)`
/// and only `main` carries `Console`.)
#[test]
fn glamour_autocounter_footprint_is_empty() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let app = manifest.join("projects/glamour/examples/autocounter/src/autocounter.witchy");
    assert!(app.exists(), "the autocounter demo must exist at {}", app.display());

    let out = Command::new(BIN)
        .arg("caps")
        .arg(&app)
        .current_dir(manifest)
        .output()
        .expect("run `witchy caps` on the autocounter rune");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "`witchy caps` failed:\n--- stderr ---\n{stderr}");
    // The effects-using export holds NO authority — the timer is the shell's.
    assert!(
        stdout.contains("export_step  (none)"),
        "the effects-using export must have an empty footprint:\n{stdout}"
    );
    // The rune touches no ambient authority: no Net/Dir/Clock anywhere.
    for forbidden in ["Net", "Dir", "Clock"] {
        assert!(
            !stdout.contains(forbidden),
            "the capability-pure rune must not demand `{forbidden}`:\n{stdout}"
        );
    }
}

/// RFC-0015 Phase A: the glamour DOM shell is XSS-safe at the attribute layer.
///
/// The committed Node driver (`web/witchy-runtime/glamour-dom-xss.test.mjs`)
/// compiles a rune whose `view` emits deliberately hostile attributes — a
/// `javascript:` href, a `javascript:` `<img>` src, and a string `onclick` handler
/// — mounts it through the real shell, and asserts the DOM was NEUTRALIZED: URL
/// attributes are scheme-checked (hostile schemes collapse to `#`), `on*` attributes
/// are never written (handlers attach only via the typed `on(event, msg)` path), and
/// safe URLs (plain/root-relative, https) pass through untouched. This guards the single
/// `applyAttr` choke point both the create and update render paths route through.
#[test]
fn glamour_dom_attribute_layer_neutralizes_xss() {
    run_node_driver("web/witchy-runtime/glamour-dom-xss.test.mjs", "GLAMOUR-DOM XSS OK", "glamour DOM XSS");
}

/// RFC-0015 Phase C: the async HTTP effect. The committed Node driver
/// (`web/witchy-runtime/glamour-http.test.mjs`) compiles a rune whose `update` returns
/// `http_get("/data", "GotData")` — the rune holds NO `Net`, only describes the request —
/// and drives it through the shell with an INJECTED fake fetch. It asserts the response
/// dispatches back as `GotData(status, body)` and updates the model, that the host shell
/// attached the session credential (`authHeaders`) ITSELF, and that the token never
/// entered the rune's state. This is the effects-as-data network access the coven-web
/// shell needs, with authority kept entirely at the host edge.
#[test]
fn glamour_http_effect_fetches_with_host_attached_auth() {
    run_node_driver("web/witchy-runtime/glamour-http.test.mjs", "GLAMOUR-HTTP OK", "glamour HTTP effect");
}

/// RFC-0015 Phase C: client-side routing. The committed Node driver
/// (`web/witchy-runtime/glamour-routing.test.mjs`) compiles a rune that maps `model` (the
/// current path) to a view and returns `navigate(path)` to change it — holding no history
/// authority, only describing the navigation. Driven with an injected fake history /
/// location / popstate, it asserts the initial path is delivered into the view, a `Nav`
/// pushes history AND re-renders, and a popstate (Back) re-delivers the route.
#[test]
fn glamour_routing_navigates_and_handles_back() {
    run_node_driver("web/witchy-runtime/glamour-routing.test.mjs", "GLAMOUR-ROUTING OK", "glamour routing");
}

/// RFC-0015 Phase C: keyed list reconciliation. The committed Node driver
/// (`web/witchy-runtime/glamour-keyed.test.mjs`) renders a `<ul>` of `keyed(k, <li>)`
/// children, marks each live node, reorders the list, and asserts the SAME node instances
/// appear in the new order — the host MOVED the existing nodes (preserving identity, and
/// thus a focused input's caret) rather than rebuilding by position. Index diffing would
/// fail this; key-based reconciliation passes it.
#[test]
fn glamour_keyed_list_reuses_nodes_on_reorder() {
    run_node_driver("web/witchy-runtime/glamour-keyed.test.mjs", "GLAMOUR-KEYED OK", "glamour keyed list");
}

/// RFC-0015 Phase D: the coven package-DETAIL page, built on glamour. The committed Node
/// driver (`web/witchy-runtime/glamour-package-page.test.mjs`) mounts the `package_page`
/// example and asserts it composes the registry's key view — the package identity (name,
/// version, copy-able install command), the capability FOOTPRINT as badges (coven's
/// differentiator), and the README + generated API docs rendered inline from Markdown via
/// `markdown.to_vnode`, with a README/Docs tab toggle. This is the template the coven-web
/// shell's package view is built on; it composes the Phase A–C pieces end to end.
#[test]
fn glamour_package_page_renders_identity_footprint_and_docs() {
    run_node_driver("web/witchy-runtime/glamour-package-page.test.mjs", "GLAMOUR-PACKAGE-PAGE OK", "glamour package page");
}

/// RFC-0015 Phase D: the d3-runes-chart COMPARTMENT renderer's chart logic. The committed
/// Node test (`projects/coven-web/web/dist/compartments/d3-runes-chart/chart.test.mjs`)
/// exercises the renderer's pure `barChartSvg(points) -> SVG` core — the foreign code that
/// would run isolated in the chart compartment. It asserts one bar per point, scaling to
/// the max, and that only NUMERIC counts reach the SVG (a hostile label cannot inject, so
/// even the in-box `innerHTML` is safe). The live rendering and the iframe/CSP isolation
/// are browser-verified; this gates the renderer's logic.
#[test]
fn d3_compartment_renderer_chart_logic_is_correct() {
    run_node_driver("projects/coven-web/web/dist/compartments/d3-runes-chart/chart.test.mjs", "RUNES-CHART OK", "D3 runes chart");
}

/// RFC-0015 Phase D: the coven catalog/index view on glamour. The committed Node driver
/// (`web/witchy-runtime/glamour-catalog.test.mjs`) mounts the `catalog` example and drives
/// its live search box, asserting that `on_input` carries the field's value into the MVU
/// loop (the rune holds no DOM) and that the keyed rune cards filter by name AND by
/// capability. This is the registry home page built on the framework, and it exercises the
/// `on_input` forms handler added for it.
#[test]
fn glamour_catalog_view_filters_by_name_and_capability() {
    run_node_driver("web/witchy-runtime/glamour-catalog.test.mjs", "GLAMOUR-CATALOG OK", "glamour catalog");
}

/// RFC-0015 Phase D: the version (record-detail) and trust (TUF) view templates render
/// their security fields, identically on both backends. These are the registry's audit
/// pages built on glamour: the version view surfaces the capability footprint, provenance,
/// and promote/yank controls; the trust view surfaces the TUF checks. The shell fills the
/// data from the registry; this gates the templates' rendering + parity.
#[test]
fn glamour_version_and_trust_views_render_security_fields() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-views-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();

    let views: &[(&str, &[&str])] = &[
        ("version_view", &["Js{d3-runes-chart}", "trusted-publisher", "state-released", "promote", "yank"]),
        ("trust_view", &["rollback check", "snapshot signature", "Registry trust"]),
    ];
    for (view, must_contain) in views {
        std::fs::copy(
            manifest.join(format!("projects/glamour/examples/{view}/src/{view}.witchy")),
            work.join(format!("{view}.witchy")),
        )
        .unwrap();
        let prog = work.join(format!("{view}.witchy"));

        // Both backends agree (the prime directive) ...
        let par = Command::new(BIN).arg("parity").arg(&prog).current_dir(&work).output().expect("witchy parity");
        let pout = String::from_utf8_lossy(&par.stdout);
        assert!(par.status.success() && pout.contains("agree"), "{view} must render identically on both backends:\n{pout}");

        // ... and the rendered VNode JSON carries the expected security fields.
        let run = Command::new(BIN).arg(&prog).current_dir(&work).output().expect("witchy run");
        let rout = String::from_utf8_lossy(&run.stdout);
        for needle in *must_contain {
            assert!(rout.contains(needle), "{view} should render `{needle}`:\n{rout}");
        }
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// RFC-0015 Phase D: the coven-web SHELL on glamour, end to end. The committed Node driver
/// (`web/witchy-runtime/glamour-coven-app.test.mjs`) mounts the `coven_app` rune with an
/// injected fake registry (fetch) and history, then drives the real app loop: the initial
/// route fetches the catalog and renders the rune list; clicking a rune navigates to its
/// package URL, which fetches and renders the docs. The rune holds no `Net` — the host shell
/// performs every fetch — so this proves the routed, data-fetched trusted shell works with
/// authority entirely at the edge (the production shell, minus the browser-only WebAuthn).
#[test]
fn glamour_coven_app_shell_routes_and_fetches() {
    run_node_driver("web/witchy-runtime/glamour-coven-app.test.mjs", "GLAMOUR-COVEN-APP OK", "glamour Coven app");
}

/// RFC-0041 Phase 1: The witchy Book itself as a glamour app — the dogfood. The committed
/// Node driver (`web/witchy-runtime/glamour-docs.test.mjs`) mounts the `docs` app
/// (`projects/docs`) with an injected fake content server (fetch) + history and drives the
/// real loop: the sidebar lists the book's pages, the initial route fetches a page and
/// renders its Markdown to real elements (via `std/markdown`), and clicking a page navigates
/// to its URL, fetches it, and renders it. The app holds no `Net` — the host shell performs
/// every fetch — so the docs SITE is a capability-pure witchy program.
#[test]
fn glamour_docs_app_renders_book_pages() {
    run_node_driver("web/witchy-runtime/glamour-docs.test.mjs", "GLAMOUR-DOCS OK", "glamour docs app");
}

/// RFC-0041: the host `Slot` — a subtree the host mounts and glamour NEVER diffs into. The
/// committed Node driver (`web/witchy-runtime/glamour-slot.test.mjs`) mounts a rune that emits
/// `glamour.slot("demo", data)` beside an ordinary counter, registers a `demo` renderer, and
/// asserts the slot renders via that renderer and — the point — that after a re-render (the
/// counter bumps) the host's widget node is the SAME instance (a host mutation to it survives)
/// and the renderer was not called again. This is the fix for the P2 wiring finding: a runnable
/// code cell mounted in a slot is not clobbered when glamour re-renders the page.
#[test]
fn glamour_slot_is_a_non_diffed_host_subtree() {
    run_node_driver("web/witchy-runtime/glamour-slot.test.mjs", "GLAMOUR-SLOT OK", "glamour slot");
}

/// RFC-0041: the DEPLOYABLE bundle, validated against the REAL book. The committed Node driver
/// (`web/witchy-runtime/glamour-docs-bundle.test.mjs`) runs `scripts/build-docs.sh` to assemble
/// the static site (the docs app → wasm + the real `book/src` content + web modules + manifest),
/// then mounts the bundle's `docs.wasm` with a fetch backed by the bundle's staged `content/` —
/// proving the actual deploy artifact renders the ACTUAL book: a full nav from the real
/// `SUMMARY.md`, real pages, and a real page's `witchy` fence becoming an editable runnable cell.
#[test]
fn glamour_docs_bundle_renders_the_real_book() {
    run_node_driver("web/witchy-runtime/glamour-docs-bundle.test.mjs", "GLAMOUR-DOCS-BUNDLE OK", "glamour docs bundle");
}

/// RFC-0015 Phase D: the host-shell PORT effect — the mechanism behind session login and
/// the WebAuthn passkey ceremony. The committed Node driver
/// (`web/witchy-runtime/glamour-port.test.mjs`) compiles a rune whose `update` returns
/// `port("passkeyLogin", "", "LoggedIn")` and drives it through an INJECTED port (the real
/// one would call `navigator.credentials` and hold the bearer token in the host). It asserts
/// the port runs on the login click, the signed-in identity renders, and ONLY the outcome
/// ("alice") — never a credential or token — enters the rune. This makes the session/WebAuthn
/// wiring suite-testable; only the real `navigator.credentials` port impl is browser-bound.
#[test]
fn glamour_port_effect_runs_session_login_at_the_host() {
    run_node_driver("web/witchy-runtime/glamour-port.test.mjs", "GLAMOUR-PORT OK", "glamour port effect");
}

/// RFC-0015 Phase D: the COMPLETE coven-web frontend as one glamour app — the suite-tested
/// core of the TypeScript->glamour migration. The committed Node driver
/// (`web/witchy-runtime/glamour-coven-web-app.test.mjs`) mounts `coven_web_app` with an
/// injected fake registry, history, and host ports, then drives the whole shell: catalog ->
/// version record -> passkey sign-in -> promote -> API docs -> registry trust. Every fetch
/// and the WebAuthn ceremony run in the host (the rune holds no `Net`, no token, empty
/// footprint), proving the trusted shell that replaces the TS app works with authority
/// entirely at the edge. (The JS bootstrap, build swap, and TS deletion are the follow-up,
/// browser-verified cutover.)
#[test]
fn glamour_coven_web_app_full_shell_works() {
    run_node_driver("web/witchy-runtime/glamour-coven-web-app.test.mjs", "GLAMOUR-COVEN-WEB-APP OK", "glamour Coven web app");
}

/// BUG-608 guard: the PRODUCTION host shell (`projects/coven-web/web/src/main.ts`) must keep
/// declaring the `[user_caps]` grant the compiled `coven_web_app` rune needs at boot. The
/// full-shell test above mounts with its OWN opts, so main.ts drifting off
/// `instantiateOpts: { userCaps: [["coven-web"]] }` (which blanked every from-source deploy)
/// stayed invisible. The committed driver
/// (`web/witchy-runtime/coven-web-shell-grant.test.mjs`) closes that: it proves the grant is
/// load-bearing (a mount WITHOUT it trips the boot trap), extracts the grant main.ts
/// actually passes to `mount`, and boots the rune under exactly that grant — so removing or
/// weakening the production line turns this red.
#[test]
fn coven_web_production_shell_declares_the_user_caps_grant() {
    run_node_driver("web/witchy-runtime/coven-web-shell-grant.test.mjs", "COVEN-WEB-SHELL-GRANT OK", "coven-web shell grant guard");
}

/// Regression: the `String -> String` WASM export ABI must not leak. The bump allocator
/// (`__galloc`) never frees, so a long-lived run loop (glamour MVU: one call per event) would
/// otherwise accumulate one call's allocations forever and eventually exhaust WASM memory —
/// `__galloc` returns an out-of-bounds pointer and the app crashes (observed after a few dozen
/// coven-web navigations). The fix exports each module's `__heap` pointer and the host
/// (`witchy-runtime.mjs`) resets it to its base after every call. The committed driver compiles
/// an allocation-heavy export and drives 3000 calls, asserting memory stays bounded and the heap
/// returns to base each time.
#[test]
fn wasm_string_export_does_not_leak_across_calls() {
    run_node_driver("web/witchy-runtime/heap-reset.test.mjs", "HEAP-RESET OK", "Wasm heap reset");
}

/// RFC-0015 Phase B: the `compartment` primitive isolates foreign code. The committed
/// Node driver (`web/witchy-runtime/glamour-compartment.test.mjs`) compiles a rune that
/// embeds `glamour.compartment("d3-runes-chart", grant, "ChartResized")` — dropping in a
/// third-party chart — and asserts the host shell renders it as a LOCKED-DOWN iframe
/// (`sandbox="allow-scripts"` with no `allow-same-origin` → opaque origin; loaded from the
/// sealed `/compartments/<id>/` path) and never inlines the foreign renderer or the grant
/// into the trusted DOM. The browser enforces the origin/CSP isolation at runtime; this
/// proves there is no code path that puts foreign content anywhere but the boxed frame —
/// the configuration behind "even a compromised d3 stays contained".
#[test]
fn glamour_compartment_isolates_foreign_code() {
    run_node_driver("web/witchy-runtime/glamour-compartment.test.mjs", "GLAMOUR-COMPARTMENT OK", "glamour compartment");
}

/// RFC-0015 Phase A3 PARITY: the Markdown renderer produces byte-identical VNode JSON on
/// the interpreter and the compiled WASM backend — the prime directive, for the pure
/// `markdown.to_vnode` string-processing path. Renders a document exercising headings,
/// bold, inline code, a sanitized link, and a list, then runs `witchy parity`.
#[test]
fn glamour_markdown_renders_identically_on_both_backends() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-md-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    for f in ["glamour.witchy", "markdown.witchy"] {
        std::fs::copy(manifest.join("projects/glamour/src").join(f), work.join(f)).unwrap();
    }
    let prog = "import glamour\nfrom glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort\nimport markdown\nimport json\nfrom json import Json\nimport reflect\n\
type Msg derive(Reflect):\n    Noop\n\n\
fn msg_to_json(m: Msg) -> Json:\n    json.from_value(m)\n\n\
fn main(console: Console):\n    \
console.print(glamour.to_json(markdown.to_vnode(\"# Title\\n\\nA **bold** word, `code`, a [link](https://example.com), and:\\n\\n- one\\n- two\\n\"), msg_to_json))\n";
    std::fs::write(work.join("mdparity.witchy"), prog).unwrap();

    let out = Command::new(BIN)
        .arg("parity")
        .arg(work.join("mdparity.witchy"))
        .current_dir(&work)
        .output()
        .expect("run witchy parity on the markdown program");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        out.status.success() && stdout.contains("agree"),
        "markdown rendering must be identical on both backends:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}

/// RFC-0015 Phase A3: the Markdown renderer (`markdown.to_vnode`) is XSS-safe.
///
/// The committed Node driver (`web/witchy-runtime/glamour-markdown.test.mjs`) compiles
/// a rune that renders deliberately hostile UNTRUSTED Markdown plus ordinary relative
/// links through `markdown.to_vnode`, mounts it, and asserts the DOM is inert: no
/// `<script>` element is ever created, unsafe hrefs become `#`, and plain relative
/// paths remain navigable. Normal Markdown (heading, bold, list) still renders to real
/// elements. This is the safe-by-construction README/doc rendering RFC-0015 relies on.
#[test]
fn glamour_markdown_renderer_is_xss_safe() {
    run_node_driver("web/witchy-runtime/glamour-markdown.test.mjs", "GLAMOUR-MARKDOWN OK", "glamour Markdown");
}

/// RFC-0008 DOGFOODING capstone: drive the glamour SYNTAX HIGHLIGHTER rune
/// headlessly — the proving ground for coven-web's sandbox-highlighter migration
/// (projects/coven-web/PLAN.md WS-I/M6).
///
/// The committed Node driver (`web/witchy-runtime/highlighter.test.mjs`) compiles
/// the `highlighter` demo to WASM, calls its `export_render({src})` export through
/// the RFC-0007 pure-compute host shim, parses the returned VNode JSON, and renders
/// it into a fake DOM with createElement/textContent ONLY. It asserts the
/// highlighted structure (a `pre>code`, the keyword `fn` in `span.kw`, the comment
/// in `span.com`, the string in `span.str`) AND — the security headline — that a
/// snippet containing `<script>` renders as ESCAPED text in a DOM text node, never
/// a live `<script>` element. Node is the host engine; if `node` is absent the
/// test SKIPS cleanly. The driver is independently runnable:
/// `node web/witchy-runtime/highlighter.test.mjs <witchy-binary>`.
#[test]
fn glamour_highlighter_renders_classed_spans_and_escapes_xss() {
    run_node_driver("web/witchy-runtime/highlighter.test.mjs", "HIGHLIGHTER OK", "glamour highlighter");
}

/// The headline RFC-0008 property for the highlighter: its render export is
/// capability-EMPTY. A syntax highlighter that renders genuinely-untrusted package
/// SOURCE is exactly where the empty-footprint + sandbox composition matters most,
/// so `witchy caps` must report no `Net`/`Dir`/`Clock` for `export_render`.
/// (`main`'s `Console` is output, not authority — the per-export breakdown shows
/// `export_render` is `(none)`.)
#[test]
fn glamour_highlighter_footprint_is_empty() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let app = manifest.join("projects/glamour/examples/highlighter/src/highlighter.witchy");
    assert!(app.exists(), "the highlighter demo must exist at {}", app.display());

    let out = Command::new(BIN)
        .arg("caps")
        .arg(&app)
        .current_dir(manifest)
        .output()
        .expect("run `witchy caps` on the highlighter rune");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "`witchy caps` failed:\n--- stderr ---\n{stderr}");
    // The render export holds NO authority — it only computes a VNode tree.
    assert!(
        stdout.contains("export_render  (none)"),
        "the highlighter render export must have an empty footprint:\n{stdout}"
    );
    // The rune touches no ambient authority: no Net/Dir/Clock anywhere.
    for forbidden in ["Net", "Dir", "Clock"] {
        assert!(
            !stdout.contains(forbidden),
            "the capability-pure highlighter must not demand `{forbidden}`:\n{stdout}"
        );
    }
}

/// Every committed `web/witchy-runtime/*.test.mjs` driver must be WIRED to a
/// Rust test — a suite that exists but is referenced by nothing never runs,
/// and dark tests read as coverage that isn't there (the 2026-08-07 published-
/// book CSP regression shipped green exactly this way; five suites, including
/// the 719-line islands suite, were dark). This meta-test makes darkness loud:
/// committing a new driver without a `run_node_driver` (or equivalent) wiring
/// fails HERE with the missing filename.
#[test]
fn every_node_test_driver_is_wired_to_a_rust_test() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let this_file = std::fs::read_to_string(manifest.join("tests/glamour/dom.rs"))
        .expect("read tests/glamour/dom.rs");
    let mut wired_elsewhere = String::new();
    // Drivers may legitimately be wired from other harness files.
    for extra in [
        "tests/browser/runtime.rs",
        "tests/browser/shim.rs",
        "tests/browser/encoding.rs",
        "tests/misc/wasm_abi_catalog.rs",
        "tests/test_for_paths.rs",
        "src/example_tests/glamour.rs",
        "scripts/check.sh",
    ] {
        if let Ok(text) = std::fs::read_to_string(manifest.join(extra)) {
            wired_elsewhere.push_str(&text);
        }
    }
    let mut dark = Vec::new();
    for entry in std::fs::read_dir(manifest.join("web/witchy-runtime")).expect("list drivers") {
        let name = entry.expect("driver entry").file_name();
        let name = name.to_string_lossy().into_owned();
        if !name.ends_with(".test.mjs") {
            continue;
        }
        if !this_file.contains(&name) && !wired_elsewhere.contains(&name) {
            dark.push(name);
        }
    }
    assert!(
        dark.is_empty(),
        "committed Node test drivers not wired to any Rust test (add a run_node_driver \
         test or delete the driver): {dark:?}"
    );
}

/// Compiler-lowered islands: publication, activation, and resume — the suite
/// that was dark when the published-book CSP regression shipped (see
/// `every_node_test_driver_is_wired_to_a_rust_test`).
#[test]
fn glamour_islands_publication_and_resume_work() {
    run_node_driver("web/witchy-runtime/glamour-islands.test.mjs", "GLAMOUR-ISLANDS OK", "glamour islands");
}

/// Resume differential: server-rendered island state must equal a fresh mount.
#[test]
fn glamour_resume_matches_fresh_mount_differentially() {
    run_node_driver(
        "web/witchy-runtime/glamour-resume-differential.test.mjs",
        "GLAMOUR-RESUME-DIFFERENTIAL OK",
        "glamour resume differential",
    );
}

/// Session/local storage host effects stay at the host boundary.
#[test]
fn glamour_storage_host_effects_work() {
    run_node_driver("web/witchy-runtime/glamour-storage.test.mjs", "GLAMOUR-STORAGE OK", "glamour storage");
}

/// The frame host protocol (init/grant/event handshake) for document frames.
#[test]
fn glamour_frame_host_protocol_works() {
    run_node_driver("web/witchy-runtime/glamour-frame.test.mjs", "glamour frame host tests passed", "glamour frame host");
}

/// Worker host effects: spawn/message/terminate at the host boundary.
#[test]
fn glamour_worker_host_effects_work() {
    run_node_driver("web/witchy-runtime/glamour-worker.test.mjs", "glamour worker host: ok", "glamour worker host");
}

/// Published runnable-cell adoption: identity retention, edited-source
/// execution, idempotence. (Sixth dark driver found by the wiring meta-test —
/// it covers the exact adopt path the 2026-08-07 highlight fix changed.)
#[test]
fn witchy_runnable_adoption_retains_identity() {
    run_node_driver(
        "web/witchy-runtime/witchy-runnable-adoption.test.mjs",
        "WITCHY-RUNNABLE-ADOPTION OK",
        "runnable cell adoption",
    );
}
