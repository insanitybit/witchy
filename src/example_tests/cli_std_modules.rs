use crate::{interpreter, parser};

    /// `main -> Int` sets the process exit code (C/Go/Rust convention) and is
    /// *not* printed; `main` returning Nil exits 0 and shows its `print` output.
    #[test]
    fn main_int_return_is_the_process_exit_code() {
        let run = |src: &str| {
            let m = parser::parse_module(src).expect("parse");
            let l = crate::pipeline::link(vec![("main".into(), m)], "main").expect("link");
            interpreter::run_module_exit(l, ".", Vec::new(), Vec::new(), None).expect("run")
        };
        let (out, code) = run("fn main() -> Int:\n    7\n");
        assert!(out.is_empty(), "an Int return must not be printed, got {out:?}");
        assert_eq!(code, 7);
        let (out, code) = run("fn main(console: Console):\n    console.print(\"hi\")\n");
        assert_eq!(out, vec!["hi"]);
        assert_eq!(code, 0);
    }
