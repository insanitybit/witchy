"""Run Grimoire host tests with the installed compiler, engine and Coven launcher."""

import sys

if __name__ == "__main__" and not {"--http-witness", "--process-boundaries"}.intersection(sys.argv):
    from suite_budget import supervise
    supervise()

import argparse
import contextlib
import os
import shutil
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import subprocess
import time
import unittest

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
MODULES = ["local_run", "registry_run", "secret_run", "test_lock_format", "test_operator_record", "test_snapshot_policy", "test_trust_policy", "test_graph", "http_file_server", "test_public_commands", "test_compiler_client", "test_environment", "test_run_grants", "test_native_launch", "test_release_format", "test_release_review", "test_install", "test_tuf_registry"]


def configure_toolchain():
    """Find this checkout's tools while honoring explicit test overrides."""
    checkout = ROOT.parent
    compiler = checkout / "witchyc"
    targets = [compiler / "target"]
    if shared := os.environ.get("CARGO_TARGET_DIR"):
        targets.insert(0, Path(shared).resolve())
    host_target = compiler / "tools" / "wasmtime-host" / "target"
    tools = (
        ("WITCHYC", "witchyc", targets),
        ("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host", [*targets, host_target]),
        ("COVEN", "coven", []),
    )
    for variable, executable, directories in tools:
        if variable in os.environ:
            found = shutil.which(os.environ[variable])
            if not found:
                raise ValueError(f"{variable} does not name an executable: {os.environ[variable]}")
        else:
            candidates = [
                target / profile / executable
                for target in directories
                for profile in ("release", "debug", "suite")
            ]
            if variable == "COVEN":
                candidates.append(checkout / "coven" / "witchy2" / "coven")
            found = next((path for candidate in candidates if (path := shutil.which(candidate))), None)
            found = found or shutil.which(executable)
            if not found:
                raise ValueError(f"cannot find {executable}; build the monorepo tools or set {variable}")
        os.environ[variable] = os.path.abspath(found)


def start_locked_project(fixture):
    """Make the shared locked project in `fixture` in a forked process, holding `.building`
    locked until it is complete (then `complete` is there). Returns the process's id."""
    import fcntl
    held = os.open(fixture / ".building", os.O_CREAT | os.O_WRONLY, 0o600)
    fcntl.flock(held, fcntl.LOCK_EX)
    sys.stdout.flush()
    sys.stderr.flush()
    child = os.fork()
    if child:
        os.close(held)   # the child's descriptor keeps the lock until it ends
        return child
    status = 1
    try:
        from registry_run import make_locked_project
        make_locked_project(fixture)
        (fixture / "complete").touch()
        status = 0
    except BaseException:
        import traceback
        traceback.print_exc()
    finally:
        sys.stdout.flush()
        sys.stderr.flush()
        os._exit(status)


def main():
    cli = argparse.ArgumentParser()
    cli.add_argument("-j", "--jobs", type=int, default=min(os.cpu_count() or 8, 16), help="cases at once (default: one per CPU, at most 16)")
    cli.add_argument("--http-witness", action="store_true", help="run the signed registry restart and clean build")
    cli.add_argument("--process-boundaries", action="store_true", help="run the focused process lifecycle checks")
    args = cli.parse_args()
    try:
        configure_toolchain()
    except ValueError as error:
        cli.error(str(error))
    if args.http_witness:
        return subprocess.call([sys.executable, HERE / "http_registry.py", "-v"])
    jobs = args.jobs
    if jobs < 1:
        cli.error("jobs must be positive")
    sys.path.insert(0, str(HERE))
    cases = []
    for name in MODULES:
        suite = unittest.defaultTestLoader.loadTestsFromName(name)
        cases += [(HERE / f"{name}.py", test.id().split(".", 1)[1]) for group in suite for test in group]
    boundaries = {"test_no_engine_outlives_the_launcher",
                  "test_a_client_gets_the_file_and_the_package_reaches_nothing_beside_it",
                  "test_the_engine_refuses_a_source_file_linked_in_after_grimoires_check",
                  # The launched twins of cases the default suite runs in process (secret_run).
                  "test_a_registry_dependency_cannot_supply_or_widen_a_secret_grant_through_the_launcher",
                  "test_a_file_grant_alias_is_refused_with_only_a_store_through_the_launcher",
                  "test_a_store_no_parameter_takes_is_refused_through_the_launcher"}
    cases = [case for case in cases if (case[1].partition(".")[2] in boundaries) == args.process_boundaries]
    count = len(cases)
    direct_modules = {"test_run_grants", "test_snapshot_policy", "test_graph", "test_trust_policy", "test_lock_format", "test_operator_record", "test_release_format", "test_release_review", "test_tuf_registry"}
    groups = {}
    for path, test in cases:
        direct = path.stem in direct_modules
        groups[path, test.split(".")[0] if direct else test] = None
    cases = list(groups)
    # Coven's module cache for the tests, kept across runs (content-keyed), apart from the user's.
    cache = Path.home() / ".cache" / "coven-tests"
    cache.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.environ.setdefault("XDG_CACHE_HOME", str(cache))
    start = time.perf_counter()
    from registry_run import inline_tools
    # These cross the real process and runtime boundaries. Give them an early
    # start on the first run, before per-case scheduling history exists.
    first = {"test_a_pipe_sources_identity_goes_to_the_engine_and_stays_live", "test_build_and_test_the_declared_executable"}
    def first_run_order(case):
        name = case[1].partition(".")[2]
        if name in first:
            return 0
        if case[0].stem in direct_modules or "before_the_build" in name:
            return 1
        if case[0].stem == "registry_run":
            return 2
        return 3
    cases.sort(key=first_run_order)
    # WITCHY_WASMTIME_RESIDENT: a resident engine (`--features resident`) for the in-process
    # commands' engine runs, through Coven's test support; without it, every run is an executable.
    resident, support = os.environ.get("WITCHY_WASMTIME_RESIDENT"), inline_tools()
    with contextlib.ExitStack() as stack, tempfile.TemporaryDirectory() as fixture, ThreadPoolExecutor(jobs) as pool:
        extra = stack.enter_context(support.resident_host(resident, os.environ.get("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))) if resident and hasattr(support, "resident_host") else {}
        env = dict(os.environ, **extra)
        maker = None
        if not args.process_boundaries:
            # The shared locked project is made in a process of its own while the forking process
            # starts and the first cases run; a case that uses it waits for it (`locked_project`).
            maker = start_locked_project(Path(fixture))
            env["GRIMOIRE_TEST_LOCKED_PROJECT"] = fixture
        if hasattr(support, "run_cases"):
            # Each case in a process of its own, forked from one that has imported the tests,
            # longest first by the last run's times (kept beside the module cache).
            history = cache / "grimoire-durations.json"
            # The forking process also loads, once, what every case would otherwise compile again.
            runs = list(support.run_cases(support.longest_first(cases, history), jobs, dict(env, GRIMOIRE_TEST_PRELOAD="1"),
                                          preload=[ROOT / "grimoire", shutil.which(os.environ.get("COVEN", "coven"))]))
            support.record_durations(runs, history)
        else:
            def run(case):
                ran = subprocess.run([sys.executable, case[0], case[1]], env=env, capture_output=True, text=True)
                return case, ran.returncode, ran.stderr
            runs = list(pool.map(run, cases))
        if maker is not None:
            _, status = os.waitpid(maker, 0)
            if status:
                runs.append(((Path(__file__), "make_locked_project"), os.waitstatus_to_exitcode(status) or 1,
                             "the shared locked project was not made (its error is above)"))
    failed = [(case, output) for case, status, output, *_ in runs if status]
    for (path, test), output in failed:
        print(f"FAIL {Path(path).name} {test}\n{output}")
    print(f"{count} tests, {len(failed)} failing groups of {len(cases)} in {time.perf_counter() - start:.1f} s")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
