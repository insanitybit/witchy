use super::*;

impl Checker<'_> {
    /// Labelled arguments (`greet(n: 1, by: 2)`, `Account(name: n, balance: 0)`): each label is the
    /// name of the parameter or field at its position, and is then taken away, so nothing after
    /// this reads one. Labels follow the order of the parameters, so the arguments are still
    /// evaluated in the order they are written.
    pub(super) fn labels(&self, body: &mut [a::Statement], sites: &mut BTreeMap<(usize,usize),(u32,String,String)>, enclosing: &str) -> Result<()> {
        let mut found: Option<Diagnostic> = None;
        a::visit_scoped(body, &mut Vec::new(), &mut |expr,names| {
                if found.is_some() { return true; }
                // A refusal here is placed at the call: the body has not been checked yet, so no
                // statement's place is known.
                let at = expr.span.clone();
                let error = |message: std::borrow::Cow<'static, str>| Diagnostic { span: at.clone(), ..error(message) };
                let a::Expression::Call(callee, args) = &mut expr.kind else {
                    if matches!(expr.kind, a::Expression::Labelled(..)) { found = Some(error("a label names a parameter of a call".into())); }
                    return false;
                };
                // A function with defaults, called by its name with fewer arguments than parameters.
                // Most calls have neither: nothing is looked up or copied for them.
                let labelled = args.iter().any(|arg| matches!(arg.kind, a::Expression::Labelled(..)));
                let defaulted = |defaults: &[Option<a::Expr>]| args.len() < defaults.len() && defaults.iter().any(Option::is_some);
                let direct = match &callee.kind {
                    a::Expression::Name(name) if !self.scope.contains_key(name) && !names.contains(name) => Some(self.function_named(name)),
                    a::Expression::Field(receiver, name) => match &receiver.kind {
                        a::Expression::Name(module) if !self.scope.contains_key(module) && !names.contains(module) => self.imports.iter()
                            .find(|(from, alias, _)| *from == self.module && alias == module)
                            .and_then(|(_, _, home)| {
                                let mangled = format!("{name}@{home}");
                                if self.signatures.contains_key(&mangled) { Some(mangled) }
                                else { self.signatures.get(name).filter(|s| s.module == *home).map(|_| name.clone()) }
                            }),
                        _ => None,
                    },
                    _ => None,
                };
                let signature = direct.as_ref().and_then(|key| self.signatures.get(key));
                let defaults = signature.filter(|sig| labelled || defaulted(&sig.defaults))
                    .map(|sig| sig.defaults.clone()).unwrap_or_default();
                let short = defaulted(&defaults);
                let site = |label: &str, sites: &mut BTreeMap<_,_>| {
                    sites.insert((at.start,at.end),(self.module,enclosing.to_string(),label.to_string()));
                    a::Expression::Integer(((at.start as u64)<<21) + at.end as u64 + 1)
                };
                for (position,arg) in args.iter_mut().enumerate() {
                    let (index,arg) = match &mut arg.kind {
                        a::Expression::Labelled(label,value) => (signature.and_then(|s|s.names.iter().position(|n|n==label)),value.as_mut()),
                        _ => (Some(position),arg),
                    };
                    if let a::Expression::SourceSite(label) = &arg.kind {
                        if !index.and_then(|i|signature.and_then(|s|s.defaults.get(i))).and_then(Option::as_ref)
                            .is_some_and(|d| matches!(&d.kind,a::Expression::SourceSite(label) if label.is_empty())) {
                            found=Some(error("source_site() is an argument to a source-site parameter".into())); return true;
                        }
                        arg.kind=site(label,sites);
                    }
                }
                if !short && !labelled { return false; }
                let names = signature.map(|sig| sig.names.clone()).or_else(|| match &callee.kind {
                    a::Expression::Name(name) => self.layouts.record_field_names(name),
                    _ => None,
                });
                // A default is a literal: where it is placed among the arguments cannot matter.
                let mut fill = |placed: &mut Vec<Option<a::Expr>>| -> bool {
                    for (slot, value) in placed.iter_mut().enumerate() {
                        if value.is_none() { match defaults.get(slot).cloned().flatten() { Some(mut d) => {
                            if matches!(&d.kind, a::Expression::SourceSite(label) if label.is_empty()) {
                                d.kind = site("",sites);
                                d.span = at.clone();
                            }
                            *value = Some(d)
                        }, None => return false } }
                    }
                    true
                };
                let Some(names) = names else {
                    found = Some(error("labels name the parameters of a function called by its name, or the fields of a record being made".into()));
                    return true;
                };
                // `Point(..p, x: 5)`: each field not written is read from `p`, a name or a field of
                // one (it is read once per field), after the fields written, in the record's order.
                if let Some(at) = args.iter().position(|arg| matches!(&arg.kind, a::Expression::Labelled(label, _) if label == "..")) {
                    let a::Expression::Name(record) = &callee.kind else { unreachable!() };
                    if self.layouts.record_field_names(record).is_none() || self.signatures.contains_key(&self.function_named(record)) {
                        found = Some(error("`..base` gives a record's other fields: it is written in making a record".into())); return true;
                    }
                    let a::Expression::Labelled(_, base) = args.remove(at).kind else { unreachable!() };
                    // `items[1]` too, when the position is a name or a number: reading it again runs nothing.
                    fn place(e: &a::Expr) -> bool { match &e.kind {
                        a::Expression::Name(_) => true,
                        a::Expression::Field(inner, _) => place(inner),
                        a::Expression::Index(inner, at) => place(inner) && matches!(at.kind, a::Expression::Name(_) | a::Expression::Integer(_)),
                        _ => false } }
                    if !place(&base) { found = Some(error("`..base` is a name, a field of one, or an element at a name or a number: bind the value with `let` first".into())); return true; }
                    if args.iter().any(|arg| !matches!(arg.kind, a::Expression::Labelled(..))) { found = Some(error("with `..base`, every field written is labelled".into())); return true; }
                    // The base's fields are read among the written values, so a written value does
                    // not name the base: with no globals, nothing else can change it, and where the
                    // reads fall cannot matter (red-team review: one that changed the base read it
                    // before or after depending on the type's field order).
                    fn root(e: &a::Expr) -> &str { match &e.kind { a::Expression::Field(inner, _) | a::Expression::Index(inner, _) => root(inner), a::Expression::Name(n) => n, _ => "" } }
                    let base_root = root(&base).to_string();
                    // Reading the base is fine (`Stats(..stats, words: stats.words + 1)`); what could
                    // change it is lending it mutably, calling a method on it, or moving it.
                    let changes = |e: &a::Expr| match &e.kind {
                        a::Expression::Unary("&mut" | "move", inner) => root(inner) == base_root,
                        a::Expression::Call(callee, _) => matches!(&callee.kind, a::Expression::Field(receiver, _) if root(receiver) == base_root),
                        _ => false,
                    };
                    if args.iter().any(|arg| crate::compiler::lending::any(arg, &mut |e| changes(e))) {
                        found = Some(error("a field written beside `..base` does not change the base (no `&mut`, `move` or method call on it): bind what it needs with `let` first".into())); return true;
                    }
                    if let Some(label) = args.iter().find_map(|arg| match &arg.kind { a::Expression::Labelled(label, _) if !names.contains(label) => Some(label.clone()), _ => None }) {
                        found = Some(error(format!("the record has no field `{label}`").into())); return true;
                    }
                    let slots: Vec<usize> = args.iter().map(|arg| match &arg.kind { a::Expression::Labelled(label, _) => names.iter().position(|n| n == label).expect("checked"), _ => unreachable!() }).collect();
                    if !slots.windows(2).all(|w| w[0] < w[1]) && !args.iter().all(quiet) {
                        found = Some(error("fields written out of the record's order are names, fields or literals, whose order of evaluation cannot matter".into())); return true;
                    }
                    let mut placed: Vec<Option<a::Expr>> = vec![None; names.len()];
                    for (slot, arg) in slots.into_iter().zip(args.drain(..)) {
                        let a::Expression::Labelled(_, value) = arg.kind else { unreachable!() };
                        if placed[slot].is_some() { found = Some(error("each field is given one value".into())); return true; }
                        placed[slot] = Some(*value);
                    }
                    *args = placed.into_iter().zip(&names).map(|(value, name)| value.unwrap_or_else(|| a::Expr { span: base.span.clone(), kind: a::Expression::Field(base.clone(), name.clone()) })).collect();
                    return false;
                }
                // Out of order only when evaluating the arguments does nothing (literals, names,
                // their fields, references to them): then the order they run in cannot matter.
                fn quiet(e: &a::Expr) -> bool {
                    match &e.kind {
                        a::Expression::Integer(_) | a::Expression::Float(_) | a::Expression::Bool(_) | a::Expression::Unit | a::Expression::Name(_) => true,
                        a::Expression::Field(inner, _) | a::Expression::Labelled(_, inner) => quiet(inner),
                        a::Expression::Unary("&" | "&mut" | "-" | "*", inner) => quiet(inner),
                        _ => false,
                    }
                }
                if let Some(label) = args.iter().find_map(|arg| match &arg.kind { a::Expression::Labelled(label, _) if !names.contains(label) => Some(label.clone()), _ => None }) {
                    found = Some(error(format!("no parameter is named `{label}` (they are {})", names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")).into()));
                    return true;
                }
                // Arguments keep the order they are written in when each one's parameter comes after
                // the one before it (some skipped), or when none of them does anything.
                fn in_order_or_quiet(args: &[a::Expr], names: &[String]) -> bool {
                    let mut last: Option<usize> = None;
                    let ordered = args.iter().enumerate().all(|(at, arg)| {
                        let slot = match &arg.kind { a::Expression::Labelled(label, _) => names.iter().position(|n| n == label).unwrap_or(usize::MAX), _ => at };
                        let fine = last.is_none_or(|l| slot > l); last = Some(slot); fine
                    });
                    ordered || args.iter().all(quiet)
                }
                let in_order = args.iter().enumerate().all(|(at, arg)| match &arg.kind { a::Expression::Labelled(label, _) => names.get(at) == Some(label), _ => true });
                // Arguments left out are given their defaults; a label may skip one by naming a later
                // parameter, and the skipped ones are filled the same way.
                let skips = args.iter().enumerate().any(|(at, arg)| matches!(&arg.kind, a::Expression::Labelled(label, _) if names.get(at) != Some(label)));
                if (short || skips) && in_order_or_quiet(args, &names) {
                    let mut placed: Vec<Option<a::Expr>> = vec![None; names.len()];
                    for (at, arg) in args.drain(..).enumerate() {
                        let (slot, value) = match arg.kind {
                            a::Expression::Labelled(label, value) => (names.iter().position(|n| *n == label).expect("checked"), *value),
                            _ => (at, arg),
                        };
                        if slot >= placed.len() || placed[slot].is_some() { found = Some(error("each parameter is given one argument".into())); return true; }
                        placed[slot] = Some(value);
                    }
                    if !fill(&mut placed) { found = Some(error("call argument count mismatch: a parameter without a default is left out".into())); return true; }
                    *args = placed.into_iter().map(|v| v.expect("filled")).collect();
                    return false;
                }
                if !in_order {
                    if !args.iter().all(quiet) {
                        found = Some(error("labelled arguments out of the parameters' order are names, fields or literals, whose order of evaluation cannot matter: write a call in its parameters' order".into()));
                        return true;
                    }
                    // Unlabelled arguments come first, at their own positions; each label names a later
                    // parameter once.
                    let mut placed: Vec<Option<a::Expr>> = vec![None; names.len()];
                    for (at, arg) in args.drain(..).enumerate() {
                        let (slot, value) = match arg.kind {
                            a::Expression::Labelled(label, value) => match names.iter().position(|n| *n == label) { Some(slot) => (slot, *value), None => { found = Some(error(format!("no parameter is named `{label}`").into())); return true; } },
                            _ => (at, arg),
                        };
                        if slot >= placed.len() || placed[slot].is_some() { found = Some(error("each parameter is given one argument".into())); return true; }
                        placed[slot] = Some(value);
                    }
                    if placed.iter().any(Option::is_none) { found = Some(error("call argument count mismatch".into())); return true; }
                    *args = placed.into_iter().map(|v| v.expect("checked")).collect();
                    return false;
                }
                for (at, arg) in args.iter_mut().enumerate() {
                    let a::Expression::Labelled(label, value) = &arg.kind else { continue };
                    match names.get(at) {
                        Some(name) if name == label => { let value = (**value).clone(); *arg = value; }
                        Some(name) => { found = Some(error(format!("argument {} is labelled `{label}`, but that parameter is `{name}`", at + 1).into())); return true; }
                        None => { found = Some(error("this call has more arguments than parameters".into())); return true; }
                    }
                }
                false
        });
        match found { Some(error) => Err(error), None => Ok(()) }
    }
}
