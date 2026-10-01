#!/usr/bin/env python3
"""Exercise service lifecycle with native or isolated Linux runtimes.

Native mode requires source-built binaries, native Pumpkin and a disposable
PostgreSQL database. It covers scheduling, process recovery, snapshots,
cross-worker gameplay and real idle behavior.
"""
import argparse
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import re
import secrets
import signal
import shutil
import shlex
import socket
import ssl
import stat
import subprocess
import tempfile
import time
import threading
import urllib.error
import urllib.parse
import urllib.request
import uuid

from verify_host_control import rpc


LOGIN_LINK = re.compile(r"(https?://[^\s\"<>]+/auth/server#token=)[A-Za-z0-9_-]{43}")
MAX_HTTP_RESPONSE_BYTES = 1 << 20


def redact_login_links(value):
    return LOGIN_LINK.sub(r"\1[redacted]", value)


def run(*args, check=True, env=None):
    return subprocess.run(args, text=True, capture_output=True, check=check, env=env)


def free_port():
    with socket.socket() as connection:
        connection.bind(("127.0.0.1", 0))
        return connection.getsockname()[1]


def create_test_tls_certificate(directory):
    certificate = directory / "bedrock-gateway-test.crt"
    private_key = directory / "bedrock-gateway-test.key"
    run(
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
        "-days", "2", "-sha256", "-keyout", str(private_key),
        "-out", str(certificate), "-subj", "/CN=*.mc.example.test",
        "-addext", "basicConstraints=critical,CA:TRUE",
        "-addext", "keyUsage=critical,digitalSignature,keyCertSign,keyEncipherment",
        "-addext", "extendedKeyUsage=serverAuth",
        "-addext", "subjectAltName=DNS:mc.example.test,DNS:*.mc.example.test,IP:127.0.0.1",
    )
    os.chmod(private_key, 0o600)
    return certificate, private_key


def bedrock_https_request(
    certificate,
    port,
    hostname,
    method,
    path,
    body=b"",
    content_type=None,
    *,
    server_name=None,
    host_header=None,
):
    context = ssl.create_default_context(cafile=str(certificate))
    context.set_alpn_protocols(["h2", "http/1.1"])
    raw = socket.create_connection(("127.0.0.1", port), timeout=5)
    with context.wrap_socket(raw, server_hostname=server_name or hostname) as connection:
        connection.settimeout(10)
        assert connection.selected_alpn_protocol() == "http/1.1"
        headers = [
            f"{method} {path} HTTP/1.1",
            f"Host: {host_header or f'{hostname}:{port}'}",
            "Connection: close",
            "Accept: application/json, application/sdp",
            f"Content-Length: {len(body)}",
        ]
        if content_type:
            headers.append(f"Content-Type: {content_type}")
        connection.sendall(("\r\n".join(headers) + "\r\n\r\n").encode("ascii") + body)
        response = http.client.HTTPResponse(connection)
        response.begin()
        payload = response.read(MAX_HTTP_RESPONSE_BYTES + 1)
        assert len(payload) <= MAX_HTTP_RESPONSE_BYTES, "Bedrock signaling response exceeded its cap"
        return response.status, response.headers, payload


def reject_bad_bedrock_tls_name(certificate, port):
    context = ssl.create_default_context(cafile=str(certificate))
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=5) as raw:
            with context.wrap_socket(raw, server_hostname="attacker.example.test"):
                pass
    except ssl.SSLCertVerificationError:
        return
    raise AssertionError("TLS accepted a hostname outside the Bedrock certificate")


def wait(check, description, timeout=180):
    print(f"Waiting: {description}", flush=True)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.5)
    raise RuntimeError(f"Timed out: {description}")


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
    parser.add_argument(
        "--readiness-only",
        action="store_true",
        help="stop after one worker starts Pumpkin and host-control reports ready",
    )
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
    evidence = {"root": str(root), "server": server, "workers": workers, "checks": []}

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
            "WORKER_PROVISIONING_DATABASE_URL": (
                args.database_url if native else
                f"postgres://postgres:{secret}@127.0.0.1:{db_port}/melon_runtime_test"
            ),
            "WORKER_ID": workers[index],
            "WORKER_NAME": f"runtime-{identifier}-{index}",
            "WORKER_ADDRESS": "127.0.0.1",
            "WORKER_SIGNAL_ADDRESS": "127.0.0.1",
            "WORKER_CONTROL_PLANE_URL": f"http://127.0.0.1:{api_port}",
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
        run(
            str(args.bin_dir / "worker-bundle"), "--worker-id", workers[index],
            "--output", str(bundle_path), env=provisioning_environment,
        )
        assert bundle_path.is_file(), f"worker-bundle did not create worker {index} credentials"
        assert stat.S_IMODE(bundle_path.stat().st_mode) == 0o600
        values = {}
        for line in bundle_path.read_text().splitlines():
            key, separator, raw = line.removeprefix("export ").partition("=")
            allowed_bundle_key = key.startswith("WORKER_") or key in {
                "CONTROL_PLANE_URL", "PUBLIC_BASE_URL", "PUMPKIN_HOST_CONTROL_TOKEN",
                "PUMPKIN_BINARY", "PUMPKIN_IMAGE", "KURBIS_CONTROL_PLUGIN_WASM",
                "SNAPSHOT_INTERVAL_SECONDS", "IDLE_TIMEOUT_SECONDS",
            }
            assert separator and allowed_bundle_key, (
                f"unexpected worker bundle entry: {key}"
            )
            parsed = shlex.split(raw)
            assert len(parsed) == 1
            values[key] = parsed[0]
        required = {
            "WORKER_ID", "WORKER_NAME", "WORKER_ADDRESS", "WORKER_SIGNAL_ADDRESS",
            "WORKER_DATABASE_URL", "WORKER_API_TOKEN", "PUMPKIN_HOST_CONTROL_TOKEN",
            "CONTROL_PLANE_URL", "PUBLIC_BASE_URL", "WORKER_RUNTIME",
            "WORKER_STORAGE_MODE", "WORKER_ROOT",
        }
        assert required <= values.keys(), f"worker {index} bundle misses {required - values.keys()}"
        assert not ({"DATABASE_URL", "INTERNAL_TOKEN", "OBJECT_STORE_TYPE", "OBJECT_STORE_LOCAL_ROOT"} & values.keys())
        role = urllib.parse.urlsplit(values["WORKER_DATABASE_URL"]).username
        assert role and role != "postgres", "worker bundle reused the control-plane database role"
        assert re.fullmatch(r"[A-Za-z0-9_-]{43}", values["WORKER_API_TOKEN"])
        worker_bundles[index] = values
        evidence["checks"].append({
            "worker_bundle": index,
            "database_role_isolated": role,
            "bundle_mode": "0600",
            "api_token_length": len(values["WORKER_API_TOKEN"]),
            "contains_no_control_plane_or_object_store_credentials": True,
        })
        return values

    def runtime_alive(index):
        record_path = root / f"worker-{index}/servers" / server / "run/native-process.json"
        if not record_path.exists():
            return False
        record = json.loads(record_path.read_text())
        command = run("ps", "-p", str(record["pid"]), "-o", "command=", check=False).stdout.split()
        return bool(command and command[0] == record["marker"])

    def player_count():
        placement_worker = sql(f"SELECT worker_id FROM placements WHERE server_id = '{server}'")
        index = workers.index(placement_worker)
        path = (Path(f"/tmp/kurbis-native-{uuid.UUID(placement_worker).hex}") / f"{uuid.UUID(server).hex}.sock"
                if native else root / f"worker-{index}/servers" / server / "run/host-control.sock")
        return rpc(path, worker_bundle(index)["PUMPKIN_HOST_CONTROL_TOKEN"], "status")["players"]

    def state():
        return sql(f"SELECT observed_state FROM servers WHERE id = '{server}'")

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
            assert "DATABASE_URL" not in process_environment
            assert "INTERNAL_TOKEN" not in process_environment
            assert not any(key.startswith(("OBJECT_STORE_", "AWS_")) for key in process_environment)
        process = subprocess.Popen([str(args.bin_dir / binary)], env=process_environment, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        return process

    def worker(index):
        environment = {
            **worker_bundle(index),
        }
        if native:
            environment["NATIVE_FIXTURE_BINARY"] = str(args.native_binary)
        return launch("worker", environment, f"worker-{index}-{len(processes)}")

    def worker_healthy(index):
        role = urllib.parse.urlsplit(worker_bundle(index)["WORKER_DATABASE_URL"]).username
        return sql(
            f"SELECT (w.heartbeat_at > now() - interval '30 seconds')::text || ':' || "
            f"EXISTS(SELECT 1 FROM pg_stat_activity WHERE usename = '{role}')::text "
            f"FROM workers w WHERE w.id = '{workers[index]}'"
        ) == "true:true"

    def worker_role_permissions(index):
        role = urllib.parse.urlsplit(worker_bundle(index)["WORKER_DATABASE_URL"]).username
        return sql(
            f"SELECT concat(rolcanlogin::text,':',rolsuper::text,':',rolcreatedb::text,':',rolcreaterole::text,':',rolbypassrls::text,':',"
            f"pg_has_role(rolname,'worker_runtime','MEMBER')::text) FROM pg_roles WHERE rolname='{role}'"
        )

    def verify_worker_runtime_identity(index):
        wait(lambda: worker_healthy(index), f"worker {index} heartbeat from its isolated database role")
        permissions = worker_role_permissions(index)
        assert permissions == "true:false:false:false:false:true", (
            f"worker {index} database role has unexpected privileges: {permissions}"
        )
        evidence["checks"].append({
            "worker_runtime_identity": index,
            "active_database_session": True,
            "worker_runtime_role_only": True,
            "superuser_createdb_createrole_bypassrls": False,
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
        assert config.get("api_base_url") == plugin_api_base_url
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
        request = urllib.request.Request(f"http://127.0.0.1:{api_port}/internal/wake/live.mc.example.test", method="POST", headers={"authorization": f"Bearer {secret}"})
        with urllib.request.urlopen(request, timeout=15) as response:
            return json.load(response)

    def protocol(port, name, **options):
        edition = "java"
        artifact = args.output / f"protocol-{name}-{edition}.json"
        expect_server_login_link = options.pop("expect_server_login_link", False)
        command = ["node", str(args.protocol_dir / "verify.mjs"), "--edition", edition,
                   "--port", str(port), "--address", options.pop("address", "live.mc.example.test"),
                   "--output", str(artifact)]
        if expect_server_login_link:
            command.append("--capture-login-link")
        for key, value in options.items():
            option = "--" + key.replace("_", "-")
            if isinstance(value, bool):
                if value:
                    command.append(option)
                continue
            for item in value if isinstance(value, list) else [value]:
                command.extend([option, str(item)])
        client_log = args.output / f"protocol-{name}-{edition}.log"
        client = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        waiting_socket = root / "waiting/run/host-control.sock"
        observed_players = []
        route_snapshots = []
        last_route = None
        last_route_check = 0.0
        try:
            deadline = time.monotonic() + 210
            while client.poll() is None and time.monotonic() < deadline:
                try:
                    players = rpc(waiting_socket, secret, "players").get("waiting_players", [])
                    if players:
                        observed_players = [{key: player.get(key) for key in
                            ("name", "edition", "server_address", "verified_identity", "ready_for_transfer")}
                            for player in players]
                        if time.monotonic() - last_route_check >= 1:
                            last_route_check = time.monotonic()
                            request = urllib.request.Request(
                                f"http://127.0.0.1:{api_port}/internal/admission",
                                data=json.dumps({"hostname": players[0]["server_address"],
                                    "identity": None, "minecraft_player_id": None}).encode(),
                                method="POST",
                                headers={"authorization": f"Bearer {secret}", "Content-Type": "application/json"},
                            )
                            try:
                                with urllib.request.urlopen(request, timeout=3) as response:
                                    target = json.load(response)["target"]
                                    route = {key: target.get(key) for key in
                                        ("state", "worker_healthy", "worker_address", "java_port")}
                            except urllib.error.HTTPError as error:
                                route = {"http_status": error.code, "body": error.read().decode()}
                            if route != last_route:
                                route_snapshots.append({"checked_at": time.monotonic(), **route})
                                last_route = route
                except (OSError, ValueError):
                    pass
                time.sleep(0.1)
            try:
                client.wait(timeout=max(1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pass
        finally:
            if client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait()
        stdout, _ = client.communicate()
        client_log.write_text(redact_login_links(stdout))
        assert client.returncode == 0, (
            f"Real {edition} protocol scenario {name} failed: {redact_login_links(stdout)}; "
            f"waiting players observed: {observed_players}; route snapshots: {route_snapshots}"
        )
        proof = json.loads(artifact.read_text())
        assert proof["passed"] is True
        evidence["checks"].append({"protocol": name, "edition": edition, "events": proof["events"]})
        if expect_server_login_link:
            marker = next(
                (line.partition("=")[2] for line in stdout.splitlines()
                 if line.startswith("KURBIS_LOGIN_LINK=")),
                None,
            )
            assert marker is not None, "Kurbis plugin did not return a browser login link"
            parsed = urllib.parse.urlsplit(marker)
            token = urllib.parse.parse_qs(parsed.fragment).get("token", [None])[0]
            assert parsed.path == "/auth/server" and re.fullmatch(r"[A-Za-z0-9_-]{43}", token or ""), (
                "Kurbis login link has an unexpected URL shape"
            )
            return marker

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
        user_action(queued_cookie, "delete", queued_server)
        wait(lambda: sql(f"SELECT count(*) FROM servers WHERE id = '{queued_server}'") == "0", "second hostname deletion and capacity release")
        user_action(cookie, "start")
        wait(lambda: state() == "online", "original hostname resumes after queued capacity test")

    def start_gateway():
        java, bedrock = free_port(), free_port()
        waiting_root = root / "waiting"
        waiting_root.mkdir()
        (waiting_root / "run").mkdir()
        (waiting_root / "pumpkin.toml").write_text(f'''accepts_transfers = true
[networking.query]
enabled = false
[networking.rcon]
enabled = false
[networking.java]
enabled = true
address = "0.0.0.0:{waiting_java_port}"
view_distance = 2
simulation_distance = 2
[networking.bedrock]
enabled = true
motd = "Kurbis waiting fixture"
view_distance = 2
simulation_distance = 2
[networking.bedrock.nethernet]
enabled = true
address = "0.0.0.0:{waiting_bedrock_port}"
signal_address = "0.0.0.0:{waiting_bedrock_port}"
external_ip = "127.0.0.1"
external_port = {bedrock}
identity_key = "nethernet-key.der"
stun_servers = []
[telemetry]
enabled = false
''')
        if native:
            log = (args.output / "waiting-native.log").open("wb")
            logs.append(log)
            process = subprocess.Popen([str(Path(__file__).resolve().with_name("native_pumpkin_fixture.py"))], cwd=waiting_root,
                env={**os.environ, "NATIVE_FIXTURE_BINARY": str(args.native_binary),
                     "PUMPKIN_HOST_CONTROL_SOCKET": str(waiting_root / "run/host-control.sock"),
                     "PUMPKIN_HOST_CONTROL_TOKEN": secret}, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            processes.append(process)
        else:
            run("chown", "-R", "2613:2613", str(waiting_root))
            run("docker", "run", "--detach", "--name", waiting_container, "--network", "host",
                "--memory", "1g", "--memory-swap", "1g", "--cpus", "1", "--volume", f"{waiting_root}:/pumpkin",
                "--env", "PUMPKIN_HOST_CONTROL_SOCKET=/pumpkin/run/host-control.sock",
                "--env", f"PUMPKIN_HOST_CONTROL_TOKEN={secret}", args.pumpkin_image)
        def ready():
            try:
                return rpc(waiting_root / "run/host-control.sock", secret, "status").get("ok")
            except (OSError, ValueError):
                return False
        wait(ready, "actual waiting Pumpkin host-control readiness")
        launch("gateway", {**common, "JAVA_LISTEN": f"127.0.0.1:{java}", "BEDROCK_LISTEN": f"127.0.0.1:{bedrock}",
            "BEDROCK_PUBLIC_PORT": str(bedrock),
            "BEDROCK_TLS_CERT_FILE": str(bedrock_certificate),
            "BEDROCK_TLS_KEY_FILE": str(bedrock_private_key),
            "WAITING_JAVA_ADDRESS": f"127.0.0.1:{waiting_java_port}", "WAITING_BEDROCK_ADDRESS": f"127.0.0.1:{waiting_bedrock_port}",
            "WAITING_CONTROL_SOCKET": str(waiting_root / "run/host-control.sock"), "PUMPKIN_GATEWAY_CONTROL_TOKEN": secret,
            "CONTROL_PLANE_URL": f"http://127.0.0.1:{api_port}", "INTERNAL_TOKEN": secret,
            "RUST_LOG": "info",
            "ROOT_DOMAIN": "mc.example.test", "WAKE_TIMEOUT_SECONDS": "180",
            "ALLOW_OFFLINE_WAITING_FOR_TESTS": "true"}, "gateway")
        def listening():
            try:
                with socket.create_connection(("127.0.0.1", java), timeout=1):
                    return True
            except OSError:
                return False
        wait(listening, "real gateway listener")
        def bedrock_tls_ready():
            try:
                status, _, _ = bedrock_https_request(
                    bedrock_certificate, bedrock, "live.mc.example.test", "GET", "/v1/join"
                )
                return status == 200
            except (OSError, ssl.SSLError, AssertionError):
                return False
        wait(bedrock_tls_ready, "trusted Bedrock TLS signaling listener")
        return java, bedrock, bedrock_certificate

    def verify_bedrock_signal(port, certificate):
        waiting_status, waiting_headers, waiting_body = bedrock_https_request(
            certificate, port, "live.mc.example.test", "GET", "/v1/join"
        )
        waiting = json.loads(waiting_body)
        assert waiting_status == 200 and waiting_headers.get_content_type() == "application/json"
        assert waiting["name"] == "Kurbis waiting fixture"
        assert isinstance(waiting.get("protocol"), int) and waiting["protocol"] > 0

        alias = f"s-{server}.mc.example.test"
        backend_status, backend_headers, backend_body = bedrock_https_request(
            certificate, port, alias, "GET", "/v1/join"
        )
        backend = json.loads(backend_body)
        assert backend_status == 200 and backend_headers.get_content_type() == "application/json"
        assert isinstance(backend.get("name"), str) and backend.get("name")

        bad_sdp, _, _ = bedrock_https_request(
            certificate, port, alias, "POST", "/v1/join/runtime-invalid-sdp",
            b"not an SDP offer", "application/sdp",
        )
        assert bad_sdp == 400, f"malformed SDP was not rejected: HTTP {bad_sdp}"
        no_content_type, _, _ = bedrock_https_request(
            certificate, port, alias, "POST", "/v1/join/runtime-invalid-content-type", b"offer"
        )
        empty_offer, _, _ = bedrock_https_request(
            certificate, port, alias, "POST", "/v1/join/runtime-empty-offer", b"", "application/sdp"
        )
        oversized, _, _ = bedrock_https_request(
            certificate, port, alias, "POST", "/v1/join/runtime-large-offer",
            b"x" * (MAX_HTTP_RESPONSE_BYTES + 1), "application/sdp",
        )
        query, _, _ = bedrock_https_request(
            certificate, port, "live.mc.example.test", "GET", "/v1/join?unexpected=1"
        )
        escaped_id, _, _ = bedrock_https_request(
            certificate, port, alias, "POST", "/v1/join/runtime%2Fescape", b"offer", "application/sdp"
        )
        wrong_port, _, _ = bedrock_https_request(
            certificate, port, "live.mc.example.test", "GET", "/v1/join",
            host_header=f"live.mc.example.test:{port + 1}",
        )
        mismatched_sni, _, _ = bedrock_https_request(
            certificate, port, "live.mc.example.test", "GET", "/v1/join",
            server_name="live.mc.example.test", host_header=f"{alias}:{port}",
        )
        unsupported, _, _ = bedrock_https_request(
            certificate, port, "live.mc.example.test", "DELETE", "/v1/join"
        )
        unknown_alias = f"s-{uuid.uuid4()}.mc.example.test"
        unavailable, _, _ = bedrock_https_request(
            certificate, port, unknown_alias, "GET", "/v1/join"
        )
        assert (no_content_type, empty_offer, oversized, query, escaped_id, wrong_port,
                mismatched_sni, unsupported, unavailable) == (400, 400, 413, 400, 404, 400, 421, 405, 503)
        reject_bad_bedrock_tls_name(certificate, port)
        evidence["checks"].append({
            "bedrock_tls": {
                "valid_waiting_status": waiting_status,
                "valid_assigned_server_status": backend_status,
                "malformed_sdp_status": bad_sdp,
                "invalid_content_type_status": no_content_type,
                "empty_offer_status": empty_offer,
                "oversized_offer_status": oversized,
                "query_status": query,
                "escaped_network_id_status": escaped_id,
                "wrong_port_status": wrong_port,
                "mismatched_sni_status": mismatched_sni,
                "unsupported_method_status": unsupported,
                "unassigned_alias_status": unavailable,
                "untrusted_certificate_name_rejected": True,
            }
        })

    class Provider(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            form = urllib.parse.parse_qs(self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode())
            token = form.get("code", [""])[0]
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"access_token": f"runtime-provider-token-{token}"}).encode())

        def do_GET(self):
            token = self.headers.get("Authorization", "").removeprefix("Bearer runtime-provider-token-")
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"sub": token, "name": "Runtime Test"}).encode())

        def log_message(self, *args):
            pass

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, request, fp, code, msg, headers, newurl):
            return None

    def user_session(subject=owner):
        client = urllib.request.build_opener(NoRedirect)
        try:
            client.open(f"http://127.0.0.1:{api_port}/auth/google/login")
        except urllib.error.HTTPError as redirect:
            assert redirect.code == 307
            state_cookie = redirect.headers["Set-Cookie"].split(";", 1)[0]
            state_value = urllib.parse.parse_qs(urllib.parse.urlsplit(redirect.headers["Location"]).query)["state"][0]
        request = urllib.request.Request(f"http://127.0.0.1:{api_port}/auth/google/callback?code={subject}&state={state_value}", headers={"Cookie": state_cookie})
        try:
            client.open(request)
        except urllib.error.HTTPError as redirect:
            assert redirect.code == 303
            cookie = next(value.split(";", 1)[0] for value in redirect.headers.get_all("Set-Cookie")
                          if value.startswith("kurbis_session="))
            session_request = urllib.request.Request(f"http://127.0.0.1:{api_port}/api/v1/session", headers={"Cookie": cookie})
            with urllib.request.urlopen(session_request, timeout=15) as response:
                actual_user = json.load(response)["user"]["id"]
            assert actual_user == subject, f"OAuth session resolved to {actual_user}, expected {subject}"
            return cookie
        raise RuntimeError("OAuth callback did not return a session")

    def user_action(cookie, action, server_id=server):
        endpoint = f"/api/v1/servers/{server_id}"
        if action != "delete":
            endpoint += "/actions"
        request = urllib.request.Request(f"http://127.0.0.1:{api_port}{endpoint}", data=None if action == "delete" else json.dumps({"action": action}).encode(), method="DELETE" if action == "delete" else "POST", headers={"Cookie": cookie, "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=15) as response:
            body = response.read()
            return json.loads(body) if body else None

    def verify_server_login(link):
        origin = f"http://127.0.0.1:{api_port}"
        parsed = urllib.parse.urlsplit(link)
        assert f"{parsed.scheme}://{parsed.netloc}" == origin
        token = urllib.parse.parse_qs(parsed.fragment).get("token", [None])[0]
        assert re.fullmatch(r"[A-Za-z0-9_-]{43}", token or "")

        def redeem(request_origin):
            request = urllib.request.Request(
                f"{origin}/auth/server/redeem",
                data=json.dumps({"token": token}).encode(),
                method="POST",
                headers={"Origin": request_origin, "Content-Type": "application/json"},
            )
            try:
                with urllib.request.urlopen(request, timeout=15) as response:
                    return response.status, json.load(response), response.headers.get("Set-Cookie", "")
            except urllib.error.HTTPError as error:
                return error.code, None, ""

        cross_origin_status, _, _ = redeem("https://attacker.example")
        assert cross_origin_status == 403
        status, payload, set_cookie = redeem(origin)
        assert status == 200 and payload.get("server_id") == server
        cookie = set_cookie.split(";", 1)[0]
        assert cookie.startswith("kurbis_server_session=")
        assert "HttpOnly" in set_cookie and "Secure" in set_cookie and "SameSite=Lax" in set_cookie

        def scoped_status(path):
            request = urllib.request.Request(f"{origin}{path}", headers={"Cookie": cookie})
            try:
                with urllib.request.urlopen(request, timeout=15) as response:
                    return response.status
            except urllib.error.HTTPError as error:
                return error.code

        assert scoped_status(f"/api/v1/servers/{server}/players") == 200
        assert scoped_status(f"/api/v1/servers/{uuid.uuid4()}/players") == 401
        assert scoped_status("/api/v1/account") == 401
        replay_status, _, _ = redeem(origin)
        assert replay_status == 401
        sql(
            f"UPDATE server_sessions SET expires_at=now()-interval '1 second' "
            f"WHERE server_id='{server}'"
        )
        expired_status = scoped_status(f"/api/v1/servers/{server}/players")
        assert expired_status == 401
        evidence["checks"].append({
            "kurbis_link_redemption": "one-use",
            "cross_origin_status": cross_origin_status,
            "server_api_status": 200,
            "cross_server_status": 401,
            "account_api_status": 401,
            "replay_status": replay_status,
            "expired_session_status": expired_status,
        })

    try:
        if not native:
            run("docker", "run", "--detach", "--name", postgres, "--publish", f"127.0.0.1:{db_port}:5432",
                "--env", f"POSTGRES_PASSWORD={secret}", "--env", "POSTGRES_DB=melon_runtime_test", "postgres:18")
            wait(lambda: run("docker", "exec", postgres, "pg_isready", "-h", "127.0.0.1", "-U", "postgres", check=False).returncode == 0, "Postgres readiness", 60)
        oauth_service = http.server.ThreadingHTTPServer(("127.0.0.1", oauth_port), Provider)
        threading.Thread(target=oauth_service.serve_forever, daemon=True).start()
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
        verify_worker_runtime_identity(0)
        wake()
        wait(lambda: state() == "online", "actual runtime/Pumpkin readiness")
        if args.readiness_only:
            evidence["checks"].append({"real_start": "online", "readiness_only": True})
            evidence["passed"] = True
            (args.output / "result.json").write_text(json.dumps(evidence, indent=2) + "\n")
            print(json.dumps(evidence, indent=2))
            return
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
        verify_server_login(login_link)
        verify_plugin_permissions(gateway_java)
        queued_hostname_clients(gateway_java, cookie)
        directory = root / "worker-0" / "servers" / server
        if not native:
            probe = directory / "run/quota-probe"
            first_worker.send_signal(signal.SIGSTOP)
            filled = run("docker", "exec", backend, "dd", "if=/dev/zero", "of=/pumpkin/run/quota-probe", "bs=1M", "count=5500", check=False)
            assert filled.returncode != 0 and "No space left on device" in filled.stderr, filled.stderr
            assert probe.stat().st_size < 5 * 1024 ** 3
            evidence["checks"].append({"hard_storage_quota": "ENOSPC", "written_bytes": probe.stat().st_size})
            probe.unlink()
            first_worker.send_signal(signal.SIGCONT)
        generation = wait(lambda: sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'"),
                          "completed periodic snapshot", max(60, args.snapshot_interval_seconds + 30))
        manifest_key = sql(f"SELECT manifest_key FROM snapshots WHERE server_id = '{server}' AND generation = {generation} AND complete")
        manifest = json.loads((root / "objects" / manifest_key).read_text())
        stable = next(file for file in manifest["files"] if file["path"].endswith("63afe389-0d98-f833-4096-cc01dd62a9f0.dat"))
        expected = (root / "objects/chunks" / stable["blake3"]).read_bytes()
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
        recovery_generation = sql(f"SELECT current_snapshot_generation FROM servers WHERE id = '{server}'")
        recovery_key = sql(f"SELECT manifest_key FROM snapshots WHERE server_id = '{server}' AND generation = {recovery_generation} AND complete")
        recovery_manifest = json.loads((root / "objects" / recovery_key).read_text())
        stable = next(file for file in recovery_manifest["files"] if file["path"].endswith("63afe389-0d98-f833-4096-cc01dd62a9f0.dat"))
        expected = (root / "objects/chunks" / stable["blake3"]).read_bytes()
        evidence["checks"].append({"latest_completed_recovery_generation": int(recovery_generation), "player_sha256": hashlib.sha256(expected).hexdigest()})
        sql(f"INSERT INTO snapshots(id,server_id,generation,manifest_key,size_bytes,checksum,reason,complete) SELECT '{uuid.uuid4()}','{server}',MAX(generation)+1,'partial',0,'partial','periodic',false FROM snapshots WHERE server_id = '{server}'")
        worker(1)
        verify_worker_runtime_identity(1)
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
        protocol(
            gateway_java,
            "idle-rejoin",
            transfer_port=worker_ports[1],
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
            run("docker", "rm", "--force", waiting_container, backend, queued_backend, check=False)
            run("docker", "rm", "--force", "--volumes", postgres, check=False)
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
                role = urllib.parse.unquote(urllib.parse.urlsplit(bundle["WORKER_DATABASE_URL"]).username or "")
                if not re.fullmatch(r"wkr_[a-f0-9]{32}_[a-f0-9]{8}", role):
                    print(f"Retaining unexpected test worker database role name: {role!r}")
                    continue
                cleanup = run(
                    "psql", args.database_url, "-qAt", "-c",
                    f"SELECT pg_terminate_backend(pid) FROM pg_stat_activity "
                    f"WHERE usename='{role}' AND pid<>pg_backend_pid(); "
                    f"DELETE FROM worker_auth.credentials WHERE worker_id='{workers[index]}'; "
                    f"REVOKE worker_runtime FROM {role}; DROP ROLE IF EXISTS {role}",
                    check=False,
                )
                if cleanup.returncode != 0:
                    print(f"Could not remove temporary database role for worker {index}: {cleanup.stderr.strip()}")
        # Refuse to remove a directory if any quota mount failed to unmount.
        mounts = "" if native else Path("/proc/mounts").read_text()
        if str(root) not in mounts:
            shutil.rmtree(root)
        else:
            print(f"Retaining mounted test storage: {root}")


if __name__ == "__main__":
    main()
