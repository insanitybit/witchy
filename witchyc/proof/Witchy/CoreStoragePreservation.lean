import Witchy.CoreAllocatedInvariant
import Witchy.CoreAuthorityInvariant
import Witchy.CoreBorrowInvariant
import Witchy.CoreRegisterInvariant
import Witchy.CoreReleaseInvariant
import Witchy.CoreScalarHeapInvariant
import Witchy.ScalarAggregateStorage
import Witchy.CoreTakeInvariant

namespace Witchy.Core

/-- Every accepted resource-valued instruction preserves the existing storage
    invariant. This is preservation, not a claim that arbitrary code is fault
    free or that source representation invariants are automatically established. -/
theorem step_storage_invariant (s next : StateOf Value) (instruction : Instruction)
    (known : StorageStateInvariant s) (valid : Memory.LoansValid s.memory)
    (accepted : step s instruction = .ok next) : StorageStateInvariant next := by
  cases instruction with
  | packMove dst sources widths => exact pack_storage s next dst sources widths known accepted
  | unpackMove destinations src widths => exact unpack_storage s next destinations src widths known accepted
  | move dst source => exact move_preserves_storage s next dst source known accepted
  | clear source => exact clear_preserves_storage s next source known accepted
  | allocate dst count => exact allocate_preserves_storage s next dst count known accepted
  | tryAllocate tag dst error count => exact try_allocate_preserves_storage s next tag dst error count known accepted
  | borrow dst source exclusive => exact borrow_preserves_storage s next dst source exclusive known valid accepted
  | endLoan source => exact end_loan_preserves_storage s next source known valid accepted
  | writeEmpty dst index source => exact write_empty_preserves_storage s next dst index source known accepted
  | take dst src index => exact take_preserves_storage s next dst src index known accepted
  | storeMove dst index source => exact store_move_preserves_storage s next dst index source known accepted
  | takeMove dst src index => exact take_move_preserves_storage s next dst src index known accepted
  | release source => exact release_preserves_storage s next source known accepted
  | narrow dst source rights => exact narrow_preserves_storage s next dst source rights known accepted
  | emit source value => exact emit_preserves_storage s next source value known accepted
  | emitByte source value => exact emit_byte_preserves_storage s next source value known accepted
  | _ =>
    simp only [step, Bind.bind, fromMemory] at accepted
    all_goals repeat' first | split at accepted | cases accepted
    all_goals exact putCopy_storage_invariant s next _ _ known accepted

end Witchy.Core
