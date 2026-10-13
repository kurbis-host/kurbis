#!/usr/bin/env python3
"""Behavior of the production release scripts that needs no host or network."""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = load("release")
host = load("release_host")
migration = load("migration_manifest")
REVISION = "a" * 40


def service(**changes):
    return {"status": "running", "health": "healthy", "revision": REVISION, "image": "sha256:app", "name": "c", **changes}


class ReleaseTests(unittest.TestCase):
    def test_migration_manifest_matches_every_sql_file_and_has_gapless_versions(self):
        self.assertEqual(len(migration.validate_manifest(ROOT / "migrations", ROOT / "migrations/MANIFEST.json")), 64)
        versions = [record["version"] for record in migration.migration_records(ROOT / "migrations")]
        self.assertEqual(versions, list(range(1, len(versions) + 1)))

    def test_environment_patch_replaces_keys_and_keeps_everything_else(self):
        original = "# comment\nBUILD_REVISION=old\nSECRET='kept value'\nBUILD_REVISION=duplicate\n"
        patched = release.patch_environment(original, {"BUILD_REVISION": REVISION})
        self.assertEqual(patched, f"# comment\nSECRET='kept value'\nBUILD_REVISION={REVISION}\n")
        self.assertEqual(release.parse_environment(patched), {"SECRET": "kept value", "BUILD_REVISION": REVISION})

    def test_stack_problems_name_what_is_wrong(self):
        state = {name: service() for name in host.SERVICES}
        # The gateway has no health check and Postgres carries no revision label.
        state["gateway"]["health"] = "none"
        state["postgres"]["revision"] = ""
        self.assertEqual(host.problems(state, REVISION), [])
        state["panel"] = {"status": "missing"}
        state["control-plane"]["health"] = "starting"
        state["waiting-pumpkin"]["revision"] = "b" * 40
        self.assertEqual(host.problems(state, REVISION),
                         ["control-plane is starting", "panel is missing", "waiting-pumpkin runs bbbbbbbbbbbb"])

    def test_package_rejects_binaries_that_differ_from_the_builder(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = Path(directory) / REVISION / "artifacts"
            artifacts.mkdir(parents=True)
            for name in host.PUMPKIN_ARTIFACTS:
                (artifacts / name).write_bytes(name.encode())
            digests = {name: host.sha256(artifacts / name) for name in host.PUMPKIN_ARTIFACTS}
            with patch.object(host, "ARTIFACTS", Path(directory)), patch.object(host, "run") as run:
                with self.assertRaisesRegex(host.Failure, "differs from the build"):
                    host.package({"revision": REVISION, "artifacts": {**digests, "pumpkin": "0" * 64}})
                run.assert_not_called()

    def test_images_are_copied_only_when_the_worker_has_its_own_daemon(self):
        with tempfile.TemporaryDirectory() as directory:
            environment = Path(directory) / "worker.env"
            calls = []
            saved = type("Saved", (), {"stdout": type("Pipe", (), {"close": lambda self: None})(),
                                       "stderr": type("Pipe", (), {"read": lambda self: b""})(), "wait": lambda self: 0})()
            loaded = type("Loaded", (), {"returncode": 0, "stderr": b""})()
            def run(command, **_):
                calls.append(command)
                return "sha256:image"
            with patch.object(host, "WORKER_ENV", environment), patch.object(host, "run", side_effect=run), \
                 patch.object(host.subprocess, "Popen", return_value=saved) as save, \
                 patch.object(host.subprocess, "run", return_value=loaded) as load:
                environment.write_text("export PUMPKIN_IMAGE=x\n", encoding="utf-8")
                host.publish_to_worker("melon-pumpkin:host-control-x")
                save.assert_not_called()
                environment.write_text("export DOCKER_HOST=unix:///run/user/1800/docker.sock\n", encoding="utf-8")
                host.publish_to_worker("melon-pumpkin:host-control-x")
                self.assertEqual(load.call_args.args[0], ["docker", "--host", "unix:///run/user/1800/docker.sock", "load"])
                self.assertEqual(calls[-1][:3], ["docker", "--host", "unix:///run/user/1800/docker.sock"])

    def test_canary_reports_why_a_server_did_not_come_online(self):
        server = "3cdbfa7d-ca8f-4baf-abc0-410dc30d18ae"
        state = {name: service(name=name) for name in host.SERVICES}
        answers = iter(["restoring", "failed", "Pumpkin exited with status 1 during startup"])
        commands = []
        codes = iter(["409", "202"])
        def run(command, **_):
            commands.append(command)
            return next(codes)
        with patch.object(host, "services", return_value=state), patch.object(host, "run", side_effect=run), \
             patch.object(host, "query", side_effect=lambda *_: next(answers)), patch.object(host.time, "sleep"):
            with self.assertRaisesRegex(host.Failure, r"state: failed\): Pumpkin exited with status 1"):
                host.canary({"server": server})
            with self.assertRaisesRegex(host.Failure, "server UUID"):
                host.canary({"server": "x'; drop table servers"})
        # The start is requested from inside the control plane as the gateway would.
        self.assertEqual(commands[0][:3], ["docker", "exec", "control-plane"])
        self.assertIn('"reason":"gateway_join"', commands[0][-1])
        self.assertIn(f"/api/v1/servers/{server}/commands", commands[0][-1])
        # A start refused during the retry spacing was asked for again.
        self.assertEqual(len(commands), 2)
        with patch.object(host, "services", return_value=state), patch.object(host, "run", return_value="200"), \
             patch.object(host, "query", return_value="online"), patch.object(host.time, "sleep"):
            self.assertEqual(host.canary({"server": server})["server"], server)
        with patch.object(host, "services", return_value=state), patch.object(host, "run", return_value="403"), \
             patch.object(host.time, "sleep"):
            with self.assertRaisesRegex(host.Failure, "HTTP 403"):
                host.canary({"server": server})

    def test_a_worker_that_fails_to_start_is_replaced_by_the_previous_one(self):
        with tempfile.TemporaryDirectory() as directory:
            worker, environment = Path(directory) / "worker", Path(directory) / "worker.env"
            worker.write_bytes(b"old worker")
            environment.write_text("export PUMPKIN_IMAGE=melon-pumpkin:host-control-old\n", encoding="utf-8")
            state = {name: service(name=name) for name in host.SERVICES}
            started = []
            def start(binary, env, database):
                started.append((binary, env))
                return "No such image" if binary == b"new worker" else None
            def run(command, **_):
                if command[:2] == ["docker", "cp"]:
                    Path(command[3]).write_bytes(b"new worker")
                return "container"
            with patch.object(host, "WORKER", worker), patch.object(host, "WORKER_ENV", environment), \
                 patch.object(host, "RECORD", Path(directory) / "record.json"), patch.object(host, "services", return_value=state), \
                 patch.object(host, "run", side_effect=run), patch.object(host, "start_worker", side_effect=start):
                with self.assertRaisesRegex(host.Failure, "No such image.*previous worker was restored and is healthy"):
                    host.worker({"revision": REVISION})
            self.assertEqual(started[0], (b"new worker", f"export PUMPKIN_IMAGE=melon-pumpkin:host-control-{REVISION}\n".encode()))
            self.assertEqual(started[1], (b"old worker", b"export PUMPKIN_IMAGE=melon-pumpkin:host-control-old\n"))
            self.assertFalse((Path(directory) / "record.json").exists())


if __name__ == "__main__":
    unittest.main()
