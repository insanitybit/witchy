# Sourced by scripts/bin/cargo, scripts/check.sh and the merge queue's
# prewarm: resolve features as `--workspace` does, whatever `-p` selection a
# cargo run makes.
#
# Cargo otherwise unifies features over the SELECTED packages only. On
# 2026-09-23 witchy-wir resolved to six different feature sets across
# -p witchy-wir / -p witchy-lower / -p witchy-runtime / --workspace, and a
# scoped gate (-p witchy-lower -p witchy-types ...) lacked
# witchy-cap-model/test-env-staging, which changed witchy-types' hash and
# everything above it. Every distinct set is a separate build of those crates
# (witchy-wir at opt-level 2) in the same target dir, and a seed built by one
# selection is useless to another. With this, agents, the scoped gate and the
# prewarm all compile one set.
#
# The setting is still unstable in cargo. The channel override is read by
# cargo alone -- unlike RUSTC_BOOTSTRAP, which build scripts such as
# proc-macro2's probe, forking the whole dependency tree. If a future cargo
# drops the override, it only warns that it is ignoring
# `resolver.feature-unification`. WITCHY_FEATURE_UNIFICATION=off restores
# per-selection resolution.
if [ "${WITCHY_FEATURE_UNIFICATION:-workspace}" != off ]; then
    export __CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS=nightly
    export CARGO_UNSTABLE_FEATURE_UNIFICATION=true
    export CARGO_RESOLVER_FEATURE_UNIFICATION=workspace
fi
