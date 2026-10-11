#!/usr/bin/env python3
"""Canonical release-manifest helpers shared by rollout tests and the deploy gate."""
from __future__ import annotations

import hashlib
import json
import re
from typing import Any, Mapping

SCHEMA = "kurbis.release-manifest.v1"
SHA256 = re.compile(r"^[0-9a-f]{64}$")
SHA40 = re.compile(r"^[0-9a-f]{40}$")


def canonical_bytes(value: Mapping[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def manifest_sha256(value: Mapping[str, Any]) -> str:
    payload = dict(value)
    payload.pop("manifest_sha256", None)
    return hashlib.sha256(canonical_bytes(payload)).hexdigest()


def environment_revision(values: Mapping[str, str]) -> str:
    return hashlib.sha256(canonical_bytes({str(key): str(value) for key, value in sorted(values.items())})).hexdigest()


def build_release_manifest(receipt: Mapping[str, Any], *, migration_manifest_sha256: str,
                           environment_values: Mapping[str, str]) -> dict[str, Any]:
    if not SHA256.fullmatch(migration_manifest_sha256):
        raise ValueError("migration manifest hash is invalid")
    revision = str(receipt.get("source_revision", ""))
    if not SHA40.fullmatch(revision):
        raise ValueError("release source revision is not a full SHA")
    manifest: dict[str, Any] = {
        "schema": SCHEMA,
        **dict(receipt),
        "migration_manifest_sha256": migration_manifest_sha256,
        "environment_revision": environment_revision(environment_values),
        "environment_keys": sorted(str(key) for key in environment_values),
    }
    manifest["manifest_sha256"] = manifest_sha256(manifest)
    return manifest


def validate_release_manifest(value: Mapping[str, Any], *, require_verified: bool = True) -> None:
    if value.get("schema") != SCHEMA:
        raise ValueError("release manifest schema is invalid")
    revision = value.get("source_revision")
    if not isinstance(revision, str) or not SHA40.fullmatch(revision):
        raise ValueError("release manifest source revision is invalid")
    if require_verified and value.get("status") != "verified":
        raise ValueError("release manifest is not verified")
    migration_hash = value.get("migration_manifest_sha256")
    environment_hash = value.get("environment_revision")
    digest = value.get("manifest_sha256")
    if not isinstance(migration_hash, str) or not SHA256.fullmatch(migration_hash):
        raise ValueError("release manifest migration hash is invalid")
    if not isinstance(environment_hash, str) or not SHA256.fullmatch(environment_hash):
        raise ValueError("release manifest environment revision is invalid")
    if not isinstance(digest, str) or not SHA256.fullmatch(digest) or manifest_sha256(value) != digest:
        raise ValueError("release manifest digest does not match its contents")
