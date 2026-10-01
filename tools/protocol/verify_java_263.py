#!/usr/bin/env python3
"""Protocol 777 Java client for worker lifecycle verification."""
import argparse
import json
import socket
import struct
import time
from pathlib import Path

PROTOCOL = 777
DIAMOND_ITEM_ID = 1012


def vi(value):
    value &= 0xFFFFFFFF
    encoded = bytearray()
    while True:
        part = value & 0x7F
        value >>= 7
        encoded.append(part | (0x80 if value else 0))
        if not value:
            return bytes(encoded)


def read_vi(data, offset=0):
    value = shift = 0
    while True:
        byte = data[offset]
        offset += 1
        value |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return value, offset
        shift += 7
        if shift >= 35:
            raise ValueError("VarInt exceeds five bytes")


def string(value):
    raw = value.encode()
    return vi(len(raw)) + raw


def read_exact(sock, length):
    result = bytearray()
    while len(result) < length:
        chunk = sock.recv(length - len(result))
        if not chunk:
            raise EOFError("server closed the connection")
        result.extend(chunk)
    return bytes(result)


def read_frame(sock, compression):
    prefix = bytearray()
    while not prefix or prefix[-1] & 0x80:
        prefix.extend(read_exact(sock, 1))
    size, _ = read_vi(prefix)
    frame = read_exact(sock, size)
    if compression is not None:
        uncompressed, offset = read_vi(frame)
        frame = frame[offset:]
        if uncompressed:
            import zlib
            frame = zlib.decompress(frame)
            if len(frame) != uncompressed:
                raise ValueError("compressed packet length mismatch")
    packet, offset = read_vi(frame)
    return packet, frame[offset:]


def send_packet(sock, packet, body=b"", compression=None):
    payload = vi(packet) + body
    if compression is not None:
        if len(payload) >= compression:
            payload = vi(len(payload)) + __import__("zlib").compress(payload)
        else:
            payload = b"\0" + payload
    sock.sendall(vi(len(payload)) + payload)


def unpack_item(body, offset):
    count, offset = read_vi(body, offset)
    if count == 0:
        return None, offset
    item_id, offset = read_vi(body, offset)
    additions, offset = read_vi(body, offset)
    removals, offset = read_vi(body, offset)
    if additions or removals:
        # Verification scenarios give plain items; keep the parsed item while
        # making an unsupported component layout fail with useful evidence.
        raise ValueError(f"unexpected item components (add={additions}, remove={removals})")
    return {"item_id": item_id, "count": count}, offset


class ProtocolClient:
    def __init__(self, args, evidence):
        self.args = args
        self.evidence = evidence
        self.inventory = {}
        self.messages = []
        self.joined = False
        self.transfer_seen = False
        self.packet_counts = {}

    def record(self, event, **data):
        item = {"event": event, "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), **data}
        self.evidence["events"].append(item)
        print(json.dumps(item), flush=True)

    def connect(self, connect_host, connect_port, handshake_host, backend):
        self.joined = False
        sock = socket.create_connection((connect_host, connect_port), timeout=15)
        sock.settimeout(1)
        send_packet(sock, 0, vi(self.args.protocol) + string(handshake_host) + struct.pack(">H", connect_port) + vi(2))
        send_packet(sock, 0, string(self.args.username) + bytes(16))
        state = "login"
        compression = None
        spawned_at = None
        commands_sent = False
        deadline = time.monotonic() + self.args.timeout

        try:
            while time.monotonic() < deadline:
                if (spawned_at and time.monotonic() - spawned_at >= self.args.hold_seconds
                        and (backend or not self.args.transfer_port)):
                    self.assert_expectations()
                    return
                try:
                    packet, body = read_frame(sock, compression)
                except socket.timeout:
                    if (spawned_at and time.monotonic() - spawned_at >= self.args.hold_seconds
                            and (backend or not self.args.transfer_port)):
                        self.assert_expectations()
                        return
                    continue

                except EOFError as error:
                    if self.args.expect_close or self.args.expect_kick:
                        self.record("connection_closed", reason=str(error), joined=self.joined)
                        return
                    raise

                key = f"{state}:{packet}"
                self.packet_counts[key] = self.packet_counts.get(key, 0) + 1

                if state == "login":
                    if packet == 0:
                        reason = body.decode("utf-8", "replace")
                        self.record("login_rejected", reason=reason)
                        if self.args.expect_close or self.args.expect_kick:
                            if self.args.expect_kick and self.args.expect_kick not in reason:
                                raise AssertionError(f"Unexpected kick: {reason}")
                            return
                        raise AssertionError(f"Login rejected: {reason}")
                    if packet == 3:
                        compression, _ = read_vi(body)
                    elif packet == 2:
                        send_packet(sock, 3, compression=compression)
                        send_packet(sock, 0, string("en_us") + b"\x02" + vi(0) + b"\x01\x7f" + vi(1) + b"\x00\x01", compression)
                        state = "config"
                    continue

                if state == "config":
                    if packet == 15:  # Select known packs
                        send_packet(sock, 7, vi(0), compression)
                    elif packet in (4, 5):  # Configuration keep-alive / ping
                        send_packet(sock, packet, body, compression)
                    elif packet == 3:  # Finish configuration
                        send_packet(sock, 3, compression=compression)
                        state = "play"
                    elif packet == 2:
                        # Resource packs are absent in the disposable fixture.
                        raise AssertionError("Unexpected plugin payload during configuration")
                    continue

                if packet == 32:  # Play disconnect
                    reason = body.decode("utf-8", "replace")
                    self.record("kicked", packet=reason)
                    if self.args.expect_kick and self.args.expect_kick in reason:
                        return
                    if self.args.expect_close:
                        return
                    raise AssertionError(f"Unexpected play disconnect: {reason}")

                if packet == 73:  # Player position / teleport
                    teleport, offset = read_vi(body)
                    accepted_position = body[offset:offset + 24] + body[offset + 48:offset + 56]
                    send_packet(sock, 0, vi(teleport) + accepted_position, compression)
                    if not self.joined:
                        self.joined = True
                        spawned_at = time.monotonic()
                        self.record("backend_joined" if backend else "waiting_joined", username=self.args.username)
                        send_packet(sock, 44, compression=compression)  # Player loaded
                        if backend:
                            if not commands_sent:
                                for command in self.args.command:
                                    send_packet(sock, 7, string(command), compression)
                                    self.record("command_sent", command=command)
                                commands_sent = True
                    continue

                if packet == 45:  # Keep alive
                    send_packet(sock, 28, body, compression)
                    continue
                if packet == 11:  # Chunk batch received
                    send_packet(sock, 11, struct.pack(">f", 10.0), compression)
                    continue
                if packet == 18:  # Container content
                    window, offset = read_vi(body)
                    _revision, offset = read_vi(body, offset)
                    count, offset = read_vi(body, offset)
                    for slot in range(count):
                        item, offset = unpack_item(body, offset)
                        self.inventory[slot] = item
                    if offset < len(body):
                        unpack_item(body, offset)  # Carried item
                    continue
                if packet == 20:  # Container slot
                    window, offset = read_vi(body)
                    _revision, offset = read_vi(body, offset)
                    slot = struct.unpack_from(">h", body, offset)[0]
                    offset += 2
                    item, _ = unpack_item(body, offset)
                    if window in (0, 0xFFFFFFFE):
                        self.inventory[slot] = item
                    continue
                if packet == 124:  # System chat
                    text = body.decode("utf-8", "replace")
                    self.messages.append(text)
                    self.record("server_message", text=text)
                    continue
                if packet == 132:  # Transfer
                    host_length, offset = read_vi(body)
                    host = body[offset:offset + host_length].decode("utf-8")
                    offset += host_length
                    port, _ = read_vi(body, offset)
                    if not self.args.transfer_port or backend or self.transfer_seen:
                        raise AssertionError("Unexpected or repeated native transfer")
                    if port != self.args.transfer_port:
                        raise AssertionError(f"Transfer port {port} != {self.args.transfer_port}")
                    if host != self.args.transfer_host:
                        raise AssertionError(f"Transfer host {host} != {self.args.transfer_host}")
                    if not self.joined:
                        raise AssertionError("Transfer arrived before waiting login completed")
                    self.transfer_seen = True
                    self.record("native_transfer", host=host, port=port)
                    sock.close()
                    self.connect(host, port, host, True)
                    return

            raise TimeoutError(
                f"Protocol verification timed out in {state} state; "
                f"received packet counts: {self.packet_counts}"
            )
        finally:
            try:
                sock.close()
            except OSError:
                pass

    def assert_expectations(self):
        for expected in self.args.expect_item:
            name, amount = expected.split(":", 1)
            if name != "diamond":
                raise AssertionError(f"Unknown 26.3 fixture item: {name}")
            actual = sum(item["count"] for item in self.inventory.values()
                         if item and item["item_id"] == DIAMOND_ITEM_ID)
            if actual != int(amount):
                raise AssertionError(f"Inventory diamond count {actual} != {amount}")
            self.record("inventory_verified", name=name, count=actual)
        for expected in self.args.expect_message:
            if not any(expected in message for message in self.messages):
                raise AssertionError(f"Missing server acknowledgment: {expected}")
        for forbidden in self.args.forbid_message:
            if any(forbidden in message for message in self.messages):
                raise AssertionError(f"Unexpected server message: {forbidden}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--edition", required=True, choices=["java"])
    parser.add_argument("--protocol", type=int, choices=[776, 777], default=PROTOCOL)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--address", required=True)
    parser.add_argument("--username", default="RuntimePlayer")
    parser.add_argument("--transfer-port", type=int)
    parser.add_argument("--transfer-host", default="127.0.0.1")
    parser.add_argument("--expect-kick")
    parser.add_argument("--hold-seconds", type=float, default=2)
    parser.add_argument("--expect-close", action="store_true")
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--command", action="append", default=[])
    parser.add_argument("--expect-item", action="append", default=[])
    parser.add_argument("--expect-message", action="append", default=[])
    parser.add_argument("--forbid-message", action="append", default=[])
    parser.add_argument("--direct", action="store_true")
    args = parser.parse_args()
    evidence = {"edition": "java", "address": args.address, "protocol": args.protocol, "events": []}
    try:
        ProtocolClient(args, evidence).connect(args.host, args.port, args.address, args.direct or not args.transfer_port)
        evidence["passed"] = True
    except Exception as error:
        evidence["passed"] = False
        evidence["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(evidence, indent=2) + "\n")


if __name__ == "__main__":
    main()
