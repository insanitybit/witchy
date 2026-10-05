//! Stable identities for checked expression nodes (RFC-0174, stable fact keys).
//!
//! A [`NodeKey`] names an expression by the item that owns it and its preorder
//! position in that item, so the same content keeps the same key wherever it
//! is allocated. A [`NodeIndex`] maps the expressions of one module
//! allocation to their keys; fact tables key their facts by `NodeKey` and
//! resolve a `&Expr` through the index of the allocation they describe.

use foldhash::{HashMap, HashMapExt as _, HashSet};
use std::sync::{Mutex, OnceLock};

use witchy_syntax::ast::{Block, Expr, Item, Module};

/// An expression's owning item and preorder position within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct NodeKey {
    owner: OwnerId,
    node: u32,
}

impl NodeKey {
    pub(crate) fn owner(self) -> OwnerId {
        self.owner
    }
}

/// An interned item name ([`owner_name`]); the same name has the same id
/// everywhere in the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct OwnerId(u32);

impl OwnerId {
    /// The owner of a free function's body.
    pub(crate) fn of_function(name: &str) -> Self {
        intern(&format!("fn {name}"))
    }
}

fn intern(name: &str) -> OwnerId {
    static NAMES: OnceLock<Mutex<std::collections::HashMap<String, u32>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&id) = names.get(name) {
        return OwnerId(id);
    }
    let id = u32::try_from(names.len()).expect("fewer than 2^32 expression owners");
    names.insert(name.to_owned(), id);
    OwnerId(id)
}

/// The expressions of one module allocation, by address, with their keys.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct NodeIndex {
    keys: HashMap<usize, NodeKey>,
}

impl NodeIndex {
    /// Index every expression the module's items own, in the order of
    /// `visit_module_exprs`. A name that occurs twice keeps the first
    /// occurrence's identity and gives later ones a numbered one.
    pub(crate) fn of(module: &Module) -> Self {
        let mut keys = HashMap::new();
        let mut seen: HashMap<String, u32> = HashMap::new();
        let mut owner_of = |name: String| {
            let occurrence = seen.entry(name.clone()).or_insert(0);
            *occurrence += 1;
            if *occurrence == 1 { intern(&name) } else { intern(&format!("{name} #{occurrence}")) }
        };
        let mut comptime = 0u32;
        for item in &module.items {
            match item {
                Item::Function(function) => {
                    index_block(&mut keys, owner_of(format!("fn {}", function.name)), &function.body);
                }
                Item::Impl(definition) => for method in &definition.methods {
                    let owner = owner_of(format!(
                        "impl {} for {} fn {}",
                        definition.trait_name.as_deref().unwrap_or(""), definition.type_name, method.name,
                    ));
                    index_block(&mut keys, owner, &method.body);
                },
                Item::Trait(definition) => for method in &definition.methods {
                    let Some(body) = &method.default else { continue };
                    index_block(&mut keys, owner_of(format!("trait {} fn {}", definition.name, method.name)), body);
                },
                Item::Const { name, value, .. } => {
                    let owner = owner_of(format!("const {name}"));
                    let mut node = 0u32;
                    let _ = witchy_syntax::ast::visit::visit_expr(value, &mut |expression: &Expr| -> Result<(), ()> {
                        keys.insert(expression as *const Expr as usize, NodeKey { owner, node });
                        node += 1;
                        Ok(())
                    });
                }
                Item::Comptime(block) => {
                    comptime += 1;
                    index_block(&mut keys, owner_of(format!("comptime {comptime}")), block);
                }
                Item::Type(_) | Item::TypeAlias { .. } => {}
            }
        }
        Self { keys }
    }

    /// The key of this exact expression, when it belongs to the indexed
    /// allocation.
    pub(crate) fn key(&self, expression: &Expr) -> Option<NodeKey> {
        self.key_at(expression as *const Expr as usize)
    }

    pub(crate) fn key_at(&self, address: usize) -> Option<NodeKey> {
        self.keys.get(&address).copied()
    }

    /// `map`'s entries for the indexed expressions, by key; entries for any
    /// other address are dropped.
    pub(crate) fn rekey_map<V>(&self, map: HashMap<usize, V>) -> HashMap<NodeKey, V> {
        map.into_iter().filter_map(|(address, value)| Some((self.key_at(address)?, value))).collect()
    }

    /// [`Self::rekey_map`] for a set.
    pub(crate) fn rekey_set(&self, set: HashSet<usize>) -> HashSet<NodeKey> {
        set.into_iter().filter_map(|address| self.key_at(address)).collect()
    }

    /// Every indexed expression's address and key.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (usize, NodeKey)> + '_ {
        self.keys.iter().map(|(&address, &key)| (address, key))
    }
}

fn index_block(keys: &mut HashMap<usize, NodeKey>, owner: OwnerId, block: &Block) {
    let mut node = 0u32;
    let _ = witchy_syntax::ast::visit::visit_block(block, &mut |expression: &Expr| -> Result<(), ()> {
        keys.insert(expression as *const Expr as usize, NodeKey { owner, node });
        node += 1;
        Ok(())
    });
}
