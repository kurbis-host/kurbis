#!/usr/bin/env python3
"""Build and validate the immutable SQLx migration manifest.

The database records SQLx's SHA-384 checksum for every applied migration.  The
manifest adds the filename and a repository-level SHA-256 over the ordered
migration inventory, so release tooling can prove that the source tree and the
schema history describe the same migration set without reading a live database.
"""
from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path
from typing import Any

MIGRATION_NAME = re.compile(r"^(?P<version>[0-9]+)_(?P<description>[a-z0-9][a-z0-9_-]*)\.sql$")
MANIFEST_SCHEMA = "kurbis.migration-manifest.v1"
SHA384_LENGTH = 96
SHA256_LENGTH = 64


def _sha384(content: bytes) -> str:
    return hashlib.sha384(content).hexdigest()


def _canonical_json(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def migration_records(directory: Path) -> list[dict[str, object]]:
    """Return the ordered migration inventory and reject ambiguous histories."""
    records: list[dict[str, object]] = []
    versions: dict[int, str] = {}
    for path in sorted(directory.glob("*.sql")):
        match = MIGRATION_NAME.fullmatch(path.name)
        if match is None:
            raise ValueError(f"migration filename is not immutable/portable: {path.name}")
        version = int(match.group("version"))
        if version <= 0:
            raise ValueError(f"migration version must be positive: {path.name}")
        previous = versions.get(version)
        if previous is not None:
            raise ValueError(f"duplicate migration version {version}: {previous}, {path.name}")
        versions[version] = path.name
        content = path.read_bytes()
        records.append(
            {
                "version": version,
                "filename": path.name,
                "sha384": _sha384(content),
            }
        )
    records.sort(key=lambda item: (int(item["version"]), str(item["filename"])))
    if not records:
        raise ValueError(f"migration directory is empty: {directory}")
    return records


def manifest_payload(directory: Path) -> dict[str, object]:
    return {"schema": MANIFEST_SCHEMA, "migrations": migration_records(directory)}


def manifest_sha256(payload: dict[str, object]) -> str:
    return hashlib.sha256(_canonical_json(payload)).hexdigest()


def read_manifest(path: Path) -> tuple[dict[str, object], str]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"migration manifest is unreadable: {path}") from error
    if not isinstance(value, dict) or value.get("schema") != MANIFEST_SCHEMA:
        raise ValueError("migration manifest schema is invalid")
    recorded_hash = value.get("manifest_sha256")
    if not isinstance(recorded_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", recorded_hash):
        raise ValueError("migration manifest hash is missing or malformed")
    payload = {key: value[key] for key in ("schema", "migrations") if key in value}
    if manifest_sha256(payload) != recorded_hash:
        raise ValueError("migration manifest hash does not match its contents")
    return payload, recorded_hash


def validate_manifest(directory: Path, path: Path) -> str:
    expected = manifest_payload(directory)
    actual, recorded_hash = read_manifest(path)
    if actual != expected:
        raise ValueError("migration manifest does not match the tracked SQL files")
    return recorded_hash


def write_manifest(directory: Path, path: Path) -> str:
    payload = manifest_payload(directory)
    digest = manifest_sha256(payload)
    value = {**payload, "manifest_sha256": digest}
    path.write_bytes(_canonical_json(value))
    return digest


def main() -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=Path(__file__).resolve().parents[1] / "migrations")
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    manifest = args.manifest or args.directory / "MANIFEST.json"
    if args.write:
        digest = write_manifest(args.directory, manifest)
    else:
        digest = validate_manifest(args.directory, manifest)
    print(digest)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
