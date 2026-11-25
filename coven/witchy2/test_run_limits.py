"""Edit loop for batch-sized input grants. The registry suite fetches a real >64 KiB index."""
from pathlib import Path
import unittest
from cli_test_support import fresh


class RunLimits(unittest.TestCase):
    def test_input_is_sized_for_the_entire_batch(self):
        run_limits = fresh(Path(__file__).with_name("coven")).run_limits
        for batch, size in [([], 1), ([bytes(65536)], 65536), ([bytes(50000), bytes(30000)], 80000)]:
            with self.subTest(size=size):
                flags = run_limits(batch)
                self.assertEqual([f for f in flags if f.startswith("--input-bytes=")], [f"--input-bytes={size}"])


if __name__ == "__main__":
    unittest.main()
