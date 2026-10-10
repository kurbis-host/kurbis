#!/usr/bin/env python3
"""Static contracts for remote builder identity fencing."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]


class CiBubblewrapContractTests(unittest.TestCase):
    def test_remote_runner_rejects_stale_source_preparer(self):
        runner = (ROOT / "tools/ci_bubblewrap_runner.py").read_text(encoding="utf-8")
        self.assertIn('"KURBIS_EXPECTED_PATCH_FINGERPRINT": header["patch_fingerprint"]', runner)
        self.assertIn("preparer_fingerprint", runner)
        self.assertIn("trusted source preparer fingerprint mismatch", runner)
        self.assertIn("--print-fingerprint", ROOT.joinpath("src/bin/ci-source-preparer.rs").read_text())

    def test_remote_job_header_carries_host_fingerprint(self):
        source = (ROOT / "src/ci/execution/remote_bubblewrap.rs").read_text(encoding="utf-8")
        self.assertIn("patch_fingerprint: String", source)
        self.assertIn("patch_fingerprint: self.patch_fingerprint.clone()", source)
        self.assertIn("patch={patch_fingerprint}", source)


if __name__ == "__main__":
    unittest.main()
