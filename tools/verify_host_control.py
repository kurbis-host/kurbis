#!/usr/bin/env python3
"""Exercise the real Pumpkin process and authenticated Unix RPC.

This verifies the host-control boundary, not client transfers or Docker quotas.
The binary must be built from runtime/pumpkin's pinned source and patch.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import uuid


def rpc(socket_path, token, action):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(30)
        connection.connect(str(socket_path))
        connection.sendall(json.dumps({"token": token, "action": action}).encode() + b"\n")
        response = b""
        while not response.endswith(b"\n"):
            part = connection.recv(8192)
            if not part:
                raise RuntimeError("host control closed without a framed response")
            response += part
            if len(response) > 1024 * 1024:
                raise RuntimeError("host-control response too large")
        return json.loads(response)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("output/runtime/host-control"))
    parser.add_argument("--protocol-dir", type=Path)
    parser.add_argument("--edition", choices=["java", "bedrock"], default="java")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    token = secrets.token_hex(32)
    # Keep the socket path short enough for macOS and Linux sockaddr_un.
    with tempfile.TemporaryDirectory(prefix="mh-rpc-", dir="/tmp") as temp:
        root = Path(temp)
        rpc_path = root / "host-control.sock"
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
            reservation.bind(("127.0.0.1", 0))
            bedrock_port = reservation.getsockname()[1]
        (root / "pumpkin.toml").write_text(f'''
allow_chat_reports = false
accepts_transfers = true
[networking.query]
enabled = false
[networking.rcon]
enabled = false
[networking.java]
enabled = true
online_mode = false
encryption = false
address = "127.0.0.1:{port}"
max_players = 0
view_distance = 2
simulation_distance = 2
[networking.bedrock]
enabled = true
online_mode = false
address = "127.0.0.1:{bedrock_port}"
view_distance = 2
simulation_distance = 2
[telemetry]
enabled = false
''')
        evidence = {"binary": str(binary), "java_port": port, "checks": []}
        with (output / "pumpkin.log").open("wb") as log:
            child = subprocess.Popen([str(binary)], cwd=root, stdin=subprocess.DEVNULL,
                                     stdout=log, stderr=subprocess.STDOUT,
                                     env={**os.environ, "PUMPKIN_HOST_CONTROL_SOCKET": str(rpc_path),
                                          "PUMPKIN_HOST_CONTROL_TOKEN": token})
            try:
                deadline = time.monotonic() + 120
                while True:
                    if child.poll() is not None:
                        raise RuntimeError(f"Pumpkin exited during readiness: {child.returncode}; see {output / 'pumpkin.log'}")
                    try:
                        status = rpc(rpc_path, token, "status")
                        if status.get("ok"):
                            break
                    except (OSError, ValueError):
                        pass
                    if time.monotonic() >= deadline:
                        raise RuntimeError("Pumpkin host-control readiness exceeded 120s")
                    time.sleep(0.2)
                assert status["players"] == 0, status
                assert status["online_authentication"] == {"java": False, "bedrock": False}, status
                evidence["checks"].append({"readiness": status})
                denied = rpc(rpc_path, "wrong-token", "status")
                assert denied["ok"] is False, denied
                evidence["checks"].append({"authentication_rejected": denied})
                players = rpc(rpc_path, token, "players")
                assert players["ok"] and players["waiting_players"] == [], players
                evidence["checks"].append({"players": players})
                invalid = rpc(rpc_path, token, "unknown_action")
                assert invalid["ok"] is False, invalid
                evidence["checks"].append({"invalid_request": invalid})
                if args.protocol_dir:
                    protocol_port = port if args.edition == "java" else bedrock_port
                    connection_ids = []
                    for attempt in range(2):
                        artifact = output / f"protocol-{attempt + 1}.json"
                        client = subprocess.Popen(["node", str(args.protocol_dir.resolve() / "verify.mjs"),
                            "--edition", args.edition, "--port", str(protocol_port), "--address", "native.mc.example.test",
                            "--hold-seconds", "10", "--output", str(artifact)],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
                        try:
                            deadline = time.monotonic() + 30
                            active = []
                            while time.monotonic() < deadline:
                                active = rpc(rpc_path, token, "players")["waiting_players"]
                                if active:
                                    break
                                if client.poll() is not None:
                                    break
                                time.sleep(0.2)
                            assert len(active) == 1, f"authenticated client was not exposed: {active}"
                            player = active[0]
                            assert player["name"] == "RuntimePlayer", player
                            assert player["verified_identity"] is None, "offline fixture must never claim a verified identity"
                            connection_id = player["connection_id"]
                            assert str(uuid.UUID(connection_id)) == connection_id
                            assert connection_id not in connection_ids, "a reconnect must receive a new connection ID"
                            connection_ids.append(connection_id)
                            output_text, _ = client.communicate(timeout=210)
                        finally:
                            if client.poll() is None:
                                client.terminate()
                                client.communicate(timeout=10)
                        (output / f"protocol-{attempt + 1}.log").write_text(output_text)
                        assert client.returncode == 0, output_text
                        proof = json.loads(artifact.read_text())
                        assert proof["passed"]
                        deadline = time.monotonic() + 20
                        while rpc(rpc_path, token, "status")["players"] != 0:
                            assert time.monotonic() < deadline, "Disconnected client remained active"
                            time.sleep(0.2)
                    after = rpc(rpc_path, token, "status")
                    assert after["activity_revision"] >= 4, after
                    evidence["checks"].append({"actual_client": args.edition,
                        "offline_identity_rejected": True, "reconnect_connection_ids": connection_ids,
                        "activity_after_leave": after})
                saved = rpc(rpc_path, token, "save")
                assert saved["ok"], saved
                evidence["checks"].append({"save_completed": saved})
                shutdown = rpc(rpc_path, token, "shutdown")
                assert shutdown["ok"], shutdown
                assert child.wait(timeout=60) == 0
                assert not rpc_path.exists(), "graceful exit must remove host-control socket"
                evidence["checks"].append({"graceful_exit": 0, "socket_removed": True})
                persistent = [str(p.relative_to(root)) for p in root.rglob("*") if p.is_file() and p.name not in {"pumpkin.toml"} and "logs" not in p.parts]
                assert persistent, "save/shutdown did not produce persistent world data"
                evidence["persistent_files"] = sorted(persistent)
                evidence["limitations"] = (["No connected players"] if not args.protocol_dir else []) + ["No external Java/Bedrock online-account authentication", "No Java/Bedrock transfer", "No Docker isolation or quotas"]
                evidence["passed"] = True
                (output / "result.json").write_text(json.dumps(evidence, indent=2) + "\n")
                print(json.dumps(evidence, indent=2))
            except BaseException as error:
                evidence["passed"] = False
                evidence["error"] = repr(error)
                (output / "result.json").write_text(json.dumps(evidence, indent=2) + "\n")
                raise
            finally:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()


if __name__ == "__main__":
    main()
