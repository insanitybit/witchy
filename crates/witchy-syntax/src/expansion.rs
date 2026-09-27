use crate::{ast::Module, origin::OriginTable};

type Expand = fn(&str, &mut Module, &[(String, Module)]) -> Result<OriginTable, String>;

/// An injected expansion pass and its per-input context dependency.
///
/// The request must be deterministic and side-effect free. Returning false
/// asks the linker to supply no siblings for that input; the expansion pass
/// still runs normally. Arbitrary callbacks receive full context by default.
#[derive(Clone, Copy)]
pub struct ComptimeExpander {
    expand: Expand,
    needs_siblings: fn(&Module) -> bool,
}

fn full_context(_: &Module) -> bool { true }

impl ComptimeExpander {
    pub const fn new(expand: Expand) -> Self {
        Self { expand, needs_siblings: full_context }
    }

    pub const fn with_sibling_request(expand: Expand, needs_siblings: fn(&Module) -> bool) -> Self {
        Self { expand, needs_siblings }
    }

    pub(crate) fn needs_siblings(self, module: &Module) -> bool {
        (self.needs_siblings)(module)
    }

    pub(crate) fn run(self, name: &str, module: &mut Module, siblings: &[(String, Module)]) -> Result<OriginTable, String> {
        (self.expand)(name, module, siblings)
    }

    // Identity separates cache namespaces only. It never determines whether
    // a callback needs context: that is the explicit request above.
    pub(crate) fn cache_identity(self) -> (usize, usize) {
        (self.expand as usize, self.needs_siblings as usize)
    }
}
