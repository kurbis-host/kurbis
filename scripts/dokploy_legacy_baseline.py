#!/usr/bin/env python3
"""Explicit, one-time attestation of the unlabelled respond5 production inventory.

This is deliberately separate from the strict SHA-provenance verifier. It never
labels/rebuilds/restarts a service; it only records an operator-reviewed snapshot.
"""
from __future__ import annotations

import argparse
import base64
import fcntl
import hashlib
import json
import os
import re
import stat
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Sequence

ROOT = Path("/srv/kurbis")
CHECKOUT = Path("/etc/dokploy/compose/kurbis-hoster-fbikvd/code")
ENV_FILE = ROOT / "worker.env"
WORKER = Path("/srv/kurbis-hoster/target/release/worker")
WORKER_UNIT = Path("/etc/systemd/system/kurbis-worker.service")
WORKER_LAUNCHER = Path("/usr/local/bin/kurbis-worker")
PROJECT = "kurbis-hoster-fbikvd"
COMPOSE_ID = "KZyAZl_v1hgoqWi-W77ot"
LEGACY_REVISION_CLAIM = "f6b05712bb130747060e5cfa049d84ed27020835"
CURRENT_BASELINE_REVISION = "2f94cc79602d45cfe6f91c629ffc4df48a3674cf"
LEGACY_BASELINE_SCHEMA = "kurbis.respond5.legacy-baseline.v1"
CURRENT_BASELINE_SCHEMA = "kurbis.respond5.current-baseline.v1"
PUMPKIN_LEGACY = "melon-pumpkin:host-control"
SERVICES = ("control-plane", "gateway", "panel", "waiting-pumpkin", "postgres")
SERVICE_PAYLOADS = {
    "control-plane": {"control_plane": "/usr/local/bin/control-plane"},
    "gateway": {"gateway": "/usr/local/bin/gateway"},
    "panel": {},
    "waiting-pumpkin": {
        "pumpkin": "/usr/local/bin/pumpkin",
        "wasm": "/usr/local/share/kurbis-control.wasm",
        "hosted_entrypoint": "/usr/local/bin/hosted-entrypoint",
        "build_revision": "/usr/share/doc/kurbis/build-revision",
        "upstream_revision": "/usr/share/doc/pumpkin/REVISION",
    },
    "postgres": {},
}
AUDIT = ROOT / "legacy-baseline-inventory.json"
CURRENT_AUDIT = ROOT / "migration-safe-baseline-inventory.json"
LOCK = Path("/run/lock/kurbis-production-deploy.lock")
OPERATION_LOCK = Path("/run/lock/kurbis-production-deploy-operation.lock")
CHECKOUT_BACKUP = ROOT / "deploy-checkout-overrides"
ENV_FILE_CHECKOUT = CHECKOUT / ".env"
WORKER_MIGRATION = ROOT / "legacy-baseline-worker-migration.json"
WORKER_BACKUP = ROOT / "legacy-baseline-worker-backups"
COMPOSE_ENV_BACKUPS = ROOT / "legacy-baseline-environment"
CURRENT_COMPOSE_ENV_BACKUPS = ROOT / "migration-safe-baseline-environment"


class BaselineError(RuntimeError):
    pass


def run(args: Sequence[str], *, input_bytes: bytes | None = None) -> bytes:
    result = subprocess.run(list(args), input=input_bytes, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode:
        detail = result.stderr.decode("utf-8", "replace").strip() or result.stdout.decode("utf-8", "replace").strip()
        raise BaselineError(f"{args[0]} failed: {detail}")
    return result.stdout


def run_text(args: Sequence[str], *, input_text: str | None = None) -> str:
    return run(args, input_bytes=input_text.encode() if input_text is not None else None).decode("utf-8").strip()


def root_only() -> None:
    if os.geteuid() != 0:
        raise BaselineError("legacy inventory bootstrap requires root on respond5")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def valid_sha(value: str) -> bool:
    return bool(re.fullmatch(r"[0-9a-f]{40}", value))


def checkout_status(expected_head: str = LEGACY_REVISION_CLAIM) -> list[dict[str, str]]:
    raw = run(["git", "-C", str(CHECKOUT), "status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignored"])
    records = [part for part in raw.split(b"\0") if part]
    allowed = {"compose.yaml", "web/tsconfig.json"} if expected_head == LEGACY_REVISION_CLAIM else set()
    parsed = []
    for record in records:
        if len(record) < 4 or record[2:3] != b" ":
            raise BaselineError(f"unexpected malformed Git status record: {record!r}")
        code = record[:2].decode("ascii")
        path = record[3:].decode("utf-8", "surrogateescape")
        if code == "!!" and path == ".env":
            parsed.append({"code": code, "path": path})
            continue
        if code not in {" M", "M ", "MM"} or path not in allowed:
            raise BaselineError(f"legacy inventory has an unexpected checkout change: {path}")
        parsed.append({"code": code, "path": path})
    head = run_text(["git", "-C", str(CHECKOUT), "rev-parse", "HEAD"])
    if head != expected_head:
        raise BaselineError("respond5 checkout does not match the expected exact source commit")
    overrides = {}
    for path in sorted(allowed):
        file_path = CHECKOUT / path
        overrides[path] = sha256_file(file_path) if file_path.is_file() else None
    return [{"code": item["code"], "path": item["path"], **({"sha256": overrides[item["path"]]} if item["path"] in overrides else {})} for item in parsed]


def image_info(image_id: str) -> dict[str, Any]:
    records = json.loads(run_text(["docker", "image", "inspect", image_id]))
    if len(records) != 1:
        raise BaselineError(f"Docker did not resolve exactly one image: {image_id}")
    image = records[0]
    config = image.get("Config") or {}
    return {
        "image_id": image.get("Id"),
        "created": image.get("Created"),
        "revision_label": (config.get("Labels") or {}).get("org.opencontainers.image.revision"),
        "rootfs_diff_ids": image.get("RootFS", {}).get("Layers", []),
        "entrypoint": config.get("Entrypoint"),
        "cmd": config.get("Cmd"),
        "working_dir": config.get("WorkingDir"),
        "user": config.get("User"),
    }


def image_payload_hashes(image_id: str, service: str) -> dict[str, dict[str, Any] | None]:
    paths = SERVICE_PAYLOADS[service]
    if not paths:
        return {}
    tempdir = Path(tempfile.mkdtemp(prefix="kurbis-legacy-inventory-"))
    container_id = run_text(["docker", "create", "--entrypoint", "/bin/true", image_id])
    result: dict[str, dict[str, Any] | None] = {}
    try:
        for name, source in paths.items():
            destination = tempdir / name
            completed = subprocess.run(
                ["docker", "cp", f"{container_id}:{source}", str(destination)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            if completed.returncode:
                if name in {"build_revision"}:
                    result[name] = None
                    continue
                detail = completed.stderr.decode("utf-8", "replace").strip()
                raise BaselineError(f"required legacy image payload is absent: {service}/{name}: {detail}")
            content = destination.read_bytes()
            result[name] = {"sha256": sha256_bytes(content), "size": len(content)}
            if name == "upstream_revision":
                result[name]["value"] = content.decode("utf-8", "strict").strip()
    finally:
        subprocess.run(["docker", "rm", "-f", container_id], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        for path in tempdir.iterdir():
            path.unlink(missing_ok=True)
        tempdir.rmdir()
    return result


def collect_inventory(*, allowed_worker_images: set[str] | None = None,
                      expected_checkout_head: str = LEGACY_REVISION_CLAIM,
                      baseline_schema: str = LEGACY_BASELINE_SCHEMA) -> dict[str, Any]:
    root_only()
    allowed_worker_images={PUMPKIN_LEGACY} if allowed_worker_images is None else allowed_worker_images
    if CHECKOUT.is_symlink() or not (CHECKOUT / ".git").is_dir() or WORKER.is_symlink() or not WORKER.is_file():
        raise BaselineError("respond5 source checkout or worker path is unsafe")
    if WORKER_UNIT.is_symlink() or WORKER_LAUNCHER.is_symlink() or not WORKER_UNIT.is_file() or not WORKER_LAUNCHER.is_file():
        raise BaselineError("respond5 worker service files are missing or symlinked")
    unit_text=WORKER_UNIT.read_text(encoding="utf-8")
    launcher_lines=WORKER_LAUNCHER.read_text(encoding="utf-8").splitlines()
    if "ExecStart=/usr/local/bin/kurbis-worker" not in unit_text or f"exec {WORKER}" not in launcher_lines:
        raise BaselineError("worker systemd unit or launcher differs from the attested executable path")

    services: dict[str, Any] = {}
    for service in SERVICES:
        ids = run_text([
            "docker", "ps", "-aq",
            "--filter", f"label=com.docker.compose.project={PROJECT}",
            "--filter", f"label=com.docker.compose.service={service}",
        ]).splitlines()
        if len(ids) != 1:
            raise BaselineError(f"legacy inventory requires exactly one {service} container")
        inspected = json.loads(run_text(["docker", "inspect", ids[0]]))[0]
        image_id = inspected.get("Image")
        image = image_info(image_id)
        state = inspected.get("State", {})
        services[service] = {
            "container_id": inspected.get("Id"),
            "container_name": inspected.get("Name", "").lstrip("/"),
            "image_id": image_id,
            "image_tag": (inspected.get("Config") or {}).get("Image"),
            "status": state.get("Status"),
            "health": (state.get("Health") or {}).get("Status", "none"),
            "image": image,
            "payloads": image_payload_hashes(image_id, service),
            "ports": inspected.get("HostConfig", {}).get("PortBindings", {}),
        }
        label = image.get("revision_label")
        if baseline_schema == LEGACY_BASELINE_SCHEMA:
            if label is not None:
                raise BaselineError(f"expected legacy-unlabelled image; found revision label on {service}")
        elif baseline_schema == CURRENT_BASELINE_SCHEMA:
            expected_label = CURRENT_BASELINE_REVISION if service in {"panel", "waiting-pumpkin"} else None
            if label != expected_label:
                raise BaselineError(f"current migration-safe baseline has unexpected {service} revision label")
        else:
            raise BaselineError("unsupported baseline schema")
        if state.get("Status") != "running" or (service != "gateway" and services[service]["health"] != "healthy"):
            raise BaselineError(f"baseline production service is not healthy: {service}")

    if services["control-plane"]["image_id"] != services["gateway"]["image_id"]:
        raise BaselineError("legacy control-plane and gateway do not share an exact image digest")
    if services["control-plane"]["image_tag"] != services["gateway"]["image_tag"]:
        raise BaselineError("legacy control-plane/gateway image tags differ")

    worker_env_lines = ENV_FILE.read_text(encoding="utf-8").splitlines()
    values = {}
    for line in worker_env_lines:
        match = re.fullmatch(r"\s*export\s+(WORKER_ID|PUMPKIN_IMAGE)\s*=\s*(.*?)\s*", line)
        if match:
            if match.group(1) in values:
                raise BaselineError(f"duplicate worker {match.group(1)} entry")
            values[match.group(1)] = match.group(2).strip("\"'")
    worker_id = values.get("WORKER_ID", "")
    try:
        worker_id = str(uuid.UUID(worker_id))
    except ValueError as error:
        raise BaselineError("invalid protected worker identity") from error
    if values.get("PUMPKIN_IMAGE") not in allowed_worker_images:
        raise BaselineError("respond5 worker setting differs from legacy baseline or protected migration journal")
    env_stat = ENV_FILE.stat()
    if ENV_FILE.is_symlink() or env_stat.st_uid != 0 or stat.S_IMODE(env_stat.st_mode) != 0o600:
        raise BaselineError("worker env file must remain root-owned mode 0600")

    pid = int(run_text(["systemctl", "show", "--property=MainPID", "--value", "kurbis-worker.service"]))
    service_state = run_text(["systemctl", "is-active", "kurbis-worker.service"])
    allowed_states = {"active"} if baseline_schema == LEGACY_BASELINE_SCHEMA else {"active", "activating"}
    if service_state not in allowed_states or pid <= 1:
        raise BaselineError("Kurbis worker systemd service is not in an inspectable state")
    process_paths = []
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            executable = (proc / "exe").resolve()
        except (FileNotFoundError, PermissionError, OSError):
            continue
        if executable.name == "worker":
            process_paths.append(str(executable))
    if process_paths != [str(WORKER.resolve())]:
        raise BaselineError(f"legacy production requires exactly one worker executable: {process_paths}")
    process_env = {}
    for entry in Path(f"/proc/{pid}/environ").read_bytes().split(b"\0"):
        if b"=" in entry:
            key, value = entry.split(b"=", 1)
            if key.decode("utf-8", "replace") == "PUMPKIN_IMAGE":
                process_env["PUMPKIN_IMAGE"] = value.decode("utf-8", "replace")
    if process_env.get("PUMPKIN_IMAGE") not in allowed_worker_images:
        raise BaselineError("running worker process differs from legacy baseline or protected migration journal")
    legacy_worker_image = image_info(PUMPKIN_LEGACY)
    legacy_worker_image["tag"] = PUMPKIN_LEGACY
    legacy_worker_image["payloads"] = image_payload_hashes(legacy_worker_image["image_id"], "waiting-pumpkin")
    if legacy_worker_image.get("revision_label") is not None:
        raise BaselineError("expected the protected worker Pumpkin image to be unlabeled")

    postgres_id = services["postgres"]["container_id"]
    sql = (
        "SELECT wc.worker_id::text,"
        "(wc.revoked_at IS NULL AND wc.lease_expires_at > now()),"
        "(w.heartbeat_at > now()-interval '30 seconds'),"
        "(SELECT count(*) FROM worker_credentials c WHERE c.worker_id=wc.worker_id "
        "AND c.revoked_at IS NULL AND c.lease_expires_at > now()) "
        f"FROM worker_credentials wc JOIN workers w ON w.id=wc.worker_id WHERE wc.worker_id='{worker_id}'::uuid AND wc.revoked_at IS NULL;"
    )
    lease_result = run_text([
        "docker", "exec", "-i", postgres_id, "sh", "-lc",
        'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -At -F "|" -f -',
    ], input_text=sql)
    expected_lease = f"{worker_id}|t|t|1"
    if baseline_schema == LEGACY_BASELINE_SCHEMA and lease_result != expected_lease:
        raise BaselineError("legacy worker lease is not uniquely active and renewing")
    if baseline_schema == CURRENT_BASELINE_SCHEMA and not re.fullmatch(re.escape(worker_id) + r"\|[tf]\|[tf]\|[01]", lease_result):
        raise BaselineError("current baseline worker lease is not uniquely owned")

    compose_env={}
    if ENV_FILE_CHECKOUT.is_symlink() or not ENV_FILE_CHECKOUT.is_file():
        raise BaselineError("Dokploy source .env is missing or symlinked")
    for line in ENV_FILE_CHECKOUT.read_text(encoding="utf-8").splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            key,value=line.split("=",1)
            if key in {"BUILD_REVISION","BEDROCK_TLS_HOST_DIRECTORY","PANEL_BIND_ADDRESS"}:
                compose_env[key]=value.strip().strip("\"'")
    return {
        "compose_id": COMPOSE_ID,
        "project": PROJECT,
        "services": services,
        "source": {
            "checkout_head": run_text(["git", "-C", str(CHECKOUT), "rev-parse", "HEAD"]),
            "modified_inputs": checkout_status(expected_checkout_head),
            "build_revision_env_present": "BUILD_REVISION" in compose_env,
            "compose_env_file_sha256": sha256_file(ENV_FILE_CHECKOUT),
            "compose_env_file_mode": stat.S_IMODE(ENV_FILE_CHECKOUT.stat().st_mode),
            "compose_env_file_uid": ENV_FILE_CHECKOUT.stat().st_uid,
            "compose_env_file_gid": ENV_FILE_CHECKOUT.stat().st_gid,
            "safe_compose_settings": compose_env,
        },
        "worker": {
            "service": "kurbis-worker.service",
            "service_active": service_state in {"active", "activating"},
            "main_pid": pid,
            "worker_id": worker_id,
            "configured_pumpkin_image": values["PUMPKIN_IMAGE"],
            "process_pumpkin_image": process_env["PUMPKIN_IMAGE"],
            "legacy_pumpkin_image": legacy_worker_image,
            "worker_env_mode": stat.S_IMODE(env_stat.st_mode),
            "worker_env_uid": env_stat.st_uid,
            "worker_env_sha256": sha256_file(ENV_FILE),
            "worker_path": str(WORKER),
            "worker_mode": stat.S_IMODE(WORKER.stat().st_mode),
            "worker_uid": WORKER.stat().st_uid,
            "worker_gid": WORKER.stat().st_gid,
            "worker_sha256": sha256_file(WORKER),
            "worker_process_sha256": sha256_file(Path(f"/proc/{pid}/exe")),
            "worker_process_count": len(process_paths),
            "service_unit_sha256": sha256_file(WORKER_UNIT),
            "launcher_sha256": sha256_file(WORKER_LAUNCHER),
            "lease": lease_result,
        },
        "provenance_status": "legacy-unproven" if baseline_schema == LEGACY_BASELINE_SCHEMA else "migration-safe-unproven",
    }


def canonical_hash(value: dict[str, Any]) -> str:
    return sha256_bytes((json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode())


def validate_manifest(manifest: dict[str, Any], current: dict[str, Any]) -> str:
    if not isinstance(manifest, dict):
        raise BaselineError("baseline manifest is not an object")
    schema = manifest.get("schema")
    if schema not in {LEGACY_BASELINE_SCHEMA, CURRENT_BASELINE_SCHEMA}:
        raise BaselineError("baseline manifest schema is unsupported")
    claim = LEGACY_REVISION_CLAIM if schema == LEGACY_BASELINE_SCHEMA else CURRENT_BASELINE_REVISION
    expected_status = "legacy-unproven" if schema == LEGACY_BASELINE_SCHEMA else "migration-safe-unproven"
    if manifest.get("source_revision_claim") != claim or current.get("source", {}).get("checkout_head") != claim:
        raise BaselineError("baseline source claim does not match the reviewed checkout")
    operator=manifest.get("operator");reason=manifest.get("reason")
    if not isinstance(operator,str) or not operator.strip() or not isinstance(reason,str) or len(reason.strip())<20:
        raise BaselineError("explicit operator name and meaningful baseline reason are required")
    expected=manifest.get("expected_inventory")
    if not isinstance(expected, dict):
        raise BaselineError("baseline manifest is missing its expected inventory")
    if schema == CURRENT_BASELINE_SCHEMA:
        # A restart-looping pre-release worker changes PID and lease liveness while
        # the app/image/container identity remains the attested rollback boundary.
        expected_compare = json.loads(json.dumps(expected))
        current_compare = json.loads(json.dumps(current))
        for inventory in (expected_compare, current_compare):
            inventory.get("worker", {}).pop("main_pid", None)
            inventory.get("worker", {}).pop("lease", None)
        if expected_compare != current_compare:
            raise BaselineError("live inventory differs from exact migration-safe baseline")
    elif expected != current:
        raise BaselineError("live inventory differs from exact operator-reviewed manifest")
    if current.get("provenance_status") != expected_status: raise BaselineError("baseline provenance status is inconsistent")
    if current.get("compose_id")!=COMPOSE_ID or current.get("project")!=PROJECT:
        raise BaselineError("legacy manifest targets the wrong Compose stack")
    source=current.get("source",{})
    if source.get("checkout_head") != claim:
        raise BaselineError("baseline checkout state is not the reviewed source")
    if schema == LEGACY_BASELINE_SCHEMA and source.get("build_revision_env_present") is not False:
        raise BaselineError("legacy checkout unexpectedly contains BUILD_REVISION")
    if schema == CURRENT_BASELINE_SCHEMA and source.get("safe_compose_settings", {}).get("BUILD_REVISION") != claim:
        raise BaselineError("current baseline BUILD_REVISION does not match its source claim")
    if not re.fullmatch(r"[0-9a-f]{64}",str(source.get("compose_env_file_sha256",""))):
        raise BaselineError("legacy source environment digest is invalid")
    allowed={"compose.yaml","web/tsconfig.json"}
    for item in source.get("modified_inputs",[]):
        if item.get("path")==".env" and item.get("code")=="!!":continue
        if item.get("path") not in allowed or item.get("code") not in {" M","M ","MM"} or not re.fullmatch(r"[0-9a-f]{64}",str(item.get("sha256",""))):
            raise BaselineError("baseline checkout includes a modified/untracked build input outside reviewed files")
    if not any(item.get("path")==".env" and item.get("code")=="!!" for item in source.get("modified_inputs",[])):
        raise BaselineError("baseline checkout requires the ignored Dokploy .env exception")
    services=current.get("services",{})
    if set(services)!=set(SERVICES):raise BaselineError("legacy manifest service inventory is incomplete")
    for name,item in services.items():
        cid=str(item.get("container_id",""));image_id=str(item.get("image_id",""));image=item.get("image",{})
        if not re.fullmatch(r"[0-9a-f]{64}",cid) or not re.fullmatch(r"sha256:[0-9a-f]{64}",image_id):
            raise BaselineError(f"legacy inventory ID/digest malformed: {name}")
        expected_label = None if schema == LEGACY_BASELINE_SCHEMA or name in {"control-plane", "gateway", "postgres"} else claim
        if image.get("image_id") != image_id or image.get("revision_label") != expected_label:
            if schema == LEGACY_BASELINE_SCHEMA:
                raise BaselineError(f"legacy image metadata is not exact/unlabelled: {name}")
            raise BaselineError(f"baseline image metadata is not exact for {name}")
        if any(not re.fullmatch(r"sha256:[0-9a-f]{64}",str(layer)) for layer in image.get("rootfs_diff_ids",[])):
            raise BaselineError(f"legacy image layer inventory is malformed: {name}")
        if item.get("status")!="running" or (name!="gateway" and item.get("health")!="healthy"):
            raise BaselineError(f"legacy service was not healthy at capture time: {name}")
    if services["control-plane"]["image_id"]!=services["gateway"]["image_id"]:
        raise BaselineError("legacy control-plane/gateway images differ")
    panel_ports=services["panel"].get("ports",{})
    if not any(binding.get("HostIp")=="127.0.0.1" for bindings in panel_ports.values() for binding in bindings):
        raise BaselineError("legacy panel was not bound to localhost")
    for name,item in services.items():
        for key,payload in item.get("payloads",{}).items():
            if payload is None and key=="build_revision":continue
            if not re.fullmatch(r"[0-9a-f]{64}",str(payload.get("sha256",""))) or not isinstance(payload.get("size"),int):
                raise BaselineError(f"legacy image payload digest is invalid: {name}/{key}")
    worker=current.get("worker",{})
    try:worker_id=str(uuid.UUID(worker.get("worker_id","")))
    except ValueError as error:raise BaselineError("legacy worker identity is invalid") from error
    if worker.get("worker_id")!=worker_id or worker.get("configured_pumpkin_image")!=PUMPKIN_LEGACY or worker.get("process_pumpkin_image")!=PUMPKIN_LEGACY:
        raise BaselineError("legacy worker tag/identity differs from expected bootstrap target")
    if worker.get("worker_process_count")!=1 or worker.get("service_active") is not True or worker.get("service")!="kurbis-worker.service":
        raise BaselineError("legacy worker service/process inventory is unsafe")
    for key in ("worker_sha256","worker_process_sha256","service_unit_sha256","launcher_sha256","legacy_pumpkin_image"):
        if key=="legacy_pumpkin_image":continue
        if not re.fullmatch(r"[0-9a-f]{64}",str(worker.get(key,""))):raise BaselineError(f"legacy worker digest is invalid: {key}")
    if worker.get("worker_sha256") != worker.get("worker_process_sha256"):
        raise BaselineError("baseline worker executable is inconsistent")
    lease_pattern = re.escape(worker_id) + (r"\|t\|t\|1" if schema == LEGACY_BASELINE_SCHEMA else r"\|[tf]\|[tf]\|[01]")
    if not re.fullmatch(lease_pattern, str(worker.get("lease", ""))):
        raise BaselineError("baseline worker lease is inconsistent")
    legacy_image=worker.get("legacy_pumpkin_image",{})
    if legacy_image.get("tag")!=PUMPKIN_LEGACY or not re.fullmatch(r"sha256:[0-9a-f]{64}",str(legacy_image.get("image_id",""))) or legacy_image.get("revision_label") is not None:
        raise BaselineError("legacy Pumpkin image identity is invalid or unexpectedly labeled")
    if schema == LEGACY_BASELINE_SCHEMA:
        for key in ("pumpkin", "wasm", "hosted_entrypoint"):
            if legacy_image.get("payloads", {}).get(key) != services["waiting-pumpkin"].get("payloads", {}).get(key):
                raise BaselineError(f"legacy worker/waiting Pumpkin payload differs: {key}")
    waiting_revision = services["waiting-pumpkin"].get("payloads", {}).get("build_revision")
    if schema == LEGACY_BASELINE_SCHEMA and waiting_revision is not None:
        raise BaselineError("legacy waiting image unexpectedly contains a Kurbis build revision")
    if schema == CURRENT_BASELINE_SCHEMA and waiting_revision is None:
        raise BaselineError("current waiting image is missing its embedded BUILD_REVISION")
    return canonical_hash({"schema":manifest["schema"],"source_revision_claim":manifest["source_revision_claim"],
                           "operator":operator,"reason":reason,"expected_inventory":expected})


def current_baseline_manifest(inventory: dict[str, Any], *, operator: str = "Kurbis production release operator") -> dict[str, Any]:
    """Build a one-shot manifest for the migration-safe 1..13 live baseline."""
    if inventory.get("source", {}).get("checkout_head") != CURRENT_BASELINE_REVISION:
        raise BaselineError("current baseline must be captured from the migration-safe 2f94 checkout")
    if inventory.get("provenance_status") != "migration-safe-unproven":
        raise BaselineError("current baseline inventory has the wrong provenance status")
    manifest = {
        "schema": CURRENT_BASELINE_SCHEMA,
        "source_revision_claim": CURRENT_BASELINE_REVISION,
        "operator": operator,
        "reason": "Exact live migration-safe 1..13 app/worker baseline for rollback after additive migrations 0014 and 0015; never restore the historical f6 release.",
        "expected_inventory": inventory,
    }
    validate_manifest(manifest, inventory)
    return manifest


def require_lock(token: str) -> None:
    if not re.fullmatch(r"[0-9a-f]{32}", token or "") or LOCK.is_symlink():
        raise BaselineError("invalid production rollout lock token")
    owner = LOCK / "owner"
    if not owner.is_file() or owner.is_symlink() or owner.read_text(encoding="ascii").strip() != token:
        raise BaselineError("production rollout lock ownership mismatch")


class OperationLock:
    def __enter__(self):
        root_only()
        OPERATION_LOCK.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
        fd=os.open(OPERATION_LOCK,os.O_CREAT|os.O_RDWR|getattr(os,"O_NOFOLLOW",0),0o600)
        metadata=os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid!=0 or stat.S_IMODE(metadata.st_mode)!=0o600:
            os.close(fd)
            raise BaselineError("operation lock must be a root-owned regular mode-0600 file")
        fcntl.flock(fd,fcntl.LOCK_EX)
        self.fd=fd
        return self

    def __exit__(self,*exc):
        fcntl.flock(self.fd,fcntl.LOCK_UN)
        os.close(self.fd)


def audit_path(manifest: dict[str, Any]) -> Path:
    return CURRENT_AUDIT if manifest.get("schema") == CURRENT_BASELINE_SCHEMA else AUDIT


def compose_env_backup_root(manifest_or_schema: dict[str, Any] | str) -> Path:
    schema = manifest_or_schema if isinstance(manifest_or_schema, str) else manifest_or_schema.get("schema")
    return CURRENT_COMPOSE_ENV_BACKUPS if schema == CURRENT_BASELINE_SCHEMA else COMPOSE_ENV_BACKUPS


def load_attestation(manifest: dict[str, Any]) -> dict[str, Any]:
    expected=manifest.get("expected_inventory") if isinstance(manifest,dict) else None
    if not isinstance(expected,dict):raise BaselineError("legacy manifest is missing expected_inventory")
    manifest_sha=validate_manifest(manifest,expected)
    audit = audit_path(manifest)
    if not audit.is_file() or audit.is_symlink():
        raise BaselineError("explicit baseline has not been bootstrapped")
    info=audit.stat()
    if info.st_uid!=0 or stat.S_IMODE(info.st_mode)!=0o600:
        raise BaselineError("legacy baseline audit record must be root-owned mode 0600")
    record=json.loads(audit.read_text(encoding="utf-8"))
    if record.get("manifest_sha256")!=manifest_sha or record.get("inventory")!=expected:
        raise BaselineError("legacy audit record differs from operator-reviewed manifest")
    backup=record.get("compose_env_backup",{})
    if backup.get("sha256")!=expected.get("source",{}).get("compose_env_file_sha256"):
        raise BaselineError("protected baseline environment digest differs from manifest")
    backup_path=Path(backup.get("path",""))
    if backup_path.is_symlink() or not backup_path.is_file() or backup_path.parent not in {COMPOSE_ENV_BACKUPS, CURRENT_COMPOSE_ENV_BACKUPS}:
        raise BaselineError("baseline Dokploy .env backup is missing or unsafe")
    if backup_path.stat().st_uid!=0 or stat.S_IMODE(backup_path.stat().st_mode)!=0o600 or sha256_file(backup_path)!=backup.get("sha256"):
        raise BaselineError("baseline Dokploy .env backup failed integrity validation")
    return record


def worker_values() -> dict[str, str]:
    values={}
    for line in ENV_FILE.read_text(encoding="utf-8").splitlines():
        match=re.fullmatch(r"\s*export\s+(WORKER_ID|PUMPKIN_IMAGE)\s*=\s*(.*?)\s*",line)
        if match:
            if match.group(1) in values:raise BaselineError(f"duplicate worker {match.group(1)} entry")
            values[match.group(1)]=match.group(2).strip("\"'")
    return values


def worker_process_environment(pid: int) -> dict[str, str]:
    result={}
    for entry in Path(f"/proc/{pid}/environ").read_bytes().split(b"\0"):
        if b"=" not in entry:continue
        key,value=entry.split(b"=",1)
        result[key.decode("utf-8","replace")]=value.decode("utf-8","replace")
    return result


def atomic_json(path: Path, value: dict[str, Any], mode: int) -> None:
    atomic_file(path,(json.dumps(value,sort_keys=True,indent=2)+"\n").encode(),mode,0,0)


def migration_record(manifest: dict[str, Any]) -> dict[str, Any] | None:
    if not WORKER_MIGRATION.exists():return None
    if WORKER_MIGRATION.is_symlink():raise BaselineError("worker migration record is a symlink")
    info=WORKER_MIGRATION.stat()
    if info.st_uid!=0 or stat.S_IMODE(info.st_mode)!=0o600:raise BaselineError("worker migration record must be root-owned mode 0600")
    record=json.loads(WORKER_MIGRATION.read_text(encoding="utf-8"))
    attestation=load_attestation(manifest)
    if record.get("baseline_manifest_sha256")!=attestation["manifest_sha256"]:
        raise BaselineError("worker migration record belongs to another baseline")
    return record


def current_service_inventory() -> dict[str, dict[str, Any]]:
    result = {}
    for service in SERVICES:
        ids = run_text([
            "docker", "ps", "-aq",
            "--filter", f"label=com.docker.compose.project={PROJECT}",
            "--filter", f"label=com.docker.compose.service={service}",
        ]).splitlines()
        if len(ids) != 1:
            raise BaselineError(f"production service is not unique: {service}")
        inspected = json.loads(run_text(["docker", "inspect", ids[0]]))[0]
        result[service] = {
            "container_id": inspected["Id"], "image_id": inspected["Image"],
            "image_tag": inspected.get("Config", {}).get("Image"),
            "status": inspected.get("State", {}).get("Status"),
            "health": inspected.get("State", {}).get("Health", {}).get("Status", "none"),
        }
    return result


def worker_lease_snapshot(worker_id: str, postgres_container_id: str, *, require_fresh: bool = True) -> dict[str, str]:
    timestamp_format="YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\""
    sql=("SELECT wc.worker_id::text,(wc.revoked_at IS NULL AND wc.lease_expires_at > now()),"
         "(w.heartbeat_at > now()-interval '30 seconds'),(SELECT count(*) FROM worker_credentials c "
         f"WHERE c.worker_id=wc.worker_id AND c.revoked_at IS NULL AND c.lease_expires_at > now()),"
         f"to_char(w.heartbeat_at AT TIME ZONE 'UTC','{timestamp_format}'),"
         f"to_char(now() AT TIME ZONE 'UTC','{timestamp_format}') FROM worker_credentials wc "
         f"JOIN workers w ON w.id=wc.worker_id WHERE wc.worker_id='{worker_id}'::uuid AND wc.revoked_at IS NULL;")
    got=run_text(["docker","exec","-i",postgres_container_id,"sh","-lc",
                  'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -At -F "|" -f -'],input_text=sql)
    fields=got.split("|")
    if len(fields)!=6 or fields[0]!=worker_id or fields[1]!="t" or fields[3]!="1" or (require_fresh and fields[2]!="t"):
        raise BaselineError("worker lease is not uniquely active and renewing")
    return {"raw":"|".join(fields[:4]),"heartbeat_at":fields[4],"database_now":fields[5]}


def worker_lease(worker_id: str, postgres_container_id: str) -> str:
    return worker_lease_snapshot(worker_id,postgres_container_id)["raw"]


def systemd_worker_pid() -> int:
    return int(run_text(["systemctl","show","--property=MainPID","--value","kurbis-worker.service"]))


def verify_restarted_worker(worker_id: str, postgres_container_id: str, **kwargs) -> dict[str, Any]:
    return verify_restarted_worker_impl(worker_id,postgres_container_id,
        systemd_worker_pid=systemd_worker_pid,
        service_active=lambda:run_text(["systemctl","is-active","kurbis-worker.service"])=="active",
        worker_process_start_ticks=lambda pid:read_process_start_ticks(pid,BaselineError),
        worker_process_environment=worker_process_environment,
        executable_sha256=lambda pid:sha256_file(Path(f"/proc/{pid}/exe")),
        env_sha256=lambda:sha256_file(ENV_FILE),worker_lease_snapshot=worker_lease_snapshot,
        error_cls=BaselineError,**kwargs)


# Remote invocations inline the companion module at this marker; local imports load it from disk.
if __file__ != "<stdin>":
    _heartbeat_module=Path(__file__).with_name("dokploy_legacy_worker_recovery.py")
    exec(compile(_heartbeat_module.read_text(encoding="utf-8"),str(_heartbeat_module),"exec"),globals())
# __LEGACY_HEARTBEAT_HELPERS__


def validate_candidate_runtime(expected_inventory: dict[str, Any], candidate_revision: str,
                                candidate_image_label: str, candidate_payloads: dict[str, Any]) -> dict[str, Any]:
    if not valid_sha(candidate_revision):raise BaselineError("candidate release revision must be a full Git SHA")
    if candidate_image_label!=candidate_revision:raise BaselineError("candidate Pumpkin image label does not match clean source revision")
    expected_worker=expected_inventory["worker"]["legacy_pumpkin_image"]["payloads"]
    expected_waiting=expected_inventory["services"]["waiting-pumpkin"]["payloads"]
    keys=("pumpkin","wasm","hosted_entrypoint")
    for key in keys:
        if candidate_payloads.get(key)!=expected_worker.get(key):
            raise BaselineError(f"candidate Pumpkin runtime differs from exact legacy worker image: {key}")
        if candidate_payloads.get(key)!=expected_waiting.get(key):
            raise BaselineError(f"candidate Pumpkin runtime differs from live waiting image: {key}")
    return {key:candidate_payloads[key] for key in keys}


def _verify_worker_compatibility_locked(manifest: dict[str, Any], candidate_image_id: str, candidate_revision: str, token: str) -> dict[str, Any]:
    require_lock(token)
    record=load_attestation(manifest)
    expected=record["inventory"]
    services=current_service_inventory()
    for service,before in expected["services"].items():
        after=services[service]
        for key in ("image_id","image_tag","status","health"):
            if after.get(key)!=before.get(key):raise BaselineError(f"pre-deploy production inventory changed: {service}/{key}")
    candidate_label=run_text(["docker","image","inspect",candidate_image_id,"--format",'{{index .Config.Labels "org.opencontainers.image.revision"}}'])
    candidate_payloads=image_payload_hashes(candidate_image_id,"waiting-pumpkin")
    validated_payloads=validate_candidate_runtime(expected,candidate_revision,candidate_label,candidate_payloads)
    old_worker=expected["worker"]["legacy_pumpkin_image"]
    if run_text(["docker","image","inspect",PUMPKIN_LEGACY,"--format","{{.Id}}"] )!=old_worker["image_id"]:
        raise BaselineError("mutable legacy Pumpkin tag no longer resolves to attested image digest")
    expected_worker=expected["worker"]
    if sha256_file(WORKER)!=expected_worker["worker_sha256"]:raise BaselineError("worker executable differs from exact legacy baseline")
    values=worker_values();pid=int(run_text(["systemctl","show","--property=MainPID","--value","kurbis-worker.service"]))
    process_env=worker_process_environment(pid);migration=migration_record(manifest)
    if values.get("WORKER_ID")!=expected_worker["worker_id"]:raise BaselineError("worker identity differs from legacy attestation")
    if migration and migration.get("status") in {"pending","migrated"}:
        old_tag=migration.get("candidate_tag")
        allowed_tags={PUMPKIN_LEGACY,old_tag} if migration.get("status")=="pending" else {old_tag}
        if values.get("PUMPKIN_IMAGE") not in allowed_tags or process_env.get("PUMPKIN_IMAGE") not in allowed_tags:
            raise BaselineError("migrated worker differs from protected migration record")
        if sha256_file(Path(f"/proc/{pid}/exe"))!=migration.get("old_worker_sha256"):raise BaselineError("migrated worker executable changed")
    elif values.get("PUMPKIN_IMAGE")!=PUMPKIN_LEGACY or process_env.get("PUMPKIN_IMAGE")!=PUMPKIN_LEGACY:
        raise BaselineError("legacy worker config/process differs from exact attested inventory")
    lease=worker_lease(expected_worker["worker_id"],expected["services"]["postgres"]["container_id"])
    return {"status":"compatible","legacy_manifest_sha256":record["manifest_sha256"],"candidate_image_id":candidate_image_id,
            "candidate_revision":candidate_revision,"runtime_payloads":validated_payloads,
            "worker_id":expected_worker["worker_id"],"worker_sha256":expected_worker["worker_sha256"],"worker_lease":lease}


def verify_worker_compatibility(manifest: dict[str, Any], candidate_image_id: str, candidate_revision: str, token: str) -> dict[str, Any]:
    with OperationLock():
        return _verify_worker_compatibility_locked(manifest,candidate_image_id,candidate_revision,token)


def bootstrap_worker(manifest: dict[str, Any], candidate_image_id: str, candidate_revision: str, token: str) -> dict[str, Any]:
    require_lock(token)
    with OperationLock():
        attestation=load_attestation(manifest);expected=attestation["inventory"]
        _verify_worker_compatibility_locked(manifest,candidate_image_id,candidate_revision,token)
        tag=f"melon-pumpkin:host-control-{candidate_revision}";worker=expected["worker"]
        existing=migration_record(manifest);values=worker_values()
        if existing and existing.get("status")=="migrated" and existing.get("candidate_tag")==tag:
            if values.get("PUMPKIN_IMAGE")==tag and worker_process_environment(int(run_text(["systemctl","show","--property=MainPID","--value","kurbis-worker.service"]))).get("PUMPKIN_IMAGE")==tag:
                return existing
        backup_dir=WORKER_BACKUP
        if not backup_dir.exists():backup_dir.mkdir(mode=0o700)
        if backup_dir.is_symlink() or backup_dir.stat().st_uid!=0 or stat.S_IMODE(backup_dir.stat().st_mode)!=0o700:
            raise BaselineError("legacy worker backup directory must be root-owned mode 0700")
        current_env=ENV_FILE.read_bytes();current_env_hash=sha256_bytes(current_env)
        worker_bytes=WORKER.read_bytes();worker_hash=sha256_bytes(worker_bytes)
        worker_backup=WORKER_BACKUP/f"legacy-worker-bin-{worker_hash}"
        if worker_backup.is_symlink():raise BaselineError("legacy worker executable backup path is unsafe")
        if not worker_backup.exists():atomic_file(worker_backup,worker_bytes,worker["worker_mode"],worker["worker_uid"],worker["worker_gid"])
        if sha256_file(worker_backup)!=worker_hash:raise BaselineError("legacy worker executable backup digest mismatch")
        if existing:
            original_backup=Path(existing["old_worker_env_backup"]);original_hash=existing["old_worker_env_sha256"]
            original_worker_hash=existing["old_worker_sha256"]
        else:
            original_hash=worker["worker_env_sha256"];original_worker_hash=worker["worker_sha256"]
            original_backup=backup_dir/f"legacy-worker-env-{original_hash}"
            if current_env_hash!=original_hash:raise BaselineError("worker config differs from exact baseline before migration")
            if not original_backup.exists():atomic_file(original_backup,current_env,0o600,0,0)
            if original_backup.is_symlink() or sha256_file(original_backup)!=original_hash:raise BaselineError("legacy worker env backup digest mismatch")
        current_values=worker_values()
        if current_values.get("WORKER_ID")!=worker["worker_id"]:raise BaselineError("worker identity differs from attested baseline")
        if current_values.get("PUMPKIN_IMAGE") not in {PUMPKIN_LEGACY,existing.get("candidate_tag") if existing else PUMPKIN_LEGACY}:
            raise BaselineError("worker image setting is not an attested or migration-recorded value")
        env_lines=ENV_FILE.read_text(encoding="utf-8").splitlines()
        matches=[i for i,line in enumerate(env_lines) if line.startswith("export PUMPKIN_IMAGE=")]
        if len(matches)!=1:raise BaselineError("worker env must contain exactly one Pumpkin image setting")
        env_lines[matches[0]]=f"export PUMPKIN_IMAGE={tag}"
        new_env=("\n".join(env_lines)+"\n").encode()
        pending={"schema":"kurbis.respond5.legacy-worker-migration.v1","status":"pending",
                 "baseline_manifest_sha256":attestation["manifest_sha256"],"candidate_revision":candidate_revision,
                 "candidate_image_id":candidate_image_id,"candidate_tag":tag,
                 "old_pumpkin_image_id":worker["legacy_pumpkin_image"]["image_id"],
                 "old_worker_sha256":original_worker_hash,"old_worker_env_sha256":original_hash,
                 "old_worker_env_backup":str(original_backup),"old_worker_binary_backup":str(worker_backup),
                 "old_worker_mode":worker["worker_mode"],"old_worker_uid":worker["worker_uid"],"old_worker_gid":worker["worker_gid"],
                 "new_worker_env_sha256":sha256_bytes(new_env),
                 "worker_id":worker["worker_id"],"data_mutation":"none"}
        atomic_json(WORKER_MIGRATION,pending,0o600)
        try:
            if run_text(["docker","image","inspect",candidate_image_id,"--format","{{.Id}}"] )!=candidate_image_id:
                raise BaselineError("candidate image is not addressed by its immutable Docker digest")
            tagged=subprocess.run(["docker","image","tag",candidate_image_id,tag],stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False)
            if tagged.returncode:raise BaselineError("failed to tag verified candidate Pumpkin image")
            if sha256_file(ENV_FILE)!=sha256_bytes(new_env):atomic_file(ENV_FILE,new_env,0o600,0,0)
            pid=int(run_text(["systemctl","show","--property=MainPID","--value","kurbis-worker.service"]))
            if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=tag:
                subprocess.run(["systemctl","restart","kurbis-worker.service"],check=True,stdout=subprocess.DEVNULL)
            pid=int(run_text(["systemctl","show","--property=MainPID","--value","kurbis-worker.service"]))
            if run_text(["systemctl","is-active","kurbis-worker.service"])!="active" or worker_process_environment(pid).get("PUMPKIN_IMAGE")!=tag:
                raise BaselineError("worker did not load the SHA-pinned Pumpkin config")
            if sha256_file(Path(f"/proc/{pid}/exe"))!=original_worker_hash:raise BaselineError("worker executable changed during image-only migration")
            time.sleep(35)
            pid=int(run_text(["systemctl","show","--property=MainPID","--value","kurbis-worker.service"]))
            if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=tag or sha256_file(Path(f"/proc/{pid}/exe"))!=original_worker_hash:
                raise BaselineError("worker changed during migration lease window")
            lease=worker_lease(worker["worker_id"],expected["services"]["postgres"]["container_id"])
            migrated={**pending,"status":"migrated","worker_pid":pid,"worker_lease":lease}
            atomic_json(WORKER_MIGRATION,migrated,0o600)
            return migrated
        except BaseException as failure:
            backup=Path(str(original_backup))
            if backup.is_symlink() or not backup.is_file() or sha256_file(backup)!=original_hash:
                raise BaselineError(f"worker migration failed ({failure}); original config backup unavailable") from failure
            atomic_file(ENV_FILE,backup.read_bytes(),0o600,0,0)
            subprocess.run(["systemctl","restart","kurbis-worker.service"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=False)
            pid=int(run_text(["systemctl","show","--property=MainPID","--value","kurbis-worker.service"]))
            old=worker_values()
            if old.get("PUMPKIN_IMAGE")!=PUMPKIN_LEGACY or old.get("WORKER_ID")!=worker["worker_id"] or sha256_file(Path(f"/proc/{pid}/exe"))!=original_worker_hash or worker_process_environment(pid).get("PUMPKIN_IMAGE")!=PUMPKIN_LEGACY:
                raise BaselineError(f"worker migration failed ({failure}); rollback not verified") from failure
            worker_lease(worker["worker_id"],expected["services"]["postgres"]["container_id"])
            pending["status"]="rolled_back";pending["last_error_type"]=type(failure).__name__
            atomic_json(WORKER_MIGRATION,pending,0o600)
            raise BaselineError(f"worker migration failed and original config/service restored: {failure}") from failure


def extract_image_payload(image_id: str, container_path: str) -> bytes:
    container=run_text(["docker","create","--entrypoint","/bin/true",image_id])
    with tempfile.TemporaryDirectory(prefix="kurbis-rollback-image-") as directory:
        target=Path(directory)/"payload"
        try:run(["docker","cp",f"{container}:{container_path}",str(target)])
        finally:run(["docker","rm","-f",container])
        return target.read_bytes()


def assert_candidate_app_image(image_id: str, service: str, revision: str) -> None:
    details=image_info(image_id)
    if details.get("revision_label")!=revision:
        raise BaselineError(f"candidate {service} image lacks exact release revision label")
    marker=revision.encode("ascii")
    if service in {"control-plane","gateway"}:
        name="control-plane" if service=="control-plane" else "gateway"
        binary=extract_image_payload(image_id,f"/usr/local/bin/{name}")
        if marker not in binary:raise BaselineError(f"candidate {service} executable lacks compiled release revision")
        if service=="gateway" and b"Join with online Minecraft authentication enabled to create it" not in binary:
            raise BaselineError("candidate gateway executable lacks first-join admission behavior")
        worker=extract_image_payload(image_id,"/usr/local/bin/worker")
        if marker not in worker:raise BaselineError("candidate worker executable lacks compiled release revision")
    elif service=="panel":
        # Next server bundle does not embed the build arg as a standalone file; its
        # OCI label is accepted only after the exact Dokploy default --build flow.
        return
    elif service=="waiting-pumpkin":
        embedded=extract_image_payload(image_id,"/usr/share/doc/kurbis/build-revision").decode("utf-8").strip()
        if embedded!=revision:raise BaselineError("candidate waiting image embedded revision mismatch")
        upstream=extract_image_payload(image_id,"/usr/share/doc/pumpkin/REVISION").decode("utf-8").strip()
        executable=extract_image_payload(image_id,"/usr/local/bin/pumpkin")
        upstream_pin=subprocess.check_output(["git","-C",str(CHECKOUT),"show",f"{revision}:runtime/pumpkin/REVISION"],text=True).strip()
        if upstream!=upstream_pin:raise BaselineError("candidate waiting image upstream Pumpkin revision mismatch")
        host_tag=f"melon-pumpkin:host-control-{revision}"
        host_id=run_text(["docker","image","inspect",host_tag,"--format","{{.Id}}"])
        host_pumpkin=extract_image_payload(host_id,"/usr/local/bin/pumpkin")
        if sha256_bytes(executable)!=sha256_bytes(host_pumpkin):raise BaselineError("candidate waiting Pumpkin executable differs from source-built host-control image")
    else:
        raise BaselineError(f"unsupported app service for rollback provenance: {service}")


def validate_rollback_scope(expected: dict[str, Any], current: dict[str, dict[str, Any]], candidate_revision: str) -> dict[str, Any]:
    if not valid_sha(candidate_revision):raise BaselineError("rollback candidate revision is invalid")
    allowed_new={}
    for service in ("control-plane","gateway","panel","waiting-pumpkin"):
        item=current.get(service)
        if not item:raise BaselineError(f"rollback cannot find current service: {service}")
        if item["image_id"]==expected["services"][service]["image_id"]:continue
        label=run_text(["docker","image","inspect",item["image_id"],"--format",'{{index .Config.Labels "org.opencontainers.image.revision"}}'])
        if label!=candidate_revision:
            raise BaselineError(f"rollback refuses non-baseline/non-candidate image: {service}")
        assert_candidate_app_image(item["image_id"],service,candidate_revision)
        allowed_new[service]=item["image_id"]
    database=current.get("postgres")
    if not database or database["image_id"]!=expected["services"]["postgres"]["image_id"]:
        raise BaselineError("rollback refuses to alter or accept an unrecognized database image")
    return allowed_new


def legacy_rollback_compose_command(env_file: Path = ENV_FILE_CHECKOUT, compose_file: Path = CHECKOUT/"compose.yaml") -> list[str]:
    return ["docker","compose","-p",PROJECT,"--env-file",str(env_file),"-f",str(compose_file),
            "up","-d","--no-build","--no-deps","control-plane","gateway","panel","waiting-pumpkin"]


def rollback_app(manifest: dict[str, Any], expected_sources: dict[str, str], token: str, candidate_revision: str) -> dict[str, Any]:
    require_lock(token)
    with OperationLock():
        record=load_attestation(manifest);expected=record["inventory"];revision=record["source_revision_claim"]
        if not valid_sha(candidate_revision):raise BaselineError("candidate rollback revision must be a full SHA")
        if manifest.get("schema") == CURRENT_BASELINE_SCHEMA:
            if revision != CURRENT_BASELINE_REVISION:
                raise BaselineError("current rollback source claim is invalid")
        elif revision != LEGACY_REVISION_CLAIM:
            raise BaselineError("legacy rollback source claim is invalid")
        if not isinstance(expected_sources,dict) or not expected_sources:
            raise BaselineError("rollback source manifest is invalid")
        current=current_service_inventory()
        validate_rollback_scope(expected,current,candidate_revision)
        restore_baseline_compose_env(record)
        for path,blob in expected_sources.items():
            if Path(path).is_absolute() or ".." in Path(path).parts or not valid_sha(blob):raise BaselineError("unsafe rollback source manifest")
        tag=f"kurbis-release-{revision}"
        run(["git","-C",str(CHECKOUT),"fetch","origin",f"refs/tags/{tag}:refs/tags/{tag}"])
        if run_text(["git","-C",str(CHECKOUT),"rev-parse",f"refs/tags/{tag}^{{commit}}"] )!=revision:
            raise BaselineError("legacy rollback tag does not resolve to attested checkout commit")
        run(["git","-C",str(CHECKOUT),"reset","--hard",revision]);run(["git","-C",str(CHECKOUT),"clean","-fdx","-e",".env"])
        tracked=set(run_text(["git","-C",str(CHECKOUT),"ls-tree","-r","--name-only",revision]).splitlines())
        if tracked!=set(expected_sources):raise BaselineError("legacy rollback manifest does not cover all tracked inputs")
        for path,blob in expected_sources.items():
            if run_text(["git","-C",str(CHECKOUT),"hash-object","--",path])!=blob:raise BaselineError(f"legacy source mismatch: {path}")
        backup=CHECKOUT_BACKUP/token
        if backup.is_symlink() or not (backup/"manifest.json").is_file():raise BaselineError("original Dokploy overrides unavailable")
        overlays=json.loads((backup/"manifest.json").read_text(encoding="utf-8"))
        for item in overlays:
            relative=item.get("path")
            if relative not in {"compose.yaml","web/tsconfig.json"}:raise BaselineError("unexpected path in checkout backup")
            path=CHECKOUT/relative
            current_blob=run_text(["git","-C",str(CHECKOUT),"hash-object","--",relative])
            if current_blob!=item.get("original_target",item.get("target")):raise BaselineError(f"rollback override base changed: {relative}")
            atomic_file(path,base64.b64decode(item["content"],validate=True),int(item["mode"]),int(item["uid"]),int(item["gid"]))
        for service in ("control-plane","gateway","panel","waiting-pumpkin"):
            entry=expected["services"][service]
            if image_info(entry["image_id"])["image_id"]!=entry["image_id"]:raise BaselineError(f"captured rollback image missing: {service}")
            run(["docker","tag",entry["image_id"],entry["image_tag"]])
        config=json.loads(run_text(["docker","compose","-p",PROJECT,"--env-file",str(ENV_FILE_CHECKOUT),"-f",str(CHECKOUT/"compose.yaml"),"config","--format","json"]))
        services=config.get("services",{})
        for service in SERVICES:
            if services.get(service,{}).get("image")!=expected["services"][service]["image_tag"]:
                raise BaselineError(f"rollback Compose image does not reference captured {service} tag")
        run(legacy_rollback_compose_command())
        deadline=time.monotonic()+900
        while time.monotonic()<deadline:
            current=current_service_inventory()
            if all(current[name]["status"]=="running" and current[name]["image_id"]==expected["services"][name]["image_id"] and current[name]["health"]==expected["services"][name]["health"] for name in ("control-plane","gateway","panel","waiting-pumpkin")):
                break
            time.sleep(5)
        else:raise BaselineError("captured baseline application image rollback timed out")
        if manifest.get("schema") == CURRENT_BASELINE_SCHEMA:
            # The current 1..13 baseline is intentionally migration-safe, not f6.
            # wait_sync owns the worker transaction and rolls it back before this
            # function runs; verify the exact old executable/config without forcing
            # another restart while the pre-release lease is known to be stale.
            worker=expected["worker"]
            values=worker_values()
            pid=systemd_worker_pid()
            process_env=worker_process_environment(pid)
            if (values.get("PUMPKIN_IMAGE") != PUMPKIN_LEGACY
                or process_env.get("PUMPKIN_IMAGE") != PUMPKIN_LEGACY
                or sha256_file(WORKER) != worker["worker_sha256"]
                or sha256_file(Path(f"/proc/{pid}/exe")) != worker["worker_process_sha256"]):
                raise BaselineError("current migration-safe worker bundle was not restored")
            final_services=current_service_inventory()
            for service in SERVICES:
                if final_services[service]["image_id"] != expected["services"][service]["image_id"]:
                    raise BaselineError(f"current rollback digest mismatch: {service}")
            return {"status":"app-rolled-back","source_revision_claim":revision,
                    "provenance_status":"migration-safe-unproven",
                    "image_ids":{name:expected["services"][name]["image_id"] for name in SERVICES},
                    "worker_id":worker["worker_id"],"worker_sha256":worker["worker_sha256"],
                    "worker_bundle_restored":True}
        migration=migration_record(manifest)
        if not migration or not migration.get("old_worker_env_backup"):
            raise BaselineError("worker migration backup missing; cannot restore original worker config")
        legacy_image_id=expected["worker"]["legacy_pumpkin_image"]["image_id"]
        if image_info(legacy_image_id)["image_id"]!=legacy_image_id:
            raise BaselineError("captured original worker Pumpkin image digest is unavailable")
        run(["docker","tag",legacy_image_id,PUMPKIN_LEGACY])
        old_env_path=Path(migration["old_worker_env_backup"])
        old_worker_path=Path(migration.get("old_worker_binary_backup",""))
        if old_env_path.is_symlink() or not old_env_path.is_file() or sha256_file(old_env_path)!=migration["old_worker_env_sha256"]:
            raise BaselineError("original worker environment backup missing or corrupt")
        if old_worker_path.is_symlink() or not old_worker_path.is_file() or sha256_file(old_worker_path)!=migration["old_worker_sha256"]:
            raise BaselineError("original worker executable backup missing or corrupt")
        worker=expected["worker"]
        previous_pid=systemd_worker_pid()
        previous_start_ticks=read_process_start_ticks(previous_pid,BaselineError)
        previous_lease=worker_lease_snapshot(worker["worker_id"],expected["services"]["postgres"]["container_id"],require_fresh=False)
        atomic_file(WORKER,old_worker_path.read_bytes(),int(migration["old_worker_mode"]),int(migration["old_worker_uid"]),int(migration["old_worker_gid"]))
        atomic_file(ENV_FILE,old_env_path.read_bytes(),0o600,0,0)
        restored_env_sha256=sha256_file(ENV_FILE)
        if restored_env_sha256!=migration["old_worker_env_sha256"]:
            raise BaselineError("restored worker environment does not match the protected original digest")
        subprocess.run(["systemctl","restart","kurbis-worker.service"],check=True,stdout=subprocess.DEVNULL)
        verified_worker=verify_restarted_worker(worker["worker_id"],expected["services"]["postgres"]["container_id"],
            previous_pid=previous_pid,previous_start_ticks=previous_start_ticks,
            previous_heartbeat_at=previous_lease["heartbeat_at"],expected_worker_sha256=worker["worker_sha256"],
            expected_env_sha256=restored_env_sha256,expected_pumpkin_image=PUMPKIN_LEGACY)
        pid=verified_worker["pid"];lease=verified_worker["lease"]
        final_services=current_service_inventory()
        for service in SERVICES:
            if final_services[service]["image_id"]!=expected["services"][service]["image_id"]:raise BaselineError(f"legacy rollback digest mismatch: {service}")
        migration["status"]="rolled_back";migration["rollback_worker_pid"]=pid;migration["rollback_worker_start_ticks"]=verified_worker["start_ticks"];migration["rollback_lease"]=lease;migration["rollback_heartbeat_at"]=verified_worker["heartbeat_at"]
        atomic_json(WORKER_MIGRATION,migration,0o600)
        return {"status":"app-rolled-back","source_revision_claim":revision,"provenance_status":"legacy-unproven",
                "image_ids":{name:expected["services"][name]["image_id"] for name in SERVICES},
                "worker_id":worker["worker_id"],"worker_sha256":worker["worker_sha256"],
                "worker_bundle_restored":True,"lease":lease}


def atomic_file(path: Path, content: bytes, mode: int, uid: int, gid: int) -> None:
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    tmp = Path(temporary)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.chown(tmp, uid, gid)
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise


def migration_record_from_hash(manifest_sha: str) -> dict[str, Any] | None:
    if not WORKER_MIGRATION.exists():return None
    if WORKER_MIGRATION.is_symlink():raise BaselineError("worker migration record is a symlink")
    info=WORKER_MIGRATION.stat()
    if info.st_uid!=0 or stat.S_IMODE(info.st_mode)!=0o600:raise BaselineError("worker migration record must be root-owned mode 0600")
    record=json.loads(WORKER_MIGRATION.read_text(encoding="utf-8"))
    if record.get("baseline_manifest_sha256")!=manifest_sha:raise BaselineError("worker migration record belongs to another baseline")
    return record


def save_baseline_compose_env(inventory: dict[str, Any]) -> dict[str, Any]:
    source=inventory["source"]
    if not COMPOSE_ENV_BACKUPS.exists():COMPOSE_ENV_BACKUPS.mkdir(mode=0o700)
    if COMPOSE_ENV_BACKUPS.is_symlink() or COMPOSE_ENV_BACKUPS.stat().st_uid!=0 or stat.S_IMODE(COMPOSE_ENV_BACKUPS.stat().st_mode)!=0o700:
        raise BaselineError("baseline environment backup directory must be root-owned mode 0700")
    digest=source["compose_env_file_sha256"]
    path=COMPOSE_ENV_BACKUPS/f"compose-env-{digest}"
    content=ENV_FILE_CHECKOUT.read_bytes()
    if sha256_bytes(content)!=digest:raise BaselineError("Dokploy .env changed while inventory was captured")
    if path.is_symlink():raise BaselineError("baseline environment backup cannot be a symlink")
    if not path.exists():atomic_file(path,content,0o600,0,0)
    if path.stat().st_uid!=0 or stat.S_IMODE(path.stat().st_mode)!=0o600 or sha256_file(path)!=digest:
        raise BaselineError("baseline Dokploy .env backup failed integrity/metadata validation")
    return {"path":str(path),"sha256":digest,"mode":source["compose_env_file_mode"],
            "uid":source["compose_env_file_uid"],"gid":source["compose_env_file_gid"]}


def restore_baseline_compose_env(record: dict[str, Any]) -> bool:
    metadata=record.get("compose_env_backup")
    if not isinstance(metadata,dict):raise BaselineError("baseline Dokploy .env backup metadata is missing")
    path=Path(metadata.get("path",""))
    if path.is_symlink() or not path.is_file() or path.parent not in {COMPOSE_ENV_BACKUPS, CURRENT_COMPOSE_ENV_BACKUPS}:
        raise BaselineError("baseline Dokploy .env backup path is invalid")
    if path.stat().st_uid!=0 or stat.S_IMODE(path.stat().st_mode)!=0o600 or sha256_file(path)!=metadata.get("sha256"):
        raise BaselineError("baseline Dokploy .env backup integrity check failed")
    if sha256_file(ENV_FILE_CHECKOUT)==metadata["sha256"]:return False
    atomic_file(ENV_FILE_CHECKOUT,path.read_bytes(),int(metadata["mode"]),int(metadata["uid"]),int(metadata["gid"]))
    if sha256_file(ENV_FILE_CHECKOUT)!=metadata["sha256"]:raise BaselineError("restored Dokploy .env digest mismatch")
    return True


def bootstrap(manifest: dict[str, Any], token: str, *, candidate_revision: str | None = None,
              expected_sources: dict[str, str] | None = None) -> dict[str, Any]:
    require_lock(token)
    with OperationLock():
        expected=manifest.get("expected_inventory") if isinstance(manifest,dict) else None
        if not isinstance(expected,dict):raise BaselineError("legacy manifest is missing expected inventory")
        manifest_sha=validate_manifest(manifest,expected)
        audit = audit_path(manifest)
        if audit.is_symlink():raise BaselineError("baseline audit file cannot be a symlink")
        if audit.exists():
            attestation=load_attestation(manifest)
            allowed_worker_images=authorized_worker_images_for_inventory(manifest)
            inventory_options = {"allowed_worker_images": allowed_worker_images}
            if manifest.get("schema") == CURRENT_BASELINE_SCHEMA:
                inventory_options["baseline_schema"] = CURRENT_BASELINE_SCHEMA
            if candidate_revision is not None:
                checkout_head=verify_attested_candidate_source(candidate_revision,expected_sources or {})
                current=collect_inventory(expected_checkout_head=checkout_head, **inventory_options)
                verify_attested_legacy_services(current,attestation["inventory"],manifest_sha,
                                                candidate_revision=candidate_revision,inventory_checkout_head=checkout_head)
                return {"status":"already-recorded","manifest_sha256":manifest_sha,"inventory":current}
            current=collect_inventory(**inventory_options)
            verify_attested_legacy_services(current,attestation["inventory"],manifest_sha)
            restore_baseline_compose_env(attestation)
            current=collect_inventory(**inventory_options)
            if current.get("source")!=attestation["inventory"].get("source"):
                raise BaselineError("protected baseline environment/checkout restoration did not match exact attested source")
            verify_attested_legacy_services(current,attestation["inventory"],manifest_sha)
            return {"status":"already-recorded","manifest_sha256":manifest_sha,"inventory":current}
        schema = manifest.get("schema")
        if candidate_revision is not None and schema != CURRENT_BASELINE_SCHEMA:
            raise BaselineError("cannot create a legacy attestation from refreshed release source; exact old baseline checkout is required")
        current=collect_inventory(baseline_schema=schema) if schema == CURRENT_BASELINE_SCHEMA else collect_inventory()
        manifest_sha=validate_manifest(manifest,current)
        compose_env_backup=save_baseline_compose_env(current, compose_env_backup_root(schema))
        record={"schema":manifest["schema"],"source_revision_claim":manifest["source_revision_claim"],
                "provenance_status":"legacy-unproven" if schema == LEGACY_BASELINE_SCHEMA else "migration-safe-unproven",
                "operator":manifest["operator"],"reason":manifest["reason"],"manifest_sha256":manifest_sha,
                "inventory":current,"compose_env_backup":compose_env_backup}
        if ROOT.is_symlink() or ROOT.stat().st_uid!=0 or ROOT.stat().st_mode&0o022:
            raise BaselineError("baseline audit directory is unsafe")
        atomic_json(audit,record,0o600)
        return {"status":"recorded","manifest_sha256":manifest_sha,"inventory":current}


def main() -> int:
    parser=argparse.ArgumentParser()
    parser.add_argument("--operation",choices=("capture","bootstrap","bootstrap-worker","reconcile-worker-bootstrap","check-worker-compatibility","rollback-app"),required=True)
    parser.add_argument("--manifest-json")
    parser.add_argument("--expected-sources-json")
    parser.add_argument("--candidate-image-id")
    parser.add_argument("--candidate-revision")
    parser.add_argument("--token")
    parser.add_argument("--mode", choices=("legacy", "current"), default="legacy")
    args=parser.parse_args()
    try:
        manifest=json.loads(args.manifest_json) if args.manifest_json else None
        if args.operation=="capture":
            schema = CURRENT_BASELINE_SCHEMA if args.mode == "current" else LEGACY_BASELINE_SCHEMA
            head = CURRENT_BASELINE_REVISION if args.mode == "current" else LEGACY_REVISION_CLAIM
            result=collect_inventory(expected_checkout_head=head, baseline_schema=schema)
        elif args.operation=="bootstrap":
            if manifest is None or not args.token:raise BaselineError("operator-reviewed manifest and rollout lock token are required")
            result=bootstrap(manifest,args.token,candidate_revision=args.candidate_revision,
                             expected_sources=json.loads(args.expected_sources_json) if args.expected_sources_json else None)
        elif args.operation=="bootstrap-worker":
            if manifest is None or not args.candidate_image_id or not args.candidate_revision or not args.token:raise BaselineError("manifest, candidate image, revision and rollout token are required")
            result=bootstrap_worker(manifest,args.candidate_image_id,args.candidate_revision,args.token)
        elif args.operation=="reconcile-worker-bootstrap":
            if manifest is None or not args.token:raise BaselineError("manifest and rollout token are required")
            result=reconcile_worker_bootstrap(manifest,args.token)
        elif args.operation=="check-worker-compatibility":
            if manifest is None or not args.candidate_image_id or not args.candidate_revision or not args.token:raise BaselineError("manifest, candidate image, revision and rollout token are required")
            result=verify_worker_compatibility(manifest,args.candidate_image_id,args.candidate_revision,args.token)
        else:
            if manifest is None or not args.expected_sources_json or not args.token:raise BaselineError("manifest, source manifest and rollout token are required")
            if not args.candidate_revision:raise BaselineError("candidate revision required for rollback scope")
            result=rollback_app(manifest,json.loads(args.expected_sources_json),args.token,args.candidate_revision)
        print(json.dumps(result,sort_keys=True));return 0
    except (BaselineError,OSError,subprocess.TimeoutExpired,json.JSONDecodeError,KeyError,ValueError) as error:
        print(f"legacy baseline refused: {error}",file=__import__("sys").stderr);return 1


if __name__ == "__main__":
    raise SystemExit(main())
