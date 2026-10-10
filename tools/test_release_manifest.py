from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


migration = load("migration_manifest", ROOT / "scripts/migration_manifest.py")
release = load("release_manifest", ROOT / "scripts/release_manifest.py")


class ReleaseManifestTests(unittest.TestCase):
    def test_migration_manifest_matches_every_sql_file_and_has_unique_versions(self):
        digest = migration.validate_manifest(ROOT / "migrations", ROOT / "migrations/MANIFEST.json")
        self.assertEqual(len(digest), 64)
        records = migration.migration_records(ROOT / "migrations")
        self.assertEqual([record["version"] for record in records], list(range(1, 17)))

    def test_release_manifest_binds_revision_digests_and_environment_revision(self):
        revision = "a" * 40
        receipt = {
            "status": "verified",
            "source_revision": revision,
            "deployment_id": "deployment-1",
            "image_id": "sha256:app",
            "control_plane_sha256": "1" * 64,
            "gateway_sha256": "2" * 64,
            "worker_sha256": "3" * 64,
            "worker_env_sha256": "4" * 64,
            "worker_id": "worker",
            "worker_lease_verified": "true",
            "pumpkin_image_id": "sha256:pumpkin",
            "pumpkin_image": f"melon-pumpkin:host-control-{revision}",
            "pumpkin_sha256": "5" * 64,
            "worker_pid": "10",
            "rollback_worker_sha256": "6" * 64,
            "rollback_worker_env_sha256": "7" * 64,
            "release_tag": f"kurbis-release-{revision}",
            "source_archive_sha256": "8" * 64,
            "host_control_pumpkin_image": "sha256:pumpkin",
            "service_image_ids": {
                "control-plane": "sha256:app",
                "gateway": "sha256:app",
                "panel": "sha256:panel",
                "waiting-pumpkin": "sha256:waiting",
                "postgres": "sha256:postgres",
            },
        }
        manifest = release.build_release_manifest(
            receipt,
            migration_manifest_sha256="9" * 64,
            environment_values={
                "BUILD_REVISION": revision,
                "BEDROCK_TLS_HOST_DIRECTORY": "/srv/kurbis/certs",
                "PANEL_BIND_ADDRESS": "127.0.0.1",
            },
        )
        release.validate_release_manifest(manifest)
        self.assertEqual(manifest["source_revision"], revision)
        self.assertEqual(manifest["migration_manifest_sha256"], "9" * 64)
        self.assertEqual(len(manifest["environment_revision"]), 64)
        self.assertEqual(len(manifest["manifest_sha256"]), 64)

    def test_release_manifest_rejects_tampering(self):
        manifest = {
            "schema": release.SCHEMA,
            "status": "verified",
            "source_revision": "a" * 40,
            "migration_manifest_sha256": "b" * 64,
            "environment_revision": "c" * 64,
            "environment_keys": [],
            "service_image_ids": {
                "control-plane": "sha256:app",
                "gateway": "sha256:app",
                "panel": "sha256:panel",
                "waiting-pumpkin": "sha256:waiting",
                "postgres": "sha256:postgres",
            },
        }
        manifest["manifest_sha256"] = release.manifest_sha256(manifest)
        release.validate_release_manifest(manifest)
        manifest["source_revision"] = "d" * 40
        with self.assertRaises(ValueError):
            release.validate_release_manifest(manifest)


if __name__ == "__main__":
    unittest.main()
