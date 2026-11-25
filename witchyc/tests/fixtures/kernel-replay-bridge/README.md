Unqualified source fixtures. Later scheduled check compiles legacy.lean to a
main object (never import it), places an exact owned copy at
.witchy-replay/CurrentBridge.olean and .witchy-replay/FalseBridge.olean beside
these CURRENT source files, and creates their separate snapshot-Module dirs.
CurrentBridge must pass CURRENT-kernel replay with extra/reordered premises;
changed-definition and absent-selector controls require MISS followed by the
unchanged ordinary macro. FalseBridge must refuse the current unused self<self
claim despite an old theorem of the same name. Final standard axiom auditing,
current source/SDK checks and fresh serialization remain mandatory. No old
module environment/initializer or receipt supplies proof authority.
