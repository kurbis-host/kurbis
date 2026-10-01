#!/usr/bin/env python3
"""Exercise service lifecycle with native or isolated Linux runtimes.

Native mode requires source-built binaries, native Pumpkin and a disposable
PostgreSQL database. It covers scheduling, process recovery, snapshots,
cross-worker gameplay and real idle behavior.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import secrets
import signal
import shutil
import socket
import subprocess
import tempfile
import time
import threading
import urllib.error
import urllib.parse
import urllib.request
import uuid

from verify_host_control import rpc


def run(*args, check=True):
    return subprocess.run(args, text=True, capture_output=True, check=check)


def free_port():
    with socket.socket() as connection:
        connection.bind(("127.0.0.1", 0))
        return connection.getsockname()[1]


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
    waiting_container = f"melon-runtime-waiting-{identifier}"
    oauth_service = None
    processes = []
    logs = []
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
        return rpc(path, secret, "status")["players"]

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
        process = subprocess.Popen([str(args.bin_dir / binary)], env={**os.environ, **environment}, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        return process

    def worker(index):
        environment = {**common,
            "WORKER_ID": workers[index], "WORKER_NAME": f"runtime-{identifier}-{index}", "WORKER_ADDRESS": "127.0.0.1",
            "WORKER_ROOT": str(root / f"worker-{index}"), "WORKER_STORAGE_MODE": "directory" if native else "loopback_ext4",
            "WORKER_RUNTIME": "native" if native else "docker",
            "PUMPKIN_BINARY": str(Path(__file__).resolve().with_name("native_pumpkin_fixture.py")),
            "NATIVE_FIXTURE_BINARY": str(args.native_binary or ""),
            "WORKER_MEMORY_GIB": "2", "WORKER_DISK_GIB": "20", "WORKER_CPU_MILLIS": "2000",
            "WORKER_PORT_MIN": str(worker_ports[index]), "WORKER_PORT_MAX": str(worker_ports[index] + 20),
            "PUMPKIN_IMAGE": args.pumpkin_image, "PUMPKIN_HOST_CONTROL_TOKEN": secret,
            "SNAPSHOT_INTERVAL_SECONDS": str(args.snapshot_interval_seconds), "IDLE_TIMEOUT_SECONDS": "300",
            "OBJECT_STORE_TYPE": "local", "OBJECT_STORE_LOCAL_ROOT": str(root / "objects"),
        }
        return launch("worker", environment, f"worker-{index}-{len(processes)}")

    def wake():
        request = urllib.request.Request(f"http://127.0.0.1:{api_port}/internal/wake/live.mc.example.test", method="POST", headers={"authorization": f"Bearer {secret}"})
        with urllib.request.urlopen(request, timeout=15) as response:
            return json.load(response)

    def protocol(port, name, **options):
        edition = "java"
        artifact = args.output / f"protocol-{name}-{edition}.json"
        command = ["node", str(args.protocol_dir / "verify.mjs"), "--edition", edition,
                   "--port", str(port), "--address", options.pop("address", "live.mc.example.test"),
                   "--output", str(artifact)]
        for key, value in options.items():
            option = "--" + key.replace("_", "-")
            if isinstance(value, bool):
                if value:
                    command.append(option)
                continue
            for item in value if isinstance(value, list) else [value]:
                command.extend([option, str(item)])
        client_log = args.output / f"protocol-{name}-{edition}.log"
        output = client_log.open("w")
        client = subprocess.Popen(command, stdout=output, stderr=subprocess.STDOUT, text=True)
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
            output.close()
        stdout = client_log.read_text()
        assert client.returncode == 0, (
            f"Real {edition} protocol scenario {name} failed: {stdout}; "
            f"waiting players observed: {observed_players}; route snapshots: {route_snapshots}"
        )
        proof = json.loads(artifact.read_text())
        assert proof["passed"] is True
        evidence["checks"].append({"protocol": name, "edition": edition, "events": proof["events"]})

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
view_distance = 2
simulation_distance = 2
[networking.bedrock.nethernet]
enabled = true
address = "0.0.0.0:{waiting_bedrock_port}"
external_ip = ""
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
        java, bedrock = free_port(), free_port()
        launch("gateway", {**common, "JAVA_LISTEN": f"127.0.0.1:{java}", "BEDROCK_LISTEN": f"127.0.0.1:{bedrock}",
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
        return java, bedrock

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
        launch("control-plane", {**common, "CONTROL_PLANE_LISTEN": f"127.0.0.1:{api_port}",
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
        wait(lambda: sql(f"SELECT count(*) FROM workers WHERE id = '{workers[0]}'") == "1", "worker heartbeat")
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
        gateway_java, gateway_bedrock = start_gateway()
        protocol(gateway_java, "unknown", address="unknown.mc.example.test", expect_close=True)
        online_commands = [
            "give @s minecraft:diamond 7",
            "execute at @s run setblock ~ ~-1 ~ minecraft:gold_block",
        ]
        online_messages = ["commands.setblock.success"]
        protocol(
            gateway_java,
            "online",
            transfer_port=worker_ports[0],
            command=online_commands,
            expect_item=["diamond:7"],
            expect_message=online_messages,
            forbid_message=["Sign in to kurbis:", "One-use server login link"],
            hold_seconds=5,
            timeout=60,
        )
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
        wait(lambda: sql(f"SELECT count(*) FROM workers WHERE id = '{workers[1]}'") == "1", "second worker heartbeat")
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
        evidence["checks"].append({"cross_worker_restore": "identical saved player data before login", "worker": workers[1]})
        protocol(gateway_java, "cross-worker-gameplay", transfer_port=worker_ports[1], command=[
            "execute at @s if block ~ ~-1 ~ minecraft:gold_block run setblock ~ ~-1 ~ minecraft:emerald_block"],
            expect_item=["diamond:7"], expect_message=["commands.setblock.success"], hold_seconds=5)
        user_action(cookie, "stop")
        wait(lambda: state() == "sleeping", "real clean save, shutdown and final snapshot")
        final = json.loads(sql(f"SELECT json_build_object('generation',sn.generation,'reason',sn.reason,'complete',sn.complete,'reserved',w.reserved_memory_bytes) FROM servers s JOIN snapshots sn ON sn.server_id=s.id AND sn.generation=s.current_snapshot_generation JOIN placements p ON p.server_id=s.id JOIN workers w ON w.id=p.worker_id WHERE s.id='{server}'"))
        assert final["reason"] == "shutdown" and final["complete"] is True and final["reserved"] == 0
        evidence["checks"].append({"clean_shutdown_final_snapshot": final})
        protocol(gateway_java, "sleeping-wake", transfer_port=worker_ports[1], command=["execute at @s if block ~ ~-1 ~ minecraft:emerald_block run setblock ~ ~-1 ~ minecraft:diamond_block"], expect_item=["diamond:7"], expect_message=["commands.setblock.success"], hold_seconds=5)
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
        protocol(gateway_java, "idle-rejoin", transfer_port=worker_ports[1], expect_item=["diamond:7"], hold_seconds=10)
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
        evidence["limitations"] = ["Native development processes have no hard memory cgroup or ext4 quota",
            "Bedrock 1.26.51 login is blocked by the pinned upstream NetherNet TLS signaling issue"]
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
                        shutil.copyfile(log_path, args.output / f"backend-{index}-{identity}.log")
        else:
            backend_logs = run("docker", "logs", backend, check=False)
            (args.output / "backend-at-exit.log").write_text(backend_logs.stdout + backend_logs.stderr)
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
        if oauth_service:
            oauth_service.shutdown()
            oauth_service.server_close()
        for log in logs:
            log.close()
        # Refuse to remove a directory if any quota mount failed to unmount.
        mounts = "" if native else Path("/proc/mounts").read_text()
        if str(root) not in mounts:
            shutil.rmtree(root)
        else:
            print(f"Retaining mounted test storage: {root}")


if __name__ == "__main__":
    main()
