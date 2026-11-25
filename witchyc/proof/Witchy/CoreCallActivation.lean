import Witchy.CoreCallProcedure

namespace Witchy.Core.Call

structure Activation where
  procedure : Nat
  pc : Nat
  base : Nat
  width : Nat
  deriving Repr, DecidableEq

/-- Control frames contain no values, owners, loans or capabilities. All of
    those remain in the single register inventory, even while a caller sleeps. -/
structure Continuation where
  caller : Activation
  destinations : List Nat
  deriving Repr, DecidableEq

end Witchy.Core.Call
