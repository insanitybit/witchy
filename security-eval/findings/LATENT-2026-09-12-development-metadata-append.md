# Dormant development packaging appended metadata after compilation

Classification: latent admission-boundary hazard, not a demonstrated live bypass.

`src/commands/compile.rs::compile_checked_to_development_wasm` appended a
`witchy.launch` custom section after receiving a compiled development module.
The ordinary compilation helper already returns exact admitted bytes unchanged.
The development lowerer currently refuses before this append is reached.

Re-enabling development/Glamour compilation by returning an admitted module
through the old helper would modify its bytes after admission. A downstream
receipt check could then reject the modified artifact; omitting that check
would instead lose exact-artifact correspondence. Neither is an acceptable
way to enable the workflow.

The wrapper now returns the compiler output unchanged. This change does not
enable development compilation, authenticate its legacy cache, or establish
Glamour coverage. The eventual source-backed development entry must construct
and authenticate all launch metadata before admission, preserve the admitted
artifact through packaging and launch, and exercise that path end to end.

Evidence in this slice is static inspection of the ordinary, cached, and
development compilation helpers. No new executable development-path evidence
is claimed; focused compilation is deferred to the next coherent integration
build.
