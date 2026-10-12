#!/usr/bin/env python3
"""Release the current origin/main commit to production.

  1. Refuse anything but a clean checkout of origin/main and run the gate.
  2. Compile Pumpkin on the remote CI builder and package it on the host.
  3. Deploy the Compose stack through Dokploy and wait for it to be healthy.
  4. Move the native worker onto the release, restoring it if it fails.

A failed deployment is reported, not rolled back: Compose leaves the previous
containers of any service it could not replace, and redeploying an older
commit is another run of this script from that commit.
"""
from __future__ import annotations

import argparse
import fcntl
import json
import os
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

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pumpkin_artifacts
from migration_manifest import validate_manifest

ROOT = Path(__file__).resolve().parents[1]
HOST_STEPS = ROOT / "scripts/release_host.py"
COMPOSE_ID = "KZyAZl_v1hgoqWi-W77ot"
PROJECT = "kurbis-hoster-fbikvd"
REMOTE_ARTIFACTS = "/srv/kurbis/release-artifacts"
# Settings every release pins in Dokploy next to the revision it deploys.
ENVIRONMENT = {"BEDROCK_TLS_HOST_DIRECTORY": "/srv/kurbis/certs", "PANEL_BIND_ADDRESS": "127.0.0.1"}
FINISHED = {"done", "error", "failed", "cancelled", "canceled"}
SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=30"]


class ReleaseError(RuntimeError):
    pass


def say(message: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)


def run(command: list[str], *, input_text: str | None = None, timeout: int | None = None) -> str:
    result = subprocess.run(command, input=input_text, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=timeout)
    if result.returncode:
        raise ReleaseError(f"{command[0]} failed: {(result.stderr.strip() or result.stdout.strip())[-3000:]}")
    return result.stdout.strip()


def release_revision() -> str:
    if run(["git", "status", "--porcelain", "--untracked-files=all"]):
        raise ReleaseError("the checkout has uncommitted or untracked files")
    commit = run(["git", "rev-parse", "HEAD"])
    remote = run(["git", "ls-remote", "origin", "refs/heads/main"], timeout=30).split()
    if not remote or remote[0] != commit:
        raise ReleaseError("the checkout is not exactly origin/main")
    return commit


def gate() -> None:
    validate_manifest(ROOT / "migrations", ROOT / "migrations/MANIFEST.json")
    if not os.environ.get("TEST_DATABASE_URL"):
        raise ReleaseError("set TEST_DATABASE_URL to a disposable PostgreSQL database for the first-join regression")
    run([sys.executable, "-m", "unittest", "discover", "-s", "tools", "-p", "test_*.py"], timeout=900)
    run(["cargo", "test", "--locked", "--test", "first_join_flow", "--", "--test-threads=1"], timeout=1800)


def dokploy_credentials() -> tuple[str, str]:
    url, token = os.getenv("DOKPLOY_URL"), os.getenv("DOKPLOY_API_KEY")
    if url and token:
        return url.rstrip("/"), token
    executable = shutil.which("dokploy")
    if not executable:
        raise ReleaseError("set DOKPLOY_URL and DOKPLOY_API_KEY, or install and log in the Dokploy CLI")
    config = json.loads((Path(executable).resolve().parent.parent / "config.json").read_text(encoding="utf-8"))
    return config["url"].rstrip("/"), config["token"]


def dokploy(endpoint: str, parameters: dict, *, post: bool = False) -> object:
    base, token = dokploy_credentials()
    body = json.dumps({"json": parameters}, separators=(",", ":"))
    # Dokploy sits behind a proxy that rejects Python's default user agent.
    headers = {"x-api-key": token, "Content-Type": "application/json", "User-Agent": "axios/1.8.4"}
    if post:
        request = urllib.request.Request(f"{base}/api/trpc/{endpoint}", data=body.encode(), headers=headers, method="POST")
    else:
        request = urllib.request.Request(f"{base}/api/trpc/{endpoint}?{urllib.parse.urlencode({'input': body})}", headers=headers)
    try:
        response = json.loads(urllib.request.urlopen(request, timeout=30).read())
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
        raise ReleaseError(f"Dokploy request {endpoint} failed: {error}") from error
    if "error" in response:
        raise ReleaseError(f"Dokploy rejected {endpoint}")
    data = response.get("result", {}).get("data", {})
    return data.get("json", data)


def compose() -> dict:
    info = dokploy("compose.one", {"composeId": COMPOSE_ID})
    if not isinstance(info, dict) or info.get("appName") != PROJECT:
        raise ReleaseError("the Dokploy target is not the Kurbis production stack")
    return info


def parse_environment(text: str) -> dict[str, str]:
    return {key.strip(): value.strip().strip("\"'") for key, _, value in
            (line.partition("=") for line in text.splitlines() if "=" in line and not line.lstrip().startswith("#"))}


def patch_environment(text: str, changes: dict[str, str]) -> str:
    kept = [line for line in text.splitlines() if line.lstrip().startswith("#") or line.partition("=")[0].strip() not in changes]
    return "\n".join(kept + [f"{key}={value}" for key, value in changes.items()]) + "\n"


def host_step(host: str, step: str, arguments: dict, *, timeout: int) -> dict:
    command = shlex.join(["python3", "-", step, json.dumps(arguments, separators=(",", ":"))])
    result = subprocess.run([*SSH, host, command], input=HOST_STEPS.read_text(encoding="utf-8"), text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=timeout)
    try:
        answer = json.loads(result.stdout.strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError) as error:
        raise ReleaseError(f"host step {step} returned no result: {result.stderr.strip()[-2000:]}") from error
    if "error" in answer:
        raise ReleaseError(f"host step {step}: {answer['error']}")
    return answer


def build_pumpkin(host: str, commit: str, settings: dict[str, str]) -> None:
    with tempfile.TemporaryDirectory(prefix="kurbis-pumpkin-artifacts-") as directory:
        local = Path(directory) / "artifacts"
        try:
            digests = pumpkin_artifacts.build(settings, local)
        except pumpkin_artifacts.ArtifactError as error:
            raise ReleaseError(str(error)) from error
        target = f"{REMOTE_ARTIFACTS}/{commit}"
        run([*SSH, host, f"rm -rf -- {shlex.quote(target)} && install -d -m 0700 {shlex.quote(target)}"], timeout=60)
        run(["scp", "-q", "-r", "-o", "BatchMode=yes", str(local), f"{host}:{target}/"], timeout=900)
    host_step(host, "package", {"revision": commit, "artifacts": digests}, timeout=1800)


def deploy(commit: str, wait_seconds: int) -> None:
    info = compose()
    # Releases are the only deployments: a plain Dokploy deploy has no Pumpkin
    # images for its commit and no matching worker.
    if info.get("autoDeploy") is not False:
        dokploy("compose.update", {"composeId": COMPOSE_ID, "autoDeploy": False}, post=True)
    if info.get("branch") != "main":
        run(["dokploy", "compose", "update", "--composeId", COMPOSE_ID, "--branch", "main", "--json"], timeout=60)
    environment = patch_environment(str(info["env"]), {**ENVIRONMENT, "BUILD_REVISION": commit})
    dokploy("compose.saveEnvironment", {"composeId": COMPOSE_ID, "env": environment}, post=True)
    known = {item["deploymentId"] for item in dokploy("deployment.allByCompose", {"composeId": COMPOSE_ID})}
    run(["dokploy", "compose", "deploy", "--composeId", COMPOSE_ID, "--title", f"Release {commit[:12]}",
         "--description", f"Commit: {commit}", "--json"], timeout=60)
    deadline = time.monotonic() + wait_seconds
    while time.monotonic() < deadline:
        time.sleep(10)
        started = [item for item in dokploy("deployment.allByCompose", {"composeId": COMPOSE_ID}) if item["deploymentId"] not in known]
        if started and str(started[0].get("status", "")).lower() in FINISHED:
            if started[0]["status"].lower() != "done":
                raise ReleaseError(f"the Dokploy deployment ended as {started[0]['status']}; see its log in Dokploy")
            return
    raise ReleaseError("the Dokploy deployment did not finish in time; it may still be queued behind other deployments")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--host", default="respond5", help="SSH name of the production host")
    parser.add_argument("--deploy-wait", type=int, default=4 * 60 * 60, help="seconds to wait for Dokploy, including its queue")
    arguments = parser.parse_args()
    lock = (Path.home() / ".cache/kurbis/release.lock")
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open("a+") as handle:
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("another release is running from this machine", file=sys.stderr)
            return 1
        try:
            commit = release_revision()
            say(f"releasing {commit}")
            gate()
            say("gate passed; compiling Pumpkin on the CI builder")
            build_pumpkin(arguments.host, commit, parse_environment(str(compose()["env"])))
            say("Pumpkin images packaged; deploying through Dokploy")
            deploy(commit, arguments.deploy_wait)
            state = host_step(arguments.host, "status", {"revision": commit}, timeout=120)
            if state["problems"]:
                raise ReleaseError("deployed, but: " + "; ".join(state["problems"]))
            say("stack healthy; updating the worker")
            record = host_step(arguments.host, "worker", {"revision": commit}, timeout=600)
        except (ReleaseError, subprocess.TimeoutExpired) as error:
            say(f"release failed: {error}")
            return 1
    say(f"released {record['revision']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
