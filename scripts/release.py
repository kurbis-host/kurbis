#!/usr/bin/env python3
"""Release the current origin/main commit to production.

  1. Refuse anything but a clean checkout of origin/main and run the gate.
  2. Install this commit's builder files, compile Pumpkin on the remote CI
     builder, and package it on the host.
  3. Deploy the Compose stack through Dokploy and wait for it to be healthy.
  4. Move the native worker onto the release, restoring it if it fails.
  5. Start the canary server like a joining player would and wait for it.

A failed deployment is reported, not rolled back: Compose leaves the previous
containers of any service it could not replace, and redeploying an older
commit is another run of this script from that commit.
"""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
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

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pumpkin_artifacts
from migration_manifest import validate_manifest

ROOT = Path(__file__).resolve().parents[1]
HOST_STEPS = ROOT / "scripts/release_host.py"
COMPOSE_ID = "KZyAZl_v1hgoqWi-W77ot"
PROJECT = "kurbis-hoster-fbikvd"
REMOTE_ARTIFACTS = "/srv/kurbis/release-artifacts"
HOST_RUN_DIRECTORY = "/run/kurbis-release"
# Settings every release pins in Dokploy next to the revision it deploys.
ENVIRONMENT = {"BEDROCK_TLS_HOST_DIRECTORY": "/srv/kurbis/certs", "PANEL_BIND_ADDRESS": "127.0.0.1"}
# Dokploy setting naming the server every release starts to prove starts work.
CANARY_SETTING = "RELEASE_CANARY_SERVER_ID"
FINISHED = {"done", "error", "failed", "cancelled", "canceled"}
HEALTHY_WAIT_SECONDS = 6 * 60
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
    """Run one step of `release_host.py` on the host and return its result.

    The step runs detached from the SSH session and leaves its result in a
    file that is polled over fresh connections. Connections to the host have
    been reset in the middle of image builds; a step must neither die with
    the connection nor leave the release without its result.
    """
    base = f"{HOST_RUN_DIRECTORY}/{secrets.token_hex(8)}"
    command = shlex.join(["python3", f"{base}.py", step, json.dumps(arguments, separators=(",", ":"))])
    launch = (f"install -d -m 0700 {HOST_RUN_DIRECTORY} && cat > {base}.py && "
              f"(setsid sh -c {shlex.quote(f'{command} > {base}.out 2> {base}.err; touch {base}.done')} "
              "< /dev/null > /dev/null 2>&1 &)")
    run([*SSH, host, launch], input_text=HOST_STEPS.read_text(encoding="utf-8"), timeout=60)
    deadline = time.monotonic() + timeout
    try:
        while time.monotonic() < deadline:
            time.sleep(5)
            poll = subprocess.run([*SSH, host, f"test -f {base}.done && cat {base}.out {base}.err"],
                                  text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=60)
            if poll.returncode:
                continue  # still running, or this connection was lost
            try:
                answer = json.loads(poll.stdout.strip().splitlines()[0])
            except (IndexError, json.JSONDecodeError) as error:
                raise ReleaseError(f"host step {step} returned no result: {poll.stdout.strip()[-2000:]}") from error
            if "error" in answer:
                raise ReleaseError(f"host step {step}: {answer['error']}")
            return answer
        raise ReleaseError(f"host step {step} did not finish within {timeout}s")
    finally:
        subprocess.run([*SSH, host, f"rm -f {base}.py {base}.out {base}.err {base}.done"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False, timeout=60)


def builder_fixtures(settings: dict[str, str]) -> dict[str, str]:
    """Install this commit's runner and patch-check binary on the CI builder.

    The control plane refuses to start unless the builder's
    `ci-source-preparer` reports the patch fingerprint compiled into the
    control plane, and the runner has to speak its job protocol. Both used to
    be copied to the builder by hand, so a release that changed the patches or
    the control plugin without that step took the control plane down. Files
    are installed under content-derived names next to the configured ones and
    never replaced, so the running control plane keeps its own. Returns the
    Dokploy settings that name the installed files.
    """
    target = settings["CI_BUILDER_SSH_TARGET"]
    root = str(Path(settings["CI_BUBBLEWRAP_RUNNER"]).parent.parent)
    runner_source = ROOT / "tools/ci_bubblewrap_runner.py"
    runner_directory = f"{root}/runner-{hashlib.sha256(runner_source.read_bytes()).hexdigest()[:12]}"
    runner = f"{runner_directory}/ci_bubblewrap_runner.py"
    fingerprint = run(["cargo", "run", "--locked", "--quiet", "--bin", "ci-source-preparer", "--", "--print-fingerprint"],
                      timeout=1800).splitlines()[-1]
    preparer_directory = f"{root}/preparer-{fingerprint[:12]}"
    preparer = f"{preparer_directory}/ci-source-preparer"

    def builder(command: str, *, input_bytes: bytes | None = None, timeout: int = 120) -> str:
        result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=30",
                                 target, command], input=input_bytes, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                check=False, timeout=timeout)
        if result.returncode:
            raise ReleaseError(f"CI builder command failed: {result.stderr.decode('utf-8', 'replace').strip()[-2000:]}")
        return result.stdout.decode("utf-8", "replace").strip()

    if builder(f"test -x {shlex.quote(runner)} && echo present || true") != "present":
        builder(f"install -d -m 0700 {shlex.quote(runner_directory)} && cat > {shlex.quote(runner)} "
                f"&& chmod 0700 {shlex.quote(runner)}", input_bytes=runner_source.read_bytes())
    if builder(f"test -x {shlex.quote(preparer)} && echo present || true") != "present":
        say("building the CI builder's patch-check binary for this patch set")
        build = f"{root}/preparer-build"
        source = subprocess.run(["git", "archive", "--format=tar", "HEAD", "--", ".", ":(exclude)web"], cwd=ROOT,
                                stdout=subprocess.PIPE, check=True).stdout
        builder(f"rm -rf {shlex.quote(build)}/source && install -d -m 0700 {shlex.quote(build)}/source "
                f"&& tar -xf - -C {shlex.quote(build)}/source", input_bytes=source, timeout=600)
        toolchain = shlex.quote(settings["CI_BUBBLEWRAP_TOOLCHAIN"] + "/bin")
        builder(f"cd {shlex.quote(build)}/source && PATH={toolchain}:$PATH CARGO_TARGET_DIR={shlex.quote(build)}/target "
                "nice cargo build --release --locked --jobs 8 --bin ci-source-preparer "
                f"&& install -d -m 0700 {shlex.quote(preparer_directory)} "
                f"&& install -m 0700 {shlex.quote(build)}/target/release/ci-source-preparer {shlex.quote(preparer)}",
                timeout=3600)
    if builder(f"{shlex.quote(preparer)} --print-fingerprint") != fingerprint:
        raise ReleaseError("the CI builder's patch-check binary reports a different patch fingerprint than this commit")
    return {"CI_BUBBLEWRAP_RUNNER": runner, "CI_BUBBLEWRAP_SOURCE_PREPARER": preparer}


# Everything the Pumpkin binaries are compiled from. A release that changes
# none of it reuses the previous build instead of compiling for ten minutes.
PUMPKIN_INPUTS = ("runtime/pumpkin", "plugins/kurbis-control", "tools/ci_bubblewrap_runner.py",
                  "tools/pumpkin_source.sh", "scripts/pumpkin_artifacts.py")
KEPT_PUMPKIN_BUILDS = 3


def pumpkin_inputs_key(settings: dict[str, str]) -> str:
    trees = run(["git", "-C", str(ROOT), "rev-parse", *(f"HEAD:{path}" for path in PUMPKIN_INPUTS)], timeout=30)
    builder = [settings.get(key, "") for key in ("CI_TOOLCHAIN_VERSION", "CI_BUBBLEWRAP_TOOLCHAIN", "CI_BUBBLEWRAP_ROOTFS")]
    return hashlib.sha256("\n".join([trees, *builder]).encode()).hexdigest()[:24]


# The gateway binary is compiled from the whole crate, but only some of its
# modules can change what the gateway does. These are found from the source,
# not listed, so a new dependency of the gateway is picked up by itself.
GATEWAY_ROOTS = ("src/bin/gateway.rs", "src/gateway")
GATEWAY_ALWAYS = ("Cargo.toml", "Cargo.lock", "runtime/Dockerfile", "migrations", "src/lib.rs")
WAITING_INPUTS = ("deployment/waiting.toml",)


def gateway_modules(source: Path) -> list[str]:
    """Top-level modules of the crate the gateway can reach, itself included."""
    modules = {path.stem if path.is_file() else path.name for path in (source / "src").iterdir()
               if path.name not in ("bin", "lib.rs", "main.rs")}

    def files(module: str) -> list[Path]:
        single = source / "src" / f"{module}.rs"
        return ([single] if single.is_file() else []) + sorted((source / "src" / module).rglob("*.rs"))

    reached: set[str] = set()
    pending = ["gateway"] + re.findall(r"\bkurbis_hoster::(\w+)", (source / "src/bin/gateway.rs").read_text(encoding="utf-8"))
    while pending:
        module = pending.pop()
        if module in reached or module not in modules:
            continue
        reached.add(module)
        for path in files(module):
            # `super::` from a top-level module also names a sibling module.
            pending.extend(re.findall(r"\b(?:crate|super)::(\w+)", path.read_text(encoding="utf-8")))
    return sorted(reached)


def tree_key(paths: list[str], *extra: str) -> str:
    trees = run(["git", "-C", str(ROOT), "rev-parse", *(f"HEAD:{path}" for path in paths)], timeout=30)
    return hashlib.sha256("\n".join([*paths, trees, *extra]).encode()).hexdigest()[:24]


def gateway_image() -> str:
    paths = [*GATEWAY_ALWAYS, "src/bin/gateway.rs"]
    for module in gateway_modules(ROOT):
        paths.append(f"src/{module}" if (ROOT / "src" / module).is_dir() else f"src/{module}.rs")
        if (ROOT / "src" / module).is_dir() and (ROOT / "src" / f"{module}.rs").is_file():
            paths.append(f"src/{module}.rs")
    return f"melon-hoster:gateway-{tree_key(sorted(set(paths)))}"


def waiting_image(settings: dict[str, str]) -> str:
    return f"melon-pumpkin:waiting-{tree_key(list(WAITING_INPUTS), pumpkin_inputs_key(settings))}"


def build_pumpkin(host: str, commit: str, settings: dict[str, str]) -> None:
    target = shlex.quote(f"{REMOTE_ARTIFACTS}/{commit}")
    kept = shlex.quote(f"{REMOTE_ARTIFACTS}/inputs-{pumpkin_inputs_key(settings)}")
    run([*SSH, host, f"rm -rf -- {target} && install -d -m 0700 {target}"], timeout=60)
    reused = run([*SSH, host, f"if test -f {kept}/digests.json; then cp -a {kept}/artifacts {target}/ && touch {kept} "
                              f"&& cat {kept}/digests.json; fi"], timeout=120).strip()
    if reused:
        say("Pumpkin inputs are unchanged; reusing the previous build")
        digests = json.loads(reused)
    else:
        say("compiling Pumpkin on the CI builder")
        with tempfile.TemporaryDirectory(prefix="kurbis-pumpkin-artifacts-") as directory:
            local = Path(directory) / "artifacts"
            try:
                digests = pumpkin_artifacts.build(settings, local)
            except pumpkin_artifacts.ArtifactError as error:
                raise ReleaseError(str(error)) from error
            run(["scp", "-q", "-r", "-o", "BatchMode=yes", str(local), f"{host}:{target}/"], timeout=900)
        # Written last, so an interrupted copy is never taken for a build.
        run([*SSH, host, f"rm -rf -- {kept} && install -d -m 0700 {kept} && cp -a {target}/artifacts {kept}/ "
                         f"&& cat > {kept}/digests.json && ls -dt {shlex.quote(REMOTE_ARTIFACTS)}/inputs-* "
                         f"| tail -n +{KEPT_PUMPKIN_BUILDS + 1} | xargs -r rm -rf --"],
            input_text=json.dumps(digests), timeout=120)
    host_step(host, "package", {"revision": commit, "artifacts": digests, "waiting_image": waiting_image(settings),
                                "gateway_image": gateway_image()}, timeout=3600)


def deploy(commit: str, wait_seconds: int, settings: dict[str, str]) -> None:
    info = compose()
    # Releases are the only deployments: a plain Dokploy deploy has no Pumpkin
    # images for its commit and no matching worker.
    if info.get("autoDeploy") is not False:
        dokploy("compose.update", {"composeId": COMPOSE_ID, "autoDeploy": False}, post=True)
    if info.get("branch") != "main":
        run(["dokploy", "compose", "update", "--composeId", COMPOSE_ID, "--branch", "main", "--json"], timeout=60)
    environment = patch_environment(str(info["env"]), {**ENVIRONMENT, **settings, "BUILD_REVISION": commit})
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


def wait_until_healthy(host: str, commit: str, images: dict[str, str]) -> None:
    # Compose returns once containers are started; the panel and control plane
    # need up to a few minutes more before their health checks pass.
    deadline = time.monotonic() + HEALTHY_WAIT_SECONDS
    while True:
        found = host_step(host, "status", {"revision": commit, "gateway_image": images["GATEWAY_IMAGE"],
                                           "waiting_image": images["WAITING_IMAGE"]}, timeout=120)["problems"]
        if not found:
            return
        if time.monotonic() >= deadline:
            raise ReleaseError("deployed, but: " + "; ".join(found))
        time.sleep(10)


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
            say("gate passed")
            settings = parse_environment(str(compose()["env"]))
            fixtures = builder_fixtures(settings)
            settings.update(fixtures)
            build_pumpkin(arguments.host, commit, settings)
            say("Pumpkin images packaged; deploying through Dokploy")
            images = {"GATEWAY_IMAGE": gateway_image(), "WAITING_IMAGE": waiting_image(settings)}
            deploy(commit, arguments.deploy_wait, {**fixtures, **images})
            wait_until_healthy(arguments.host, commit, images)
            say("stack healthy; updating the worker")
            record = host_step(arguments.host, "worker", {"revision": commit, "gateway_image": images["GATEWAY_IMAGE"],
                                                         "waiting_image": images["WAITING_IMAGE"]}, timeout=600)
            canary = settings.get(CANARY_SETTING)
            if canary:
                say("worker updated; starting the canary server")
                started = host_step(arguments.host, "canary", {"server": canary}, timeout=25 * 60)
                say(f"canary server online after {started['seconds']}s")
            else:
                say(f"WARNING: {CANARY_SETTING} is not set in Dokploy, so no server start was tested")
        except (ReleaseError, subprocess.TimeoutExpired) as error:
            say(f"release failed: {error}")
            return 1
    say(f"released {record['revision']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
