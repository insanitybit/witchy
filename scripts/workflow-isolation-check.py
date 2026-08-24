#!/usr/bin/env python3
"""Enforce the workflow isolation invariant over .github/workflows.

The invariant: a GitHub Actions job either EXECUTES CODE (builds, tests,
repository scripts) or HOLDS WRITE AUTHORITY (publishing, deployment,
attestation, OIDC minting), never both. A compromised dependency's build
script must never run in a process that can reach a deploy credential or
mint an identity token.

Concretely, a job is "privileged" when its effective permissions grant any
`write`. A privileged job must not:

  * check out the repository (`actions/checkout`) — privileged jobs operate
    on artifacts handed to them by unprivileged builders, never on a tree;
  * use a local composite action (`uses: ./...`) — that is repository code;
  * invoke a toolchain (cargo, rustc, node, python, make, docker, ...) or
    execute a workspace-relative path (`./anything`) in a `run:` step.

Marketplace actions pinned by SHA and plain file plumbing (gh, sha256sum,
cmp, curl) are allowed in privileged jobs; zizmor separately audits pins.

Escape hatch: a step carrying `env.WORKFLOW_ISOLATION_EXEMPT: "<reason>"`
skips the run-text scan for that one step. The exemption is for the single
case where the published product IS the publishing client (the coven demo
executes the freshly built `witchy` binary to publish). It never exempts
checkout or local actions, and the reason string is required.

A job with no resolvable `permissions` (neither job- nor workflow-level) is
treated as privileged: the repository default could be write-all, so every
job must opt in explicitly, which zizmor's pedantic persona also demands.
"""

import re
import sys
from pathlib import Path

import yaml

TOOLCHAIN = re.compile(
    r"(?<![\w./=-])(cargo|rustc|rustup|npm|npx|pnpm|yarn|node|deno|bun"
    r"|python3?|pip3?|make|cmake|go|docker|gcc|clang)(?![\w-])"
)
DOTSLASH = re.compile(r"""(^|[\s"'`(|&;])\./""")


def has_write(perms):
    if perms is None:
        return None  # unresolved: caller falls back or fails closed
    if isinstance(perms, str):
        return perms != "read-all" and perms != "{}"
    if isinstance(perms, dict):
        return any(v == "write" for v in perms.values())
    return True


def check_job(path, job_id, job, workflow_perms, problems):
    effective = has_write(job.get("permissions"))
    if effective is None:
        effective = has_write(workflow_perms)
    if effective is None:
        problems.append(
            f"{path}: job '{job_id}' has no explicit permissions at any level; "
            "declare them (a write-all repository default would make this job privileged)"
        )
        return
    if not effective:
        return

    for step in job.get("steps") or []:
        label = step.get("name") or step.get("uses") or "run step"
        uses = step.get("uses") or ""
        if uses.startswith("./"):
            problems.append(
                f"{path}: privileged job '{job_id}' step '{label}' uses local action "
                f"'{uses}' — repository code must not run with write authority"
            )
        if uses.split("@")[0].endswith("actions/checkout"):
            problems.append(
                f"{path}: privileged job '{job_id}' step '{label}' checks out the "
                "repository — privileged jobs must consume artifacts, not a tree"
            )
        run = step.get("run")
        if run is None:
            continue
        if (step.get("env") or {}).get("WORKFLOW_ISOLATION_EXEMPT"):
            continue
        hit = TOOLCHAIN.search(run)
        if hit:
            problems.append(
                f"{path}: privileged job '{job_id}' step '{label}' runs "
                f"'{hit.group(1)}' — build/exec belongs in an unprivileged job"
            )
        if DOTSLASH.search(run):
            problems.append(
                f"{path}: privileged job '{job_id}' step '{label}' executes a "
                "workspace-relative path — repository code must not run with write authority"
            )


def main(argv):
    root = Path(argv[1]) if len(argv) > 1 else Path(".github/workflows")
    files = sorted(list(root.glob("*.yml")) + list(root.glob("*.yaml")))
    if not files:
        print(f"workflow-isolation-check: no workflows under {root}", file=sys.stderr)
        return 1
    problems = []
    for path in files:
        doc = yaml.safe_load(path.read_text())
        jobs = (doc or {}).get("jobs")
        if not isinstance(jobs, dict) or not jobs:
            problems.append(f"{path}: no jobs found (parse problem?)")
            continue
        for job_id, job in jobs.items():
            check_job(path, job_id, job or {}, doc.get("permissions"), problems)
    for p in problems:
        print(f"ISOLATION VIOLATION: {p}", file=sys.stderr)
    if problems:
        return 1
    print(f"workflow-isolation-check: {len(files)} workflows OK")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
