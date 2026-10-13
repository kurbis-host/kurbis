#!/usr/bin/env python3
"""Time a sleeping server's start from a player's join, phase by phase.

Runs the real control plane, gateway, waiting Pumpkin and one native worker
against a disposable PostgreSQL database, then joins with the Java protocol
client the way a player does: through the gateway, to a sleeping server. Each
run reports where the time between "connect" and "spawned in the world" went.

Native mode has no Docker, so this covers everything a start shares between
runtimes: the gateway, the control plane, the worker's claim, storage restore,
Pumpkin's boot and the hand-off back to the player. The worker logs the same
split for every production start ("server start completed"), including the
Docker-only steps.

    createdb kurbis_start_timing
    python3 tools/measure_server_start.py --bin-dir target/debug \\
        --native-binary output/native-pumpkin/target/release/pumpkin \\
        --database-url postgres://localhost/kurbis_start_timing
"""
import argparse
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import urllib.request
import uuid

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS / "protocol"))

from runtime_verification_auth import create_user_session, start_provider  # noqa: E402
from runtime_verification_contract import command_request, command_route, read_worker_bundle  # noqa: E402
from runtime_verification_protocol import (  # noqa: E402
    create_test_tls_certificate,
    free_port,
    run,
    start_gateway,
    wait,
)
from verify_java_263 import PROTOCOL, ProtocolClient  # noqa: E402

# The offline UUID the Java client's default username resolves to.
PLAYER_ID = "63afe389-0d98-f833-4096-cc01dd62a9f0"
HOSTNAME = "live.mc.example.test"
ANSI = re.compile(r"\x1b\[[0-9;]*m")


class TimedClient(ProtocolClient):
    """The verification client, with a wall-clock time on every event."""

    def record(self, event, **data):
        self.evidence["events"].append({"event": event, "time": time.time(), **data})


def join(port, timeout):
    """Join through the gateway and return (connect time, spawn time)."""
    arguments = SimpleNamespace(
        protocol=PROTOCOL, username="RuntimePlayer", timeout=timeout, hold_seconds=0,
        transfer_port=None, transfer_port_range=[], transfer_host="127.0.0.1",
        transfer_connect_host=None, expect_kick=None, expect_close=False, command=[],
        capture_login_link=False, expect_item=[], expect_message=[], forbid_message=[],
        click_slot=[], expect_gui_opens=None,
    )
    evidence = {"events": []}
    connected = time.time()
    TimedClient(arguments, evidence).connect("127.0.0.1", port, HOSTNAME, True)
    spawned = next(event["time"] for event in evidence["events"] if event["event"] == "backend_joined")
    return connected, spawned


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--native-binary", type=Path, required=True)
    parser.add_argument("--plugin-wasm", type=Path,
                        default=TOOLS.parent / "plugins/kurbis-control/target/wasm32-wasip2/release/kurbis_control.wasm")
    parser.add_argument("--database-url", required=True, help="an empty, disposable database")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--output", type=Path, default=TOOLS.parent / "output/start-timing")
    args = parser.parse_args()
    args.bin_dir = args.bin_dir.resolve(strict=True)
    args.native_binary = args.native_binary.resolve(strict=True)
    args.plugin_wasm = args.plugin_wasm.resolve(strict=True)
    shutil.rmtree(args.output, ignore_errors=True)
    args.output.mkdir(parents=True)

    root = Path(tempfile.mkdtemp(prefix="kurbis-start-", dir="/tmp"))
    secret = secrets.token_hex(32)
    api_port, oauth_port, worker_port = free_port(), free_port(), 47000
    api = f"http://127.0.0.1:{api_port}"
    server, owner, worker_id = str(uuid.uuid4()), str(uuid.uuid4()), str(uuid.uuid4())
    processes, logs = [], []

    def sql(query):
        return run("psql", args.database_url, "-X", "-qAt", "-c", query).stdout.strip()

    def launch(binary, environment, name):
        log = (args.output / f"{name}.log").open("wb")
        logs.append(log)
        keep = {"PATH", "HOME", "LANG", "TMPDIR"}
        base = {key: value for key, value in os.environ.items() if key in keep} if binary == "worker" else os.environ.copy()
        process = subprocess.Popen([str(args.bin_dir / binary)], env={**base, **environment},
                                   stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        return process

    def state():
        return sql(f"SELECT observed_state FROM servers WHERE id = '{server}'")

    def epoch(column, operation):
        return float(sql(f"SELECT extract(epoch FROM {column}) FROM operations WHERE id = '{operation}'"))

    try:
        start_provider("127.0.0.1", oauth_port)
        common = {"DATABASE_URL": args.database_url, "OBJECT_STORE_TYPE": "local",
                  "OBJECT_STORE_LOCAL_ROOT": str(root / "objects")}
        oauth = {f"{provider}_OAUTH_{key}": value for provider in ("GOOGLE", "DISCORD") for key, value in {
            "CLIENT_ID": "timing", "CLIENT_SECRET": secret,
            "AUTHORIZE_URL": f"http://127.0.0.1:{oauth_port}/authorize",
            "TOKEN_URL": f"http://127.0.0.1:{oauth_port}/token",
            "USERINFO_URL": f"http://127.0.0.1:{oauth_port}/userinfo"}.items()}
        launch("control-plane", {**common, **oauth, "CONTROL_PLANE_LISTEN": f"127.0.0.1:{api_port}",
                                 "PUBLIC_BASE_URL": api, "PANEL_ORIGIN": "http://127.0.0.1:18082",
                                 "ROOT_DOMAIN": "mc.example.test", "INTERNAL_TOKEN": secret}, "control-plane")

        def healthy():
            try:
                with urllib.request.urlopen(f"{api}/healthz", timeout=2) as response:
                    return response.status == 204
            except OSError:
                return False
        wait(healthy, "control plane", 90)

        sql(f"INSERT INTO users(id,display_name) VALUES('{owner}','Timing'); "
            f"INSERT INTO identities(id,user_id,provider,subject,display_name) "
            f"VALUES(gen_random_uuid(),'{owner}','google','{owner}','Timing'); "
            f"INSERT INTO identities(id,user_id,provider,subject,display_name,minecraft_player_id) "
            f"VALUES(gen_random_uuid(),'{owner}','minecraft_java','{PLAYER_ID}','RuntimePlayer','{PLAYER_ID}'); "
            f"INSERT INTO servers(id,owner_id,subdomain) VALUES('{server}','{owner}','live')")
        cookie = create_user_session(api, owner)
        contract = json.loads(run(str(args.bin_dir / "api-openapi")).stdout)
        commands = command_route(contract).replace("{server_id}", server)

        def stop():
            request = urllib.request.Request(
                f"{api}{commands}", data=json.dumps(command_request(contract, "stop")).encode(),
                method="POST", headers={"Cookie": cookie, "Content-Type": "application/json"})
            urllib.request.urlopen(request, timeout=15).close()
            wait(lambda: state() == "sleeping", "save, shutdown and snapshot", 120)

        bundle = root / "worker.env"
        run(str(args.bin_dir / "worker-bundle"), "--worker-id", worker_id, "--database-url", args.database_url,
            "--output", str(bundle), env={
                **{key: os.environ[key] for key in ("PATH", "HOME") if key in os.environ},
                "WORKER_ID": worker_id, "WORKER_NAME": "start-timing", "WORKER_ADDRESS": "127.0.0.1",
                "WORKER_SIGNAL_ADDRESS": "127.0.0.1", "WORKER_CONTROL_PLANE_URL": api,
                "WORKER_RUNTIME_API_PROXY_UPSTREAM": api, "WORKER_RUNTIME_API_EGRESS_NETWORK": "bridge",
                "WORKER_RUNTIME_API_PROXY_IMAGE": "unused-native-proxy-image", "PUBLIC_BASE_URL": api,
                "WORKER_RUNTIME": "native", "WORKER_STORAGE_MODE": "directory",
                "WORKER_ROOT": str(root / "worker"), "WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP": "true",
                "SNAPSHOT_INTERVAL_SECONDS": "3600", "IDLE_TIMEOUT_SECONDS": "3600",
                "WORKER_MEMORY_GIB": "2", "WORKER_DISK_GIB": "20", "WORKER_CPU_MILLIS": "2000",
                "WORKER_PORT_MIN": str(worker_port), "WORKER_PORT_MAX": str(worker_port + 20),
                # The offline-identity wrapper; it execs the real binary below.
                "PUMPKIN_BINARY": str(TOOLS / "native_pumpkin_fixture.py"),
                "KURBIS_CONTROL_PLUGIN_WASM": str(args.plugin_wasm)})
        launch("worker", {**read_worker_bundle(bundle), "NATIVE_FIXTURE_BINARY": str(args.native_binary),
                          "KURBIS_ALLOW_UNSANDBOXED_NATIVE_DEV": "true"}, "worker")
        wait(lambda: sql(f"SELECT count(*) FROM workers WHERE id = '{worker_id}' "
                         "AND heartbeat_at > now() - interval '30 seconds'") == "1", "worker heartbeat", 60)

        certificate, private_key = create_test_tls_certificate(root)
        gateway, _, _ = start_gateway(
            args=args, native=True, root=root, waiting_java_port=free_port(), waiting_bedrock_port=free_port(),
            gateway_java_port=free_port(), bedrock_public_port=free_port(), api_port=api_port, secret=secret,
            bedrock_certificate=certificate, bedrock_private_key=private_key, common=common, launch=launch,
            wait_for=wait, processes=processes, logs=logs, waiting_container=None)

        worker_log = args.output / "worker.log"
        results = []
        for index in range(args.runs + 1):
            consumed = worker_log.stat().st_size
            connected, spawned = join(gateway, 180)
            operation = sql(f"SELECT id FROM operations WHERE server_id = '{server}' AND kind = 'start' "
                            "ORDER BY created_at DESC LIMIT 1")
            wait(lambda: sql(f"SELECT status FROM operations WHERE id = '{operation}'") == "succeeded",
                 "start operation recorded", 30)
            requested, claimed, completed = (epoch(column, operation) for column in ("created_at", "claimed_at", "updated_at"))
            with worker_log.open("rb") as log:
                log.seek(consumed)
                tail = ANSI.sub("", log.read().decode(errors="replace"))
            reported = re.search(r'phases=(?:"([^"]+)"|(\S+(?: \S+=\d+ms)*))', tail)
            phases = (reported.group(1) or reported.group(2)) if reported else ""
            pumpkin = re.findall(r"Started server; took (\S+)",
                                 (root / "worker/servers" / server / "run/native.log").read_text(errors="replace"))
            warm_connected, warm_spawned = join(gateway, 60)
            result = {
                "world": "new" if index == 0 else "restored",
                "total": spawned - connected,
                "gateway_and_placement": requested - connected,
                "worker_claim_wait": claimed - requested,
                "worker_start": completed - claimed,
                "handoff_and_login": spawned - completed,
                "login_alone_on_running_server": warm_spawned - warm_connected,
                "worker_phases": phases,
                "pumpkin_reported_boot": pumpkin[-1] if pumpkin else None,
            }
            results.append(result)
            print(json.dumps(result), flush=True)
            stop()

        summary = {"runs": results}
        restored = [result for result in results if result["world"] == "restored"]
        if restored:
            summary["median_restored"] = {
                key: round(statistics.median(result[key] for result in restored), 3)
                for key in ("total", "gateway_and_placement", "worker_claim_wait", "worker_start",
                            "handoff_and_login", "login_alone_on_running_server")}
            print(json.dumps({"median_restored": summary["median_restored"]}), flush=True)
        (args.output / "result.json").write_text(json.dumps(summary, indent=2) + "\n")
    finally:
        for process in reversed(processes):
            process.terminate()
        for process in processes:
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                process.kill()
        for log in logs:
            log.close()
        # Native Pumpkin outlives its worker by design; the run directory is disposable.
        run("pkill", "-f", str(root), check=False)
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
