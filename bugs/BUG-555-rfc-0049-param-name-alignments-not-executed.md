# BUG-555: RFC-0049's param-name alignments were never executed

**Severity:** LOW
**Status:** FIXED
**Verified:** 2026-07-08 SOURCE on master 0352504
**Component:** `std/option.witchy`, `std/list.witchy`, RFC-0049 naming consistency, generated `spec/stdlib.md`
**Found:** 2026-07-07, quality audit (scratch/audit-2026-07-07-quality/REPORT.md, part of F5)

## Symptom

Current source and generated docs now reflect both RFC-0049 doc-only
alignments: `option.filter` uses `keep`, and `list.get` uses `index`, matching
`list.at`.

## Historical Symptom

RFC-0049 (status: implemented, frozen) records two "doc-only alignments"
(rfcs/0049-naming-lexicon.md:108-113) that are not reflected in std:

1. `option.filter`'s callback param is still `pred` (`std/option.witchy:44`),
   not `keep` — the RFC aligned it with `list.filter`/`iter.filter`/
   `dict.filter`, which all use `keep`.
2. `list.at` uses `index` while `list.get` uses `i` (`std/list.witchy:215`);
   the RFC aligned both on `index`.

Same class as BUG-141 (the RFC-0049 `random` → `prng` rename that also never
landed): the rename cut shipped incompletely, and the frozen RFC now overstates
what was executed.

## Repro

```sh
grep -n 'pred' std/option.witchy        # line 43-46: doc + signature use `pred`
grep -n 'i: Int' std/list.witchy        # list.get(xs, i) vs list.at(xs, index)
```

## Root cause

The RFC-0049 rename cut executed the function renames but skipped the
"doc-only alignments" subsection. Param names are API surface in the generated
reference (`spec/stdlib.md` renders signatures), so the drift is user-visible.

## Fix

Rename `pred` → `keep` in `std/option.witchy:44` (doc-comment line 43 too);
rename `list.get`'s `i` → `index` in `std/list.witchy:215` (and its
doc-comment). Regenerate the reference: `witchy doc std/*.witchy >
spec/stdlib.md` (never hand-edit; `stdlib_docs_are_current` gates it). No
behavior change — witchy has no named-argument call syntax, so call sites are
unaffected. Gate: `./scripts/check.sh --fast`.
