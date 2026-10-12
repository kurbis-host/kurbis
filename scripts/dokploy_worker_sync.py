#!/usr/bin/env python3
"""Root-only respond5 verifier used by the Kurbis Dokploy release wrapper."""
from __future__ import annotations

import argparse
import base64
import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
import tarfile
import time
import uuid
from pathlib import Path
from typing import Sequence

ROOT = Path("/srv/kurbis")
CHECKOUT = Path("/etc/dokploy/compose/kurbis-hoster-fbikvd/code")
# Dokploy clones with a GitHub App token embedded in the `origin` URL, and that
# token expires after an hour. Release tags are fetched through this plain URL
# so the host's own Git credential helper authenticates instead.
RELEASE_REMOTE = "https://github.com/kurbis-host/kurbis.git"
ENV = CHECKOUT / ".env"
WORKER = Path("/srv/kurbis-hoster/target/release/worker")
WORKER_ENV = ROOT / "worker.env"
WORKER_UNIT = Path("/etc/systemd/system/kurbis-worker.service")
WORKER_LAUNCHER = Path("/usr/local/bin/kurbis-worker")
WORKER_SERVICE = "kurbis-worker.service"
PROJECT = "kurbis-hoster-fbikvd"
SERVICES = ("control-plane", "gateway", "panel", "waiting-pumpkin", "postgres")
PUMPKIN_PREFIX = "melon-pumpkin:host-control-"
WAITING_PREFIX = "melon-pumpkin:waiting-"
PUMPKIN_ARTIFACTS = ("pumpkin", "kurbis-control.wasm")
LOCK = Path("/run/lock/kurbis-production-deploy.lock")
OPERATION_LOCK = Path("/run/lock/kurbis-production-deploy-operation.lock")
CHECKOUT_BACKUP = ROOT / "deploy-checkout-overrides"
# Pumpkin binaries compiled on the remote CI builder, uploaded per release SHA.
RELEASE_ARTIFACTS = ROOT / "release-artifacts"
WORKER_BACKUP = ROOT / "worker-rollbacks"
ENV_BACKUP = ROOT / "worker-bundle-rollbacks"
MARKER = ROOT / "deployed-release.json"
RELEASE_MANIFESTS = ROOT / "release-manifests"
TRANSACTIONS = ROOT / "worker-release-transactions"
LEGACY = (b"identity_not_linked", b"Link this Minecraft account")
ADMISSION = b"Join with online Minecraft authentication enabled to create it"


class Error(RuntimeError):
    pass


def run(args: Sequence[str], *, input_text: str | None = None, cwd: Path | None = None) -> str:
    p = subprocess.run(list(args), input=input_text, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if p.returncode:
        raise Error(f"{args[0]} failed: {p.stderr.strip() or p.stdout.strip()}")
    return p.stdout.strip()


def root_only() -> None:
    if os.geteuid() != 0:
        raise Error("respond5 release verification requires root")


def run_bytes(args: Sequence[str], *, input_bytes: bytes | None = None, cwd: Path | None = None) -> bytes:
    """Run a command without decoding or trimming output (required for Git -z records)."""
    p = subprocess.run(list(args), input=input_bytes, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if p.returncode:
        detail = p.stderr.decode("utf-8", "replace").strip() or p.stdout.decode("utf-8", "replace").strip()
        raise Error(f"{args[0]} failed: {detail}")
    return p.stdout


def valid_sha(value: str) -> bool:
    return bool(re.fullmatch(r"[0-9a-f]{40}", value))


def git_blob(content: bytes) -> str:
    header = f"blob {len(content)}\0".encode("ascii")
    return hashlib.sha1(header + content).hexdigest()


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def release_manifest_hash(value: dict) -> str:
    payload = dict(value)
    payload.pop("manifest_sha256", None)
    encoded = (json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def persist_immutable_release_manifest(value: dict) -> Path:
    if ROOT.is_symlink() or ROOT.stat().st_uid != 0 or ROOT.stat().st_mode & 0o022:
        raise Error("release manifest root is unsafe")
    if not RELEASE_MANIFESTS.exists():
        RELEASE_MANIFESTS.mkdir(mode=0o700)
    if (RELEASE_MANIFESTS.is_symlink() or RELEASE_MANIFESTS.stat().st_uid != 0
            or stat.S_IMODE(RELEASE_MANIFESTS.stat().st_mode) != 0o700):
        raise Error("release manifest directory ownership/mode invalid")
    revision = str(value["source_revision"])
    path = RELEASE_MANIFESTS / f"{revision}.json"
    content = (json.dumps(value, sort_keys=True) + "\n").encode("utf-8")
    if path.exists():
        if path.is_symlink() or path.read_bytes() != content:
            raise Error("immutable release manifest already exists with different contents")
        return path
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.chown(path, 0, 0)
    except BaseException:
        path.unlink(missing_ok=True)
        raise
    return path


def validate_release_manifest(value: dict) -> None:
    if value.get("schema") != "kurbis.release-manifest.v1":
        raise Error("release manifest schema is invalid")
    if not valid_sha(str(value.get("source_revision", ""))):
        raise Error("release manifest source revision is invalid")
    for key in ("migration_manifest_sha256", "environment_revision", "manifest_sha256"):
        if not re.fullmatch(r"[0-9a-f]{64}", str(value.get(key, ""))):
            raise Error(f"release manifest digest is invalid: {key}")
    if value.get("status") != "verified" or release_manifest_hash(value) != value.get("manifest_sha256"):
        raise Error("release manifest is not verified or has been tampered with")
    keys = value.get("environment_keys")
    if not isinstance(keys, list) or keys != sorted(set(str(key) for key in keys)):
        raise Error("release manifest environment key inventory is invalid")
    image_ids = value.get("service_image_ids")
    if not isinstance(image_ids, dict) or set(image_ids) != set(SERVICES):
        raise Error("release manifest service image inventory is incomplete")


def identity() -> str:
    root_only()
    if WORKER_ENV.is_symlink() or not WORKER_ENV.is_file():
        raise Error("worker credentials are not a regular file")
    m = WORKER_ENV.stat()
    if m.st_uid != 0 or stat.S_IMODE(m.st_mode) != 0o600:
        raise Error("worker credentials must be root-owned mode 0600")
    for line in WORKER_ENV.read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"\s*(?:export\s+)?WORKER_ID\s*=\s*(.*?)\s*", line)
        if match:
            try:
                return str(uuid.UUID(match.group(1).strip("\"'")))
            except ValueError:
                break
    raise Error("invalid worker identity")


def worker_id() -> str:
    return identity()


def worker_image_value() -> str:
    values = [line.split("=", 1)[1].strip().strip("\"'") for line in WORKER_ENV.read_text(encoding="utf-8").splitlines() if re.match(r"^export PUMPKIN_IMAGE=", line)]
    if len(values) != 1:
        raise Error("worker env must contain exactly one PUMPKIN_IMAGE")
    value=values[0]
    if value != "melon-pumpkin:host-control" and not re.fullmatch(re.escape(PUMPKIN_PREFIX) + r"[0-9a-f]{40}", value):
        raise Error("worker PUMPKIN_IMAGE is neither the recognized legacy tag nor a full SHA-pinned Kurbis image")
    return value


def worker_setting(name: str) -> str | None:
    """Return one optional `export NAME=value` setting from the worker environment."""
    values = [line.split("=", 1)[1].strip().strip("\"'") for line in WORKER_ENV.read_text(encoding="utf-8").splitlines() if line.startswith(f"export {name}=")]
    if len(values) > 1:
        raise Error(f"worker env must not repeat {name}")
    return values[0] if values else None


def publish_image_to_worker(image: str) -> None:
    """Copy a release image into the Docker daemon the worker starts runtimes on.

    The worker uses its own rootless daemon, whose image store is separate
    from the one release images are built in; without the copy the worker's
    startup probe fails with "No such image".
    """
    host = worker_setting("DOCKER_HOST")
    if not host:
        return
    if not re.fullmatch(r"unix:///[A-Za-z0-9_./-]+", host):
        raise Error("worker DOCKER_HOST is not a local Unix socket")
    image_id = run(["docker", "image", "inspect", image, "--format", "{{.Id}}"])
    saved = subprocess.Popen(["docker", "save", image], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    loaded = subprocess.run(["docker", "--host", host, "load"], stdin=saved.stdout, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    saved.stdout.close()
    save_error = saved.stderr.read().decode("utf-8", "replace").strip()
    if saved.wait() or loaded.returncode:
        raise Error(f"could not copy {image} to the worker Docker daemon: {save_error or loaded.stderr.decode('utf-8', 'replace').strip()}")
    if run(["docker", "--host", host, "image", "inspect", image, "--format", "{{.Id}}"]) != image_id:
        raise Error(f"worker Docker daemon holds a different image for {image}")


def worker_image() -> str:
    image=worker_image_value()
    if not re.fullmatch(re.escape(PUMPKIN_PREFIX) + r"[0-9a-f]{40}", image):
        raise Error("worker PUMPKIN_IMAGE is not a full SHA-pinned Kurbis image")
    return image


def service_valid() -> None:
    root_only()
    if WORKER.resolve() != WORKER or "ExecStart=/usr/local/bin/kurbis-worker" not in WORKER_UNIT.read_text(encoding="utf-8") or f"exec {WORKER}" not in WORKER_LAUNCHER.read_text(encoding="utf-8").splitlines():
        raise Error("worker unit/launcher does not target the approved Kurbis path")


def states() -> dict[str, list[dict[str, str]]]:
    out = {}
    for service in SERVICES:
        ids = run(["docker", "ps", "-aq", "--filter", f"label=com.docker.compose.project={PROJECT}", "--filter", f"label=com.docker.compose.service={service}"]).splitlines()
        items = json.loads(run(["docker", "inspect", *ids])) if ids else []
        out[service] = [{"id": x["Id"], "image": x["Image"], "status": x["State"]["Status"], "health": x["State"].get("Health", {}).get("Status", "none"), "revision": x.get("Config", {}).get("Labels", {}).get("org.opencontainers.image.revision", "")} for x in items]
    return out


def worker_process_environment(pid: int) -> dict[str, str]:
    raw=Path(f"/proc/{pid}/environ").read_bytes()
    values={}
    for entry in raw.split(b"\0"):
        if b"=" not in entry:continue
        key,value=entry.split(b"=",1)
        values[key.decode("utf-8","surrogateescape")]=value.decode("utf-8","surrogateescape")
    return values


def single_worker() -> int:
    service_valid()
    if run(["systemctl", "is-active", WORKER_SERVICE]) != "active":
        raise Error("worker systemd service is not active")
    pid = int(run(["systemctl", "show", "--property=MainPID", "--value", WORKER_SERVICE]))
    paths = []
    for proc in Path("/proc").iterdir():
        if proc.name.isdigit():
            try:
                exe = (proc / "exe").resolve()
            except (FileNotFoundError, PermissionError, OSError):
                continue
            if exe.name == "worker":
                paths.append(exe)
    if paths != [WORKER.resolve()] or digest(Path(f"/proc/{pid}/exe")) != digest(WORKER):
        raise Error(f"worker process set is unsafe: {paths}")
    return pid


def lease(db_id: str) -> None:
    wid = identity()
    sql = """SELECT wc.worker_id::text,(wc.revoked_at IS NULL AND wc.lease_expires_at > now()),(w.heartbeat_at > now()-interval '30 seconds'),(SELECT count(*) FROM worker_credentials c WHERE c.worker_id=wc.worker_id AND c.revoked_at IS NULL AND c.lease_expires_at > now()) FROM worker_credentials wc JOIN workers w ON w.id=wc.worker_id WHERE wc.worker_id=:'worker_id'::uuid AND wc.revoked_at IS NULL;"""
    got = run(["docker", "exec", "-i", "--env", f"KURBIS_WORKER_ID={wid}", db_id, "sh", "-lc", 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -At -F "|" -v worker_id="$KURBIS_WORKER_ID" -f -'], input_text=sql)
    if got.strip() != f"{wid}|t|t|1":
        raise Error("worker lease is not uniquely active and renewing")


def image_for_app(state: dict[str, list[dict[str, str]]], revision: str | None = None) -> str:
    cp, gw = state["control-plane"], state["gateway"]
    if len(cp) != 1 or len(gw) != 1 or cp[0]["id"] == gw[0]["id"] or cp[0]["image"] != gw[0]["image"]:
        raise Error("control-plane/gateway are not two running containers on one image")
    if revision and (cp[0]["revision"] != revision or gw[0]["revision"] != revision):
        raise Error("running app OCI revision differs from requested SHA")
    if cp[0]["status"] != "running" or gw[0]["status"] != "running" or cp[0]["health"] != "healthy":
        raise Error("app is not healthy/running")
    return cp[0]["image"]


def pending_worker_transactions() -> list[Path]:
    if not TRANSACTIONS.exists():
        return []
    if TRANSACTIONS.is_symlink() or TRANSACTIONS.stat().st_uid != 0 or stat.S_IMODE(TRANSACTIONS.stat().st_mode) != 0o700:
        raise Error("worker transaction directory ownership/mode invalid")
    pending=[]
    try:marker=json.loads(MARKER.read_text(encoding="utf-8"))
    except (OSError,json.JSONDecodeError):marker={}
    for path in TRANSACTIONS.glob("*.json"):
        if path.is_symlink() or not path.is_file():raise Error("worker transaction directory contains unsafe records")
        record=json.loads(path.read_text(encoding="utf-8"))
        if record.get("status")=="accepted":
            if (marker.get("source_revision")==record.get("source_revision") and marker.get("deployment_id")==record.get("deployment_id")
                and digest(WORKER)==record.get("new_worker_sha256")
                and digest(WORKER_ENV)==record.get("new_worker_env_sha256")
                and worker_image_value()==record.get("new_pumpkin_image")):
                path.unlink()
                continue
            raise Error("accepted worker transaction does not match current release marker/runtime")
        pending.append(path)
    return pending


def _baseline_locked() -> dict[str, list[dict[str, str]]]:
    root_only(); service_valid(); identity()
    if pending_worker_transactions():raise Error("unresolved worker rollback transaction blocks a new rollout")
    pinned=worker_image();state=states()
    for service in SERVICES:
        if len(state[service]) != 1 or state[service][0]["status"] != "running" or (service != "gateway" and state[service][0]["health"] != "healthy"):
            raise Error(f"baseline service is not unique/healthy: {service}")
    app_revision=state["control-plane"][0]["revision"]
    if not valid_sha(app_revision) or state["gateway"][0]["revision"]!=app_revision:
        raise Error("baseline control plane and gateway revisions do not match")
    if any(state[name][0]["revision"]!=app_revision for name in ("panel","waiting-pumpkin")):
        raise Error("strict baseline panel/waiting services have mixed app revisions")
    verify_image_revision(state["panel"][0]["image"],app_revision)
    pid=single_worker()
    if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=pinned:raise Error("baseline worker process environment differs from its protected config")
    lease(state["postgres"][0]["id"]);return state


def baseline() -> dict[str, list[dict[str, str]]]:
    with OperationLock():
        return _baseline_locked()


def _baseline_preflight_locked() -> dict[str, object]:
    """Validate the live service/lease before allowing a safe legacy-image migration."""
    root_only(); service_valid(); identity()
    if pending_worker_transactions():raise Error("unresolved worker rollback transaction blocks a new rollout")
    state=states()
    for service in SERVICES:
        if len(state[service])!=1 or state[service][0]["status"]!="running" or (service!="gateway" and state[service][0]["health"]!="healthy"):
            raise Error(f"pre-bootstrap service is not unique/healthy: {service}")
    app_revision=state["control-plane"][0]["revision"]
    if not valid_sha(app_revision) or state["gateway"][0]["revision"]!=app_revision:
        raise Error("current app release has no consistent full-SHA OCI revision; safe worker bootstrap is impossible")
    if any(state[name][0]["revision"]!=app_revision for name in ("panel","waiting-pumpkin")):
        raise Error("current production services have mixed revisions; safe worker bootstrap is impossible")
    verify_image_revision(state["panel"][0]["image"],app_revision)
    upstream=run(["git","-C",str(CHECKOUT),"show",f"{app_revision}:runtime/pumpkin/REVISION"]).strip()
    waiting_id,label=run(["docker","image","inspect",state["waiting-pumpkin"][0]["image"],"--format",'{{.Id}}|{{index .Config.Labels "org.opencontainers.image.revision"}}']).split("|",1)
    if waiting_id!=state["waiting-pumpkin"][0]["image"] or label!=app_revision:raise Error("current waiting image label differs from active app revision")
    image=worker_image_value()
    pid=single_worker()
    if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=image:
        raise Error("running worker environment differs from protected config; safe bootstrap is impossible")
    lease(state["postgres"][0]["id"])
    return {"status":"preflight","snapshot":state,"app_revision":app_revision,"worker_image":image,"upstream_revision":upstream}


def baseline_preflight() -> dict[str, object]:
    with OperationLock():
        return _baseline_preflight_locked()


def _bootstrap_worker_image_locked(token: str, revision: str, pinned_image: str, upstream: str) -> dict[str, str]:
    require_lock(token)
    if not valid_sha(revision) or pinned_image!=f"{PUMPKIN_PREFIX}{revision}":raise Error("bootstrap image is not pinned to the active app revision")
    old_env=WORKER_ENV.read_bytes();old_image=worker_image_value()
    state=states()
    if any(len(state.get(name,[]))!=1 or state[name][0]["revision"]!=revision or state[name][0]["status"]!="running"
           for name in ("control-plane","gateway","panel","waiting-pumpkin")):
        raise Error("production services changed after preflight; refusing legacy image bootstrap")
    verify_image_revision(state["panel"][0]["image"],revision)
    verify_waiting(state["waiting-pumpkin"][0],revision,upstream,pinned_image)
    if old_image==pinned_image:
        verify_pinned_pumpkin(old_image)
        return {"status":"already-pinned","pumpkin_image":pinned_image}
    pinned_id,pinned_sha=verify_pumpkin(pinned_image,revision,upstream)
    if old_image=="melon-pumpkin:host-control":
        old_id=run(["docker","image","inspect",old_image,"--format","{{.Id}}"])
    else:
        old_id,_=verify_pinned_pumpkin(old_image)
    old_payload=pumpkin_runtime_payload_hashes(old_id)
    pinned_payload=pumpkin_runtime_payload_hashes(pinned_id)
    if old_payload!=pinned_payload or old_payload.get("pumpkin")!=pinned_sha:
        raise Error("current Pumpkin runtime bundle differs from source-pinned active-release build; refusing migration")
    new_env=updated_worker_env(old_env,pinned_image)
    database=state["postgres"]
    if len(database)!=1 or database[0]["status"]!="running":raise Error("database unavailable before worker bootstrap")
    database_id=database[0]["id"]
    old_hash=digest(WORKER)
    def verify_old():
        if worker_image_value()!=old_image or digest(WORKER_ENV)!=hashlib.sha256(old_env).hexdigest():raise Error("legacy worker environment was not restored")
        pid=single_worker()
        if digest(Path(f"/proc/{pid}/exe"))!=old_hash:raise Error("legacy worker executable changed during rollback")
        if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=old_image:raise Error("legacy worker process retained a partial bootstrap environment")
        lease(database_id)
    def verify_new():
        if worker_image()!=pinned_image:raise Error("pinned worker image bootstrap did not persist")
        pid=single_worker()
        if digest(Path(f"/proc/{pid}/exe"))!=old_hash:raise Error("worker executable changed during image-only bootstrap")
        if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=pinned_image:raise Error("worker process did not load the SHA-pinned Pumpkin environment")
        time.sleep(35)
        pid=single_worker()
        if digest(Path(f"/proc/{pid}/exe"))!=old_hash:raise Error("worker executable changed during bootstrap lease window")
        if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=pinned_image:raise Error("worker process environment changed during bootstrap lease window")
        lease(database_id)
    try:
        mutate_worker_bundle(old_binary=WORKER.read_bytes(),new_binary=WORKER.read_bytes(),old_env=old_env,new_env=new_env,
            binary_changed=False,env_changed=True,write_binary=lambda content:None,
            write_env=lambda content:atomic(WORKER_ENV,content,0o600,0,0),
            restart=lambda:run(["systemctl","restart",WORKER_SERVICE]),verify_new=verify_new,verify_old=verify_old)
    except BundleMutationError as error:
        raise Error(f"worker image bootstrap failed: {error}") from error
    return {"status":"bootstrapped","pumpkin_image":pinned_image,"pumpkin_image_id":pinned_id,"pumpkin_sha256":pinned_sha}


def bootstrap_worker_image(token: str, revision: str, pinned_image: str, upstream: str) -> dict[str, str]:
    with OperationLock():
        return _bootstrap_worker_image_locked(token,revision,pinned_image,upstream)


def lock(token: str, acquire: bool) -> dict[str, str]:
    root_only()
    if not re.fullmatch(r"[0-9a-f]{32}", token): raise Error("invalid rollout token")
    if acquire:
        LOCK.parent.mkdir(mode=0o700, exist_ok=True)
        if LOCK.exists(): raise Error("another rollout lock exists")
        LOCK.mkdir(mode=0o700); (LOCK / "owner").write_text(token + "\n", encoding="ascii"); os.chmod(LOCK / "owner", 0o600); return {"status":"locked"}
    if LOCK.is_symlink() or not (LOCK / "owner").is_file() or (LOCK / "owner").read_text().strip() != token: raise Error("rollout lock ownership mismatch")
    (LOCK / "owner").unlink(); LOCK.rmdir(); return {"status":"released"}


class OperationLock:
    def __enter__(self):
        root_only()
        OPERATION_LOCK.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
        flags=os.O_CREAT|os.O_RDWR|getattr(os,"O_NOFOLLOW",0)
        self.fd=os.open(OPERATION_LOCK,flags,0o600)
        metadata=os.fstat(self.fd)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid!=0 or stat.S_IMODE(metadata.st_mode)!=0o600:
            os.close(self.fd)
            raise Error("worker operation lock must be a root-owned regular mode-0600 file")
        fcntl.flock(self.fd,fcntl.LOCK_EX)
        return self

    def __exit__(self,*exc):
        fcntl.flock(self.fd,fcntl.LOCK_UN)
        os.close(self.fd)


def require_lock(token: str) -> None:
    root_only()
    if not re.fullmatch(r"[0-9a-f]{32}", token or "") or LOCK.is_symlink():
        raise Error("invalid rollout lock ownership")
    owner = LOCK / "owner"
    if not owner.is_file() or owner.is_symlink() or owner.read_text(encoding="ascii").strip() != token:
        raise Error("rollout lock ownership mismatch")


# Files Dokploy and the panel build rewrite in place during a deployment:
# Dokploy re-serializes compose.yaml (dropping comments and renaming anchors)
# and the Next.js build normalizes web/tsconfig.json.
DEPLOYMENT_REWRITTEN = frozenset({"compose.yaml", "web/tsconfig.json"})

def validate_checkout_status(status: bytes, *, allow_overrides: bool) -> None:
    allowed = DEPLOYMENT_REWRITTEN if allow_overrides else set()
    for record in (part for part in status.split(b"\0") if part):
        if len(record) < 4 or record[2:3] != b" ":
            raise Error(f"malformed Dokploy checkout status record: {record!r}")
        code, path = record[:2].decode("ascii"), record[3:].decode("utf-8", "surrogateescape")
        if code == "!!" and path == ".env":
            continue
        if code.startswith("?") or code == "!!":
            raise Error(f"untracked/ignored Dokploy build input forbidden: {path}")
        if code not in {" M", "M ", "MM"} or path not in allowed:
            raise Error(f"unexpected Dokploy checkout change: {path}")


def validate_expected_manifest(expected: dict[str, str]) -> None:
    if not isinstance(expected, dict) or not expected:
        raise Error("release source manifest is empty")
    for path, blob in expected.items():
        p = Path(path)
        if p.is_absolute() or ".." in p.parts or not valid_sha(blob):
            raise Error("release source manifest contains an unsafe path or blob id")


def stage(revision: str, token: str, expected: dict[str, str]) -> dict[str, str]:
    root_only(); require_lock(token); identity(); validate_expected_manifest(expected)
    if not valid_sha(revision): raise Error("invalid release SHA")
    if CHECKOUT.is_symlink() or not (CHECKOUT / ".git").is_dir(): raise Error("invalid Dokploy checkout")
    status = run_bytes(["git","-C",str(CHECKOUT),"status","--porcelain=v1","-z","--untracked-files=all","--ignored"])
    validate_checkout_status(status, allow_overrides=True)
    CHECKOUT_BACKUP.mkdir(mode=0o700, exist_ok=True)
    if CHECKOUT_BACKUP.is_symlink() or CHECKOUT_BACKUP.stat().st_uid != 0 or stat.S_IMODE(CHECKOUT_BACKUP.stat().st_mode) != 0o700:
        raise Error("checkout backup directory ownership/mode invalid")
    backup = CHECKOUT_BACKUP / token
    if backup.exists(): raise Error("checkout backup already exists")
    backup.mkdir(mode=0o700)
    changed = run_bytes(["git","-C",str(CHECKOUT),"diff","--name-only","-z","HEAD"]).split(b"\0")
    manifest=[]
    for raw_relative in filter(None, changed):
        relative = raw_relative.decode("utf-8", "surrogateescape")
        path=CHECKOUT/relative
        if not path.is_file() or path.is_symlink() or Path(relative).is_absolute() or ".." in Path(relative).parts: raise Error(f"unsafe override: {relative}")
        m=path.stat(); content=path.read_bytes()
        head_target=subprocess.run(["git","-C",str(CHECKOUT),"rev-parse",f"HEAD:{relative}"],text=True,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,check=False)
        original_target=head_target.stdout.strip() if head_target.returncode==0 else "missing"
        manifest.append({"path":relative,"content":base64.b64encode(content).decode(),"mode":stat.S_IMODE(m.st_mode),"uid":m.st_uid,"gid":m.st_gid,"target":expected.get(relative,"missing"),"original_target":original_target})
    (backup/"manifest.json").write_text(json.dumps(manifest),encoding="utf-8"); os.chmod(backup/"manifest.json",0o600)
    tag=f"kurbis-release-{revision}"
    run(["git","-C",str(CHECKOUT),"fetch",RELEASE_REMOTE,f"refs/tags/{tag}:refs/tags/{tag}"])
    if run(["git","-C",str(CHECKOUT),"rev-parse",f"refs/tags/{tag}^{{commit}}"] ) != revision: raise Error("immutable release tag SHA mismatch")
    run(["git","-C",str(CHECKOUT),"reset","--hard",revision]); run(["git","-C",str(CHECKOUT),"clean","-fdx","-e",".env"])
    status = run_bytes(["git","-C",str(CHECKOUT),"status","--porcelain=v1","-z","--untracked-files=all","--ignored"])
    validate_checkout_status(status, allow_overrides=False)
    tracked = set(run(["git", "-C", str(CHECKOUT), "ls-tree", "-r", "--name-only", revision]).splitlines())
    if tracked != set(expected): raise Error("release source manifest does not cover every tracked input")
    for path, blob in expected.items():
        if run(["git","-C",str(CHECKOUT),"hash-object","--",path]) != blob: raise Error(f"source mismatch: {path}")
    return {"status":"staged","revision":revision}


def restore(token: str) -> dict[str, str]:
    root_only(); require_lock(token)
    if not re.fullmatch(r"[0-9a-f]{32}", token): raise Error("invalid checkout backup token")
    backup=CHECKOUT_BACKUP/token
    if not backup.exists(): return {"status":"restored","restored":"[]","preserved":"[]"}
    if backup.is_symlink() or not (backup/"manifest.json").is_file(): raise Error("checkout backup is missing or unsafe")
    manifest=json.loads((backup/"manifest.json").read_text(encoding="utf-8")); restored=[]; preserved=[]
    for entry in manifest:
        path=CHECKOUT/entry["path"]
        current = run(["git","-C",str(CHECKOUT),"hash-object","--",entry["path"]]) if path.is_file() else "missing"
        content=base64.b64decode(entry["content"])
        backup_blob=git_blob(content)
        allowed_targets={entry.get("target","missing"),entry.get("original_target","missing"),backup_blob}
        if current not in allowed_targets: preserved.append(entry["path"]); continue
        if current!=backup_blob:atomic(path,content,int(entry["mode"]),int(entry["uid"]),int(entry["gid"]))
        restored.append(entry["path"])
    if preserved:
        return {"status":"preserved","restored":json.dumps(restored),"preserved":json.dumps(preserved)}
    shutil.rmtree(backup)
    return {"status":"restored","restored":json.dumps(restored),"preserved":"[]"}


def atomic(path: Path, content: bytes, mode: int, uid: int=0, gid: int=0) -> None:
    fd,tmp=tempfile.mkstemp(prefix=f".{path.name}.",dir=path.parent); temp=Path(tmp)
    try:
        with os.fdopen(fd,"wb") as f: f.write(content); f.flush(); os.fsync(f.fileno())
        os.chown(temp,uid,gid); os.chmod(temp,mode); os.replace(temp,path)
        directory_fd=os.open(path.parent,os.O_RDONLY)
        try: os.fsync(directory_fd)
        finally: os.close(directory_fd)
    except BaseException: temp.unlink(missing_ok=True); raise


def extract(image: str, directory: Path) -> tuple[Path,Path,Path]:
    cid=run(["docker","create","--entrypoint","/bin/true",image])
    try:
        cp,gw,w=directory/"control",directory/"gateway",directory/"worker"
        for source,target in (("control-plane",cp),("gateway",gw),("worker",w)): run(["docker","cp",f"{cid}:/usr/local/bin/{source}",str(target)])
        return cp,gw,w
    finally: run(["docker","rm",cid])


def verify_git_archive(archive: Path, expected: dict[str, str]) -> str:
    validate_expected_manifest(expected)
    with tarfile.open(archive, "r:") as tar:
        members = {}
        for member in tar.getmembers():
            name = member.name.removeprefix("./")
            if name in members or Path(name).is_absolute() or ".." in Path(name).parts:
                raise Error("immutable Git archive contains duplicate or unsafe paths")
            if member.isfile() or member.issym():
                members[name] = member
        if set(members) != set(expected):
            raise Error("immutable Git archive manifest mismatch")
        for name, member in members.items():
            if member.issym():
                content = member.linkname.encode("utf-8")
            else:
                stream = tar.extractfile(member)
                if stream is None:
                    raise Error(f"immutable Git archive entry is unreadable: {name}")
                content = stream.read()
            if git_blob(content) != expected[name]:
                raise Error(f"immutable Git archive blob mismatch: {name}")
    return digest(archive)


def release_artifact_context(revision: str, artifacts: dict[str,str]) -> Path:
    """Return the Docker build context holding this release's verified Pumpkin binaries."""
    if set(artifacts)!=set(PUMPKIN_ARTIFACTS) or not all(re.fullmatch(r"[0-9a-f]{64}",str(value)) for value in artifacts.values()):
        raise Error("Pumpkin artifact digest manifest is invalid")
    context=RELEASE_ARTIFACTS/revision
    directory=context/"artifacts"
    for path in (RELEASE_ARTIFACTS,context,directory):
        if path.is_symlink() or not path.is_dir() or path.stat().st_uid!=0 or stat.S_IMODE(path.stat().st_mode)!=0o700:
            raise Error("release artifact directory ownership/mode invalid")
    if {entry.name for entry in directory.iterdir()}!=set(PUMPKIN_ARTIFACTS): raise Error("release artifact directory holds unexpected files")
    for name,expected_digest in artifacts.items():
        path=directory/name
        if path.is_symlink() or not path.is_file() or digest(path)!=expected_digest: raise Error(f"uploaded Pumpkin artifact differs from the remote build: {name}")
    return context

def build_pumpkin(revision: str, expected: dict[str,str], token: str, artifacts: dict[str,str]) -> dict[str,str]:
    require_lock(token); validate_expected_manifest(expected)
    if not valid_sha(revision): raise Error("invalid Pumpkin build revision")
    context=release_artifact_context(revision,artifacts)
    tag_name=f"kurbis-release-{revision}"
    run(["git","-C",str(CHECKOUT),"fetch",RELEASE_REMOTE,f"refs/tags/{tag_name}:refs/tags/{tag_name}"])
    if run(["git","-C",str(CHECKOUT),"rev-parse",f"refs/tags/{tag_name}^{{commit}}"] )!=revision:
        raise Error("Pumpkin source tag does not resolve to requested release SHA")
    root=ROOT/"release-builds"
    if not root.exists():root.mkdir(mode=0o700)
    if root.is_symlink() or root.stat().st_uid!=0 or stat.S_IMODE(root.stat().st_mode)!=0o700: raise Error("release build root ownership/mode invalid")
    source=root/revision
    if source.exists(): raise Error("detached release source path already exists")
    run(["git","-C",str(CHECKOUT),"worktree","add","--detach",str(source),revision])
    try:
        if run(["git","-C",str(source),"status","--porcelain=v1","--untracked-files=all","--ignored"]): raise Error("detached source build directory is not clean")
        for path,blob in expected.items():
            if run(["git","-C",str(source),"hash-object","--",path])!=blob: raise Error(f"detached source mismatch: {path}")
        with tempfile.TemporaryDirectory(prefix="kurbis-release-archive-") as d:
            archive=Path(d)/"release.tar"
            with archive.open("wb") as output:
                result=subprocess.run(["git","-C",str(source),"archive","--format=tar",revision],stdout=output,stderr=subprocess.PIPE,check=False)
            if result.returncode: raise Error("git archive failed")
            archive_sha=verify_git_archive(archive,expected)
        tag=f"{PUMPKIN_PREFIX}{revision}"
        # The named context replaces the Dockerfile's from-source `artifacts`
        # stage, so respond5 only packages the binaries compiled on the builder.
        for target,image in (("host-control",tag),("waiting",f"{WAITING_PREFIX}{revision}")):
            run(["docker","build","--target",target,"--build-arg",f"BUILD_REVISION={revision}","--build-context",f"artifacts={context}",
                 "-f","runtime/pumpkin/Dockerfile","-t",image,"."],cwd=source)
        image_id,label=run(["docker","image","inspect",tag,"--format",'{{.Id}}|{{index .Config.Labels "org.opencontainers.image.revision"}}']).split("|",1)
        if label!=revision: raise Error("Pumpkin image revision label mismatch")
        upstream=run(["git","-C",str(source),"show",f"{revision}:runtime/pumpkin/REVISION"]).strip()
        pump_id,pump_sha=verify_pumpkin(tag,revision,upstream)
        if pump_id!=image_id: raise Error("Pumpkin immutable image ID changed during verification")
        if pump_sha!=artifacts["pumpkin"]: raise Error("packaged Pumpkin executable differs from the remote build")
        publish_image_to_worker(tag)
        return {"status":"built","pumpkin_image":tag,"pumpkin_image_id":image_id,"pumpkin_revision":revision,"pumpkin_sha256":pump_sha,"archive_sha256":archive_sha}
    finally: run(["git","-C",str(CHECKOUT),"worktree","remove","--force",str(source)])

def updated_worker_env(current:bytes,tag:str)->bytes:
    if tag!=f"{PUMPKIN_PREFIX}{tag.removeprefix(PUMPKIN_PREFIX)}" or not re.fullmatch(r"[0-9a-f]{40}",tag.removeprefix(PUMPKIN_PREFIX)): raise Error("invalid SHA-pinned worker Pumpkin image")
    lines=current.decode("utf-8").splitlines(); matches=[i for i,line in enumerate(lines) if line.startswith("export PUMPKIN_IMAGE=")]
    if len(matches)!=1: raise Error("worker env must have exactly one PUMPKIN_IMAGE")
    lines[matches[0]]=f"export PUMPKIN_IMAGE={tag}"
    return ("\n".join(lines)+"\n").encode()

def extract_binary(image_id:str,name:str,directory:Path)->Path:
    cid=run(["docker","create","--entrypoint","/bin/true",image_id]);destination=directory/name
    try:run(["docker","cp",f"{cid}:/usr/local/bin/{name}",str(destination)])
    finally:run(["docker","rm",cid])
    return destination


def verify_application_image(image_id:str,revision:str)->tuple[dict[str,str],bytes]:
    label=run(["docker","image","inspect",image_id,"--format",'{{index .Config.Labels "org.opencontainers.image.revision"}}'])
    if label!=revision:raise Error("application image label differs from release SHA")
    with tempfile.TemporaryDirectory(prefix="kurbis-app-verify-") as directory:
        root=Path(directory);binaries={name:extract_binary(image_id,name,root) for name in ("control-plane","gateway","worker")}
        data={name:path.read_bytes() for name,path in binaries.items()}
        if any(revision.encode("ascii") not in value for value in data.values()):raise Error("compiled app binary missing exact BUILD_REVISION")
        if any(mark in data["control-plane"] or mark in data["gateway"] for mark in LEGACY):raise Error("legacy account-link flow detected in runtime app binary")
        if ADMISSION not in data["gateway"]:raise Error("gateway binary lacks auto-admission release marker")
        hashes={"control_plane_sha256":digest(binaries["control-plane"]),"gateway_sha256":digest(binaries["gateway"]),"worker_sha256":digest(binaries["worker"])}
        return hashes,data["worker"]


def verify_worker_binary_in_app_image(image_id: str, revision: str) -> str:
    label=run(["docker","image","inspect",image_id,"--format",'{{index .Config.Labels "org.opencontainers.image.revision"}}'])
    if label!=revision:raise Error("application image OCI revision label differs from expected source SHA")
    with tempfile.TemporaryDirectory(prefix="kurbis-prior-worker-") as directory:
        binary=extract_binary(image_id,"worker",Path(directory))
        return digest(binary)


def verify_image_revision(image_id: str, revision: str) -> str:
    details=run(["docker","image","inspect",image_id,"--format",'{{.Id}}|{{index .Config.Labels "org.opencontainers.image.revision"}}'])
    parts=details.split("|",1)
    if len(parts)!=2 or parts[0]!=image_id or parts[1]!=revision:
        raise Error("container image OCI revision label mismatch")
    return parts[0]


def verify_pinned_pumpkin(image: str) -> tuple[str, str]:
    prefix=PUMPKIN_PREFIX
    if not image.startswith(prefix):raise Error("worker Pumpkin image tag is not SHA-pinned")
    revision=image[len(prefix):]
    if not valid_sha(revision):raise Error("worker Pumpkin image tag has invalid source SHA")
    cid=run(["docker","create","--entrypoint","/bin/true",image])
    try:
        with tempfile.TemporaryDirectory(prefix="kurbis-pinned-pumpkin-") as directory:
            root=Path(directory);build_revision=root/"build-revision";upstream_revision=root/"upstream-revision"
            run(["docker","cp",f"{cid}:/usr/share/doc/kurbis/build-revision",str(build_revision)])
            run(["docker","cp",f"{cid}:/usr/share/doc/pumpkin/REVISION",str(upstream_revision)])
            if build_revision.read_text(encoding="utf-8").strip()!=revision:raise Error("worker Pumpkin embedded source revision mismatch")
            return verify_pumpkin(image,revision,upstream_revision.read_text(encoding="utf-8").strip())
    finally:run(["docker","rm",cid])


def pumpkin_runtime_payload_hashes(image_id: str) -> dict[str, str]:
    files={"pumpkin":"/usr/local/bin/pumpkin",
           "plugin":"/usr/local/share/kurbis-control.wasm",
           "entrypoint":"/usr/local/bin/hosted-entrypoint"}
    with tempfile.TemporaryDirectory(prefix="kurbis-pumpkin-exe-") as directory:
        root=Path(directory);cid=run(["docker","create","--entrypoint","/bin/true",image_id])
        try:
            for name,source in files.items():run(["docker","cp",f"{cid}:{source}",str(root/name)])
        finally:run(["docker","rm",cid])
        return {name:digest(root/name) for name in files}



def verify_pumpkin(image:str,revision:str,upstream:str)->tuple[str,str]:
    if image!=f"{PUMPKIN_PREFIX}{revision}":raise Error("Pumpkin image tag is not pinned to exact release SHA")
    image_id,label=run(["docker","image","inspect",image,"--format",'{{.Id}}|{{index .Config.Labels "org.opencontainers.image.revision"}}']).split("|",1)
    if label!=revision or not image_id.startswith("sha256:"):raise Error("Pumpkin immutable image label/digest mismatch")
    with tempfile.TemporaryDirectory(prefix="kurbis-pumpkin-verify-") as directory:
        root=Path(directory);revision_file=run(["docker","create","--entrypoint","/bin/true",image_id])
        try:
            for source,target in (("/usr/share/doc/kurbis/build-revision","build-revision"),("/usr/share/doc/pumpkin/REVISION","upstream-revision"),("/usr/local/bin/pumpkin","pumpkin")):
                run(["docker","cp",f"{revision_file}:{source}",str(root/target)])
        finally:run(["docker","rm",revision_file])
        if (root/"build-revision").read_text().strip()!=revision or (root/"upstream-revision").read_text().strip()!=upstream:raise Error("Pumpkin image embedded source provenance mismatch")
        return image_id,digest(root/"pumpkin")


def verify_waiting(container:dict[str,str],revision:str,upstream:str,pumpkin_image:str)->None:
    if container["revision"]!=revision:raise Error("waiting runtime container OCI revision mismatch")
    image_id,label=run(["docker","image","inspect",container["image"],"--format",'{{.Id}}|{{index .Config.Labels "org.opencontainers.image.revision"}}']).split("|",1)
    if image_id!=container["image"] or label!=revision:raise Error("waiting runtime image OCI revision label mismatch")
    _,expected=verify_pumpkin(pumpkin_image,revision,upstream)
    with tempfile.TemporaryDirectory(prefix="kurbis-waiting-verify-") as directory:
        root=Path(directory);cid=run(["docker","create","--entrypoint","/bin/true",container["image"]])
        try:
            for source,target in (("/usr/local/bin/pumpkin","pumpkin"),("/usr/share/doc/kurbis/build-revision","build-revision"),("/usr/share/doc/pumpkin/REVISION","upstream-revision")):
                run(["docker","cp",f"{cid}:{source}",str(root/target)])
        finally:run(["docker","rm",cid])
        if (root/"build-revision").read_text().strip()!=revision or (root/"upstream-revision").read_text().strip()!=upstream:raise Error("waiting runtime embedded source provenance mismatch")
        if digest(root/"pumpkin")!=expected:raise Error("waiting Pumpkin executable differs from release-built host-control image")


def save_backup(directory:Path,name:str,content:bytes,mode:int,uid:int=0,gid:int=0)->Path:
    if not directory.exists():directory.mkdir(mode=0o700)
    if directory.is_symlink() or directory.stat().st_uid!=0 or stat.S_IMODE(directory.stat().st_mode)!=0o700:raise Error("worker rollback directory must be root-owned mode 0700")
    path=directory/name
    if path.is_symlink():raise Error("worker rollback backup must not be a symlink")
    if not path.exists():atomic(path,content,mode,uid,gid)
    if not path.is_file() or path.stat().st_uid!=0 or stat.S_IMODE(path.stat().st_mode)!=mode or digest(path)!=hashlib.sha256(content).hexdigest():raise Error("worker rollback backup integrity/metadata mismatch")
    return path


def transaction_path(token: str) -> Path:
    if not re.fullmatch(r"[0-9a-f]{32}", token or ""):
        raise Error("invalid worker transaction token")
    if not TRANSACTIONS.exists():
        TRANSACTIONS.mkdir(mode=0o700)
    if TRANSACTIONS.is_symlink() or TRANSACTIONS.stat().st_uid != 0 or stat.S_IMODE(TRANSACTIONS.stat().st_mode) != 0o700:
        raise Error("worker transaction directory ownership/mode invalid")
    return TRANSACTIONS / f"{token}.json"


def write_transaction(token: str, record: dict[str, object]) -> Path:
    path = transaction_path(token)
    if path.exists() or path.is_symlink():
        raise Error("worker transaction already exists for rollout lock")
    atomic(path, (json.dumps(record, sort_keys=True) + "\n").encode(), 0o600, 0, 0)
    return path


class BundleMutationError(Error):
    def __init__(self, message: str, *, rollback_verified: bool):
        super().__init__(message)
        self.rollback_verified = rollback_verified


def mutate_worker_bundle(*, old_binary: bytes, new_binary: bytes, old_env: bytes, new_env: bytes,
                         binary_changed: bool, env_changed: bool, write_binary, write_env,
                         restart, verify_new, verify_old) -> None:
    try:
        if env_changed:
            write_env(new_env)
        if binary_changed:
            write_binary(new_binary)
        if binary_changed or env_changed:
            restart()
        verify_new()
    except BaseException as failure:
        try:
            # Always restore both files: a failed atomic write may have replaced its target
            # before a later fsync, restart, or verification failure surfaced.
            write_binary(old_binary)
            write_env(old_env)
            restart()
            verify_old()
        except BaseException as rollback:
            raise BundleMutationError(
                f"worker release mutation failed ({failure}); rollback incomplete: {rollback}",
                rollback_verified=False,
            ) from failure
        raise BundleMutationError(
            f"worker release mutation failed and prior worker/env were restored: {failure}",
            rollback_verified=True,
        ) from failure


def worker_transaction(new_binary:bytes,pumpkin_image:str,*,token:str,revision:str,restart,verify_new,verify_old)->dict[str,str]:
    require_lock(token)
    service_valid();old_bin=WORKER.read_bytes();bm=WORKER.stat();old_env=WORKER_ENV.read_bytes();em=WORKER_ENV.stat()
    if em.st_uid!=0 or stat.S_IMODE(em.st_mode)!=0o600:raise Error("worker env must remain root-owned mode 0600")
    new_env=updated_worker_env(old_env,pumpkin_image);bin_changed=old_bin!=new_binary;env_changed=old_env!=new_env
    old_image=worker_image_value()
    old_bin_hash=hashlib.sha256(old_bin).hexdigest();old_env_hash=hashlib.sha256(old_env).hexdigest()
    bin_backup=save_backup(WORKER_BACKUP,f"worker-{old_bin_hash}",old_bin,stat.S_IMODE(bm.st_mode),bm.st_uid,bm.st_gid)
    env_backup=save_backup(ENV_BACKUP,f"worker-env-{old_env_hash}",old_env,0o600,0,0)
    transaction={"token":token,"source_revision":revision,"old_worker_sha256":old_bin_hash,"old_worker_env_sha256":old_env_hash,
        "old_worker_mode":stat.S_IMODE(bm.st_mode),"old_worker_uid":bm.st_uid,"old_worker_gid":bm.st_gid,
        "old_worker_image":old_image,"new_worker_sha256":hashlib.sha256(new_binary).hexdigest(),
        "new_worker_env_sha256":hashlib.sha256(new_env).hexdigest(),"new_pumpkin_image":pumpkin_image}
    tx_path=write_transaction(token,transaction)
    try:
        mutate_worker_bundle(
            old_binary=old_bin, new_binary=new_binary, old_env=old_env, new_env=new_env,
            binary_changed=bin_changed, env_changed=env_changed,
            write_binary=lambda content: atomic(WORKER,content,stat.S_IMODE(bm.st_mode),bm.st_uid,bm.st_gid),
            write_env=lambda content: atomic(WORKER_ENV,content,0o600,0,0),
            restart=restart, verify_new=verify_new, verify_old=verify_old,
        )
    except BundleMutationError as error:
        if error.rollback_verified:
            tx_path.unlink(missing_ok=True)
        raise
    return {"worker_sha256":hashlib.sha256(new_binary).hexdigest(),
        "worker_env_sha256":hashlib.sha256(new_env).hexdigest(),
        "worker_changed":str(bin_changed or env_changed).lower(),
        "rollback_worker_sha256":old_bin_hash,"rollback_worker_env_sha256":old_env_hash}


def _rollback_worker_locked(token: str) -> dict[str, str]:
    require_lock(token)
    tx_path=transaction_path(token)
    if not tx_path.exists(): return {"status":"no-pending-worker-transaction"}
    if tx_path.is_symlink(): raise Error("worker transaction record is unsafe")
    data=json.loads(tx_path.read_text(encoding="utf-8"))
    if data.get("token")!=token or not valid_sha(str(data.get("source_revision", ""))):raise Error("worker transaction record is invalid")
    if data.get("status")=="accepted":
        try:marker=json.loads(MARKER.read_text(encoding="utf-8"))
        except (OSError,json.JSONDecodeError) as error:raise Error("accepted worker record has no valid release marker") from error
        if marker.get("source_revision")!=data.get("source_revision") or marker.get("deployment_id")!=data.get("deployment_id") or digest(WORKER)!=data.get("new_worker_sha256") or worker_image()!=data.get("new_pumpkin_image"):
            raise Error("accepted worker record no longer matches the live release")
        return {"status":"already-accepted"}
    old_hash=str(data.get("old_worker_sha256", ""));old_env_hash=str(data.get("old_worker_env_sha256", ""))
    if not re.fullmatch(r"[0-9a-f]{64}",old_hash) or not re.fullmatch(r"[0-9a-f]{64}",old_env_hash):raise Error("worker rollback hashes are invalid")
    bin_backup=WORKER_BACKUP/f"worker-{old_hash}";env_backup=ENV_BACKUP/f"worker-env-{old_env_hash}"
    if bin_backup.is_symlink() or env_backup.is_symlink() or digest(bin_backup)!=old_hash or digest(env_backup)!=old_env_hash:raise Error("worker rollback backup missing or corrupt")
    try:
        marker=json.loads(MARKER.read_text(encoding="utf-8"))
        if marker.get("source_revision")==data["source_revision"] and marker.get("deployment_id")==data.get("deployment_id"):
            MARKER.unlink()
    except FileNotFoundError:
        pass
    atomic(WORKER,bin_backup.read_bytes(),int(data["old_worker_mode"]),int(data["old_worker_uid"]),int(data["old_worker_gid"]))
    atomic(WORKER_ENV,env_backup.read_bytes(),0o600,0,0)
    run(["systemctl","restart",WORKER_SERVICE])
    started=time.monotonic()
    deadline=started+75
    last_error=""
    while time.monotonic()<deadline:
        try:
            pid=single_worker()
            if digest(Path(f"/proc/{pid}/exe"))!=old_hash:raise Error("rollback worker executable hash mismatch")
            if digest(WORKER_ENV)!=old_env_hash or worker_image_value()!=data["old_worker_image"]:raise Error("rollback worker environment mismatch")
            state=states()
            if len(state["postgres"])!=1 or state["postgres"][0]["status"]!="running":raise Error("database is unavailable for worker rollback verification")
            lease(state["postgres"][0]["id"])
            if time.monotonic()-started < 35:
                time.sleep(min(3, 35-(time.monotonic()-started)))
                continue
            # Confirm a fresh lease after a full heartbeat window, not just a stale pre-restart lease.
            pid=single_worker()
            if digest(Path(f"/proc/{pid}/exe"))!=old_hash:raise Error("rollback worker executable changed during lease window")
            lease(state["postgres"][0]["id"])
            break
        except (Error, OSError, ValueError) as error:
            last_error=str(error);time.sleep(3)
    else:raise Error(f"prior worker rollback could not be verified: {last_error}")
    tx_path.unlink()
    return {"status":"rolled-back","worker_sha256":old_hash,"worker_env_sha256":old_env_hash}


def rollback_worker(token: str) -> dict[str, str]:
    with OperationLock():
        return _rollback_worker_locked(token)


# Remote invocations inline the companion TLS verification module at this marker.
if __file__ != "<stdin>":
    _tls_module=Path(__file__).with_name("dokploy_worker_tls.py")
    exec(compile(_tls_module.read_text(encoding="utf-8"),str(_tls_module),"exec"),globals())
# __TLS_HELPERS__

def verify_build_context(checkout:Path,revision:str,expected:dict[str,str],release_environment:dict[str,str]|None=None,*,deployed:bool=False)->None:
    """Check the Dokploy checkout against the release manifest.

    After a deployment (`deployed`), the files Dokploy rewrites in place are
    exempt from the byte comparison; the rendered Compose configuration below
    is still checked in full.
    """
    if run(["git","-C",str(checkout),"rev-parse","HEAD"])!=revision:raise Error("Dokploy checkout HEAD differs from release SHA")
    validate_expected_manifest(expected)
    status=run_bytes(["git","-C",str(checkout),"status","--porcelain=v1","-z","--untracked-files=all","--ignored"])
    validate_checkout_status(status, allow_overrides=deployed)
    tracked=set(run(["git","-C",str(checkout),"ls-tree","-r","--name-only",revision]).splitlines())
    if tracked!=set(expected):raise Error("expected manifest does not cover all tracked build inputs")
    for path,blob in expected.items():
        if deployed and path in DEPLOYMENT_REWRITTEN:continue
        if run(["git","-C",str(checkout),"hash-object","--",path])!=blob:raise Error(f"Dokploy build input mismatch: {path}")
    for ignore in (checkout/".dockerignore",checkout/"web/.dockerignore"):
        if not ignore.is_file() or ".env*" not in ignore.read_text().splitlines():raise Error("Docker context does not exclude .env secrets")
    values={}
    for line in ENV.read_text(encoding="utf-8").splitlines():
        if line.strip() and not line.lstrip().startswith("#") and "=" in line:
            key,value=line.split("=",1);values[key]=value.strip().strip("\"'")
    required={"BUILD_REVISION":revision,"BEDROCK_TLS_HOST_DIRECTORY":"/srv/kurbis/certs","PANEL_BIND_ADDRESS":"127.0.0.1"}
    compose_env_files=["--env-file",str(ENV)]
    if release_environment is not None:
        if set(release_environment)!=set(required) or release_environment!=required:
            raise Error("pre-deploy Compose environment overrides differ from reviewed release/prod values")
        values.update(release_environment)
        fd,override_name=tempfile.mkstemp(prefix="kurbis-release-env-",suffix=".env")
        override=Path(override_name)
        try:
            with os.fdopen(fd,"w",encoding="utf-8") as output:
                for key in sorted(release_environment):output.write(f"{key}={release_environment[key]}\n")
                output.flush();os.fsync(output.fileno())
            os.chmod(override,0o600)
            compose_env_files.extend(["--env-file",str(override)])
            if any(values.get(k)!=v for k,v in required.items()):raise Error("Dokploy env does not match exact release/prod binds")
            configured=json.loads(run(["docker","compose","-p",PROJECT,*compose_env_files,"-f",str(checkout/"compose.yaml"),"config","--format","json"]))
        finally:
            override.unlink(missing_ok=True)
    else:
        if any(values.get(k)!=v for k,v in required.items()):raise Error("Dokploy env does not match exact release/prod binds")
        configured=json.loads(run(["docker","compose","-p",PROJECT,*compose_env_files,"-f",str(checkout/"compose.yaml"),"config","--format","json"]))
    services=configured.get("services",{})
    if set(SERVICES)-set(services):raise Error("rendered Compose configuration omits production services")
    if services["control-plane"].get("image")!=services["gateway"].get("image"):
        raise Error("control-plane/gateway Compose services do not share the expected image")
    verify_gateway_tls_configuration(services,values["BEDROCK_TLS_HOST_DIRECTORY"],values["BEDROCK_PORT"],error_cls=Error)
    verify_bedrock_tls_assets(values["BEDROCK_TLS_HOST_DIRECTORY"],values["ROOT_DOMAIN"],
                              error_cls=Error,run_command=run,project=PROJECT)
    expected_contexts={"control-plane":checkout,"panel":checkout/"web"}
    for service,expected_context in expected_contexts.items():
        build=services[service].get("build",{})
        args=build.get("args",{}) or {}
        if args.get("BUILD_REVISION")!=revision:raise Error(f"Compose build revision mismatch: {service}")
        if Path(str(build.get("context", ""))).resolve()!=expected_context.resolve():raise Error(f"Compose build context differs from tracked source: {service}")
        if build.get("additional_contexts"):raise Error(f"unexpected external Compose build context: {service}")
    waiting=services["waiting-pumpkin"]
    if waiting.get("build") or waiting.get("image")!=f"{WAITING_PREFIX}{revision}":
        raise Error("waiting Pumpkin service does not run the release-built revision-pinned image")
    panel_ports=services["panel"].get("ports",[])
    if not any((isinstance(port,dict) and port.get("host_ip")=="127.0.0.1") or str(port).startswith("127.0.0.1:") for port in panel_ports):raise Error("panel port is not bound to localhost")

def _wait_sync_locked(revision:str,pumpkin_image:str,baseline_state:dict[str,list[dict[str,str]]],expected:dict[str,str],token:str,timeout:int=900)->dict[str,str]:
    require_lock(token)
    if not valid_sha(revision):raise Error("invalid release SHA")
    verify_build_context(CHECKOUT,revision,expected,deployed=True)
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        current=states();cp=current["control-plane"];gw=current["gateway"]
        replaced=lambda name:len(current[name])==1 and current[name][0]["id"] not in {x["id"] for x in baseline_state[name]}
        healthy=(replaced("control-plane") and replaced("gateway") and len(cp)==len(gw)==1 and cp[0]["image"]==gw[0]["image"] and cp[0]["revision"]==gw[0]["revision"]==revision and cp[0]["status"]==gw[0]["status"]=="running" and cp[0]["health"]=="healthy")
        healthy=healthy and all(len(current[name])==1 and current[name][0]["status"]=="running" and current[name][0]["health"]=="healthy" and current[name][0]["revision"]==revision for name in ("panel","waiting-pumpkin"))
        healthy=healthy and len(current["postgres"])==1 and current["postgres"][0]["status"]=="running" and current["postgres"][0]["health"]=="healthy"
        if healthy:break
        time.sleep(5)
    else:raise Error("Dokploy services did not become healthy at exact release SHA")
    image=cp[0]["image"];app_hashes,new_worker=verify_application_image(image,revision)
    verify_image_revision(current["panel"][0]["image"],revision)
    upstream=run(["git","-C",str(CHECKOUT),"show",f"{revision}:runtime/pumpkin/REVISION"]).strip()
    pumpkin_id,pumpkin_sha=verify_pumpkin(pumpkin_image,revision,upstream)
    verify_waiting(current["waiting-pumpkin"][0],revision,upstream,pumpkin_image)
    database_id=current["postgres"][0]["id"]
    proxy_image=worker_setting("WORKER_RUNTIME_API_PROXY_IMAGE")
    if proxy_image:
        # The runtime API proxy runs next to each server from the app image.
        if run(["docker","image","inspect",proxy_image,"--format","{{.Id}}"])!=image:raise Error("worker runtime API proxy image is not the deployed app image")
        publish_image_to_worker(proxy_image)
    old_env=WORKER_ENV.read_bytes()
    old_image=worker_image_value()
    old_hash=digest(WORKER)
    def verify_new():
        pid=single_worker()
        if digest(Path(f"/proc/{pid}/exe"))!=app_hashes["worker_sha256"]:raise Error("new worker binary hash mismatch")
        if worker_image()!=pumpkin_image:raise Error("worker environment did not switch to the verified Pumpkin image")
        if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=pumpkin_image:raise Error("running worker process did not load the verified Pumpkin image")
        time.sleep(35)
        pid=single_worker()
        if digest(Path(f"/proc/{pid}/exe"))!=app_hashes["worker_sha256"]:raise Error("worker binary changed during lease check")
        if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=pumpkin_image:raise Error("worker process environment changed during lease check")
        lease(database_id)
        final=states()
        if any(len(final[name])!=1 or final[name][0]["id"]!=current[name][0]["id"] for name in SERVICES):raise Error("Compose service changed during worker update")
        if any(final[name][0]["status"]!="running" or (name!="gateway" and final[name][0]["health"]!="healthy") for name in SERVICES):raise Error("Compose health changed during worker update")
        verify_waiting(final["waiting-pumpkin"][0],revision,upstream,pumpkin_image)
    def verify_old():
        time.sleep(35)
        pid=single_worker()
        if digest(Path(f"/proc/{pid}/exe"))!=old_hash:raise Error("rollback worker binary hash mismatch")
        if digest(WORKER_ENV)!=hashlib.sha256(old_env).hexdigest() or worker_image_value()!=old_image:raise Error("rollback worker environment mismatch")
        if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=old_image:raise Error("worker process environment rollback mismatch")
        lease(database_id)
    worker=worker_transaction(new_worker,pumpkin_image,token=token,revision=revision,restart=lambda:run(["systemctl","restart",WORKER_SERVICE]),verify_new=verify_new,verify_old=verify_old)
    return {"status":"verified","image_id":image,"pumpkin_image":pumpkin_image,
        "pumpkin_image_id":pumpkin_id,"pumpkin_sha256":pumpkin_sha,
        "service_image_ids":{name: current[name][0]["image"] for name in SERVICES},
        **app_hashes,**worker,"worker_id":worker_id(),"worker_lease_verified":"true",
        "worker_pid":str(single_worker())}

def wait_sync(revision:str,pumpkin_image:str,baseline_state:dict[str,list[dict[str,str]]],expected:dict[str,str],token:str,timeout:int=900)->dict[str,str]:
    with OperationLock():
        return _wait_sync_locked(revision,pumpkin_image,baseline_state,expected,token,timeout)


def _verify_app_revision_locked(revision: str, expected_ids: dict[str, list[dict[str, str]]], token: str) -> dict[str, str]:
    require_lock(token)
    if not valid_sha(revision):raise Error("rollback app revision is not a full SHA")
    for service in ("control-plane","gateway","panel","waiting-pumpkin"):
        prior=expected_ids.get(service,[])
        if len(prior)!=1 or prior[0].get("revision")!=revision:
            raise Error("rollback revision does not match the pre-deploy production snapshot")
    state=states()
    cp,gw=state["control-plane"],state["gateway"]
    if len(cp)!=1 or len(gw)!=1 or cp[0]["image"]!=gw[0]["image"] or cp[0]["revision"]!=revision or gw[0]["revision"]!=revision:
        raise Error("control-plane/gateway rollback does not match prior accepted revision")
    if cp[0]["status"]!="running" or gw[0]["status"]!="running" or cp[0]["health"]!="healthy":raise Error("rolled-back app services are unhealthy")
    if any(len(state[name])!=1 or state[name][0]["status"]!="running" or state[name][0]["health"]!="healthy" or state[name][0]["revision"]!=revision for name in ("panel","waiting-pumpkin")):
        raise Error("rolled-back panel/waiting services do not match prior accepted revision")
    if len(state["postgres"])!=1 or state["postgres"][0]["status"]!="running" or state["postgres"][0]["health"]!="healthy":raise Error("database health failed after app rollback")
    app_worker_sha=verify_worker_binary_in_app_image(cp[0]["image"],revision)
    verify_image_revision(state["panel"][0]["image"],revision)
    upstream=run(["git","-C",str(CHECKOUT),"show",f"{revision}:runtime/pumpkin/REVISION"]).strip()
    pinned=worker_image()
    if pinned!=f"{PUMPKIN_PREFIX}{revision}":raise Error("rollback worker Pumpkin image differs from prior app revision")
    verify_waiting(state["waiting-pumpkin"][0],revision,upstream,pinned)
    pid=single_worker()
    if digest(Path(f"/proc/{pid}/exe"))!=app_worker_sha:raise Error("rollback worker executable differs from prior app image")
    if worker_process_environment(pid).get("PUMPKIN_IMAGE")!=pinned:raise Error("rollback worker process environment differs from prior app revision")
    lease(state["postgres"][0]["id"])
    return {"status":"app-rolled-back","revision":revision,"worker_pid":str(pid),"worker_image":pinned}


def verify_app_revision(revision: str, expected_ids: dict[str, list[dict[str, str]]], token: str) -> dict[str, str]:
    with OperationLock():
        return _verify_app_revision_locked(revision,expected_ids,token)


def mark(receipt:str,token:str)->dict[str,str]:
    require_lock(token);data=json.loads(receipt)
    required=("schema","source_revision","deployment_id","image_id","control_plane_sha256","gateway_sha256",
              "worker_sha256","worker_env_sha256","worker_id","worker_lease_verified","pumpkin_image_id",
              "pumpkin_image","pumpkin_sha256","worker_pid","rollback_worker_sha256",
              "rollback_worker_env_sha256","release_tag","source_archive_sha256","host_control_pumpkin_image",
              "migration_manifest_sha256","environment_revision","environment_keys","manifest_sha256",
              "service_image_ids")
    if any(key not in data or data.get(key) in (None, "", []) for key in required):
        raise Error("release receipt is incomplete")
    validate_release_manifest(data)
    revision=data["source_revision"]
    if not valid_sha(revision) or data["release_tag"]!=f"kurbis-release-{revision}":raise Error("release receipt source SHA/tag is invalid")
    if data.get("worker_lease_verified")!="true" or data.get("host_control_pumpkin_image")!=data.get("pumpkin_image_id"):
        raise Error("release receipt worker or host-control image provenance is inconsistent")
    for key in ("control_plane_sha256","gateway_sha256","worker_sha256","pumpkin_sha256","rollback_worker_sha256","rollback_worker_env_sha256","source_archive_sha256"):
        if not re.fullmatch(r"[0-9a-f]{64}",str(data.get(key,""))):raise Error(f"release receipt has invalid digest: {key}")
    current=states();cp=current["control-plane"];gw=current["gateway"]
    expected_images=data["service_image_ids"]
    if any(len(current[name])!=1 or current[name][0]["image"]!=expected_images[name] for name in SERVICES):
        raise Error("active service image differs from verified release manifest")
    if len(cp)!=1 or len(gw)!=1 or cp[0]["image"]!=gw[0]["image"] or cp[0]["image"]!=data["image_id"]:raise Error("active application image differs from verified receipt")
    if cp[0]["revision"]!=revision or gw[0]["revision"]!=revision:raise Error("active application revision differs from verified receipt")
    if any(len(current[name])!=1 or current[name][0]["status"]!="running" or (name!="gateway" and current[name][0]["health"]!="healthy") for name in SERVICES):raise Error("production service health failed before acceptance marker")
    if any(current[name][0]["revision"]!=revision for name in ("panel","waiting-pumpkin")):raise Error("panel/waiting container revision changed before acceptance marker")
    verify_image_revision(current["panel"][0]["image"],revision)
    hashes,_=verify_application_image(data["image_id"],revision)
    if any(data.get(key)!=value for key,value in hashes.items()):raise Error("application executable hashes differ from receipt")
    pid=single_worker()
    if str(pid)!=str(data.get("worker_pid")) or digest(Path(f"/proc/{pid}/exe"))!=data["worker_sha256"]:
        raise Error("worker PID/hash changed before release marker")
    if digest(WORKER_ENV)!=data["worker_env_sha256"]:
        raise Error("worker environment digest changed before release marker")
    if identity()!=data.get("worker_id") or worker_image()!=data["pumpkin_image"]:raise Error("worker identity/environment differs from verified receipt")
    lease(current["postgres"][0]["id"])
    upstream=run(["git","-C",str(CHECKOUT),"show",f"{revision}:runtime/pumpkin/REVISION"]).strip()
    pumpkin_id,pumpkin_sha=verify_pumpkin(data["pumpkin_image"],revision,upstream)
    if pumpkin_id!=data["pumpkin_image_id"] or pumpkin_sha!=data.get("pumpkin_sha256"):raise Error("Pumpkin digest differs from verified receipt")
    verify_waiting(current["waiting-pumpkin"][0],revision,upstream,data["pumpkin_image"])
    if ROOT.is_symlink() or ROOT.stat().st_uid!=0 or ROOT.stat().st_mode&0o022:raise Error("Kurbis release marker root is unsafe")
    tx_path=transaction_path(token)
    if not tx_path.is_file() or tx_path.is_symlink():raise Error("pending worker transaction is missing")
    tx=json.loads(tx_path.read_text(encoding="utf-8"))
    if (tx.get("source_revision")!=revision
        or tx.get("old_worker_sha256")!=data["rollback_worker_sha256"]
        or tx.get("old_worker_env_sha256")!=data["rollback_worker_env_sha256"]
        or tx.get("new_worker_env_sha256")!=data["worker_env_sha256"]):
        raise Error("worker transaction and release receipt provenance differ")
    tx["deployment_id"]=data["deployment_id"]
    atomic(tx_path,(json.dumps(tx,sort_keys=True)+"\n").encode(),0o600,0,0)
    persist_immutable_release_manifest(data)
    atomic(MARKER,(json.dumps(data,sort_keys=True)+"\n").encode(),0o600,0,0)
    return {"status":"marked","path":str(MARKER)}


def finalize_worker(token: str, revision: str, receipt: str) -> dict[str, str]:
    require_lock(token)
    data=json.loads(receipt)
    validate_release_manifest(data)
    if data.get("source_revision")!=revision or data.get("status")!="verified":raise Error("finalized worker receipt does not match release")
    marker=json.loads(MARKER.read_text(encoding="utf-8"))
    if (marker.get("source_revision")!=revision
        or marker.get("deployment_id")!=data.get("deployment_id")
        or marker.get("manifest_sha256")!=data.get("manifest_sha256")
        or release_manifest_hash(marker)!=marker.get("manifest_sha256")):
        raise Error("acceptance marker does not match worker transaction")
    persist_immutable_release_manifest(data)
    tx_path=transaction_path(token)
    if not tx_path.is_file() or tx_path.is_symlink():raise Error("pending worker transaction is missing")
    tx=json.loads(tx_path.read_text(encoding="utf-8"))
    if tx.get("source_revision")!=revision or tx.get("deployment_id")!=data.get("deployment_id"):raise Error("worker transaction provenance mismatch")
    tx["status"]="accepted"
    atomic(tx_path,(json.dumps(tx,sort_keys=True)+"\n").encode(),0o600,0,0)
    return {"status":"finalized"}

def main() -> int:
    parser=argparse.ArgumentParser(); parser.add_argument("--remote",required=True); parser.add_argument("--token"); parser.add_argument("--revision"); parser.add_argument("--expected"); parser.add_argument("--baseline"); parser.add_argument("--pumpkin"); parser.add_argument("--upstream"); parser.add_argument("--receipt"); parser.add_argument("--release-environment"); parser.add_argument("--artifacts"); args=parser.parse_args()
    try:
        expected=json.loads(args.expected) if args.expected else None
        if args.remote=="lock": result=lock(args.token,True)
        elif args.remote=="unlock": result=lock(args.token,False)
        elif args.remote=="baseline": result={"status":"snapshot","snapshot":baseline()}
        elif args.remote=="baseline-preflight": result=baseline_preflight()
        elif args.remote=="bootstrap-worker-image": result=bootstrap_worker_image(args.token,args.revision,args.pumpkin,args.upstream)
        elif args.remote=="verify-app-revision": result=verify_app_revision(args.revision,json.loads(args.baseline),args.token)
        elif args.remote=="stage": result=stage(args.revision,args.token,expected)
        elif args.remote=="verify-build-context":
            require_lock(args.token)
            release_environment=json.loads(args.release_environment) if args.release_environment else None
            verify_build_context(CHECKOUT,args.revision,expected,release_environment=release_environment)
            result={"status":"build-context-verified","revision":args.revision}
        elif args.remote=="restore": result=restore(args.token)
        elif args.remote=="build-pumpkin": result=build_pumpkin(args.revision,expected,args.token,json.loads(args.artifacts))
        elif args.remote=="wait": result=wait_sync(args.revision,args.pumpkin,json.loads(args.baseline),expected,args.token)
        elif args.remote=="mark": result=mark(args.receipt,args.token)
        elif args.remote=="rollback-worker": result=rollback_worker(args.token)
        elif args.remote=="finalize-worker": result=finalize_worker(args.token,args.revision,args.receipt)
        elif args.remote=="accept-stage":
            require_lock(args.token)
            result=restore(args.token)
            if result.get("status")!="restored":raise Error("staged checkout changed unexpectedly; preserved backup for inspection")
            result["status"]="accepted"
        else: raise Error("unknown rollout operation")
        print(json.dumps(result,sort_keys=True)); return 0
    except (Error, OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as e:
        print(f"rollout rejected: {e}",file=__import__('sys').stderr); return 1

if __name__=="__main__": raise SystemExit(main())
