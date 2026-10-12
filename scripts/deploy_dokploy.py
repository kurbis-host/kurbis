#!/usr/bin/env python3
"""Release exact origin/main through Dokploy and synchronize the respond5 worker."""
from __future__ import annotations

import fcntl
import json
import os
import secrets
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Sequence

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pumpkin_artifacts
from migration_manifest import validate_manifest
from release_manifest import build_release_manifest

ROOT = Path(__file__).resolve().parents[1]
MIGRATION_MANIFEST = ROOT / "migrations/MANIFEST.json"
REMOTE = ROOT / "scripts/dokploy_worker_sync.py"
REMOTE_RELEASE_ARTIFACTS = "/srv/kurbis/release-artifacts"
WORKER_TLS = ROOT / "scripts/dokploy_worker_tls.py"
COMPOSE_ID = "KZyAZl_v1hgoqWi-W77ot"
PROJECT = "kurbis-hoster-fbikvd"
EXPECTED_SOURCE = ("github", "kurbis-host", "kurbis", "main", "./compose.yaml")
DEFAULT_REMOTE_COMMAND_TIMEOUT_SECONDS = 5400
REMOTE_COMMAND_TIMEOUT_ENV = "KURBIS_REMOTE_COMMAND_TIMEOUT_SECONDS"
MIN_REMOTE_COMMAND_TIMEOUT_SECONDS = 1800
MAX_REMOTE_COMMAND_TIMEOUT_SECONDS = 24 * 60 * 60

SERVICES = {
    "control-plane": "control-plane", "gateway": "gateway", "panel": "panel",
    "waiting-pumpkin": "waiting-pumpkin", "postgres": "postgres",
}
TERMINAL = {"done", "error", "failed", "cancelled", "canceled"}
ENV_UPDATES = {
    "BEDROCK_TLS_HOST_DIRECTORY": "/srv/kurbis/certs",
    "PANEL_BIND_ADDRESS": "127.0.0.1",
}


class DeploymentError(RuntimeError):
    pass


class DeploymentRollbackError(DeploymentError):
    def __init__(self, message: str, *, app_rolled_back: bool, worker_rolled_back: bool = False):
        super().__init__(message)
        self.app_rolled_back = app_rolled_back
        self.worker_rolled_back = worker_rolled_back


def run(command: Sequence[str], *, input_text: str | None = None, cwd: Path | None = None, timeout: int | None = None) -> str:
    result = subprocess.run(
        list(command), input=input_text, cwd=cwd, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=timeout,
    )
    if result.returncode:
        raise DeploymentError(f"{command[0]} failed: {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout.strip()


def remote_command_timeout_seconds() -> int:
    """Return the bounded SSH lifetime for long-running remote operations.

    Remote verification and packaging steps emit no output while they run.
    Keep this timeout generous while retaining an operator override. The lower
    bound deliberately exceeds the old 30-minute cutoff so an accidental
    override cannot recreate the premature SSH disconnect that left a remote
    operation ambiguous.
    """
    raw = os.environ.get(REMOTE_COMMAND_TIMEOUT_ENV)
    if raw is None or not raw.strip():
        return DEFAULT_REMOTE_COMMAND_TIMEOUT_SECONDS
    try:
        value = int(raw, 10)
    except ValueError as error:
        raise DeploymentError(
            f"{REMOTE_COMMAND_TIMEOUT_ENV} must be an integer from "
            f"{MIN_REMOTE_COMMAND_TIMEOUT_SECONDS} to {MAX_REMOTE_COMMAND_TIMEOUT_SECONDS} seconds"
        ) from error
    if not MIN_REMOTE_COMMAND_TIMEOUT_SECONDS <= value <= MAX_REMOTE_COMMAND_TIMEOUT_SECONDS:
        raise DeploymentError(
            f"{REMOTE_COMMAND_TIMEOUT_ENV} must be from "
            f"{MIN_REMOTE_COMMAND_TIMEOUT_SECONDS} to {MAX_REMOTE_COMMAND_TIMEOUT_SECONDS} seconds"
        )
    return value


def release_revision() -> str:
    commit = run(["git", "-C", str(ROOT), "rev-parse", "HEAD"])
    if len(commit) != 40 or any(character not in "0123456789abcdef" for character in commit):
        raise DeploymentError("release must be a full Git SHA")
    if run(["git", "-C", str(ROOT), "status", "--porcelain", "--untracked-files=all"]):
        raise DeploymentError("refusing to release a dirty or untracked checkout")
    pumpkin_dockerfile = ROOT / "runtime/pumpkin/Dockerfile"
    for dockerfile in (ROOT / "runtime/Dockerfile", pumpkin_dockerfile):
        text = dockerfile.read_text(encoding="utf-8")
        if dockerfile != pumpkin_dockerfile:
            if "target=/source/target" in text:
                raise DeploymentError("mutable Cargo target cache remains in application Dockerfile")
            continue
        # The Pumpkin host-control image may cache its target only under the
        # immutable source SHA. That keeps retries warm without crossing
        # release boundaries or weakening the application's stale-artifact
        # protection.
        scoped_target_mount = (
            "id=kurbis-pumpkin-target-${BUILD_REVISION},"
            "target=/source/target,sharing=locked"
        )
        if scoped_target_mount not in text:
            raise DeploymentError("Pumpkin Cargo target cache is not scoped to BUILD_REVISION")
    remote = run(["git", "-C", str(ROOT), "ls-remote", "origin", "refs/heads/main"], timeout=30).split()
    if not remote or remote[0] != commit:
        raise DeploymentError("release checkout is not exactly origin/main")
    return commit


def migration_manifest_sha256() -> str:
    try:
        return validate_manifest(ROOT / "migrations", MIGRATION_MANIFEST)
    except ValueError as error:
        raise DeploymentError(f"migration manifest is not immutable or current: {error}") from error


def source_inputs(commit: str) -> dict[str, str]:
    paths = run(["git", "-C", str(ROOT), "ls-tree", "-r", "--name-only", commit]).splitlines()
    return {path: run(["git", "-C", str(ROOT), "rev-parse", f"{commit}:{path}"]) for path in paths}


def cli_credentials() -> tuple[str, str]:
    url = os.getenv("DOKPLOY_URL")
    token = os.getenv("DOKPLOY_API_KEY") or os.getenv("DOKPLOY_AUTH_TOKEN")
    if url and token:
        return url.rstrip("/"), token
    executable = shutil.which(os.getenv("DOKPLOY_CLI", "dokploy"))
    if not executable:
        raise DeploymentError("Dokploy CLI is not installed")
    config_path = Path(executable).resolve().parent.parent / "config.json"
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise DeploymentError("Dokploy CLI authentication is unavailable") from error
    if not isinstance(config.get("url"), str) or not isinstance(config.get("token"), str):
        raise DeploymentError("Dokploy CLI authentication is incomplete")
    return config["url"].rstrip("/"), config["token"]


def api(endpoint: str, params: dict, *, post: bool = False) -> object:
    base, token = cli_credentials()
    body = json.dumps({"json": params}, separators=(",", ":")).encode()
    headers = {"x-api-key": token, "User-Agent": "axios/1.8.4", "Content-Type": "application/json"}
    if post:
        request = urllib.request.Request(f"{base}/api/trpc/{endpoint}", data=body, headers=headers, method="POST")
    else:
        query = urllib.parse.urlencode({"input": body.decode()})
        request = urllib.request.Request(f"{base}/api/trpc/{endpoint}?{query}", headers=headers)
    try:
        response = json.loads(urllib.request.urlopen(request, timeout=30).read())
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
        raise DeploymentError(f"Dokploy API request failed: {endpoint}") from error
    if "error" in response:
        raise DeploymentError(f"Dokploy rejected {endpoint}")
    result = response.get("result", {}).get("data", {})
    return result.get("json", result)


def validate_compose_execution(info: dict) -> None:
    # Dokploy's createCommand uses this generated command only when `command` is
    # unset. A populated command overrides the default, even when it resembles it.
    if info.get("composeType") != "docker-compose":
        raise DeploymentError("Dokploy target must use the documented docker-compose mode")
    if "command" not in info or info.get("command") not in (None, ""):
        raise DeploymentError("Dokploy custom Compose commands are not allowed; leave the command unset")
    default_command = api("compose.getDefaultCommand", {"composeId": COMPOSE_ID})
    if isinstance(default_command, dict):
        default_command = default_command.get("command")
    if not isinstance(default_command, str) or not default_command.strip():
        raise DeploymentError("Dokploy documented default Compose command is unavailable")
    try:
        tokens = shlex.split(default_command)
    except ValueError as error:
        raise DeploymentError("Dokploy documented default Compose command is malformed") from error
    if len(tokens) < 2 or tokens[:2] != ["docker", "compose"]:
        raise DeploymentError("Dokploy default command is not the documented Docker Compose command")
    if "up" not in tokens or "-d" not in tokens or "--build" not in tokens or "--no-build" in tokens:
        raise DeploymentError("Dokploy default command is not the documented build-enabled Compose command")
    if any(token in {";", "|", "&&", "||", ">", "<"} for token in tokens):
        raise DeploymentError("Dokploy default Compose command contains shell control operators")


def validate_compose_patches(info: dict) -> None:
    # Dokploy applies enabled patches after cloning the Git source and before
    # running Compose. A stale compose.yaml patch therefore silently replaces
    # the current source with an older normalized document (and can remove
    # required environment, build, and mount entries). Keep file-specific
    # overrides out of the production release path; source control is the sole
    # authority for the Compose definition.
    patches = api("patch.byEntityId", {"id": COMPOSE_ID, "type": "compose"})
    if not isinstance(patches, list) or any(not isinstance(patch, dict) for patch in patches):
        raise DeploymentError("Dokploy Compose patch inventory is malformed")
    compose_path = os.path.normpath(str(info.get("composePath", "./compose.yaml")))
    enabled = [
        patch for patch in patches
        if patch.get("enabled") is True
        and os.path.normpath(str(patch.get("filePath", ""))) == compose_path
    ]
    if enabled:
        patch_ids = ", ".join(str(patch.get("patchId", "<unknown>")) for patch in enabled)
        raise DeploymentError(
            "Dokploy has enabled production Compose override patch(es) for "
            f"{compose_path}: {patch_ids}; remove them before deploying source"
        )


def compose_info() -> dict:
    info = api("compose.one", {"composeId": COMPOSE_ID})
    if not isinstance(info, dict) or info.get("composeId") != COMPOSE_ID or info.get("appName") != PROJECT:
        raise DeploymentError("Dokploy target is not the approved Kurbis production stack")
    source = (info.get("sourceType"), info.get("owner"), info.get("repository"), info.get("composePath"))
    if source != ("github", "kurbis-host", "kurbis", "./compose.yaml") or not isinstance(info.get("branch"), str):
        raise DeploymentError("Dokploy Git repository or Compose path differs from Kurbis production")
    if not isinstance(info.get("env"), str) or info.get("environment", {}).get("name") != "production":
        raise DeploymentError("Kurbis production Dokploy environment is unavailable")
    validate_compose_patches(info)
    validate_compose_execution(info)
    return info


def patch_env(original: str, changes: dict[str, str]) -> str:
    lines = original.splitlines()
    output: list[str] = []
    seen: set[str] = set()
    for line in lines:
        key = line.split("=", 1)[0].strip() if "=" in line and not line.lstrip().startswith("#") else None
        if key in changes:
            if key not in seen:
                output.append(f"{key}={changes[key]}")
                seen.add(key)
            continue
        output.append(line)
    output.extend(f"{key}={value}" for key, value in changes.items() if key not in seen)
    return "\n".join(output).rstrip("\n") + "\n"


def remove_env_key(original: str, key_to_remove: str) -> str:
    return "\n".join(
        line for line in original.splitlines()
        if line.split("=", 1)[0].strip() != key_to_remove or line.lstrip().startswith("#")
    ).rstrip("\n") + "\n"


def restore_build_revision(previous_env: str) -> None:
    info = compose_info()
    old = parse_env(previous_env)
    current = str(info["env"])
    restored = patch_env(current, {"BUILD_REVISION": old["BUILD_REVISION"]}) if "BUILD_REVISION" in old else remove_env_key(current, "BUILD_REVISION")
    api("compose.saveEnvironment", {"composeId": COMPOSE_ID, "env": restored}, post=True)


def prepare_dokploy(revision: str) -> tuple[str, str]:
    info = compose_info()
    old_branch, old_env = str(info.get("branch", "main")), str(info["env"])
    if old_branch != "main":
        raise DeploymentError("Dokploy production source branch is not main")
    if info.get("autoDeploy") is not False:
        api("compose.update", {"composeId": COMPOSE_ID, "autoDeploy": False}, post=True)
    updates = {**ENV_UPDATES, "BUILD_REVISION": revision}
    api("compose.saveEnvironment", {"composeId": COMPOSE_ID, "env": patch_env(old_env, updates)}, post=True)
    updated = compose_info()
    values = parse_env(str(updated["env"]))
    if updated.get("autoDeploy") is not False or any(values.get(key) != value for key, value in updates.items()):
        raise DeploymentError("Dokploy auto-deploy/build environment settings did not persist")
    return old_branch, old_env


def parse_env(value: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in value.splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            key, item = line.split("=", 1)
            result[key.strip()] = item.strip().strip("\"'")
    return result


def set_branch(branch: str) -> None:
    run([os.getenv("DOKPLOY_CLI", "dokploy"), "compose", "update", "--composeId", COMPOSE_ID, "--branch", branch, "--json"], timeout=60)
    updated = compose_info()
    if updated.get("branch") != branch:
        raise DeploymentError("Dokploy did not persist the requested source branch")
    if updated.get("autoDeploy") is not False:
        raise DeploymentError("Dokploy auto-deploy policy unexpectedly became enabled")


def ensure_release_tag(commit: str) -> str:
    tag = f"kurbis-release-{commit}"
    local = subprocess.run(["git", "-C", str(ROOT), "rev-parse", f"refs/tags/{tag}^{{commit}}"], text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False)
    if local.returncode == 0 and local.stdout.strip() != commit:
        raise DeploymentError("local immutable release tag points to another commit")
    remote = run(["git", "-C", str(ROOT), "ls-remote", "origin", f"refs/tags/{tag}"]).split()
    if remote and remote[0] != commit:
        raise DeploymentError("remote immutable release tag points to another commit")
    if local.returncode:
        run(["git", "-C", str(ROOT), "tag", tag, commit])
    if not remote:
        run(["git", "-C", str(ROOT), "push", "origin", f"refs/tags/{tag}"])
    return tag


def deployment_list() -> list[dict]:
    result = api("deployment.allByCompose", {"composeId": COMPOSE_ID})
    if not isinstance(result, list) or any(not isinstance(item, dict) for item in result):
        raise DeploymentError("Dokploy deployment history response is malformed")
    return result


def assert_no_active_deployments() -> set[str]:
    records = deployment_list()
    active = [item for item in records if str(item.get("status", "")).lower() not in TERMINAL]
    if active:
        raise DeploymentError("Dokploy already has a non-terminal production deployment")
    return {str(item["deploymentId"]) for item in records if isinstance(item.get("deploymentId"), str)}


def wait_for_terminal_deployments(timeout: int) -> None:
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        records=deployment_list()
        active=[item for item in records if str(item.get("status","")).lower() not in TERMINAL]
        if not active:
            return
        time.sleep(5)
    raise DeploymentError("an active Dokploy deployment did not finish before rollback")


def run_release_tests() -> None:
    migration_manifest_sha256()
    if not os.environ.get("TEST_DATABASE_URL"):
        raise DeploymentError("TEST_DATABASE_URL is required for the disposable first-join database regression")
    run([sys.executable, "-m", "unittest", "discover", "-s", "tools", "-p", "test_dokploy_worker_rollout.py"], cwd=ROOT, timeout=300)
    run([sys.executable, "-m", "unittest", "discover", "-s", "tools", "-p", "test_release_manifest.py"], cwd=ROOT, timeout=300)
    run(["cargo", "test", "--locked", "--test", "first_join_flow", "--", "--test-threads=1"], cwd=ROOT, timeout=900)


def wait_deployment(commit: str, previous: set[str], timeout: int) -> str:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        records = [item for item in deployment_list() if str(item.get("deploymentId")) not in previous]
        if len(records) > 1:
            raise DeploymentError("overlapping/superseding Dokploy deployment detected")
        if records:
            item = records[0]
            deployment_id = str(item["deploymentId"])
            if commit not in str(item.get("description", "")):
                raise DeploymentError("Dokploy deployment does not identify exact source SHA")
            status = str(item.get("status", "")).lower()
            if status in {"error", "failed", "cancelled", "canceled"}:
                raise DeploymentError(f"Dokploy deployment failed: {status}")
            if status == "done":
                logs = api("deployment.readLogs", {"deploymentId": deployment_id, "tail": 4000})
                if not isinstance(logs, str) or "Docker Compose Deployed:" not in logs or "target=/source/target" in logs:
                    raise DeploymentError("Dokploy logs do not prove a clean Compose build succeeded")
                return deployment_id
        time.sleep(5)
    raise DeploymentError("Dokploy deployment timed out")


def worker_sync_source() -> str:
    source=REMOTE.read_text(encoding="utf-8")
    marker="# __TLS_HELPERS__"
    if source.count(marker)!=1:
        raise DeploymentError("Respond5 worker TLS verification injection point is invalid")
    helper=WORKER_TLS.read_text(encoding="utf-8")
    local_loader='''if __file__ != "<stdin>":
    _tls_module=Path(__file__).with_name("dokploy_worker_tls.py")
    exec(compile(_tls_module.read_text(encoding="utf-8"),str(_tls_module),"exec"),globals())
'''
    if source.count(local_loader)!=1:
        raise DeploymentError("Respond5 worker TLS verification local loader is invalid")
    return source.replace(local_loader,"").replace(marker,helper)


def remote(host: str, operation: str, *, token: str | None = None, revision: str | None = None,
           expected: dict | None = None, baseline: dict | None = None,
           pumpkin: str | None = None, upstream: str | None = None,
           receipt: dict | None = None, release_environment: dict | None = None,
           artifacts: dict | None = None) -> dict:
    command = ["python3", "-", "--remote", operation]
    for key, value in (("token", token), ("revision", revision), ("expected", expected),
                       ("baseline", baseline), ("pumpkin", pumpkin), ("upstream", upstream), ("receipt", receipt),
                       ("release-environment",release_environment), ("artifacts", artifacts)):
        if value is not None:
            command.extend([f"--{key}", value if isinstance(value, str) else json.dumps(value, separators=(",", ":"))])
    timeout = remote_command_timeout_seconds()
    # The remote build emits no output while rustc links Pumpkin. Keep the SSH
    # session alive instead of letting an idle connection die and leaving the
    # caller unsure whether a build is still running. The rollout never retries
    # this operation automatically, so one bounded invocation owns the build.
    keepalive_count = max(3, (timeout + 29) // 30 + 2)
    output = run([
        "ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
        "-o", "ServerAliveInterval=30", f"-o", f"ServerAliveCountMax={keepalive_count}",
        host, shlex.join(command),
    ], input_text=worker_sync_source(), timeout=timeout)
    try:
        result = json.loads(output)
    except json.JSONDecodeError as error:
        raise DeploymentError("respond5 verifier returned invalid JSON") from error
    if not isinstance(result, dict):
        raise DeploymentError("respond5 verifier returned invalid result")
    return result

def build_pumpkin_artifacts(host: str, revision: str) -> dict[str, str]:
    """Compile Pumpkin on the remote CI builder and upload the binaries to the worker host."""
    settings = parse_env(str(compose_info()["env"]))
    with tempfile.TemporaryDirectory(prefix="kurbis-pumpkin-artifacts-") as directory:
        local = Path(directory) / "artifacts"
        try:
            digests = pumpkin_artifacts.build(settings, local)
        except pumpkin_artifacts.ArtifactError as error:
            raise DeploymentError(str(error)) from error
        target = f"{REMOTE_RELEASE_ARTIFACTS}/{revision}"
        run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host,
             f"rm -rf -- {shlex.quote(target)} && install -d -m 0700 {shlex.quote(REMOTE_RELEASE_ARTIFACTS)} {shlex.quote(target)}"], timeout=60)
        run(["scp", "-q", "-r", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", str(local), f"{host}:{target}/"], timeout=900)
        run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host,
             f"chown -R root:root {shlex.quote(target)} && chmod 0700 {shlex.quote(target)}/artifacts"], timeout=60)
    return digests


def update_build_revision(revision: str) -> None:
    info=compose_info()
    api("compose.saveEnvironment", {"composeId": COMPOSE_ID, "env": patch_env(str(info["env"]), {"BUILD_REVISION": revision})}, post=True)
    if parse_env(str(compose_info()["env"])).get("BUILD_REVISION")!=revision:
        raise DeploymentError("Dokploy BUILD_REVISION did not persist")


def queue_and_wait_deployment(revision: str, title: str, timeout: int) -> str:
    previous=assert_no_active_deployments()
    output=run([os.getenv("DOKPLOY_CLI", "dokploy"), "compose", "deploy", "--composeId", COMPOSE_ID,
        "--title", title, "--description", f"Deploy exact source revision {revision}", "--json"], timeout=60)
    try:
        queued=json.loads(output)
    except json.JSONDecodeError as error:
        raise DeploymentError("Dokploy CLI returned invalid deployment response") from error
    if not isinstance(queued,dict) or queued.get("success") is not True:
        raise DeploymentError("Dokploy did not confirm deployment queueing")
    return wait_deployment(revision,previous,timeout)


def rollback_application(host: str, branch: str, revision: str, baseline: dict, token: str, timeout: int) -> dict:
    if not revision or len(revision) != 40:
        raise DeploymentError("prior running app revision is unavailable; cannot safely roll back production")
    tag = ensure_release_tag(revision)
    wait_for_terminal_deployments(timeout)
    update_build_revision(revision)
    set_branch(tag)
    deployment_id = queue_and_wait_deployment(revision, f"Rollback to {revision[:12]}", timeout)
    checked = remote(host, "verify-app-revision", token=token, revision=revision, baseline=baseline)
    if checked.get("status") != "app-rolled-back":
        raise DeploymentError("respond5 did not verify restoration of the prior app revision")
    set_branch(branch)
    return {"status": "app-rolled-back", "deployment_id": deployment_id}


def should_rollback_failed_release(*, deployment_attempted: bool, marked: bool) -> bool:
    return deployment_attempted and not marked


def rollout_unlock_safe(*, deployment_attempted: bool, marked: bool, rollback_verified: bool) -> bool:
    if marked or rollback_verified:
        return True
    return not deployment_attempted


def dispatch_worker_wait(remote_call, rollback_call, on_attempt, **kwargs):
    # Set the rollback guard before SSH dispatch: the remote may mutate state and
    # the response may be lost after that mutation completed.
    on_attempt()
    try:
        return remote_call(**kwargs)
    except Exception:
        rollback_call()
        raise


def should_restore_dokploy_source(*, deployment_attempted: bool, app_revision_restored: bool, marked: bool) -> bool:
    return not deployment_attempted or app_revision_restored or marked


def rollback_failed_release(worker_rollback, app_rollback):
    worker_error = None
    try:
        worker=worker_rollback()
        if worker.get("status")=="already-accepted":
            return {"status":"already-accepted"}
        if worker.get("status") not in {"rolled-back","no-pending-worker-transaction"}:
            worker_error=DeploymentError("worker bundle rollback was not verified")
    except Exception as error:
        worker_error=error
    # Always request app rollback, even when worker rollback failed; accepting a mixed
    # state is forbidden, but both independent recovery paths should still be attempted.
    try:
        app=app_rollback()
        if app.get("status")!="app-rolled-back":
            raise DeploymentError("prior app revision rollback was not verified")
    except Exception as app_error:
        if worker_error:
            raise DeploymentError(f"worker rollback failed ({worker_error}); app rollback failed ({app_error})") from app_error
        raise
    if worker_error:
        worker_restored_by_app = bool(app.get("worker_bundle_restored"))
        if worker_restored_by_app:
            return app
        raise DeploymentRollbackError(
            f"prior app revision was restored; worker rollback status was not reconciled by the app baseline: {worker_error}",
            app_rolled_back=True,
            worker_rolled_back=False,
        ) from worker_error
    return app


def main(argv: Sequence[str] | None = None) -> int:
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compose-id", default=COMPOSE_ID)
    parser.add_argument("--worker-host", default="respond5")
    parser.add_argument("--timeout", type=int, default=900)
    args = parser.parse_args(argv)
    token = secrets.token_hex(16)
    locked = staged = prepared = wait_attempted = deployment_attempted = finalization_attempted = marked = False
    old_env = ""
    branch = "main"
    commit = ""
    baseline_revision = ""
    migration_hash = ""
    baseline: dict | None = None
    receipt: dict | None = None
    rollback_verified = False
    rollback_app_revision_restored = False
    try:
        lock_path = Path.home() / ".cache/kurbis/deploy.lock"
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        with lock_path.open("a+") as local_lock:
            try:
                fcntl.flock(local_lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise DeploymentError("another local Kurbis rollout is active") from error
            commit = release_revision()
            if args.compose_id != COMPOSE_ID:
                raise DeploymentError("only approved Kurbis production Compose target is allowed")
            expected = source_inputs(commit)
            migration_hash = migration_manifest_sha256()
            run_release_tests()
            remote(args.worker_host, "lock", token=token)
            locked = True
            preflight = remote(args.worker_host, "baseline-preflight")
            if preflight.get("status") != "preflight" or not isinstance(preflight.get("snapshot"), dict):
                raise DeploymentError("respond5 did not provide a safe pre-bootstrap snapshot")
            baseline_revision = str(preflight.get("app_revision", ""))
            initial_config=compose_info()
            branch,old_env=str(initial_config.get("branch","main")),str(initial_config.get("env",""))
            if branch!="main":raise DeploymentError("Dokploy production source branch is not main")
            prepared=True
            prepare_dokploy(commit)
            ensure_release_tag(commit)
            staged=True
            staged_result=remote(args.worker_host,"stage",token=token,revision=commit,expected=expected)
            if staged_result.get("status")!="staged" or staged_result.get("revision")!=commit:
                raise DeploymentError("respond5 did not stage the exact tagged source tree")
            tls_context=remote(args.worker_host,"verify-build-context",token=token,revision=commit,expected=expected,
                               release_environment={"BUILD_REVISION":commit,**ENV_UPDATES})
            if tls_context.get("status")!="build-context-verified" or tls_context.get("revision")!=commit:
                raise DeploymentError("Respond5 did not verify release build inputs and Bedrock TLS assets before deployment")
            artifacts=build_pumpkin_artifacts(args.worker_host,commit)
            pumpkin=remote(args.worker_host,"build-pumpkin",token=token,revision=commit,expected=expected,artifacts=artifacts)
            if pumpkin.get("status")!="built" or pumpkin.get("pumpkin_image")!=f"melon-pumpkin:host-control-{commit}":
                raise DeploymentError("respond5 host-control Pumpkin image is not pinned to clean source SHA")
            baseline_result = remote(args.worker_host, "baseline")
            baseline = baseline_result.get("snapshot")
            if baseline_result.get("status") != "snapshot" or not baseline:
                raise DeploymentError("respond5 strict baseline service/worker lease check failed")
            tag=ensure_release_tag(commit)
            set_branch(tag)
            update_build_revision(commit)
            deployment_attempted=True
            deployment_id=queue_and_wait_deployment(commit,f"Deploy {commit[:12]}",args.timeout)
            def mark_wait_attempted() -> None:
                nonlocal wait_attempted
                wait_attempted=True
            receipt=dispatch_worker_wait(
                lambda **kwargs: remote(args.worker_host,"wait",**kwargs),
                lambda: remote(args.worker_host,"rollback-worker",token=token), mark_wait_attempted,
                token=token,revision=commit,pumpkin=pumpkin["pumpkin_image"],baseline=baseline,expected=expected)
            if receipt.get("status")!="verified":raise DeploymentError("respond5 failed image/worker/lease verification")
            receipt.update(status="verified",source_revision=commit,deployment_id=deployment_id,
                           host_control_pumpkin_image=pumpkin.get("pumpkin_image_id"),
                           source_archive_sha256=pumpkin.get("archive_sha256"),release_tag=tag)
            receipt = build_release_manifest(
                receipt,
                migration_manifest_sha256=migration_hash,
                environment_values={"BUILD_REVISION": commit, **ENV_UPDATES},
            )
            if release_revision()!=commit:raise DeploymentError("origin/main or local release checkout changed during rollout")
            set_branch(branch)
            accepted=remote(args.worker_host,"accept-stage",token=token,revision=commit)
            if accepted.get("status")!="accepted":raise DeploymentError("respond5 did not safely restore staged checkout overrides")
            staged=False
            marked_result=remote(args.worker_host,"mark",token=token,receipt=receipt)
            if marked_result.get("status")!="marked":raise DeploymentError("respond5 did not write the verified release marker")
            finalization_attempted=True
            finalized=remote(args.worker_host,"finalize-worker",token=token,revision=commit,receipt=receipt)
            if finalized.get("status")!="finalized":raise DeploymentError("respond5 did not finalize the worker transaction")
            marked=True
            print(json.dumps(receipt,sort_keys=True))
            return 0
    except (DeploymentError,OSError,subprocess.TimeoutExpired) as error:
        if finalization_attempted and locked and receipt is not None:
            try:
                recovery=remote(args.worker_host,"rollback-worker",token=token)
                if recovery.get("status")=="already-accepted":
                    marked=True
                    print(json.dumps(receipt,sort_keys=True))
                    return 0
            except Exception:
                pass
        print(f"deployment not accepted: {error}",file=sys.stderr)
        return 1
    finally:
        # Once a deployment is queued, recovery must verify both the app and the
        # worker before the rollout lock can be released.
        if should_rollback_failed_release(deployment_attempted=deployment_attempted,marked=marked):
            try:
                if baseline is None or not baseline_revision:
                    raise DeploymentError("prior app snapshot was not verified; cannot safely accept mixed release state")
                result=rollback_failed_release(
                    lambda:remote(args.worker_host,"rollback-worker",token=token),
                    lambda:rollback_application(args.worker_host, branch, baseline_revision, baseline, token, args.timeout),
                )
                rollback_app_revision_restored=result.get("status")=="app-rolled-back"
                rollback_verified=rollback_app_revision_restored and bool(result.get("worker_bundle_restored"))
                if result.get("status")=="already-accepted":marked=True
            except Exception as error:
                rollback_app_revision_restored=bool(getattr(error,"app_rolled_back",False))
                rollback_verified=rollback_app_revision_restored and bool(getattr(error,"worker_rolled_back",False))
                print(f"prior production app/worker rollback could not be verified: {error}",file=sys.stderr)
        if staged and locked:
            try:
                remote(args.worker_host,"restore",token=token)
            except Exception as error:
                print(f"Dokploy checkout override restore failed; inspect backup: {error}",file=sys.stderr)
        if prepared and should_restore_dokploy_source(
            deployment_attempted=deployment_attempted,
            app_revision_restored=rollback_app_revision_restored,
            marked=marked,
        ):
            try:
                if compose_info().get("branch")!=branch:set_branch(branch)
                if not marked:
                    if deployment_attempted and rollback_app_revision_restored and baseline_revision:
                        update_build_revision(baseline_revision)
                    elif not rollback_verified:
                        restore_build_revision(old_env)
            except Exception as error:
                print(f"Dokploy release configuration restore failed: {error}",file=sys.stderr)
        elif prepared:
            print("Dokploy immutable rollback source and BUILD_REVISION intentionally retained because app rollback is unverified",file=sys.stderr)
        if locked and rollout_unlock_safe(
            deployment_attempted=deployment_attempted,
            marked=marked,
            rollback_verified=rollback_verified,
        ):
            try:
                remote(args.worker_host, "unlock", token=token)
            except Exception as error:
                print(f"respond5 rollout lock retained: {error}", file=sys.stderr)
        elif locked:
            print("respond5 rollout lock intentionally retained because production rollback is unverified", file=sys.stderr)


if __name__ == "__main__":
    raise SystemExit(main())
