"""Helpers that keep runtime HTTP requests tied to the generated Rust API."""

import re
import shlex
from pathlib import Path

WORKER_BUNDLE_REQUIRED = {
    "WORKER_ID",
    "WORKER_NAME",
    "WORKER_ADDRESS",
    "WORKER_SIGNAL_ADDRESS",
    "WORKER_API_TOKEN",
    "PUMPKIN_HOST_CONTROL_TOKEN",
    "CONTROL_PLANE_URL",
    "WORKER_RUNTIME_API_PROXY_UPSTREAM",
    "WORKER_RUNTIME_API_EGRESS_NETWORK",
    "WORKER_RUNTIME_API_PROXY_IMAGE",
    "PUBLIC_BASE_URL",
    "WORKER_RUNTIME",
    "WORKER_STORAGE_MODE",
    "WORKER_ROOT",
    "WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP",
    "WORKER_MEMORY_GIB",
    "WORKER_DISK_GIB",
    "WORKER_CPU_MILLIS",
    "WORKER_PORT_MIN",
    "WORKER_PORT_MAX",
    "SNAPSHOT_INTERVAL_SECONDS",
    "IDLE_TIMEOUT_SECONDS",
}
WORKER_BUNDLE_OPTIONAL = {
    "WORKER_GATEWAY_ADDRESS",
    "WORKER_GATEWAY_BEDROCK_IP",
    "WORKER_GATEWAY_BEDROCK_PORT",
    "PUMPKIN_BINARY",
    "PUMPKIN_IMAGE",
    "KURBIS_CONTROL_PLUGIN_WASM",
}
WORKER_BUNDLE_KEYS = WORKER_BUNDLE_REQUIRED | WORKER_BUNDLE_OPTIONAL
FORBIDDEN_WORKER_ENV_PREFIXES = (
    "DATABASE",
    "PG",
    "POSTGRES",
    "OBJECT_STORE_",
    "S3_",
    "AWS_",
    "MINIO_",
    "GCS_",
    "AZURE_STORAGE_",
    "OAUTH_",
    "GOOGLE_OAUTH_",
    "DISCORD_OAUTH_",
)
FORBIDDEN_WORKER_ENV_KEYS = {
    "DATABASE_URL",
    "WORKER_DATABASE_URL",
    "WORKER_PROVISIONING_DATABASE_URL",
    "INTERNAL_TOKEN",
    "CONTROL_PLANE_INTERNAL_URL",
}


def server_object_path(root, server_id, relative_key):
    """Resolve a control-plane object key inside its server-scoped namespace."""
    return Path(root) / "objects" / "servers" / str(server_id) / relative_key


def read_worker_bundle(path):
    """Read a shell-safe bundle and reject anything beyond the worker API contract."""
    values = {}
    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        if not line.startswith("export "):
            raise ValueError(f"worker bundle line {line_number} is not an export")
        key, separator, raw = line.removeprefix("export ").partition("=")
        if not separator or key not in WORKER_BUNDLE_KEYS:
            raise ValueError(f"unexpected worker bundle entry on line {line_number}: {key}")
        if key in values:
            raise ValueError(f"duplicate worker bundle entry: {key}")
        parsed = shlex.split(raw)
        if len(parsed) != 1:
            raise ValueError(f"worker bundle entry must contain one shell value: {key}")
        values[key] = parsed[0]

    missing = WORKER_BUNDLE_REQUIRED - values.keys()
    if missing:
        raise ValueError(f"worker bundle is missing required entries: {sorted(missing)}")
    assert_worker_environment({key: value for key, value in values.items()})
    for key in ("WORKER_API_TOKEN", "PUMPKIN_HOST_CONTROL_TOKEN"):
        if not re.fullmatch(r"[A-Za-z0-9_-]{43}", values[key]):
            raise ValueError(f"worker bundle contains an invalid {key}")
    return values


def assert_worker_environment(environment):
    """Fail closed if a worker bundle or process gains operator credentials."""
    forbidden = sorted(
        key
        for key in environment
        if key.upper() in FORBIDDEN_WORKER_ENV_KEYS
        or key.upper().startswith(FORBIDDEN_WORKER_ENV_PREFIXES)
        or (
            "DATABASE" in key.upper()
            or "POSTGRES" in key.upper()
            or re.search(r"(?:^|_)DB(?:_|$)", key.upper())
        )
        or ("INTERNAL" in key.upper() and "TOKEN" in key.upper())
        or "OAUTH" in key.upper()
    )
    if forbidden:
        raise ValueError(f"worker environment contains operator-only entries: {forbidden}")


def _resolve(document, schema):
    reference = schema.get("$ref")
    if not reference:
        return schema
    prefix = "#/components/schemas/"
    if not reference.startswith(prefix):
        raise ValueError(f"unsupported OpenAPI reference: {reference}")
    target = document["components"]["schemas"]
    for part in reference[len(prefix):].split("/"):
        target = target[part.replace("~1", "/").replace("~0", "~")]
    return _resolve(document, target)


def command_route(document):
    """Find the single server lifecycle command route in the generated API."""
    paths = document.get("paths", {})
    if any(path.endswith("/actions") for path in paths):
        raise ValueError("generated API still exposes a legacy /actions route")
    matches = [
        path
        for path, operations in paths.items()
        if re.fullmatch(r"/api/v1/servers/\{[^{}]+\}/commands", path)
        and "post" in operations
    ]
    if len(matches) != 1:
        raise ValueError(f"expected one server command route in generated API, found {len(matches)}")
    return matches[0]


def command_request(document, command, fields=None):
    """Build the minimal valid command body from the generated OpenAPI schema."""
    command_path = command_route(document)

    operation = document["paths"][command_path]["post"]
    request_body = operation.get("requestBody", {})
    content = request_body.get("content", {}).get("application/json", {})
    schema = _resolve(document, content.get("schema", {}))
    variants = schema.get("oneOf") or schema.get("anyOf")
    if not variants:
        raise ValueError("generated command request is not a tagged schema union")

    for variant in variants:
        variant = _resolve(document, variant)
        properties = variant.get("properties", {})
        required = set(variant.get("required", []))
        if "command" not in required or "command" not in properties:
            raise ValueError("generated command variant must require its discriminator")
        discriminator = _resolve(document, properties.get("command", {}))
        if discriminator.get("type") not in (None, "string"):
            raise ValueError("generated command discriminator must be a string")
        accepted = discriminator.get("const", discriminator.get("enum", []))
        if isinstance(accepted, str):
            accepted = [accepted]
        if command not in accepted:
            continue

        fields = dict(fields or {})
        unknown_fields = fields.keys() - properties.keys()
        if unknown_fields:
            raise ValueError(f"generated {command!r} command rejects fields: {sorted(unknown_fields)}")
        for key, value in fields.items():
            field_schema = _resolve(document, properties[key])
            choices = field_schema.get("const", field_schema.get("enum"))
            if choices is not None and value not in (choices if isinstance(choices, list) else [choices]):
                raise ValueError(f"generated {command!r} command rejects {key}={value!r}")

        body = {"command": command, **fields}
        missing = required - body.keys()
        if missing:
            raise ValueError(
                f"{command!r} requires fields the runtime verifier does not provide: "
                f"{sorted(missing)}"
            )
        if variant.get("additionalProperties") is False and not body.keys() <= properties.keys():
            raise ValueError(f"generated {command!r} command schema rejects its discriminator")
        return body

    raise ValueError(f"generated command schema does not define {command!r}")
