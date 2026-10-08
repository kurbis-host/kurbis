#!/usr/bin/env python3
"""Exercise Kurbis AI on the official binary with a real 26.3 protocol client.

Uses an isolated runtime and deterministic Codex event fixture. No external Python deps.
Run after bootstrap: python3 test-server/tests/dashboard_e2e.py
"""
import json
import pathlib
import socket
import struct
import subprocess
import tempfile
import threading
import time
import zlib

import reload_fixture
import reload_checks

ROOT = pathlib.Path(__file__).resolve().parents[1]


def vi(value):
    out = bytearray()
    while True:
        b = value & 127
        value >>= 7
        out.append(b | (128 if value else 0))
        if not value:
            return bytes(out)


def decode(data, offset=0):
    value = shift = 0
    while True:
        b = data[offset]
        offset += 1
        value |= (b & 127) << shift
        if not b & 128:
            return value, offset
        shift += 7


def string(value):
    value = value.encode()
    return vi(len(value)) + value


def eventually(check, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError("Timed out waiting for expected state")


class Client:
    def __init__(self, port, name):
        self.socket = socket.create_connection(("127.0.0.1", port))
        self.socket.settimeout(30)
        self.compression = None
        self.lock = threading.Lock()
        self.packets = []
        self.window = self.revision = 0
        self.state = "login"
        self.error = None
        self.gamemode = None
        self.send(0, vi(777) + string("localhost") + struct.pack(">H", port) + vi(2))
        self.send(0, string(name) + bytes(16))
        threading.Thread(target=self.read, daemon=True).start()
        try:
            eventually(lambda: self.state == "play")
        except AssertionError:
            raise AssertionError(f"Login stalled: state={self.state}, error={self.error}, packets={[(p, b[:100]) for p,b in self.packets[:10]]}")

    def send(self, packet, body=b""):
        body = vi(packet) + body
        if self.compression is not None:
            body = vi(len(body)) + zlib.compress(body) if len(body) >= self.compression else vi(0) + body
        with self.lock:
            self.socket.sendall(vi(len(body)) + body)

    def read_exact(self, size):
        out = b""
        while len(out) < size:
            block = self.socket.recv(size - len(out))
            if not block:
                raise EOFError()
            out += block
        return out

    def read(self):
        try:
            while True:
                prefix = b""
                while not prefix or prefix[-1] & 128:
                    prefix += self.read_exact(1)
                size, _ = decode(prefix)
                data = self.read_exact(size)
                if self.compression is not None:
                    uncompressed, off = decode(data)
                    data = zlib.decompress(data[off:]) if uncompressed else data[off:]
                packet, offset = decode(data)
                body = data[offset:]
                if self.state != "play": self.packets.append((packet, body))
                if self.state == "login" and packet == 3:
                    self.compression, _ = decode(body)
                elif self.state == "login" and packet == 2:
                    self.send(3)
                    self.send(0, string("en_us") + b"\x0a" + vi(0) + b"\x01\x7f" + vi(1) + b"\x00\x01")
                    self.state = "config"
                elif self.state == "config":
                    if packet == 15:
                        self.send(7, vi(0))
                    elif packet == 4:
                        self.send(4, body)
                    elif packet == 5:
                        self.send(5, body)
                    elif packet == 3:
                        self.send(3)
                        self.state = "play"
                elif self.state == "play":
                    self.packets.append((packet, body))
                    if packet == 39 and body[0] == 3:
                        self.gamemode = int(struct.unpack(">f", body[1:5])[0])
                    if packet == 45:
                        self.send(28, body)
                    elif packet == 62:
                        self.send(45, body)
                    elif packet == 73:
                        teleport, off = decode(body)
                        self.send(0, vi(teleport) + body[off:off + 24] + body[off + 48:off + 56])
                    elif packet == 11:
                        self.send(11, struct.pack(">f", 10))
                    elif packet == 60:
                        self.window, _ = decode(body)
                    elif packet == 18:
                        window, off = decode(body)
                        revision, off = decode(body, off)
                        if window == self.window:
                            self.revision = revision
        except (OSError, EOFError) as error:
            self.error = str(error)

    def command(self, text):
        self.send(7, string(text))

    def chat(self, text):
        self.send(9, string(text) + struct.pack(">qq", int(time.time() * 1000), 0) + b"\x00" + vi(0) + bytes(4))

    def click(self, slot, mode=0, button=0, window=None):
        self.send(18, vi(self.window if window is None else window) + vi(self.revision) + struct.pack(">hb", slot, button) + vi(mode) + vi(0) + b"\x00")

    def close(self):
        self.send(19, vi(self.window))

    def seen(self, text, since=0, packet=None):
        return any(text.encode() in body for pid, body in self.packets[since:] if packet is None or pid == packet)

    def menu(self, title, operation):
        start = len(self.packets)
        operation()
        eventually(lambda: self.seen(title, start, 60))
        eventually(lambda: any(pid == 18 and b"Refresh" in body for pid, body in self.packets[start:]))
        time.sleep(0.1)
        return start


def run(runtime, port, gamemode, artifacts):
    server_dir = runtime / "server"
    (server_dir / "plugins").mkdir(parents=True)
    (server_dir / "plugins/kurbis-ai.wasm").write_bytes((ROOT / "runtime/server/plugins/kurbis-ai.wasm").read_bytes())
    config = (ROOT / "runtime/server/pumpkin.toml").read_text()
    config = config.replace("127.0.0.1:25565", f"127.0.0.1:{port}").replace("view_distance = 16", "view_distance = 2").replace("simulation_distance = 10", "simulation_distance = 2")
    config = config.replace("[networking.java.compression]\nenabled = true", "[networking.java.compression]\nenabled = false")
    config = config.replace("[telemetry]\nenabled = true", "[telemetry]\nenabled = false")
    (server_dir / "pumpkin.toml").write_text(config)
    (runtime / "fixture-artifacts.json").write_text(json.dumps(artifacts))
    worker = runtime / "fixture-codex"
    worker.write_text('''#!/usr/bin/env python3
import sys,json,uuid,pathlib,os
args=sys.argv[1:]
if "--device-auth" in args:
 pathlib.Path(os.environ["CODEX_HOME"],"auth.json").write_text("{\\"access_token\\":\\"fixture-token\\"}")
 print("Open https://auth.openai.com/codex/device and enter code: WXYZ-9876",flush=True)
 sys.exit(0)
plugin_dir=pathlib.Path(args[1])
pathlib.Path(plugin_dir,"captured-args.json").write_text(json.dumps(args))
prompt=sys.stdin.read()
if plugin_dir.name == "server-control" and "spawn a pig" in prompt:
 import subprocess,tomllib
 config=next(value for i,value in enumerate(args[:-1]) if value == "-c" for value in [args[i+1]] if value.startswith("mcp_servers.kurbis_ai="))
 server_config=tomllib.loads("mcp="+config.split("=",1)[1])["mcp"]
 child=subprocess.Popen([server_config["command"]]+server_config["args"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,bufsize=1)
 def rpc(message):
  child.stdin.write(json.dumps(message)+chr(10)); child.stdin.flush()
  return json.loads(child.stdout.readline())
 rpc({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})
 child.stdin.write(json.dumps({"jsonrpc":"2.0","method":"notifications/initialized"})+chr(10)); child.stdin.flush()
 tools=rpc({"jsonrpc":"2.0","id":2,"method":"tools/list"})["result"]["tools"]
 names={tool["name"] for tool in tools}
 assert {"spawn_entity","run_server_command"}.issubset(names),names
 spawned=rpc({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"spawn_entity","arguments":{"entity_type":"pig"}}})
 command=rpc({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"run_server_command","arguments":{"command":"say KURBIS_COMMAND_OK"}}})
 child.stdin.close(); child.wait(timeout=10)
 print(json.dumps({"type":"item.completed","item":{"type":"mcp_tool_result","tool":"spawn_entity","result":spawned}}),flush=True)
 print(json.dumps({"type":"item.completed","item":{"type":"mcp_tool_result","tool":"run_server_command","result":command}}),flush=True)
if plugin_dir.name in ("reload-probe", "fresh-probe"):
 version=prompt.split("PLAYER REQUEST:")[1].strip()
 artifacts=json.loads((pathlib.Path(args[1]).parents[2]/"fixture-artifacts.json").read_text())
 target=pathlib.Path(args[1],"target/wasm32-wasip2/release", pathlib.Path(args[1]).name.replace("-","_")+".wasm")
 target.parent.mkdir(parents=True,exist_ok=True)
 target.write_bytes(pathlib.Path(artifacts[version]).read_bytes())
 pathlib.Path(args[1],"revision.txt").write_text(version)

thread=args[args.index("resume")+1] if "resume" in args else str(uuid.uuid4())
print(json.dumps({"type":"thread.started","thread_id":thread}),flush=True)
print("fixture stderr diagnostic",file=sys.stderr,flush=True)
for i in range(40):
 print(json.dumps({"type":"item.completed","item":{"type":"command_execution","command":"echo step "+str(i),"aggregated_output":"output "+str(i)}}),flush=True)
print(json.dumps({"type":"item.completed","item":{"type":"agent_message","text":"Response to: "+prompt+" END-OF-RESPONSE"}}),flush=True)
print(json.dumps({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}),flush=True)
''')
    worker.chmod(0o755)
    log = (runtime / "server.log").open("w")
    server = subprocess.Popen(["python3", str(ROOT / "scripts/server_runner.py"), "--runtime", str(runtime), "--binary", str(ROOT / "runtime/bin/pumpkin")], cwd=server_dir, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT, text=True)
    controller_log = (runtime / "controller.log").open("w")
    controller = subprocess.Popen([str(ROOT / "target/debug/kurbis-ai-controller"), "--runtime", str(runtime), "--codex", str(worker)], stdout=controller_log, stderr=subprocess.STDOUT)
    clients = []
    try:
        def listening():
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=0.2): return True
            except OSError: return False
        eventually(listening)
        op = Client(port, "KurbisTester")
        guest = Client(port, "Guest")
        clients.extend([op, guest])
        server.stdin.write("op KurbisTester\n")
        server.stdin.flush()
        time.sleep(1)
        server.stdin.write("gamemode creative KurbisTester\n")
        server.stdin.flush()
        eventually(lambda: op.gamemode == 1)
        server.stdin.write(f"gamemode {gamemode} KurbisTester\n")
        server.stdin.flush()
        eventually(lambda: op.gamemode == (3 if gamemode == "spectator" else 0))
        data = server_dir / "plugins/data/kurbis-ai"
        if gamemode == "spectator":
            results = data / "results"
            before = set(results.glob("*.json"))
            start = len(guest.packets)
            op.command("ai ask spawn a pig and say a phrase")
            result_path = eventually(lambda: next(iter(set(results.glob("*.json")) - before), None))
            result = eventually(lambda: (value if value["state"] in ("completed", "failed") else None)
                                if (value := json.loads(result_path.read_text())) else None, timeout=20)
            assert result["state"] == "completed", result
            events = (data / "events" / (result["id"] + ".jsonl")).read_text()
            assert "spawn_entity" in events and "run_server_command" in events
            assert not guest.seen("spawn a pig", start), "Spectator prompt leaked"
            assert guest.seen("KURBIS_COMMAND_OK"), "Spectator server command did not execute"
            print("PASS (spectator): OP command route, immediate MCP spawn/command actions, audit, and private prompt")
            return
        op.menu("Workshop", lambda: op.command("ai"))
        assert op.seen("Create or edit a plugin") and op.seen("Conversations")
        old_window = op.window
        op.menu("Settings", lambda: op.click(34))
        settings = lambda: json.loads((data / "settings.json").read_text())
        op.click(22, window=old_window)
        time.sleep(0.2)
        assert settings()["reasoning"] == "medium", "Stale chest click changed settings"
        if gamemode == "spectator":
            op.click(22, mode=5)
            time.sleep(0.2)
            assert settings()["reasoning"] == "medium", "Drag activated a spectator button"
        op.menu("Settings", lambda: op.click(22))
        assert settings()["reasoning"] == "high"
        op.menu("Settings", lambda: op.click(24))
        assert settings()["timeout_seconds"] == 1800
        op.menu("Settings", lambda: op.click(31))
        assert not settings()["chat_progress"]
        op.menu("Settings", lambda: op.click(31))
        assert settings()["chat_progress"]
        op.click(20)
        eventually(lambda: op.seen("model ID, or default"))
        op.close()
        start = len(guest.packets)
        op.chat("fixture-model")
        eventually(lambda: settings()["model"] == "fixture-model")
        assert not guest.seen("fixture-model", start)
        start = len(guest.packets)
        guest.command("ai")
        time.sleep(0.5)
        assert not any(pid == 60 for pid, _ in guest.packets[start:]), "Non-op opened workshop"
        op.command("ai login")
        data = server_dir / "plugins/data/kurbis-ai"
        eventually(lambda: next(iter((data / "private/auth").glob("*.json")), None))
        eventually(lambda: op.seen("WXYZ-9876"))
        assert not guest.seen("WXYZ-9876"), "Device code leaked to another player"
        login_events = list((data / "events").glob("*.jsonl"))
        assert not any("WXYZ-9876" in path.read_text() for path in login_events)
        op.menu("Workshop", lambda: op.command("ai"))
        op.click(31)
        eventually(lambda: op.seen("server action request"))
        op.close()
        start = len(guest.packets)
        op.chat("spawn a pig and say a phrase")
        sessions_path = data / "sessions.json"
        def session():
            if not sessions_path.exists(): return None
            values = json.loads(sessions_path.read_text())["sessions"]
            return values[0] if values and values[0]["thread_id"] else None
        saved = eventually(session)
        first = saved["turns"][0]["job_id"]
        eventually(lambda: json.loads((data / "results" / f"{first}.json").read_text())["state"] == "completed")
        assert not guest.seen("spawn a pig", start), "Private prompt leaked"
        assert guest.seen("KURBIS_COMMAND_OK"), "MCP command did not immediately act on the live server"
        assert not (server_dir / "plugins/server-control.wasm").exists(), "Ask installed a plugin artifact"
        full = [json.loads(line) for line in (data / "events" / f"{first}.jsonl").read_text().splitlines()]
        assert any("server action by" in event["message"] and "spawn_entity" in event["message"] for event in full), "MCP action was not audited"
        assert any(e["kind"] == "stderr" for e in full)
        assert any("END-OF-RESPONSE" in e["message"] for e in full)
        op.menu("Sessions", lambda: op.command("ai sessions"))
        op.menu("Conversation", lambda: op.click(10))
        op.menu("Event log", lambda: op.click(10))
        op.menu("Event log", lambda: op.click(50))
        op.menu("Event details", lambda: op.click(10))
        assert "END-OF-RESPONSE" in (data / "events" / f"{first}.jsonl").read_text()
        controller.terminate()
        controller.wait(timeout=10)
        controller = subprocess.Popen([str(ROOT / "target/debug/kurbis-ai-controller"), "--runtime", str(runtime), "--codex", str(worker)], stdout=controller_log, stderr=subprocess.STDOUT)
        op.menu("Sessions", lambda: op.command("ai sessions"))
        op.menu("Conversation", lambda: op.click(10))
        op.click(51)
        eventually(lambda: op.seen("next prompt"))
        op.close()
        op.chat("what did I ask you to do?")
        eventually(lambda: len(session()["turns"]) == 2)
        saved2 = eventually(session)
        second = saved2["turns"][1]["job_id"]
        eventually(lambda: json.loads((data / "results" / f"{second}.json").read_text())["state"] == "completed")
        assert saved2["thread_id"] == saved["thread_id"]
        full = (data / "events" / f"{second}.jsonl").read_text()
        assert "Resuming Codex session" in full
        op.menu("Settings", lambda: op.command("ai settings"))
        op.menu("Settings", lambda: op.click(33))
        assert settings()["model"] == "" and settings()["timeout_seconds"] == 900
        assert "medium" == settings()["reasoning"]
        # The pinned server does not safely expose spectator inventory packets;
        # close while entering spectator, then reopen the chest after returning.
        server.stdin.write("gamemode spectator KurbisTester\n")
        server.stdin.flush()
        eventually(lambda: op.gamemode == 3)
        op.close()
        server.stdin.write("gamemode survival KurbisTester\n")
        server.stdin.flush()
        eventually(lambda: op.gamemode == 0)
        op.menu("Settings", lambda: op.command("ai settings"))
        op.menu("Settings", lambda: op.click(22))
        assert settings()["reasoning"] == "high"
        op.menu("Settings", lambda: op.click(33))
        assert settings()["reasoning"] == "medium"
        # Settings buttons and pending prompts must recheck permission after de-op.
        op.click(20)
        time.sleep(0.2)
        server.stdin.write("deop KurbisTester\n")
        server.stdin.flush()
        time.sleep(0.5)
        start = len(guest.packets)
        op.chat("revoked-model")
        eventually(lambda: op.seen("Operator access required."))
        assert settings()["model"] == ""
        assert not guest.seen("revoked-model", start)
        op.click(24)
        time.sleep(0.2)
        assert settings()["timeout_seconds"] == 900
        op.close()
        server.stdin.write("op KurbisTester\n")
        server.stdin.flush()
        time.sleep(0.5)
        op.menu("Settings", lambda: op.command("ai settings"))
        op.click(20)
        time.sleep(0.2)
        op.close()
        op.chat("cancel")
        eventually(lambda: op.seen("Prompt cancelled."))
        op.chat("normal-chat-after-cancel")
        eventually(lambda: guest.seen("normal-chat-after-cancel"))
        assert op.gamemode == (3 if gamemode == "spectator" else 0), "Workshop changed gamemode"
        reload_checks.exercise(op, guest, server_dir / "plugins/data/kurbis-ai", eventually)
        print(f"PASS ({gamemode}): official binary, OP gate, menus, private device login and prompts, MCP spawn/command/audit, session resume, settings, live plugin reload and recovery")
    finally:
        for client in clients:
            client.socket.close()
        controller.terminate()
        controller.wait(timeout=10)
        server.stdin.write("stop\n")
        server.stdin.flush()
        try:
            server.wait(timeout=20)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait()
        log.close()
        controller_log.close()


if __name__ == "__main__":
    fixture_temp = tempfile.TemporaryDirectory(prefix="pumpkin-reload-fixtures-")
    artifacts = reload_fixture.build(ROOT, pathlib.Path(fixture_temp.name) / "plugin")
    for gamemode in ("survival", "spectator"):
        # Select an available port without taking over the normal test server.
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        with tempfile.TemporaryDirectory(prefix="pumpkin-kurbis-ai-e2e-") as temp:
            try:
                run(pathlib.Path(temp), port, gamemode, artifacts)
            except Exception:
                for filename in ("server.log", "controller.log"):
                    path = pathlib.Path(temp) / filename
                    if path.exists():
                        print(f"{filename}:\n{path.read_text()[-10000:]}")
                raise
