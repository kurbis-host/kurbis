#!/usr/bin/env python3
"""Run the official Pumpkin binary and serialize confirmed plugin reloads."""
import argparse
import collections
import json
import os
import pathlib
import re
import signal
import subprocess
import sys
import threading
import time
import uuid

ANSI = re.compile(r"\x1b\[[0-9;]*m")
NAME = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?\Z")


def atomic_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value))
    temporary.replace(path)


class Console:
    def __init__(self, process):
        self.process = process
        self.condition = threading.Condition()
        self.lines = collections.deque(maxlen=4096)
        self.sequence = 0
        self.input_lock = threading.Lock()
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in self.process.stdout:
            print(line, end="", flush=True)
            with self.condition:
                self.sequence += 1
                self.lines.append((self.sequence, ANSI.sub("", line).strip()))
                self.condition.notify_all()

    def send(self, command):
        with self.input_lock:
            self.process.stdin.write(command + "\n")
            self.process.stdin.flush()

    def execute(self, command, success, failures, transcript):
        with self.condition:
            cursor = self.sequence
        transcript.append("console: " + command)
        self.send(command)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            with self.condition:
                for sequence, line in self.lines:
                    if sequence <= cursor:
                        continue
                    cursor = sequence
                    transcript.append(line)
                    if any(marker in line for marker in failures):
                        raise RuntimeError(line)
                    if any(marker in line for marker in success):
                        return
                if self.process.poll() is not None:
                    raise RuntimeError("Pumpkin exited during reload")
                self.condition.wait(timeout=0.2)
        raise RuntimeError("Timed out waiting for Pumpkin command confirmation: " + command)

    def reload(self, plugin, artifact, transcript):
        self.execute(
            "plugin unload " + plugin,
            [f"Plugin {plugin} unloaded successfully", f"Plugin {plugin} is not loaded"],
            [f"Failed to unload plugin {plugin}:"], transcript,
        )
        # The release's SingleWord command parser accepts filenames but not '/'.
        # A private hard link gives the official loader a legal filename without
        # copying bytes or changing the installed artifact's atomicity.
        alias = artifact.parent.parent / (".kurbis-ai-load-" + uuid.uuid4().hex + ".wasm")
        os.link(artifact, alias)
        try:
            self.execute(
                "plugin load " + alias.name,
                [f"Plugin {alias.name} loaded successfully"],
                [f"Failed to load plugin {alias.name}:", f"Failed to initialize plugin {plugin}:"], transcript,
            )
        finally:
            alias.unlink(missing_ok=True)


def run(runtime, binary):
    data = runtime / "server/plugins/data/kurbis-ai"
    requests = data / "activation/requests"
    results = data / "activation/results"
    ready = data / "activation/ready.json"
    requests.mkdir(parents=True, exist_ok=True)
    results.mkdir(parents=True, exist_ok=True)
    process = subprocess.Popen(
        [str(binary)], cwd=runtime / "server", stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1,
    )
    console = Console(process)
    stopping = threading.Event()

    def stop(*_):
        stopping.set()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    def forward_input():
        for line in sys.stdin:
            if process.poll() is None:
                console.send(line.rstrip("\n"))

    threading.Thread(target=forward_input, daemon=True).start()
    try:
        while process.poll() is None and not stopping.is_set():
            atomic_json(ready, {"pid": os.getpid(), "updated_unix_ms": int(time.time() * 1000)})
            for path in sorted(requests.glob("*.json")):
                transcript = []
                try:
                    request = json.loads(path.read_text())
                    plugin = request["plugin"]
                    if not isinstance(plugin, str) or not NAME.fullmatch(plugin) or "--" in plugin or plugin == "kurbis-ai":
                        raise ValueError("Invalid or protected plugin name")
                    artifact = runtime / "server/plugins" / (plugin + ".wasm")
                    if not artifact.is_file() or artifact.is_symlink():
                        raise ValueError("Installed plugin artifact not found")
                    console.reload(plugin, artifact, transcript)
                    result = {"ok": True, "message": f"{plugin} reloaded without restarting Pumpkin"}
                except Exception as error:
                    result = {"ok": False, "message": str(error)}
                result["events"] = transcript
                atomic_json(results / path.name, result)
                path.unlink(missing_ok=True)
            stopping.wait(0.1)
    finally:
        ready.unlink(missing_ok=True)
        if process.poll() is None:
            console.send("stop")
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    return process.returncode


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime", type=pathlib.Path, required=True)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    args = parser.parse_args()
    sys.exit(run(args.runtime.resolve(), args.binary.resolve()))
