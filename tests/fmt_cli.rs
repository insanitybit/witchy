//! CLI regression test for BUG-012: `witchy fmt a.witchy b.witchy …` must format
//! EVERY file argument, not just the first. A shell glob (`witchy fmt std/*.witchy`)
//! expands to many paths, and the old single-arg handler silently dropped all but
//! the first while exiting 0 — a no-op that fooled callers (and the fmt gate).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

/// A program whose parenthesized `for (k, v)` tuple pattern fmt canonicalizes to
/// the unparenthesized `for k, v` — so formatting it is observably non-idempotent
/// (the file content must change). `<tag>` just keeps the two fixtures distinct.
fn unformatted(tag: &str) -> String {
    format!(
        "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"{tag}\", 1)\n    for (k, v) in dict.pairs(d):\n        print(console, \"${{k}}=${{v}}\")\n"
    )
}

#[test]
fn fmt_formats_every_file_argument() {
    let work = std::env::temp_dir().join(format!("witchy-fmt-multi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();

    let f1 = work.join("a.witchy");
    let f2 = work.join("b.witchy");
    let src1 = unformatted("a");
    let src2 = unformatted("b");
    std::fs::write(&f1, &src1).unwrap();
    std::fs::write(&f2, &src2).unwrap();

    // ONE invocation, BOTH files as arguments.
    let out = Command::new(BIN)
        .args(["fmt", f1.to_str().unwrap(), f2.to_str().unwrap()])
        .output()
        .expect("spawn witchy fmt");
    assert!(
        out.status.success(),
        "witchy fmt should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let got1 = std::fs::read_to_string(&f1).unwrap();
    let got2 = std::fs::read_to_string(&f2).unwrap();
    // Both files must have been rewritten to the canonical (unparenthesized) form —
    // the second one is the regression: it used to be silently skipped.
    assert_ne!(got1, src1, "first file must be formatted");
    assert_ne!(got2, src2, "SECOND file must ALSO be formatted (BUG-012)");
    assert!(got1.contains("for k, v in"), "a.witchy canonicalized: {got1}");
    assert!(got2.contains("for k, v in"), "b.witchy canonicalized: {got2}");

    // And the result is now clean: `--check` over both files exits 0.
    let check = Command::new(BIN)
        .args(["fmt", "--check", f1.to_str().unwrap(), f2.to_str().unwrap()])
        .output()
        .expect("spawn witchy fmt --check");
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        check.status.success(),
        "both files should now be canonical: {}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn fmt_refuses_to_delete_trailing_comments() {
    let work = std::env::temp_dir().join(format!("witchy-fmt-comments-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();

    let file = work.join("comments.witchy");
    let src = "fn main(console: Console):\n    let x = 1 // keep me\n    print(console, __render(x))\n";
    std::fs::write(&file, src).unwrap();

    let out = Command::new(BIN)
        .args(["fmt", file.to_str().unwrap()])
        .output()
        .expect("spawn witchy fmt");
    assert!(
        !out.status.success(),
        "witchy fmt must refuse unsupported comments instead of deleting them"
    );
    let got = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&work);
    assert_eq!(got, src, "failed fmt must leave the source untouched");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot format"),
        "stderr should explain refusal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
