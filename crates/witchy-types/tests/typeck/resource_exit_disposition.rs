//! Structured exits cannot borrow disposition from code they skip.
use super::source;

const WORKER: &str = r#"must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)
pub fn finish(own value: Box(Ticket)):
    ()
"#;
const CONTEXTS: [(&str, &str, bool); 4] = [
    ("", "", false), ("mode opt\n", "mode opt\n", false),
    ("", "mode opt\n", false), ("", "mode opt\n", true),
];

fn checked(root: &str, worker: &str, local: bool, body: &str) -> Result<(), String> {
    let mut main = format!("{root}import worker\n{body}");
    if local {
        main = main.replace("worker.finish(token)", "finish(token)");
        main.push_str("\nfn finish(own value: worker.Box(worker.Ticket)):\n    ()\n");
    }
    let worker = format!("{worker}{WORKER}");
    source::checked_bundle(vec![("main", &main), ("worker", &worker)], "main").map(drop)
}

fn losing_body(header: &str, exit: &str) -> String {
    format!(r#"fn choose(skip: Bool) -> Int:
    var index = 0
    {header}:
        index = index + 1
        let token = worker.Box(worker.Ticket(7))
        if skip:
            {exit}
        worker.finish(token)
    9
fn main() -> Int:
    choose(true)
"#)
}

#[test]
fn loop_exits_refuse_skipped_nested_resource_cleanup_across_modes() {
    for (root, worker, local) in CONTEXTS {
        for exit in ["break", "continue"] {
            let error = checked(root, worker, local, &losing_body("while index < 1", exit))
                .expect_err("cleanup after an exit cannot discharge that exit's resource");
            assert!(error.contains(exit) && error.contains("undisposed") && error.contains("token"), "{error}");
        }
    }
}

#[test]
fn iteration_syntax_shares_the_exit_obligation_boundary() {
    for mode in ["", "mode opt\n"] {
        for header in ["for item in [0]", "while let Some(item) = Some(0)"] {
            let error = checked(mode, mode, false, &losing_body(header, "break"))
                .expect_err("each loop spelling must check the same exited scopes");
            assert!(error.contains("break") && error.contains("undisposed"), "{error}");
        }
        let shadowed = losing_body("while index < 1", "break")
            .replace("    var index", "    let token = worker.Box(worker.Ticket(6))\n    var index")
            .replace("    9\n", "    worker.finish(token)\n    9\n");
        let error = checked(mode, mode, false, &shadowed)
            .expect_err("an outer same-name owner cannot disguise the exited inner obligation");
        assert!(error.contains("break") && error.contains("undisposed") && error.contains("token"), "{error}");
    }
}

#[test]
fn outer_owners_survive_and_disposition_before_exit_is_valid() {
    let body = r#"fn choose(skip: Bool) -> Int:
    let outer = worker.Box(worker.Ticket(7))
    var index = 0
    while index < 1:
        index = index + 1
        let token = worker.Box(worker.Ticket(8))
        var inner = 0
        while inner < 1:
            inner = inner + 1
            if skip:
                continue
            break
        worker.finish(token)
        if skip:
            continue
        break
    worker.finish(outer)
    9
fn main() -> Int:
    choose(true) * 10 + choose(false)
"#;
    for (root, worker, local) in CONTEXTS {
        checked(root, worker, local, body).expect("only iteration-local live obligations are lost by the exit");
    }
}

#[test]
fn own_parameter_transfer_does_not_erase_the_callees_must_obligation() {
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}must type Ticket:\n    Ticket(Int)\n\nfn vanish(own ticket: Ticket) -> Int:\n    1\n\nfn main() -> Int:\n    vanish(Ticket(7))\n");
        let error = source::checked(&source)
            .expect_err("the callee must dispose of its received Ticket on ordinary return");
        assert!(error.contains("ticket") && error.contains("must-consume"), "{error}");
    }
}
