#!/usr/bin/env python3
"""Static contracts for remote builder identity fencing."""
from pathlib import Path
import subprocess
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

    def test_remote_identity_probe_keeps_shell_script_in_one_ssh_argument(self):
        source = (ROOT / "src/ci/execution/remote_bubblewrap.rs").read_text(encoding="utf-8")
        start = source.index("async fn probe_remote_identity")
        end = source.index("fn ssh_base_command", start)
        body = source[start:end]

        # ssh concatenates every argument after the target before handing the
        # command to the remote shell.  Splitting `sh -c` and its script into
        # separate ssh arguments makes the script's format string become `$0`
        # and reproduces dash's `printf: usage` failure seen in production.
        expected = '.arg("printf \'%s\\n%s\\n\' \\"$(id -u)\\" \\"$(id -g)\\"")'
        self.assertIn(expected, body)
        self.assertNotIn('.args(["sh", "-c"', body)

        probe = "printf '%s\\n%s\\n' \"$(id -u)\" \"$(id -g)\""
        direct = subprocess.run(
            ["sh", "-c", probe],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(direct.returncode, 0, direct.stderr)
        self.assertEqual(len(direct.stdout.splitlines()), 2)
        self.assertTrue(all(value.isdecimal() for value in direct.stdout.splitlines()))

        legacy = subprocess.run(
            ["sh", "-c", f"sh -c {probe}"],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(legacy.returncode, 0)
        self.assertIn("printf", legacy.stderr)


if __name__ == "__main__":
    unittest.main()
