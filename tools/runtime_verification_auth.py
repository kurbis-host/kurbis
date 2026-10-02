"""Disposable OAuth provider used by local runtime integration verification."""

import http.server
import json
import threading
import urllib.error
import urllib.parse
import urllib.request


class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        form = urllib.parse.parse_qs(
            self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode()
        )
        token = form.get("code", [""])[0]
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"access_token": f"runtime-provider-token-{token}"}).encode())

    def do_GET(self):
        token = self.headers.get("Authorization", "").removeprefix("Bearer runtime-provider-token-")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"sub": token, "name": "Runtime Test"}).encode())

    def log_message(self, *args):
        pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


def start_provider(host, port):
    service = http.server.ThreadingHTTPServer((host, port), Provider)
    threading.Thread(target=service.serve_forever, daemon=True).start()
    return service


def create_user_session(api_origin, subject):
    client = urllib.request.build_opener(NoRedirect)
    try:
        client.open(f"{api_origin}/auth/google/login")
    except urllib.error.HTTPError as redirect:
        assert redirect.code == 307
        state_cookie = redirect.headers["Set-Cookie"].split(";", 1)[0]
        state_value = urllib.parse.parse_qs(
            urllib.parse.urlsplit(redirect.headers["Location"]).query
        )["state"][0]
    request = urllib.request.Request(
        f"{api_origin}/auth/google/callback?code={subject}&state={state_value}",
        headers={"Cookie": state_cookie},
    )
    try:
        client.open(request)
    except urllib.error.HTTPError as redirect:
        assert redirect.code == 303
        cookie = next(
            value.split(";", 1)[0]
            for value in redirect.headers.get_all("Set-Cookie")
            if value.startswith("kurbis_session=")
        )
        session_request = urllib.request.Request(
            f"{api_origin}/api/v1/session", headers={"Cookie": cookie}
        )
        with urllib.request.urlopen(session_request, timeout=15) as response:
            actual_user = json.load(response)["user"]["id"]
        assert actual_user == subject, f"OAuth session resolved to {actual_user}, expected {subject}"
        return cookie
    raise RuntimeError("OAuth callback did not return a session")

