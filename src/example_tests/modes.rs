use super::*;
use crate::{codegen, parser};

    /// In a `mode opt` file, an ownership-relevant parameter (a heap buffer) must
    /// carry an explicit `let`/`var`/`own` convention; scalars and capabilities are
    /// exempt; an ordinary file is never enforced.
    #[test]
    fn mode_requires_ownership_conventions() {
        let unannotated = "mode opt\n\nfn tag(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    console.print(\"${tag([1, 2, 3])}\")\n";
        let err = crate::enforce_performance_modes(&link_mode(unannotated), "t")
            .expect_err("unannotated List param must be rejected in a mode file");
        assert!(err.contains("ownership convention"), "{err}");

        // The same code with `let` is accepted. `list.length` declares an
        // owned parameter, so an opt caller holding only call-scoped access
        // supplies the copy explicitly (RFC-0167 section 3 call protocol).
        let annotated = unannotated
            .replace("fn tag(xs:", "fn tag(let xs:")
            .replace("list.length(xs)", "list.length(xs.owned())");
        crate::enforce_performance_modes(&link_mode(&annotated), "t").expect("annotated param passes");

        // A scalar param needs no annotation even in a mode file.
        let scalar = "mode opt\n\nfn twice(n: Int) -> Int:\n    n + n\n\nfn main(console: Console):\n    console.print(\"${twice(3)}\")\n";
        crate::enforce_performance_modes(&link_mode(scalar), "t").expect("scalar param is exempt");

        // Bare capability values are authority tokens, not heap buffers; adding
        // `let`/`own`/`var` to them would be pure annotation noise. Keep this
        // list behind the shared capability predicate so new caps don't drift.
        let caps = "mode opt\n\nfn use_caps(console: Console, clock: Clock, rand: Rand, env: Env, exec: Exec, dir: Dir, file: File, net: Net, secret: Secret, store: SecretStore, sock: Socket, listener: Listener) -> Int:\n    1\n\nfn main(console: Console):\n    console.print(\"${1}\")\n";
        crate::enforce_performance_modes(&link_mode(caps), "t").expect("bare capabilities are exempt");

        // An aggregate that carries a capability is still a heap value; the
        // convention matters for the aggregate even though the bare cap is exempt.
        let cap_aggregate = "mode opt\n\nfn keep(maybe: Option(Secret)) -> Int:\n    1\n\nfn main(console: Console):\n    console.print(\"${1}\")\n";
        let err = crate::enforce_performance_modes(&link_mode(cap_aggregate), "t")
            .expect_err("cap-carrying aggregate still needs an ownership convention");
        assert!(err.contains("ownership convention") && err.contains("maybe"), "{err}");

        // Without a mode directive, the unannotated param is fine.
        let plain = unannotated.replacen("mode opt\n\n", "", 1);
        crate::enforce_performance_modes(&link_mode(&plain), "t").expect("non-mode file is not enforced");
    }

    /// In a mode file, an accumulator that reverts to the copying path inside a
    /// loop (a `Cliff`) is a hard error; in an ordinary file the same shape is
    /// accepted silently.
    #[test]
    fn mode_rejects_accumulator_cliff() {
        let cliff = "mode opt\n\nfn main(console: Console):\n    var xs = []\n    var snaps = []\n    for i in [1, 2, 3]:\n        list.push(snaps, xs)\n        list.push(xs, i)\n    console.print(\"${list.length(xs)}\")\n";
        let err = crate::enforce_performance_modes(&link_mode(cliff), "t")
            .expect_err("a repeated copy-revert in a mode file must be rejected");
        assert!(err.contains("rebuilt by copy"), "{err}");

        // The same body without the mode directive is accepted silently.
        let plain = cliff.replacen("mode opt\n\n", "", 1);
        crate::enforce_performance_modes(&link_mode(&plain), "t").expect("non-mode file is accepted");
    }
