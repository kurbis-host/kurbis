from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
import os
import subprocess
import stat
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sync = load("sync", ROOT / "scripts/dokploy_worker_sync.py")
deploy = load("deploy", ROOT / "scripts/deploy_dokploy.py")


class RolloutTests(unittest.TestCase):
    def test_no_mutable_cargo_target_cache_mounts(self):
        for path in (ROOT / "runtime/Dockerfile", ROOT / "runtime/pumpkin/Dockerfile"):
            self.assertNotIn("target=/source/target", path.read_text(encoding="utf-8"))

    def test_run_bytes_preserves_git_nul_records_and_leading_status_spaces(self):
        output = sync.run_bytes(["python3", "-c", "import sys; sys.stdout.buffer.write(bytes([32,77,32,99,111,109,112,111,115,101,46,121,97,109,108,0,33,33,32,46,101,110,118,0]))"])
        self.assertEqual(output, b" M compose.yaml\0!! .env\0")
        sync.validate_checkout_status(output, allow_overrides=True)

    def test_stage_preserves_allowed_compose_override_with_nul_status(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout = root / "checkout"
            backup_root = root / "backup"
            checkout.mkdir()
            backup_root.mkdir(mode=0o700)
            bare = root / "origin.git"
            subprocess.run(["git", "init", "--bare", "-q", str(bare)], check=True)
            subprocess.run(["git", "init", "-q", str(checkout)], check=True)
            subprocess.run(["git", "-C", str(checkout), "config", "user.name", "Rollout Test"], check=True)
            subprocess.run(["git", "-C", str(checkout), "config", "user.email", "rollout@example.invalid"], check=True)
            (checkout / ".gitignore").write_text(".env\n", encoding="utf-8")
            (checkout / "compose.yaml").write_text("services: {app: {image: app:old}}\n", encoding="utf-8")
            (checkout / "web").mkdir()
            (checkout / "web/tsconfig.json").write_text("{}\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(checkout), "add", ".gitignore", "compose.yaml", "web/tsconfig.json"], check=True)
            subprocess.run(["git", "-C", str(checkout), "commit", "-qm", "release fixture"], check=True)
            revision = subprocess.check_output(["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
            tag = f"kurbis-release-{revision}"
            subprocess.run(["git", "-C", str(checkout), "tag", tag, revision], check=True)
            subprocess.run(["git", "-C", str(checkout), "remote", "add", "origin", str(bare)], check=True)
            subprocess.run(["git", "-C", str(checkout), "push", "-q", "origin", f"refs/tags/{tag}"], check=True)
            expected = {path: subprocess.check_output(["git", "-C", str(checkout), "rev-parse", f"HEAD:{path}"], text=True).strip()
                        for path in (".gitignore", "compose.yaml", "web/tsconfig.json")}
            override = "services: {app: {image: app:operator-override}}\n"
            (checkout / "compose.yaml").write_text(override, encoding="utf-8")
            (checkout / ".env").write_text("PRESERVE_ME=1\n", encoding="utf-8")
            original_stat = Path.stat
            def stat_with_root_backup_owner(path, *args, **kwargs):
                if path == backup_root:
                    return type("RootOwnedDirectoryStat", (), {"st_uid": 0, "st_mode": stat.S_IFDIR | 0o700})()
                return original_stat(path, *args, **kwargs)
            token = "a" * 32
            with patch.object(sync, "CHECKOUT", checkout), patch.object(sync, "CHECKOUT_BACKUP", backup_root), \
                 patch.object(sync, "root_only"), patch.object(sync, "require_lock"), patch.object(sync, "identity", return_value="worker-id"), \
                 patch.object(Path, "stat", stat_with_root_backup_owner):
                result = sync.stage(revision, token, expected)
            self.assertEqual(result, {"status": "staged", "revision": revision})
            self.assertEqual((checkout / "compose.yaml").read_text(encoding="utf-8"), "services: {app: {image: app:old}}\n")
            self.assertEqual((checkout / ".env").read_text(encoding="utf-8"), "PRESERVE_ME=1\n")
            manifest = json.loads((backup_root / token / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest[0]["path"], "compose.yaml")
            self.assertEqual(base64.b64decode(manifest[0]["content"]).decode(), override)

    def test_checkout_status_allows_only_explicit_env_and_reviewed_overrides(self):
        sync.validate_checkout_status(b"!! .env\0 M compose.yaml\0", allow_overrides=True)
        sync.validate_checkout_status(b"!! .env\0", allow_overrides=False)
        for status in (b"?? generated.rs\0", b"!! target/\0", b"!! .env.local\0", b" M src/lib.rs\0"):
            with self.subTest(status=status), self.assertRaises(sync.Error):
                sync.validate_checkout_status(status, allow_overrides=True)

    def test_git_archive_contains_exact_tracked_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            (repo / "compose.yaml").write_bytes(b"services: {}\n")
            (repo / "runtime.txt").write_bytes(b"pinned source\x00bytes\n")
            subprocess.run(["git", "-C", str(repo), "add", "compose.yaml", "runtime.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "fixture"], check=True)
            archive = repo / "source.tar"
            with archive.open("wb") as output:
                subprocess.run(["git", "-C", str(repo), "archive", "--format=tar", "HEAD"], stdout=output, check=True)
            expected = {name: sync.git_blob((repo / name).read_bytes()) for name in ("compose.yaml", "runtime.txt")}
            self.assertEqual(len(sync.verify_git_archive(archive, expected)), 64)
            expected["runtime.txt"] = "0" * 40
            with self.assertRaises(sync.Error):
                sync.verify_git_archive(archive, expected)

    def test_source_manifest_covers_compose_and_web_tsconfig(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            paths = ("compose.yaml", "web/tsconfig.json", "runtime/Dockerfile")
            for name in paths:
                path = repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(f"{name}\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(repo), "add", *paths], check=True)
            subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "fixture"], check=True)
            with patch.object(deploy, "ROOT", repo):
                manifest = deploy.source_inputs("HEAD")
            tsconfig_blob = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD:web/tsconfig.json"], text=True).strip()
        self.assertEqual(set(manifest), set(paths))
        self.assertEqual(manifest["web/tsconfig.json"], tsconfig_blob)

    def test_compose_build_context_verifies_full_revision_and_secret_boundaries(self):
        revision = "c" * 40
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory)
            (checkout / "web").mkdir()
            expected = {}
            for name, content in {
                "compose.yaml": b"compose", "web/tsconfig.json": b"config", "runtime/Dockerfile": b"runtime",
                "runtime/pumpkin/Dockerfile": b"pumpkin", "web/Dockerfile": b"panel",
            }.items():
                path = checkout / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
                expected[name] = sync.git_blob(content)
            (checkout / ".dockerignore").write_text(".env*\n")
            (checkout / "web/.dockerignore").write_text(".env*\n")
            env_file = checkout / ".env"
            env_file.write_text("BUILD_REVISION=" + revision + "\nBEDROCK_TLS_HOST_DIRECTORY=/srv/kurbis/certs\nPANEL_BIND_ADDRESS=127.0.0.1\n")
            config = {"services": {
                "control-plane": {"image": "app:local", "build": {"context": str(checkout), "args": {"BUILD_REVISION": revision}}},
                "gateway": {"image": "app:local"},
                "panel": {"build": {"context": str(checkout / "web"), "args": {"BUILD_REVISION": revision}}, "ports": [{"host_ip": "127.0.0.1"}]},
                "waiting-pumpkin": {"build": {"context": str(checkout), "dockerfile": "runtime/pumpkin/Dockerfile", "target": "waiting", "args": {"BUILD_REVISION": revision}}},
                "postgres": {},
            }}
            def fake_run(args, **kwargs):
                if "rev-parse" in args:
                    return revision
                if "--ignored" in args:
                    return "!! .env"
                if "ls-tree" in args:
                    return "\n".join(expected)
                if "hash-object" in args:
                    return expected[args[-1]]
                if "compose" in args and "config" in args:
                    return json.dumps(config)
                raise AssertionError(args)
            with patch.object(sync, "CHECKOUT", checkout), patch.object(sync, "ENV", env_file), patch.object(sync, "run", side_effect=fake_run), patch.object(sync, "run_bytes", return_value=b"!! .env\0"):
                sync.verify_build_context(checkout, revision, expected)
                config["services"]["waiting-pumpkin"]["build"]["target"] = "host-control"
                with self.assertRaises(sync.Error):
                    sync.verify_build_context(checkout, revision, expected)

    def test_pending_worker_rollback_blocks_another_release(self):
        with tempfile.TemporaryDirectory() as directory:
            txdir = Path(directory)
            txdir.chmod(0o700)
            (txdir / "pending.json").write_text("{}")
            original_stat = Path.stat
            def root_owned_stat(path, *args, **kwargs):
                result = original_stat(path, *args, **kwargs)
                if path == txdir:
                    result = type("RootOwnedStat", (), {"st_uid": 0, "st_mode": stat.S_IFDIR | 0o700})()
                return result
            with patch.object(sync, "TRANSACTIONS", txdir), patch.object(Path, "stat", root_owned_stat):
                self.assertEqual(len(sync.pending_worker_transactions()), 1)

    def test_waiting_image_checks_real_label_and_embedded_executable(self):
        revision = "a" * 40
        executable = b"the exact host-control Pumpkin executable"
        calls: list[list[str]] = []

        def fake_run(args, **kwargs):
            calls.append(list(args))
            if args[:3] == ["docker", "image", "inspect"]:
                return f"sha256:waiting|{revision}"
            if args[:2] == ["docker", "create"]:
                return "container-id"
            if args[:2] == ["docker", "cp"]:
                source, destination = args[2], Path(args[3])
                if source.endswith("/usr/share/doc/kurbis/build-revision"):
                    destination.write_text(revision)
                elif source.endswith("/usr/share/doc/pumpkin/REVISION"):
                    destination.write_text("upstream-pin")
                elif source.endswith("/usr/local/bin/pumpkin"):
                    destination.write_bytes(executable)
                else:
                    raise AssertionError(source)
                return ""
            if args[:2] == ["docker", "rm"]:
                return ""
            raise AssertionError(args)

        with patch.object(sync, "run", side_effect=fake_run), patch.object(
            sync, "verify_pumpkin", return_value=("sha256:host-control", hashlib.sha256(executable).hexdigest())
        ):
            sync.verify_waiting(
                {"revision": revision, "image": "sha256:waiting"}, revision, "upstream-pin",
                f"{sync.PUMPKIN_PREFIX}{revision}",
            )
        self.assertTrue(any("sha256:waiting" in args for args in calls))

        with patch.object(sync, "run", return_value=f"sha256:waiting|{'b' * 40}") as inspect_mock, self.assertRaises(sync.Error):
            sync.verify_waiting({"revision": revision, "image": "sha256:waiting"}, revision, "upstream-pin", f"{sync.PUMPKIN_PREFIX}{revision}")
        inspect_mock.assert_called_once()

        def wrong_executable(args, **kwargs):
            if args[:3] == ["docker", "image", "inspect"]:
                return f"sha256:waiting|{revision}"
            if args[:2] == ["docker", "create"]:
                return "container-id"
            if args[:2] == ["docker", "cp"]:
                source, destination = args[2], Path(args[3])
                destination.write_text(revision if "build-revision" in source else "upstream-pin") if "REVISION" in source or "build-revision" in source else destination.write_bytes(b"different executable")
                return ""
            if args[:2] == ["docker", "rm"]:
                return ""
            raise AssertionError(args)

        with patch.object(sync, "run", side_effect=wrong_executable), patch.object(
            sync, "verify_pumpkin", return_value=("sha256:host-control", hashlib.sha256(executable).hexdigest())
        ), self.assertRaises(sync.Error):
            sync.verify_waiting(
                {"revision": revision, "image": "sha256:waiting"}, revision, "upstream-pin",
                f"{sync.PUMPKIN_PREFIX}{revision}",
            )

    def test_worker_bundle_verification_failure_restores_both_files_and_old_service(self):
        state = {"binary": b"old-worker", "env": b"old-worker-env"}
        restarts: list[tuple[bytes, bytes]] = []
        new_verifications = 0

        def write_binary(value: bytes) -> None:
            state["binary"] = value

        def write_env(value: bytes) -> None:
            state["env"] = value

        def restart() -> None:
            restarts.append((state["binary"], state["env"]))

        def verify_new() -> None:
            nonlocal new_verifications
            new_verifications += 1
            raise sync.Error("simulated health/lease failure after service restart")

        def verify_old() -> None:
            self.assertEqual(state, {"binary": b"old-worker", "env": b"old-worker-env"})

        with self.assertRaises(sync.BundleMutationError) as raised:
            sync.mutate_worker_bundle(
                old_binary=b"old-worker", new_binary=b"new-worker", old_env=b"old-worker-env", new_env=b"new-worker-env",
                binary_changed=True, env_changed=True, write_binary=write_binary, write_env=write_env,
                restart=restart, verify_new=verify_new, verify_old=verify_old,
            )
        self.assertTrue(raised.exception.rollback_verified)
        self.assertEqual(new_verifications, 1)
        self.assertEqual(state, {"binary": b"old-worker", "env": b"old-worker-env"})
        self.assertEqual(restarts, [(b"new-worker", b"new-worker-env"), (b"old-worker", b"old-worker-env")])

    def test_worker_transaction_reports_unverified_rollback_as_failure(self):
        state = {"binary": b"new-worker", "env": b"new-env"}

        def fail_old_verification():
            raise sync.Error("old service did not recover")

        with self.assertRaises(sync.BundleMutationError) as raised:
            sync.mutate_worker_bundle(
                old_binary=b"old-worker", new_binary=b"new-worker", old_env=b"old-env", new_env=b"new-env",
                binary_changed=True, env_changed=True,
                write_binary=lambda value: state.__setitem__("binary", value),
                write_env=lambda value: state.__setitem__("env", value),
                restart=lambda: None, verify_new=lambda: (_ for _ in ()).throw(sync.Error("new health failed")),
                verify_old=fail_old_verification,
            )
        self.assertFalse(raised.exception.rollback_verified)
        self.assertEqual(state, {"binary": b"old-worker", "env": b"old-env"})

    def test_remote_lock_rejects_wrong_owner(self):
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / "lock"
            lock.mkdir()
            (lock / "owner").write_text("b" * 32)
            with patch.object(sync, "LOCK", lock), patch.object(sync, "root_only"):
                with self.assertRaises(sync.Error):
                    sync.require_lock("a" * 32)
                sync.require_lock("b" * 32)

    def test_marker_refuses_mixed_control_plane_gateway_revisions(self):
        revision = "a" * 40
        receipt = {
            "status": "verified", "source_revision": revision, "deployment_id": "deployment-1",
            "image_id": "sha256:expected", "control_plane_sha256": "1" * 64,
            "gateway_sha256": "2" * 64, "worker_sha256": "3" * 64, "worker_id": "worker-id",
            "worker_lease_verified": "true", "pumpkin_image_id": "sha256:pumpkin",
            "pumpkin_image": f"{sync.PUMPKIN_PREFIX}{revision}", "pumpkin_sha256": "4" * 64,
            "worker_pid": "123", "rollback_worker_sha256": "5" * 64,
            "rollback_worker_env_sha256": "6" * 64, "release_tag": f"kurbis-release-{revision}",
            "source_archive_sha256": "7" * 64, "host_control_pumpkin_image": "sha256:pumpkin",
        }
        mixed = {name: [{"id": name, "image": "sha256:wrong", "status": "running", "health": "healthy", "revision": revision}]
                 for name in sync.SERVICES}
        mixed["control-plane"][0]["image"] = "sha256:expected"
        with patch.object(sync, "require_lock"), patch.object(sync, "states", return_value=mixed), patch.object(
            sync, "atomic", side_effect=AssertionError("marker must not be written")
        ), self.assertRaises(sync.Error):
            sync.mark(json.dumps(receipt), "a" * 32)

    def test_compose_info_rejects_stack_mode_and_custom_commands(self):
        base = {"composeId": deploy.COMPOSE_ID, "appName": deploy.PROJECT, "sourceType": "github",
                "owner": "kurbis-host", "repository": "kurbis", "branch": "main", "composePath": "./compose.yaml",
                "environment": {"name": "production"}, "composeType": "docker-compose", "command": None,
                "env": "", "autoDeploy": False}
        for update in ({"composeType": "docker-stack"}, {"command": "docker compose up -d"}, {"command": "docker compose up -d --build"}, {"command": "absent"}):
            value = {**base, **update}
            if update == {"command": "absent"}:
                value.pop("command")
            with self.subTest(update=update), patch.object(deploy, "api", return_value=value), self.assertRaises(deploy.DeploymentError):
                deploy.compose_info()
        with patch.object(deploy, "api", return_value=base):
            self.assertEqual(deploy.compose_info()["composeType"], "docker-compose")

    def test_compose_source_and_auto_deploy_policy_persist(self):
        state = {
            "composeId": deploy.COMPOSE_ID, "appName": deploy.PROJECT, "sourceType": "github",
            "owner": "kurbis-host", "repository": "kurbis", "branch": "main",
            "composePath": "./compose.yaml", "environment": {"name": "production"},
            "composeType": "docker-compose", "command": None,
            "env": "KEEP=this-value\nBUILD_REVISION=old\n", "autoDeploy": True,
        }
        calls = []

        def fake_api(endpoint, params, *, post=False):
            calls.append((endpoint, params, post))
            if endpoint == "compose.one":
                return dict(state)
            if endpoint == "compose.update":
                state["autoDeploy"] = params["autoDeploy"]
                return {}
            if endpoint == "compose.saveEnvironment":
                state["env"] = params["env"]
                return {}
            raise AssertionError(endpoint)

        with patch.object(deploy, "api", side_effect=fake_api):
            old_branch, old_env = deploy.prepare_dokploy("a" * 40)
        self.assertEqual(old_branch, "main")
        self.assertIn("BUILD_REVISION=old", old_env)
        self.assertIs(state["autoDeploy"], False)
        final_env = deploy.parse_env(state["env"])
        self.assertEqual(final_env["KEEP"], "this-value")
        self.assertEqual(final_env["BUILD_REVISION"], "a" * 40)
        self.assertEqual(final_env["BEDROCK_TLS_HOST_DIRECTORY"], "/srv/kurbis/certs")
        self.assertEqual(final_env["PANEL_BIND_ADDRESS"], "127.0.0.1")
        self.assertTrue(any(endpoint == "compose.update" for endpoint, _, _ in calls))

    def test_failed_rollout_restores_revision_pin_but_not_auto_deploy_policy(self):
        state = {"composeId": deploy.COMPOSE_ID, "appName": deploy.PROJECT, "sourceType": "github",
                 "owner": "kurbis-host", "repository": "kurbis", "branch": "main", "composePath": "./compose.yaml",
                 "environment": {"name": "production"}, "env": "KEEP=value\nBUILD_REVISION=old\n", "autoDeploy": False}
        saved = []
        with patch.object(deploy, "compose_info", return_value=state), patch.object(
            deploy, "api", side_effect=lambda endpoint, params, **kwargs: saved.append(params["env"])
        ):
            deploy.restore_build_revision("KEEP=value\nBUILD_REVISION=old\n")
        self.assertIn("BUILD_REVISION=old", saved[0])
        self.assertIn("KEEP=value", saved[0])
        self.assertIs(state["autoDeploy"], False)

    def test_deployment_gate_rejects_existing_active_run(self):
        with patch.object(deploy, "deployment_list", return_value=[{"deploymentId": "active", "status": "running"}]), self.assertRaises(deploy.DeploymentError):
            deploy.assert_no_active_deployments()

    def test_first_join_regression_is_part_of_release_gate(self):
        test_file = ROOT / "tests/first_join_flow.rs"
        self.assertTrue(test_file.is_file())
        contents = test_file.read_text(encoding="utf-8")
        self.assertIn("TEST_DATABASE_URL", contents)
        self.assertIn("complete_start", contents)

    def test_database_regression_refuses_to_run_without_disposable_database(self):
        with patch.dict(os.environ, {}, clear=True), patch.object(deploy, "run") as run_mock:
            with self.assertRaisesRegex(deploy.DeploymentError, "TEST_DATABASE_URL"):
                deploy.run_release_tests()
        run_mock.assert_not_called()

    def test_release_suite_runs_rollout_behavior_and_first_join_database_tests(self):
        with patch.dict(os.environ, {"TEST_DATABASE_URL": "postgres://disposable"}), patch.object(deploy, "run") as run_mock:
            deploy.run_release_tests()
        commands = [entry.args[0] for entry in run_mock.call_args_list]
        self.assertEqual(commands[0][1:4], ["-m", "unittest", "discover"])
        self.assertIn("test_dokploy_worker_rollout.py", commands[0])
        self.assertEqual(commands[1][1:5], ["test", "--locked", "--test", "first_join_flow"])


if __name__ == "__main__":
    unittest.main()
