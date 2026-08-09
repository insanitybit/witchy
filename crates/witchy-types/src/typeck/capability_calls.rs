//! Capability-operation type checking and rights enforcement.

use witchy_syntax::{ast::Expr, cap_ops};

use super::{
    terr, Checker, ConsoleRights, DirRights, FileRights, NetRights, SecretRights, Ty, TypeError,
};

impl Checker {
    /// Resolve a call's first argument as a `Dir` capability and yield its rights.
    /// An unconstrained variable defaults to the full right-set (bare `Dir`).
    fn dir_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<DirRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::Dir(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::Dir(DirRights::full()))?;
                Ok(DirRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `Dir` capability but got `{other}`"
            )),
        }
    }

    /// Resolve a call's first argument as a `File` capability and yield its rights.
    /// An unconstrained variable defaults to the full right-set (bare `File`).
    fn file_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<FileRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::File(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::File(FileRights::full()))?;
                Ok(FileRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `File` capability but got `{other}`"
            )),
        }
    }

    /// Type-check a file-capability op (RFC-0012). A `File` is a leaf, so its ops
    /// take no path: `read(f: File[Read]) -> String` (arity 1) and
    /// `write(f: File[Write], data) -> Nil` (arity 2). Returns `Ok(None)` when the
    /// name/arity isn't a File op, so the Dir forms (`dir.read(path)` etc.) fall
    /// through — `read`/`write` are disambiguated from `Dir` by arity.
    pub(super) fn check_file_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        let Some(operation) = cap_ops::operation(name, cap_ops::ReceiverKind::File) else {
            return Ok(None);
        };
        let arity = operation.total_arity();
        if args.len() != arity {
            return Ok(None);
        }
        let rights = self.file_cap_rights(name, &args[0])?;
        for arg in &args[1..] {
            let at = self.infer(arg)?;
            self.unify(&Ty::String, &at).map_err(|e| TypeError {
                message: format!("in call to `{name}`: {}", e.message),
            })?;
        }
        let ret = match name {
            "read" => {
                if !rights.read {
                    return terr(format!("`read` needs `Read` but the file is `{rights}`"));
                }
                Ty::String
            }
            "write" => {
                if !rights.write {
                    return terr(format!("`write` needs `Write` but the file is `{rights}`"));
                }
                Ty::Unit
            }
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    pub(super) fn check_env_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        if name != "only" {
            return Ok(None);
        }
        let Some(receiver) = args.first() else {
            return Ok(None);
        };
        let receiver_ty = self.infer(receiver)?;
        if !matches!(self.resolve(&receiver_ty), Ty::Env) {
            return Ok(None);
        }
        let arity = cap_ops::operation(name, cap_ops::ReceiverKind::Env)
            .expect("Env.only is cataloged")
            .total_arity();
        if args.len() != arity {
            return terr(format!("`only` expects {arity} argument(s) but got {}", args.len()));
        }
        let names = self.infer(&args[1])?;
        self.unify(&Ty::List(Box::new(Ty::String)), &names)
            .map_err(|error| TypeError {
                message: format!(
                    "in call to `only`: Env names must be a List(String): {}",
                    error.message
                ),
            })?;
        Ok(Some(Ty::Env))
    }

    pub(super) fn check_console_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        let Some(operation) = cap_ops::operation(name, cap_ops::ReceiverKind::Console) else {
            return Ok(None);
        };
        let expected = operation.total_arity();
        if args.len() != expected {
            return terr(format!(
                "`{name}` expects {expected} argument(s) but got {}",
                args.len()
            ));
        }
        let cap = self.infer(&args[0])?;
        let rights = match self.resolve(&cap) {
            Ty::Console(rights) => rights,
            Ty::Var(_) => {
                self.unify(&cap, &Ty::Console(ConsoleRights::full()))?;
                ConsoleRights::full()
            }
            other => {
                return terr(format!(
                    "`{name}` expects a `Console` capability but got `{other}`"
                ));
            }
        };
        match name {
            "print" => {
                if !rights.write {
                    return terr(format!(
                        "`print` needs `Write` but the capability is `{rights}`"
                    ));
                }
                let message = self.infer(&args[1])?;
                self.unify(&Ty::String, &message).map_err(|error| TypeError {
                    message: format!("in call to `print`: {}", error.message),
                })?;
                Ok(Some(Ty::Unit))
            }
            "read_line" => {
                if !rights.read {
                    return terr(format!(
                        "`read_line` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ok(Some(Ty::String))
            }
            _ => unreachable!(),
        }
    }

    /// Type-check a directory-capability op, enforcing that the `Dir`'s rights
    /// permit the verb: `read`/`exists`/`subdir`/`list` need `Read`; `write`/
    /// `append`/`make_dir` need `Write`. (Narrowing is done with the `as`
    /// ascription, not per-op builtins.) Returns `Ok(None)` when `name` is not
    /// a Dir op.
    pub(super) fn check_dir_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        // RFC-0011: `only` is polymorphic — `dir.only(DirPolicy)` narrows a Dir's
        // entry policy (handled here); a `Net` receiver defers to `check_net_op`.
        if name == "only" {
            let arity = cap_ops::operation(name, cap_ops::ReceiverKind::Dir)
                .expect("Dir.only is cataloged")
                .total_arity();
            if args.len() != arity {
                return terr(format!("`only` expects {arity} argument(s) but got {}", args.len()));
            }
            let recv = self.infer(&args[0])?;
            let Ty::Dir(rights) = self.resolve(&recv) else {
                return Ok(None); // not a Dir.only — let check_net_op handle Net.only
            };
            if !rights.read {
                return terr(format!("`only` needs `Read` but the capability is `{rights}`"));
            }
            let pt = self.infer(&args[1])?;
            self.unify(&Ty::Named("DirPolicy".into(), Vec::new()), &pt)
                .map_err(|e| TypeError { message: format!("in call to `only`: {}", e.message) })?;
            return Ok(Some(Ty::Dir(rights)));
        }
        let Some(operation) = cap_ops::operation(name, cap_ops::ReceiverKind::Dir) else {
            return Ok(None);
        };
        let arity = operation.total_arity();
        if args.len() != arity {
            return terr(format!(
                "`{name}` expects {arity} argument(s) but got {}",
                args.len()
            ));
        }
        let rights = self.dir_cap_rights(name, &args[0])?;
        // The trailing arguments (path, and for `write` the content) are strings.
        for arg in &args[1..] {
            let at = self.infer(arg)?;
            self.unify(&Ty::String, &at).map_err(|e| TypeError {
                message: format!("in call to `{name}`: {}", e.message),
            })?;
        }
        let ret = match name {
            "read" => {
                if !rights.read {
                    return terr(format!("`read` needs `Read` but the capability is `{rights}`"));
                }
                Ty::String
            }
            "exists" => {
                if !rights.read {
                    return terr(format!(
                        "`exists` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Bool
            }
            "is_dir" => {
                if !rights.read {
                    return terr(format!(
                        "`is_dir` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Bool
            }
            "subtree" => {
                if !rights.read {
                    return terr(format!(
                        "`{name}` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Dir(rights)
            }
            "list" => {
                if !rights.read {
                    return terr(format!("`list` needs `Read` but the capability is `{rights}`"));
                }
                Ty::List(Box::new(Ty::String))
            }
            "write" | "append" => {
                if !rights.write {
                    return terr(format!(
                        "`{name}` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::Unit
            }
            "make_dir" => {
                if !rights.write {
                    return terr(format!(
                        "`make_dir` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::Unit
            }
            // RFC-0118 atomic primitives — all `Write`. `create_new` reports whether
            // this call won the exclusive create (`true`) or the path already existed
            // (`false`); `replace`/`rename` are atomic whole-file swaps returning Nil.
            "create_new" => {
                if !rights.write {
                    return terr(format!(
                        "`create_new` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::Bool
            }
            "replace" | "rename" => {
                if !rights.write {
                    return terr(format!(
                        "`{name}` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::Unit
            }
            // RFC-0012 navigation: a `Dir` opens a confined `File` (the leaf). The
            // name states the conferred right: `read_file` needs `Read` and yields
            // `File[Read]`; `write_file` needs `Write` and yields `File[Write]`.
            "read_file" => {
                if !rights.read {
                    return terr(format!(
                        "`read_file` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::File(FileRights { read: true, write: false })
            }
            "write_file" => {
                if !rights.write {
                    return terr(format!(
                        "`write_file` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::File(FileRights { read: false, write: true })
            }
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    /// Type-check the low-level `exec` op:
    /// `exec(exec, dir, path, args, stdin) -> String`.
    /// `Exec` is the right to spawn a subprocess; the executable is named through a
    /// `Dir[Read]` (the same confinement as `read`), so you can only run a file you
    /// can read. `args` is a single `\0`-joined argv string and the result is a
    /// `"<exit_code>\n<output>"` payload — the std `exec` module wraps this as
    /// `(Int, String)` over a `List(String)`. Returns `Ok(None)` when `name` is not
    /// `exec`.
    pub(super) fn check_exec_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        if name == "only" {
            let Some(receiver) = args.first() else {
                return Ok(None);
            };
            let receiver_ty = self.infer(receiver)?;
            if !matches!(self.resolve(&receiver_ty), Ty::Exec) {
                return Ok(None);
            }
            let arity = cap_ops::operation(name, cap_ops::ReceiverKind::Exec)
                .expect("Exec.only is cataloged")
                .total_arity();
            if args.len() != arity {
                return terr(format!("`only` expects {arity} argument(s) but got {}", args.len()));
            }
            let programs = self.infer(&args[1])?;
            self.unify(&Ty::List(Box::new(Ty::String)), &programs)
                .map_err(|error| TypeError {
                    message: format!(
                        "in call to `only`: Exec programs must be a List(String): {}",
                        error.message
                    ),
                })?;
            return Ok(Some(Ty::Exec));
        }
        if name != "exec" {
            return Ok(None);
        }
        let arity = cap_ops::operation(name, cap_ops::ReceiverKind::Exec)
            .expect("Exec.exec is cataloged")
            .total_arity();
        if args.len() != arity {
            return terr(format!(
                "`exec` expects (exec, dir, path, args, stdin) — {arity} arguments but got {}",
                args.len()
            ));
        }
        let e = self.infer(&args[0])?;
        self.unify(&Ty::Exec, &e).map_err(|err| TypeError {
            message: format!("`exec`'s first argument must be an `Exec` capability: {}", err.message),
        })?;
        let rights = self.dir_cap_rights("exec", &args[1])?;
        if !rights.read {
            return terr(format!(
                "`exec` needs a `Dir` with `Read` to locate the executable, but the capability is `{rights}`"
            ));
        }
        for (i, what) in [(2usize, "path"), (3, "args"), (4, "stdin")] {
            let at = self.infer(&args[i])?;
            self.unify(&Ty::String, &at).map_err(|err| TypeError {
                message: format!("in call to `exec`: {what} must be a String: {}", err.message),
            })?;
        }
        Ok(Some(Ty::String))
    }

    /// Type-check the narrow host Fetch ABI. The stdlib owns typed
    /// Request/Response conversion; the compiler sees only strings and an
    /// unforgeable, origin-scoped authority.
    pub(super) fn check_fetch_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        if name == "fetch" {
            let arity = cap_ops::operation(name, cap_ops::ReceiverKind::Net)
                .expect("Net.fetch is cataloged")
                .total_arity();
            if args.len() != arity {
                return terr(format!(
                    "`fetch` expects {arity} argument(s) but got {}",
                    args.len()
                ));
            }
            let rights = self.net_cap_rights(name, &args[0])?;
            if !rights.connect || !rights.tcp {
                return terr(format!(
                    "`fetch` needs `Net[Connect, Tcp]` but the capability is `{rights}`"
                ));
            }
            let origins = self.infer(&args[1])?;
            self.unify(&Ty::String, &origins).map_err(|err| TypeError {
                message: format!("in call to `fetch`: {}", err.message),
            })?;
            return Ok(Some(Ty::Fetch));
        }
        let Some(operation) = cap_ops::operation(name, cap_ops::ReceiverKind::Fetch) else {
            return Ok(None);
        };
        let Some(receiver) = args.first() else {
            return Ok(None);
        };
        let receiver_ty = self.infer(receiver)?;
        match self.resolve(&receiver_ty) {
            Ty::Fetch => {}
            Ty::Var(_) if name == "send_raw" => self.unify(&receiver_ty, &Ty::Fetch)?,
            _ => return Ok(None),
        }
        let arity = operation.total_arity();
        if args.len() != arity {
            return terr(format!(
                "`{name}` expects {arity} argument(s) but got {}",
                args.len()
            ));
        }
        for arg in &args[1..] {
            let actual = self.infer(arg)?;
            self.unify(&Ty::String, &actual).map_err(|err| TypeError {
                message: format!("in call to `{name}`: {}", err.message),
            })?;
        }
        Ok(Some(if name == "only" {
            Ty::Fetch
        } else {
            Ty::String
        }))
    }

    /// Resolve a call's first argument as a `Net` capability and yield its verbs.
    /// An unconstrained variable defaults to the full set (bare `Net`).
    fn net_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<NetRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::Net(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::Net(NetRights::full()))?;
                Ok(NetRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `Net` capability but got `{other}`"
            )),
        }
    }

    /// Type-check a network-capability op, enforcing the `Net`'s rights permit it:
    /// `connect` needs `Connect` (+`Tcp`); `listen` needs `Listen` (+`Tcp`);
    /// `restrict` is verb-neutral address attenuation (preserves the rights set).
    /// (Narrowing is done with the `as` ascription, not per-verb builtins.)
    /// Returns `Ok(None)` when `name` is not a Net op.
    pub(super) fn check_net_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        let Some(operation) = cap_ops::operation(name, cap_ops::ReceiverKind::Net) else {
            return Ok(None);
        };
        // `Net.fetch` has its distinct origin-scoped result contract above.
        if name == "fetch" {
            return Ok(None);
        }
        let arity = operation.total_arity();
        if args.len() != arity {
            return terr(format!(
                "`{name}` expects {arity} argument(s) but got {}",
                args.len()
            ));
        }
        let rights = self.net_cap_rights(name, &args[0])?;
        if name == "connect_pinned" || name == "try_connect_pinned" {
            // (RFC-0020) mixed trailing args: ip:String, host:String, port:Int, secure:Bool.
            for (arg, expected) in args[1..].iter().zip([Ty::String, Ty::String, Ty::Int, Ty::Bool]) {
                let at = self.infer(arg)?;
                self.unify(&expected, &at).map_err(|e| TypeError {
                    message: format!("in call to `{name}`: {}", e.message),
                })?;
            }
        } else if name == "listen_tls" {
            // (RFC-0060) mixed trailing args: addr:String, cert_pem:String, key:Secret.
            // The key is a `Secret` — never a String path or raw bytes — so the
            // private key stays host-side, consumed by handle. (RFC-0121) TLS serving
            // is a by-handle op, so it asks only for `Seal`; `coerce_arg` lets a bare
            // `Secret` stand in, while a `Secret[Seal]` is accepted as-is.
            let expected_key = Ty::Secret(SecretRights::sealed());
            for (arg, expected) in args[1..].iter().zip([Ty::String, Ty::String, expected_key]) {
                let at = self.infer(arg)?;
                self.coerce_arg(&expected, &at).map_err(|e| TypeError {
                    message: format!("in call to `{name}`: {}", e.message),
                })?;
            }
        } else {
            // The trailing argument: a typed `NetPolicy` for the policy verbs (`only`/`deny`,
            // RFC-0011), a `host:port` string for the address verbs (`connect`/`listen`/`restrict`),
            // a bare host string for `resolve`.
            let expected = if name == "only" || name == "deny" {
                Ty::Named("NetPolicy".into(), Vec::new())
            } else {
                Ty::String
            };
            for arg in &args[1..] {
                let at = self.infer(arg)?;
                self.unify(&expected, &at).map_err(|e| TypeError {
                    message: format!("in call to `{name}`: {}", e.message),
                })?;
            }
        }
        let ret = match name {
            "connect" => {
                if !rights.connect {
                    return terr(format!(
                        "`connect` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`connect` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Socket
            }
            // Like `connect` but total: returns `Option(Socket)` — `None` on a failed
            // dial instead of trapping. Lets a server (e.g. a proxy) survive a down
            // upstream. Same rights as `connect`.
            "try_connect" => {
                if !rights.connect {
                    return terr(format!(
                        "`try_connect` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`try_connect` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Named("Option".into(), vec![Ty::Socket])
            }
            "listen" => {
                if !rights.listen {
                    return terr(format!(
                        "`listen` needs `Listen` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`listen` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Listener
            }
            // (RFC-0060) HTTPS listen — the same rights as `listen` (the TLS layer
            // adds no network authority; the key's authority is the Secret itself).
            "listen_tls" => {
                if !rights.listen {
                    return terr(format!(
                        "`listen_tls` needs `Listen` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`listen_tls` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Listener
            }
            // (RFC-0020) Resolve a name to its IP literals. Gated on `Connect` alone (it
            // adds no authority — `connect_pinned` re-checks the chosen IP); no transport
            // requirement, since resolution is not itself a dial.
            "resolve" => {
                if !rights.connect {
                    return terr(format!(
                        "`resolve` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                Ty::List(Box::new(Ty::String))
            }
            // Pinned dials — same rights as `connect` (a literal-IP TCP dial), the hostname
            // carried only for SNI/Host. `try_` is total (`Option(Socket)`).
            "connect_pinned" => {
                if !rights.connect {
                    return terr(format!(
                        "`connect_pinned` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`connect_pinned` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Socket
            }
            "try_connect_pinned" => {
                if !rights.connect {
                    return terr(format!(
                        "`try_connect_pinned` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`try_connect_pinned` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Named("Option".into(), vec![Ty::Socket])
            }
            // Attenuating the address set leaves the rights (verbs + transports) intact.
            // `only` narrows a `Net` to a `NetPolicy`'s address set; `deny` subtracts an
            // address pattern (a monotone exclusion). Both preserve the rights set.
            "only" | "deny" => Ty::Net(rights),
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

}
