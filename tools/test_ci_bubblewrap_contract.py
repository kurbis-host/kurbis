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

    def test_only_the_code_free_fetch_phase_has_network(self):
        import importlib.util

        spec = importlib.util.spec_from_file_location("runner", ROOT / "tools/ci_bubblewrap_runner.py")
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        # The fetch phase may only unpack archives and download locked
        # dependencies; anything that compiles or runs fetched code must stay
        # in the network-less build phase.
        commands = [
            line.split()[0]
            for line in runner.FETCH_SCRIPT.splitlines()
            if line and not line.startswith(("#", "export ", "set ", "umask "))
        ]
        self.assertEqual(set(commands), {"mkdir", "tar", "cd", "cargo", "printf"})
        self.assertNotIn("cargo build", runner.FETCH_SCRIPT)
        self.assertIn("--offline", runner.BUILD_SCRIPT)
        source = (ROOT / "tools/ci_bubblewrap_runner.py").read_text(encoding="utf-8")
        self.assertIn('command.append("--unshare-net")', source)
        self.assertIn("runner_command(header, workspace, FETCH_SCRIPT, network=True)", source)
        self.assertEqual(source.count("network=True"), 1)

    def test_code_executing_phases_cannot_write_the_shared_cache(self):
        import importlib.util

        spec = importlib.util.spec_from_file_location("runner", ROOT / "tools/ci_bubblewrap_runner.py")
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        # Pull-request code runs in the build phase; the dependency cache it
        # reads is reused by default-branch and release builds.
        self.assertNotIn("sccache", runner.BUILD_SCRIPT)
        self.assertNotIn("mkdir -p /cache", runner.BUILD_SCRIPT)
        source = (ROOT / "tools/ci_bubblewrap_runner.py").read_text(encoding="utf-8")
        self.assertIn('cache_bind = "--bind" if network else "--ro-bind"', source)
        self.assertIn("cd /work\n", runner.FETCH_SCRIPT)

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
