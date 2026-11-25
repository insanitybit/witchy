/-! Indexed lookup in an array given as a tree of appended parts. The same
    proof as `Witchy.Core.CodeTree`, for any element type. -/
namespace Witchy.LookupTree

inductive Tree (α : Type) where
  | leaf (code : Array α)
  | branch (left right : Tree α)

def Tree.flatten {α : Type} : Tree α → Array α
  | .leaf code => code
  | .branch left right => left.flatten ++ right.flatten

def Tree.size {α : Type} : Tree α → Nat
  | .leaf code => code.size
  | .branch left right => left.size + right.size

def Tree.lookup {α : Type} : Tree α → Nat → Option α
  | .leaf code, pc => code[pc]?
  | .branch left right, pc =>
    if pc < left.size then left.lookup pc else right.lookup (pc - left.size)

theorem Tree.flatten_size {α : Type} (tree : Tree α) : tree.flatten.size = tree.size := by
  induction tree with
  | leaf code => rfl
  | branch left right ihLeft ihRight => simp [flatten, size, ihLeft, ihRight]

theorem Tree.lookup_flatten {α : Type} (tree : Tree α) (pc : Nat) :
    tree.lookup pc = tree.flatten[pc]? := by
  induction tree generalizing pc with
  | leaf code => rfl
  | branch left right ihLeft ihRight =>
    simp only [lookup, flatten, Array.getElem?_append, flatten_size, ihLeft, ihRight]

/-- A fetch from an array proved equal to the tree's flattening is the tree's lookup. -/
theorem Tree.fetch {α : Type} (tree : Tree α) (code : Array α) (same : tree.flatten = code)
    (pc : Nat) (value : Option α) (found : tree.lookup pc = value) : code[pc]? = value := by
  rw [← same, ← tree.lookup_flatten pc]; exact found

end Witchy.LookupTree
