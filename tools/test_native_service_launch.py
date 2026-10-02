#!/usr/bin/env python3
"""Process-level checks for native service launch arguments and defaults."""

from __future__ import annotations

import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = ROOT / "scripts/run-service.sh"
WAITING_LAUNCHER = ROOT / "tools/run_native_waiting.sh"


class NativeServiceLaunchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="kurbis-native-launch-")
        self.workspace = Path(self.temp.name)
        self.root = self.workspace / "deployment"
        self.env_file = self.workspace / "deployment.env"
        self.root.mkdir()
        self.env_file.write_text(
            """DATABASE_URL_NATIVE=postgres://operator@127.0.0.1/kurbis
WORKER_ID=11111111-1111-4111-8111-111111111111
INTERNAL_TOKEN=internal-token
PUMPKIN_GATEWAY_CONTROL_TOKEN=gateway-token
ROOT_DOMAIN=localhost
BEDROCK_PORT=19144
""",
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_native_gateway_defaults_bedrock_port_and_generates_localhost_tls(self) -> None:
        gateway = self.root / "target/debug/gateway"
        gateway.parent.mkdir(parents=True)
        capture = self.workspace / "gateway-environment.txt"
        gateway.write_text(
            f"#!/bin/sh\n/usr/bin/env > {shlex.quote(str(capture))}\n",
            encoding="utf-8",
        )
        gateway.chmod(0o755)

        result = subprocess.run(
            [str(LAUNCHER), str(self.env_file), str(self.root), "native-debug", "gateway"],
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        environment = dict(
            line.split("=", 1)
            for line in capture.read_text(encoding="utf-8").splitlines()
            if "=" in line
        )
        self.assertEqual(environment["BEDROCK_PUBLIC_PORT"], "19144")
        self.assertEqual(environment["WAITING_SIGNAL_ADDRESS"], "127.0.0.1:19134")
        certificate = Path(environment["BEDROCK_TLS_CERT_FILE"])
        private_key = Path(environment["BEDROCK_TLS_KEY_FILE"])
        self.assertTrue(certificate.is_file())
        self.assertTrue(private_key.is_file())
        self.assertEqual(private_key.stat().st_mode & 0o777, 0o600)
        san = subprocess.run(
            ["openssl", "x509", "-in", str(certificate), "-noout", "-ext", "subjectAltName"],
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        self.assertIn("DNS:localhost", san)
        self.assertIn("DNS:*.localhost", san)

    def test_native_panel_forwards_supported_arguments_without_separator(self) -> None:
        web = self.root / "web"
        web.mkdir()
        fake_bin = self.workspace / "bin"
        fake_bin.mkdir()
        arguments = self.workspace / "panel-arguments.txt"
        pnpm = fake_bin / "pnpm"
        pnpm.write_text(
            f"#!/bin/sh\nprintf '%s\\n' \"$@\" > {shlex.quote(str(arguments))}\n",
            encoding="utf-8",
        )
        pnpm.chmod(0o755)

        environment = os.environ.copy()
        environment["PATH"] = f"{fake_bin}:{environment.get('PATH', '/usr/bin:/bin')}"
        result = subprocess.run(
            [str(LAUNCHER), str(self.env_file), str(self.root), "native-debug", "panel"],
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            arguments.read_text(encoding="utf-8").splitlines(),
            ["run", "dev", "--hostname", "0.0.0.0"],
        )

    def test_existing_native_waiting_config_gets_separate_signal_port(self) -> None:
        waiting_root = self.workspace / "native-waiting"
        waiting_root.mkdir()
        (waiting_root / "pumpkin.toml").write_text(
            "[networking.bedrock.nethernet]\n"
            "address = \"127.0.0.1:19133\"\n"
            "signal_address = \"0.0.0.0:19132\"\n",
            encoding="utf-8",
        )
        capture = self.workspace / "waiting-config.txt"
        pumpkin = self.workspace / "pumpkin-probe"
        pumpkin.write_text(
            f"#!/bin/sh\ncat pumpkin.toml > {shlex.quote(str(capture))}\n",
            encoding="utf-8",
        )
        pumpkin.chmod(0o755)
        environment = os.environ.copy()
        environment.update(
            {
                "PUMPKIN_GATEWAY_CONTROL_TOKEN": "gateway-token",
                "PUMPKIN_BINARY": str(pumpkin),
                "NATIVE_WAITING_ROOT": str(waiting_root),
                "WAITING_CONTROL_SOCKET": str(self.workspace / "waiting.sock"),
            }
        )

        result = subprocess.run(
            [str(WAITING_LAUNCHER)],
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        config = capture.read_text(encoding="utf-8")
        self.assertIn('address = "127.0.0.1:19133"', config)
        self.assertIn('signal_address = "127.0.0.1:19134"', config)
        self.assertNotIn('signal_address = "0.0.0.0:19132"', config)


if __name__ == "__main__":
    unittest.main()
