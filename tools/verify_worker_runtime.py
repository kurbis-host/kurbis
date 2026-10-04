#!/usr/bin/env python3
"""Exercise service lifecycle with native or isolated Linux runtimes.

Native mode requires source-built binaries, native Pumpkin and a disposable
PostgreSQL database. It covers scheduling, process recovery, snapshots,
cross-worker gameplay and real idle behavior.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import signal
import shutil
import stat
import subprocess
import tempfile
import time
import tarfile
import io
import platform
import urllib.error
import urllib.request
import uuid

from runtime_verification_contract import (
    assert_worker_environment,
    command_route,
    command_request,
    read_worker_bundle,
    server_object_path,
)
from runtime_verification_access import AccessVerifier
from runtime_verification_auth import create_user_session, start_provider
from runtime_verification_protocol import (
    create_test_tls_certificate,
    free_port,
    redact_login_links,
    run,
    run_java_protocol,
    start_gateway as start_protocol_gateway,
    verify_bedrock_signal as verify_protocol_bedrock_signal,
    wait,
)
from verify_host_control import rpc


def build_runtime_api_proxy_image(base_image: str, bin_dir: Path, root: Path, identifier: str) -> str:
    image = f"melon-runtime-api-proxy-{identifier}:test"
    context = root / "runtime-api-proxy-image"
    context.mkdir(parents=True, exist_ok=True)
    binary = bin_dir / "runtime-api-proxy"
    if not binary.is_file():
        raise RuntimeError(f"runtime API proxy binary is missing: {binary}")
    shutil.copy2(binary, context / "runtime-api-proxy")
    (context / "Dockerfile").write_text(
        f"FROM {base_image}\n"
        "USER 0:0\n"
        "COPY runtime-api-proxy /usr/local/bin/runtime-api-proxy\n"
        "RUN chmod 0755 /usr/local/bin/runtime-api-proxy\n"
        "USER 2613:2613\n"
        "ENTRYPOINT [\"/usr/local/bin/runtime-api-proxy\"]\n",
        encoding="utf-8",
    )
    run("docker", "build", "--pull=false", "--network=none", "--tag", image, str(context))
    return image


def api_proxy_probe(image: str, network: str, url: str, token: str, method="GET", body=None):
    payload = json.dumps({
        "url": url,
        "method": method,
        "bearer_token": token,
        "body": body,
    })
    result = subprocess.run(
        ["docker", "run", "--rm", "--network", network, "--interactive",
         "--entrypoint", "/usr/local/bin/runtime-api-proxy", image, "--probe-http"],
        input=payload,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"runtime API probe failed: {result.stderr.strip()}")
    return json.loads(result.stdout)


def api_proxy_egress_probe(image: str, network: str, target: str):
    result = subprocess.run(
        ["docker", "run", "--rm", "--network", network, "--interactive",
         "--entrypoint", "/usr/local/bin/runtime-api-proxy", image, "--probe-egress"],
        input=target,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"runtime egress probe failed: {result.stderr.strip()}")
    return json.loads(result.stdout)


def blake3_digest(bin_dir: Path, data: bytes) -> str:
    result = subprocess.run(
        [str(bin_dir / "runtime-api-proxy"), "--hash-stdin"],
        input=data,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError("could not calculate BLAKE3 digest")
    return result.stdout.decode().strip()


def create_ci_fixture_artifact(base_image: str, root: Path, bin_dir: Path, preview_id: str):
    container = f"kurbis-ci-artifact-{uuid.UUID(preview_id).hex[:12]}"
    run("docker", "create", "--name", container, base_image)
    runtime = root / "ci-proxy-fixture"
    runtime.mkdir(parents=True, exist_ok=True)
    try:
        run("docker", "cp", f"{container}:/usr/local/bin/pumpkin", str(runtime / "pumpkin"))
        run("docker", "cp", f"{container}:/usr/local/share/kurbis-control.wasm", str(runtime / "kurbis-control.wasm"))
    finally:
        run("docker", "rm", container, check=False)
    architecture = {"x86_64": "x86_64-linux", "aarch64": "aarch64-linux"}.get(platform.machine())
    if not architecture:
        raise RuntimeError(f"unsupported CI fixture architecture: {platform.machine()}")
    commit = "a" * 40
    metadata = json.dumps({
        "repository": "kurbis-host/Pumpkin",
        "commit_sha": commit,
        "target_architecture": architecture,
        "format_version": 1,
    }).encode()
    artifact = io.BytesIO()
    with tarfile.open(fileobj=artifact, mode="w:gz") as archive:
        for name, data, mode in [
            ("pumpkin", (runtime / "pumpkin").read_bytes(), 0o755),
            ("kurbis-control.wasm", (runtime / "kurbis-control.wasm").read_bytes(), 0o644),
            ("metadata.json", metadata, 0o644),
        ]:
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = mode
            archive.addfile(info, io.BytesIO(data))
    artifact_bytes = artifact.getvalue()
    digest = blake3_digest(bin_dir, artifact_bytes)
    return artifact_bytes, digest, commit, architecture


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--pumpkin-image", default="melon-pumpkin:host-control")
    parser.add_argument("--output", type=Path, default=Path("/opt/melon-hoster-verification/results"))
    parser.add_argument("--native-binary", type=Path)
    parser.add_argument("--plugin-wasm", type=Path)
    parser.add_argument("--database-url")
    parser.add_argument("--protocol-dir", type=Path, default=Path("/verification/protocol"))
    parser.add_argument("--worker-port-base", type=int, default=46000)
    parser.add_argument("--waiting-port-base", type=int, default=45000)
    parser.add_argument("--snapshot-interval-seconds", type=int, default=90)
    args = parser.parse_args()
    native = args.native_binary is not None
    if native:
        args.native_binary = args.native_binary.resolve(strict=True)
        if not args.database_url:
            parser.error("--native-binary requires a disposable --database-url")
        default_plugin = Path(__file__).resolve().parents[1] / "plugins/kurbis-control/target/wasm32-wasip2/debug/kurbis_control.wasm"
        args.plugin_wasm = (args.plugin_wasm or default_plugin).resolve(strict=True)
    if not native and (os.geteuid() != 0 or os.uname().sysname != "Linux"):
        raise RuntimeError("Live quota verification requires a root Linux worker")
    args.bin_dir = args.bin_dir.resolve(strict=True)
    api_contract = json.loads(run(str(args.bin_dir / "api-openapi")).stdout)
    command_path = command_route(api_contract)
    canonical_commands = {
        command: command_request(api_contract, command)
        for command in ("start", "stop", "restart", "delete")
    }
    gateway_start_command = command_request(
        api_contract, "start", {"reason": "gateway_join"}
    )
    canonical_commands["reset_world"] = command_request(
        api_contract, "reset", {"mode": "world"}
    )
    if not 1024 <= args.worker_port_base <= 65335:
        parser.error("--worker-port-base must leave room for two 21-port ranges")
    if not 1024 <= args.waiting_port_base <= 65534:
        parser.error("--waiting-port-base must leave room for the Java and Bedrock ports")
    if args.snapshot_interval_seconds < 10:
        parser.error("--snapshot-interval-seconds must be at least 10")
    worker_ports = (args.worker_port_base, args.worker_port_base + 100)
    waiting_java_port = args.waiting_port_base
    waiting_bedrock_port = args.waiting_port_base + 1
    args.output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="melon-runtime-", dir="/tmp" if native else "/opt/melon-hoster-verification"))
    identifier = uuid.uuid4().hex[:12]
    proxy_image = None if native else build_runtime_api_proxy_image(args.pumpkin_image, args.bin_dir, root, identifier)
    proxy_egress_network = None if native else f"kurbis-runtime-api-egress-{identifier}"
    proxy_upstream = None
    postgres = f"melon-runtime-db-{identifier}"
    server = str(uuid.uuid4())
    owner = str(uuid.uuid4())
    workers = [str(uuid.uuid4()), str(uuid.uuid4())]
    backend = f"kurbis-{server}"
    queued_server = str(uuid.uuid4())
    queued_owner = str(uuid.uuid4())
    queued_backend = f"kurbis-{queued_server}"
    secret = secrets.token_hex(32)
    db_port, api_port, oauth_port = free_port(), free_port(), free_port()
    plugin_api_base_url = f"http://{'127.0.0.1' if native else '172.18.0.1'}:{api_port}"
    bedrock_certificate, bedrock_private_key = create_test_tls_certificate(root)
    waiting_container = f"melon-runtime-waiting-{identifier}"
    oauth_service = None
    processes = []
    logs = []
    worker_bundles = {}
    ci_preview_ids = []
    evidence = {
        "root": str(root),
        "server": server,
        "workers": workers,
        "checks": [{
            "generated_api_contract": {
                "source": "api-openapi",
                "commands_path": command_path,
                "lifecycle_commands": sorted(canonical_commands),
                "legacy_actions_route": False,
            }
        }],
    }

    def sql(query):
        if native:
            return run("psql", args.database_url, "-qAt", "-c", query).stdout.strip()
        return run("docker", "exec", postgres, "psql", "-U", "postgres", "-d", "melon_runtime_test", "-qAt", "-c", query).stdout.strip()

    def seed_account(user_id, subject, display_name, link_runtime_player=False):
        sql(f"INSERT INTO users(id,display_name) VALUES('{user_id}','{display_name}'); "
            f"INSERT INTO identities(id,user_id,provider,subject,display_name) "
            f"VALUES(gen_random_uuid(),'{user_id}','google','{subject}','{display_name}')")
        if link_runtime_player:
            player_id = "63afe389-0d98-f833-4096-cc01dd62a9f0"
            sql(f"INSERT INTO identities(id,user_id,provider,subject,display_name,minecraft_player_id) "
                f"VALUES(gen_random_uuid(),'{user_id}','minecraft_java','{player_id}','RuntimePlayer','{player_id}')")

    def worker_bundle(index):
        if index in worker_bundles:
            return worker_bundles[index]
        bundle_path = root / f"worker-{index}.env"
        provisioning_environment = {
            key: os.environ[key]
            for key in ("PATH", "HOME", "LANG", "LC_ALL", "RUST_LOG", "SSL_CERT_FILE", "SSL_CERT_DIR")
            if key in os.environ
        }
        provisioning_environment.update({
            "WORKER_ID": workers[index],
            "WORKER_NAME": f"runtime-{identifier}-{index}",
            "WORKER_ADDRESS": "127.0.0.1",
            "WORKER_SIGNAL_ADDRESS": "127.0.0.1",
            "WORKER_CONTROL_PLANE_URL": f"http://127.0.0.1:{api_port}",
            "WORKER_RUNTIME_API_PROXY_UPSTREAM": proxy_upstream or plugin_api_base_url,
            "WORKER_RUNTIME_API_EGRESS_NETWORK": proxy_egress_network or "bridge",
            "WORKER_RUNTIME_API_PROXY_IMAGE": proxy_image or "unused-native-proxy-image",
            "PUBLIC_BASE_URL": plugin_api_base_url,
            "WORKER_RUNTIME": "native" if native else "docker",
            "WORKER_STORAGE_MODE": "directory" if native else "loopback_ext4",
            "WORKER_ROOT": str(root / f"worker-{index}"),
            "WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP": "true",
            "SNAPSHOT_INTERVAL_SECONDS": str(args.snapshot_interval_seconds),
            "IDLE_TIMEOUT_SECONDS": "300",
            "WORKER_MEMORY_GIB": "2",
            "WORKER_DISK_GIB": "20",
            "WORKER_CPU_MILLIS": "2000",
            "WORKER_PORT_MIN": str(worker_ports[index]),
            "WORKER_PORT_MAX": str(worker_ports[index] + 20),
        })
        if native:
            provisioning_environment["PUMPKIN_BINARY"] = str(
                Path(__file__).resolve().with_name("native_pumpkin_fixture.py")
            )
            provisioning_environment["KURBIS_CONTROL_PLUGIN_WASM"] = str(args.plugin_wasm)
        else:
            provisioning_environment["PUMPKIN_IMAGE"] = args.pumpkin_image
            provisioning_environment["WORKER_GATEWAY_ADDRESS"] = "127.0.0.1"
        provisioning_database_url = (
            args.database_url if native else
            f"postgres://postgres:{secret}@127.0.0.1:{db_port}/melon_runtime_test"
        )
        run(
            str(args.bin_dir / "worker-bundle"), "--worker-id", workers[index],
            "--database-url", provisioning_database_url,
            "--output", str(bundle_path), env=provisioning_environment,
        )
        assert bundle_path.is_file(), f"worker-bundle did not create worker {index} credentials"
        assert stat.S_IMODE(bundle_path.stat().st_mode) == 0o600
        assert stat.S_IMODE(bundle_path.parent.stat().st_mode) == 0o700
        values = read_worker_bundle(bundle_path)
        assert values["WORKER_ID"] == workers[index]
        assert values["CONTROL_PLANE_URL"] == f"http://127.0.0.1:{api_port}"
        assert values["PUBLIC_BASE_URL"] == plugin_api_base_url
        if not native:
            assert values["WORKER_RUNTIME_API_PROXY_IMAGE"] == proxy_image
            assert values["WORKER_RUNTIME_API_PROXY_UPSTREAM"] == proxy_upstream
            assert values["WORKER_RUNTIME_API_EGRESS_NETWORK"] == proxy_egress_network
        assert all(
            values["WORKER_API_TOKEN"] != previous["WORKER_API_TOKEN"]
            for previous in worker_bundles.values()
        ), "workers received a shared API token"
        assert all(secret not in value for value in values.values()), (
            "operator database/internal secret reached the worker bundle"
        )
        token_hash_length = sql(
            f"SELECT octet_length(api_token_hash) FROM worker_credentials "
            f"WHERE worker_id='{workers[index]}' AND revoked_at IS NULL"
        )
        assert token_hash_length == "32", "worker API token is not stored as a 32-byte hash"
        worker_bundles[index] = values
        evidence["checks"].append({
            "worker_bundle": index,
            "bundle_mode": "0600",
            "contains_api_token_and_no_operator_credentials": True,
            "stored_api_token_hash_bytes": int(token_hash_length),
        })
        return values

    def runtime_alive(index):
        record_path = root / f"worker-{index}/servers" / server / "run/native-process.json"
        if not record_path.exists():
            return False
        record = json.loads(record_path.read_text())
        command = run("ps", "-p", str(record["pid"]), "-o", "command=", check=False).stdout.split()
        return bool(command and command[0] == record["marker"])

    def docker_host_control_socket(server_id):
        hex_id = uuid.UUID(server_id).hex
        name = f"kurbis-docker-{hex_id}.sock"
        system_temp = Path(tempfile.gettempdir()) / name
        return system_temp if len(str(system_temp)) < 100 else Path("/tmp") / name

    def player_count():
        placement_worker = sql(f"SELECT worker_id FROM placements WHERE server_id = '{server}'")
        index = workers.index(placement_worker)
        path = (Path(f"/tmp/kurbis-native-{uuid.UUID(placement_worker).hex}") / f"{uuid.UUID(server).hex}.sock"
                if native else docker_host_control_socket(server))
        return rpc(path, worker_bundle(index)["PUMPKIN_HOST_CONTROL_TOKEN"], "status")["players"]

    def server_state(server_id):
        return sql(f"SELECT observed_state FROM servers WHERE id = '{server_id}'")

    def state():
        return server_state(server)

    access_verifier = AccessVerifier(
        api_origin=f"http://127.0.0.1:{api_port}",
        command_path=command_path,
        canonical_commands=canonical_commands,
        sql=sql,
        checks=evidence["checks"],
        root=root,
        worker_ids=workers,
        server_state=server_state,
        wait_for=wait,
        api_document=api_contract,
    )

    def keep_online_through_snapshot():
        current = state()
        assert current in ("online", "snapshotting"), f"Server left the online lifecycle: {current}"
        if current == "snapshotting":
            wait(lambda: state() == "online", "periodic snapshot completes during idle verification", 30)

    def launch(binary, environment, name):
        log = (args.output / f"{name}.log").open("wb")
        logs.append(log)
        if binary == "worker":
            allow = {
                "PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "RUST_LOG",
                "SSL_CERT_FILE", "SSL_CERT_DIR", "DOCKER_HOST", "DOCKER_CONFIG",
                "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH",
            }
            process_environment = {key: value for key, value in os.environ.items() if key in allow}
        else:
            process_environment = os.environ.copy()
        process_environment.update(environment)
        if binary == "worker":
            assert_worker_environment(process_environment)
            assert all(secret not in value for value in process_environment.values()), (
                "operator secret reached worker process environment"
            )
        process = subprocess.Popen([str(args.bin_dir / binary)], env=process_environment, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        if binary == "worker" and Path("/proc").is_dir() and process.poll() is None:
            raw_environment = Path(f"/proc/{process.pid}/environ").read_bytes()
            actual_environment = {
                key.decode("utf-8", errors="strict"): value.decode("utf-8", errors="replace")
                for entry in raw_environment.split(b"\0") if entry
                for key, separator, value in (entry.partition(b"="),) if separator
            }
            assert_worker_environment(actual_environment)
            assert all(secret not in value for value in actual_environment.values()), (
                "operator secret reached worker process environment"
            )
            evidence["checks"].append({
                "worker_process_environment": name,
                "no_database_or_operator_credentials": True,
            })
        return process

    def worker(index):
        environment = {
            **worker_bundle(index),
        }
        if native:
            environment["NATIVE_FIXTURE_BINARY"] = str(args.native_binary)
        return launch("worker", environment, f"worker-{index}-{len(processes)}")

    def worker_healthy(index):
        return sql(
            f"SELECT (w.heartbeat_at > now() - interval '30 seconds')::text || ':' || "
            f"(c.lease_id IS NOT NULL AND c.lease_expires_at > now())::text "
            f"FROM workers w JOIN worker_credentials c ON c.worker_id = w.id "
            f"WHERE w.id = '{workers[index]}'"
        ) == "true:true"

    def worker_lease_expired(index):
        return sql(
            f"SELECT lease_id IS NULL OR lease_expires_at <= now() "
            f"FROM worker_credentials WHERE worker_id = '{workers[index]}'"
        ) == "t"

    def verify_worker_api_boundary(index):
        wait(lambda: worker_healthy(index), f"worker {index} control-plane heartbeat and active API lease")
        evidence["checks"].append({
            "worker_api_identity": index,
            "control_plane_heartbeat": True,
            "active_fenced_api_lease": True,
            "worker_bundle_has_no_database_or_operator_credentials": True,
        })

    def verify_plugin_staging(index=0):
        server_dir = root / f"worker-{index}" / "servers" / server
        plugin_path = server_dir / "plugins/kurbis-control.wasm"
        config_path = server_dir / "plugins/data/kurbis-control/config.json"
        capability_path = root / f"worker-{index}" / "capabilities" / f"{server}.token"
        if native:
            plugin_bytes = plugin_path.read_bytes()
            plugin_hash = hashlib.sha256(plugin_bytes).hexdigest()
            assert plugin_hash == hashlib.sha256(args.plugin_wasm.read_bytes()).hexdigest()
        else:
            plugin_probe = run(
                "docker", "exec", backend, "sha256sum", "/pumpkin/plugins/kurbis-control.wasm",
                check=False,
            )
            if plugin_probe.returncode != 0:
                listing = run(
                    "docker", "exec", backend, "find", "/pumpkin/plugins", "-maxdepth", "2",
                    "-type", "f", check=False,
                )
                raise RuntimeError(
                    "Docker Pumpkin plugin probe failed: "
                    f"{plugin_probe.stderr.strip()}; files={listing.stdout.strip()}"
                )
            image_hash = plugin_probe.stdout.split()[0]
            plugin_hash = image_hash
            expected_hash = run(
                "docker", "run", "--rm", "--entrypoint", "/usr/bin/sha256sum",
                args.pumpkin_image, "/usr/local/share/kurbis-control.wasm",
            ).stdout.split()[0]
            assert plugin_hash == expected_hash
        config = json.loads(config_path.read_text())
        capability = config.get("capability", "")
        assert config.get("server_id") == server
        expected_plugin_api_url = plugin_api_base_url if native else "http://kurbis-runtime-api-proxy:8080"
        assert config.get("api_base_url") == expected_plugin_api_url
        assert re.fullmatch(r"[A-Za-z0-9_-]{43}", capability)
        assert capability_path.read_text().strip() == capability
        assert stat.S_IMODE(config_path.stat().st_mode) == 0o600
        assert stat.S_IMODE(capability_path.stat().st_mode) == 0o600
        assert stat.S_IMODE(capability_path.parent.stat().st_mode) == 0o700
        assert sql(
            f"SELECT count(*) FROM server_instance_capabilities c JOIN placements p "
            f"ON p.server_id=c.server_id AND p.worker_id=c.worker_id "
            f"AND p.generation=c.placement_generation WHERE c.server_id='{server}' "
            f"AND c.worker_id='{workers[index]}'"
        ) == "1"
        evidence["checks"].append({
            "managed_plugin_staged": True,
            "capability_outside_server_volume": True,
            "capability_file_mode": "0600",
            "plugin_sha256": plugin_hash,
        })
        return capability

    def verify_runtime_api_proxy_server(index, capability):
        if native:
            return
        generation = int(sql(f"SELECT generation FROM placements WHERE server_id='{server}'"))
        network = f"kurbis-{uuid.UUID(server).hex}-g{generation}"
        proxy_url = "http://kurbis-runtime-api-proxy:8080"
        inspect = json.loads(run("docker", "inspect", f"kurbis-api-proxy-{uuid.UUID(server).hex}").stdout)[0]
        proxy_environment = dict(item.split("=", 1) for item in inspect["Config"]["Env"] if "=" in item)
        assert proxy_environment["KURBIS_RUNTIME_API_PROXY_SCOPE"] == "server"
        assert proxy_environment["KURBIS_RUNTIME_API_PROXY_ID"] == server
        assert proxy_environment["KURBIS_RUNTIME_API_PROXY_UPSTREAM"] == proxy_upstream
        assert proxy_environment["KURBIS_RUNTIME_API_PROXY_CAPABILITY_HASH"] == blake3_digest(args.bin_dir, capability.encode())
        assert all(capability not in value for value in proxy_environment.values()), "raw server capability reached API proxy environment"
        assert not inspect["HostConfig"]["PortBindings"], "API proxy must not publish a host port"
        assert inspect["HostConfig"]["ReadonlyRootfs"] and inspect["HostConfig"]["CapDrop"] == ["ALL"]
        assert set(inspect["NetworkSettings"]["Networks"]) == {network, proxy_egress_network}
        runtime_inspect = json.loads(run("docker", "inspect", backend).stdout)[0]
        runtime_environment = dict(item.split("=", 1) for item in runtime_inspect["Config"]["Env"] if "=" in item)
        for forbidden in ("WORKER_API_TOKEN", "INTERNAL_TOKEN", "CONTROL_PLANE_URL", "DATABASE_URL", "S3_ACCESS_KEY", "S3_SECRET_KEY"):
            assert forbidden not in runtime_environment, f"{forbidden} reached Pumpkin container"
        assert set(runtime_inspect["NetworkSettings"]["Networks"]) == {network}, "Pumpkin joined the API proxy egress network"
        own = api_proxy_probe(
            proxy_image, network,
            f"{proxy_url}/api/v1/servers/{server}/players", capability,
        )
        assert own["status"] == 200, own
        account = api_proxy_probe(
            proxy_image, network, f"{proxy_url}/api/v1/account", capability,
        )
        assert account["status"] == 404, account
        sibling_id = str(uuid.uuid4())
        sibling = api_proxy_probe(
            proxy_image, network,
            f"{proxy_url}/api/v1/servers/{sibling_id}/players", capability,
        )
        assert sibling["status"] == 404, sibling
        direct = api_proxy_egress_probe(proxy_image, network, "http://1.1.1.1/")
        assert direct == {"reachable": False}, direct
        proxy_direct = api_proxy_egress_probe(proxy_image, proxy_egress_network, "http://1.1.1.1/")
        assert proxy_direct == {"reachable": False}, proxy_direct
        evidence["checks"].append({
            "managed_plugin_api_proxy": {
                "runtime": "normal",
                "own_server_api_status": own["status"],
                "account_api_denied_status": account["status"],
                "sibling_server_api_denied_status": sibling["status"],
                "direct_external_egress_blocked": True,
                "proxy_external_egress_blocked": True,
            }
        })

    def verify_runtime_api_proxy_ci():
        if native:
            return
        preview_id = str(uuid.uuid4())
        ci_preview_ids.append(preview_id)
        build_id = str(uuid.uuid4())
        lease = str(uuid.uuid4())
        commit = "b" * 40
        artifact, digest, commit, architecture = create_ci_fixture_artifact(
            args.pumpkin_image, root, args.bin_dir, preview_id
        )
        object_key = f"ci-proxy-tests/{preview_id}.tar.gz"
        object_path = root / "objects" / object_key
        object_path.parent.mkdir(parents=True, exist_ok=True)
        object_path.write_bytes(artifact)
        now_expiry = "now() + interval '2 hours'"
        sql(
            f"INSERT INTO ci_artifacts(digest,object_key,size_bytes,expires_at) "
            f"VALUES('{digest}','{object_key}',{len(artifact)},{now_expiry}); "
            f"INSERT INTO ci_builds(id,repository,commit_sha,patch_fingerprint,toolchain_version,"
            f"target_architecture,stage,artifact_digest,expires_at) VALUES('{build_id}',"
            f"'kurbis-host/Pumpkin','{commit}','proxy-test','proxy-test','{architecture}',"
            f"'ready','{digest}',{now_expiry}); "
            f"INSERT INTO ci_preview_aliases(id,repository,commit_sha,kind,hostname,build_id,state,"
            f"runtime_generation,expires_at) VALUES('{preview_id}','kurbis-host/Pumpkin','{commit}',"
            f"'commit','proxy-{uuid.UUID(preview_id).hex[:16]}.mc.example.test','{build_id}',"
            f"'booting',1,{now_expiry}); "
            f"INSERT INTO ci_runtime_placements(preview_id,worker_id,generation,lease_token,java_port,"
            f"bedrock_port,signal_port,state,active,capability_hash) VALUES('{preview_id}',"
            f"'{workers[1]}',1,'{lease}',{worker_ports[1] + 10},{worker_ports[1] + 11},"
            f"{worker_ports[1] + 12},'starting',false,decode(repeat('00',32),'hex')); "
            f"INSERT INTO ci_port_leases(worker_id,port,preview_id,purpose) VALUES "
            f"('{workers[1]}',{worker_ports[1] + 10},'{preview_id}','java'),"
            f"('{workers[1]}',{worker_ports[1] + 11},'{preview_id}','bedrock'),"
            f"('{workers[1]}',{worker_ports[1] + 12},'{preview_id}','signal')"
        )
        wait(
            lambda: sql(
                f"SELECT state='running' AND active FROM ci_runtime_placements "
                f"WHERE preview_id='{preview_id}'"
            ) == "t",
            "CI runtime starts with a current scoped API capability",
            180,
        )
        runtime_world = root / "worker-1" / "ci-preview-storage" / "servers" / preview_id
        config_path = runtime_world / "plugins/data/kurbis-control/config.json"
        config = json.loads(config_path.read_text())
        capability = config["capability"]
        assert config["server_id"] == preview_id and config["ci_preview"] is True
        assert config["api_base_url"] == "http://kurbis-runtime-api-proxy:8080"
        network = f"kurbis-{uuid.UUID(preview_id).hex}-g1"
        inspect = json.loads(run("docker", "inspect", f"kurbis-api-proxy-{uuid.UUID(preview_id).hex}").stdout)[0]
        proxy_environment = dict(item.split("=", 1) for item in inspect["Config"]["Env"] if "=" in item)
        assert proxy_environment["KURBIS_RUNTIME_API_PROXY_SCOPE"] == "ci"
        assert proxy_environment["KURBIS_RUNTIME_API_PROXY_ID"] == preview_id
        assert proxy_environment["KURBIS_RUNTIME_API_PROXY_CAPABILITY_HASH"] == blake3_digest(args.bin_dir, capability.encode())
        assert all(capability not in value for value in proxy_environment.values()), "raw CI capability reached API proxy environment"
        assert not inspect["HostConfig"]["PortBindings"] and inspect["HostConfig"]["ReadonlyRootfs"]
        assert set(inspect["NetworkSettings"]["Networks"]) == {network, proxy_egress_network}
        runtime_inspect = json.loads(run("docker", "inspect", f"kurbis-ci-{uuid.UUID(preview_id).hex}").stdout)[0]
        runtime_environment = dict(item.split("=", 1) for item in runtime_inspect["Config"]["Env"] if "=" in item)
        for forbidden in ("WORKER_API_TOKEN", "INTERNAL_TOKEN", "CONTROL_PLANE_URL", "DATABASE_URL", "S3_ACCESS_KEY", "S3_SECRET_KEY"):
            assert forbidden not in runtime_environment, f"{forbidden} reached CI Pumpkin container"
        assert set(runtime_inspect["NetworkSettings"]["Networks"]) == {network}, "CI Pumpkin joined the API proxy egress network"
        proxy_url = f"http://kurbis-runtime-api-proxy:8080/internal/ci/previews/{preview_id}"
        own = api_proxy_probe(proxy_image, network, proxy_url, capability)
        assert own["status"] == 200, own
        account = api_proxy_probe(
            proxy_image, network, "http://kurbis-runtime-api-proxy:8080/api/v1/account", capability,
        )
        assert account["status"] == 404, account
        sibling_id = str(uuid.uuid4())
        sibling = api_proxy_probe(
            proxy_image, network,
            f"http://kurbis-runtime-api-proxy:8080/internal/ci/previews/{sibling_id}",
            capability,
        )
        assert sibling["status"] == 404, sibling
        direct = api_proxy_egress_probe(proxy_image, network, "http://1.1.1.1/")
        assert direct == {"reachable": False}, direct
        proxy_direct = api_proxy_egress_probe(proxy_image, proxy_egress_network, "http://1.1.1.1/")
        assert proxy_direct == {"reachable": False}, proxy_direct
        evidence["checks"].append({
            "managed_plugin_api_proxy": {
                "runtime": "ci",
                "own_preview_api_status": own["status"],
                "account_api_denied_status": account["status"],
                "sibling_preview_api_denied_status": sibling["status"],
                "direct_external_egress_blocked": True,
                "proxy_external_egress_blocked": True,
            }
        })
        stopping = api_proxy_probe(
            proxy_image, network,
            f"http://kurbis-runtime-api-proxy:8080/internal/ci/previews/{preview_id}/commands",
            capability, method="POST", body={"command": "stop"},
        )
        assert stopping["status"] == 200, stopping
        wait(
            lambda: sql(
                f"SELECT state='stopped' FROM ci_runtime_placements WHERE preview_id='{preview_id}'"
            ) == "t",
            "CI proxy stop command releases its runtime network",
            90,
        )

    def server_management_state():
        return sql(
            f"SELECT json_build_object("
            f"'settings',(SELECT settings FROM servers WHERE id='{server}'),"
            f"'whitelist',COALESCE((SELECT json_agg(jsonb_build_object("
            f"'player_id',player_id,'name',name) ORDER BY player_id) "
            f"FROM whitelist_entries WHERE server_id='{server}'),'[]'::json),"
            f"'whitelist_revision',(SELECT jsonb_build_object("
            f"'revision',revision,'applied_revision',applied_revision) "
            f"FROM server_whitelist_state WHERE server_id='{server}'),"
            f"'operations',COALESCE((SELECT json_agg(jsonb_build_object("
            f"'id',id,'kind',kind,'status',status,'generation',generation,"
            f"'attempt',attempt,'payload',payload,'last_error',last_error) ORDER BY id) "
            f"FROM operations WHERE server_id='{server}'),'[]'::json),"
            f"'start_request',(SELECT jsonb_build_object('reason',reason) "
            f"FROM start_requests WHERE server_id='{server}')"
            f")::text"
        )

    def verify_plugin_permissions(gateway_java):
        operator_before = server_management_state()
        protocol(
            gateway_java,
            "operator-kurbis-menu-start",
            transfer_port=worker_ports[0],
            command=["kurbis"],
            click_slot=[39, 20, 22],
            expect_gui_opens=3,
            expect_message=["start requested; current state: online"],
            forbid_message=["Kurbis: authentication required"],
            hold_seconds=5,
            timeout=60,
        )
        wait(lambda: state() == "online", "operator GUI action leaves the live server online", 10)
        assert server_management_state() == operator_before, (
            "operator's already-online start action changed settings, whitelist, or operation state"
        )

        non_operator_before = server_management_state()
        forbidden_commands = [
            "kurbis",
            "kurbis status",
            "kurbis login",
            "kurbis start confirm",
            "kurbis stop confirm",
            "kurbis restart confirm",
            "kurbis snapshot confirm",
            "kurbis set motd unauthorized",
            f"kurbis whitelist add {uuid.uuid4()} NoOpPlayer",
        ]
        protocol(
            gateway_java,
            "non-operator-kurbis-permissions",
            transfer_port=worker_ports[0],
            username="NoOpPlayer",
            command=forbidden_commands,
            expect_gui_opens=0,
            forbid_message=[
                "One-use server login link",
                "requested; current state:",
                "Updated motd",
                "Added NoOpPlayer to the whitelist",
                "Restoring snapshot",
            ],
            hold_seconds=5,
            timeout=60,
        )
        assert server_management_state() == non_operator_before, (
            "non-operator Kurbis commands changed settings, whitelist, or operation state"
        )
        evidence["checks"].append({
            "kurbis_operator_menu": {"gui_opens": 3, "clicked_slots": [39, 20, 22], "action": "online start acknowledged"},
            "kurbis_non_operator": {"commands_denied": len(forbidden_commands), "gui_opens": 0,
                                    "settings_whitelist_operations_unchanged": True},
        })

    def wake():
        endpoint = command_path.replace("{server_id}", server)
        request = urllib.request.Request(
            f"http://127.0.0.1:{api_port}{endpoint}",
            data=json.dumps(gateway_start_command).encode(),
            method="POST",
            headers={
                "authorization": f"Bearer {secret}",
                "content-type": "application/json",
            },
        )
        with urllib.request.urlopen(request, timeout=15) as response:
            return json.load(response)

    def protocol(port, name, **options):
        return run_java_protocol(
            protocol_dir=args.protocol_dir,
            output_directory=args.output,
            port=port,
            name=name,
            options=options,
            root=root,
            secret=secret,
            api_port=api_port,
            checks=evidence["checks"],
        )

    def queued_hostname_clients(java, cookie):
        seed_account(queued_owner, queued_owner, "Queue Test")
        sql(f"INSERT INTO servers(id,owner_id,subdomain,settings) VALUES('{queued_server}','{queued_owner}','queued','{{\"view_distance\":2,\"simulation_distance\":2}}')")
        pending = []
        edition, backend_port, name = "java", worker_ports[0], "QueueJava"
        artifact = args.output / f"queued-{edition}.json"
        log = (args.output / f"queued-{edition}.log").open("wb")
        logs.append(log)
        client = subprocess.Popen(["node", str(args.protocol_dir / "verify.mjs"), "--edition", edition,
            "--port", str(java), "--address", "queued.mc.example.test", "--username", name,
            "--transfer-port", str(backend_port), "--output", str(artifact)], stdout=log, stderr=subprocess.STDOUT)
        processes.append(client)
        pending.append((edition, client, artifact))
        wait(lambda: sql(f"SELECT observed_state FROM servers WHERE id = '{queued_server}'") == "queued", "capacity waiting on second hostname")
        for edition, client, artifact in pending:
            wait(lambda edition=edition: 'waiting_joined' in (args.output / f"queued-{edition}.log").read_text(), f"actual {edition} queued waiting login")
            assert client.poll() is None
        assert sql(f"SELECT count(*) FROM start_requests WHERE server_id = '{queued_server}'") == "1"
        assert sql(f"SELECT count(*) FROM operations WHERE server_id = '{queued_server}' AND kind = 'start'") == "0"
        # Repeated waiting wakes must retain one durable queue entry.
        time.sleep(12)
        assert all(client.poll() is None for _, client, _ in pending)
        assert sql(f"SELECT count(*) FROM start_requests WHERE server_id = '{queued_server}'") == "1"
        user_action(cookie, "stop")
        wait(lambda: state() == "sleeping", "capacity release after real clean shutdown")
        for edition, client, artifact in pending:
            assert client.wait(timeout=180) == 0, (args.output / f"queued-{edition}.log").read_text()
            proof = json.loads(artifact.read_text())
            assert proof["passed"]
            evidence["checks"].append({"queued_hostname": "queued.mc.example.test", "edition": edition, "events": proof["events"]})
        assert sql(f"SELECT count(*) FROM operations WHERE server_id = '{queued_server}' AND kind = 'start'") == "1"
        queued_cookie = user_session(queued_owner)
        queued_login_link = mint_server_login_link(queued_server)
        return queued_cookie, queued_login_link

    def start_gateway():
        return start_protocol_gateway(
            args=args,
            native=native,
            root=root,
            waiting_java_port=waiting_java_port,
            waiting_bedrock_port=waiting_bedrock_port,
            api_port=api_port,
            secret=secret,
            bedrock_certificate=bedrock_certificate,
            bedrock_private_key=bedrock_private_key,
            common=common,
            launch=launch,
            wait_for=wait,
            processes=processes,
            logs=logs,
            waiting_container=waiting_container,
        )

    def verify_bedrock_signal(port, certificate):
        verify_protocol_bedrock_signal(port, certificate, server, evidence)

    def user_session(subject=owner):
        return create_user_session(f"http://127.0.0.1:{api_port}", subject)

    def user_action(cookie, action, server_id=server):
        return access_verifier.user_action(cookie, action, server_id)

    def scoped_command(cookie, action, server_id):
        return access_verifier.scoped_command(cookie, action, server_id)

    def mint_server_login_link(server_id):
        return access_verifier.mint_server_login_link(server_id)

    def verify_server_login(link, second_link, account_cookie, second_server):
        access_verifier.verify_server_login(
            link, second_link, account_cookie, server, second_server
        )

    try:
        if not native:
            run("docker", "network", "create", "--driver", "bridge", "--internal", proxy_egress_network)
            egress_details = json.loads(run("docker", "network", "inspect", proxy_egress_network).stdout)[0]
            egress_gateway = egress_details["IPAM"]["Config"][0]["Gateway"]
            proxy_upstream = f"http://{egress_gateway}:{api_port}"
            run("docker", "run", "--detach", "--name", postgres, "--publish", f"127.0.0.1:{db_port}:5432",
                "--env", f"POSTGRES_PASSWORD={secret}", "--env", "POSTGRES_DB=melon_runtime_test", "postgres:18")
            wait(lambda: run("docker", "exec", postgres, "pg_isready", "-h", "127.0.0.1", "-U", "postgres", check=False).returncode == 0, "Postgres readiness", 60)
        oauth_service = start_provider("127.0.0.1", oauth_port)
        common = {
            "DATABASE_URL": args.database_url if native else f"postgres://postgres:{secret}@127.0.0.1:{db_port}/melon_runtime_test",
            "OBJECT_STORE_TYPE": "local",
            "OBJECT_STORE_LOCAL_ROOT": str(root / "objects"),
        }
        listen_host = "127.0.0.1" if native else "0.0.0.0"
        launch("control-plane", {**common, "CONTROL_PLANE_LISTEN": f"{listen_host}:{api_port}",
            "PUBLIC_BASE_URL": f"http://127.0.0.1:{api_port}", "PANEL_ORIGIN": "http://127.0.0.1:18082", "ROOT_DOMAIN": "mc.example.test",
            "INTERNAL_TOKEN": secret,
            "GOOGLE_OAUTH_CLIENT_ID": "runtime-google", "GOOGLE_OAUTH_CLIENT_SECRET": secret,
            "GOOGLE_OAUTH_AUTHORIZE_URL": f"http://127.0.0.1:{oauth_port}/authorize", "GOOGLE_OAUTH_TOKEN_URL": f"http://127.0.0.1:{oauth_port}/token", "GOOGLE_OAUTH_USERINFO_URL": f"http://127.0.0.1:{oauth_port}/userinfo",
            "DISCORD_OAUTH_CLIENT_ID": "runtime-discord", "DISCORD_OAUTH_CLIENT_SECRET": secret,
            "DISCORD_OAUTH_AUTHORIZE_URL": f"http://127.0.0.1:{oauth_port}/authorize", "DISCORD_OAUTH_TOKEN_URL": f"http://127.0.0.1:{oauth_port}/token", "DISCORD_OAUTH_USERINFO_URL": f"http://127.0.0.1:{oauth_port}/userinfo",
        }, "control-plane")
        wait(lambda: run("psql", args.database_url, "-qAt", "-c", "SELECT count(*) FROM servers", check=False).returncode == 0 if native else run("docker", "exec", postgres, "psql", "-U", "postgres", "-d", "melon_runtime_test", "-qAt", "-c", "SELECT count(*) FROM servers", check=False).returncode == 0, "control-plane migrations")
        def control_plane_ready():
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{api_port}/healthz", timeout=2) as response:
                    return response.status == 204
            except (TimeoutError, urllib.error.URLError):
                return False
        wait(control_plane_ready, "control-plane HTTP readiness", timeout=90)
        seed_account(owner, owner, "Runtime Test", link_runtime_player=True)
        sql(f"INSERT INTO servers(id,owner_id,subdomain) VALUES('{server}','{owner}','live'); UPDATE servers SET settings = '{{\"view_distance\":2,\"simulation_distance\":2}}' WHERE id = '{server}'")
        cookie = user_session()
        first_worker = worker(0)
        verify_worker_api_boundary(0)
        wake()
        wait(lambda: state() == "online", "actual runtime/Pumpkin readiness")
        if native:
            evidence["checks"].append({"real_start": "online", "runtime": "native"})
        else:
            details = json.loads(run("docker", "inspect", backend).stdout)[0]
            constraints = details["HostConfig"]
            assert constraints["Memory"] == 2 * 1024 ** 3 and constraints["MemorySwap"] == 2 * 1024 ** 3
            assert constraints["ReadonlyRootfs"] and constraints["PidsLimit"] == 512
            evidence["checks"].append({"real_start": "online", "memory_bytes": constraints["Memory"], "memory_swap_bytes": constraints["MemorySwap"], "readonly_root": True, "pids_limit": 512})
        gateway_java, gateway_bedrock, gateway_certificate = start_gateway()
        verify_bedrock_signal(gateway_bedrock, gateway_certificate)
        protocol(gateway_java, "unknown", address="unknown.mc.example.test", expect_close=True)
        online_commands = [
            "give @s minecraft:diamond 7",
            "execute at @s run setblock ~ ~-1 ~ minecraft:gold_block",
            "kurbis login",
        ]
        online_messages = [
            "commands.setblock.success",
            "One-use server login link (valid for 15 minutes):",
        ]
        initial_capability = verify_plugin_staging(0)
        verify_runtime_api_proxy_server(0, initial_capability)
        login_link = protocol(
            gateway_java,
            "online",
            transfer_port=worker_ports[0],
            command=online_commands,
            expect_item=["diamond:7"],
            expect_message=online_messages,
            forbid_message=["Sign in to kurbis:"],
            expect_server_login_link=True,
            hold_seconds=5,
            timeout=60,
        )
        generation_before_restart = int(
            sql(f"SELECT generation FROM servers WHERE id='{server}'")
        )
        user_action(cookie, "restart")
        wait(
            lambda: state() == "online"
            and int(sql(f"SELECT generation FROM servers WHERE id='{server}'")) > generation_before_restart
            and sql(
                f"SELECT count(*) FROM operations WHERE server_id='{server}' "
                "AND status IN ('pending','running')"
            ) == "0",
            "real owner restart completes with a new fenced generation",
            180,
        )
        assert run(
            "docker", "inspect", "--format", "{{.State.Running}}", backend, check=False
        ).stdout.strip() == "true" if not native else runtime_alive(0)
        evidence["checks"].append({
            "owner_restart": {
                "generation_before": generation_before_restart,
                "generation_after": int(sql(f"SELECT generation FROM servers WHERE id='{server}'")),
                "runtime_alive": True,
            }
        })
        verify_plugin_permissions(gateway_java)
        access_verifier.verify_world_export(cookie, server, "63afe389-0d98-f833-4096-cc01dd62a9f0")
        queued_cookie, queued_login_link = queued_hostname_clients(gateway_java, cookie)
        verify_server_login(login_link, queued_login_link, cookie, queued_server)
        if sql(f"SELECT count(*) FROM servers WHERE id = '{queued_server}'") != "0":
            user_action(queued_cookie, "delete", queued_server)
            wait(
                lambda: sql(f"SELECT count(*) FROM servers WHERE id = '{queued_server}'") == "0",
                "second hostname deletion and capacity release",
            )
        user_action(cookie, "start")
        wait(lambda: state() == "online", "original hostname resumes after queued capacity test")
        directory = root / "worker-0" / "servers" / server
        if not native:
            probe = directory / "quota-probe"
            first_worker.send_signal(signal.SIGSTOP)
            filled = run("docker", "exec", backend, "dd", "if=/dev/zero", "of=/pumpkin/quota-probe", "bs=1M", "count=5500", check=False)
            assert filled.returncode != 0 and "No space left on device" in filled.stderr, filled.stderr
            assert probe.stat().st_size < 5 * 1024 ** 3
            evidence["checks"].append({"hard_storage_quota": "ENOSPC", "written_bytes": probe.stat().st_size})
            probe.unlink()
            first_worker.send_signal(signal.SIGCONT)
        generation = wait(lambda: sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'"),
                          "completed periodic snapshot", max(60, args.snapshot_interval_seconds + 30))
        manifest_key = sql(f"SELECT manifest_key FROM snapshots WHERE server_id = '{server}' AND generation = {generation} AND complete")
        manifest = json.loads(server_object_path(root, server, manifest_key).read_text())
        stable = next(file for file in manifest["files"] if file["path"].endswith("63afe389-0d98-f833-4096-cc01dd62a9f0.dat"))
        expected = server_object_path(root, server, f"chunks/{stable['blake3']}").read_bytes()
        evidence["checks"].append({"periodic_generation": int(generation), "saved_player_path": stable["path"], "sha256": hashlib.sha256(expected).hexdigest()})
        # Fail the actual filesystem object-store boundary, then restore it. An
        # upload outage must retain the online runtime, worker lease/heartbeat,
        # reservation and last completed recovery generation until retry succeeds.
        objects = root / "objects"
        unavailable = root / "objects-unavailable"
        objects.rename(unavailable)
        objects.write_text("object-store outage")
        try:
            wait(lambda: sql(f"SELECT count(*) FROM operations WHERE server_id = '{server}' AND kind = 'snapshot' AND payload->>'reason' = 'periodic' AND payload->>'stage' = 'deferred' AND status = 'pending' AND last_error IS NOT NULL") == "1",
                 "periodic upload failure deferred with a persisted retry", max(60, args.snapshot_interval_seconds + 30))
            assert state() == "online"
            assert sql(f"SELECT active FROM placements WHERE server_id = '{server}'") == "t"
            assert runtime_alive(0) if native else run("docker", "inspect", "--format", "{{.State.Running}}", backend).stdout.strip() == "true"
            assert first_worker.poll() is None
            outage_generation = sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'")
            assert sql(f"SELECT complete FROM snapshots WHERE server_id = '{server}' AND generation = {outage_generation}") == "t"
            heartbeat = sql(f"SELECT heartbeat_at FROM workers WHERE id = '{workers[0]}'")
            wait(lambda: sql(f"SELECT heartbeat_at FROM workers WHERE id = '{workers[0]}'") != heartbeat, "worker heartbeat continues during snapshot outage", 20)
            assert sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'") == outage_generation
            assert sql(f"SELECT reserved_memory_bytes FROM workers WHERE id = '{workers[0]}'") == str(2 * 1024 ** 3)
        finally:
            objects.unlink()
            unavailable.rename(objects)
        wait(lambda: int(sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'")) > int(outage_generation) and sql(f"SELECT last_error IS NULL FROM servers WHERE id = '{server}'") == "t", "periodic upload recovery clears its error", 120)
        evidence["checks"].append({"periodic_object_store_outage": "runtime, reservation and heartbeat retained; snapshot retries recover"})
        if native:
            identity = json.loads((directory / "run/native-process.json").read_text())
            before_recovery = int(sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'"))
            heartbeat = sql(f"SELECT heartbeat_at FROM workers WHERE id = '{workers[0]}'")
            first_worker.kill()
            first_worker.wait()
            wait(lambda: worker_lease_expired(0), "expired API lease after hard worker loss", 45)
            first_worker = worker(0)
            wait(lambda: sql(f"SELECT heartbeat_at FROM workers WHERE id = '{workers[0]}'") != heartbeat,
                 "restarted native worker heartbeat", 20)
            snapshot_in_flight = sql(
                f"SELECT EXISTS(SELECT 1 FROM operations WHERE server_id = '{server}' "
                "AND kind = 'snapshot' AND status IN ('pending','running'))"
            ) == "t"
            if not snapshot_in_flight:
                assert json.loads((directory / "run/native-process.json").read_text()) == identity
            wait(lambda: int(sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'")) > before_recovery
                 and state() == "online"
                 and sql(f"SELECT active FROM placements WHERE server_id = '{server}'") == "t"
                 and runtime_alive(0),
                 "restarted native worker resumes actual runtime snapshots",
                 max(60, args.snapshot_interval_seconds + 30))
            resumed_identity = json.loads((directory / "run/native-process.json").read_text())
            evidence["checks"].append({
                "native_worker_restart": "runtime remains available and periodic snapshots resume",
                "pid_before": identity["pid"], "pid_after": resumed_identity["pid"],
                "snapshot_overlapped_restart": snapshot_in_flight,
            })
        # Lose the worker while its Docker runtime is still alive. Its placement
        # must remain fenced even after heartbeat expiry and another healthy worker.
        first_worker.kill()
        first_worker.wait()
        wait(lambda: worker_lease_expired(0), "expired API lease while prior runtime remains fenced", 45)
        recovery_generation = sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'")
        recovery_key = sql(f"SELECT manifest_key FROM snapshots WHERE server_id = '{server}' AND generation = {recovery_generation} AND complete")
        recovery_manifest = json.loads(server_object_path(root, server, recovery_key).read_text())
        stable = next(file for file in recovery_manifest["files"] if file["path"].endswith("63afe389-0d98-f833-4096-cc01dd62a9f0.dat"))
        expected = server_object_path(root, server, f"chunks/{stable['blake3']}").read_bytes()
        evidence["checks"].append({"latest_completed_recovery_generation": int(recovery_generation), "player_sha256": hashlib.sha256(expected).hexdigest()})
        sql(f"INSERT INTO snapshots(id,server_id,generation,manifest_key,size_bytes,checksum,reason,complete) SELECT '{uuid.uuid4()}','{server}',MAX(generation)+1,'partial',0,'partial','periodic',false FROM snapshots WHERE server_id = '{server}'")
        worker(1)
        verify_worker_api_boundary(1)
        sql(f"UPDATE workers SET heartbeat_at = now() - interval '1 minute' WHERE id = '{workers[0]}'")
        assert sql(f"SELECT active FROM placements WHERE server_id = '{server}'") == "t"
        assert runtime_alive(0) if native else run("docker", "inspect", "--format", "{{.State.Running}}", backend).stdout.strip() == "true"
        try:
            wake()
        except urllib.error.HTTPError:
            pass
        assert sql(f"SELECT worker_id FROM placements WHERE server_id = '{server}'") == workers[0]
        evidence["checks"].append({"worker_loss_fence": "preserved while old container lives"})
        # Only explicit runtime inactivity allows resource release and relocation.
        if native:
            record = json.loads((directory / "run/native-process.json").read_text())
            os.kill(record["pid"], signal.SIGKILL)
            wait(lambda: not runtime_alive(0), "host process absence")
        else:
            run("docker", "rm", "--force", backend)
        recovered_worker = worker(0)
        wait(lambda: state() == "failed", "runtime-loss reconciliation")
        recovered_worker.terminate()
        recovered_worker.wait(timeout=20)
        sql(f"UPDATE workers SET draining = true, heartbeat_at = now() - interval '1 minute' WHERE id = '{workers[0]}'")
        wake()
        wait(lambda: state() == "online", "cross-worker snapshot restoration")
        assert sql(f"SELECT worker_id FROM placements WHERE server_id = '{server}'") == workers[1]
        restored = root / "worker-1/servers" / server / stable["path"]
        assert restored.read_bytes() == expected
        relocated_capability = verify_plugin_staging(1)
        assert relocated_capability != initial_capability, (
            "placement reassignment reused a stale server capability"
        )
        evidence["checks"].append({"cross_worker_restore": "identical saved player data before login", "worker": workers[1]})
        protocol(gateway_java, "cross-worker-gameplay", transfer_port=worker_ports[1], command=[
            "execute at @s if block ~ ~-1 ~ minecraft:gold_block run setblock ~ ~-1 ~ minecraft:emerald_block"],
            expect_item=["diamond:7"], expect_message=["commands.setblock.success"], hold_seconds=5)
        user_action(cookie, "stop")
        wait(lambda: state() == "sleeping", "real clean save, shutdown and final snapshot")
        final = json.loads(sql(f"SELECT json_build_object('generation',sn.generation,'reason',sn.reason,'complete',sn.complete,'reserved',w.reserved_memory_bytes) FROM servers s JOIN snapshots sn ON sn.server_id=s.id AND sn.generation=s.current_snapshot_generation JOIN placements p ON p.server_id=s.id JOIN workers w ON w.id=p.worker_id WHERE s.id='{server}'"))
        assert final["reason"] == "shutdown" and final["complete"] is True and final["reserved"] == 0
        evidence["checks"].append({"clean_shutdown_final_snapshot": final})
        protocol(gateway_java, "sleeping-wake",
            transfer_port_range=[
                f"{worker_ports[0]}-{worker_ports[0] + 20}",
                f"{worker_ports[1]}-{worker_ports[1] + 20}",
            ],
            command=["execute at @s if block ~ ~-1 ~ minecraft:emerald_block run setblock ~ ~-1 ~ minecraft:diamond_block"],
            expect_item=["diamond:7"], expect_message=["commands.setblock.success"], hold_seconds=5)
        wait(lambda: state() == "online", "native Java wake and transfer")
        user_action(cookie, "stop")
        wait(lambda: state() == "sleeping", "shutdown before manual idle-test resume")
        user_action(cookie, "start")
        wait(lambda: state() == "online", "manual resume before idle verification")
        # Measure wall time, not an accelerated/configured timeout. Rejoining
        # after 150 seconds must reset the original continuous-zero deadline.
        wait(lambda: player_count() == 0, "zero players before real idle interval", 30)
        initial_empty = time.monotonic()
        while time.monotonic() - initial_empty < 150:
            keep_online_through_snapshot()
            time.sleep(1)
        wait(
            lambda: state() == "snapshotting",
            "periodic snapshot before idle rejoin",
            max(60, args.snapshot_interval_seconds + 30),
        )
        wait(
            lambda: state() == "online",
            "periodic snapshot completes before idle rejoin",
            30,
        )
        idle_worker = sql(f"SELECT worker_id FROM placements WHERE server_id = '{server}'")
        protocol(
            gateway_java,
            "idle-rejoin",
            transfer_port=worker_ports[workers.index(idle_worker)],
            expect_item=["diamond:7"],
            hold_seconds=2,
        )
        wait(lambda: state() == "online" and player_count() == 0, "rejoining player leaves", 30)
        reset_empty = time.monotonic()
        while time.monotonic() - initial_empty < 305:
            keep_online_through_snapshot()
            time.sleep(1)
        wait(lambda: state() == "sleeping", "five actual continuous empty minutes after rejoin", 220)
        elapsed = time.monotonic() - reset_empty
        assert elapsed >= 290, f"Idle sleep was premature after rejoin: {elapsed}"
        evidence["checks"].append({"real_idle_seconds_after_rejoin": elapsed,
            "original_empty_seconds": time.monotonic() - initial_empty,
            "final_snapshot_reason": sql(f"SELECT reason FROM snapshots sn JOIN servers s ON s.id=sn.server_id AND s.current_snapshot_generation=sn.generation WHERE s.id='{server}'")})
        if not native:
            user_action(cookie, "start")
            wait(lambda: state() == "online", "restart before hard memory load")
        if not native:
            # Trigger real allocation pressure inside the same constrained cgroup.
            def memory_events():
                return dict((key, int(value)) for key, value in
                            (line.split() for line in run("docker", "exec", backend, "cat", "/sys/fs/cgroup/memory.events").stdout.splitlines()))
            assert run("docker", "exec", backend, "cat", "/sys/fs/cgroup/memory.max").stdout.strip() == str(2 * 1024 ** 3)
            assert run("docker", "exec", backend, "cat", "/sys/fs/cgroup/memory.swap.max").stdout.strip() == "0"
            before_pressure = memory_events()
            pressure = run("docker", "exec", backend, "dd", "if=/dev/zero", "of=/dev/null", "bs=3G", "count=1", check=False)
            assert pressure.returncode in (137, -9), f"Memory pressure did not hit cgroup enforcement: {pressure.returncode}, {pressure.stderr}"
            after_pressure = memory_events()
            assert after_pressure["oom_kill"] > before_pressure["oom_kill"], f"No kernel cgroup OOM kill: {before_pressure}, {after_pressure}"
            evidence["checks"].append({"memory_pressure_exit": pressure.returncode, "memory_limit_bytes": 2 * 1024 ** 3,
                                       "swap_limit_bytes": 0, "memory_events_before": before_pressure, "memory_events_after": after_pressure})
            user_action(cookie, "stop")
            wait(lambda: state() == "sleeping", "stop normal runtime before independent CI proxy integration")
            verify_runtime_api_proxy_ci()
        evidence["limitations"] = (
            ["Native development processes have no hard memory cgroup or ext4 quota"]
            if native else []
        )
        evidence["not_performed_by_user_instruction"] = [
            "physical Bedrock client login/UI interaction"
        ]
        evidence["passed"] = True
        (args.output / "result.json").write_text(json.dumps(evidence, indent=2) + "\n")
        print(json.dumps(evidence, indent=2))
    except BaseException as error:
        evidence["passed"] = False
        evidence["error"] = repr(error)
        (args.output / "result.json").write_text(json.dumps(evidence, indent=2) + "\n")
        raise
    finally:
        query = "SELECT json_build_object('servers',(SELECT json_agg(s) FROM servers s),'placements',(SELECT json_agg(p) FROM placements p),'operations',(SELECT json_agg(o) FROM operations o),'workers',(SELECT json_agg(w) FROM workers w))"
        try:
            (args.output / "database-at-exit.json").write_text(sql(query))
        except Exception as diagnostic:
            (args.output / "database-at-exit.json").write_text(repr(diagnostic))
        if native:
            for index in range(2):
                for identity in (server, queued_server):
                    record_path = root / f"worker-{index}/servers" / identity / "run/native-process.json"
                    if record_path.exists():
                        record = json.loads(record_path.read_text())
                        try:
                            command = run("ps", "-p", str(record["pid"]), "-o", "command=", check=False).stdout.split()
                            if command and command[0] == record["marker"]:
                                os.kill(record["pid"], signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                    log_path = record_path.with_name("native.log")
                    if log_path.exists():
                        (args.output / f"backend-{index}-{identity}.log").write_text(
                            redact_login_links(log_path.read_text(errors="replace"))
                        )
        else:
            backend_logs = run("docker", "logs", backend, check=False)
            (args.output / "backend-at-exit.log").write_text(
                redact_login_links(backend_logs.stdout + backend_logs.stderr)
            )
        for process in reversed(processes):
            if process.poll() is None:
                process.send_signal(signal.SIGCONT)
                process.terminate()
                try:
                    process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        if not native:
            waiting_logs = run("docker", "logs", waiting_container, check=False)
            (args.output / "waiting.log").write_text(waiting_logs.stdout + waiting_logs.stderr)
            runtime_containers = [
                f"kurbis-ci-{uuid.UUID(preview_id).hex}"
                for preview_id in ci_preview_ids
            ] + [
                f"kurbis-api-proxy-{uuid.UUID(preview_id).hex}"
                for preview_id in ci_preview_ids
            ]
            run("docker", "rm", "--force", waiting_container, backend, queued_backend, *runtime_containers, check=False)
            run("docker", "rm", "--force", "--volumes", postgres, check=False)
            for preview_id in ci_preview_ids:
                mount = root / "worker-1" / "ci-preview-storage" / "servers" / preview_id
                if mount.exists():
                    run("umount", str(mount), check=False)
            for index in range(2):
                mount = root / f"worker-{index}/servers" / server
                if mount.exists():
                    run("umount", str(mount), check=False)
                queued_mount = root / f"worker-{index}/servers" / queued_server
                if queued_mount.exists():
                    run("umount", str(queued_mount), check=False)
        for index in range(2):
            capability_directory = root / f"worker-{index}/capabilities"
            if capability_directory.exists():
                shutil.rmtree(capability_directory)
            for identity in (server, queued_server):
                plugin_config = (
                    root / f"worker-{index}/servers" / identity /
                    "plugins/data/kurbis-control/config.json"
                )
                try:
                    plugin_config.unlink()
                except FileNotFoundError:
                    pass
        for bundle_path in root.glob("worker-*.env"):
            try:
                bundle_path.unlink()
            except FileNotFoundError:
                pass
        if oauth_service:
            oauth_service.shutdown()
            oauth_service.server_close()
        for log in logs:
            log.close()
        for certificate_file in (bedrock_certificate, bedrock_private_key):
            try:
                certificate_file.unlink()
            except FileNotFoundError:
                pass
        if native:
            for index, bundle in worker_bundles.items():
                cleanup = run(
                    "psql", args.database_url, "-qAt", "-c",
                    f"DELETE FROM worker_credentials WHERE worker_id='{workers[index]}'",
                    check=False,
                )
                if cleanup.returncode != 0:
                    print(f"Could not remove temporary worker credentials for worker {index}: {cleanup.stderr.strip()}")
        # Refuse to remove a directory if any quota mount failed to unmount.
        mounts = "" if native else Path("/proc/mounts").read_text()
        if str(root) not in mounts:
            shutil.rmtree(root)
        else:
            print(f"Retaining mounted test storage: {root}")


if __name__ == "__main__":
    main()
