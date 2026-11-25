#![cfg(all(unix, any(target_arch = "aarch64", target_arch = "x86_64")))]

use std::{fs, process::Command};

#[test]
fn linked_pair_add_wraps_each_lane_and_uses_packed_arithmetic() {
    let fixture = super::Fixture::new();
    let dir = &fixture.0;
    let object = dir.join("add.o");
    let executable = dir.join("check");
    let cli = env!("CARGO_BIN_EXE_witchyc");
    let source = dir.join("kernels.wy");
    fs::write(
        &source,
        format!(
            "{}\n{}",
            include_str!("../../examples/native-simd-object.wy"),
            r#"
fn classify(a: U8x16, cutoff: U8x16) -> U8x16:
    return a.cmp_gt(cutoff).bit_and(a)

fn scale(a: U16x8, b: U16x8) -> U16x8:
    return a.wrapping_mul(b)

fn delta(a: U32x4, b: U32x4) -> U32x4:
    return a.wrapping_sub(b)
"#
        ),
    )
    .unwrap();
    assert!(
        Command::new(cli)
            .arg("build-object")
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .status()
            .unwrap()
            .success()
    );
    let caller = dir.join("caller.c");
    fs::write(
        &caller,
        r#"
typedef unsigned long long V __attribute__((vector_size(16)));
extern V add_pair(V, V);
extern V classify(V, V), scale(V, V), delta(V, V);
int main(void) {
    V result = add_pair((V){~0ULL, 10}, (V){1, 4});
    V bytes = classify((V){0x807fff01, 0xf0}, (V){0x7f807f00, 0xff});
    V halves = scale((V){0x8000ffff, 3}, (V){0x00020002, 9});
    V words = delta((V){0, 3}, (V){1, 5});
    return result[0] != 0 || result[1] != 14
        || bytes[0] != 0x8000ff01 || bytes[1] != 0
        || halves[0] != 0xfffe || halves[1] != 27
        || words[0] != 0xffffffff || words[1] != 0xfffffffe;
}
"#,
    )
    .unwrap();
    assert!(
        Command::new("clang")
            .args([&caller, &object])
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap()
            .success()
    );
    assert!(Command::new(&executable).status().unwrap().success());
    let (tool, option, start) = if cfg!(target_os = "macos") {
        ("otool", "-tv", "_add_pair:")
    } else {
        ("objdump", "-d", "<add_pair>:")
    };
    let asm = Command::new(tool)
        .arg(option)
        .arg(&object)
        .output()
        .unwrap();
    assert!(asm.status.success());
    let asm = String::from_utf8(asm.stdout).unwrap();
    let body = asm
        .split(start)
        .nth(1)
        .unwrap()
        .split("ret")
        .next()
        .unwrap();
    let mnemonic = if cfg!(target_arch = "x86_64") {
        "paddq"
    } else if cfg!(target_os = "macos") {
        "add.2d"
    } else {
        ".2d,"
    };
    assert!(body.contains(mnemonic), "{body}");
    let source = dir.join("invalid.wy");
    fs::write(
        &source,
        include_str!("../../examples/native-simd-object.wy").replace("wrapping_add", "divide"),
    )
    .unwrap();
    let rejection = Command::new(cli)
        .arg("build-object")
        .arg(source)
        .arg("-o")
        .arg(&object)
        .output()
        .unwrap();
    assert!(!rejection.status.success());
    assert!(String::from_utf8_lossy(&rejection.stderr).contains("unsupported vector operation"));
}
