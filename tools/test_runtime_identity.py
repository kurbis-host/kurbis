"""Regression tests for the non-root runtime identity used by OpenSSH CI probes."""
from __future__ import annotations

import os
import re
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DOCKERFILE = ROOT / "runtime/Dockerfile"
PROBE = ROOT / "tools/verify_runtime_identity.sh"


class RuntimeIdentityTests(unittest.TestCase):
    def test_dockerfile_creates_resolvable_uid_before_numeric_user(self):
        source = DOCKERFILE.read_text(encoding="utf-8")
        self.assertRegex(source, r"groupadd\s+--gid\s+65532\s+kurbis")
        self.assertRegex(
            source,
            r"useradd\s+--uid\s+65532\s+--gid\s+65532\s+"
            r"--home-dir\s+/home/kurbis\s+--create-home",
        )
        self.assertIn('test "$(getent passwd 65532 | cut -d: -f1)" = kurbis', source)
        self.assertIn('test "$(getent group 65532 | cut -d: -f1)" = kurbis', source)
        self.assertIn("USER 65532:65532", source)

    def test_identity_probe_checks_ssh_initialization_and_meminfo(self):
        source = PROBE.read_text(encoding="utf-8")
        for expected in (
            "--user 65532:65532",
            'test "$(id -un)" = kurbis',
            'getent passwd 65532',
            'getent group 65532',
            "ssh -G",
            "ci-builder@builder.invalid",
            "/proc/meminfo",
        ):
            self.assertIn(expected, source)

    @unittest.skipUnless(
        os.environ.get("KURBIS_RUNTIME_UID_TEST_IMAGE"),
        "set KURBIS_RUNTIME_UID_TEST_IMAGE to run the image integration probe",
    )
    def test_runtime_image_identity_and_ssh_probe(self):
        image = os.environ["KURBIS_RUNTIME_UID_TEST_IMAGE"]
        result = subprocess.run(
            [str(PROBE), image],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr or result.stdout)
