#!/usr/bin/env python3
"""Focused measurement-integrity tests for RFC-0146 Track 0."""

import importlib.util
import os
import pathlib
import subprocess
import tempfile
import unittest


BENCH_DIR = pathlib.Path(__file__).resolve().parent
ROOT_DIR = BENCH_DIR.parent
SPEC = importlib.util.spec_from_file_location("benchmark_summarize", BENCH_DIR / "summarize.py")
SUMMARIZE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUMMARIZE)


class SummarizeTests(unittest.TestCase):
    def test_ratio_and_interval_use_the_same_ratio_of_medians_estimand(self):
        point, interval = SUMMARIZE.paired_ratio_summary([1, 100], [1, 10])
        self.assertAlmostEqual(point, 50.5 / 5.5)
        self.assertLessEqual(interval[0], point)
        self.assertGreaterEqual(interval[1], point)

    def test_kernel_samples_reject_short_malformed_and_nonpositive_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            build = pathlib.Path(directory)
            path = build / "fib.witchy.kernel.tsv"
            path.write_text("witchy\t10\n")
            with self.assertRaisesRegex(ValueError, "expected 2 samples"):
                SUMMARIZE.raw_kernel_samples(build, "fib", "witchy", 2)
            path.write_text("other\t10\n")
            with self.assertRaisesRegex(ValueError, "malformed"):
                SUMMARIZE.raw_kernel_samples(build, "fib", "witchy", 1)
            path.write_text("witchy\t0\n")
            with self.assertRaisesRegex(ValueError, "positive"):
                SUMMARIZE.raw_kernel_samples(build, "fib", "witchy", 1)

    def test_missing_results_cannot_be_reported_as_a_match(self):
        with tempfile.TemporaryDirectory() as directory:
            build = pathlib.Path(directory)
            (build / "fib.witchy.kernel.tsv").write_text("witchy\t10\n")
            (build / "fib.go.kernel.tsv").write_text("go\t10\n")
            with self.assertRaisesRegex(ValueError, "missing result file"):
                SUMMARIZE.benchmark_record(build, "fib", "quick", 1)


class DriverTests(unittest.TestCase):
    def test_build_input_census_covers_embedded_non_rust_sources(self):
        result = subprocess.run(
            [str(ROOT_DIR / "bench.sh"), "--print-build-inputs"],
            cwd=ROOT_DIR,
            check=True,
            capture_output=True,
            text=True,
        )
        inputs = set(result.stdout.splitlines())
        self.assertIn("build.rs", inputs)
        self.assertIn("menus/native.toml", inputs)
        self.assertIn("projects/grimoire/src/grimoire.witchy", inputs)
        self.assertIn("web/witchy-runtime/witchy-runtime.mjs", inputs)

    def test_explicit_stale_binary_is_rejected_before_timing(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "witchy"
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            os.utime(binary, (1, 1))
            environment = dict(os.environ, WITCHY=str(binary))
            result = subprocess.run(
                [str(ROOT_DIR / "bench.sh"), "--quick", "fib"],
                cwd=ROOT_DIR,
                env=environment,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("explicitly supplied WITCHY is stale", result.stderr)
            self.assertNotIn("Building release binary", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
