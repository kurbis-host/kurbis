#!/usr/bin/env python3
"""Verify durable queued wakes and live routes across local service restarts.

Uses an existing disposable PostgreSQL database, the local control-plane and
gateway binaries, the native Pumpkin build, and real Java/Bedrock clients. It
never starts Docker and refuses databases not explicitly named for this test.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

from verify_host_control import rpc


def free_port(kind=socket.SOCK_STREAM):
    with socket.socket(socket.AF_INET, kind) as connection:
        connection.bind(("127.0.0.1", 0))
        return connection.getsockname()[1]


def wait_for(check, description, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.25)
    raise RuntimeError(f"Timed out waiting for {description}")


class RestartVerification:
    def __init__(self, args):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix="krs-", dir="/tmp"))
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.token = secrets.token_hex(32)
        self.suffix = secrets.token_hex(4)
        self.live_subdomain = f"live-{self.suffix}"
        self.queued_subdomain = f"queue-{self.suffix}"
        self.live_user = uuid.uuid4()
        self.queued_user = uuid.uuid4()
        self.live_server = uuid.uuid4()
        self.queued_server = uuid.uuid4()
        self.worker = uuid.uuid4()
        self.waiting_java = free_port()
        self.waiting_bedrock = free_port(socket.SOCK_DGRAM)
        self.java_backend = free_port()
        self.bedrock_backend = free_port(socket.SOCK_DGRAM)
        self.java_gateway = free_port()
        self.bedrock_gateway = free_port(socket.SOCK_DGRAM)
        self.api_port = free_port()
        self.waiting_root = self.root / "waiting"
        self.waiting_root.mkdir()
        self.backend_root = self.root / "backend"
        self.backend_root.mkdir()
        self.control_sockets = {
            "waiting": self.root / "waiting.sock",
            "backend": self.root / "backend.sock",
        }
        self.processes = {}
        self.logs = {}
        self.heartbeat_stop = threading.Event()
        self.heartbeat_thread = None
        self.evidence = {"checks": [], "database": args.database_url}

    def sql(self, statement):
        result = subprocess.run(
            ["psql", self.args.database_url, "-qAt", "-c", statement],
            check=True, text=True, capture_output=True,
        )
        return result.stdout.strip()

    def launch(self, name, binary, env, cwd=None):
        previous = self.processes.get(name)
        if previous and previous.poll() is None:
            raise RuntimeError(f"{name} is already running")
        log = (self.output / f"{name}-{len(self.logs.get(name, [])) + 1}.log").open("wb")
        self.logs.setdefault(name, []).append(log)
        process = subprocess.Popen(
            [str(binary)], cwd=cwd, env={**os.environ, **env},
            stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
        )
        self.processes[name] = process
        return process

    def stop(self, name):
        process = self.processes.get(name)
        if process is None or process.poll() is not None:
            return
        process.send_signal(signal.SIGTERM)
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)

    def start_pumpkin(self, name, root, java_port, bedrock_port):
        source = Path(__file__).resolve().parent.parent / "examples/config/native-waiting.toml"
        config = source.read_text()
        config = config.replace("127.0.0.1:25566", f"127.0.0.1:{java_port}")
        config = config.replace("127.0.0.1:19133", f"127.0.0.1:{bedrock_port}")
        (root / "pumpkin.toml").write_text(config)
        process = self.launch(name, Path(__file__).with_name("native_pumpkin_fixture.py"), {
            "NATIVE_FIXTURE_BINARY": str(self.args.native_binary),
            "PUMPKIN_HOST_CONTROL_SOCKET": str(self.control_sockets[name]),
            "PUMPKIN_HOST_CONTROL_TOKEN": self.token,
        }, cwd=root)
        def waiting_ready():
            try:
                return rpc(self.control_sockets[name], self.token, "status").get("ok")
            except (OSError, ValueError):
                return False
        wait_for(waiting_ready, f"{name} Pumpkin readiness")
        if process.poll() is not None:
            raise RuntimeError(f"{name} Pumpkin exited during startup")

    def start_control_plane(self):
        env = {
            "DATABASE_URL": self.args.database_url,
            "CONTROL_PLANE_LISTEN": f"127.0.0.1:{self.api_port}",
            "PUBLIC_BASE_URL": f"http://127.0.0.1:{self.api_port}",
            "ROOT_DOMAIN": "mc.example.test",
            "INTERNAL_TOKEN": self.token,
            "GOOGLE_OAUTH_CLIENT_ID": "restart-test-google",
            "GOOGLE_OAUTH_CLIENT_SECRET": self.token,
            "DISCORD_OAUTH_CLIENT_ID": "restart-test-discord",
            "DISCORD_OAUTH_CLIENT_SECRET": self.token,
        }
        process = self.launch("control-plane", self.args.bin_dir / "control-plane", env)
        wait_for(lambda: self.get("/healthz"), "control-plane health", timeout=90)
        if process.poll() is not None:
            raise RuntimeError("Control plane exited during startup")

    def start_gateway(self):
        process = self.launch("gateway", self.args.bin_dir / "gateway", {
            "DATABASE_URL": self.args.database_url,
            "JAVA_LISTEN": f"127.0.0.1:{self.java_gateway}",
            "BEDROCK_LISTEN": f"127.0.0.1:{self.bedrock_gateway}",
            "WAITING_JAVA_ADDRESS": f"127.0.0.1:{self.waiting_java}",
            "WAITING_BEDROCK_ADDRESS": f"127.0.0.1:{self.waiting_bedrock}",
            "WAITING_CONTROL_SOCKET": str(self.control_sockets["waiting"]),
            "PUMPKIN_GATEWAY_CONTROL_TOKEN": self.token,
            "CONTROL_PLANE_URL": f"http://127.0.0.1:{self.api_port}",
            "INTERNAL_TOKEN": self.token,
            "ROOT_DOMAIN": "mc.example.test",
            "WAKE_TIMEOUT_SECONDS": "120",
            "ALLOW_OFFLINE_WAITING_FOR_TESTS": "true",
        })
        wait_for(self.gateway_accepting_connections, "gateway listener")
        if process.poll() is not None:
            raise RuntimeError("Gateway exited during startup")

    def gateway_accepting_connections(self):
        try:
            with socket.create_connection(("127.0.0.1", self.java_gateway), timeout=1):
                return True
        except OSError:
            return False

    def get(self, path):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{self.api_port}{path}", timeout=2) as response:
                return response.status < 400
        except (OSError, urllib.error.URLError):
            return False

    def seed_database(self):
        self.sql(
            "INSERT INTO users(id,display_name) VALUES "
            f"('{self.live_user}','Live Route'),('{self.queued_user}','Queued Route');"
            "INSERT INTO identities(id,user_id,provider,subject,display_name) VALUES "
            f"(gen_random_uuid(),'{self.live_user}','google','restart-{self.live_user}','Live Route'),"
            f"(gen_random_uuid(),'{self.queued_user}','google','restart-{self.queued_user}','Queued Route');"
            "INSERT INTO servers(id,owner_id,subdomain,desired_state,observed_state,generation) VALUES "
            f"('{self.live_server}','{self.live_user}','{self.live_subdomain}','running','online',1),"
            f"('{self.queued_server}','{self.queued_user}','{self.queued_subdomain}','stopped','sleeping',0);"
            "INSERT INTO workers(id,name,address,total_memory_bytes,total_disk_bytes,total_cpu_millis,"
            "port_min,port_max,heartbeat_at,draining) VALUES "
            f"('{self.worker}','restart-{self.suffix}','127.0.0.1',8589934592,"
            f"107374182400,8000,46000,46020,now(),true);"
            "INSERT INTO placements(server_id,worker_id,generation,lease_token,java_port,bedrock_port,active) VALUES "
            f"('{self.live_server}','{self.worker}',1,'{uuid.uuid4()}',{self.java_backend},{self.bedrock_backend},true)"
        )

    def keep_worker_healthy(self):
        def heartbeat():
            while not self.heartbeat_stop.wait(5):
                try:
                    self.sql(f"UPDATE workers SET heartbeat_at=now() WHERE id='{self.worker}'")
                except Exception:
                    # The assertion path reports a missing route/heartbeat;
                    # this background fixture only models a healthy worker.
                    continue
        self.heartbeat_thread = threading.Thread(target=heartbeat, name="fixture-worker-heartbeat", daemon=True)
        self.heartbeat_thread.start()

    def run_client(self, edition, subdomain, stage, expected_event):
        if edition == "java":
            port = self.java_gateway
        else:
            port = self.bedrock_gateway
        artifact = self.output / f"{stage}-{subdomain}-{edition}.json"
        log_path = self.output / f"{stage}-{subdomain}-{edition}.log"
        command = [
            "node", str(self.args.protocol_dir / "verify.mjs"),
            "--edition", edition, "--port", str(port),
            "--address", f"{subdomain}.mc.example.test",
            "--username", f"Restart{edition.title()}",
            "--hold-seconds", "5", "--timeout", "90", "--output", str(artifact),
        ]
        if expected_event == "direct_route" and edition == "bedrock":
            command.extend(["--transfer-port", str(self.bedrock_backend)])
        elif expected_event == "direct_route":
            command.append("--direct")
        destination = "backend" if expected_event == "direct_route" else "waiting"
        other = "waiting" if destination == "backend" else "backend"
        waiting_is_transfer_hop = edition == "bedrock" and destination == "backend"
        player_name = f"Restart{edition.title()}"
        with log_path.open("wb") as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 75
                present = False
                while time.monotonic() < deadline and process.poll() is None:
                    observed = {}
                    for location in (destination, other):
                        try:
                            response = rpc(self.control_sockets[location], self.token, "players")
                            observed[location] = [entry["name"] for entry in response["waiting_players"]]
                        except (OSError, KeyError, ValueError):
                            observed[location] = []
                    if player_name in observed[other] and not waiting_is_transfer_hop:
                        raise AssertionError(f"{player_name} reached the {other} runtime: {observed}")
                    if player_name in observed[destination]:
                        present = True
                        break
                    time.sleep(0.25)
                if not present:
                    raise AssertionError(f"{player_name} never reached the {destination} runtime")
                result_code = process.wait(timeout=110)
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
        if result_code:
            raise RuntimeError(f"{edition} client failed for {subdomain}: {log_path.read_text(errors='replace')}")
        proof = json.loads(artifact.read_text())
        assert proof.get("passed") is True, proof
        events = [entry["event"] for entry in proof["events"]]
        if expected_event == "direct_route":
            if edition == "bedrock":
                assert "native_transfer" in events and "backend_joined" in events, events
            else:
                assert "backend_joined" in events and "native_transfer" not in events, events
        else:
            assert expected_event in events, f"{expected_event} absent from {events}"
        return events

    def verify_routes(self, stage):
        live = {}
        for edition in ("java", "bedrock"):
            live[edition] = self.run_client(edition, self.live_subdomain, stage, "direct_route")
        queued = {}
        for edition in ("java", "bedrock"):
            queued[edition] = self.run_client(edition, self.queued_subdomain, stage, "waiting_joined")
        summary = self.sql(
            "SELECT json_build_array(s.observed_state::text,"
            "(SELECT count(*) FROM start_requests WHERE server_id=s.id)::int,"
            "(SELECT requested_at FROM start_requests WHERE server_id=s.id)) "
            f"FROM servers s WHERE s.id='{self.queued_server}'"
        )
        state, queue_count, requested_at = json.loads(summary)
        assert state == "queued" and queue_count == 1, (state, queue_count)
        live_state, live_start_requests, live_lifecycle_ops = json.loads(self.sql(
            "SELECT json_build_array(s.observed_state::text,"
            "(SELECT count(*) FROM start_requests WHERE server_id=s.id)::int,"
            "(SELECT count(*) FROM operations WHERE server_id=s.id AND kind IN ('start','stop','restart','delete') "
            "AND status IN ('pending','running'))::int) "
            f"FROM servers s WHERE s.id='{self.live_server}'"
        ))
        assert live_state == "online" and live_start_requests == 0 and live_lifecycle_ops == 0, (
            live_state, live_start_requests, live_lifecycle_ops
        )
        placement = self.sql(
            "SELECT json_build_object('worker',worker_id,'java_port',java_port,'bedrock_port',bedrock_port,"
            "'generation',generation,'lease',lease_token,'active',active) "
            f"FROM placements WHERE server_id='{self.live_server}'"
        )
        assert placement and json.loads(placement)["active"] is True
        self.evidence["checks"].append({
            "stage": stage,
            "live_placement": json.loads(placement),
            "live_real_clients": {"destination": "worker backend process", "events": live},
            "queued": {"state": state, "entry_count": queue_count, "requested_at": requested_at,
                       "destination": "waiting process", "real_clients": queued},
        })
        return requested_at, placement

    def run(self):
        try:
            self.start_pumpkin("waiting", self.waiting_root, self.waiting_java, self.waiting_bedrock)
            self.start_pumpkin("backend", self.backend_root, self.java_backend, self.bedrock_backend)
            self.start_control_plane()
            self.seed_database()
            self.keep_worker_healthy()
            self.start_gateway()
            initial_queue_time, initial_placement = self.verify_routes("before-restart")

            self.stop("gateway")
            self.stop("control-plane")
            self.evidence["checks"].append({"service_restart": ["control-plane", "gateway"]})

            persisted = self.sql(
                "SELECT json_build_object('state',s.observed_state,'queued',"
                "(SELECT count(*) FROM start_requests WHERE server_id=s.id), 'requested_at',"
                "(SELECT requested_at FROM start_requests WHERE server_id=s.id)) "
                f"FROM servers s WHERE s.id='{self.queued_server}'"
            )
            persisted = json.loads(persisted)
            assert persisted["state"] == "queued" and persisted["queued"] == 1
            assert persisted["requested_at"] == initial_queue_time
            assert self.sql(
                "SELECT json_build_object('worker',worker_id,'java_port',java_port,'bedrock_port',bedrock_port,"
                "'generation',generation,'lease',lease_token,'active',active) "
                f"FROM placements WHERE server_id='{self.live_server}'"
            ) == initial_placement

            self.start_control_plane()
            self.start_gateway()
            after_queue_time, after_placement = self.verify_routes("after-restart")
            assert after_queue_time == initial_queue_time
            assert after_placement == initial_placement
            self.evidence["passed"] = True
        except BaseException as error:
            self.evidence["passed"] = False
            self.evidence["error"] = repr(error)
            raise
        finally:
            self.heartbeat_stop.set()
            if self.heartbeat_thread:
                self.heartbeat_thread.join(timeout=6)
            for name in ("gateway", "control-plane", "backend", "waiting"):
                self.stop(name)
            try:
                self.sql(
                    f"DELETE FROM users WHERE id IN ('{self.live_user}','{self.queued_user}');"
                    f"DELETE FROM workers WHERE id='{self.worker}'"
                )
            except Exception as error:
                self.evidence.setdefault("cleanup_error", repr(error))
            for group in self.logs.values():
                for log in group:
                    log.close()
            shutil.rmtree(self.root, ignore_errors=True)
            (self.output / "result.json").write_text(json.dumps(self.evidence, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--native-binary", type=Path, required=True)
    parser.add_argument("--database-url", required=True)
    parser.add_argument("--protocol-dir", type=Path, default=Path("tools/protocol"))
    parser.add_argument("--output", type=Path, default=Path("output/runtime/gateway-restart"))
    args = parser.parse_args()
    if not args.database_url.rsplit("/", 1)[-1].endswith("_restart_e2e"):
        parser.error("--database-url must point to a disposable database ending in _restart_e2e")
    args.bin_dir = args.bin_dir.resolve(strict=True)
    args.native_binary = args.native_binary.resolve(strict=True)
    args.protocol_dir = args.protocol_dir.resolve(strict=True)
    for name in ("control-plane", "gateway"):
        if not (args.bin_dir / name).is_file():
            parser.error(f"missing local binary: {args.bin_dir / name}")
    result = RestartVerification(args)
    try:
        result.run()
    except BaseException:
        print(json.dumps(result.evidence, indent=2), file=sys.stderr)
        raise
    print(json.dumps(result.evidence, indent=2))


if __name__ == "__main__":
    main()
