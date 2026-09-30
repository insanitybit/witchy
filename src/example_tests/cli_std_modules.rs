    /// `main -> Int` sets the process exit code (C/Go/Rust convention) and is
    /// *not* printed; `main` returning `()` exits 0 and shows its `print` output.
    #[test]
    fn main_int_return_is_the_process_exit_code() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "witchy_normal_exit_code_{}", std::process::id()
        )));
        std::fs::create_dir_all(&scratch.0).expect("create exit-code fixture directory");
        let run = |src: &str| {
            let path = scratch.0.join("main.witchy");
            std::fs::write(&path, src).expect("write exit-code fixture");
            crate::execute_file_exit(
                path.to_str().expect("UTF-8 fixture path"),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
            ).expect("run admitted source")
        };
        let (out, code) = run("fn main() -> Int:\n    7\n");
        assert!(out.is_empty(), "an Int return must not be printed, got {out:?}");
        assert_eq!(code, 7);
        let (out, code) = run("fn main(console: Console):\n    console.print(\"hi\")\n");
        assert_eq!(out, vec!["hi"]);
        assert_eq!(code, 0);
    }
