#!/usr/bin/env python3
"""Process-level checks for the API-only worker environment in run-service.sh."""

from __future__ import annotations

import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = ROOT / "scripts/run-service.sh"
WORKER_ID = "11111111-1111-4111-8111-111111111111"


def quote(value: str) -> str:
    return shlex.quote(value)


class WorkerLaunch:
    def __init__(self, mode: str, root_http_opt_in: str | None = None):
        self.temp = tempfile.TemporaryDirectory(prefix="kurbis-worker-env-")
        self.workspace = Path(self.temp.name)
        self.root = self.workspace / "deployment"
        self.mode = mode
        profile = "debug" if mode == "native-debug" else "release"
        self.worker_path = self.root / "target" / profile / "worker"
        self.worker_path.parent.mkdir(parents=True)
        self.bundle = self.workspace / "worker-bundle.env"
        self.capture_path = self.workspace / "worker-environment.txt"
        self.env_file = self.workspace / "deployment.env"
        self.env_file.write_text(self._deployment_env(root_http_opt_in), encoding="utf-8")
        self.write_probe()

    def _deployment_env(self, root_http_opt_in: str | None) -> str:
        values = {
            "WORKER_ID": WORKER_ID,
            "WORKER_BUNDLE_PATH": str(self.bundle),
            "POSTGRES_USER": "operator",
            "POSTGRES_PASSWORD": "operator-database-secret",
            "POSTGRES_DB": "control_plane",
            "DATABASE_PORT": "55432",
            "CONTROL_PLANE_PORT": "18080",
            "DATABASE_URL": "postgres://operator:operator-database-secret@127.0.0.1/control_plane",
            "DATABASE_URL_NATIVE": "postgres://operator:operator-database-secret@127.0.0.1/control_plane",
            "WORKER_API_TOKEN": "operator-worker-api-fallback-sentinel",
            "PUMPKIN_HOST_CONTROL_TOKEN": "operator-host-control-fallback-sentinel",
            "WORKER_NAME": "operator-worker-name-sentinel",
            "WORKER_ADDRESS": "operator-worker-address-sentinel",
            "WORKER_SIGNAL_ADDRESS": "operator-worker-signal-sentinel",
            "WORKER_ROOT": "/operator/worker-root-sentinel",
            "WORKER_STORAGE_MODE": "directory",
            "WORKER_MEMORY_GIB": "8",
            "WORKER_DISK_GIB": "40",
            "WORKER_CPU_MILLIS": "4000",
            "WORKER_PORT_MIN": "20000",
            "WORKER_PORT_MAX": "20040",
            "PUBLIC_BASE_URL": "https://panel.example.test",
            "INTERNAL_TOKEN": "operator-internal-token-sentinel",
            "PUMPKIN_GATEWAY_CONTROL_TOKEN": "operator-gateway-token-sentinel",
            "GOOGLE_OAUTH_CLIENT_SECRET": "operator-google-oauth-sentinel",
            "DISCORD_OAUTH_CLIENT_SECRET": "operator-discord-oauth-sentinel",
            "OBJECT_STORE_TYPE": "s3",
            "OBJECT_STORE_LOCAL_ROOT": "/operator/object-store-sentinel",
            "S3_ACCESS_KEY": "operator-s3-access-sentinel",
            "S3_SECRET_KEY": "operator-s3-secret-sentinel",
            "AWS_ACCESS_KEY_ID": "operator-aws-access-sentinel",
            "AWS_SECRET_ACCESS_KEY": "operator-aws-secret-sentinel",
            "AWS_SESSION_TOKEN": "operator-aws-session-sentinel",
            "CLOUD_CREDENTIALS_JSON": "operator-cloud-credentials-sentinel",
            "WORKER_CAPTURE_PATH": str(self.capture_path),
        }
        if root_http_opt_in is not None:
            values["WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP"] = root_http_opt_in
        return "".join(f"{name}={quote(value)}\n" for name, value in values.items())

    def write_bundle(
        self,
        *,
        include_api_token: bool = True,
        allow_http: str | None = None,
    ) -> None:
        values = {
            "WORKER_ID": WORKER_ID,
            "WORKER_NAME": "browser-worker",
            "WORKER_ADDRESS": "198.51.100.20",
            "WORKER_SIGNAL_ADDRESS": "10.0.0.20",
            "WORKER_MEMORY_GIB": "8",
            "WORKER_DISK_GIB": "40",
            "WORKER_CPU_MILLIS": "4000",
            "WORKER_PORT_MIN": "20000",
            "WORKER_PORT_MAX": "20040",
            "PUMPKIN_HOST_CONTROL_TOKEN": "per-worker-host-control-secret",
            "CONTROL_PLANE_URL": "http://127.0.0.1:18080" if self.mode == "docker" else "http://127.0.0.1:8080",
            "PUBLIC_BASE_URL": "https://panel.example.test",
            "WORKER_RUNTIME": "docker" if self.mode == "docker" else "native",
            "WORKER_STORAGE_MODE": "directory",
            "WORKER_ROOT": str(self.root / "data/worker"),
            "SNAPSHOT_INTERVAL_SECONDS": "300",
            "IDLE_TIMEOUT_SECONDS": "300",
        }
        if self.mode == "docker":
            values["PUMPKIN_IMAGE"] = "melon-pumpkin:host-control"
        else:
            values["PUMPKIN_BINARY"] = str(self.root / "output/native-pumpkin/target/release/pumpkin")
            values["KURBIS_CONTROL_PLUGIN_WASM"] = str(
                self.root / "plugins/kurbis-control/target/wasm32-wasip2/release/kurbis_control.wasm"
            )
        if include_api_token:
            values["WORKER_API_TOKEN"] = "a" * 43
        if allow_http is not None:
            values["WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP"] = allow_http
        self.bundle.write_text(
            "".join(f"export {name}={quote(value)}\n" for name, value in values.items()),
            encoding="utf-8",
        )

    def write_probe(self) -> None:
        self.worker_path.write_text(
            f"#!/bin/sh\n/usr/bin/env > {quote(str(self.capture_path))}\n",
            encoding="utf-8",
        )
        self.worker_path.chmod(0o755)

    def run(self) -> subprocess.CompletedProcess[str]:
        environment = {
            "PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
            "HOME": os.environ.get("HOME", "/"),
            "INTERNAL_TOKEN": "ambient-internal-token-sentinel",
            "PUMPKIN_HOST_CONTROL_TOKEN": "ambient-host-control-fallback-sentinel",
            "GOOGLE_OAUTH_CLIENT_SECRET": "ambient-google-oauth-sentinel",
            "DISCORD_OAUTH_CLIENT_SECRET": "ambient-discord-oauth-sentinel",
            "DATABASE_URL": "ambient-control-database-sentinel",
            "S3_ACCESS_KEY": "ambient-s3-access-sentinel",
            "S3_SECRET_KEY": "ambient-s3-secret-sentinel",
            "AWS_ACCESS_KEY_ID": "ambient-aws-access-sentinel",
            "AWS_SECRET_ACCESS_KEY": "ambient-aws-secret-sentinel",
            "AWS_SESSION_TOKEN": "ambient-aws-session-sentinel",
            "CLOUD_CREDENTIALS_JSON": "ambient-cloud-credentials-sentinel",
        }
        return subprocess.run(
            [str(LAUNCHER), str(self.env_file), str(self.root), self.mode, "worker", str(self.bundle)],
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def child_environment(self) -> dict[str, str]:
        return dict(
            line.split("=", 1)
            for line in self.capture_path.read_text(encoding="utf-8").splitlines()
            if "=" in line
        )

    def close(self) -> None:
        self.temp.cleanup()


class WorkerServiceEnvironmentTests(unittest.TestCase):
    def test_worker_exec_gets_worker_credentials_and_mode_runtime_paths(self) -> None:
        for mode in ("docker", "native-debug", "native-release"):
            with self.subTest(mode=mode):
                launch = WorkerLaunch(mode, root_http_opt_in="false")
                self.addCleanup(launch.close)
                launch.write_bundle()
                result = launch.run()
                self.assertEqual(result.returncode, 0, result.stderr)
                environment = launch.child_environment()

                self.assertEqual(environment["WORKER_ID"], WORKER_ID)
                self.assertEqual(environment["WORKER_API_TOKEN"], "a" * 43)
                self.assertEqual(environment["PUMPKIN_HOST_CONTROL_TOKEN"], "per-worker-host-control-secret")
                self.assertEqual(environment["WORKER_NAME"], "browser-worker")
                self.assertEqual(environment["WORKER_ADDRESS"], "198.51.100.20")
                self.assertEqual(environment["WORKER_SIGNAL_ADDRESS"], "10.0.0.20")
                self.assertEqual(environment["WORKER_RUNTIME"], "docker" if mode == "docker" else "native")
                self.assertEqual(environment["CONTROL_PLANE_URL"], "http://127.0.0.1:18080" if mode == "docker" else "http://127.0.0.1:8080")
                self.assertEqual(environment["PUBLIC_BASE_URL"], "https://panel.example.test")
                self.assertEqual(environment["WORKER_ROOT"], str(launch.root / "data/worker"))
                self.assertEqual(environment["WORKER_STORAGE_MODE"], "directory")
                self.assertIn("PATH", environment)
                self.assertIn("HOME", environment)
                if mode == "native-debug":
                    self.assertEqual(environment["KURBIS_ALLOW_UNSANDBOXED_NATIVE_DEV"], "true")
                else:
                    self.assertNotIn("KURBIS_ALLOW_UNSANDBOXED_NATIVE_DEV", environment)

                if mode == "docker":
                    self.assertEqual(environment["PUMPKIN_IMAGE"], "melon-pumpkin:host-control")
                    self.assertNotIn("PUMPKIN_BINARY", environment)
                    self.assertNotIn("KURBIS_CONTROL_PLUGIN_WASM", environment)
                else:
                    profile = "debug" if mode == "native-debug" else "release"
                    self.assertEqual(
                        environment["PUMPKIN_BINARY"],
                        str(launch.root / f"output/native-pumpkin/target/{profile}/pumpkin"),
                    )
                    self.assertEqual(
                        environment["KURBIS_CONTROL_PLUGIN_WASM"],
                        str(launch.root / f"plugins/kurbis-control/target/wasm32-wasip2/{profile}/kurbis_control.wasm"),
                    )

                for name in (
                    "DATABASE_URL",
                    "DATABASE_URL_NATIVE",
                    "WORKER_DATABASE_URL",
                    "WORKER_PROVISIONING_DATABASE_URL",
                    "INTERNAL_TOKEN",
                    "PUMPKIN_GATEWAY_CONTROL_TOKEN",
                    "GOOGLE_OAUTH_CLIENT_SECRET",
                    "DISCORD_OAUTH_CLIENT_SECRET",
                    "OBJECT_STORE_TYPE",
                    "OBJECT_STORE_LOCAL_ROOT",
                    "S3_ACCESS_KEY",
                    "S3_SECRET_KEY",
                    "AWS_ACCESS_KEY_ID",
                    "AWS_SECRET_ACCESS_KEY",
                    "AWS_SESSION_TOKEN",
                    "CLOUD_CREDENTIALS_JSON",
                    "CONTROL_PLANE_INTERNAL_URL",
                    "WORKER_CAPTURE_PATH",
                    "WORKER_BUNDLE_PATH",
                ):
                    self.assertNotIn(name, environment, f"operator variable {name} reached worker")

                for secret in (
                    "operator-database-secret",
                    "operator-internal-token-sentinel",
                    "operator-gateway-token-sentinel",
                    "operator-host-control-fallback-sentinel",
                    "operator-google-oauth-sentinel",
                    "operator-discord-oauth-sentinel",
                    "operator-s3-access-sentinel",
                    "operator-s3-secret-sentinel",
                    "operator-aws-access-sentinel",
                    "operator-aws-session-sentinel",
                    "operator-cloud-credentials-sentinel",
                    "ambient-internal-token-sentinel",
                    "ambient-host-control-fallback-sentinel",
                    "ambient-google-oauth-sentinel",
                    "ambient-discord-oauth-sentinel",
                    "ambient-control-database-sentinel",
                    "ambient-s3-access-sentinel",
                    "ambient-s3-secret-sentinel",
                    "ambient-aws-access-sentinel",
                    "ambient-aws-session-sentinel",
                    "ambient-cloud-credentials-sentinel",
                ):
                    self.assertFalse(any(secret in value for value in environment.values()), f"operator secret reached worker: {secret}")

    def test_local_http_opt_in_is_not_added_when_unset(self) -> None:
        launch = WorkerLaunch("docker", root_http_opt_in="true")
        self.addCleanup(launch.close)
        launch.write_bundle()
        result = launch.run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP", launch.child_environment())

    def test_explicit_local_http_opt_in_values_are_preserved(self) -> None:
        cases = (
            ("true", "false", "false"),
            ("false", "true", "true"),
        )
        for root_value, bundle_value, expected in cases:
            with self.subTest(root_value=root_value, bundle_value=bundle_value):
                launch = WorkerLaunch("docker", root_http_opt_in=root_value)
                self.addCleanup(launch.close)
                launch.write_bundle(allow_http=bundle_value)
                result = launch.run()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    launch.child_environment().get("WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP"),
                    expected,
                )

    def test_missing_and_malformed_bundles_do_not_execute_worker(self) -> None:
        missing = WorkerLaunch("docker")
        self.addCleanup(missing.close)
        missing_result = missing.run()
        self.assertEqual(missing_result.returncode, 2)
        self.assertIn("worker credential bundle missing", missing_result.stderr)
        self.assertFalse(missing.capture_path.exists())

        malformed = WorkerLaunch("docker")
        self.addCleanup(malformed.close)
        malformed.bundle.write_text("export WORKER_API_TOKEN='unterminated\n", encoding="utf-8")
        malformed_result = malformed.run()
        self.assertNotEqual(malformed_result.returncode, 0)
        self.assertFalse(malformed.capture_path.exists())

    def test_missing_api_credential_fails_before_worker_exec(self) -> None:
        launch = WorkerLaunch("docker")
        self.addCleanup(launch.close)
        launch.write_bundle(include_api_token=False)
        result = launch.run()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("worker credential bundle is missing WORKER_API_TOKEN", result.stderr)
        self.assertFalse(launch.capture_path.exists())

    def test_malformed_api_credential_fails_before_worker_exec(self) -> None:
        launch = WorkerLaunch("docker")
        self.addCleanup(launch.close)
        launch.write_bundle()
        bundle = launch.bundle.read_text(encoding="utf-8").replace(
            "export WORKER_API_TOKEN=a" + "a" * 42,
            "export WORKER_API_TOKEN=short",
        )
        launch.bundle.write_text(bundle, encoding="utf-8")
        result = launch.run()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("malformed WORKER_API_TOKEN", result.stderr)
        self.assertFalse(launch.capture_path.exists())

    def test_launcher_rejects_missing_startup_arguments_before_exec(self) -> None:
        result = subprocess.run(
            [str(LAUNCHER)],
            env={"PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"), "HOME": "/"},
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: run-service.sh", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
