# BUG-566: Docs-project changes skipped the focused e2e shard

Status: FIXED
Severity: MEDIUM
Component: `scripts/test-for-paths.sh`, agent validation

## Summary

`test-for-paths.sh` mapped Witchy changes under PM, Coven, Coven-web, and
glamour to `./scripts/check.sh --e2e`, but omitted `projects/docs`. A behavioral
change to the Witchy docs app was therefore reported as prose-only even though
its integration coverage lives in `tests/glamour_dom.rs` and the e2e shard.

This could send a docs-app branch to the full merge gate without the focused
preflight that the concurrency protocol promises.

## Resolution

`projects/docs/*` now shares the project e2e mapping. Running
`./scripts/test-for-paths.sh` with a docs-app source diff reports
`./scripts/check.sh --e2e`.
