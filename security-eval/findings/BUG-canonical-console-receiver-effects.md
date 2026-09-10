# Canonical Console calls discarded receiver evaluation

`lower_console_print` checked the receiver's source type but did not lower the
receiver expression. For `receiver(console).print(message(console))`, effects
inside `receiver` were lost. A captured Console value also never reached the
host: the old print ABI carried only the message's pointer and length.

The canonical lowering now evaluates the receiver before the message and passes
the resulting opaque value through `witchy.console_print`. Its authenticated
contract requires a Console lane and a checked read slice. The host rejects
null, non-Console externrefs, and missing output grants. Legacy `print` keeps its
ABI for frozen consumers; canonical source projection does not select it.

Focused regression: `canonical_console_receiver_precedes_message_three_legs`.
The independently specified output is `receiver`, `message`, then `payload`.
