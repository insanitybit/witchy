use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::lexer::lex;
use crate::token::{Kind, Token};

/// The marked call an opening's `brand` is read as; no source name can spell it.
pub const OPENING: &str = "#open";

mod constant;
mod generator;
mod private_types;
mod grants;
mod closure;
mod interpolation;
mod expression;
mod postcondition;
mod text_literal;
mod derive;
mod recipe;
mod static_if;
pub(crate) use static_if::{Target, debug_build};

/// Select the profile for a direct compiler CLI request on this thread.
#[doc(hidden)]
pub fn configure_debug_build(enabled: bool) {
    static_if::set_debug_build(enabled);
}

pub fn parse(source: &str) -> Result<Program, Diagnostic> { parse_modules(source, &[]) }

/// A type written as text, as a bound with a trait argument is kept (`Equivalent<K>`).
pub fn parse_type(text: &str) -> Result<Type, Diagnostic> {
    // The text is the compiler's own (a bound it wrote): its names may carry the characters no
    // source can spell, `Name@file` (private_types.rs) and `#...` (generated). They are read
    // as part of the name here, and only here (found by an adversarial review).
    let mut tokens = lex(&text.replace('@', "__witchy_at__").replace('#', "__witchy_hash__"))?;
    for token in &mut tokens {
        if let Kind::Name(name) = &mut token.kind && name.contains("__witchy_") { *name = name.replace("__witchy_at__", "@").replace("__witchy_hash__", "#"); }
    }
    let mut parser = Parser { tokens, cursor: 0, depth: 0, expression_depth: 0, texts: Vec::new(), modules: Vec::new(), constants: Default::default(), private: Default::default(), declared: Default::default(), uses: Vec::new(), generic_scope: Vec::new(), imports: Vec::new(), import_kinds: Vec::new(), variants: Default::default(), capabilities: Vec::new(), generator: false, extra: Vec::new(), shared_private: Default::default(), renamer: Default::default(), derived: Vec::new(), grantable: Vec::new(), generators: Vec::new(), inline_generators: Default::default(), fn_types: false, fn_locals: Vec::new(), scope_types: Vec::new(), call_arities: Default::default(), indent: 0, closure_shapes: Default::default(), view: Vec::new(), quiet: Vec::new(), record_fields: Default::default(), impl_parameters: false, results: Default::default(), recipes: Default::default(), formats: 0 };
    parser.ty()
}

/// Source the compiler wrote, lexed: its names may carry `@` (a private type, `Name@file`) and
/// `#`, which no source spells, so they are escaped around the lexer (as `parse_type` does).
/// Two files each declaring a private `type Key` with a derive made the derived source fail to
/// lex, reported as `unexpected character` inside another file (review round 4).
fn lex_written(text: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut tokens = lex(&text.replace('@', "__witchy_at__").replace('#', "__witchy_hash__"))?;
    for token in &mut tokens {
        if let Kind::Name(name) = &mut token.kind && name.contains("__witchy_") { *name = name.replace("__witchy_at__", "@").replace("__witchy_hash__", "#"); }
        // And in a literal's text (review: a recipe's `"a@b #x"` printed the escapes).
        if let Kind::Text(text) = &mut token.kind && text.contains("__witchy_") { *text = text.replace("__witchy_at__", "@").replace("__witchy_hash__", "#"); }
    }
    Ok(tokens)
}

/// Give compiler-written tokens the spans `place` chooses, keeping what the lexer saw of `<<` and
/// `>>`: the expression parser reads two `<` (or `>`) as a shift only when the second starts where
/// the first ends, and written tokens all share a span, so a recipe's or an interpolation's
/// `a << 8` read as `a < < 8` (found by the four-U32 grouping experiment, 2026-11-19).
fn respan(tokens: &mut [Token], mut place: impl FnMut(&Token) -> std::ops::Range<usize>) {
    let joined: Vec<bool> = (0..tokens.len()).map(|i| matches!(tokens[i].kind, Kind::Symbol("<" | ">"))
        && tokens.get(i + 1).is_some_and(|next| next.kind == tokens[i].kind && next.span.start == tokens[i].span.end)).collect();
    for token in tokens.iter_mut() { token.span = place(token); }
    for i in 0..tokens.len() {
        if joined[i] { let end = tokens[i].span.end; tokens[i + 1].span = end..end; }
    }
}

/// `modules` holds the offset at which each bundled source file starts.
pub fn parse_modules(source: &str, modules: &[usize]) -> Result<Program, Diagnostic> {
    parse_for_tests(source, modules, &Default::default(), None, None)
}

pub(crate) fn parse_for_tests(source: &str, modules: &[usize], members: &std::collections::BTreeSet<u32>, test_entry: Option<u32>, production_entry: Option<u32>) -> Result<Program, Diagnostic> {
    parse_target(source, modules, members, test_entry, production_entry, Target::Wasm)
}

pub(crate) fn parse_target(source: &str, modules: &[usize], members: &std::collections::BTreeSet<u32>, test_entry: Option<u32>, production_entry: Option<u32>, target: Target) -> Result<Program, Diagnostic> {
    let mut marked_declarations = Vec::new();
    // Derive recipes are templates, not declarations: taken out before anything reads tokens.
    let (source, recipes) = recipe::lift(source, modules)?;
    let source = source.as_str();
    let tokens = static_if::select(lex(source)?, target)?;
    // The program's own literals: what their `${...}` values may spell (interpolation.rs).
    interpolation::written_names(&tokens)?;
    let (separated, shared_private, renamer) = private_types::separate(tokens, modules)?;
    let (tokens, constants) = constant::lift(separated, modules)?;
    let mut parser = Parser {
        quiet: Vec::new(),
        tokens,
        cursor: 0,
        depth: 0,
        expression_depth: 0,
        texts: Vec::new(),
        modules: modules.to_vec(),
        constants,
        private: Default::default(),
        declared: Default::default(),
        uses: Vec::new(),
        generic_scope: Vec::new(),
        imports: Vec::new(), import_kinds: Vec::new(),
        variants: Default::default(),
        capabilities: Vec::new(),
        generator: false,
        extra: Vec::new(),
        shared_private,
        renamer,
        derived: Vec::new(),
        grantable: Vec::new(),
        generators: Vec::new(), inline_generators: Default::default(),
        fn_types: false,
        fn_locals: Vec::new(),
        scope_types: Vec::new(),
        call_arities: Default::default(),
        record_fields: Default::default(),
        impl_parameters: false,
        results: Default::default(),
        recipes: Default::default(),
        formats: 0,
        indent: 0,
        closure_shapes: Default::default(),
        view: Vec::new(),
    };
    parser.results = results(&parser.tokens, &parser.modules);
    parser.recipes = recipes;
    let mut declarations = Vec::new();
    while !parser.at(&Kind::End) {
        // `import NAME FILE`, written by bundling (wasm/files.rs) where the source said `import`.
        if matches!(&parser.token().kind, Kind::Name(n) if n == "import") {
            let importer = parser.module();
            parser.take();
            // A file that cannot be qualified is imported with no name (`import FILE`): the edge
            // counts for what this file may name bare, and no written name means it.
            let name = if matches!(parser.token().kind, Kind::Integer(_)) { String::new() } else { parser.name()? };
            // Bundling rewrites the imports at the head of a file to `import NAME FILE`; one written
            // anywhere else reaches here as written.
            let Kind::Integer(file) = parser.token().kind else { return Err(parser.error("an `import` goes at the top of the file, before every declaration")) };
            parser.take();
            // Whose the imported file is (wasm/files.rs): 1 the standard library's, 2 another
            // package's first file, and with nothing written the importer's own package's.
            let kind = if let Kind::Integer(kind) = parser.token().kind { parser.take(); kind as u8 } else { 0 };
            while parser.eat(&Kind::Newline) {}
            parser.imports.push((importer, name, file as u32));
            parser.import_kinds.push(kind);
            continue;
        }
        // `from shapes import Box, Corner`: the types are named bare in this file anyway, since
        // a type is one name per program; the line checks that the module offers them.
        if matches!(&parser.token().kind, Kind::Name(n) if n == "from") {
            parser.take();
            let at = parser.token().span.clone();
            let module = parser.name()?;
            let Some(file) = parser.qualifier(&module) else { return Err(Diagnostic::new("W0401", at, "`from` names a module this file imports")) };
            if !parser.keyword("import") { return Err(parser.error("expected `import` after the module's name")); }
            loop {
                let at = parser.token().span.clone();
                let name = parser.name()?;
                parser.offered(&module, file, &name, at)?;
                if !parser.symbol(",") { break; }
            }
            while parser.eat(&Kind::Newline) {}
            continue;
        }
        let provider = if parser.keyword("using") {
            let first = parser.name()?;
            let provider = if parser.symbol(".") {
                let module = parser.qualifier(&first).ok_or_else(|| parser.error("a qualified fixture provider names an imported module"))?;
                (module, parser.name()?)
            } else { (parser.module(), first) };
            parser.expect(Kind::Newline)?;
            Some(provider)
        } else { None };
        let marked = parser.keyword("test");
        if provider.is_some() && !marked {
            return Err(parser.error("a fixture provider precedes a test function"));
        }
        if marked && (!members.contains(&parser.module()) || !matches!(&parser.token().kind, Kind::Name(n) if n == "fn")) {
            return Err(parser.error("`test fn` belongs in a test module of the selected package"));
        }
        let entry = test_entry == Some(parser.module());
        let original = production_entry == Some(parser.module());
        let declaration = parser.declaration_in(entry,original)?;
        if matches!(&declaration,Declaration::Function(f) if f.name=="#production_main") { continue; }
        if marked {
            let Declaration::Function(f) = &declaration else { return Err(parser.error("expected a test function")) };
            if matches!(f.name.as_str(),"main" | "#package_schedule") || !f.generics.is_empty() || f.defaults.iter().any(Option::is_some) {
                return Err(Diagnostic::new("W0401", f.span.clone(), "a test case is a nongeneric free function with no default arguments"));
            }
            marked_declarations.push((declarations.len(), provider));
        }
        declarations.push(declaration);
        // A `gen fn` is several declarations (parser/generator.rs).
        declarations.append(&mut parser.extra);
    }
    // A built-in type's name is the language's, private or not: refused where it is declared
    // (review: a private `type U16` turned the library's uses of U16 into uses of a private type,
    // reported inside wasm-hash.wy).
    // Only with several files, where the privacy check below would otherwise report it inside
    // another file; one file keeps the checker's reserved-name messages.
    for (name, (_, span, function)) in parser.declared.iter().filter(|_| parser.modules.len() > 1) {
        if !function && constant::BUILT_IN_TYPES.contains(&name.as_str()) {
            return Err(Diagnostic::new("W0403", span.clone(), format!("`{name}` is a built-in type: give this type another name")));
        }
    }
    // `memory.` and `provider.` are the compiler's own operations in every file, the library's
    // included: nothing a program declares takes either name (the checker refuses it again).
    for (name, (_, span, _)) in parser.declared.iter() {
        if matches!(name.as_str(), "memory" | "provider") {
            return Err(Diagnostic::new("W0401", span.clone(), format!("`{name}` cannot be declared: it names the compiler's own operations (`{name}.…`) in every file of the program; choose another name")));
        }
    }
    // A type or trait declared without `pub` is used only in its own file. Here: every place
    // the syntax says a type or trait goes. Constructors and `Type.function` calls are
    // checked where the checker resolves them (`Program::private`).
    for (name, span) in &parser.uses {
        let Some(&home) = parser.private.get(name) else { continue };
        let here = parser.modules.iter().filter(|start| **start <= span.start).count().saturating_sub(1) as u32;
        if here != home {
            return Err(Diagnostic::new("W0401", span.clone(), "this type or trait is private to the module that declares it; that module marks what it offers with `pub`"));
        }
    }
    // Functions of one name in several files: each becomes `name@file`, and a call from a file
    // finds its own first, or names the file (wasm/check.rs). Names no other file shares keep
    // their spelling.
    let mut homes: std::collections::BTreeMap<String, std::collections::BTreeSet<u32>> = Default::default();
    for declaration in &declarations {
        if let Declaration::Function(f) = declaration && f.name != "main" && !f.name.contains(['.', '#']) { homes.entry(f.name.clone()).or_default().insert(f.module); }
    }
    for declaration in &mut declarations {
        if let Declaration::Function(f) = declaration && homes.get(&f.name).is_some_and(|files| files.len() > 1) {
            f.name = format!("{}@{}", f.name, f.module);
        }
    }
    let test_cases = marked_declarations.into_iter().map(|(index, provider)| {
        let Declaration::Function(case) = &declarations[index] else { unreachable!() };
        (case.name.clone(), provider)
    }).collect();
    // What the compiler itself declares (the `Text` methods, the call traits): in every file's
    // reach, as the built-in types are. Collected where each is written, never by its name.
    let mut language: std::collections::BTreeSet<String> = Default::default();
    // `Text` exists in a program that writes a literal or names the type.
    let named = parser.tokens.iter().any(|token| matches!(&token.kind, Kind::Name(name) if name == text_literal::TYPE || name == "str"));
    // The name is the language's: a program's own type of that name would be a second `Text`.
    let taken = declarations.iter().any(|declaration| matches!(declaration,
        Declaration::Record { name, .. } | Declaration::Sum { name, .. } | Declaration::Alias { name, .. } | Declaration::Trait { name, .. } if name == text_literal::TYPE));
    if taken { return Err(Diagnostic::new("W0401", 0..0, "`Text` is the built-in type of text literals: give this type another name")); }
    let checked_text = parser.tokens.iter().any(|token| matches!(&token.kind, Kind::Name(name) if matches!(name.as_str(), "str_from_bytes" | "from_utf8_bytes")));
    if named || !parser.texts.is_empty() || checked_text {
        let mut definitions = if named || !parser.texts.is_empty() {
            text_literal::definitions(&parser.texts)
        } else {
            String::new()
        };
        if checked_text {
            definitions.push_str(include_str!("parser/str_from_bytes.wy"));
        }
        let mut tokens = lex(&definitions)?;
        // The functions that hold a literal's bytes get names no source can spell.
        for token in &mut tokens {
            if let Kind::Name(name) = &mut token.kind { if matches!(name.as_str(), "validated_utf8_view" | "static_utf8_literal" | "str_from_bytes" | "str_from_bytes_step") { name.insert(0, '#'); } }
        }
        let mut written = Parser { tokens, cursor: 0, depth: 0, expression_depth: 0, texts: Vec::new(), modules: modules.to_vec(), constants: Default::default(), private: Default::default(), declared: Default::default(), uses: Vec::new(), generic_scope: Vec::new(), imports: Vec::new(), import_kinds: Vec::new(), variants: Default::default(), capabilities: Vec::new(), generator: false, extra: Vec::new(), shared_private: Default::default(), renamer: Default::default(), derived: Vec::new(), grantable: Vec::new(), generators: Vec::new(), inline_generators: Default::default(), fn_types: false, fn_locals: Vec::new(), scope_types: Vec::new(), call_arities: Default::default(), indent: 0, closure_shapes: Default::default(), view: Vec::new(), quiet: Vec::new(), record_fields: Default::default(), impl_parameters: false, results: Default::default(), recipes: Default::default(), formats: 0 };
        while !written.at(&Kind::End) {
            // The language's own `Text` methods are for every module.
            let mut declaration = written.declaration()?;
            match &mut declaration {
                Declaration::Function(f) => { f.public = true; language.insert(f.name.clone()); }
                Declaration::Impl { methods, target, .. } => for (m, _) in methods {
                    m.public = true;
                    if let Type::Named(owner, _) = &*target { language.insert(format!("{owner}.{}", m.name)); }
                },
                Declaration::Record { name, .. } => { language.insert(name.clone()); }
                _ => {}
            }
            declarations.push(declaration);
        }
    }
    grants::desugar(&mut declarations, &parser.grantable, modules)?;
    // `WitchyFn<n>` is the language's call trait, which closures and the checker's rules for
    // them rely on: a program implements it, never declares it (also private, `name@file`).
    for declaration in &declarations {
        if let Declaration::Trait { name, .. } = declaration && closure::reserved(name) {
            return Err(Diagnostic::new("W0401", 0..0, "`WitchyFn<n>` is the call trait of closures: a program may implement it for its own type, and does not declare it"));
        }
    }
    // The call-once trait is always there: the tasks library takes work as `W: WitchyOnce0<()>`.
    let mut arities = parser.call_arities.clone();
    arities.insert(closure::ONCE);
    let call_traits = closure::call_traits(&arities, modules)?;
    for declaration in &call_traits {
        if let Declaration::Trait { name, methods, .. } = declaration {
            language.insert(name.clone());
            for (method, _) in methods { language.insert(format!("{name}.{}", method.name)); }
        }
    }
    declarations.extend(call_traits);
    // The file each type, trait and capability is declared in (`pub` or not): what a bare name
    // may mean is decided by the files, never by the name being the only one of its spelling.
    let mut type_homes: std::collections::BTreeMap<String, (u32, usize)> = Default::default();
    for declaration in &declarations {
        match declaration {
            Declaration::Record { name, module, span, .. } => { type_homes.insert(name.clone(), (*module, span.start)); }
            Declaration::Alias { name, .. } | Declaration::Trait { name, .. } | Declaration::Sum { name, .. } => {
                if let Some((home, at, _)) = parser.declared.get(name) { type_homes.entry(name.clone()).or_insert((*home, at.start)); }
            }
            _ => {}
        }
    }
    // Each file's package, as a number files of one package share (bundling says whose each
    // imported file is): files joined by relative imports are one package, the standard
    // library's files are one, and a package's first file starts its own.
    let count = modules.len().max(1);
    let mut package: Vec<u32> = (0..=count as u32).collect();
    fn root(package: &mut [u32], mut at: u32) -> u32 { while package[at as usize] != at { package[at as usize] = package[package[at as usize] as usize]; at = package[at as usize]; } at }
    for ((from, _, to), kind) in parser.imports.iter().zip(&parser.import_kinds) {
        if *from as usize >= count || *to as usize >= count { continue; }
        let (a, b) = match kind { 0 => (*from, *to), 1 => (count as u32, *to), _ => continue };
        let (a, b) = (root(&mut package, a), root(&mut package, b));
        if a != b { package[a.max(b) as usize] = a.min(b); }
    }
    let standard = root(&mut package, count as u32);
    let packages: Vec<(u32, bool)> = (0..count as u32).map(|file| { let r = root(&mut package, file); (r, r == standard) }).collect();
    Ok(Program { text_literals: parser.texts, type_uses: std::mem::take(&mut parser.uses), type_homes, packages, language, starts: modules.to_vec(), test_cases, test_reporter: None, test_members: members.clone(), test_edges: Default::default(), test_adapter: None, coverage_modules: Default::default(), proof_views:Default::default(), declarations, derived: parser.derived, implements: Vec::new(), private: parser.private, imports: parser.imports, capabilities: parser.capabilities, grantable: parser.grantable, generators: parser.generators, inline_generators: parser.inline_generators, key_roots: Vec::new() })
}

struct Parser {
    /// The `nosuspend:` blocks of the function being parsed, as source ranges.
    quiet: Vec<(usize, usize)>,
    tokens: Vec<Token>,
    cursor: usize,
    depth: usize,
    /// An upper bound on how deep the expression being read is as a tree: what `expression`
    /// has read below the one it is in, and that one's own operators so far (expression.rs).
    expression_depth: usize,
    /// The different text literals met so far, in source order.
    texts: Vec<String>,
    modules: Vec<usize>,
    /// Module constants by name, each already its value.
    constants: std::collections::BTreeMap<String, Expression>,
    /// Types, traits and variants declared without `pub`, with the file of each.
    private: std::collections::BTreeMap<String, u32>,
    /// Every top-level name declared so far, with its file and position, and whether it is a
    /// private function: those may share a name across files.
    declared: std::collections::BTreeMap<String, (u32, std::ops::Range<usize>, bool)>,
    /// Each name written where a type or trait goes, and where: checked against `private`
    /// once every file is read. Type parameters in scope are not recorded.
    uses: Vec<(String, std::ops::Range<usize>)>,
    /// The type parameters of the declaration being read.
    generic_scope: Vec<String>,
    /// What each file imports so far: (importing file, the name it qualifies by, imported file).
    imports: Vec<(u32, String, u32)>,
    /// For each of `imports`, whose the imported file is: 0 the importer's package's, 1 the
    /// standard library's, 2 another package's first file.
    import_kinds: Vec<u8>,
    /// Each variant of a sum, with the file declaring the sum and whether it is `pub`.
    variants: std::collections::BTreeMap<String, (u32, bool)>,
    /// The types declared with `capability X from U`.
    capabilities: Vec<String>,
    /// Inside a `gen fn`, where `yield` is a statement.
    generator: bool,
    /// Declarations a declaration brought with it (a `gen fn`'s record and methods).
    extra: Vec<Declaration>,
    /// Reading a function's parameter's type, where `impl Trait` may be written.
    impl_parameters: bool,
    /// Each plain function's result type when it is a number, a Bool or an F64, found before
    /// parsing (`results`): what `let x = f(...)` shows a closure or a generator. A name
    /// declared twice maps to None.
    results: std::collections::BTreeMap<String, Option<Type>>,
    /// Private type names several files declare: each file's is `Name@file` (private_types.rs).
    shared_private: private_types::Shared,
    /// Renames the code a derive recipe writes as its own file's code is renamed.
    renamer: private_types::Renamer,
    /// Each derive's expansion, for the checker's authority rules (`ast::Derived`).
    derived: Vec<Derived>,
    /// `grantable capability` names (grants.rs).
    grantable: Vec<String>,
    /// The records `gen fn`s became, and where (generator.rs).
    generators: Vec<(String, std::ops::Range<usize>)>,
    /// `gen fn`s as written, that a `for` may run in place (`Program::inline_generators`).
    inline_generators: std::collections::BTreeMap<String, Vec<Function>>,
    /// Closures (closure.rs): whether a function type may be parsed here (a parameter's type),
    /// the current function's parameters and locals that are called as functions, its
    /// bindings with their types (what a closure may capture), and the call traits used.
    fn_types: bool,
    /// Each with the indentation level it was bound at: a dedent past it drops it (`take`).
    fn_locals: Vec<(String, usize)>,
    /// The fields a method's view receiver lends (`self: Self { &mut count }`), with whether
    /// each is mutable, while its body is parsed: `self.count` is that field's parameter.
    pub(super) view: Vec<(String, bool)>,
    scope_types: Vec<(String, Option<Type>, usize)>,
    indent: usize,
    /// Each closure's record, with its parameter types and result type (`returned` checks a
    /// declared `-> fn(...) -> R` against it).
    closure_shapes: std::collections::BTreeMap<String, Vec<Type>>,
    call_arities: std::collections::BTreeSet<usize>,
    /// Each record without type parameters parsed so far, with its fields' types: names bound by
    /// taking one apart (`let Reader(socket, buffer) = move reader`) have those types, so a
    /// closure may capture them.
    record_fields: std::collections::BTreeMap<String, Vec<Type>>,
    /// The derive recipes the bundled files declare, by (file, name) (parser/recipe.rs).
    recipes: std::collections::BTreeMap<(u32, String), recipe::Recipe>,
    /// Interpolations written so far, so two at one span (a recipe's expansion carries its
    /// type's span on every token) get different record names.
    formats: usize,
}

impl Parser {
    /// A parser over tokens the compiler wrote itself (text literals, grants.rs).
    fn written(tokens: Vec<Token>, modules: &[usize]) -> Parser {
        Parser { tokens, cursor: 0, depth: 0, expression_depth: 0, texts: Vec::new(), modules: modules.to_vec(), constants: Default::default(), private: Default::default(), declared: Default::default(), uses: Vec::new(), generic_scope: Vec::new(), imports: Vec::new(), import_kinds: Vec::new(), variants: Default::default(), capabilities: Vec::new(), generator: false, extra: Vec::new(), shared_private: Default::default(), renamer: Default::default(), derived: Vec::new(), grantable: Vec::new(), generators: Vec::new(), inline_generators: Default::default(), fn_types: false, fn_locals: Vec::new(), scope_types: Vec::new(), call_arities: Default::default(), indent: 0, closure_shapes: Default::default(), view: Vec::new(), quiet: Vec::new(), record_fields: Default::default(), impl_parameters: false, results: Default::default(), recipes: Default::default(), formats: 0 }
    }
}

impl Parser {
    fn nested<T>(
        &mut self,
        work: impl FnOnce(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<T, Diagnostic> {
        if self.depth >= 128 {
            return Err(Diagnostic::new(
                "W0102",
                self.token().span.clone(),
                "syntax nesting limit exceeded",
            ));
        }
        self.depth += 1;
        let result = work(self);
        self.depth -= 1;
        result
    }
    fn token(&self) -> &Token {
        &self.tokens[self.cursor]
    }
    fn at(&self, kind: &Kind) -> bool {
        self.token().kind == *kind
    }
    /// Moves past the current token and returns it (not a copy: most callers only move on).
    fn take(&mut self) -> &Token {
        let at = self.cursor;
        // What a block bound is out of a closure's reach once the block ends, whatever kind
        // of block it was (found by an adversarial review: a sibling block's name was captured).
        match self.tokens[at].kind {
            Kind::End => {}
            Kind::Indent => { self.cursor += 1; self.indent += 1 }
            Kind::Dedent => {
                self.cursor += 1;
                self.indent = self.indent.saturating_sub(1);
                let level = self.indent;
                self.scope_types.retain(|(_, _, at)| *at <= level);
                self.fn_locals.retain(|(_, at)| *at <= level);
            }
            _ => self.cursor += 1,
        }
        &self.tokens[at]
    }
    fn error(&self, message: impl Into<std::borrow::Cow<'static, str>>) -> Diagnostic {
        Diagnostic::new("W0100", self.token().span.clone(), message)
    }
    fn eat(&mut self, kind: &Kind) -> bool {
        if self.at(kind) {
            self.take();
            true
        } else {
            false
        }
    }
    fn symbol(&mut self, s: &'static str) -> bool {
        self.eat(&Kind::Symbol(s))
    }
    fn expect(&mut self, kind: Kind) -> Result<(), Diagnostic> {
        if self.eat(&kind) {
            Ok(())
        } else {
            // Say what was wanted: the rule's words, then the token.
            let wanted = match &kind {
                Kind::Symbol(":") => "unexpected token: expected `:` here; a line that opens a block (`if`, `for`, `while`, `match`, `fn`, `type`) ends with `:`".to_string(),
                Kind::Symbol(s) => format!("unexpected token: expected `{s}` here"),
                // What follows a complete expression (the third most common refusal of mutated programs).
                Kind::Newline => {
                    let found = match &self.token().kind {
                        Kind::Name(name) => format!("`{name}`"),
                        Kind::Integer(n) => format!("the number `{n}`"),
                        Kind::Float(_) | Kind::Duration(_) => "a number".to_string(),
                        Kind::Text(_) => "a text literal".to_string(),
                        Kind::Symbol(symbol) => format!("`{symbol}`"),
                        _ => String::new(),
                    };
                    if found.is_empty() { "unexpected token: expected the end of the line here".to_string() }
                    else { format!("unexpected token: expected the end of the line here, and found {found} after a complete expression: two values need an operator between them, and each statement is a line of its own") }
                }
                Kind::Indent => "unexpected token: expected an indented block on the next line: the line above ends with `:`, so what it opens is written under it, four spaces further in (a block with nothing to do holds `()`)".to_string(),
                _ => "unexpected token".to_string(),
            };
            Err(self.error(wanted))
        }
    }
    fn name(&mut self) -> Result<String, Diagnostic> {
        if let Kind::Name(name) = &self.token().kind {
            let name = name.clone();
            self.take();
            Ok(name)
        } else {
            let found = match &self.token().kind {
                Kind::Symbol(s) => format!("`{s}`"),
                Kind::Integer(_) | Kind::Float(_) | Kind::Duration(_) => "a number".to_string(),
                Kind::Text(_) => "a text literal".to_string(),
                Kind::Newline | Kind::End => "the end of the line".to_string(),
                Kind::Indent | Kind::Dedent => "a change of indentation".to_string(),
                Kind::Reserved(word) => format!("the reserved keyword `{word}`"),
                Kind::Brand => "`brand`".to_string(),
                Kind::Name(_) => unreachable!("a name is taken above"),
            };
            Err(self.error(format!("expected a name, found {found}")))
        }
    }
    fn keyword(&mut self, name: &str) -> bool {
        if matches!(&self.token().kind, Kind::Name(n) if n == name) {
            self.take();
            true
        } else {
            false
        }
    }
    fn parameters<T>(
        &mut self,
        end: &'static str,
        mut item: impl FnMut(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<Vec<T>, Diagnostic> {
        let mut items = Vec::new();
        if !self.symbol(end) {
            loop {
                items.push(item(self)?);
                if self.symbol(end) {
                    break;
                }
                self.expect(Kind::Symbol(","))?;
                if self.symbol(end) {
                    break;
                }
            }
        }
        Ok(items)
    }
    fn generics(&mut self) -> Result<Vec<String>, Diagnostic> {
        if self.symbol("<") {
            let names = self.parameters(">", Self::name)?;
            self.generic_scope.extend(names.iter().cloned());
            Ok(names)
        } else {
            Ok(Vec::new())
        }
    }
    /// A name in a place only a type or trait can be: remembered for the privacy check.
    fn type_use(&mut self, name: &str, span: std::ops::Range<usize>) {
        if name != "Self" && !self.generic_scope.iter().any(|g| g == name) { self.uses.push((name.to_string(), span)); }
    }
    /// `<T, U: Any>`: names, and which are declared `Any`.
    /// `<T, U: Any + Eq>`: names, which are `Any`, and the traits each must implement.
    fn bounded_generics(&mut self) -> Result<(Vec<String>, Vec<bool>, Vec<Vec<String>>, Vec<bool>), Diagnostic> {
        if !self.symbol("<") { return Ok(Default::default()); }
        let parsed = self.parameters(">", |parser| {
            let name = parser.name()?;
            parser.generic_scope.push(name.clone());
            let (mut any, mut traits) = (false, Vec::new());
            if parser.symbol(":") {
                loop {
                    let at = parser.token().span.clone();
                    let bound = parser.name()?;
                    parser.type_use(&bound, at);
                    // `Equivalent<K>`: a trait with type arguments, kept as its text.
                    let bound = if parser.symbol("<") { Type::Named(bound, parser.parameters(">", Self::ty)?).render() } else { bound };
                    if bound == "Any" { any = true; } else if !traits.contains(&bound) { traits.push(bound); }
                    if !parser.symbol("+") { break; }
                }
            }
            // `R: Read nosuspend`: no method of `R` called here may pause (docs/design-async.md).
            let quiet = parser.keyword("nosuspend");
            Ok((name, any, traits, quiet))
        })?;
        let mut out: (Vec<String>, Vec<bool>, Vec<Vec<String>>, Vec<bool>) = Default::default();
        for (name, any, traits, quiet) in parsed { out.0.push(name); out.1.push(any); out.2.push(traits); out.3.push(quiet); }
        Ok(out)
    }
    fn typed_name(&mut self) -> Result<(String, Type), Diagnostic> {
        let name = self.name()?;
        self.expect(Kind::Symbol(":"))?;
        // `self: Self { &name, &mut count }`: a receiver that borrows only these fields.
        if name == "self" && matches!(&self.token().kind, Kind::Name(n) if n == "Self") && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol("{"))) {
            self.take();
            self.take();
            let fields = self.parameters("}", |parser| {
                if !parser.symbol("&") { return Err(parser.error("a view receiver lists fields it borrows: `&name` or `&mut name`")); }
                let mutable = parser.keyword("mut");
                let field = parser.name()?;
                Ok(Type::Named(field, if mutable { vec![Type::Unit] } else { Vec::new() }))
            })?;
            if fields.is_empty() { return Err(self.error("a view receiver lists at least one field")); }
            return Ok((name, Type::Named("#View".into(), fields)));
        }
        Ok((name, self.ty()?))
    }
    fn ty(&mut self) -> Result<Type, Diagnostic> {
        self.nested(Self::type_inner)
    }
    fn type_inner(&mut self) -> Result<Type, Diagnostic> {
        if matches!(&self.token().kind, Kind::Name(n) if n == "fn") && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol("("))) {
            if !self.fn_types { return Err(self.error("a function type is a parameter's type in this first version: `f: fn(U64) -> U64`")); }
            self.take();
            return self.function_type();
        }
        if self.symbol("&") {
            let mutable = self.keyword("mut");
            return Ok(Type::Borrow {
                mutable,
                inner: Box::new(self.ty()?),
            });
        }
        // `impl Show`: a parameter of any type with that trait (made a type parameter below).
        if self.keyword("impl") {
            let at = self.token().span.clone();
            if !self.impl_parameters { return Err(Diagnostic::new("W0100", at, "`impl Trait` is the type of a function's parameter: elsewhere write the type, or a type parameter `<T: Trait>`")); }
            let name = self.name()?;
            self.type_use(&name, at);
            return Ok(Type::Named(format!("impl#{name}"), Vec::new()));
        }
        if self.symbol("(") {
            if self.symbol(")") { return Ok(Type::Unit); }
            // `(U64, Bool)`: a tuple's type, the anonymous record `.{0: U64, 1: Bool}`.
            let at = self.token().span.clone();
            let mut types = vec![self.ty()?];
            if !self.at(&Kind::Symbol(",")) { self.expect(Kind::Symbol(")"))?; return Ok(types.pop().expect("one")); }
            while self.symbol(",") { if self.at(&Kind::Symbol(")")) { break; } types.push(self.ty()?); }
            self.expect(Kind::Symbol(")"))?;
            if types.len() > 8 { return Err(Diagnostic::new("W0100", at, "a tuple holds at most 8 values: name them in a record")); }
            return Ok(Type::Record(types.into_iter().enumerate().map(|(index, ty)| (index.to_string(), ty)).collect()));
        }
        if self.symbol("[") {
            let element = Box::new(self.ty()?);
            if self.symbol(";") {
                let length = self.integer()?;
                self.expect(Kind::Symbol("]"))?;
                return Ok(Type::Array { element, length });
            }
            self.expect(Kind::Symbol("]"))?;
            return Ok(Type::Slice(element));
        }
        if self.symbol(".") {
            if self.symbol("[") {
                let mut variants = Vec::new();
                loop {
                    let tag = self.name()?;
                    let payload = self.held()?;
                    variants.push((tag, payload));
                    if !self.symbol("|") { break; }
                }
                self.expect(Kind::Symbol("]"))?;
                return Ok(Type::Union(variants));
            }
            self.expect(Kind::Symbol("{"))?;
            return Ok(Type::Record(self.parameters("}", Self::typed_name)?));
        }
        let mut at = self.token().span.clone();
        let mut name = self.name()?;
        // `shapes.Box`: a type the imported module `shapes` offers.
        if let Some(file) = self.qualifier(&name) && matches!(self.token().kind, Kind::Symbol(".")) {
            self.take();
            let module = std::mem::replace(&mut name, String::new());
            at = self.token().span.clone();
            name = self.name()?;
            self.offered(&module, file, &name, at.clone())?;
        }
        self.type_use(&name, at);
        let recorded = self.uses.len();
        let args = if self.symbol("<") {
            self.parameters(">", Self::ty)?
        } else {
            Vec::new()
        };
        // `Console<Read>`, `Dir<Write>`: a capability's right, whatever a program calls its own types.
        if matches!(name.as_str(), "Console" | "Dir" | "File") { self.uses.truncate(recorded); }
        Ok(Type::Named(name, args))
    }
    fn integer(&mut self) -> Result<u64, Diagnostic> {
        match self.token().kind {
            Kind::Integer(n) => {
                self.take();
                Ok(n)
            }
            _ => Err(self.error("expected an integer")),
        }
    }
    fn declaration(&mut self) -> Result<Declaration, Diagnostic> { self.declaration_in(false,false) }
    fn declaration_in(&mut self, test_entry: bool, original: bool) -> Result<Declaration, Diagnostic> {
        self.generic_scope.clear();
        // `pub`: visible to other modules. Only functions and methods are private without it yet.
        let module = self.module();
        let start = self.token().span.clone();
        let public = self.keyword("pub");
        let mut declaration = self.declaration_after(public)?;
        if (test_entry || original) && let Declaration::Function(f) = &mut declaration && f.name == "main" {
            f.name = if original { "#production_main" } else { "#package_schedule" }.into();
        }
        // One program has one of each top-level name, whichever files declare them.
        let declared = match &declaration {
            Declaration::Function(f) => Some(f.name.clone()),
            Declaration::Record { name, .. } | Declaration::Alias { name, .. } | Declaration::Trait { name, .. } | Declaration::Sum { name, .. } => Some(name.clone()),
            _ => None,
        };
        // A function is its file's: another file may have one of the same name. A call says
        // which with the file's name (`shapes.area`) or, unqualified, means its own file's.
        let private_function = matches!(&declaration, Declaration::Function(f) if f.name != "main" && !f.name.starts_with('#') && !f.name.contains('.'));
        if let Some(name) = declared {
            if let Some((first, earlier, earlier_private)) = self.declared.get(&name).cloned() {
                if !(first != module && private_function && earlier_private) {
                    let message = if first == module { format!("duplicate declaration: `{name}` is declared twice in this file (first at ⟪at {}⟫)", earlier.start) }
                        else { format!("duplicate declaration: `{name}` is also declared by another file of the program; a type or trait offered with `pub` is declared once per program") };
                    return Err(Diagnostic::new("W0401", start, message));
                }
            } else {
                self.declared.insert(name, (module, start, private_function));
            }
        }
        if let Declaration::Function(f) = &mut declaration { f.public = public || f.name.starts_with('#'); }
        if let Declaration::Sum { variants, .. } = &declaration {
            for (variant, _) in variants { self.variants.entry(variant.clone()).or_insert((module, public)); }
        }
        // A private type or trait is a name only its file may use.
        if !public {
            match &declaration {
                // Variant names are not checked: anonymous unions in other files may use the same tags.
                Declaration::Record { name, .. } | Declaration::Alias { name, .. } | Declaration::Trait { name, .. } | Declaration::Sum { name, .. } => { self.private.insert(name.clone(), module); }
                _ => {}
            }
        }
        Ok(declaration)
    }
    fn declaration_after(&mut self, public: bool) -> Result<Declaration, Diagnostic> {
        if public && !matches!(&self.token().kind, Kind::Name(n) if matches!(n.as_str(), "fn" | "pure" | "nopanic" | "gen" | "type" | "sealed" | "trait" | "capability" | "grantable" | "predicate")) {
            return Err(self.error("`pub` goes before `fn`, `type`, `trait` or `capability`"));
        }
        // `grantable capability UiRoot:`: policy data `main` may take, granted by the run.
        let grantable = matches!(&self.token().kind, Kind::Name(n) if n == "grantable")
            && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Name(n)) if n == "capability");
        if grantable {
            self.take();
            if matches!(self.tokens.get(self.cursor + 2).map(|t| &t.kind), Some(Kind::Name(n)) if n == "from") {
                return Err(self.error("a grantable capability carries policy data and no host authority: declare its fields, not `from`"));
            }
        }
        // `capability ConfigDir from Dir<Read>` (book: capabilities-narrowing.md): a sealed
        // record holding the capability, which only this file may build or open.
        if matches!(&self.token().kind, Kind::Name(n) if n == "capability")
            && matches!(self.tokens.get(self.cursor + 2).map(|t| &t.kind), Some(Kind::Name(n)) if n == "from") {
            let module = self.module();
            self.take();
            let span = self.token().span.clone();
            let name = self.name()?;
            self.take();
            let ty = self.ty()?;
            self.expect(Kind::Newline)?;
            self.capabilities.push(name.clone());
            return Ok(Declaration::Record { name, generics: Vec::new(), fields: vec![("authority".into(), ty)], invariants: Vec::new(), conditions: Vec::new(), sealed: true, module, span });
        }
        if self.keyword("lemma") {
            let name = self.name()?;
            self.expect(Kind::Symbol("("))?;
            let parameters = self.parameters(")", Self::typed_name)?;
            self.open_block()?;
            let mut requires = Vec::new();
            while self.keyword("requires") {
                requires.push(self.expression(0)?);
                self.expect(Kind::Newline)?;
            }
            if !self.keyword("ensures") { return Err(self.error("lemma needs an ensures proposition")); }
            let ensures = self.expression(0)?;
            self.expect(Kind::Newline)?;
            if !self.keyword("proof") { return Err(self.error("lemma needs a checked proof block")); }
            let proof = self.proof_steps()?;
            self.expect(Kind::Dedent)?;
            return Ok(Declaration::Lemma(Lemma { name, parameters, requires, ensures, proof }));
        }
        if self.keyword("predicate") {
            let module = self.module();
            let name = self.name()?;
            let generics = self.generics()?;
            self.expect(Kind::Symbol("("))?;
            let parameters = self.parameters(")", Self::typed_name)?;
            self.open_block()?;
            let body = self.spec_clause()?;
            self.expect(Kind::Dedent)?;
            return Ok(Declaration::Predicate { module, public, name, generics, parameters, body });
        }
        // `gen fn`: a generator (docs/design-generators.md).
        if matches!(&self.token().kind, Kind::Name(n) if n == "gen") && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Name(n)) if n == "fn") {
            let at = self.token().span.clone();
            self.take();
            self.take();
            self.generator = true;
            let function = self.function();
            self.generator = false;
            let mut function = function?;
            function.public = public;
            // The generator's type is named after it; a name already taken is the clearest refusal here.
            let mut made = generator::type_name(&function.name);
            let file = self.modules.iter().filter(|first| **first <= at.start).count().saturating_sub(1);
            if self.shared_private.get(&made).is_some_and(|files| files.contains(&file)) { made = format!("{made}@{file}"); }
            if self.declared.contains_key(&made) || self.variants.contains_key(&made) || generator::built_in(&made) {
                return Err(Diagnostic::new("W0401", at, format!("the generator `{}` makes a type named `{made}`, and that name is already declared: rename the generator", function.name)));
            }
            let home = function.module;
            if generator::inlinable(&function) { self.inline_generators.entry(function.name.clone()).or_default().push(function.clone()); }
            let (record, constructor, methods) = generator::desugar(function, at.clone(), made.clone(), &self.results)?;
            self.generators.push((made.clone(), at.clone()));
            self.declared.insert(made, (home, at, false));
            if !public { if let Declaration::Record { name, module, .. } = &record { self.private.insert(name.clone(), *module); } }
            self.extra.push(record);
            self.extra.push(methods);
            return Ok(Declaration::Function(constructor));
        }
        let pure = self.keyword("pure");
        let at = self.token().span.clone();
        let nopanic = self.keyword("nopanic").then_some(at);
        if (pure || nopanic.is_some()) && !matches!(&self.token().kind, Kind::Name(n) if n == "fn") { return Err(self.error("`pure` and `nopanic` go before `fn`, in that order")); }
        if self.keyword("fn") {
            let at = self.token().span.clone();
            let mut function = self.function()?;
            function.pure = pure;
            function.nopanic = nopanic;
            if function.required { return Err(Diagnostic::new("W0100", at, "a function needs a body")); }
            return Ok(Declaration::Function(function));
        }
        if matches!(&self.token().kind, Kind::Name(n) if n == "trait") && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Name(_))) {
            let span = self.token().span.clone();
            self.take();
            let name = self.name()?;
            // In scope as type parameters, so a program's own type of the same name (`type K`) is
            // not taken for a use of it (red-team review: `Equivalent<K>` met a private `K`).
            let generics = self.generics()?;
            self.open_block()?;
            let mut methods = Vec::new();
            while !self.at(&Kind::Dedent) {
                let method_span = self.token().span.clone();
                // `pure fn len(self: &Self) -> U64`: every implementation is pure, so a contract
                // may read it (`requires index < self.len()`).
                let pure = self.keyword("pure");
                if !self.keyword("fn") { return Err(self.error("a trait lists method signatures: `fn name(self: &Self) -> Type`")); }
                let mut method = self.function()?;
                method.pure = pure;
                // A signature, or one with a contract its implementations prove and its callers
                // rely on: clauses and no body.
                if !method.required && method.body.is_empty() && !(method.requires.is_empty() && method.ensures.is_empty()) { method.required = true; }
                if !method.required { return Err(Diagnostic::new("W0100", method_span, "a trait method is a signature without a body")); }
                methods.push((method, method_span));
            }
            self.expect(Kind::Dedent)?;
            return Ok(Declaration::Trait { name, generics, methods, span });
        }
        if self.keyword("impl") {
            let span = self.tokens[self.cursor - 1].span.clone();
            let (generics, any, bounds, quiet) = self.bounded_generics()?;
            if quiet.contains(&true) { return Err(Diagnostic::new("W0100", span.clone(), "`nosuspend` bounds a function's type parameter, not an impl's")); }
            let mut head_at = self.cursor;
            let first = self.ty()?;
            let mut head_end = self.cursor;
            let (implements, trait_arguments, target) = if self.keyword("for") {
                let Type::Named(name, arguments) = first else { return Err(self.error("`impl Trait for Type` names a trait")); };
                head_at = self.cursor;
                let target = self.ty()?;
                head_end = self.cursor;
                (Some(name), arguments, target)
            } else { (None, Vec::new(), first) };
            self.open_block()?;
            self.self_is(head_at..head_end);
            let mut methods = Vec::new();
            while !self.at(&Kind::Dedent) {
                let method_span = self.token().span.clone();
                let public = self.keyword("pub");
                let pure = self.keyword("pure");
                let at = self.token().span.clone();
                let nopanic = self.keyword("nopanic").then_some(at);
                // `gen fn` in an impl: a generator method. Its state type and `next_mut` go beside
                // the program's other declarations; the method itself makes the state.
                if implements.is_none() && matches!(&self.token().kind, Kind::Name(n) if n == "gen") && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Name(n)) if n == "fn") {
                    if pure || nopanic.is_some() { return Err(Diagnostic::new("W0100", method_span, "a generator method is not `pure` or `nopanic`")); }
                    let Type::Named(owner, _) = &target else { return Err(Diagnostic::new("W0100", method_span, "a generator method belongs to a named type")); };
                    let at = self.token().span.clone();
                    self.take();
                    self.take();
                    self.generator = true;
                    let function = self.function();
                    self.generator = false;
                    let mut function = function?;
                    function.public = public;
                    let made = generator::type_name(&format!("{owner}_{}", function.name));
                    if self.declared.contains_key(&made) || self.variants.contains_key(&made) || generator::built_in(&made) {
                        return Err(Diagnostic::new("W0401", at, format!("the generator method `{}` makes a type named `{made}`, and that name is already declared", function.name)));
                    }
                    let home = function.module;
                    // The state type's `next_mut` has its own `self`: the method's is held under a
                    // name no source writes, and is `self` again in the method that makes the state.
                    const HELD: &str = "self#held";
                    let rename = |expr: &mut Expr, from: &str, to: &str| expr.visit_mut(&mut |e| { if let Expression::Name(n) = &mut e.kind && n == from { *n = to.to_string(); } false });
                    for (name, _) in function.parameters.iter_mut() { if name == "self" { *name = HELD.into(); } }
                    for statement in function.body.iter_mut() { statement.visit_exprs_mut(&mut |e| { rename(e, "self", HELD); true }); }
                    // The state type is generic over the impl's type parameters too
                    // (`impl<G: Brand> Nodes<G>` gives `NodesNeighbors<G>`); the method that makes
                    // it keeps its own, beside the impl's.
                    let own = (function.generics.clone(), function.any.clone(), function.bounds.clone());
                    let mut function = function;
                    function.generics = generics.iter().cloned().chain(own.0.iter().cloned()).collect();
                    function.any = any.iter().cloned().chain(own.1.iter().cloned()).collect();
                    function.bounds = bounds.iter().cloned().chain(own.2.iter().cloned()).collect();
                    if own.0.is_empty() && generics.is_empty() && generator::inlinable(&function) { self.inline_generators.entry(format!("{owner}.{}", function.name)).or_default().push(function.clone()); }
                    let (record, mut constructor, state_methods) = generator::desugar(function, at.clone(), made.clone(), &self.results)?;
                    (constructor.generics, constructor.any, constructor.bounds) = own;
                    for (name, _) in constructor.parameters.iter_mut() { if name == HELD { *name = "self".into(); } }
                    for statement in constructor.body.iter_mut() { statement.visit_exprs_mut(&mut |e| { rename(e, HELD, "self"); true }); }
                    self.generators.push((made.clone(), at.clone()));
                    self.declared.insert(made, (home, at, false));
                    if !public { if let Declaration::Record { name, module, .. } = &record { self.private.insert(name.clone(), *module); } }
                    self.extra.push(record);
                    self.extra.push(state_methods);
                    methods.push((constructor, method_span));
                    continue;
                }
                if !self.keyword("fn") {
                    let found = match &self.token().kind { Kind::Name(n) => format!("`{n}`"), Kind::Symbol(t) => format!("`{t}`"), _ => "something else".to_string() };
                    return Err(self.error(format!("expected method declaration: an `impl` holds methods, each starting with `fn`; found {found}")));
                }
                let mut method = self.function()?;
                method.pure = pure;
                method.nopanic = nopanic;
                // A trait's methods are as visible as the trait.
                method.public = public || implements.is_some();
                if method.required { return Err(Diagnostic::new("W0100", method_span, "a method needs a body")); }
                methods.push((method, method_span));
            }
            self.expect(Kind::Dedent)?;
            return Ok(Declaration::Impl { generics, any, bounds, implements, trait_arguments, target, methods, span });
        }
        let module = self.module();
        // `capability Table:` and its fields: a sealed type that holds authority and the
        // policy data beside it (book: "Capabilities that carry policy").
        let capability = matches!(&self.token().kind, Kind::Name(n) if n == "capability");
        let sealed = capability || (matches!(&self.token().kind, Kind::Name(n) if n == "sealed")
            && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Name(n)) if n == "type"));
        if sealed { self.take(); }
        if capability || self.keyword("type") {
            let span = self.token().span.clone();
            let name = self.name()?;
            if capability && grantable { self.grantable.push(name.clone()); } else if capability { self.capabilities.push(name.clone()); }
            let generics = self.generics()?;
            // `type Score derive(Eq, Hash, Before):` (book: appendix-operators.md).
            // A recipe from a library (`keys.Key`, `keys.Key(by: id)`, parser/recipe.rs): the
            // recipe, its selection of fields (empty: all), and where it was named.
            let mut recipes: Vec<(recipe::Recipe, Vec<String>, Option<String>, std::ops::Range<usize>)> = Vec::new();
            let derived = if self.keyword("derive") {
                self.expect(Kind::Symbol("("))?;
                let mut names = Vec::new();
                loop {
                    let at = self.token().span.clone();
                    let first = self.name()?;
                    let found = if matches!(self.token().kind, Kind::Symbol(".")) {
                        let Some(file) = self.qualifier(&first) else {
                            return Err(Diagnostic::new("W0401", at, format!("`{first}` is not a module this file imports")));
                        };
                        self.take();
                        let inner = self.name()?;
                        let Some(recipe) = self.recipes.get(&(file, inner.clone())).cloned() else {
                            return Err(Diagnostic::new("W0401", at, format!("the module `{first}` declares no derive recipe `{inner}`")));
                        };
                        if !recipe.public {
                            return Err(Diagnostic::new("W0401", at, format!("the derive recipe `{inner}` is private to the module `{first}`; that module marks what it offers with `pub`")));
                        }
                        Some(recipe)
                    } else if matches!(first.as_str(), "Eq" | "Hash" | "Before" | "Discard") { None } else { self.recipes.get(&(self.module(), first.clone())).cloned() };
                    match found {
                        Some(recipe) => {
                            let mut selection = Vec::new();
                            // `for: Type` (the recipe's `For`, as `Query(for: NameKey)`), then
                            // `by: fields`, each optional.
                            let mut for_type = None;
                            if self.symbol("(") {
                                if self.keyword("for") {
                                    if !self.symbol(":") { return Err(self.error("a derive recipe takes the type it is for as `for: Type`")); }
                                    for_type = Some(self.ty()?.render());
                                    if !self.symbol(",") { self.expect(Kind::Symbol(")"))?; recipes.push((recipe, selection, for_type, at)); if !self.symbol(",") { break; } continue; }
                                }
                                if !self.keyword("by") || !self.symbol(":") { return Err(self.error("a derive recipe takes the fields it is by: `Key(by: id)`, after any `for: Type`")); }
                                loop {
                                    selection.push(self.name()?);
                                    if !self.symbol(",") { break; }
                                }
                                self.expect(Kind::Symbol(")"))?;
                            }
                            recipes.push((recipe, selection, for_type, at));
                        }
                        None if matches!(first.as_str(), "Eq" | "Hash" | "Before" | "Discard") => names.push(first),
                        None => return Err(Diagnostic::new("W0100", at, format!("`derive` makes `Eq`, `Hash`, `Before` and `Discard`, or a recipe a module declares (`keys.Key`), not `{first}`"))),
                    }
                    if !self.symbol(",") { break; }
                }
                self.expect(Kind::Symbol(")"))?;
                names
            } else { Vec::new() };
            if !sealed && self.symbol("=") {
                let ty = self.ty()?;
                self.expect(Kind::Newline)?;
                return Ok(Declaration::Alias { name, generics, ty });
            }
            self.open_block()?;
            let mut fields = Vec::new();
            let mut invariants = Vec::new();
            let mut conditions = Vec::new();
            let mut variants = Vec::new();
            while !self.at(&Kind::Dedent) {
                // A member with no `:` after its name is a variant, not a field.
                let variant = matches!(&self.token().kind, Kind::Name(n) if n != "invariant")
                    && !matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol(":")));
                if variant {
                    let name = self.name()?;
                    let payload = self.held()?;
                    variants.push((name, payload));
                } else if self.keyword("invariant") {
                    let condition = self.spec_clause()?;
                    // The older `invariant predicate(field, ...)` form is still recorded for the previous lowering.
                    if let Expression::Call(callee, arguments) = &condition.kind {
                        let fields = arguments.iter().filter_map(|a| if let Expression::Name(n) = &a.kind { Some(n.clone()) } else { None }).collect::<Vec<_>>();
                        if let (Expression::Name(predicate), true) = (&callee.kind, fields.len() == arguments.len()) {
                            invariants.push(Invariant { predicate: predicate.clone(), fields });
                        }
                    }
                    conditions.push(condition);
                    continue;
                } else {
                    fields.push(self.typed_name()?);
                }
                self.expect(Kind::Newline)?;
            }
            self.expect(Kind::Dedent)?;
            if !variants.is_empty() {
                if sealed || !fields.is_empty() || !conditions.is_empty() {
                    return Err(self.error("a type lists either fields or variants; variants cannot be sealed or carry an invariant yet"));
                }
                if let Some((_, _, _, at)) = recipes.iter().find(|(_, selection, _, _)| !selection.is_empty()) {
                    return Err(Diagnostic::new("W0100", at.clone(), "a sum type has variants, not fields: a recipe over it takes no `by:`"));
                }
                if !derived.is_empty() {
                    // The impls name the variants; a constant of the same name would stand in for one
                    // (review: the generated source then failed to lex, reported inside the library).
                    if let Some((variant, _)) = variants.iter().find(|(v, _)| self.constants.contains_key(v)) {
                        let variant = variant.split('@').next().unwrap_or(variant);
                        return Err(Diagnostic::new("W0100", span.clone(), format!("the variant `{variant}` has a constant's name, so it cannot be named in the derived impls: rename one")));
                    }
                    let source = derive::sum_source(&name, &generics, &variants, &derived);
                    let mut tokens = lex_written(&source)?;
                    respan(&mut tokens, |_| span.clone());
                    let mut written = Parser::written(tokens, &self.modules);
                    loop {
                        while matches!(written.token().kind, Kind::Newline) { written.take(); }
                        if matches!(written.token().kind, Kind::End) { break; }
                        let declaration = written.declaration()?;
                        self.extra.push(declaration);
                    }
                    self.extra.append(&mut written.extra);
                }
                // Recipes over the variants (2026-11-19, the user's call): each variant with how many
                // values it holds, which the recipe binds only in that variant's arm.
                let shape: Vec<(String, usize)> = variants.iter().map(|(v, ty)| (v.clone(), match ty {
                    None => 0,
                    Some(Type::Record(fields)) => fields.len(),
                    Some(_) => 1,
                })).collect();
                for (recipe, _, for_type, at) in &recipes {
                    if let Some(parameter) = generics.iter().find(|g| recipe::writes_word(&recipe.body, g)) {
                        return Err(Diagnostic::new("W0100", at.clone(), format!("`{name}`'s type parameter `{parameter}` is a name the derive recipe writes: rename the parameter")));
                    }
                    self.apply_recipe(recipe, &name, &generics, for_type.as_deref(), at, &span, recipe::Shape::Variants(&shape))?;
                }
                return Ok(Declaration::Sum { name, generics, variants, span: span.clone(), module });
            }
            if generics.is_empty() { self.record_fields.insert(name.clone(), fields.iter().map(|(_, t)| t.clone()).collect()); }
            for (recipe, selection, for_type, at) in &recipes {
                // The fields it is by, each one of the type's and named once; all of them otherwise.
                let declared: std::collections::BTreeMap<_, _> = fields.iter().map(|(f, t)| (f.as_str(), t)).collect();
                let mut chosen = Vec::new();
                if selection.is_empty() {
                    chosen.extend(fields.iter().map(|(f, t)| (f.as_str(), t)));
                } else {
                    for (index, field) in selection.iter().enumerate() {
                        let Some(&ty) = declared.get(field.as_str()) else {
                            return Err(Diagnostic::new("W0100", at.clone(), format!("`{name}` has no field `{field}` to derive by")));
                        };
                        if selection[..index].contains(field) {
                            return Err(Diagnostic::new("W0100", at.clone(), format!("the field `{field}` is named twice")));
                        }
                        chosen.push((field.as_str(), ty));
                    }
                }
                // A type parameter named like a word the recipe writes would take that word's
                // place in the recipe's code (seventh review: `Pair<Unit>` turned the recipe's
                // private `Unit` into the parameter, and another type's impl ran).
                if let Some(parameter) = generics.iter().find(|g| recipe::writes_word(&recipe.body, g)) {
                    return Err(Diagnostic::new("W0100", at.clone(), format!("`{name}`'s type parameter `{parameter}` is a name the derive recipe writes: rename the parameter")));
                }
                self.apply_recipe(recipe, &name, &generics, for_type.as_deref(), at, &span, recipe::Shape::Fields(&chosen))?;
            }
            if !derived.is_empty() {
                let source = derive::source(&name, &generics, &fields, &derived);
                let mut tokens = lex_written(&source)?;
                respan(&mut tokens, |_| span.clone());
                let mut written = Parser::written(tokens, &self.modules);
                loop {
                    while matches!(written.token().kind, Kind::Newline) { written.take(); }
                    if matches!(written.token().kind, Kind::End) { break; }
                    let declaration = written.declaration()?;
                    self.extra.push(declaration);
                }
                self.extra.append(&mut written.extra);
            }
            return Ok(Declaration::Record {
                name,
                generics,
                fields,
                invariants,
                conditions,
                sealed,
                module,
                span,
            });
        }
        // Say what was found, and for a word other languages declare with, what to write instead.
        let found = match &self.token().kind {
            Kind::Name(n) => format!("`{n}`"),
            Kind::Symbol(s) => format!("`{s}`"),
            Kind::Integer(_) | Kind::Float(_) | Kind::Duration(_) => "a number".to_string(),
            Kind::Text(_) => "a text literal".to_string(),
            // A line indented under nothing: the block it belongs to ended above it.
            Kind::Indent => "an indented line: the line above it does not open a block (it does not end with `:`), or the block it belongs to ended earlier".to_string(),
            _ => "something else".to_string(),
        };
        let hint = match &self.token().kind {
            // A module constant is a top-level `let NAME = value` (parser/constant.rs).
            Kind::Name(n) if matches!(n.as_str(), "const" | "static") => ": a module constant is written `let NAME = value`, with a value made of numbers, Bools, operators and earlier constants",
            Kind::Name(n) if n == "var" => ": a module has no variables; a constant is written `let NAME = value`",
            Kind::Name(n) if matches!(n.as_str(), "struct" | "enum" | "class" | "record") => ": records and sums are both declared with `type`",
            Kind::Name(n) if matches!(n.as_str(), "def" | "func" | "function" | "fun") => ": functions are declared with `fn`",
            // A statement at the top level (the most common way here: a line that lost its indent).
            Kind::Name(n) if matches!(n.as_str(), "return" | "if" | "for" | "while" | "match" | "break" | "continue") || self.statement_follows() =>
                ": this line reads as a statement, and a statement belongs inside a function: indent it under the one it is part of",
            _ => "",
        };
        Err(self.error(format!("expected a declaration (`fn`, `type`, `impl`, `trait`, `lemma` or `capability`), found {found}{hint}")))
    }
    /// Whether the name at the cursor is followed by what starts a statement's rest: a call, a
    /// field or method, or an assignment (`console.line(...)`, `total += 1`, `main()`).
    fn statement_follows(&self) -> bool {
        matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind),
            Some(Kind::Symbol("." | "(" | "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "[")))
    }
    /// The file the current token was written in.
    /// `shapes.Square` where `name` is `shapes`, an imported module: the variant or type it
    /// offers, checked. Any other name is itself.
    fn unqualified(&mut self, name: String) -> Result<String, Diagnostic> {
        let Some(file) = self.qualifier(&name) else { return Ok(name) };
        if !matches!(self.token().kind, Kind::Symbol(".")) { return Ok(name); }
        self.take();
        let at = self.token().span.clone();
        let inner = self.name()?;
        self.offered(&name, file, &inner, at)?;
        Ok(inner)
    }
    /// The file this one imports under `name`, if it does.
    fn qualifier(&self, name: &str) -> Option<u32> {
        let here = self.module();
        self.imports.iter().find(|(from, imported, _)| *from == here && imported == name).map(|(_, _, file)| *file)
    }
    /// Whether the module `module` (file `file`) offers the type or variant `name`.
    fn offered(&self, module: &str, file: u32, name: &str, at: std::ops::Range<usize>) -> Result<(), Diagnostic> {
        let declared = self.declared.get(name).filter(|(home, _, function)| *home == file && !function).map(|_| !self.private.contains_key(name))
            .or_else(|| self.variants.get(name).filter(|(home, _)| *home == file).map(|(_, public)| *public));
        match declared {
            Some(true) => Ok(()),
            Some(false) => Err(Diagnostic::new("W0401", at, format!("`{name}` is private to the module `{module}`; that module marks what it offers with `pub`"))),
            None => Err(Diagnostic::new("W0401", at, format!("the module `{module}` offers no type or constructor `{name}`"))),
        }
    }
    /// The impls a derive recipe writes for the type `name`, over its fields or its variants
    /// (`recipe::Shape`), read with definition-site resolution and pushed beside the program's.
    fn apply_recipe(&mut self, recipe: &recipe::Recipe, name: &str, generics: &[String], for_type: Option<&str>, at: &std::ops::Range<usize>, span: &std::ops::Range<usize>, shape: recipe::Shape) -> Result<(), Diagnostic> {
        let recipe::Expanded { text: source, replaced, lines } = recipe::expand(recipe, name, generics, for_type, shape)
            .map_err(|why| Diagnostic::new("W0100", recipe.at..recipe.at, why))?;
        // The body line each written offset came from (review: lexer spans were offsets into
        // the written text, so they named another file).
        let line_of = |at: usize| lines[lines.partition_point(|(start, _)| *start <= at).saturating_sub(1)].1.clone();
        let mut tokens = lex_written(&source).map_err(|error| {
            // The lexer's offsets are into the escaped text: `@` and `#` grew.
            let (mut left, mut plain) = (error.span.start, source.len().saturating_sub(1));
            for (at, byte) in source.bytes().enumerate() {
                let grown = match byte { b'@' => "__witchy_at__".len(), b'#' => "__witchy_hash__".len(), _ => 1 };
                if left < grown { plain = at; break; }
                left -= grown;
            }
            Diagnostic::new("W0100", line_of(plain), format!("the recipe writes code that does not read: {}", error.message))
        })?;
        // Definition-site resolution (2026-11-19, the user's call): a token the recipe wrote
        // is read in the recipe's file, so its helpers, imports and types are the ones its
        // author meant wherever the recipe is applied, and the methods it writes belong to
        // that file; the words the derive put in (the type, `For`, the fields) keep the type's
        // span and are read in the type's file. The lexer read the text with `@` and `#`
        // escaped (longer), so the replaced ranges are moved to its offsets first.
        let mut escaped = Vec::with_capacity(source.len() + 1);
        let mut grown = 0;
        for (at, byte) in source.bytes().enumerate() {
            escaped.push(at + grown);
            grown += match byte { b'@' => "__witchy_at__".len() - 1, b'#' => "__witchy_hash__".len() - 1, _ => 0 };
        }
        escaped.push(source.len() + grown);
        let derive_words: Vec<std::ops::Range<usize>> = replaced.iter().map(|r| escaped[r.start]..escaped[r.end]).collect();
        // Every other token carries the span of the body line it was written from, so an error in
        // it is reported there (it was reported at the recipe's header).
        let plain_lines: Vec<(usize, std::ops::Range<usize>)> = lines.iter().map(|(start, from)| (escaped[*start], from.clone())).collect();
        let written_from = |at: usize| plain_lines[plain_lines.partition_point(|(start, _)| *start <= at).saturating_sub(1)].1.clone();
        respan(&mut tokens, |token| if derive_words.iter().any(|r| r.contains(&token.span.start)) { span.clone() } else { written_from(token.span.start) });
        // Names private to a file and shared with another are spelled `Name@file` by then,
        // as in the recipe's own file's code (parser/private_types.rs).
        self.renamer.rename(&mut tokens)?;
        let file_index = |at: usize| self.modules.iter().filter(|first| **first <= at).count().saturating_sub(1) as u32;
        // The type `For` stands for, read as a type in the deriving file (its words renamed as
        // that file's are); the checker resolves it, through aliases and references, to decide
        // what the recipe may not reach (review round 4: an alias and a `&mut` slipped past a
        // check of the written name).
        let mut for_parsed = None;
        if let Some(given) = for_type {
            let refuse = |why: String| Diagnostic::new("W0100", at.clone(), format!("`for: {given}` names a type: {why}"));
            let mut words = lex_written(given).map_err(|error| refuse(error.message.to_string()))?;
            for token in &mut words { token.span = span.clone(); }
            self.renamer.rename(&mut words)?;
            let end = words.last().map(|t| t.span.clone()).unwrap_or_else(|| span.clone());
            words.push(Token { kind: Kind::End, span: end });
            let mut reading = Parser::written(words, &self.modules);
            let parsed = reading.ty().map_err(|error| refuse(error.message.to_string()))?;
            while matches!(reading.token().kind, Kind::Newline) { reading.take(); }
            if !matches!(reading.token().kind, Kind::End) { return Err(refuse("more follows the type".to_string())); }
            for_parsed = Some(parsed);
        }
        self.derived.push(Derived { recipe: recipe.at..recipe.end, recipe_module: file_index(recipe.at), site: span.clone(), module: file_index(span.start), target: name.to_string(), for_type: for_parsed });
        let mut written = Parser::written(tokens, &self.modules);
        // The program's own literal table, so a recipe's text literal gets the program's
        // index for its text (review of slice 1: with a table of its own, thrown away
        // after, a literal compiled to another literal's text).
        written.texts = std::mem::take(&mut self.texts);
        written.formats = self.formats;
        loop {
            while matches!(written.token().kind, Kind::Newline) { written.take(); }
            if matches!(written.token().kind, Kind::End) { break; }
            // An error in what the recipe wrote names the recipe (it was reported at the type with no
            // word of where the code came from).
            let declaration = written.declaration().map_err(|error| Diagnostic::new(error.code, error.span.clone(), format!("{} (in the code the derive recipe writes for `{name}`)", error.message)))?;
            // Impls only (review: a recipe's `fn` or `type` landed in the type's module,
            // and a second type using it collided).
            if !matches!(declaration, Declaration::Impl { .. }) {
                self.texts = std::mem::take(&mut written.texts);
                return Err(Diagnostic::new("W0100", at.clone(), "a derive recipe writes impls (`impl Trait for Self:`), nothing else"));
            }
            self.extra.push(declaration);
        }
        self.texts = std::mem::take(&mut written.texts);
        self.formats = written.formats;
        self.extra.append(&mut written.extra);
        // The types it names are checked for privacy as the type's own code is (review:
        // a recipe reached another module's private type).
        self.uses.append(&mut written.uses);
        Ok(())
    }
    /// `Self` in an impl block is the implementing type as the header writes it, with its type
    /// arguments (`Pair<T>`); before `(` or `.` (a constructor, a static call) it is the type's
    /// name alone, as a constructor or call names a generic type (2026-11-19, the user's call).
    /// The block's tokens from the cursor are rewritten; a view receiver's `self: Self { ... }`
    /// keeps its word.
    fn self_is(&mut self, head: std::ops::Range<usize>) {
        let written: Vec<Token> = self.tokens[head].to_vec();
        let bare = written.iter().position(|t| matches!(t.kind, Kind::Symbol("<"))).map_or(written.len(), |i| i);
        let mut depth = 0usize;
        let mut at = self.cursor;
        while at < self.tokens.len() {
            match &self.tokens[at].kind {
                Kind::Indent => depth += 1,
                Kind::Dedent => { if depth == 0 { break; } depth -= 1; }
                Kind::End => break,
                Kind::Name(word) if word == "Self" => {
                    let view = at >= 2 && matches!(&self.tokens[at - 2].kind, Kind::Name(w) if w == "self") && matches!(self.tokens[at - 1].kind, Kind::Symbol(":"))
                        && matches!(self.tokens.get(at + 1).map(|t| &t.kind), Some(Kind::Symbol("{")));
                    if !view {
                        let used = if matches!(self.tokens.get(at + 1).map(|t| &t.kind), Some(Kind::Symbol("(" | "."))) { &written[..bare] } else { &written[..] };
                        let span = self.tokens[at].span.clone();
                        let replacement: Vec<Token> = used.iter().map(|t| Token { span: span.clone(), ..t.clone() }).collect();
                        let count = replacement.len();
                        self.tokens.splice(at..at + 1, replacement);
                        at += count;
                        continue;
                    }
                }
                _ => {}
            }
            at += 1;
        }
    }
    fn module(&self) -> u32 {
        let at = self.token().span.start;
        self.modules.iter().filter(|start| **start <= at).count().saturating_sub(1) as u32
    }
    fn function(&mut self) -> Result<Function, Diagnostic> {
        // A method's own type parameters are not in scope in the next method.
        let outer = self.generic_scope.len();
        let function = self.function_inner();
        self.generic_scope.truncate(outer);
        function
    }
    fn function_inner(&mut self) -> Result<Function, Diagnostic> {
        let start = self.token().span.clone();
        let module = self.module();
        let name = self.name()?;
        let (mut generics, mut any, mut bounds, mut nosuspend) = self.bounded_generics()?;
        let outer_quiet = std::mem::take(&mut self.quiet);
        self.expect(Kind::Symbol("("))?;
        self.fn_types = true;
        // `by: U64 = 10`: a default, a literal, for parameters after all those without one.
        let parameters = self.parameters(")", |parser| {
            parser.impl_parameters = true;
            let named = parser.typed_name();
            parser.impl_parameters = false;
            let named = named?;
            let default = if parser.symbol("=") {
                let value = parser.expression(0)?;
                let literal = |e: &Expr| matches!(e.kind, Expression::Integer(_) | Expression::Float(_) | Expression::Bool(_));
                let site = matches!(&value.kind, Expression::SourceSite(label) if label.is_empty());
                if !(site || literal(&value) || matches!(&value.kind, Expression::Unary("-", inner) if literal(inner))) {
                    return Err(Diagnostic::new("W0100", value.span, "a parameter's default is a number or a Bool written out"));
                }
                Some(value)
            } else { None };
            Ok((named, default))
        });
        self.fn_types = false;
        let (mut parameters, mut defaults): (Vec<(String, Type)>, Vec<Option<Expr>>) = parameters?.into_iter().unzip();
        // `x: impl Show` is `<Impl#n: Show>` and `x: Impl#n`, a type parameter of its own each.
        for (index, (_, ty)) in parameters.iter_mut().enumerate() {
            if let Type::Named(name, _) = ty && let Some(bound) = name.strip_prefix("impl#") {
                let fresh = format!("Impl#{index}");
                generics.push(fresh.clone()); any.push(false); bounds.push(vec![bound.to_string()]); nosuspend.push(false);
                self.generic_scope.push(fresh.clone());
                *ty = Type::Named(fresh, Vec::new());
            }
        }
        if let Some(first) = defaults.iter().position(Option::is_some) && defaults[first..].iter().any(Option::is_none) {
            return Err(Diagnostic::new("W0100", start.clone(), "parameters with a default come after all those without one"));
        }
        // A view receiver is one reference parameter per field it lists (`self#count:
        // &mut #FieldOf<count>`, typed when the impl is lifted: methods.rs).
        self.view = Vec::new();
        if let Some((_, Type::Named(view, fields))) = parameters.first() && view == "#View" {
            let fields = fields.clone();
            let mut expanded = Vec::new();
            for field in &fields {
                let Type::Named(name, mark) = field else { unreachable!("a view lists names") };
                if self.view.iter().any(|(n, _)| n == name) { return Err(Diagnostic::new("W0100", start.clone(), "a view receiver lists each field once")); }
                self.view.push((name.clone(), !mark.is_empty()));
                expanded.push((format!("self#{name}"), Type::Borrow { mutable: !mark.is_empty(), inner: Box::new(Type::Named("#FieldOf".into(), vec![Type::Named(name.clone(), Vec::new())])) }));
            }
            let added = expanded.len();
            parameters.splice(0..1, expanded);
            defaults.splice(0..1, std::iter::repeat_n(None, added));
        }
        if parameters.iter().skip(1).any(|(_, t)| matches!(t, Type::Named(v, _) if v == "#View")) { return Err(Diagnostic::new("W0100", start.clone(), "only a method's first parameter, `self`, may be a view")); }
        // A function's parameters are bound one level inside it: its body's own level.
        let level = self.indent + 1;
        self.fn_locals = self.function_parameters(&mut generics, &mut any, &mut bounds, &mut parameters)?.into_iter().map(|n| (n, level)).collect();
        self.scope_types = parameters.iter().map(|(n, t)| (n.clone(), Some(t.clone()), level)).collect();
        // A function may return a closure: `-> fn(U64) -> U64` (closure.rs `returned`).
        self.fn_types = true;
        let result = if self.symbol("->") { self.ty() } else { Ok(Type::Unit) };
        self.fn_types = false;
        let result = result?;
        // `-> T suspends`: this function may pause its task (docs/design-async.md).
        let suspends = self.keyword("suspends");
        // Parameters made generic by the signature (closures) have no `nosuspend` of their own.
        nosuspend.resize(generics.len(), false);
        if self.eat(&Kind::Newline) {
            self.quiet = outer_quiet;
            return Ok(Function { name, generics, any, bounds, required: true, module, public: true, pure: false, nopanic: None, span: start.clone(), suspends, nosuspend, quiet: Vec::new(), parameters, defaults, result, body: Vec::new(), requires: Vec::new(), ensures: Vec::new(), borrows: Vec::new() });
        }
        let (requires, ensures, borrows, body) = self.nested(|parser| {
            parser.open_block()?;
            let (mut requires, mut ensures, mut borrows) = (Vec::new(), Vec::new(), Vec::new());
            loop {
                if parser.keyword("borrows") {
                    let span = parser.token().span.clone();
                    let variant = parser.name()?;
                    if !parser.keyword("from") { return Err(parser.error("`borrows Variant from parameter`")); }
                    let mut from = vec![parser.name()?];
                    while parser.eat(&Kind::Symbol(",")) { from.push(parser.name()?); }
                    parser.expect(Kind::Newline)?;
                    borrows.push(a_borrows(variant, from, span));
                } else if parser.keyword("requires") {
                    requires.push(parser.spec_clause()?);
                } else if parser.keyword("ensures") {
                    ensures.push(parser.postcondition()?);
                } else { break }
            }
            if !requires.is_empty() && ensures.is_empty() {
                ensures.push(Postcondition::truth(start.clone()));
            }
            if parser.keyword("by") {
                parser.justification()?;
                return Err(Diagnostic::new("W0315", parser.token().span.clone(),
                    "external Lean body terms are retired; use source contracts and proof assertions"));
            }
            let mut body = Vec::new();
            while !parser.at(&Kind::Dedent) { body.push(parser.statement()?); }
            parser.expect(Kind::Dedent)?;
            Ok((requires, ensures, borrows, body))
        })?;
        self.view = Vec::new();
        let quiet = std::mem::replace(&mut self.quiet, outer_quiet);
        let result = closure::returned(result, &body, &start, &self.closure_shapes)?;
        Ok(Function { name, generics, any, bounds, required: false, module, public: false, pure: false, nopanic: None, span: start.clone(), suspends, nosuspend, quiet, parameters, defaults, result, body, requires, ensures, borrows })
    }
    fn open_block(&mut self) -> Result<(), Diagnostic> {
        self.expect(Kind::Symbol(":"))?;
        self.expect(Kind::Newline)?;
        self.expect(Kind::Indent)
    }
    fn block(&mut self) -> Result<Vec<Statement>, Diagnostic> {
        self.nested(Self::block_inner)
    }
    fn block_inner(&mut self) -> Result<Vec<Statement>, Diagnostic> {
        self.open_block()?;
        let mut statements = Vec::new();
        while !self.at(&Kind::Dedent) {
            statements.push(self.statement()?);
        }
        self.expect(Kind::Dedent)?;
        Ok(statements)
    }
    fn proof_steps(&mut self) -> Result<Vec<ProofStep>, Diagnostic> {
        self.open_block()?;
        let mut steps = Vec::new();
        while !self.at(&Kind::Dedent) {
            let step = if self.keyword("auto") { ProofStep::Auto }
                else if self.keyword("have") {
                    if matches!(self.token().kind, Kind::Name(_))
                        && self.tokens.get(self.cursor + 1).is_some_and(|token|token.kind == Kind::Symbol(":")) {
                        let name = self.name()?; self.expect(Kind::Symbol(":"))?;
                        ProofStep::NamedHave { name, claim: self.expression(0)? }
                    } else { ProofStep::Have(self.expression(0)?) }
                }
                else if self.keyword("apply") {
                    let application = self.expression(0)?;
                    let facts = if self.keyword("using") {
                        let mut facts = vec![self.name()?];
                        while self.symbol(",") { facts.push(self.name()?); }
                        Some(facts)
                    } else { None };
                    let binding = if self.keyword("as") { Some(self.name()?) } else { None };
                    if facts.is_some() || binding.is_some() { ProofStep::ApplyUsing { application, facts, binding } }
                    else { ProofStep::Apply(application) }
                }
                else if self.keyword("exact") {
                    let span = self.token().span.clone();
                    ProofStep::Exact { name: self.name()?, span }
                }
                else { return Err(self.error("expected auto, have, apply or exact in checked proof")); };
            self.expect(Kind::Newline)?;
            steps.push(step);
        }
        self.expect(Kind::Dedent)?;
        if steps.is_empty() { return Err(self.error("proof block cannot be empty")); }
        Ok(steps)
    }
    /// What a variant holds: nothing, one type, or `(A, B, ...)`, which is the record `.{0: A, 1: B, ...}`.
    /// No source can write those labels, so such a value is made and opened only by position.
    fn held(&mut self) -> Result<Option<Type>, Diagnostic> {
        if !self.symbol("(") { return Ok(None); }
        let mut types = vec![self.ty()?];
        while self.symbol(",") { types.push(self.ty()?); }
        self.expect(Kind::Symbol(")"))?;
        Ok(Some(if types.len() == 1 { types.remove(0) } else {
            Type::Record(types.into_iter().enumerate().map(|(index, ty)| (index.to_string(), ty)).collect())
        }))
    }
    /// `Variant`, `Variant(name)` or `Variant(Pattern)`. Inside the parentheses a variant is told from a
    /// name by its dot, its own parentheses or its capital letter.
    /// `(a, b)`, an optional `: Type`, `=` and the value: a tuple opened by position.
    fn tuple_binding(&mut self, mutable: bool) -> Result<Statement, Diagnostic> {
        self.expect(Kind::Symbol("("))?;
        let names = self.parameters(")", Self::name)?;
        if names.len() < 2 { return Err(self.error("`let (a, b) =` opens a tuple of two values or more")); }
        let fields = names.into_iter().enumerate().map(|(index, name)| (index.to_string(), name)).collect();
        let ty = if self.symbol(":") { Some(self.ty()?) } else { None };
        self.expect(Kind::Symbol("="))?;
        let value = self.expression(0)?;
        self.expect(Kind::Newline)?;
        Ok(Statement::OpenFields { fields, value, mutable, ty })
    }
    fn pattern(&mut self) -> Result<Pattern, Diagnostic> {
        // `.Tag` names a variant of an anonymous sum.
        let variant = if self.symbol(".") { format!(".{}", self.name()?) } else { let name = self.name()?; self.unqualified(name)? };
        let (mut binding, mut nested, mut open) = (None, None, Vec::new());
        if self.symbol("(") {
            let inner = matches!(&self.token().kind, Kind::Symbol("."))
                || matches!(&self.token().kind, Kind::Name(name) if name.starts_with(|c: char| c.is_ascii_uppercase()))
                || matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol("(")));
            if inner { nested = Some(Box::new(self.nested(|p| p.pattern())?)); } else {
                let first = self.name()?;
                // `Variant(a, b)`: one name for each value the variant holds.
                if self.at(&Kind::Symbol(",")) {
                    open.push(first);
                    while self.symbol(",") { open.push(self.name()?); }
                } else { binding = Some(first); }
            }
            self.expect(Kind::Symbol(")"))?;
        }
        Ok(Pattern { variant, binding, nested, open })
    }
    /// `match value:` whose arms are numbers is the chain of `if value == n:` it reads as,
    /// ending in the `_` arm. The value is written once per test, so it must be a name or
    /// fields of one: reading it runs nothing.
    fn number_arms(&mut self, value: Expr) -> Result<Statement, Diagnostic> {
        fn named(expr: &Expr) -> bool {
            match &expr.kind { Expression::Name(_) => true, Expression::Field(base, _) | Expression::Unary("*", base) => named(base), _ => false }
        }
        if !named(&value) { return Err(Diagnostic::new("W0100", value.span.clone(), "a match on numbers reads a named value: bind it with `let` first")); }
        let mut arms = Vec::new();
        let mut rest = None;
        while !self.at(&Kind::Dedent) {
            if matches!(&self.token().kind, Kind::Name(n) if n == "_") {
                self.take();
                rest = Some(self.block()?);
                break;
            }
            // `k:` last: the `_` arm, with the value bound to `k`.
            if let Kind::Name(name) = self.token().kind.clone() && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol(":"))) {
                let span = self.token().span.clone();
                self.take();
                let mut body = vec![Statement::Bind { mutable: false, name, ty: None, value: Expr { kind: value.kind.clone(), span }, proof: None }];
                body.extend(self.block()?);
                rest = Some(body);
                break;
            }
            let mut condition: Option<Expr> = None;
            loop {
                let token = self.token().clone();
                let Kind::Integer(number) = token.kind else { return Err(Diagnostic::new("W0100", token.span, "expected a number or `_`")); };
                self.take();
                let literal = |number: u64, span: &std::ops::Range<usize>| Box::new(Expr { kind: Expression::Integer(number), span: span.clone() });
                let compare = |operator: &'static str, number: u64| Expr { kind: Expression::Binary(operator, Box::new(value.clone()), literal(number, &token.span)), span: token.span.clone() };
                // `lo..hi` stops before `hi`, as a counting `for` does; `lo..=hi` includes it.
                let test = if self.symbol("..") {
                    let inclusive = self.symbol("=");
                    let end = self.token().clone();
                    let Kind::Integer(last) = end.kind else { return Err(Diagnostic::new("W0100", end.span, "a range in a match on numbers ends with a number")); };
                    self.take();
                    if last < number || (!inclusive && last == number) { return Err(Diagnostic::new("W0100", end.span, "this range holds no number")); }
                    Expr { kind: Expression::Binary("&&", Box::new(compare(">=", number)), Box::new(compare(if inclusive { "<=" } else { "<" }, last))), span: token.span.clone() }
                } else { compare("==", number) };
                condition = Some(match condition {
                    None => test,
                    Some(earlier) => Expr { kind: Expression::Binary("||", Box::new(earlier), Box::new(test)), span: token.span },
                });
                if !self.symbol("|") { break; }
            }
            arms.push((condition.expect("one number"), self.block()?));
        }
        let end = self.token().span.clone();
        if rest.is_none() || !self.at(&Kind::Dedent) { return Err(Diagnostic::new("W0100", end, "a match on numbers ends with a `_` arm, or a name bound to the value, which is the last")); }
        self.expect(Kind::Dedent)?;
        let mut no = rest.expect("checked");
        for (condition, yes) in arms.into_iter().rev() { no = vec![Statement::If { condition, yes, no }]; }
        Ok(no.pop().expect("one arm"))
    }
    fn statement(&mut self) -> Result<Statement, Diagnostic> {
        if self.keyword("proof") { return Ok(Statement::Proof(self.proof_steps()?)); }
        // `yield value`: the next value of the `gen fn` being read.
        // `yield` outside a `gen fn` (a closure inside one is outside it too).
        if !self.generator && matches!(&self.token().kind, Kind::Name(n) if n == "yield")
            && !matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol("=" | "(" | "." | "+=" | "-="))) {
            return Err(self.error("`yield` is written in a `gen fn`, and a closure inside one is not part of it"));
        }
        if self.generator && self.keyword("yield") {
            let value = self.expression(0)?;
            self.expect(Kind::Newline)?;
            return Ok(Statement::Yield { name: generator::YIELD.into(), value });
        }
        if self.keyword("assert") {
            let claim = self.expression(0)?;
            self.expect(Kind::Newline)?;
            return Ok(Statement::Assert(claim));
        }
        if self.keyword("match") {
            let value = self.expression(0)?;
            self.open_block()?;
            if matches!(self.token().kind, Kind::Integer(_)) { return self.number_arms(value); }
            let mut arms = Vec::new();
            let mut guards = 0;
            while !self.at(&Kind::Dedent) {
                // `.Tag` names a variant of an anonymous sum.
                // `A | B:` is an arm for each pattern with the one body, which is checked for each.
                let mut patterns = vec![self.pattern()?];
                while self.symbol("|") { patterns.push(self.pattern()?); }
                // `Some(x) if x > 3:`: the arm is taken only when the guard holds.
                let guard = if self.keyword("if") { guards += 1; Some(self.expression(0)?) } else { None };
                let body = self.block()?;
                for Pattern { variant, binding, nested, open } in patterns {
                    if guard.is_some() && variant == "_" { return Err(self.error("a `_` arm has no guard: it is the arm taken when no other is")); }
                    arms.push((MatchArm { variant, binding, body: body.clone(), nested, open }, guard.clone()));
                }
            }
            self.expect(Kind::Dedent)?;
            if guards == 0 { return Ok(Statement::Match { value, arms: arms.into_iter().map(|(arm, _)| arm).collect() }); }
            fn place(e: &Expr) -> bool { match &e.kind { Expression::Name(_) => true, Expression::Field(inner, _) => place(inner), _ => false } }
            if !place(&value) { return Err(Diagnostic::new("W0100", value.span.clone(), "a match with guards matches a name or a field of one: when a guard fails the value is matched again by the arms after it")); }
            if guards > 8 { return Err(Diagnostic::new("W0100", value.span.clone(), "a match has at most 8 guards")); }
            // A guard does not name the matched value: a failed guard has it matched again by the
            // later arms, which must see the value first matched (red-team review: a guard that
            // changed it through `&mut` left no arm to run). It reads what the arm binds.
            fn root(e: &Expr) -> &str { match &e.kind { Expression::Field(inner, _) => root(inner), Expression::Name(n) => n, _ => "" } }
            let matched = root(&value).to_string();
            for (_, guard) in &arms {
                let Some(guard) = guard else { continue };
                let mut named = false;
                let mut probe = guard.clone();
                probe.visit_mut(&mut |e| { if matches!(&e.kind, Expression::Name(n) if *n == matched) { named = true; } false });
                if named { return Err(Diagnostic::new("W0100", guard.span.clone(), "a guard reads what its arm binds, not the matched value: the arms after it match that value again when it fails")); }
            }
            return guarded(&value, &arms);
        }
        let here = self.token().span.clone();
        for (word, statement) in [("break", Statement::Break), ("continue", Statement::Continue), ("unreachable", Statement::Unreachable(Site { span: here, why: Why::Written }))] {
            if matches!(&self.token().kind, Kind::Name(n) if n == word) && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Newline)) {
                self.take(); self.expect(Kind::Newline)?;
                return Ok(statement);
            }
        }
        if self.keyword("while") {
            // `while let Variant(x) = value:` is `while true:` around a `match` of that arm and a
            // `_` arm that leaves the loop; `value` is evaluated again on each pass.
            let pattern = if self.keyword("let") {
                let pattern = self.pattern()?;
                self.expect(Kind::Symbol("="))?;
                Some(pattern)
            } else { None };
            let condition = self.expression(0)?;
            if let Some(Pattern { variant, binding, nested, open }) = pattern {
                let (invariants, body) = self.nested(|parser| {
                    parser.open_block()?;
                    let mut invariants = Vec::new();
                    while parser.keyword("invariant") {
                        invariants.push(parser.expression(0)?); parser.expect(Kind::Newline)?;
                    }
                    let mut body = Vec::new();
                    while !parser.at(&Kind::Dedent) { body.push(parser.statement()?); }
                    parser.expect(Kind::Dedent)?;
                    Ok((invariants, body))
                })?;
                let span = condition.span.clone();
                let arms = vec![
                    MatchArm { variant, binding, body, nested, open },
                    MatchArm { variant: "_".into(), binding: None, body: vec![Statement::Break], nested: None, open: vec![] },
                ];
                return Ok(Statement::While { condition: Expr { kind: Expression::Bool(true), span }, invariants, body: vec![Statement::Match { value: condition, arms }] });
            }
            let (invariants, body) = self.nested(|parser| {
                parser.open_block()?;
                let mut invariants = Vec::new();
                while parser.keyword("invariant") {
                    invariants.push(parser.expression(0)?); parser.expect(Kind::Newline)?;
                }
                let mut body = Vec::new();
                while !parser.at(&Kind::Dedent) { body.push(parser.statement()?); }
                parser.expect(Kind::Dedent)?;
                Ok((invariants, body))
            })?;
            return Ok(Statement::While { condition, invariants, body });
        }
        // `for (a, b) in values:` takes each value apart: `for ?for<at> in values:` whose body
        // starts `let (a, b) = ?for<at>`. `for (i, x) in values.enumerate():` is
        // `for i, x in values:`, the walk with an index. Both are rewritten here, in the tokens,
        // and parsed as those loops.
        if matches!(&self.token().kind, Kind::Name(n) if n == "for") && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol("("))) {
            let at = self.token().span.clone();
            let mut close = self.cursor + 2;
            let mut names = Vec::new();
            loop {
                match self.tokens.get(close).map(|t| &t.kind) {
                    Some(Kind::Name(name)) => names.push(name.clone()),
                    _ => return Err(Diagnostic::new("W0100", at, "`for (a, b) in` names each part of the value")),
                }
                match self.tokens.get(close + 1).map(|t| &t.kind) {
                    Some(Kind::Symbol(",")) => close += 2,
                    Some(Kind::Symbol(")")) => { close += 1; break; }
                    _ => return Err(Diagnostic::new("W0100", at, "`for (a, b) in` names each part of the value")),
                }
            }
            if names.len() < 2 { return Err(Diagnostic::new("W0100", at, "`for (a, b) in` takes a value of two parts or more apart; one name is `for item in`")); }
            if !matches!(self.tokens.get(close + 1).map(|t| &t.kind), Some(Kind::Name(n)) if n == "in") { return Err(Diagnostic::new("W0100", at, "expected `in` after `for (a, b)`")); }
            // The first `:` at depth 0, where the block opens (review: the last one could be a
            // body's, were a one-line body allowed).
            let mut depth = 0i32;
            let mut colon = None;
            for index in close + 2..self.tokens.len() {
                match &self.tokens[index].kind {
                    Kind::Symbol("(" | "[" | "{") => depth += 1,
                    Kind::Symbol(")" | "]" | "}") => depth -= 1,
                    Kind::Symbol(":") if depth == 0 => { colon = Some(index); break; }
                    Kind::Newline | Kind::End => break,
                    _ => {}
                }
            }
            let Some(colon) = colon else { return Err(Diagnostic::new("W0100", at, "expected `:` here; a line that opens a block (`if`, `for`, `while`, `match`, `fn`, `type`) ends with `:`")) };
            let enumerate = names.len() == 2 && colon >= 4
                && matches!(&self.tokens[colon - 4].kind, Kind::Symbol("."))
                && matches!(&self.tokens[colon - 3].kind, Kind::Name(n) if n == "enumerate")
                && matches!(&self.tokens[colon - 2].kind, Kind::Symbol("("))
                && matches!(&self.tokens[colon - 1].kind, Kind::Symbol(")"));
            let token = |kind: Kind| Token { kind, span: at.clone() };
            if enumerate {
                self.tokens.drain(colon - 4..colon);
                let pair = vec![token(Kind::Name(names[0].clone())), token(Kind::Symbol(",")), token(Kind::Name(names[1].clone()))];
                self.tokens.splice(self.cursor + 1..=close, pair);
                return self.statement();
            }
            let hidden = format!("?for{}", at.start);
            self.tokens.splice(self.cursor + 1..=close, [token(Kind::Name(hidden.clone()))]);
            let mut made = self.statement()?;
            if let Statement::For { body, .. } = &mut made {
                let value = Expr { kind: Expression::Name(hidden), span: at.clone() };
                body.insert(0, Statement::OpenFields { fields: names.into_iter().enumerate().map(|(index, name)| (index.to_string(), name)).collect(), value, mutable: false, ty: None });
            }
            return Ok(made);
        }
        // `for item in …` or `for index, item in container`.
        let pair = matches!(self.tokens.get(self.cursor + 2).map(|t| &t.kind), Some(Kind::Symbol(","))) && matches!(self.tokens.get(self.cursor + 4).map(|t| &t.kind), Some(Kind::Name(n)) if n == "in");
        if matches!(&self.token().kind, Kind::Name(n) if n == "for") && (pair || matches!(self.tokens.get(self.cursor + 2).map(|t| &t.kind), Some(Kind::Name(n)) if n == "in")) {
            self.take();
            let index = if pair { let name = self.name()?; self.take(); Some(name) } else { None };
            let binding = self.name()?;
            self.take();
            let start = self.expression(0)?;
            // `a..=b` includes `b`: its end is marked `..=` for the rewrite (iteration.rs).
            let end = if self.symbol("..") {
                let inclusive = self.symbol("=");
                let end = self.expression(0)?;
                Some(if inclusive { Expr { span: end.span.clone(), kind: Expression::Unary("..=", Box::new(end)) } } else { end })
            } else { None };
            if index.is_some() && end.is_some() { return Err(Diagnostic::new("W0100", start.span.clone(), "`for index, item in` walks a container; a count has one name, `for index in a..b`")); }
            // A count is a U64 a closure in the body may capture; an item's type is not known here.
            // Both are the body's, one level in.
            let (scope, called) = (self.scope_types.len(), self.fn_locals.clone());
            let u64 = Type::Named("U64".into(), Vec::new());
            let level = self.indent + 1;
            if let Some(index) = &index { self.scope_types.push((index.clone(), Some(u64.clone()), level)); }
            self.scope_types.push((binding.clone(), end.is_some().then_some(u64), level));
            self.fn_locals.retain(|(n, _)| *n != binding);
            let parsed = self.nested(|parser| {
                parser.open_block()?;
                let mut invariants = Vec::new();
                while parser.keyword("invariant") {
                    invariants.push(parser.expression(0)?); parser.expect(Kind::Newline)?;
                }
                let mut body = Vec::new();
                while !parser.at(&Kind::Dedent) { body.push(parser.statement()?); }
                parser.expect(Kind::Dedent)?;
                Ok((invariants, body))
            });
            self.scope_types.truncate(scope);
            self.fn_locals = called;
            let (invariants, body) = parsed?;
            if !invariants.is_empty() && matches!(&end, Some(Expr { kind: Expression::Unary("..=", _), .. })) {
                return Err(Diagnostic::new("W0100", start.span.clone(), "a loop over `a..=b` takes no invariants: write `a..b + 1` when `b` is below the largest U64"));
            }
            return Ok(Statement::For { index, binding, start, end, invariants, body });
        }
        // `nosuspend:`: no call in the block may pause its task, cleanup included. The block is
        // an ordinary block; its range is what the effect check reads (docs/design-async.md).
        if matches!(&self.token().kind, Kind::Name(n) if n == "nosuspend") && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol(":"))) {
            let start = self.take().span.start;
            let body = self.block()?;
            let end = self.tokens[self.cursor.saturating_sub(1)].span.end;
            self.quiet.push((start, end));
            return Ok(Statement::Block(body));
        }
        // `brand roads.nodes() as nodes:`: an opening (docs/design-brands.md). The value is a call
        // of a function with a `Fresh` type parameter; the checker gives that parameter a brand
        // of its own, new at this opening, which nothing outside the block can name. Read as a
        // block whose first line binds the name to the marked call.
        if matches!(self.token().kind, Kind::Brand) {
            let at = self.take().span.clone();
            let value = self.expression(0)?;
            if !self.keyword("as") { return Err(self.error("an opening reads `brand value as name:`")); }
            let name = self.name()?;
            let mut body = self.block()?;
            let span = at.start..value.span.end;
            let opened = Expr { kind: Expression::Call(Box::new(Expr { kind: Expression::Name(OPENING.to_string()), span: span.clone() }), vec![value]), span };
            body.insert(0, Statement::Bind { mutable: false, name, ty: None, value: opened, proof: None });
            return Ok(Statement::Block(body));
        }
        if self.keyword("using") {
            let mutable = self.keyword("var");
            let name = self.name()?;
            let ty = if self.symbol(":") { Some(self.ty()?) } else { None };
            self.expect(Kind::Symbol("="))?;
            let value = self.expression(0)?;
            let body = self.block()?;
            return Ok(Statement::Using { mutable, name, ty, value, body });
        }
        // `if c: a else: b` (or `if let`) on one line is a value, as a statement too: a function's
        // last line gives it back.
        if matches!(&self.token().kind, Kind::Name(n) if n == "if") {
            // Read by tokens, not by parsing (parsing a text literal's `${...}` makes code, so
            // it is parsed once): the first `:` outside brackets ends the condition, and on one
            // line a value follows it.
            let mut depth = 0usize;
            let mut one_line = false;
            for at in self.cursor + 1..self.tokens.len() {
                match &self.tokens[at].kind {
                    Kind::Newline | Kind::End => break,
                    Kind::Symbol("(" | "[" | "{") => depth += 1,
                    Kind::Symbol(")" | "]" | "}") => depth = depth.saturating_sub(1),
                    Kind::Symbol(":") if depth == 0 => {
                        one_line = !matches!(self.tokens.get(at + 1).map(|t| &t.kind), Some(Kind::Newline) | None);
                        break;
                    }
                    _ => {}
                }
            }
            if one_line {
                // The `if` or `match` statement it reads as, each branch one value.
                let value = self.expression(0)?;
                self.expect(Kind::Newline)?;
                // Branches that are one-line `if`s themselves become statements the same way.
                fn statement(value: Expr) -> Statement {
                    let Expression::Conditional(condition, yes, no) = value.kind else { return Statement::Expr(value) };
                    match condition.kind {
                        Expression::Matches(matched, arm) => Statement::Match { value: *matched, arms: vec![
                            MatchArm { body: vec![statement(*yes)], ..*arm },
                            MatchArm { variant: "_".into(), binding: None, body: vec![statement(*no)], nested: None, open: vec![] },
                        ] },
                        _ => Statement::If { condition: *condition, yes: vec![statement(*yes)], no: vec![statement(*no)] },
                    }
                }
                return Ok(statement(value));
            }
        }
        if self.keyword("if") {
            // `if let Variant(x) = value: ... else: ...` is a `match` of that one arm and a `_` arm
            // (book: tour-errors.md). The `_` arm may not drop an owner, as any `_` arm may not:
            // when the other variants hold one, the program writes the whole `match`.
            if self.keyword("let") {
                let pattern = self.pattern()?;
                self.expect(Kind::Symbol("="))?;
                let value = self.expression(0)?;
                let yes = self.block()?;
                let no = if self.keyword("else") {
                    if matches!(&self.token().kind, Kind::Name(n) if n == "if") { vec![self.statement()?] } else { self.block()? }
                } else {
                    vec![Statement::Expr(Expr { kind: Expression::Unit, span: value.span.clone() })]
                };
                let Pattern { variant, binding, nested, open } = pattern;
                let arms = vec![
                    MatchArm { variant, binding, body: yes, nested, open },
                    MatchArm { variant: "_".into(), binding: None, body: no, nested: None, open: vec![] },
                ];
                return Ok(Statement::Match { value, arms });
            }
            let condition = self.expression(0)?;
            let yes = self.block()?;
            let no = if self.keyword("else") {
                // `else if` is an `if` alone in the else block.
                if matches!(&self.token().kind, Kind::Name(n) if n == "if") { vec![self.statement()?] } else { self.block()? }
            } else {
                Vec::new()
            };
            return Ok(Statement::If { condition, yes, no });
        }
        // `var (a, b) = pair`: the same, each name a variable.
        if matches!(&self.token().kind, Kind::Name(n) if n == "var") && matches!(self.tokens.get(self.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol("("))) {
            self.take();
            return self.tuple_binding(true);
        }
        let statement = if self.keyword("let") {
            if self.symbol(".") {
                self.expect(Kind::Symbol("{"))?;
                let fields = self.parameters("}", |parser| {
                    let label = parser.name()?;
                    let name = if parser.symbol(":") { parser.name()? } else { label.clone() };
                    Ok((label, name))
                })?;
                self.expect(Kind::Symbol("="))?;
                let value = self.expression(0)?;
                self.expect(Kind::Newline)?;
                return Ok(Statement::OpenFields { fields, value, mutable: false, ty: None });
            }
            // `let (a, b) = pair`: a tuple opened by position; `_` takes nothing from its place.
            if self.at(&Kind::Symbol("(")) { return self.tuple_binding(false); }
            if self.tokens.get(self.cursor + 1).is_some_and(|token| token.kind == Kind::Symbol("(")) {
                let variant = self.name()?;
                self.expect(Kind::Symbol("("))?;
                let mut names = self.parameters(")", Self::name)?;
                let ty = if self.symbol(":") { Some(self.ty()?) } else { None };
                self.expect(Kind::Symbol("="))?;
                let value = self.expression(0)?;
                if !self.keyword("else") {
                    if ty.is_some() { return Err(self.error("a typed `let Variant(name): Type = value` needs `else`")); }
                    self.expect(Kind::Newline)?;
                    // Each name has its field's type, when the record is known here; a closure that
                    // captures one without is refused (it was not seen as a capture at all).
                    let types = self.record_fields.get(&variant).filter(|types| types.len() == names.len()).cloned();
                    for (index, name) in names.iter().enumerate() {
                        self.scope_types.push((name.clone(), types.as_ref().map(|types| types[index].clone()), self.indent));
                    }
                    return Ok(Statement::Open { record: variant, names, value });
                }
                if names.len() != 1 { return Err(self.error("refutable binding takes one name")); }
                let binding = names.remove(0);
                let variant_failure = self.name()?;
                let payload = if self.symbol("(") {
                    let name = self.name()?; self.expect(Kind::Symbol(")"))?; Some(name)
                } else { None };
                let failure = MatchArm { variant: variant_failure, binding: payload, body: self.block()?, nested: None, open: vec![] };
                // What it binds is in scope after the `else` block; with its type, a closure may capture it.
                self.scope_types.push((binding.clone(), ty.clone(), self.indent));
                self.fn_locals.retain(|(n, _)| *n != binding);
                return Ok(Statement::Guard { value, variant, binding, ty, failure });
            }
            self.binding(false)?
        } else if self.keyword("var") {
            self.binding(true)?
        } else if self.keyword("return") {
            // `return match ...:` returns the value that ends whichever path runs.
            if self.chooses() { return self.chosen(&|value| Statement::Return(Some(value))); }
            Statement::Return(if self.at(&Kind::Newline) {
                None
            } else {
                Some(self.expression(0)?)
            })
        } else {
            let target = self.expression(0)?;
            let operation = ["=", "+=", "-="].into_iter().find(|s| self.symbol(s));
            // `x *= y` is `x = x * y`. The target is read twice, so it must be one whose reading runs nothing.
            let scaled = [("*=", "*"), ("/=", "/"), ("%=", "%")].into_iter().find(|(symbol, _)| self.symbol(symbol));
            fn quiet(expr: &Expr) -> bool {
                match &expr.kind {
                    Expression::Name(_) | Expression::Integer(_) => true,
                    Expression::Field(value, _) | Expression::Unary("*", value) => quiet(value),
                    Expression::Index(value, index) => quiet(value) && quiet(index),
                    _ => false,
                }
            }
            // `target = match ...:` assigns the value that ends whichever path runs. The target is
            // written once per path, so it must be one whose reading runs nothing.
            if (operation.is_some() || scaled.is_some()) && self.chooses() {
                if !quiet(&target) { return Err(Diagnostic::new("W0101", target.span.clone(), "assigning a `match` or `if` needs a target that is a name, a field or an element at a named index")); }
                return self.chosen(&|value| match (operation, scaled) {
                    (Some(operation), _) => Statement::Assign { target: target.clone(), operation, value },
                    (None, Some((_, operator))) => {
                        let span = target.span.start..value.span.end;
                        Statement::Assign { target: target.clone(), operation: "=", value: Expr { kind: Expression::Binary(operator, Box::new(target.clone()), Box::new(value)), span } }
                    }
                    (None, None) => unreachable!(),
                });
            }
            if let Some((symbol, operator)) = scaled {
                if !quiet(&target) {
                    return Err(Diagnostic::new("W0101", target.span.clone(), match symbol {
                        "*=" => "`*=` needs a target that is a name, a field or an element at a named index",
                        "/=" => "`/=` needs a target that is a name, a field or an element at a named index",
                        _ => "`%=` needs a target that is a name, a field or an element at a named index",
                    }));
                }
                let value = self.expression(0)?;
                let span = target.span.start..value.span.end;
                Statement::Assign { target: target.clone(), operation: "=", value: Expr { kind: Expression::Binary(operator, Box::new(target), Box::new(value)), span } }
            } else if let Some(operation) = operation {
                Statement::Assign {
                    target,
                    operation,
                    value: self.expression(0)?,
                }
            } else {
                Statement::Expr(target)
            }
        };
        // A `match` or `if` bound as a value ends with its own block.
        // A closure with a block body ends with that block too (closure.rs).
        let block_ended = self.cursor > 0 && matches!(self.tokens[self.cursor - 1].kind, Kind::Dedent);
        if !matches!(statement, Statement::Choose { .. }) && !block_ended { self.expect(Kind::Newline)?; }
        Ok(statement)
    }
    /// Whether a `match` or `if` starts here, where a value is expected.
    /// A `match` or an `if` whose paths are blocks: its line ends with `:`. A one-line
    /// `if c: a else: b` is an expression.
    fn chooses(&self) -> bool {
        match &self.token().kind {
            Kind::Name(n) if n == "match" => true,
            Kind::Name(n) if n == "if" => {
                let end = self.tokens[self.cursor..].iter().position(|t| matches!(t.kind, Kind::Newline | Kind::End)).map_or(self.tokens.len(), |n| self.cursor + n);
                end > self.cursor && matches!(self.tokens[end - 1].kind, Kind::Symbol(":"))
            }
            _ => false,
        }
    }
    /// The `match` or `if` that starts here, each of its paths ending in what `with` makes of its value.
    fn chosen(&mut self, with: &dyn Fn(Expr) -> Statement) -> Result<Statement, Diagnostic> {
        let mut inner = vec![self.statement()?];
        if let Err(message) = Self::tails(&mut inner, with) { return Err(self.error(message)); }
        Ok(inner.remove(0))
    }
    /// Turns the value that ends each path of a `match` or `if` into the statement `with` makes of it.
    fn tails(body: &mut Vec<Statement>, with: &dyn Fn(Expr) -> Statement) -> Result<(), &'static str> {
        match body.last_mut() {
            Some(Statement::Expr(_)) => {
                let Some(Statement::Expr(value)) = body.pop() else { unreachable!() };
                body.push(with(value));
                Ok(())
            }
            Some(Statement::Return(_) | Statement::Break | Statement::Continue | Statement::Unreachable(_)) => Ok(()),
            Some(Statement::If { yes, no, .. }) => {
                if no.is_empty() { return Err("an `if` used as a value needs an `else`"); }
                Self::tails(yes, with)?;
                Self::tails(no, with)
            }
            Some(Statement::Match { arms, .. }) => arms.iter_mut().try_for_each(|arm| Self::tails(&mut arm.body, with)),
            Some(Statement::Using { body, .. }) => Self::tails(body, with),
            _ => Err("each path of a `match` or `if` used as a value ends with the value, or leaves with `return`, `break`, `continue` or `unreachable`"),
        }
    }
    fn binding(&mut self, mutable: bool) -> Result<Statement, Diagnostic> {
        let name = self.name()?;
        let ty = if self.symbol(":") {
            Some(self.ty()?)
        } else {
            None
        };
        self.expect(Kind::Symbol("="))?;
        if self.chooses() {
            let inner = self.chosen(&|value| Statement::Yield { name: name.clone(), value })?;
            return Ok(Statement::Choose { mutable, name, ty, inner: Box::new(inner) });
        }
        let value = self.expression(0)?;
        // What a later closure may capture, and whether this name is now called as a function.
        // Unwritten, the type is taken from what is plainly evident (`allocator.share()`, `move x`).
        let known = ty.clone().or_else(|| self.evident(&value));
        self.scope_types.push((name.clone(), known, self.indent));
        let closure = matches!(&value.kind, Expression::Call(callee, _) if matches!(&callee.kind, Expression::Name(n) if n.starts_with("Closure#")));
        self.fn_locals.retain(|(n, _)| *n != name);
        if closure { self.fn_locals.push((name.clone(), self.indent)); }
        let proof = if self.keyword("by") {
            Some(self.justification()?)
        } else {
            None
        };
        Ok(Statement::Bind {
            mutable,
            name,
            ty,
            value,
            proof,
        })
    }
    fn justification(&mut self) -> Result<Justification, Diagnostic> {
        if self.keyword("trust") {
            return Err(Diagnostic::new("W0304", self.token().span.clone(),
                "trust cannot discharge a safety obligation; supply a checked proof"));
        }
        let mut name = self.name()?;
        while self.symbol(".") { name.push('.'); name.push_str(&self.name()?); }
        Ok(Justification::Rule(name))
    }
}

fn a_borrows(variant: String, from: Vec<String>, span: std::ops::Range<usize>) -> Borrows { Borrows { variant, from, span } }

/// A match whose arms have guards, as matches without: a guarded arm runs its body when the guard
/// holds and otherwise matches the same value again by the arms after it. Later arms for a variant
/// whose arm has a guard are reached only through that inner match, so the arms are still tried in
/// the order written. The value is read again, so one that holds an owner is refused as used after
/// it moved, as it would be written out by hand.
fn guarded(value: &Expr, arms: &[(MatchArm, Option<Expr>)]) -> Result<Statement, Diagnostic> {
    let mut out = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    for (at, (arm, guard)) in arms.iter().enumerate() {
        if arm.variant != "_" && taken.contains(&arm.variant) { continue; }
        let Some(guard) = guard else { out.push(arm.clone()); continue };
        let rest = &arms[at + 1..];
        if !rest.iter().any(|(later, _)| later.variant == arm.variant || later.variant == "_") {
            return Err(Diagnostic::new("W0100", guard.span.clone(), "when this guard fails, a later arm for the same variant, or `_`, is taken: write one"));
        }
        if arm.nested.is_some() { return Err(Diagnostic::new("W0100", guard.span.clone(), "a guard follows a pattern one level deep: match the inner value in the arm")); }
        let otherwise = guarded(value, rest)?;
        // The arm binds hidden names, so the inner match may bind the same ones; the guard reads
        // the hidden names, and the body binds the written ones to them first.
        let hidden = |name: &str| format!("guard#{name}#{}#{}", arm.variant, guard.span.start);
        let names: Vec<String> = arm.binding.iter().chain(arm.open.iter()).filter(|n| *n != "_").cloned().collect();
        let mut condition = guard.clone();
        condition.visit_mut(&mut |e| { if let Expression::Name(n) = &mut e.kind && names.contains(n) { *n = hidden(n); } false });
        let mut yes: Vec<Statement> = names.iter().map(|n| Statement::Bind { mutable: false, name: n.clone(), ty: None, value: Expr { kind: Expression::Name(hidden(n)), span: guard.span.clone() }, proof: None }).collect();
        yes.extend(arm.body.iter().cloned());
        let rename = |n: &String| if n == "_" { n.clone() } else { hidden(n) };
        let body = vec![Statement::If { condition, yes, no: vec![otherwise] }];
        out.push(MatchArm { body, binding: arm.binding.as_ref().map(rename), open: arm.open.iter().map(rename).collect(), ..arm.clone() });
        taken.push(arm.variant.clone());
    }
    // Arms of the variants taken earlier are missing from an inner match, and a `_` may name no
    // variant left once guarded arms have their own: `_?` is the `_` arm that may be taken by none
    // (wasm/nested_patterns.rs). An inner one is never run: it is reached only for the guarded
    // variant, which a later arm names.
    const REST: &str = "_?";
    for arm in out.iter_mut() { if arm.variant == "_" { arm.variant = REST.into(); } }
    if !out.iter().any(|arm| arm.variant == REST) {
        out.push(MatchArm { variant: REST.into(), binding: None, body: vec![Statement::Expr(Expr { kind: Expression::Unit, span: value.span.clone() })], nested: None, open: vec![] });
    }
    Ok(Statement::Match { value: value.clone(), arms: out })
}

/// `fn name(...) -> Type:` at the start of a line, without type parameters: the result type, read
/// from the tokens before anything is parsed, so a function declared later is known too.
fn results(tokens: &[Token], modules: &[usize]) -> std::collections::BTreeMap<String, Option<Type>> {
    let mut found: std::collections::BTreeMap<String, Option<Type>> = Default::default();
    for at in 0..tokens.len() {
        if !matches!(&tokens[at].kind, Kind::Name(n) if n == "fn") { continue; }
        if at > 0 && !matches!(tokens[at - 1].kind, Kind::Newline | Kind::Indent | Kind::Dedent) && !matches!(&tokens[at - 1].kind, Kind::Name(n) if matches!(n.as_str(), "pub" | "pure" | "nopanic" | "gen")) { continue; }
        let Some(Token { kind: Kind::Name(name), .. }) = tokens.get(at + 1) else { continue };
        // A generic function's result depends on the call: its name is known to be unknown, so a
        // plain function of the same name elsewhere does not lend it a type (red-team review).
        if matches!(tokens.get(at + 2).map(|t| &t.kind), Some(Kind::Symbol("<"))) { found.insert(name.clone(), None); continue; }
        if !matches!(tokens.get(at + 2).map(|t| &t.kind), Some(Kind::Symbol("("))) { continue; }
        // A method (`self` first) is not called by its bare name. A function whose first
        // parameter is a plain named type without type arguments (`value: &String`) is also
        // kept as `Owner.name`, for `x.name()` (the same key twice is unknown, as below).
        let is_method = matches!(tokens.get(at + 3).map(|t| &t.kind), Some(Kind::Name(n)) if n == "self");
        let mut from = at + 5;
        while matches!(tokens.get(from).map(|t| &t.kind), Some(Kind::Symbol("&"))) || matches!(tokens.get(from).map(|t| &t.kind), Some(Kind::Name(m)) if m == "mut") { from += 1; }
        let owner = match (tokens.get(at + 3).map(|t| &t.kind), tokens.get(at + 4).map(|t| &t.kind), tokens.get(from).map(|t| &t.kind), tokens.get(from + 1).map(|t| &t.kind)) {
            (Some(Kind::Name(_)), Some(Kind::Symbol(":")), Some(Kind::Name(owner)), Some(Kind::Symbol("," | ")"))) => Some(format!("{owner}.{name}")),
            _ => None,
        };
        let keys: Vec<String> = owner.into_iter().chain((!is_method).then(|| name.clone())).collect();
        if keys.is_empty() { continue; }
        let mut depth = 0usize;
        let mut close = None;
        for (index, token) in tokens.iter().enumerate().skip(at + 2) {
            match token.kind { Kind::Symbol("(" | "[" | "{") => depth += 1, Kind::Symbol(")" | "]" | "}") => { depth -= 1; if depth == 0 { close = Some(index); break; } } Kind::Newline | Kind::End => break, _ => {} }
        }
        let Some(close) = close else { continue };
        // A `gen fn` (not generic) whose parameters are all numbers or Bools: a call of it makes its
        // generator, a record of numbers (its locals are numbers too), whose type is named for it.
        // Such a generator may be a local of another `gen fn` (generator.rs `collect`).
        if at > 0 && matches!(&tokens[at - 1].kind, Kind::Name(g) if g == "gen") && !is_method {
            let mut numbers = true;
            let mut index = at + 3;
            while index < close {
                // `name: Type` then `,` or the closing `)`.
                let shaped = matches!(tokens.get(index).map(|t| &t.kind), Some(Kind::Name(_)))
                    && matches!(tokens.get(index + 1).map(|t| &t.kind), Some(Kind::Symbol(":")))
                    && matches!(tokens.get(index + 2).map(|t| &t.kind), Some(Kind::Name(t)) if matches!(t.as_str(), "U8" | "U16" | "U32" | "U64" | "I64" | "F64" | "Bool"))
                    && (index + 3 == close || matches!(tokens.get(index + 3).map(|t| &t.kind), Some(Kind::Symbol(","))));
                if !shaped { numbers = false; break; }
                index += 4;
            }
            if numbers && !generator::built_in(&generator::type_name(name)) {
                let made = Type::Named(generator::type_name(name), Vec::new());
                match found.get(name) { Some(_) => { found.insert(name.clone(), None); } None => { found.insert(name.clone(), Some(made)); } }
                // How to make one to start from: the function and its parameters' types, under a
                // key no source can spell (generator.rs `default`).
                let parameters: Vec<Type> = (at + 3..close).step_by(4).filter_map(|i| match &tokens[i + 2].kind { Kind::Name(t) => Some(Type::Named(t.clone(), Vec::new())), _ => None }).collect();
                found.insert(format!("#generator {}", generator::type_name(name)), Some(Type::Named(name.clone(), parameters)));
            }
            continue;
        }
        let ty = if matches!(tokens.get(close + 1).map(|t| &t.kind), Some(Kind::Symbol("->"))) {
            let end = tokens[close + 2..].iter().position(|t| matches!(t.kind, Kind::Symbol(":") | Kind::Newline)).map(|p| close + 2 + p);
            end.and_then(|end| {
                let mut slice = tokens[close + 2..end].to_vec();
                slice.push(Token { kind: Kind::End, span: tokens[end].span.clone() });
                let mut parser = Parser::written(slice, modules);
                parser.ty().ok().filter(|t| matches!(t, Type::Named(n, a) if a.is_empty() && matches!(n.as_str(), "U8" | "U16" | "U32" | "U64" | "I64" | "F64" | "Bool")))
            })
        } else { None };
        for name in keys { match found.get(&name) { Some(_) => { found.insert(name, None); } None => { found.insert(name, ty.clone()); } } }
    }
    found
}
