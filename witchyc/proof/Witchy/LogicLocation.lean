import Witchy.CoreCallMachine

namespace Witchy.Logic

/-- Resource addresses include service ownership, not only storage addresses.
    A block entry denotes a live block; absence is not an ownable block fact. -/
inductive Location where
  | register (index : Nat)
  | block (handle : Memory.Handle)
  | loan (identity : Memory.LoanId)
  | permissions (handle : Memory.Handle)
  | allocator
  | loanSupply
  | authority
  deriving DecidableEq

/-- Service tuples contain budgets/counters, never unrelated block or loan
    contents. Capability values remain ordinary affine payloads. -/
abbrev Location.Data : Location → Type
  | .register _ => Option Core.Value
  | .block _ => Storage.Block Core.Value
  | .loan _ => Memory.Loan
  | .permissions _ => List Memory.Loan
  | .allocator => Nat × Nat × Nat × Nat
  | .loanSupply => Nat × Nat × Nat
  | .authority => Authority.State

end Witchy.Logic
