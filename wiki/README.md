# wiki/

A generated, cross-linked synthesis over the authoritative sources in `rfcs/`,
`spec/`, and `external-refs/`. It is a convenience layer, not a source of truth.

Rules:

- Do not make implementation decisions here. Put current behavior in `spec/` and
  proposed changes in `rfcs/`.
- Treat pages here as disposable build output. If they drift, regenerate them from
  authoritative sources instead of hand-editing them.
- Every generated page should name the source commit or input set it was derived
  from so stale pages are easy to identify.
