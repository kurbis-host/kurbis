#!/usr/bin/env python3
"""Release steps that run as root on the production host.

`scripts/release.py` pipes this file to `python3 -` over SSH with one step
name and a JSON argument object, and reads one JSON result line back.

  package  build the Pumpkin images from the uploaded binaries
  status   report the running Compose services
  worker   move the native worker onto the deployed release
  canary   start one real server the way a player's join does
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PROJECT = "kurbis-hoster-fbikvd"
SERVICES = ("control-plane", "gateway", "panel", "waiting-pumpkin", "postgres")
# The gateway image defines no health check; running is its healthy state.
UNCHECKED = ("gateway",)
REPOSITORY = "https://github.com/kurbis-host/kurbis.git"
CHECKOUT = Path("/etc/dokploy/compose/kurbis-hoster-fbikvd/code")
ARTIFACTS = Path("/srv/kurbis/release-artifacts")
RECORD = Path("/srv/kurbis/deployed-release.json")
WORKER = Path("/srv/kurbis-hoster/target/release/worker")
WORKER_ENV = Path("/srv/kurbis/worker.env")
WORKER_SERVICE = "kurbis-worker.service"
PUMPKIN_ARTIFACTS = ("pumpkin", "kurbis-control.wasm")
# One worker process must stay up this long, heartbeating, to count as started.
WORKER_SETTLE_SECONDS = 40
WORKER_START_SECONDS = 240
# A start restores the world, boots Pumpkin, and is retried by the worker.
CANARY_SECONDS = 6 * 60
# Longest spacing the control plane puts between join-triggered retries.
CANARY_ADMISSION_SECONDS = 16 * 60


class Failure(RuntimeError):
    pass


def run(command: list[str], *, cwd: Path | None = None, input_text: str | None = None) -> str:
    result = subprocess.run(command, cwd=cwd, input=input_text, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode:
        raise Failure(f"{' '.join(command[:3])} failed: {(result.stderr.strip() or result.stdout.strip())[-2000:]}")
    return result.stdout.strip()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def revision_of(value: object) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}", value):
        raise Failure("release revision must be a full Git SHA")
    return value


def worker_setting(name: str) -> str | None:
    values = [line.split("=", 1)[1].strip().strip("\"'") for line in WORKER_ENV.read_text(encoding="utf-8").splitlines()
              if line.startswith(f"export {name}=")]
    if len(values) > 1:
        raise Failure(f"worker environment repeats {name}")
    return values[0] if values else None


def publish_to_worker(image: str) -> None:
    """Copy an image into the Docker daemon the worker starts runtimes on.

    The worker uses its own rootless daemon with a separate image store, so an
    image built here does not exist there until it is copied.
    """
    host = worker_setting("DOCKER_HOST")
    if not host:
        return
    image_id = run(["docker", "image", "inspect", image, "--format", "{{.Id}}"])
    saved = subprocess.Popen(["docker", "save", image], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    loaded = subprocess.run(["docker", "--host", host, "load"], stdin=saved.stdout, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    saved.stdout.close()
    save_error = saved.stderr.read().decode("utf-8", "replace").strip()
    if saved.wait() or loaded.returncode:
        raise Failure(f"could not copy {image} to the worker Docker daemon: {save_error or loaded.stderr.decode('utf-8', 'replace').strip()}")
    if run(["docker", "--host", host, "image", "inspect", image, "--format", "{{.Id}}"]) != image_id:
        raise Failure(f"worker Docker daemon holds a different image for {image}")


def package(arguments: dict) -> dict:
    """Build the server and waiting images around the binaries compiled on the CI builder."""
    revision = revision_of(arguments.get("revision"))
    context = ARTIFACTS / revision
    for name in PUMPKIN_ARTIFACTS:
        if sha256(context / "artifacts" / name) != arguments["artifacts"][name]:
            raise Failure(f"uploaded {name} differs from the build on the CI builder")
    images = {"host-control": f"melon-pumpkin:host-control-{revision}", "waiting": f"melon-pumpkin:waiting-{revision}"}
    with tempfile.TemporaryDirectory(prefix="kurbis-release-source-") as directory:
        source = Path(directory) / "source"
        run(["git", "init", "--quiet", str(source)])
        run(["git", "-C", str(source), "fetch", "--quiet", "--depth=1", REPOSITORY, revision])
        run(["git", "-C", str(source), "checkout", "--quiet", "--detach", "FETCH_HEAD"])
        for target, image in images.items():
            # The named context replaces the Dockerfile's from-source stage.
            run(["docker", "build", "--target", target, "--build-arg", f"BUILD_REVISION={revision}",
                 "--build-context", f"artifacts={context}", "-f", "runtime/pumpkin/Dockerfile", "-t", image, "."], cwd=source)
    publish_to_worker(images["host-control"])
    return {"images": images}


def services() -> dict[str, dict[str, str]]:
    state = {}
    for service in SERVICES:
        ids = run(["docker", "ps", "-q", "--filter", f"label=com.docker.compose.project={PROJECT}",
                   "--filter", f"label=com.docker.compose.service={service}"]).split()
        if len(ids) != 1:
            state[service] = {"status": "missing" if not ids else "duplicated"}
            continue
        container = json.loads(run(["docker", "inspect", ids[0]]))[0]
        state[service] = {
            "status": container["State"]["Status"],
            "health": container["State"].get("Health", {}).get("Status", "none"),
            "revision": container["Config"].get("Labels", {}).get("org.opencontainers.image.revision", ""),
            "image": container["Image"],
            "name": container["Name"].lstrip("/"),
        }
    return state


def problems(state: dict[str, dict[str, str]], revision: str) -> list[str]:
    found = []
    for service, item in state.items():
        if item["status"] != "running":
            found.append(f"{service} is {item['status']}")
        elif service not in UNCHECKED and item["health"] != "healthy":
            found.append(f"{service} is {item['health']}")
        elif service != "postgres" and item["revision"] != revision:
            found.append(f"{service} runs {item['revision'][:12] or 'an unlabelled image'}")
    return found


def status(arguments: dict) -> dict:
    revision = revision_of(arguments.get("revision"))
    state = services()
    found = problems(state, revision)
    # Dokploy deploys the branch head, which may have moved past this release.
    deployed = run(["git", "-C", str(CHECKOUT), "rev-parse", "HEAD"])
    if deployed != revision:
        found.append(f"Dokploy deployed {deployed[:12]}, not this release")
    return {"services": state, "problems": found}


def query(database: str, sql: str, value: str) -> str:
    """Run one read-only statement in the production database; `:'value'` is its only parameter."""
    return run(["docker", "exec", "-i", "--env", f"KURBIS_QUERY_VALUE={value}", database, "sh", "-c",
                'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -At -v value="$KURBIS_QUERY_VALUE" -f -'], input_text=sql)


def worker_heartbeat_is_fresh(database: str) -> bool:
    count = query(database, "SELECT count(*) FROM workers WHERE id::text = :'value' AND heartbeat_at > now() - interval '30 seconds'",
                  worker_setting("WORKER_ID") or "")
    return count == "1"


def start_worker(binary: bytes, environment: bytes, database: str) -> str | None:
    """Install and start a worker; return why it is unhealthy, or None."""
    for path, content, mode in ((WORKER, binary, 0o755), (WORKER_ENV, environment, 0o600)):
        staged = path.with_name(f".{path.name}.release")
        staged.write_bytes(content)
        staged.chmod(mode)
        staged.replace(path)
    run(["systemctl", "restart", WORKER_SERVICE])
    # The control plane refuses a new worker process until the previous one's
    # lease lapses, so the first starts exit with "worker_already_active" and
    # systemd retries. Healthy means one process that stays up and heartbeats.
    expected, stable_pid, stable_since = hashlib.sha256(binary).hexdigest(), "", 0.0
    deadline = time.monotonic() + WORKER_START_SECONDS
    while time.monotonic() < deadline:
        time.sleep(5)
        pid = run(["systemctl", "show", "--property=MainPID", "--value", WORKER_SERVICE])
        if pid == "0" or subprocess.run(["systemctl", "is-active", "--quiet", WORKER_SERVICE], check=False).returncode:
            stable_pid = ""
            continue
        if pid != stable_pid:
            stable_pid, stable_since = pid, time.monotonic()
        try:
            running = sha256(Path(f"/proc/{pid}/exe"))
        except OSError:
            stable_pid = ""
            continue
        if running != expected:
            return "the running worker is not the installed binary"
        if time.monotonic() - stable_since >= WORKER_SETTLE_SECONDS and worker_heartbeat_is_fresh(database):
            return None
    log = subprocess.run(["journalctl", "-u", WORKER_SERVICE, "--since", "-2min", "--no-pager", "-o", "cat"],
                         text=True, stdout=subprocess.PIPE, check=False).stdout
    errors = [line for line in log.splitlines() if "Error" in line]
    return errors[-1][-500:] if errors else "the worker did not stay up and send heartbeats"


def worker(arguments: dict) -> dict:
    """Run the worker binary and Pumpkin image of the deployed release."""
    revision = revision_of(arguments.get("revision"))
    state = services()
    found = problems(state, revision)
    if found:
        raise Failure("the deployed stack is not healthy at this release: " + "; ".join(found))
    pumpkin_image = f"melon-pumpkin:host-control-{revision}"
    proxy_image = worker_setting("WORKER_RUNTIME_API_PROXY_IMAGE")
    if proxy_image:
        # The runtime API proxy next to each server runs from the app image.
        publish_to_worker(proxy_image)
    container = run(["docker", "create", "--entrypoint", "/bin/true", state["control-plane"]["image"]])
    try:
        with tempfile.TemporaryDirectory(prefix="kurbis-release-worker-") as directory:
            run(["docker", "cp", f"{container}:/usr/local/bin/worker", f"{directory}/worker"])
            new_binary = Path(directory, "worker").read_bytes()
    finally:
        run(["docker", "rm", container])
    old_binary, old_environment = WORKER.read_bytes(), WORKER_ENV.read_bytes()
    lines = old_environment.decode("utf-8").splitlines()
    pinned = [index for index, line in enumerate(lines) if line.startswith("export PUMPKIN_IMAGE=")]
    if len(pinned) != 1:
        raise Failure("worker environment must set PUMPKIN_IMAGE exactly once")
    lines[pinned[0]] = f"export PUMPKIN_IMAGE={pumpkin_image}"
    database = state["postgres"]["name"]
    failure = start_worker(new_binary, ("\n".join(lines) + "\n").encode(), database)
    if failure:
        restored = start_worker(old_binary, old_environment, database)
        raise Failure(f"the new worker did not come up ({failure}); the previous worker was restored"
                      + (f" but is unhealthy too: {restored}" if restored else " and is healthy"))
    record = {"revision": revision, "released_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "pumpkin_image": pumpkin_image, "worker_sha256": hashlib.sha256(new_binary).hexdigest(),
              "services": {name: item["image"] for name, item in state.items()}}
    RECORD.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return record


def canary(arguments: dict) -> dict:
    """Start one real server exactly as a player's join does and wait until it is online.

    Healthy containers and a heartbeating worker do not prove that a server
    can start; only a start does. The server goes back to sleep by itself
    once it has been empty for the idle timeout.
    """
    server = str(arguments.get("server", ""))
    if not re.fullmatch(r"[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}", server):
        raise Failure("canary server must be a server UUID")
    state = services()
    database = state["postgres"]["name"]
    # The gateway's wake request, sent from inside the control plane so its
    # internal token never leaves the container.
    wake = ('curl --silent --show-error --max-time 20 --output /dev/null --write-out "%{http_code}" -X POST '
            '-H "Authorization: Bearer $INTERNAL_TOKEN" -H "Content-Type: application/json" '
            '--data \'{"command":"start","reason":"gateway_join"}\' '
            f"http://127.0.0.1:8080/api/v1/servers/{server}/commands")
    asked = time.monotonic()
    while True:
        code = run(["docker", "exec", state["control-plane"]["name"], "sh", "-c", wake])
        if code.startswith("2"):
            break
        # After failed starts the control plane spaces out join-triggered
        # retries; the canary waits that out like a player would have to.
        if code != "409" or time.monotonic() - asked > CANARY_ADMISSION_SECONDS:
            raise Failure(f"the control plane refused to start the canary server (HTTP {code})")
        time.sleep(20)
    started, observed = time.monotonic(), ""
    while time.monotonic() - started < CANARY_SECONDS:
        time.sleep(5)
        observed = query(database, "SELECT observed_state FROM servers WHERE id::text = :'value'", server)
        if observed == "online":
            return {"server": server, "seconds": round(time.monotonic() - started)}
        if observed == "failed":
            break
    cause = query(database, "SELECT coalesce(last_error, '') FROM operations WHERE server_id::text = :'value' "
                            "AND kind = 'start' ORDER BY updated_at DESC LIMIT 1", server)
    raise Failure(f"the canary server did not come online (state: {observed or 'unknown'}): {cause[-1500:] or 'no error recorded'}")


STEPS = {"package": package, "status": status, "worker": worker, "canary": canary}


def main() -> int:
    if os.geteuid() != 0:
        print(json.dumps({"error": "release steps must run as root"}))
        return 1
    try:
        result = STEPS[sys.argv[1]](json.loads(sys.argv[2]))
    except (Failure, KeyError, OSError, ValueError) as error:
        print(json.dumps({"error": str(error) or type(error).__name__}))
        return 1
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
