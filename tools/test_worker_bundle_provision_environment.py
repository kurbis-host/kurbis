#!/usr/bin/env python3
"""Ensure worker provisioning passes deployment metadata through its clean environment."""

from __future__ import annotations

import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
START = ROOT / "start.sh"


class WorkerBundleProvisionEnvironmentTests(unittest.TestCase):
    def test_provisioner_receives_worker_metadata_from_deployment_environment(self) -> None:
        start_script = START.read_text(encoding="utf-8")
        functions_start = start_script.index("worker_bundle_path() {")
        functions_end = start_script.index("\nensure_worker_bundle() {", functions_start)
        functions = start_script[functions_start:functions_end]

        with tempfile.TemporaryDirectory(prefix="kurbis-worker-provision-") as temporary:
            workspace = Path(temporary)
            fake_bin = workspace / "bin"
            fake_bin.mkdir()
            captured_environment = workspace / "cargo-environment.txt"
            fake_cargo = fake_bin / "cargo"
            fake_cargo.write_text(
                "#!/bin/sh\n"
                f"/usr/bin/env > {shlex.quote(str(captured_environment))}\n",
                encoding="utf-8",
            )
            fake_cargo.chmod(0o755)

            environment = {
                "PATH": f"{fake_bin}:{os.environ.get('PATH', '/usr/bin:/bin')}",
                "HOME": str(workspace),
                "ROOT": str(workspace),
                "WORKER_ID": "11111111-1111-4111-8111-111111111111",
                "DATABASE_URL_NATIVE": "postgres://worker-provision-test",
                "WORKER_NAME": "configured-worker-name",
                "WORKER_ADDRESS": "198.51.100.20",
                "WORKER_SIGNAL_ADDRESS": "10.0.0.20",
                "PUBLIC_BASE_URL": "https://panel.example.test",
                "WORKER_MEMORY_GIB": "12",
                "WORKER_DISK_GIB": "80",
                "WORKER_CPU_MILLIS": "6000",
                "WORKER_PORT_MIN": "21000",
                "WORKER_PORT_MAX": "22000",
            }
            result = subprocess.run(
                [
                    "bash",
                    "-c",
                    f'{functions}\nprovision_worker_bundle native "$WORKER_ID" false debug',
                ],
                cwd=ROOT,
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            provisioned_environment = dict(
                line.split("=", 1)
                for line in captured_environment.read_text(encoding="utf-8").splitlines()
                if "=" in line
            )
            for name in (
                "WORKER_NAME",
                "WORKER_ADDRESS",
                "WORKER_SIGNAL_ADDRESS",
                "WORKER_MEMORY_GIB",
                "WORKER_DISK_GIB",
                "WORKER_CPU_MILLIS",
                "WORKER_PORT_MIN",
                "WORKER_PORT_MAX",
            ):
                with self.subTest(name=name):
                    self.assertEqual(provisioned_environment[name], environment[name])


if __name__ == "__main__":
    unittest.main()
