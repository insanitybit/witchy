use super::{Command, Fixture, fs};

#[test]
fn native_task_yields_and_resumes_with_live_locals() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"type Input:
    out: Console<Write>
    number: U64

fn pause_with(number: U64) -> U64 suspends:
    return memory.pause(number)

fn work(input: Input) suspends:
    let local = input.number + memory.first_word()
    let resumed = pause_with(local)
    if memory.in_task():
        input.out.print_int(local + resumed)

fn duplicate(allocator: &Allocator) -> Allocator:
    return memory.share(allocator)

fn main(allocator: Allocator, out: Console<Write>):
    let shared = duplicate(&allocator)
    let Ok(created): Task = Task.spawn(&shared, 4096, work, Input(move out, 35)) else Err(back):
        return
    var task = move created
    match memory.step(&mut task, 5):
        Ok:
            memory.finish(move task)
            return
        Err(token):
            let result = memory.step(&mut task, token - 38)
    memory.finish(move task)
"#,
    )
    .unwrap();
    build_task(&fixture);
    let run = Command::new(fixture.0.join("task"))
        .args(["--output", "--allocator"])
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"42\n");
}

#[test]
fn native_task_stack_overflow_hits_guard() {
    use std::os::unix::process::ExitStatusExt;

    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"type Input:
    out: Console<Write>
    depth: U64

fn descend(depth: U64) -> U64:
    if depth == 0:
        return 0
    return descend(depth - 1) + depth

fn work(input: Input) suspends:
    memory.pause(0)
    input.out.print_int(descend(input.depth))

fn main(allocator: Allocator, out: Console<Write>):
    let Ok(created): Task = Task.spawn(&allocator, 4096, work, Input(move out, 65536)) else Err(back):
        return
    var task = move created
    let first = memory.step(&mut task, 0)
    let second = memory.step(&mut task, 0)
    memory.finish(move task)
"#,
    )
    .unwrap();

    build_task(&fixture);
    let run = Command::new(fixture.0.join("task"))
        .args(["--output", "--allocator"])
        .output()
        .unwrap();
    // macOS reports a protection fault on the task stack as SIGBUS.
    let guard_fault = if cfg!(target_os = "macos") { 10 } else { 11 };
    assert_eq!(run.status.signal(), Some(guard_fault), "{run:?}");
    assert!(run.stdout.is_empty(), "{run:?}");
}

fn build_task(fixture: &Fixture) {
    let target = if cfg!(target_os = "macos") {
        "--target=macos-arm64"
    } else {
        "--target=linux-arm64"
    };
    let built = Command::new(env!("CARGO_BIN_EXE_witchyc"))
        .current_dir(&fixture.0)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["build", "main.wy", "-o", "task", target])
        .output()
        .unwrap();
    assert!(built.status.success(), "{built:?}");
}
