import Init

namespace Witchy.Authority

inductive Right where
  | consoleRead | consoleWrite
  deriving Repr, DecidableEq

abbrev Token := List Right

structure State where
  tokens : List Token
  output : List Int
  /-- Raw bytes in emission order. Each is tagged with the number of numeric
      outputs that preceded it, so the one interleaved stream is recoverable
      while every numeric-trace statement keeps its meaning. -/
  bytes : List (Nat × UInt8)
  deriving Repr, DecidableEq

inductive Command where
  | narrow (token : Nat) (requested : Token)
  | emit (token : Nat) (value : Int)
  | emitByte (token : Nat) (value : UInt8)
  deriving Repr, DecidableEq

inductive Error where
  | unknownToken | denied
  deriving Repr, DecidableEq

inductive Result where
  | ok (created : Option Nat)
  | error (reason : Error)
  deriving Repr, DecidableEq

def initial (roots : List Token) : State := ⟨roots, [], []⟩

/-- Every delegated right descends from some host root. This initial model
    proves rights attenuation, not endpoint policies or concrete token opacity. -/
def Valid (roots : List Token) (s : State) : Prop :=
  ∀ token ∈ s.tokens, ∃ root ∈ roots, token ⊆ root

def step (s : State) : Command → State × Result
  | .narrow index requested =>
    match s.tokens[index]? with
    | none => (s, .error .unknownToken)
    | some parent =>
      if requested ⊆ parent then
        ({ s with tokens := s.tokens ++ [requested] }, .ok (some s.tokens.length))
      else (s, .error .denied)
  | .emit index value =>
    match s.tokens[index]? with
    | none => (s, .error .unknownToken)
    | some token =>
      if .consoleWrite ∈ token then
        ({ s with output := s.output ++ [value] }, .ok none)
      else (s, .error .denied)
  | .emitByte index value =>
    match s.tokens[index]? with
    | none => (s, .error .unknownToken)
    | some token =>
      if .consoleWrite ∈ token then
        ({ s with bytes := s.bytes ++ [(s.output.length, value)] }, .ok none)
      else (s, .error .denied)

end Witchy.Authority
