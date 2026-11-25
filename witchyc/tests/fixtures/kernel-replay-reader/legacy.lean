-- Deliberately legacy (no `module` header): all values live in the main .olean.
namespace RawReaderOld

initialize oldState : IO.Ref Nat ← do
  if let some path ← IO.getEnv "WITCHY_READER_SENTINEL" then
    IO.FS.writeFile path "OLD INITIALIZER EXECUTED"
  IO.mkRef 17

private theorem auxiliary (x y : Nat) (h : x = y) : x = y := h

theorem original (x y : Nat) (h : x = y) : x = y := auxiliary x y h

def localMark : Nat := 0

theorem mark_zero : localMark = 0 := rfl

opaque unsupported : Nat := 0

theorem universe_polymorphic {α : Type u} (x : α) : x = x := rfl

end RawReaderOld
