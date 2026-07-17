# Coven

Coven is Witchy's package-manager and registry prototype. Its purpose is to make
a rune's authority visible and enforceable across publish, release, dependency
resolution, and updates.

## Status

- **Status:** experimental, self-hosted prototype.
- **Trust model:** registry records are signed, package source is retained, and
  clients derive capability footprints from source instead of trusting declared
  metadata alone.
- **Release model:** publish stages a record; human promotion releases it.
- **Update model:** dependency updates that widen runtime or build-time
  capability footprints are blocked until explicitly approved.

## Canonical local lifecycle

The core user story that Coven docs and demos should keep working is:

1. Create a rune and run it locally.
2. Publish it to a local registry.
3. Stage it, promote it, and consume it from another project.
4. Update a dependency whose capability footprint does not widen.
5. Attempt an update whose footprint widens and confirm the client blocks it
   until approval.

The repository-level demo script exercises this local lifecycle:

```sh
./scripts/local-registry-demo.sh
```

The step-by-step registry layout and protocol notes live in
[`../../spec/local-registry.md`](../../spec/local-registry.md). Keep substantial
wire-format or threat-model changes in the spec/RFC process, not in this status
note.

## Related example

[`../../examples/coven_check/README.md`](../../examples/coven_check/README.md) is a focused example
of the same safety rule at manifest-check time: compare declared capabilities
with the source-derived footprint and fail when the manifest under-declares what
the code demands.
