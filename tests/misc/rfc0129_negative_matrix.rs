//! RFC-0129 row 5: invalid concurrency programs fail while source structure remains intact.

use witchy::pipeline::PipelineStage;

#[derive(Clone, Copy)]
enum ExpectedRejection {
    Parse,
    PreLowerSemantic,
}

struct NegativeCase {
    name: &'static str,
    source: &'static str,
    rejection: ExpectedRejection,
    diagnostic_fragments: &'static [&'static str],
}

#[test]
fn rfc0129_acceptance_row_5_rejects_invalid_concurrency_sources_before_lowering() {
    let cases = [
        NegativeCase {
            name: "reference escape across await frame",
            source: r#"
mode opt

fn view(text: let('a) String) -> View(String, 'a):
    text

async fn main(console: Console):
    let text = "borrowed"
    let escaped = view(text)
    let _ = task.done(0).await
    console.print(escaped)
"#,
            rejection: ExpectedRejection::PreLowerSemantic,
            diagnostic_fragments: &["escaped", "live across `await`", ".owned()"],
        },
        NegativeCase {
            name: "capability transfer through worker result",
            source: r#"
import vm

fn leak_capability(dir: Dir, _input: Bytes) -> Dir:
    dir

fn invoke(dir: Dir, input: Bytes) -> Bytes:
    vm.with_dir(dir, leak_capability, input)

fn main(console: Console):
    console.print("unreachable")
"#,
            rejection: ExpectedRejection::PreLowerSemantic,
            diagnostic_fragments: &["vm.with_dir", "expected `Bytes`", "found `Dir`"],
        },
        NegativeCase {
            name: "async trait method syntax",
            source: r#"
trait Fetcher:
    async fn fetch(self) -> Int

fn main(console: Console):
    console.print("unreachable")
"#,
            rejection: ExpectedRejection::Parse,
            diagnostic_fragments: &["async", "trait method"],
        },
        NegativeCase {
            name: "discarded async task result",
            source: r#"
async fn compute() -> Int:
    42

async fn main(console: Console):
    compute()
    console.print("unreachable")
"#,
            rejection: ExpectedRejection::PreLowerSemantic,
            diagnostic_fragments: &["compute", "discard"],
        },
    ];

    for case in cases {
        let error = witchy::resolve_std_only_checked(case.source)
            .expect_err(case.name);
        let diagnostic = match (case.rejection, error) {
            (ExpectedRejection::Parse, witchy::ResolveStdError::Parse { error, .. }) => {
                error.to_string()
            }
            (
                ExpectedRejection::PreLowerSemantic,
                witchy::ResolveStdError::Pipeline(error),
            ) => {
                assert!(
                    matches!(error.stage(), PipelineStage::Link | PipelineStage::Source),
                    "{} was diagnosed after the source-proof boundary: {error}",
                    case.name,
                );
                error.to_string()
            }
            (_, error) => panic!("{} failed at the wrong boundary: {error}", case.name),
        };
        for fragment in case.diagnostic_fragments {
            assert!(
                diagnostic.contains(fragment),
                "{} diagnostic should contain {fragment:?}: {diagnostic}",
                case.name,
            );
        }
    }
}
