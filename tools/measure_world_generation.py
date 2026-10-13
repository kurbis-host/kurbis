#!/usr/bin/env python3
"""Measure how fast a Pumpkin server generates and streams new terrain.

Starts Pumpkin in a fresh world, joins with the offline Java protocol client
and asks for a full view of chunks. Once the spawn area has streamed, the
console teleports the player into untouched terrain a few times. Every phase
reports how many chunks arrived and how long they took, which is the wait a
player sees while exploring.

Run a Pumpkin image the way a worker does, under the CPU quota a production
server gets, and compare it with more CPUs:

    python3 tools/measure_world_generation.py --image kurbis-pumpkin:local \\
        --cpus 1 --cpus 4

or a native build with no quota:

    python3 tools/measure_world_generation.py \\
        --binary output/native-pumpkin/target/release/pumpkin
"""
import argparse
import json
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS / "protocol"))

from runtime_verification_protocol import free_port  # noqa: E402
from verify_java_263 import PROTOCOL, read_frame, read_vi, send_packet, string, vi  # noqa: E402

USERNAME = "WorldGenProbe"
CONFIG = """default_difficulty = "Normal"
default_gamemode = "Creative"
seed = "{seed}"
[networking.query]
enabled = false
[networking.rcon]
enabled = false
[networking.java]
enabled = true
online_mode = false
encryption = false
address = "0.0.0.0:25565"
max_players = 0
view_distance = {view_distance}
simulation_distance = 8
[networking.bedrock]
enabled = false
"""
# Login, configuration and play packet ids of protocol 777.
LOGIN_DISCONNECT, LOGIN_SUCCESS, LOGIN_COMPRESSION = 0, 2, 3
CONFIG_FINISH, CONFIG_KEEP_ALIVE, CONFIG_PING, CONFIG_KNOWN_PACKS = 3, 4, 5, 15
PLAY_CHUNK_BATCH_FINISHED, PLAY_DISCONNECT, PLAY_KEEP_ALIVE, PLAY_POSITION = 11, 32, 45, 73


class Server:
    """One Pumpkin process in a throwaway world, natively or in Docker."""

    def __init__(self, args, cpus, directory):
        self.port = free_port()
        directory.chmod(0o777)  # The image runs Pumpkin as its own user.
        (directory / "pumpkin.toml").write_text(
            CONFIG.format(seed=args.seed, view_distance=args.view_distance))
        if args.image:
            command = ["docker", "run", "--rm", "-i", "--name", f"kurbis-worldgen-{self.port}",
                       "--cpu-period", "100000", "--cpu-quota", str(int(cpus * 100000)),
                       "--memory", "2g", "--memory-swap", "2g",
                       "--publish", f"127.0.0.1:{self.port}:25565",
                       "--mount", f"type=bind,source={directory},target=/pumpkin",
                       "--workdir", "/pumpkin", "--entrypoint", "/usr/local/bin/pumpkin", args.image]
        else:
            config = directory / "pumpkin.toml"
            config.write_text(config.read_text().replace("0.0.0.0:25565", f"127.0.0.1:{self.port}"))
            command = [str(args.binary)]
        self.log = (directory / "server.log").open("wb")
        self.process = subprocess.Popen(command, cwd=directory, stdin=subprocess.PIPE,
                                        stdout=self.log, stderr=subprocess.STDOUT)
        self.directory = directory

    def console(self, command):
        self.process.stdin.write(command.encode() + b"\n")
        self.process.stdin.flush()

    def wait_listening(self, timeout):
        """Seconds until Pumpkin logs that it is running (its spawn area is ready)."""
        started = time.monotonic()
        while time.monotonic() - started < timeout:
            if self.process.poll() is not None:
                break
            if b"is now running" in (self.directory / "server.log").read_bytes():
                return time.monotonic() - started
            time.sleep(0.05)
        raise RuntimeError("Pumpkin did not start:\n" + (self.directory / "server.log").read_text()[-4000:])

    def stop(self):
        try:
            self.console("stop")
            self.process.wait(timeout=60)
        except (OSError, subprocess.TimeoutExpired):
            self.process.kill()
            self.process.wait()
        self.log.close()


def explore(server, args):
    """Join, then teleport into new terrain; return one record per phase."""
    sock = socket.create_connection(("127.0.0.1", server.port), timeout=15)
    sock.settimeout(0.25)
    send_packet(sock, 0, vi(PROTOCOL) + string("localhost") + struct.pack(">H", server.port) + vi(2))
    send_packet(sock, 0, string(USERNAME) + bytes(16))
    state, compression = "login", None
    targets = [(4096 * (index + 1), 4096 * (index % 2)) for index in range(args.teleports)]
    phases, phase, chunks, started, last_chunk = [], "spawn", 0, None, None
    deadline = time.monotonic() + args.timeout
    try:
        while time.monotonic() < deadline:
            if last_chunk and time.monotonic() - last_chunk >= args.settle:
                elapsed = last_chunk - started
                phases.append({"phase": phase, "chunks": chunks, "seconds": round(elapsed, 2),
                               "chunks_per_second": round(chunks / elapsed, 1)})
                if not targets:
                    return phases
                x, z = targets.pop(0)
                phase, chunks, last_chunk, started = f"teleport {x} {z}", 0, None, time.monotonic()
                server.console(f"tp {USERNAME} {x} 200 {z}")
            try:
                packet, body = read_frame(sock, compression)
            except socket.timeout:
                continue
            if state == "login":
                if packet == LOGIN_DISCONNECT:
                    raise RuntimeError("login rejected: " + body.decode("utf-8", "replace"))
                if packet == LOGIN_COMPRESSION:
                    compression, _ = read_vi(body)
                elif packet == LOGIN_SUCCESS:
                    send_packet(sock, 3, compression=compression)
                    # Client information: the view distance a real client asks for.
                    send_packet(sock, 0, string("en_us") + bytes([args.view_distance]) + vi(0)
                                + b"\x01\x7f" + vi(1) + b"\x00\x01", compression)
                    state = "config"
            elif state == "config":
                if packet == CONFIG_KNOWN_PACKS:
                    send_packet(sock, 7, vi(0), compression)
                elif packet in (CONFIG_KEEP_ALIVE, CONFIG_PING):
                    send_packet(sock, packet, body, compression)
                elif packet == CONFIG_FINISH:
                    send_packet(sock, 3, compression=compression)
                    state, started = "play", time.monotonic()
                    server.console(f"op {USERNAME}")
            elif packet == PLAY_DISCONNECT:
                raise RuntimeError("kicked: " + body.decode("utf-8", "replace"))
            elif packet == PLAY_POSITION:
                teleport, offset = read_vi(body)
                send_packet(sock, 0, vi(teleport) + body[offset:offset + 24] + body[offset + 48:offset + 56],
                            compression)
                send_packet(sock, 44, compression=compression)  # Player loaded
            elif packet == PLAY_KEEP_ALIVE:
                send_packet(sock, 28, body, compression)
            elif packet == PLAY_CHUNK_BATCH_FINISHED:
                count, _ = read_vi(body)
                if count:
                    chunks += count
                    last_chunk = time.monotonic()
                # A vanilla client asks for at most 64 chunks per tick.
                send_packet(sock, 11, struct.pack(">f", 64.0), compression)
        raise TimeoutError(f"exploration timed out in phase {phase!r} after {chunks} chunks")
    finally:
        sock.close()


def measure(args, cpus):
    directory = Path(tempfile.mkdtemp(prefix="kurbis-worldgen-"))
    server = Server(args, cpus, directory)
    try:
        boot = server.wait_listening(180)
        phases = explore(server, args)
    finally:
        server.stop()
        shutil.rmtree(directory, ignore_errors=True)
    explored = phases[1:] or phases
    total_chunks = sum(phase["chunks"] for phase in explored)
    total_seconds = sum(phase["seconds"] for phase in explored)
    return {"cpus": cpus, "boot_seconds": round(boot, 2), "phases": phases,
            "new_terrain_chunks_per_second": round(total_chunks / total_seconds, 1)}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--image", help="a Kurbis Pumpkin image, run under a Docker CPU quota")
    target.add_argument("--binary", type=Path, help="a native Pumpkin binary, run without a quota")
    parser.add_argument("--cpus", type=float, action="append",
                        help="Docker CPU quota to measure; repeat to compare (default 1, a production server)")
    parser.add_argument("--view-distance", type=int, default=12, help="Kurbis' default view distance")
    parser.add_argument("--teleports", type=int, default=3)
    parser.add_argument("--settle", type=float, default=4, help="seconds without chunks that end a phase")
    parser.add_argument("--seed", default="kurbis-worldgen")
    parser.add_argument("--timeout", type=float, default=900)
    args = parser.parse_args()
    if args.binary:
        args.binary = args.binary.resolve(strict=True)
        if args.cpus:
            parser.error("--cpus needs --image; a native binary runs without a quota")
    for cpus in args.cpus or ([1.0] if args.image else [None]):
        print(json.dumps(measure(args, cpus)), flush=True)


if __name__ == "__main__":
    main()
