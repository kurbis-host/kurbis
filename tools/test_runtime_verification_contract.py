from pathlib import Path
import tempfile
import unittest

from runtime_verification_contract import (
    assert_worker_environment,
    command_route,
    command_request,
    read_worker_bundle,
    server_object_path,
)

COMMAND_PATH = "/api/v1/servers/{server_id}/commands"


class RuntimeVerificationContractTests(unittest.TestCase):
    def setUp(self):
        self.document = {
            "paths": {
                COMMAND_PATH: {
                    "post": {
                        "requestBody": {
                            "content": {
                                "application/json": {
                                    "schema": {"$ref": "#/components/schemas/ServerCommand"}
                                }
                            }
                        }
                    }
                }
            },
            "components": {
                "schemas": {
                    "ServerCommand": {
                        "oneOf": [
                            {
                                "type": "object",
                                "required": ["command"],
                                "properties": {"command": {"enum": ["start"]}},
                                "additionalProperties": False,
                            },
                            {
                                "type": "object",
                                "required": ["command"],
                                "properties": {"command": {"enum": ["stop"]}},
                                "additionalProperties": False,
                            },
                            {
                                "type": "object",
                                "required": ["command", "mode"],
                                "properties": {
                                    "command": {"enum": ["reset"]},
                                    "mode": {"$ref": "#/components/schemas/ResetMode"},
                                },
                                "additionalProperties": False,
                            },
                            {
                                "type": "object",
                                "required": ["command"],
                                "properties": {"command": {"enum": ["delete"]}},
                                "additionalProperties": False,
                            },
                        ]
                    },
                    "ResetMode": {"enum": ["world", "full"]},
                }
            },
        }

    def test_minimal_lifecycle_requests_follow_generated_union(self):
        for command in ("start", "stop", "delete"):
            with self.subTest(command=command):
                self.assertEqual(command_request(self.document, command), {"command": command})

    def test_rejects_command_fields_that_verifier_does_not_supply(self):
        self.document["components"]["schemas"]["ServerCommand"]["oneOf"][1]["required"].append("generation")
        with self.assertRaisesRegex(ValueError, "generation"):
            command_request(self.document, "stop")

    def test_builds_reset_request_only_from_generated_enum(self):
        self.assertEqual(
            command_request(self.document, "reset", {"mode": "world"}),
            {"command": "reset", "mode": "world"},
        )
        with self.assertRaisesRegex(ValueError, "mode"):
            command_request(self.document, "reset", {"mode": "unknown"})

    def test_rejects_removed_actions_alias(self):
        self.document["paths"]["/api/v1/servers/{server_id}/actions"] = {"post": {}}
        with self.assertRaisesRegex(ValueError, "legacy /actions"):
            command_request(self.document, "start")

    def test_rejects_missing_canonical_route_or_variant(self):
        self.document["paths"].clear()
        with self.assertRaisesRegex(ValueError, "expected one server command route"):
            command_request(self.document, "start")

    def test_resolves_referenced_variants_and_discriminator_schemas(self):
        self.document["components"]["schemas"]["ServerCommand"] = {
            "oneOf": [
                {"$ref": "#/components/schemas/StartCommand"},
                {"$ref": "#/components/schemas/StopCommand"},
                {"$ref": "#/components/schemas/DeleteCommand"},
            ]
        }
        for command in ("start", "stop", "delete"):
            self.document["components"]["schemas"][command.title() + "Command"] = {
                "type": "object",
                "required": ["command"],
                "properties": {"command": {"const": command}},
                "additionalProperties": False,
            }
        self.assertEqual(command_request(self.document, "stop"), {"command": "stop"})

    def test_selects_the_command_path_from_generated_api(self):
        self.assertEqual(command_route(self.document), COMMAND_PATH)

    def test_resolves_snapshot_objects_inside_server_namespace(self):
        root = Path("/tmp/runtime-verification")
        server = "123e4567-e89b-12d3-a456-426614174000"
        self.assertEqual(
            server_object_path(root, server, "chunks/abc"),
            root / "objects" / "servers" / server / "chunks/abc",
        )


class WorkerBundleContractTests(unittest.TestCase):
    def setUp(self):
        self.entries = {
            "WORKER_ID": "worker-1",
            "WORKER_NAME": "runtime-worker",
            "WORKER_ADDRESS": "127.0.0.1",
            "WORKER_SIGNAL_ADDRESS": "127.0.0.1",
            "WORKER_API_TOKEN": "a" * 43,
            "PUMPKIN_HOST_CONTROL_TOKEN": "b" * 43,
            "CONTROL_PLANE_URL": "https://control.example.test",
            "PUBLIC_BASE_URL": "https://panel.example.test",
            "WORKER_RUNTIME": "docker",
            "WORKER_STORAGE_MODE": "loopback_ext4",
            "WORKER_ROOT": "/var/lib/kurbis",
            "WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP": "false",
            "WORKER_MEMORY_GIB": "16",
            "WORKER_DISK_GIB": "100",
            "WORKER_CPU_MILLIS": "2000",
            "WORKER_PORT_MIN": "20000",
            "WORKER_PORT_MAX": "40000",
            "SNAPSHOT_INTERVAL_SECONDS": "300",
            "IDLE_TIMEOUT_SECONDS": "300",
        }

    def write_bundle(self, entries):
        temporary = tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", delete=False)
        with temporary:
            temporary.writelines(f"export {key}={value!r}\n" for key, value in entries.items())
        self.addCleanup(Path(temporary.name).unlink, missing_ok=True)
        return Path(temporary.name)

    def test_accepts_api_only_bundle(self):
        values = read_worker_bundle(self.write_bundle(self.entries))
        self.assertEqual(values["WORKER_API_TOKEN"], "a" * 43)

    def test_rejects_worker_database_or_operator_secrets(self):
        for key in ("WORKER_DATABASE_URL", "DATABASE_URL", "INTERNAL_TOKEN", "AWS_SECRET_ACCESS_KEY", "GOOGLE_OAUTH_CLIENT_SECRET"):
            with self.subTest(key=key):
                with self.assertRaisesRegex(ValueError, key):
                    read_worker_bundle(self.write_bundle({**self.entries, key: "secret"}))

    def test_rejects_ambient_process_database_and_provider_credentials(self):
        for key in (
            "DATABASE_URL",
            "WORKER_PROVISIONING_DATABASE_URL",
            "CONTROL_PLANE_DB_DSN",
            "S3_SECRET_KEY",
            "AWS_ROLE_ARN",
            "GCS_CREDENTIALS",
            "DISCORD_OAUTH_CLIENT_SECRET",
            "CONTROL_PLANE_INTERNAL_AUTH_TOKEN",
        ):
            with self.subTest(key=key):
                with self.assertRaisesRegex(ValueError, key):
                    assert_worker_environment({key: "secret"})

    def test_rejects_duplicate_and_malformed_worker_bundle_entries(self):
        with self.subTest(case="duplicate"):
            duplicate = {**self.entries, "WORKER_API_TOKEN": "c" * 43}
            bundle = self.write_bundle(duplicate)
            with bundle.open("a", encoding="utf-8") as output:
                output.write("export WORKER_ID='duplicate'\n")
            with self.assertRaisesRegex(ValueError, "duplicate worker bundle entry"):
                read_worker_bundle(bundle)
        with self.subTest(case="malformed token"):
            malformed = {**self.entries, "WORKER_API_TOKEN": "not-a-token"}
            with self.assertRaisesRegex(ValueError, "invalid WORKER_API_TOKEN"):
                read_worker_bundle(self.write_bundle(malformed))

    def test_rejects_non_export_bundle_lines(self):
        bundle = self.write_bundle(self.entries)
        contents = bundle.read_text(encoding="utf-8").replace(
            "export WORKER_ID=", "WORKER_ID=", 1
        )
        bundle.write_text(contents, encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "not an export"):
            read_worker_bundle(bundle)


if __name__ == "__main__":
    unittest.main(verbosity=2)
