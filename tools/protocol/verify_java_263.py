#!/usr/bin/env python3
"""Protocol 777 Java client for worker lifecycle verification."""
import argparse
import json
import re
import socket
import struct
import time
from pathlib import Path

PROTOCOL = 777
DIAMOND_ITEM_ID = 1012
LOGIN_LINK = re.compile(r"(https?://[^\s\"<>]+/auth/server#token=)[A-Za-z0-9_-]{43}")


def redact_login_links(value):
    if isinstance(value, str):
        return LOGIN_LINK.sub(r"\1[redacted]", value)
    if isinstance(value, list):
        return [redact_login_links(item) for item in value]
    if isinstance(value, dict):
        return {key: redact_login_links(item) for key, item in value.items()}
    return value


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


def container_click_payload(sync_id, revision, slot):
    if not 0 <= slot < 54:
        raise ValueError("GUI slot must be between 0 and 53")
    return vi(sync_id) + vi(revision) + struct.pack(">hB", slot, 0) + vi(0) + vi(0) + b"\x00"


def port_range(value):
    try:
        lower, upper = (int(part) for part in value.split("-", 1))
    except (ValueError, TypeError):
        raise argparse.ArgumentTypeError("transfer range must be MIN-MAX")
    if not 1 <= lower <= upper <= 65535:
        raise argparse.ArgumentTypeError("transfer range must be within 1..65535")
    return lower, upper


def transfer_port_allowed(port, exact_port, ranges):
    if exact_port is not None:
        return port == exact_port
    return any(lower <= port <= upper for lower, upper in ranges)


def skip_item_component(data, offset, component_id):
    # The managed Kurbis menu uses custom_data, custom_name, and lore components. Decode
    # those network forms so GUI packets remain verifiable on modern clients;
    # unknown component payloads are rejected rather than guessed.
    if component_id in (0, 6, 9):  # custom_data/custom_name/item_name are Java NBT payloads
        return skip_nbt(data, offset)
    if component_id == 11:  # lore is a VarInt line count followed by NBT text components
        if data[offset] == 9 and len(data) > offset + 1 and data[offset + 1] in (8, 10):
            return skip_nbt(data, offset)
        count, offset = read_vi(data, offset)
        for _ in range(count):
            offset = skip_nbt(data, offset)
        return offset
    if component_id in (1, 2, 3, 12) or 88 <= component_id <= 121:
        # max-stack/damage/max-damage/rarity and registry-backed variants
        # are encoded as VarInts.
        _, offset = read_vi(data, offset)
        return offset
    if component_id == 4:  # unbreakable is a unit component (0 payload bytes)
        return offset
    if component_id == 10:  # item_model is a VarInt-prefixed string
        length, offset = read_vi(data, offset)
        return offset + length
    raise ValueError(f"unsupported added item component id {component_id}")


def skip_nbt(data, offset):
    def payload(tag, cursor):
        if tag == 1:
            return cursor + 1
        if tag == 2:
            return cursor + 2
        if tag in (3, 4, 5, 6):
            return cursor + {3: 4, 4: 8, 5: 4, 6: 8}[tag]
        if tag in (7, 11, 12):
            count = int.from_bytes(data[cursor:cursor + 4], "big", signed=True)
            cursor += 4
            width = {7: 1, 11: 4, 12: 8}[tag]
            return cursor + max(count, 0) * width
        if tag == 8:
            length = int.from_bytes(data[cursor:cursor + 2], "big")
            return cursor + 2 + length
        if tag == 9:
            element = data[cursor]
            count = int.from_bytes(data[cursor + 1:cursor + 5], "big", signed=True)
            cursor += 5
            if element == 0 or count <= 0:
                return cursor
            for _ in range(count):
                cursor = payload(element, cursor)
            return cursor
        if tag == 10:
            while True:
                child = data[cursor]
                cursor += 1
                if child == 0:
                    return cursor
                name_len = int.from_bytes(data[cursor:cursor + 2], "big")
                cursor += 2 + name_len
                cursor = payload(child, cursor)
        raise ValueError(f"unsupported NBT tag {tag}")

    tag = data[offset]
    offset += 1
    if tag == 0:
        return offset
    return payload(tag, offset)


def read_nbt_string(data, offset):
    length = int.from_bytes(data[offset:offset + 2], "big")
    offset += 2
    return data[offset:offset + length].decode("utf-8"), offset + length


def read_nbt_payload(data, offset, tag):
    if tag == 1:
        return int.from_bytes(data[offset:offset + 1], "big", signed=True), offset + 1
    if tag == 2:
        return int.from_bytes(data[offset:offset + 2], "big", signed=True), offset + 2
    if tag in (3, 4):
        width = 4 if tag == 3 else 8
        return int.from_bytes(data[offset:offset + width], "big", signed=True), offset + width
    if tag in (5, 6):
        width = 4 if tag == 5 else 8
        value = struct.unpack_from(">f" if tag == 5 else ">d", data, offset)[0]
        return value, offset + width
    if tag in (7, 11, 12):
        length = int.from_bytes(data[offset:offset + 4], "big", signed=True)
        offset += 4
        width = {7: 1, 11: 4, 12: 8}[tag]
        values = []
        for _ in range(max(length, 0)):
            values.append(int.from_bytes(data[offset:offset + width], "big", signed=(tag == 7)))
            offset += width
        return values, offset
    if tag == 8:
        return read_nbt_string(data, offset)
    if tag == 9:
        element = data[offset]
        length = int.from_bytes(data[offset + 1:offset + 5], "big", signed=True)
        offset += 5
        values = []
        for _ in range(max(length, 0)):
            value, offset = read_nbt_payload(data, offset, element)
            values.append(value)
        return values, offset
    if tag == 10:
        values = {}
        while data[offset] != 0:
            child = data[offset]
            name, offset = read_nbt_string(data, offset + 1)
            values[name], offset = read_nbt_payload(data, offset, child)
        return values, offset + 1
    raise ValueError(f"unsupported NBT tag {tag}")


def read_nbt(data, offset=0):
    tag = data[offset]
    if tag == 0:
        return None, offset + 1
    # Minecraft network NBT omits the root name; named child tags still carry
    # their regular NBT names inside compound payloads.
    return read_nbt_payload(data, offset + 1, tag)


def find_clickable_url(component, url):
    if isinstance(component, dict):
        for key in ("click_event", "clickEvent"):
            click = component.get(key)
            if (component.get("text") == url and isinstance(click, dict)
                    and click.get("action") == "open_url"
                    and click.get("url", click.get("value")) == url):
                return key
        for value in component.values():
            key = find_clickable_url(value, url)
            if key:
                return key
    elif isinstance(component, list):
        for value in component:
            key = find_clickable_url(value, url)
            if key:
                return key
    return None


def unpack_item(body, offset):
    count, offset = read_vi(body, offset)
    if count == 0:
        return None, offset
    item_id, offset = read_vi(body, offset)
    additions, offset = read_vi(body, offset)
    removals, offset = read_vi(body, offset)
    for _ in range(additions):
        component_id, offset = read_vi(body, offset)
        offset = skip_item_component(body, offset, component_id)
    for _ in range(removals):
        _, offset = read_vi(body, offset)
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
        self.gui_open_count = 0
        self.pending_gui_sync_id = None
        self.gui_click_index = 0

    def record(self, event, **data):
        item = {"event": event, "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), **data}
        item = redact_login_links(item)
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
                        self.assert_expectations()
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
                        self.assert_expectations()
                        return
                    if self.args.expect_close:
                        self.assert_expectations()
                        return
                    raise AssertionError(f"Unexpected play disconnect: {reason}")

                if packet == 60:  # Open screen
                    sync_id, _ = read_vi(body)
                    self.gui_open_count += 1
                    self.pending_gui_sync_id = sync_id
                    self.record("gui_opened", sync_id=sync_id)
                    continue

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
                    revision, offset = read_vi(body, offset)
                    count, offset = read_vi(body, offset)
                    for slot in range(count):
                        item, offset = unpack_item(body, offset)
                        self.inventory[slot] = item
                    if offset < len(body):
                        unpack_item(body, offset)  # Carried item
                    if window == self.pending_gui_sync_id:
                        self.pending_gui_sync_id = None
                        if self.gui_click_index < len(self.args.click_slot):
                            slot = self.args.click_slot[self.gui_click_index]
                            click = container_click_payload(window, revision, slot)
                            send_packet(sock, 18, click, compression)
                            self.gui_click_index += 1
                            self.record("gui_slot_clicked", slot=slot, sync_id=window)
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
                    if self.args.capture_login_link:
                        link = re.search(
                            r"https?://[^\s\"<>]+/auth/server#token=[A-Za-z0-9_-]{43}",
                            text,
                        )
                        if link:
                            component, _ = read_nbt(body)
                            click_key = find_clickable_url(component, link.group(0))
                            if click_key is None:
                                raise AssertionError(
                                    "Kurbis login URL was not sent with a matching visible-text open_url click event"
                                )
                            self.record(
                                "login_link_clickable",
                                click_key=click_key,
                                action="open_url",
                                target_matches_visible_url=True,
                            )
                            print(f"KURBIS_LOGIN_LINK={link.group(0)}", flush=True)
                    self.record("server_message", text=text)
                    continue
                if packet == 132:  # Transfer
                    host_length, offset = read_vi(body)
                    host = body[offset:offset + host_length].decode("utf-8")
                    offset += host_length
                    port, _ = read_vi(body, offset)
                    if (self.args.transfer_port is None and not self.args.transfer_port_range) or backend or self.transfer_seen:
                        raise AssertionError("Unexpected or repeated native transfer")
                    if not transfer_port_allowed(
                        port, self.args.transfer_port, self.args.transfer_port_range
                    ):
                        raise AssertionError(f"Transfer port {port} is outside the provisioned ranges")
                    if host != self.args.transfer_host:
                        raise AssertionError(f"Transfer host {host} != {self.args.transfer_host}")
                    if not self.joined:
                        raise AssertionError("Transfer arrived before waiting login completed")
                    self.transfer_seen = True
                    self.record("native_transfer", host=host, port=port)
                    sock.close()
                    connect_host = self.args.transfer_connect_host or host
                    self.connect(connect_host, port, host, True)
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
        if self.args.expect_gui_opens is not None:
            if self.gui_open_count != self.args.expect_gui_opens:
                raise AssertionError(
                    f"GUI opened {self.gui_open_count} times, expected {self.args.expect_gui_opens}"
                )
            if self.gui_click_index != len(self.args.click_slot):
                raise AssertionError(
                    f"Sent {self.gui_click_index} GUI clicks, expected {len(self.args.click_slot)}"
                )
            self.record("gui_verified", open_count=self.gui_open_count, clicks=self.gui_click_index)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--edition", required=True, choices=["java"])
    parser.add_argument("--protocol", type=int, choices=[776, 777], default=PROTOCOL)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--address", required=True)
    parser.add_argument("--username", default="RuntimePlayer")
    parser.add_argument("--transfer-port", type=int)
    parser.add_argument("--transfer-port-range", type=port_range, action="append", default=[])
    parser.add_argument("--transfer-host", default="127.0.0.1")
    parser.add_argument("--transfer-connect-host", help="test-only DNS override; Java handshake retains the transferred hostname")
    parser.add_argument("--expect-kick")
    parser.add_argument("--hold-seconds", type=float, default=2)
    parser.add_argument("--expect-close", action="store_true")
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--command", action="append", default=[])
    parser.add_argument("--capture-login-link", action="store_true")
    parser.add_argument("--expect-item", action="append", default=[])
    parser.add_argument("--expect-message", action="append", default=[])
    parser.add_argument("--forbid-message", action="append", default=[])
    parser.add_argument("--click-slot", action="append", type=int, default=[])
    parser.add_argument("--expect-gui-opens", type=int)
    parser.add_argument("--direct", action="store_true")
    args = parser.parse_args()
    evidence = {"edition": "java", "address": args.address, "protocol": args.protocol, "events": []}
    try:
        ProtocolClient(args, evidence).connect(
            args.host, args.port, args.address,
            args.direct or (args.transfer_port is None and not args.transfer_port_range),
        )
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
