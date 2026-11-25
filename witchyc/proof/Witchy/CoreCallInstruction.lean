import Witchy.CoreCallRegisters
import Witchy.CoreCallReturnLayout

namespace Witchy.Core.Call

/-- Primitive operations retain the existing semantics. Call and return are
    control operations; local registers cannot address suspended activations. -/
inductive Instruction where
  | core (instruction : Core.Instruction)
  | call (procedure : Nat) (arguments destinations : List Nat)
  | ret
  | retLayout (layout : ReturnLayout)
  deriving Repr, DecidableEq

def relocateRegister (width base index : Nat) : Option Nat :=
  if index < width then some (base + index) else none

/-- Checked relocation changes register operands only. Branch targets remain
    procedure-local instruction positions. An invalid local operand is refused. -/
def relocate (width base : Nat) : Core.Instruction → Option Core.Instruction
  | .packMove dst sources widths => return .packMove (← relocateRegister width base dst) (← sources.mapM (relocateRegister width base)) widths
  | .unpackMove destinations src widths => return .unpackMove (← destinations.mapM (relocateRegister width base)) (← relocateRegister width base src) widths
  | .constant dst value => return .constant (← relocateRegister width base dst) value
  | .boolean dst value => return .boolean (← relocateRegister width base dst) value
  | .copy dst src => return .copy (← relocateRegister width base dst) (← relocateRegister width base src)
  | .move dst src => return .move (← relocateRegister width base dst) (← relocateRegister width base src)
  | .clear src => return .clear (← relocateRegister width base src)
  | .calculate dst left right op => return .calculate (← relocateRegister width base dst) (← relocateRegister width base left) (← relocateRegister width base right) op
  | .allocate dst count => return .allocate (← relocateRegister width base dst) (← relocateRegister width base count)
  | .tryAllocate tag dst error count => return .tryAllocate (← relocateRegister width base tag) (← relocateRegister width base dst) (← relocateRegister width base error) (← relocateRegister width base count)
  | .borrow dst src exclusive => return .borrow (← relocateRegister width base dst) (← relocateRegister width base src) exclusive
  | .endLoan src => return .endLoan (← relocateRegister width base src)
  | .read dst src index => return .read (← relocateRegister width base dst) (← relocateRegister width base src) (← relocateRegister width base index)
  | .readUnchecked dst src index => return .readUnchecked (← relocateRegister width base dst) (← relocateRegister width base src) (← relocateRegister width base index)
  | .writeEmpty dst index value => return .writeEmpty (← relocateRegister width base dst) (← relocateRegister width base index) (← relocateRegister width base value)
  | .take dst src index => return .take (← relocateRegister width base dst) (← relocateRegister width base src) (← relocateRegister width base index)
  | .storeMove dst index src => return .storeMove (← relocateRegister width base dst) (← relocateRegister width base index) (← relocateRegister width base src)
  | .takeMove dst src index => return .takeMove (← relocateRegister width base dst) (← relocateRegister width base src) (← relocateRegister width base index)
  | .release src => return .release (← relocateRegister width base src)
  | .narrow dst src rights => return .narrow (← relocateRegister width base dst) (← relocateRegister width base src) rights
  | .emit capability value => return .emit (← relocateRegister width base capability) (← relocateRegister width base value)
  | .emitByte capability value => return .emitByte (← relocateRegister width base capability) (← relocateRegister width base value)
  | .utf8Check dst src length => return .utf8Check (← relocateRegister width base dst) (← relocateRegister width base src) (← relocateRegister width base length)
  | .jump target => some (.jump target)
  | .branchFalse condition target => return .branchFalse (← relocateRegister width base condition) target
  | .halt => some .halt
  | .loanRoom dst count => return .loanRoom (← relocateRegister width base dst) (← relocateRegister width base count)

end Witchy.Core.Call
