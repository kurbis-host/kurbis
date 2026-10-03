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
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Sequence

ROOT = Path(__file__).resolve().parents[1]
REMOTE = ROOT / "scripts/dokploy_worker_sync.py"
COMPOSE_ID = "KZyAZl_v1hgoqWi-W77ot"
PROJECT = "kurbis-hoster-fbikvd"
EXPECTED_SOURCE = ("github", "kurbis-host", "kurbis", "main", "./compose.yaml")
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


def run(command: Sequence[str], *, input_text: str | None = None, cwd: Path | None = None, timeout: int | None = None) -> str:
    result = subprocess.run(
        list(command), input=input_text, cwd=cwd, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=timeout,
    )
    if result.returncode:
        raise DeploymentError(f"{command[0]} failed: {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout.strip()


def release_revision() -> str:
    commit = run(["git", "-C", str(ROOT), "rev-parse", "HEAD"])
    if len(commit) != 40 or any(character not in "0123456789abcdef" for character in commit):
        raise DeploymentError("release must be a full Git SHA")
    if run(["git", "-C", str(ROOT), "status", "--porcelain", "--untracked-files=all"]):
        raise DeploymentError("refusing to release a dirty or untracked checkout")
    for dockerfile in (ROOT / "runtime/Dockerfile", ROOT / "runtime/pumpkin/Dockerfile"):
        if "target=/source/target" in dockerfile.read_text(encoding="utf-8"):
            raise DeploymentError(f"mutable Cargo target cache remains in {dockerfile.name}")
    remote = run(["git", "-C", str(ROOT), "ls-remote", "origin", "refs/heads/main"], timeout=30).split()
    if not remote or remote[0] != commit:
        raise DeploymentError("release checkout is not exactly origin/main")
    return commit


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


def compose_info() -> dict:
    info = api("compose.one", {"composeId": COMPOSE_ID})
    if not isinstance(info, dict) or info.get("composeId") != COMPOSE_ID or info.get("appName") != PROJECT:
        raise DeploymentError("Dokploy target is not the approved Kurbis production stack")
    source = (info.get("sourceType"), info.get("owner"), info.get("repository"), info.get("composePath"))
    if source != ("github", "kurbis-host", "kurbis", "./compose.yaml") or not isinstance(info.get("branch"), str):
        raise DeploymentError("Dokploy Git repository or Compose path differs from Kurbis production")
    if not isinstance(info.get("env"), str) or info.get("environment", {}).get("name") != "production":
        raise DeploymentError("Kurbis production Dokploy environment is unavailable")
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


def run_release_tests() -> None:
    if not os.environ.get("TEST_DATABASE_URL"):
        raise DeploymentError("TEST_DATABASE_URL is required for the disposable first-join database regression")
    run([sys.executable, "-m", "unittest", "discover", "-s", "tools", "-p", "test_dokploy_worker_rollout.py"], cwd=ROOT, timeout=300)
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


def remote(host: str, operation: str, *, token: str | None = None, revision: str | None = None,
           expected: dict | None = None, baseline: dict | None = None,
           pumpkin: str | None = None, receipt: dict | None = None) -> dict:
    command = ["python3", "-", "--remote", operation]
    for key, value in (("token", token), ("revision", revision), ("expected", expected),
                       ("baseline", baseline), ("pumpkin", pumpkin), ("receipt", receipt)):
        if value is not None:
            command.extend([f"--{key}", value if isinstance(value, str) else json.dumps(value, separators=(",", ":"))])
    output = run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host, shlex.join(command)],
                 input_text=REMOTE.read_text(encoding="utf-8"), timeout=1800)
    try:
        result = json.loads(output)
    except json.JSONDecodeError as error:
        raise DeploymentError("respond5 verifier returned invalid JSON") from error
    if not isinstance(result, dict):
        raise DeploymentError("respond5 verifier returned invalid result")
    return result


def main(argv: Sequence[str] | None = None) -> int:
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compose-id", default=COMPOSE_ID)
    parser.add_argument("--worker-host", default="respond5")
    parser.add_argument("--timeout", type=int, default=900)
    args = parser.parse_args(argv)
    token = secrets.token_hex(16)
    locked = staged = prepared = wait_attempted = marked = False
    old_env = ""
    branch = "main"
    tag = ""
    commit = ""
    receipt: dict | None = None
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
            # Both behavioral suites run before any production setting or checkout is mutated.
            run_release_tests()
            remote(args.worker_host, "lock", token=token)
            locked = True
            baseline_result = remote(args.worker_host, "baseline")
            baseline = baseline_result.get("snapshot")
            if baseline_result.get("status") != "snapshot" or not baseline:
                raise DeploymentError("respond5 baseline service/worker lease check failed")
            initial_config = compose_info()
            branch, old_env = str(initial_config.get("branch", "main")), str(initial_config.get("env", ""))
            if branch != "main":
                raise DeploymentError("Dokploy production source branch is not main")
            prepared = True
            prepare_dokploy(commit)
            tag = ensure_release_tag(commit)
            set_branch(tag)
            staged = True  # remote stage may fail after preserving a backup but before returning
            staged_result = remote(args.worker_host, "stage", token=token, revision=commit, expected=expected)
            if staged_result.get("status") != "staged" or staged_result.get("revision") != commit:
                raise DeploymentError("respond5 did not stage the exact tagged source tree")
            pumpkin = remote(args.worker_host, "build-pumpkin", token=token, revision=commit, expected=expected)
            if pumpkin.get("status") != "built" or pumpkin.get("pumpkin_image") != f"melon-pumpkin:host-control-{commit}":
                raise DeploymentError("respond5 host-control Pumpkin image is not pinned to source SHA")
            previous = assert_no_active_deployments()
            command_output = run([os.getenv("DOKPLOY_CLI", "dokploy"), "compose", "deploy", "--composeId", COMPOSE_ID,
                "--title", f"Deploy {commit[:12]}", "--description", f"Deploy exact origin/main revision {commit}", "--json"], timeout=60)
            try:
                queued = json.loads(command_output)
            except json.JSONDecodeError as error:
                raise DeploymentError("Dokploy CLI returned invalid deployment response") from error
            if not isinstance(queued, dict) or queued.get("success") is not True:
                raise DeploymentError("Dokploy did not confirm deployment queueing")
            deployment_id = wait_deployment(commit, previous, args.timeout)
            wait_attempted = True
            receipt = remote(args.worker_host, "wait", token=token, revision=commit, pumpkin=pumpkin["pumpkin_image"],
                             baseline=baseline, expected=expected)
            if receipt.get("status") != "verified":
                raise DeploymentError("respond5 failed image/worker/lease verification")
            receipt.update(status="verified", source_revision=commit, deployment_id=deployment_id,
                           host_control_pumpkin_image=pumpkin.get("pumpkin_image_id"),
                           source_archive_sha256=pumpkin.get("archive_sha256"), release_tag=tag)
            if release_revision() != commit:
                raise DeploymentError("origin/main or the local release checkout changed during rollout")
            # autoDeploy stays false by policy. Dokploy temporarily points to the immutable tag
            # only for this manually queued build, then returns to the canonical main branch.
            set_branch(branch)
            latest = [item for item in deployment_list() if str(item.get("deploymentId")) not in previous]
            if len(latest) != 1 or str(latest[0].get("deploymentId")) != deployment_id or str(latest[0].get("status", "")).lower() != "done":
                raise DeploymentError("Dokploy deployment was superseded or changed before acceptance")
            accepted = remote(args.worker_host, "accept-stage", token=token, revision=commit)
            if accepted.get("status") != "accepted":
                raise DeploymentError("respond5 did not safely restore the staged checkout overrides")
            staged = False
            marked_result = remote(args.worker_host, "mark", token=token, receipt=receipt)
            if marked_result.get("status") != "marked":
                raise DeploymentError("respond5 did not write the verified release marker")
            finalized = remote(args.worker_host, "finalize-worker", token=token, revision=commit, receipt=receipt)
            if finalized.get("status") != "finalized":
                raise DeploymentError("respond5 did not finalize the worker transaction")
            marked = True
            print(json.dumps(receipt, sort_keys=True))
            return 0
    except (DeploymentError, OSError, subprocess.TimeoutExpired) as error:
        print(f"deployment not accepted: {error}", file=sys.stderr)
        return 1
    finally:
        if locked and wait_attempted and not marked:
            try:
                remote(args.worker_host, "rollback-worker", token=token)
            except Exception as error:
                print(f"respond5 worker rollback could not be verified; transaction retained: {error}", file=sys.stderr)
        if staged and locked:
            try:
                remote(args.worker_host, "restore", token=token)
            except Exception as error:
                print(f"Dokploy checkout override restore failed; inspect backup: {error}", file=sys.stderr)
        if prepared:
            try:
                if compose_info().get("branch") != branch:
                    set_branch(branch)
                # Deliberately keep autoDeploy disabled and keep the successful revision pin.
                if not marked:
                    restore_build_revision(old_env)
            except Exception as error:
                print(f"Dokploy release configuration restore failed: {error}", file=sys.stderr)
        if locked:
            try:
                remote(args.worker_host, "unlock", token=token)
            except Exception as error:
                print(f"respond5 rollout lock retained: {error}", file=sys.stderr)


if __name__ == "__main__":
    raise SystemExit(main())
