//! witchy-syntax's tests. The library target is `test = false` (its
//! `cfg(test)` build was a second full compile of the crate), so the
//! surviving pins -- capability/privacy boundaries, distinct source
//! refusals, and catalog consistency -- run here against the public API.

#[path = "syntax/async_lower.rs"]
mod async_lower;
#[path = "syntax/cap_ops.rs"]
mod cap_ops;
#[path = "syntax/derive.rs"]
mod derive;
#[path = "syntax/generators.rs"]
mod generators;
#[path = "syntax/intrinsics.rs"]
mod intrinsics;
#[path = "syntax/lambda_scan.rs"]
mod lambda_scan;
#[path = "syntax/lexer.rs"]
mod lexer;
#[path = "syntax/linker.rs"]
mod linker;
#[path = "syntax/parser.rs"]
mod parser;
#[path = "syntax/source_check.rs"]
mod source_check;
#[path = "syntax/type_resolve.rs"]
mod type_resolve;
#[path = "syntax/compiler_session.rs"]
mod compiler_session;

use witchy_syntax::ast;
#[path = "syntax/async_local_type_hints.rs"]
mod async_local_type_hints;
