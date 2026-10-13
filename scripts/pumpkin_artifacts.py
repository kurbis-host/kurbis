#!/usr/bin/env python3
"""Compile the pinned Pumpkin integration on the remote CI builder.

A release sends the same framed job the control plane sends for a CI build to
`tools/ci_bubblewrap_runner.py` on the builder host, so the production runtime
and CI artifacts come from one toolchain, sandbox, and build script.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import secrets
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / "tools/ci_bubblewrap_runner.py"
ARTIFACTS = ("pumpkin", "kurbis-control.wasm")
RUST_TARGET = "x86_64-unknown-linux-gnu"
BUILD_TIMEOUT_SECONDS = 90 * 60
# Remote builder settings the control plane reads from the same deployment
# environment; a release must build with exactly the configured fixtures.
BUILDER_SETTINGS = (
    "CI_BUILDER_SSH_TARGET", "CI_BUBBLEWRAP_RUNNER", "CI_BUBBLEWRAP_PATH", "CI_BUBBLEWRAP_ROOTFS",
    "CI_BUBBLEWRAP_TOOLCHAIN", "CI_BUBBLEWRAP_WORKSPACE", "CI_BUBBLEWRAP_SOURCE_PREPARER",
    "CI_TOOLCHAIN_VERSION",
)


class ArtifactError(RuntimeError):
    pass


def runner_protocol_version() -> int:
    spec = importlib.util.spec_from_file_location("kurbis_ci_bubblewrap_runner", RUNNER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.REMOTE_PROTOCOL_VERSION


def pack(directory: Path, names: list[str] | None = None) -> bytes:
    with tempfile.TemporaryFile() as buffer:
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            for path in sorted(directory.iterdir()) if names is None else [directory / name for name in names]:
                if path.name != ".git":
                    archive.add(path, arcname=path.name)
        buffer.seek(0)
        return buffer.read()


def patched_source() -> bytes:
    with tempfile.TemporaryDirectory(prefix="kurbis-pumpkin-source-") as directory:
        source = Path(directory) / "source"
        result = subprocess.run(
            [str(ROOT / "tools/pumpkin_source.sh"), str(ROOT / "runtime/pumpkin"), str(source)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=600, check=False,
        )
        if result.returncode:
            raise ArtifactError(f"Pumpkin source preparation failed: {result.stderr.strip()[-2000:]}")
        return pack(source)


def job_header(settings: dict[str, str], source: bytes, plugin: bytes) -> dict[str, object]:
    missing = [key for key in BUILDER_SETTINGS if not settings.get(key)]
    if missing:
        raise ArtifactError(f"remote builder settings are missing: {', '.join(missing)}")
    workspace = settings["CI_BUBBLEWRAP_WORKSPACE"]
    return {
        "version": runner_protocol_version(), "mode": "compile", "job_id": secrets.token_hex(16),
        "target": RUST_TARGET, "rustc": settings["CI_TOOLCHAIN_VERSION"],
        "workspace_root": workspace, "cache_root": str(Path(workspace).parent / "compiler-cache"),
        "bwrap": settings["CI_BUBBLEWRAP_PATH"], "rootfs": settings["CI_BUBBLEWRAP_ROOTFS"],
        "toolchain": settings["CI_BUBBLEWRAP_TOOLCHAIN"],
        "source_preparer": settings["CI_BUBBLEWRAP_SOURCE_PREPARER"],
        "patch_fingerprint": "release", "capability_fingerprint": "release",
        # A release builds this repository's own pinned source, so its compiled
        # dependencies may be kept for later builds; see the runner.
        "trusted_source": True,
        "cache_max_bytes": int(settings.get("CI_BUILD_CACHE_MAX_BYTES") or 8 * 1024 ** 3),
        "source_len": len(source), "plugin_len": len(plugin), "timeout_seconds": BUILD_TIMEOUT_SECONDS,
        # The runner validates these as the builder's fixed workspace contract.
        "profile_memory_bytes": 10 * 1024 ** 3, "profile_workspace_bytes": 5 * 1024 ** 3,
        "profile_workspace_inodes": 1_310_720, "profile_concurrency": 2,
    }


def read_exact(stream, length: int) -> bytes:
    data = stream.read(length)
    if len(data) != length:
        raise ArtifactError("remote builder closed the stream before sending a complete artifact")
    return data


def build(settings: dict[str, str], output: Path) -> dict[str, str]:
    """Build both runtime artifacts into `output` and return their SHA-256 digests."""
    source = patched_source()
    plugin = pack(ROOT / "plugins/kurbis-control", ["Cargo.toml", "Cargo.lock", "src"])
    header = job_header(settings, source, plugin)
    process = subprocess.Popen(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=30", "-T", "--",
         settings["CI_BUILDER_SSH_TARGET"], "unshare", "-Urm", "python3", settings["CI_BUBBLEWRAP_RUNNER"]],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    try:
        process.stdin.write(json.dumps(header, separators=(",", ":")).encode() + b"\n" + source + plugin)
        process.stdin.close()
        line = process.stdout.readline()
        try:
            result = json.loads(line)
        except json.JSONDecodeError as error:
            detail = process.stderr.read().decode("utf-8", "replace").strip()[-2000:]
            raise ArtifactError(f"remote builder returned no result: {detail}") from error
        if not result.get("ok"):
            raise ArtifactError(f"remote Pumpkin build failed: {str(result.get('stderr', '')).strip()[-4000:]}")
        reported = {item["name"]: item for item in result.get("artifacts", [])}
        if list(reported) != list(ARTIFACTS):
            raise ArtifactError("remote builder returned an unexpected artifact set")
        output.mkdir(mode=0o700, parents=True, exist_ok=True)
        digests = {}
        for name in ARTIFACTS:
            data = read_exact(process.stdout, int(reported[name]["size"]))
            digests[name] = hashlib.sha256(data).hexdigest()
            if digests[name] != reported[name]["sha256"]:
                raise ArtifactError(f"remote artifact {name} failed its SHA-256 check")
            (output / name).write_bytes(data)
        if process.wait(timeout=60):
            raise ArtifactError("remote builder exited with an error after sending artifacts")
        return digests
    finally:
        if process.poll() is None:
            process.kill()


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} SETTINGS.json OUTPUT_DIRECTORY", file=sys.stderr)
        return 2
    try:
        digests = build(json.loads(Path(sys.argv[1]).read_text(encoding="utf-8")), Path(sys.argv[2]))
    except ArtifactError as error:
        print(error, file=sys.stderr)
        return 1
    print(json.dumps(digests, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
