# BUG-225: PM duplicates Coven protocol-envelope builders

Status: FIXED

Fixed by: PM now calls the canonical `coven_proto` request-envelope builders.

Summary:
- `projects/pm/src/pm.witchy` imports `coven_proto` and uses
  `coven_proto.publish_body`, `coven_proto.promote_body`, and
  `coven_proto.yank_body` for the PM publish/promote/yank client path.
- The local PM copies of the publish/promote/yank JSON-envelope builders were
  deleted, including the duplicate `id_token` field handling.
- The embedded `witchy pm` loader now bundles the small Coven protocol module
  set (`coven_proto`, `coven_json`, `coven_validate`) for the PM front-end.
- Legacy e2e tests that executed `projects/pm/src/pm.witchy` directly now use
  the compiled embedded `witchy pm` command, matching the release path.

Validation:
- `cargo run --quiet -- pm help`
- `cargo run --quiet -- pm check projects/pm`
- `cargo run --quiet -- pm tree projects/pm`
- `cargo run --quiet -- check projects/coven/src/coven_proto.witchy`
- `cargo test pm_passes_its_own_check -- --nocapture`
- `cargo test pm_ -- --nocapture`
- `cargo test witchy_pm_add_resolves_and_fetches_from_coven -- --nocapture`
- `cargo test witchy_coven_yank_excludes_from_resolution -- --nocapture`
- `cargo test witchy_pm_add_resolves_transitive_dependencies -- --nocapture`
- `cargo test witchy_pm_embedded_frontend -- --nocapture`
- `cargo test witchy_pm_publishes_with_trusted_token -- --nocapture`
- `cargo check --workspace`
