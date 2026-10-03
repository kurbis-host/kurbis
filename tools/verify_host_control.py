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


def rpc(socket_path, token, action, **fields):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(30)
        connection.connect(str(socket_path))
        connection.sendall(json.dumps({"token": token, "action": action, **fields}).encode() + b"\n")
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
enabled = false
online_mode = false
view_distance = 2
simulation_distance = 2
[telemetry]
enabled = false
''')
        unmanaged_operator_id = str(uuid.uuid4())
        owner_operator_id = str(uuid.uuid4())
        managed_operator_id = str(uuid.uuid4())
        # Seed a legitimate operator as if it came from ops.json/in-game admin
        # setup before Kurbis applies its server-owned operator roster.
        ops_path = root / "data" / "ops.json"
        ops_path.parent.mkdir(parents=True, exist_ok=True)
        seeded_unmanaged_operator = {
            "uuid": unmanaged_operator_id,
            "name": "FixtureVanillaOp",
            "level": 2,
            "bypasses_player_limit": False,
        }
        ops_path.write_text(json.dumps([seeded_unmanaged_operator], indent=2) + "\n")
        evidence = {
            "binary": str(binary),
            "java_port": port,
            "unmanaged_operator_id": unmanaged_operator_id,
            "owner_operator_id": owner_operator_id,
            "managed_operator_id": managed_operator_id,
            "checks": [],
        }
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
                initial_ops = json.loads(ops_path.read_text())
                seeded = next(entry for entry in initial_ops if entry["uuid"] == unmanaged_operator_id)
                assert seeded["level"] == 2 and seeded["name"] == "FixtureVanillaOp", initial_ops
                evidence["checks"].append({"unmanaged_operator_seeded": seeded})

                managed_roster = [
                    {"uuid": owner_operator_id, "name": "FixtureOwner"},
                    {"uuid": managed_operator_id, "name": "FixtureManagedOp"},
                ]
                managed = rpc(
                    rpc_path,
                    token,
                    "operators_replace",
                    operators=managed_roster,
                )
                assert managed["ok"], managed
                ops = json.loads(ops_path.read_text())
                unmanaged = next(entry for entry in ops if entry["uuid"] == unmanaged_operator_id)
                owner = next(entry for entry in ops if entry["uuid"] == owner_operator_id)
                panel_operator = next(entry for entry in ops if entry["uuid"] == managed_operator_id)
                assert unmanaged["level"] == 2 and unmanaged["name"] == "FixtureVanillaOp", ops
                assert owner["level"] == 4 and owner["name"] == "FixtureOwner", owner
                assert panel_operator["level"] == 4 and panel_operator["name"] == "FixtureManagedOp", panel_operator
                sidecar_path = root / "plugins" / "data" / "kurbis-control" / "operator-config.json"
                sidecar = json.loads(sidecar_path.read_text())
                assert set(sidecar["player_ids"]) == {owner_operator_id, managed_operator_id}, sidecar
                assert any(entry["uuid"] == unmanaged_operator_id for entry in sidecar["unrelated_ops"]), sidecar
                evidence["checks"].append({
                    "managed_operator_roster_applied": {
                        "owner": owner,
                        "managed": panel_operator,
                        "preserved_unmanaged": unmanaged,
                    }
                })

                # Revoke one managed roster entry while retaining the actual
                # server owner. The unrelated preexisting ops.json entry must
                # survive both the grant and revoke transitions unchanged.
                revoked = rpc(
                    rpc_path,
                    token,
                    "operators_replace",
                    operators=[{"uuid": owner_operator_id, "name": "FixtureOwner"}],
                )
                assert revoked["ok"], revoked
                ops = json.loads(ops_path.read_text())
                assert all(entry["uuid"] != managed_operator_id for entry in ops), ops
                owner = next(entry for entry in ops if entry["uuid"] == owner_operator_id)
                unmanaged = next(entry for entry in ops if entry["uuid"] == unmanaged_operator_id)
                assert owner["level"] == 4 and owner["name"] == "FixtureOwner", ops
                assert unmanaged["level"] == 2 and unmanaged["name"] == "FixtureVanillaOp", ops
                sidecar = json.loads(sidecar_path.read_text())
                assert sidecar["player_ids"] == [owner_operator_id], sidecar
                assert [entry["uuid"] for entry in sidecar["unrelated_ops"]] == [unmanaged_operator_id], sidecar
                evidence["checks"].append({
                    "managed_operator_revoked_owner_and_unmanaged_preserved": {
                        "owner": owner,
                        "unmanaged": unmanaged,
                        "sidecar": sidecar,
                    }
                })
                command = rpc(rpc_path, token, "execute_command", command="list")
                assert command["ok"] and command["output"], command
                evidence["checks"].append({"console_command": command})
                rejected_by_dispatcher = rpc(
                    rpc_path,
                    token,
                    "execute_command",
                    command="kurbis_fixture_unknown_command",
                )
                assert rejected_by_dispatcher["ok"] is True, rejected_by_dispatcher
                assert any("unknown command" in line.lower() for line in rejected_by_dispatcher["output"]), rejected_by_dispatcher
                evidence["checks"].append({"dispatcher_rejection_output": rejected_by_dispatcher})
                invalid_command = rpc(rpc_path, token, "execute_command", command="   ")
                assert invalid_command["ok"] is False, invalid_command
                evidence["checks"].append({"invalid_console_command": invalid_command})
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
