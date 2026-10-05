"""Real Java and Bedrock protocol fixtures for local runtime verification."""

import http.client
import json
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

from verify_host_control import rpc

LOGIN_LINK = re.compile(r"(https?://[^\s\"<>]+/auth/server#token=)[A-Za-z0-9_-]{43}")
MAX_HTTP_RESPONSE_BYTES = 1 << 20


def redact_login_links(value):
    return LOGIN_LINK.sub(r"\1[redacted]", value)


def run(*args, check=True, env=None):
    return subprocess.run(args, text=True, capture_output=True, check=check, env=env)


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


def create_test_tls_certificate(directory):
    certificate = directory / "bedrock-gateway-test.crt"
    private_key = directory / "bedrock-gateway-test.key"
    run(
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
        "-days", "2", "-sha256", "-keyout", str(private_key),
        "-out", str(certificate), "-subj", "/CN=*.mc.example.test",
        "-addext", "basicConstraints=critical,CA:TRUE",
        "-addext", "keyUsage=critical,digitalSignature,keyCertSign,keyEncipherment",
        "-addext", "extendedKeyUsage=serverAuth",
        "-addext", "subjectAltName=DNS:mc.example.test,DNS:*.mc.example.test,IP:127.0.0.1",
    )
    os.chmod(private_key, 0o600)
    return certificate, private_key


def bedrock_https_request(
    certificate,
    port,
    hostname,
    method,
    path,
    body=b"",
    content_type=None,
    *,
    server_name=None,
    host_header=None,
):
    context = ssl.create_default_context(cafile=str(certificate))
    context.set_alpn_protocols(["h2", "http/1.1"])
    raw = socket.create_connection(("127.0.0.1", port), timeout=5)
    with context.wrap_socket(raw, server_hostname=server_name or hostname) as connection:
        connection.settimeout(10)
        assert connection.selected_alpn_protocol() == "http/1.1"
        headers = [
            f"{method} {path} HTTP/1.1",
            f"Host: {host_header or f'{hostname}:{port}'}",
            "Connection: close",
            "Accept: application/json, application/sdp",
            f"Content-Length: {len(body)}",
        ]
        if content_type:
            headers.append(f"Content-Type: {content_type}")
        connection.sendall(("\r\n".join(headers) + "\r\n\r\n").encode("ascii") + body)
        response = http.client.HTTPResponse(connection)
        response.begin()
        payload = response.read(MAX_HTTP_RESPONSE_BYTES + 1)
        assert len(payload) <= MAX_HTTP_RESPONSE_BYTES, "Bedrock signaling response exceeded its cap"
        return response.status, response.headers, payload


def reject_bad_bedrock_tls_name(certificate, port):
    context = ssl.create_default_context(cafile=str(certificate))
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=5) as raw:
            with context.wrap_socket(raw, server_hostname="attacker.example.test"):
                pass
    except ssl.SSLCertVerificationError:
        return
    raise AssertionError("TLS accepted a hostname outside the Bedrock certificate")


def run_java_protocol(
    *,
    protocol_dir,
    output_directory,
    port,
    name,
    options,
    root,
    secret,
    api_port,
    checks,
):
    edition = "java"
    artifact = output_directory / f"protocol-{name}-{edition}.json"
    expect_server_login_link = options.pop("expect_server_login_link", False)
    command = [
        "node", str(protocol_dir / "verify.mjs"), "--edition", edition,
        "--port", str(port), "--address", options.pop("address", "live.mc.example.test"),
        "--output", str(artifact),
    ]
    if expect_server_login_link:
        command.append("--capture-login-link")
    for key, value in options.items():
        option = "--" + key.replace("_", "-")
        if isinstance(value, bool):
            if value:
                command.append(option)
            continue
        for item in value if isinstance(value, list) else [value]:
            command.extend([option, str(item)])
    client_log = output_directory / f"protocol-{name}-{edition}.log"
    client = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
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
    stdout, _ = client.communicate()
    client_log.write_text(redact_login_links(stdout))
    assert client.returncode == 0, (
        f"Real {edition} protocol scenario {name} failed: {redact_login_links(stdout)}; "
        f"waiting players observed: {observed_players}; route snapshots: {route_snapshots}"
    )
    proof = json.loads(artifact.read_text())
    assert proof["passed"] is True
    checks.append({"protocol": name, "edition": edition, "events": proof["events"]})
    if not expect_server_login_link:
        return None

    marker = next(
        (line.partition("=")[2] for line in stdout.splitlines()
         if line.startswith("KURBIS_LOGIN_LINK=")),
        None,
    )
    assert marker is not None, "Kurbis plugin did not return a browser login link"
    parsed = urllib.parse.urlsplit(marker)
    token = urllib.parse.parse_qs(parsed.fragment).get("token", [None])[0]
    assert parsed.path == "/auth/server" and re.fullmatch(r"[A-Za-z0-9_-]{43}", token or ""), (
        "Kurbis login link has an unexpected URL shape"
    )
    return marker


def start_gateway(
    *,
    args,
    native,
    root,
    waiting_java_port,
    waiting_bedrock_port,
    gateway_java_port,
    bedrock_public_port,
    api_port,
    secret,
    bedrock_certificate,
    bedrock_private_key,
    common,
    launch,
    wait_for,
    processes,
    logs,
    waiting_container,
):
    java, bedrock = gateway_java_port, bedrock_public_port
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
motd = "Kurbis waiting fixture"
view_distance = 2
simulation_distance = 2
[networking.bedrock.nethernet]
enabled = true
address = "0.0.0.0:{waiting_bedrock_port}"
signal_address = "0.0.0.0:{waiting_bedrock_port}"
external_ip = "127.0.0.1"
external_port = {bedrock}
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

    wait_for(ready, "actual waiting Pumpkin host-control readiness")
    launch("gateway", {**common, "JAVA_LISTEN": f"0.0.0.0:{java}", "JAVA_PUBLIC_PORT": str(java), "BEDROCK_LISTEN": f"0.0.0.0:{bedrock}",
        "BEDROCK_PUBLIC_IP": "127.0.0.1",
        "BEDROCK_PUBLIC_PORT": str(bedrock),
        "BEDROCK_TLS_CERT_FILE": str(bedrock_certificate),
        "BEDROCK_TLS_KEY_FILE": str(bedrock_private_key),
        "WAITING_JAVA_ADDRESS": f"127.0.0.1:{waiting_java_port}", "WAITING_BEDROCK_ADDRESS": f"127.0.0.1:{waiting_bedrock_port}",
        "WAITING_SIGNAL_ADDRESS": f"127.0.0.1:{waiting_bedrock_port}",
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

    wait_for(listening, "real gateway listener")

    def bedrock_tls_ready():
        try:
            status, _, _ = bedrock_https_request(
                bedrock_certificate, bedrock, "live.mc.example.test", "GET", "/v1/join"
            )
            return status == 200
        except (OSError, ssl.SSLError, AssertionError):
            return False

    wait_for(bedrock_tls_ready, "trusted Bedrock TLS signaling listener")
    return java, bedrock, bedrock_certificate


def verify_bedrock_signal(port, certificate, server, evidence):
    waiting_status, waiting_headers, waiting_body = bedrock_https_request(
        certificate, port, "live.mc.example.test", "GET", "/v1/join"
    )
    waiting = json.loads(waiting_body)
    assert waiting_status == 200 and waiting_headers.get_content_type() == "application/json"
    assert waiting["name"] == "Kurbis waiting fixture"
    assert isinstance(waiting.get("protocol"), int) and waiting["protocol"] > 0

    alias = f"s-{server}.mc.example.test"
    backend_status, backend_headers, backend_body = bedrock_https_request(
        certificate, port, alias, "GET", "/v1/join"
    )
    backend = json.loads(backend_body)
    assert backend_status == 200 and backend_headers.get_content_type() == "application/json"
    assert isinstance(backend.get("name"), str) and backend.get("name")

    bad_sdp, _, _ = bedrock_https_request(
        certificate, port, alias, "POST", "/v1/join/runtime-invalid-sdp",
        b"not an SDP offer", "application/sdp",
    )
    assert bad_sdp == 400, f"malformed SDP was not rejected: HTTP {bad_sdp}"
    no_content_type, _, _ = bedrock_https_request(
        certificate, port, alias, "POST", "/v1/join/runtime-invalid-content-type", b"offer"
    )
    empty_offer, _, _ = bedrock_https_request(
        certificate, port, alias, "POST", "/v1/join/runtime-empty-offer", b"", "application/sdp"
    )
    oversized, _, _ = bedrock_https_request(
        certificate, port, alias, "POST", "/v1/join/runtime-large-offer",
        b"x" * (MAX_HTTP_RESPONSE_BYTES + 1), "application/sdp",
    )
    query, _, _ = bedrock_https_request(
        certificate, port, "live.mc.example.test", "GET", "/v1/join?unexpected=1"
    )
    escaped_id, _, _ = bedrock_https_request(
        certificate, port, alias, "POST", "/v1/join/runtime%2Fescape", b"offer", "application/sdp"
    )
    wrong_port, _, _ = bedrock_https_request(
        certificate, port, "live.mc.example.test", "GET", "/v1/join",
        host_header=f"live.mc.example.test:{port + 1}",
    )
    mismatched_sni, _, _ = bedrock_https_request(
        certificate, port, "live.mc.example.test", "GET", "/v1/join",
        server_name="live.mc.example.test", host_header=f"{alias}:{port}",
    )
    unsupported, _, _ = bedrock_https_request(
        certificate, port, "live.mc.example.test", "DELETE", "/v1/join"
    )
    unknown_alias = f"s-{uuid.uuid4()}.mc.example.test"
    unavailable, _, _ = bedrock_https_request(
        certificate, port, unknown_alias, "GET", "/v1/join"
    )
    assert (no_content_type, empty_offer, oversized, query, escaped_id, wrong_port,
            mismatched_sni, unsupported, unavailable) == (400, 400, 413, 400, 404, 400, 421, 405, 503)
    reject_bad_bedrock_tls_name(certificate, port)
    evidence["checks"].append({
        "bedrock_tls": {
            "valid_waiting_status": waiting_status,
            "valid_assigned_server_status": backend_status,
            "malformed_sdp_status": bad_sdp,
            "invalid_content_type_status": no_content_type,
            "empty_offer_status": empty_offer,
            "oversized_offer_status": oversized,
            "query_status": query,
            "escaped_network_id_status": escaped_id,
            "wrong_port_status": wrong_port,
            "mismatched_sni_status": mismatched_sni,
            "unsupported_method_status": unsupported,
            "unassigned_alias_status": unavailable,
            "untrusted_certificate_name_rejected": True,
        }
    })

