import Witchy.Core
import Witchy.AuthorityProofs

namespace Witchy.Storage

/-- Capabilities reference existing authority-table entries. This is distinct
    from allocation/loan ownership and from rights attenuation. -/
def valueCapabilitiesValid (authority : Authority.State) : Core.Value → Prop
  | .capability token => token < authority.tokens.length
  | _ => True

def cellsCapabilitiesValid (authority : Authority.State) (cells : List (Option Core.Value)) : Prop :=
  ∀ value, some value ∈ cells → valueCapabilitiesValid authority value

def heapCapabilitiesValid (authority : Authority.State) (memory : Memory.StateOf Core.Value) : Prop :=
  ∀ block, some block ∈ memory.blocks → cellsCapabilitiesValid authority block.slots

def allCapabilitiesValid (authority : Authority.State) (registers : List (Option Core.Value))
    (memory : Memory.StateOf Core.Value) : Prop :=
  cellsCapabilitiesValid authority registers ∧ heapCapabilitiesValid authority memory

theorem cells_caps_set (authority : Authority.State) (cells : List (Option Core.Value)) (index : Nat)
    (replacement : Option Core.Value) (valid : cellsCapabilitiesValid authority cells)
    (newValid : ∀ value, replacement = some value → valueCapabilitiesValid authority value) :
    cellsCapabilitiesValid authority (cells.set index replacement) := by
  intro value member
  rcases List.mem_or_eq_of_mem_set member with old | replaced
  · exact valid value old
  · exact newValid value replaced.symm

theorem heap_caps_set (authority : Authority.State) (memory : Memory.StateOf Core.Value) (handle : Nat)
    (replacement : Option (Storage.Block Core.Value)) (valid : heapCapabilitiesValid authority memory)
    (newValid : ∀ block, replacement = some block → cellsCapabilitiesValid authority block.slots) :
    heapCapabilitiesValid authority { memory with blocks := memory.blocks.set handle replacement } := by
  intro block member
  rcases List.mem_or_eq_of_mem_set member with old | replaced
  · exact valid block old
  · exact newValid block replaced.symm

theorem value_caps_monotone (authority next : Authority.State) (value : Core.Value)
    (growth : authority.tokens.length ≤ next.tokens.length) (valid : valueCapabilitiesValid authority value) :
    valueCapabilitiesValid next value := by
  cases value <;> simp_all [valueCapabilitiesValid]
  omega

theorem all_caps_monotone (authority next : Authority.State) (registers : List (Option Core.Value))
    (memory : Memory.StateOf Core.Value) (growth : authority.tokens.length ≤ next.tokens.length)
    (valid : allCapabilitiesValid authority registers memory) : allCapabilitiesValid next registers memory := by
  constructor
  · intro value member
    exact value_caps_monotone authority next value growth (valid.1 value member)
  · intro block member value cell
    exact value_caps_monotone authority next value growth (valid.2 block member value cell)

/-- Stored or registered capability validity composes with the existing root
    attenuation theorem; a token index alone is not an authority proof. -/
theorem valid_capability_root (roots : List Authority.Token) (authority : Authority.State) (index : Nat)
    (granted : Authority.Valid roots authority) (valid : valueCapabilitiesValid authority (.capability index)) :
    ∃ token root, authority.tokens[index]? = some token ∧ root ∈ roots ∧ token ⊆ root := by
  have bound : index < authority.tokens.length := valid
  let token := authority.tokens[index]'bound
  have found : authority.tokens[index]? = some token := List.getElem?_eq_getElem bound
  obtain ⟨root, member, subset⟩ := granted token (List.mem_of_getElem? found)
  exact ⟨token, root, found, member, subset⟩

end Witchy.Storage
