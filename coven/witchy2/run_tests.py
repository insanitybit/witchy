"""Run Coven host and Witchy package tests."""

import sys

if __name__ == "__main__" and not {"--engine-boundaries", "--package-units"}.intersection(sys.argv):
    from suite_budget import supervise
    supervise()

import argparse
import contextlib
import os
import subprocess
import tempfile
from pathlib import Path
import time
import unittest

HERE = Path(__file__).resolve().parent


def main():
    cli = argparse.ArgumentParser()
    cli.add_argument("-j", "--jobs", type=int, default=9, help="maximum workers per test group")
    group = cli.add_mutually_exclusive_group()
    group.add_argument("--engine-boundaries", action="store_true", help="run the focused engine lifecycle checks")
    group.add_argument("--all", action="store_true", help="run both host test groups under one time limit")
    group.add_argument("--package-units", action="store_true", help="run the Witchy package suites")
    cli.add_argument("--packages", action="store_true", help="include the Witchy package suites in --all")
    args = cli.parse_args()
    if args.jobs < 1:
        cli.error("jobs must be positive")
    if args.packages and not args.all:
        cli.error("--packages requires --all")
    if args.package_units:
        statuses = [subprocess.run(["grimoire", "test", path], cwd=HERE.parent).returncode
                    for path in ("witchy2", "witchy2/tuf", "witchy2/mirror", "witchy2/tls13")]
        return int(any(statuses))
    if not args.all:
        return run_group(args)
    runner = [sys.executable, str(Path(__file__).resolve())]
    with contextlib.ExitStack() as stack:
        boundaries = stack.enter_context(subprocess.Popen([*runner, "--engine-boundaries", "--jobs", str(args.jobs)]))
        packages = stack.enter_context(subprocess.Popen([*runner, "--package-units"])) if args.packages else None
        status = run_group(args)
    return status or int(boundaries.returncode != 0 or (packages is not None and packages.returncode != 0))


def run_group(args):
    sys.path.insert(0, str(HERE))
    cases = []
    modules = ("test_registry",) if args.engine_boundaries else (
        "test_registry", "test_run_limits", "test_index_policy", "test_http_store", "test_coven_web", "test_coven_approval",
        "test_coven_tuf_publish", "test_coven_ci", "test_compiler_client", "test_release_format", "test_release_registry",
    )
    for name in modules:
        suite = unittest.defaultTestLoader.loadTestsFromName(name)
        cases.extend((HERE / f"{name}.py", test.id().split(".", 1)[1]) for group in suite for test in group)
    boundaries = {"test_the_operator_seed_reaches_only_the_engine", "test_each_command_verifies_in_the_engine_run_that_acts",
                  "test_a_batch_above_the_hosts_default_input_is_verified_in_the_run",
                  "test_the_operator_signs_in_the_engine_and_a_client_verifies_with_its_key_id"}
    cases = [case for case in cases if (case[1].partition(".")[2] in boundaries) == args.engine_boundaries]
    count = len(cases)
    cases = list(dict.fromkeys((path, test.split(".")[0] if path.stem in ("test_index_policy", "test_release_format") else test)
                               for path, test in cases))
    first = {"test_a_release_is_signed_once_and_verified_on_fetch", "test_a_refused_publish_does_not_become_the_owners_next_base"}
    cases.sort(key=lambda case: (case[1].partition(".")[2] not in first, case[0].stem == "test_registry"))
    # Coven's module cache for the tests, kept across runs (entries are keyed by the compiler's and
    # the sources' digests, so a stale one is never used), apart from the user's own.
    cache = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache" / "coven-tests")
    cache.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.environ["XDG_CACHE_HOME"] = str(cache)
    start = time.perf_counter()
    import cli_test_support
    # WITCHY_WASMTIME_RESIDENT: a resident engine (`--features resident`) for the in-process
    # commands' engine runs (cli_test_support.resident_host); without it, every run is an executable.
    resident = None if args.engine_boundaries else os.environ.get("WITCHY_WASMTIME_RESIDENT")
    with contextlib.ExitStack() as stack, tempfile.TemporaryDirectory() as fixture:
        extra = stack.enter_context(cli_test_support.resident_host(resident, os.environ.get("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))) if resident else {}
        env = dict(os.environ, COVEN_TEST_INITIAL_RELEASE=fixture, **extra)
        if not args.engine_boundaries:
            env["COVEN_TEST_STAGING_TEMPLATE"] = str(Path(fixture) / "staging")
        # Each case in a process of its own, forked from one that has imported the tests.
        # Longest first, by the last run's times (kept beside the module cache).
        if args.engine_boundaries:
            history = cache / "coven-boundary-durations.json"
        elif resident:
            history = cache / "coven-resident-durations.json"
        else:
            history = cache / "coven-durations.json"
        launchers = ("coven_candidate_stage", "coven_ci_identity", "coven_ci_http", "coven_ci_store",
                     "coven_review_commit", "coven_policy_rotation", "coven_target",
                     "coven_tuf_extract", "coven_tuf_sign", "coven_tuf_publish", "coven_tuf_select",
                     "coven_staging_schema", "coven_recovery_workers")
        runs = list(cli_test_support.run_cases(cli_test_support.longest_first(cases, history), args.jobs, env,
                                              preload=[HERE / "coven"], resident_launchers=launchers))
        cli_test_support.record_durations(runs, history)
    failed = [(case, output) for case, status, output, _ in runs if status]
    for case, output in failed:
        print(f"FAIL {case}\n{output}")
    print(f"{count} tests, {len(failed)} failing groups of {len(cases)} in {time.perf_counter() - start:.1f} s")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
