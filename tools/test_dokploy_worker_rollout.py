from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
import os
import shlex
import shutil
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


class NoopOperationLock:
    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


class RolloutTests(unittest.TestCase):



    def test_strict_baseline_rejects_worker_pumpkin_from_another_app_revision(self):
        active = "f" * 40
        other = "e" * 40
        state = {name: [{"id": name, "image": "sha256:app", "status": "running", "health": "healthy", "revision": active}]
                 for name in sync.SERVICES}
        with patch.object(sync, "OperationLock", NoopOperationLock), patch.object(sync, "root_only"), patch.object(sync, "service_valid"), patch.object(sync, "identity", return_value="worker"), \
             patch.object(sync, "pending_worker_transactions", return_value=[]), patch.object(sync, "worker_image", return_value=f"{sync.PUMPKIN_PREFIX}{other}"), \
             patch.object(sync, "states", return_value=state), self.assertRaisesRegex(sync.Error, "revisions do not match"):
            sync.baseline()

    def test_preflight_rejects_inconsistent_active_app_before_bootstrap(self):
        state = {name: [{"id": name, "image": "sha256:app", "status": "running", "health": "healthy", "revision": "a" * 40}]
                 for name in sync.SERVICES}
        state["gateway"][0]["revision"] = "b" * 40
        with patch.object(sync, "OperationLock", NoopOperationLock), patch.object(sync, "root_only"), patch.object(sync, "service_valid"), patch.object(sync, "identity", return_value="worker"), \
             patch.object(sync, "pending_worker_transactions", return_value=[]), patch.object(sync, "states", return_value=state), \
             self.assertRaisesRegex(sync.Error, "consistent full-SHA"):
            sync.baseline_preflight()

    def test_worker_process_environment_reads_nul_delimited_exports_exactly(self):
        raw = b"PATH=/usr/bin\0PUMPKIN_IMAGE=melon-pumpkin:host-control-" + b"a" * 40 + b"\0WORKER_ID=worker\0"
        with patch.object(Path, "read_bytes", return_value=raw):
            environment = sync.worker_process_environment(123)
        self.assertEqual(environment["PUMPKIN_IMAGE"], "melon-pumpkin:host-control-" + "a" * 40)
        self.assertEqual(environment["WORKER_ID"], "worker")

    def test_canonical_release_has_no_legacy_deployment_paths(self):
        for relative in (
            "scripts/dokploy_legacy_baseline.py",
            "scripts/dokploy_legacy_worker_recovery.py",
            "deployment/legacy-baseline-respond5.json",
            "tests/fixtures/respond5_legacy_baseline_inventory.json",
            "tools/test_dokploy_legacy_baseline.py",
        ):
            self.assertFalse((ROOT / relative).exists(), relative)
        source = (ROOT / "scripts/deploy_dokploy.py").read_text(encoding="utf-8")
        self.assertNotIn("legacy_baseline", source)
        self.assertNotIn("--legacy", source)

    def test_no_mutable_cargo_target_cache_mounts(self):
        for path in (ROOT / "runtime/Dockerfile", ROOT / "runtime/pumpkin/Dockerfile"):
            self.assertNotIn("target=/source/target", path.read_text(encoding="utf-8"))

    def test_runtime_image_propagates_build_revision_to_binaries_and_label(self):
        dockerfile = (ROOT / "runtime/Dockerfile").read_text(encoding="utf-8")
        builder_start = dockerfile.index("FROM rust:1.98-bookworm AS builder")
        runtime_start = dockerfile.index("FROM debian:bookworm-slim", builder_start)
        builder = dockerfile[builder_start:runtime_start]
        runtime = dockerfile[runtime_start:]

        self.assertIn("ARG BUILD_REVISION=development", builder)
        self.assertIn("ENV KURBIS_BUILD_REVISION=${BUILD_REVISION}", builder)
        self.assertIn("ARG BUILD_REVISION=development", runtime)
        self.assertIn("LABEL org.opencontainers.image.revision=${BUILD_REVISION}", runtime)



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
                manifest = json.loads((backup_root / token / "manifest.json").read_text(encoding="utf-8"))
                restored = sync.restore(token)
            self.assertEqual(result, {"status": "staged", "revision": revision})
            self.assertEqual(manifest[0]["path"], "compose.yaml")
            self.assertEqual(base64.b64decode(manifest[0]["content"]).decode(), override)
            self.assertEqual(restored["status"], "restored")
            self.assertEqual((checkout / "compose.yaml").read_text(encoding="utf-8"), override)
            self.assertEqual((checkout / ".env").read_text(encoding="utf-8"), "PRESERVE_ME=1\n")

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
            # Dokploy's protected source .env predates the centrally stored release-only
            # settings; these must be overlaid from the reviewed prepared Dokploy values.
            env_file.write_text("ROOT_DOMAIN=kurbis.host\nBEDROCK_PORT=19132\n")
            config = {"services": {
                "control-plane": {"image": "app:local", "build": {"context": str(checkout), "args": {"BUILD_REVISION": revision}}},
                "gateway": {"image": "app:local", "user": "2613:2613",
                    "environment": {"BEDROCK_TLS_CERT_FILE": "/run/kurbis-tls/fullchain.pem",
                                    "BEDROCK_TLS_KEY_FILE": "/run/kurbis-tls/privkey.pem"},
                    "ports": [{"published": "19132", "target": 19132, "protocol": protocol} for protocol in ("tcp", "udp")],
                    "volumes": [{"type": "bind", "source": "/srv/kurbis/certs", "target": "/run/kurbis-tls", "read_only": True}]},
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
                    env_args=[args[i+1] for i,value in enumerate(args[:-1]) if value=="--env-file"]
                    self.assertEqual(len(env_args),2)
                    self.assertEqual(Path(env_args[0]),env_file)
                    override_values={}
                    for line in Path(env_args[1]).read_text(encoding="utf-8").splitlines():
                        key,value=line.split("=",1);override_values[key]=value
                    self.assertEqual(override_values,{"BEDROCK_TLS_HOST_DIRECTORY":"/srv/kurbis/certs",
                        "BUILD_REVISION":revision,"PANEL_BIND_ADDRESS":"127.0.0.1"})
                    self.assertEqual(Path(env_args[1]).stat().st_mode & 0o777,0o600)
                    return json.dumps(config)
                raise AssertionError(args)
            with patch.object(sync, "CHECKOUT", checkout), patch.object(sync, "ENV", env_file), patch.object(sync, "run", side_effect=fake_run), patch.object(sync, "run_bytes", return_value=b"!! .env\0"), patch.object(sync, "verify_bedrock_tls_assets"):
                release_env={"BUILD_REVISION":revision,"BEDROCK_TLS_HOST_DIRECTORY":"/srv/kurbis/certs","PANEL_BIND_ADDRESS":"127.0.0.1"}
                sync.verify_build_context(checkout, revision, expected,release_environment=release_env)
                config["services"]["waiting-pumpkin"]["build"]["target"] = "host-control"
                with self.assertRaises(sync.Error):
                    sync.verify_build_context(checkout, revision, expected,release_environment=release_env)
                with self.assertRaisesRegex(sync.Error,"environment overrides differ"):
                    sync.verify_build_context(checkout,revision,expected,release_environment={**release_env,"PANEL_BIND_ADDRESS":"0.0.0.0"})

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

    def test_app_rollback_verifier_accepts_healthy_prior_revision_without_forcing_recreate(self):
        revision = "9" * 40
        state = {name: [{"id": name, "image": "sha256:prior-app", "status": "running", "health": "healthy", "revision": revision}]
                 for name in sync.SERVICES}
        class NoopLock:
            def __enter__(self): return self
            def __exit__(self, *args): return False
        worker_hash = "1" * 64
        with patch.object(sync, "OperationLock", NoopLock), patch.object(sync, "require_lock"), \
             patch.object(sync, "states", return_value=state), patch.object(sync, "verify_worker_binary_in_app_image", return_value=worker_hash), \
             patch.object(sync, "verify_image_revision"), patch.object(sync, "run", return_value="upstream-pin"), \
             patch.object(sync, "worker_image", return_value=f"{sync.PUMPKIN_PREFIX}{revision}"), \
             patch.object(sync, "verify_waiting"), patch.object(sync, "single_worker", return_value=12), \
             patch.object(sync, "digest", return_value=worker_hash), \
             patch.object(sync, "worker_process_environment", return_value={"PUMPKIN_IMAGE": f"{sync.PUMPKIN_PREFIX}{revision}"}), patch.object(sync, "lease"):
            result = sync.verify_app_revision(revision, state, "a" * 32)
        self.assertEqual(result["status"], "app-rolled-back")
        self.assertEqual(result["revision"], revision)

    def test_app_rollback_verifier_requires_prior_snapshot_revision(self):
        revision = "9" * 40
        state = {name: [{"id": name, "image": "sha256:prior-app", "status": "running", "health": "healthy", "revision": revision}]
                 for name in sync.SERVICES}
        snapshot = {name: [dict(items[0])] for name, items in state.items()}
        snapshot["control-plane"][0]["revision"] = "8" * 40
        class NoopLock:
            def __enter__(self): return self
            def __exit__(self, *args): return False
        with patch.object(sync, "OperationLock", NoopLock), patch.object(sync, "require_lock"), patch.object(sync, "states", return_value=state), \
             self.assertRaisesRegex(sync.Error, "pre-deploy production snapshot"):
            sync.verify_app_revision(revision, snapshot, "a" * 32)

    def test_app_rollback_verifier_rejects_mixed_gateway_revision(self):
        revision = "9" * 40
        state = {name: [{"id": name, "image": "sha256:prior-app", "status": "running", "health": "healthy", "revision": revision}]
                 for name in sync.SERVICES}
        snapshot = {name: [dict(items[0])] for name, items in state.items()}
        state["gateway"][0]["revision"] = "8" * 40
        class NoopLock:
            def __enter__(self): return self
            def __exit__(self, *args): return False
        with patch.object(sync, "OperationLock", NoopLock), patch.object(sync, "require_lock"), patch.object(sync, "states", return_value=state), \
             self.assertRaisesRegex(sync.Error, "prior accepted revision"):
            sync.verify_app_revision(revision, snapshot, "a" * 32)

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
            "schema": "kurbis.release-manifest.v1", "worker_env_sha256": "8" * 64,
            "migration_manifest_sha256": "9" * 64, "environment_revision": "a" * 64,
            "environment_keys": ["BUILD_REVISION"],
            "service_image_ids": {
                "control-plane": "sha256:expected", "gateway": "sha256:expected",
                "panel": "sha256:expected", "waiting-pumpkin": "sha256:expected",
                "postgres": "sha256:expected",
            },
        }
        receipt["manifest_sha256"] = sync.release_manifest_hash(receipt)
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
        default = "docker compose up -d --build"
        for update in ({"composeType": "docker-stack"}, {"command": "docker compose up -d"},
                       {"command": "docker compose up -d --no-build"}, {"command": "absent"}):
            value = {**base, **update}
            if update == {"command": "absent"}:
                value.pop("command")
            def fake_api(endpoint, params):
                if endpoint == "compose.one":
                    return value
                if endpoint == "patch.byEntityId":
                    return []
                return default
            with self.subTest(update=update), patch.object(deploy, "api", side_effect=fake_api), self.assertRaises(deploy.DeploymentError):
                deploy.compose_info()
        def safe_api(endpoint, params):
            if endpoint == "compose.one":
                return base
            if endpoint == "patch.byEntityId":
                return []
            return default
        with patch.object(deploy, "api", side_effect=safe_api):
            self.assertEqual(deploy.compose_info()["composeType"], "docker-compose")
        custom_default = {**base, "command": default}
        def custom_api(endpoint, params):
            if endpoint == "compose.one":
                return custom_default
            if endpoint == "patch.byEntityId":
                return []
            return default
        with patch.object(deploy, "api", side_effect=custom_api):
            with self.assertRaisesRegex(deploy.DeploymentError,"custom Compose command"):
                deploy.compose_info()

    def test_compose_info_rejects_enabled_compose_override_patch(self):
        base = {"composeId": deploy.COMPOSE_ID, "appName": deploy.PROJECT, "sourceType": "github",
                "owner": "kurbis-host", "repository": "kurbis", "branch": "main", "composePath": "./compose.yaml",
                "environment": {"name": "production"}, "composeType": "docker-compose", "command": None,
                "env": "", "autoDeploy": False}
        def fake_api(endpoint, params):
            if endpoint == "compose.one":
                return base
            if endpoint == "patch.byEntityId":
                return [{"patchId": "stale-compose", "filePath": "compose.yaml", "enabled": True}]
            return "docker compose up -d --build"
        # A path spelling without the leading ./ is still the same source file.
        def fake_api_normalized(endpoint, params):
            if endpoint == "compose.one":
                return base
            if endpoint == "patch.byEntityId":
                return [{"patchId": "stale-compose", "filePath": "./compose.yaml", "enabled": True}]
            return "docker compose up -d --build"
        with patch.object(deploy, "api", side_effect=fake_api_normalized), self.assertRaisesRegex(
            deploy.DeploymentError, "enabled production Compose override"
        ):
            deploy.compose_info()
        with patch.object(deploy, "api", side_effect=fake_api), self.assertRaisesRegex(
            deploy.DeploymentError, "enabled production Compose override"
        ):
            deploy.compose_info()

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
            if endpoint == "compose.getDefaultCommand":
                return "docker compose up -d --build"
            if endpoint == "patch.byEntityId":
                return []
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





    def test_lost_wait_response_rolls_back_worker_before_prior_app_revision(self):
        state = {"worker": "old", "app": "new"}
        attempts = {"rollback": 0, "app": 0}
        wait_attempted = False
        def on_attempt():
            nonlocal wait_attempted
            wait_attempted = True
        def remote_wait(**kwargs):
            state["worker"] = "new"
            raise TimeoutError("SSH response lost after remote mutation")
        def worker_rollback():
            attempts["rollback"] += 1
            if state["worker"] == "new":
                state["worker"] = "old"
                return {"status": "rolled-back"}
            return {"status": "no-pending-worker-transaction"}
        def app_rollback():
            attempts["app"] += 1
            self.assertEqual(state["worker"], "old")
            state["app"] = "old"
            return {"status": "app-rolled-back"}
        with self.assertRaisesRegex(TimeoutError, "response lost"):
            deploy.dispatch_worker_wait(remote_wait, worker_rollback, on_attempt, token="token")
        result = deploy.rollback_failed_release(worker_rollback, app_rollback)
        self.assertTrue(wait_attempted)
        self.assertEqual(result["status"], "app-rolled-back")
        self.assertEqual(state, {"worker": "old", "app": "old"})
        self.assertEqual(attempts, {"rollback": 2, "app": 1})

    def test_unverified_app_rollback_keeps_immutable_dokploy_source_and_lock(self):
        self.assertFalse(deploy.should_restore_dokploy_source(deployment_attempted=True, app_revision_restored=False, marked=False))
        self.assertTrue(deploy.should_restore_dokploy_source(deployment_attempted=True, app_revision_restored=True, marked=False))
        self.assertTrue(deploy.should_restore_dokploy_source(deployment_attempted=False, app_revision_restored=False, marked=False))
        self.assertTrue(deploy.should_restore_dokploy_source(deployment_attempted=True, app_revision_restored=False, marked=True))

    def test_accepted_worker_release_is_not_rolled_back_after_finalize_response_loss(self):
        app_rollback = unittest.mock.Mock(side_effect=AssertionError("accepted release must not roll back app"))
        result = deploy.rollback_failed_release(lambda: {"status": "already-accepted"}, app_rollback)
        self.assertEqual(result["status"], "already-accepted")
        app_rollback.assert_not_called()

    def test_worker_verification_failure_rolls_back_old_app_after_worker(self):
        actions = []
        def worker_rollback():
            actions.append("worker")
            return {"status": "rolled-back"}
        def app_rollback():
            actions.append("app")
            return {"status": "app-rolled-back"}
        result = deploy.rollback_failed_release(worker_rollback, app_rollback)
        self.assertEqual(result["status"], "app-rolled-back")
        self.assertEqual(actions, ["worker", "app"])

    def test_app_rollback_is_still_attempted_when_worker_rollback_is_unverified(self):
        actions = []
        def worker_rollback():
            actions.append("worker")
            raise TimeoutError("rollback acknowledgement unavailable")
        def app_rollback():
            actions.append("app")
            return {"status": "app-rolled-back"}
        with self.assertRaisesRegex(deploy.DeploymentRollbackError, "worker rollback status was not reconciled") as raised:
            deploy.rollback_failed_release(worker_rollback, app_rollback)
        self.assertTrue(raised.exception.app_rolled_back)
        self.assertFalse(raised.exception.worker_rolled_back)
        self.assertEqual(actions, ["worker", "app"])


    def test_rollback_waits_for_superseded_deployment_to_finish(self):
        samples = [
            [{"deploymentId": "new-release", "status": "running"}],
            [{"deploymentId": "new-release", "status": "done"}],
        ]
        with patch.object(deploy, "deployment_list", side_effect=samples), patch.object(deploy.time, "sleep") as sleep:
            deploy.wait_for_terminal_deployments(10)
        sleep.assert_called_once_with(5)

    def test_rollback_refuses_to_start_overlapping_when_deploy_never_finishes(self):
        with patch.object(deploy, "deployment_list", return_value=[{"deploymentId": "stuck", "status": "running"}]), \
             patch.object(deploy.time, "monotonic", side_effect=[0, 2]), patch.object(deploy.time, "sleep"), \
             self.assertRaisesRegex(deploy.DeploymentError, "did not finish before rollback"):
            deploy.wait_for_terminal_deployments(1)

    def test_deployment_gate_rejects_existing_active_run(self):
        with patch.object(deploy, "deployment_list", return_value=[{"deploymentId": "active", "status": "running"}]), self.assertRaises(deploy.DeploymentError):
            deploy.assert_no_active_deployments()

    def test_bedrock_tls_preflight_rejects_world_readable_private_key(self):
        with tempfile.TemporaryDirectory() as directory:
            tls=Path(directory);(tls/"fullchain.pem").write_text("certificate fixture")
            key=tls/"privkey.pem";key.write_text("private-key fixture");key.chmod(0o644)
            gateway_id="a"*64
            inspected=json.dumps([{"Mounts":[{"Destination":"/run/kurbis-tls","Source":str(tls),"RW":False}]}])
            with patch.object(sync,"run",side_effect=[gateway_id,inspected]) as run_command:
                with self.assertRaisesRegex(sync.Error,"must not be accessible to other"):
                    sync.verify_bedrock_tls_assets(str(tls),"example.test",error_cls=sync.Error,run_command=sync.run,project=sync.PROJECT)
            ps_args=run_command.call_args_list[0].args[0]
            self.assertIn("--no-trunc",ps_args)
            self.assertIn("-q",ps_args)

    @unittest.skipUnless(shutil.which("docker"), "Docker Compose CLI is needed to render the production Compose fixture")
    def test_rendered_production_compose_has_required_read_only_bedrock_tls_and_ports(self):
        with tempfile.TemporaryDirectory() as directory:
            rendered_env=Path(directory)/"legacy-production.env"
            release_env=Path(directory)/"release-overrides.env"
            values={}
            for line in (ROOT/".env.example").read_text(encoding="utf-8").splitlines():
                if line and not line.lstrip().startswith("#") and "=" in line:
                    key,value=line.split("=",1);values[key]=value
            for key in ("BEDROCK_TLS_HOST_DIRECTORY","PANEL_BIND_ADDRESS","BUILD_REVISION"):
                values.pop(key,None)
            values.update({"BEDROCK_PORT":"19132","JAVA_PORT":"25565"})
            rendered_env.write_text("".join(f"{key}={value}\n" for key,value in values.items()),encoding="utf-8")
            release_values={"BEDROCK_TLS_HOST_DIRECTORY":"/srv/kurbis/certs","PANEL_BIND_ADDRESS":"127.0.0.1","BUILD_REVISION":"a"*40}
            release_env.write_text("".join(f"{key}={value}\n" for key,value in release_values.items()),encoding="utf-8")
            release_env.chmod(0o600)
            output=subprocess.run([shutil.which("docker"),"compose","--env-file",str(rendered_env),"--env-file",str(release_env),
                "-f",str(ROOT/"compose.yaml"),"config","--format","json"],
                cwd=ROOT,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False)
            self.assertEqual(output.returncode,0,output.stderr)
            services=json.loads(output.stdout)["services"]
            sync.verify_gateway_tls_configuration(services,"/srv/kurbis/certs","19132",error_cls=sync.Error)
            gateway=services["gateway"]
            self.assertEqual(services["control-plane"]["build"]["args"]["BUILD_REVISION"],"a"*40)
            self.assertEqual(services["panel"]["ports"][0]["host_ip"],"127.0.0.1")
            control_environment = services["control-plane"]["environment"]
            self.assertEqual(control_environment["CI_REPOSITORY"],"Pumpkin-MC/Pumpkin")
            self.assertEqual(control_environment["CI_GITHUB_API_BASE_URL"],"https://api.github.com")
            self.assertNotIn("CI_GITHUB_WEBHOOK_SECRET", control_environment)
            for key in ("CI_GITHUB_PAT", "CI_GITHUB_WEBHOOK_ENABLED", "CI_DEFAULT_BRANCH", "CI_BUILDER_BACKEND",
                        "CI_BUILDER_SSH_TARGET", "CI_BUBBLEWRAP_RUNNER", "CI_BUBBLEWRAP_PATH",
                        "CI_BUBBLEWRAP_ROOTFS", "CI_BUBBLEWRAP_TOOLCHAIN", "CI_BUBBLEWRAP_WORKSPACE",
                        "CI_BUBBLEWRAP_SOURCE_PREPARER", "CI_BUILD_CONCURRENCY"):
                self.assertIn(key, control_environment)
            self.assertTrue(any(v.get("target") == "/home/kurbis/.ssh" for v in services["control-plane"]["volumes"]))
            self.assertEqual(gateway["environment"]["BEDROCK_PUBLIC_IP"],"203.0.113.10")
            self.assertEqual(gateway["environment"]["JAVA_PUBLIC_PORT"],"25565")
            self.assertEqual(gateway["environment"]["BEDROCK_TLS_CERT_FILE"],"/run/kurbis-tls/fullchain.pem")
            self.assertEqual(gateway["environment"]["BEDROCK_TLS_KEY_FILE"],"/run/kurbis-tls/privkey.pem")
            mount=next(v for v in gateway["volumes"] if v.get("target")=="/run/kurbis-tls")
            self.assertTrue(mount["read_only"])
            self.assertEqual(mount["source"],"/srv/kurbis/certs")
            broken=json.loads(json.dumps(services));next(v for v in broken["gateway"]["volumes"] if v.get("target")=="/run/kurbis-tls")["read_only"]=False
            with self.assertRaisesRegex(sync.Error,"mounted read-only"):
                sync.verify_gateway_tls_configuration(broken,"/srv/kurbis/certs","19132",error_cls=sync.Error)

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

    def test_worker_sync_remote_payload_inlines_tls_module_and_compiles(self):
        source=deploy.worker_sync_source()
        self.assertNotIn("__TLS_HELPERS__",source)
        self.assertIn("def verify_gateway_tls_configuration",source)
        self.assertIn("def verify_bedrock_tls_assets",source)
        compile(source,"<respond5-worker-sync-stdin>","exec")


    def test_remote_sends_only_reviewed_release_environment_overrides(self):
        overrides={"BUILD_REVISION":"a"*40,"BEDROCK_TLS_HOST_DIRECTORY":"/srv/kurbis/certs",
                   "PANEL_BIND_ADDRESS":"127.0.0.1"}
        with patch.object(deploy,"run",return_value="{}") as remote_run:
            result=deploy.remote("respond5","verify-build-context",token="token",revision="a"*40,
                                 release_environment=overrides)
        self.assertEqual(result,{})
        command=shlex.split(remote_run.call_args.args[0][-1])
        index=command.index("--release-environment")
        self.assertEqual(json.loads(command[index+1]),overrides)
        self.assertNotIn("POSTGRES_PASSWORD",command[index+1])

    def test_release_suite_runs_rollout_behavior_and_first_join_database_tests(self):
        with patch.dict(os.environ, {"TEST_DATABASE_URL": "postgres://disposable"}), patch.object(deploy, "run") as run_mock:
            deploy.run_release_tests()
        commands = [entry.args[0] for entry in run_mock.call_args_list]
        self.assertEqual(len(commands), 3)
        self.assertEqual(commands[0][1:4], ["-m", "unittest", "discover"])
        self.assertIn("test_dokploy_worker_rollout.py", commands[0])
        self.assertEqual(commands[1][1:4], ["-m", "unittest", "discover"])
        self.assertIn("test_release_manifest.py", commands[1])
        self.assertEqual(commands[2][1:5], ["test", "--locked", "--test", "first_join_flow"])



if __name__ == "__main__":
    unittest.main()
