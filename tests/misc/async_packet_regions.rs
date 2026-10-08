use witchy::opt::OptSet;
use witchy::runtime::{Capabilities, Runtime};

const SOURCE: &str = r#"from chan import Receiver, Sender

type Pair:
    Pair(Int, Int)

async fn produce(console: Console, tx: Sender(Pair), count: Int):
    var index: Int = 0
    var preparations: Int = 0
    while index < count:
        preparations = preparations + 1
        chan.send(tx, Pair(index + 4294967296, -index - 4294967296 - 1)).await
        index = index + 1
    console.print("sent=${index}:${preparations}")

async fn consume(console: Console, rx: Receiver(Pair)):
    var sum_left: Int = 0
    var sum_right: Int = 0
    var order: Int = 0
    for await packet in rx:
        match packet:
            Pair(left, right) ->
                sum_left = sum_left + left
                sum_right = sum_right + right
                order = order * 10 + left - 4294967296
    console.print("received=${sum_left}:${sum_right}:${order}")

async fn main(console: Console):
    console.print("before")
    let (tx, rx) = chan.channel(1).await
    chan.scope([produce(console, tx, 7), consume(console, rx)]).await
    console.print("joined")
"#;

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }



fn run(source: &str, options: OptSet, selected: bool) -> Vec<String> {
    witchy::opt::set_for_tests(Some(options));
    let checked = witchy::resolve_std_only_checked(source).expect("check closed scalar source");
    assert_eq!(checked.canonical_source().expect("canonical source").view().scalar_region().is_some(), selected);
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("admit scalar source");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut outputs = Vec::new();
    for _ in 0..2 {
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, print_int: true, clock: true, quiet: true, ..Capabilities::none() }, 256)
            .expect("spawn scalar source");
        vm.run().expect("execute scalar source");
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
        let first = vm.output();
        vm.run().expect("replay scalar root on same VM");
        assert_eq!(vm.canonical_heap_leaks().expect("replay heap audit"), Some(0));
        assert_eq!(vm.output(), [first.clone(), first.clone()].concat());
        outputs.push(first);
    }
    assert_eq!(outputs[0], outputs[1]);
    outputs.remove(0)
}

#[test]
fn closed_packet_lanes_preserve_both_fields_fifo_and_completion_order() {
    let _reset = Reset;
    let reference = run(SOURCE, OptSet::none(), false);
    assert!(reference.iter().any(|line| line == "received=30064771093:-30064771100:123456"));
    assert!(reference.iter().any(|line| line == "sent=7:7"));
    assert_eq!(run(SOURCE, OptSet::all(), true), reference);
}

#[test]
fn closed_packet_lanes_keep_empty_swapped_full_and_unbounded_rounds() {
    let _reset = Reset;
    for (count, capacity, swapped) in [(0,1,false),(7,0,true),(7,4,false),(7,-1,false)] {
        let mut source = SOURCE.replace("tx, 7)", &format!("tx, {count})"))
            .replace("chan.channel(1)", &format!("chan.channel({capacity})"));
        if swapped {
            source = source.replace(&format!("produce(console, tx, {count}), consume(console, rx)"),
                &format!("consume(console, rx), produce(console, tx, {count})"));
        }
        assert_eq!(run(&source, OptSet::all(), true), run(&source, OptSet::none(), false));
    }
}

#[test]
fn closed_packet_lanes_replay_large_signed_field_streams_without_owned_leaks() {
    let _reset = Reset;
    for count in [10000_i64, 100000] {
        let source = SOURCE.replace("tx, 7)", &format!("tx, {count})"))
            .replace("order = order * 10 + left - 4294967296", "order = order + 1");
        let left = count * 4294967296 + count * (count - 1) / 2;
        let right = -left - count;
        let output = run(&source, OptSet::all(), true);
        assert!(output.iter().any(|line| line == &format!("received={left}:{right}:{count}")));
        assert!(output.iter().any(|line| line == &format!("sent={count}:{count}")));
    }
}

#[test]
fn closed_packet_integer_layouts_preserve_all_lanes_and_fifo() {
    let _reset = Reset;
    for (width, capacity) in [(1, 1), (3, 4), (8, 0), (16, -1)] {
        let fields = vec!["Int"; width].join(", ");
        let values = (0..width).map(|lane| if lane % 2 == 0 {
            format!("index + 4294967296 + {lane}")
        } else { format!("-index - 4294967296 - {lane}") }).collect::<Vec<_>>().join(", ");
        let bindings = (0..width).map(|lane| format!("field_{lane}")).collect::<Vec<_>>();
        let source = format!(r#"from chan import Receiver, Sender

type Message:
    Message({fields})

async fn produce(tx: Sender(Message), count: Int):
    var index: Int = 0
    while index < count:
        chan.send(tx, Message({values})).await
        index = index + 1

async fn consume(console: Console, rx: Receiver(Message)):
    var total: Int = 0
    var order: Int = 0
    for await message in rx:
        match message:
            Message({patterns}) ->
                total = total + {sum}
                order = order * 10 + field_0 - 4294967296
    console.print("received=${{total}}:${{order}}")

async fn main(console: Console):
    let (tx, rx) = chan.channel({capacity}).await
    chan.scope([produce(tx, 7), consume(console, rx)]).await
"#, patterns = bindings.join(", "), sum = bindings.iter().enumerate().map(|(lane, name)| format!("{name} * {}", lane + 1)).collect::<Vec<_>>().join(" + "));
        let expected = (0..width).map(|lane| {
            let total = 7 * 4294967296 + 21 + 7 * lane as i64;
            (if lane % 2 == 0 { total } else { -total }) * (lane as i64 + 1)
        }).sum::<i64>();
        let ordinary = run(&source, OptSet::none(), false);
        assert_eq!(ordinary, [format!("received={expected}:123456")]);
        assert_eq!(run(&source, OptSet::all(), true), ordinary);
    }
}

#[test]
fn closed_packet_completion_clock_follows_the_final_receive() {
    let _reset = Reset;
    let source = include_str!("../../benchmarks/async_general/packet3.witchy")
        .replace("__MESSAGES__", "32")
        .replace("bench_ns=${bench_t1 - bench_t0}", "completed=${bench_t1 >= bench_t0}");
    let ordinary = run(&source, OptSet::none(), false);
    assert_eq!(ordinary, ["1584", "completed=true"]);
    assert_eq!(run(&source, OptSet::all(), true), ordinary);
}

#[test]
fn closed_packet_boolean_codecs_preserve_fifo_and_conditional_updates() {
    let _reset = Reset;
    let source = include_str!("../../benchmarks/async_general/packet_bool.witchy")
        .replace("__MESSAGES__", "32")
        .replace("bench_ns=${bench_t1 - bench_t0}", "completed=${bench_t1 >= bench_t0}");
    let reversed = source.replace("Packet(Int, Bool)", "Packet(Bool, Int)")
        .replace("Packet(i, i % 2 == 0)", "Packet(i % 2 == 0, i)")
        .replace("Packet(left, right)", "Packet(right, left)");
    for source in [source, reversed] {
        let ordinary = run(&source, OptSet::none(), false);
        assert_eq!(ordinary, ["496", "completed=true"]);
        assert_eq!(run(&source, OptSet::all(), true), ordinary);
    }
}
