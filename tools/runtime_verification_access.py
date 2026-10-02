"""Account and persistent server-grant API checks for the runtime verifier."""

import json
import io
import re
import urllib.error
import urllib.parse
import urllib.request
import uuid
import zipfile

MAX_RUNTIME_EXPORT_BYTES = 64 * 1024 * 1024


class AccessVerifier:
    def __init__(
        self,
        *,
        api_origin,
        command_path,
        canonical_commands,
        sql,
        checks,
        root,
        worker_ids,
        server_state,
        wait_for,
        api_document,
    ):
        self.api_origin = api_origin
        self.command_path = command_path
        self.canonical_commands = canonical_commands
        self.sql = sql
        self.checks = checks
        self.root = root
        self.worker_ids = worker_ids
        self.server_state = server_state
        self.wait_for = wait_for
        self.api_document = api_document

    def issue_command(self, cookie, action, server_id, actor):
        body = self.canonical_commands[action]
        placeholder = re.search(r"\{([^{}]+)\}", self.command_path)
        assert placeholder is not None
        endpoint = self.command_path.replace(placeholder.group(0), server_id)
        request = urllib.request.Request(
            f"{self.api_origin}{endpoint}",
            data=json.dumps(body).encode(),
            method="POST",
            headers={"Cookie": cookie, "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=15) as response:
            payload = json.load(response)
            assert response.status == 202
            assert isinstance(payload.get("observed_state"), str)
            self.checks.append({
                "command_actor": actor,
                "command": action,
                "server": server_id,
                "path": self.command_path,
                "status": response.status,
            })
            return payload

    def user_action(self, cookie, action, server_id):
        return self.issue_command(cookie, action, server_id, "account_owner")

    def scoped_command(self, cookie, action, server_id):
        return self.issue_command(cookie, action, server_id, "server_session")

    def mint_server_login_link(self, server_id):
        placement_worker = self.sql(
            f"SELECT worker_id FROM placements WHERE server_id = '{server_id}'"
        )
        index = self.worker_ids.index(placement_worker)
        config_path = (
            self.root / f"worker-{index}/servers" / server_id /
            "plugins/data/kurbis-control/config.json"
        )
        config = json.loads(config_path.read_text())
        capability = config.get("capability", "")
        assert config.get("server_id") == server_id
        assert re.fullmatch(r"[A-Za-z0-9_-]{43}", capability)
        request = urllib.request.Request(
            f"{self.api_origin}/api/v1/server/login-links",
            method="POST",
            headers={"Authorization": f"Bearer {capability}"},
        )
        with urllib.request.urlopen(request, timeout=15) as response:
            payload = json.load(response)
            assert response.status == 200
        assert payload.get("expires_in_seconds") == 900
        return payload["url"]

    def verify_world_export(self, account_cookie, server_id, expected_player_id):
        paths = self.api_document.get("paths", {})
        export_path = f"/api/v1/servers/{{server_id}}/exports"
        download_path = f"{export_path}/{{export_id}}/download"
        assert "post" in paths.get(export_path, {}), "generated API is missing world-export creation"
        assert "get" in paths.get(download_path, {}), "generated API is missing world-export download"
        endpoint = f"{self.api_origin}{export_path.replace('{server_id}', server_id)}"
        request = urllib.request.Request(
            endpoint,
            method="POST",
            headers={"Cookie": account_cookie},
        )
        with urllib.request.urlopen(request, timeout=30) as response:
            assert response.status == 202
            export = json.load(response)
        export_id = export.get("id")
        assert isinstance(export_id, str) and export.get("status") in ("pending", "running")

        def completed():
            row = self.sql(
                "SELECT json_build_object('status', status, 'stage', stage, "
                "'archive_key', archive_key, 'archive_size', archive_size, "
                "'archive_checksum', archive_checksum, 'last_error', last_error)::text "
                f"FROM world_exports WHERE id='{export_id}' AND server_id='{server_id}'"
            )
            export_state = json.loads(row)
            if export_state["status"] in ("failed", "cancelled"):
                raise RuntimeError(
                    f"world export {export_id} ended in {export_state['status']}: "
                    f"{export_state['last_error']}"
                )
            return export_state if export_state["status"] == "completed" else False

        result = self.wait_for(completed, "worker-generated world export completes", 180)
        archive_key = result["archive_key"]
        archive_size = result["archive_size"]
        archive_checksum = result["archive_checksum"]
        assert archive_key.startswith(f"exports/{export_id}/")
        assert int(archive_size) > 0
        assert re.fullmatch(r"[0-9a-f]{64}", archive_checksum)

        download_endpoint = (
            f"{self.api_origin}{download_path.replace('{server_id}', server_id).replace('{export_id}', export_id)}"
        )
        download = urllib.request.Request(
            download_endpoint,
            headers={"Cookie": account_cookie},
        )
        with urllib.request.urlopen(download, timeout=60) as response:
            assert response.status == 200
            assert response.headers.get_content_type() == "application/zip"
            archive_bytes = response.read(MAX_RUNTIME_EXPORT_BYTES + 1)
        assert len(archive_bytes) <= MAX_RUNTIME_EXPORT_BYTES, (
            "runtime fixture export exceeded its bounded download size"
        )
        assert len(archive_bytes) == int(archive_size)
        with zipfile.ZipFile(io.BytesIO(archive_bytes)) as archive:
            assert archive.testzip() is None, "world export ZIP failed CRC validation"
            entries = set(archive.namelist())
            assert "level.dat" in entries
            player_entry = f"playerdata/{expected_player_id}.dat"
            assert player_entry in entries, f"world export omitted {player_entry}"
            player_bytes = archive.read(player_entry)
            assert player_bytes, "world export player data is empty"
        self.checks.append({
            "world_export": {
                "id": export_id,
                "status": "completed",
                "download_http_status": 200,
                "archive_bytes": len(archive_bytes),
                "archive_checksum_bytes": len(archive_checksum),
                "zip_crc_valid": True,
                "retained_level_metadata": True,
                "retained_player_data": expected_player_id,
            }
        })

    def verify_server_login(
        self,
        link,
        second_link,
        account_cookie,
        server,
        second_server,
    ):
        origin = self.api_origin

        def link_token_and_ttl(url, server_id):
            parsed = urllib.parse.urlsplit(url)
            assert f"{parsed.scheme}://{parsed.netloc}" == origin
            token = urllib.parse.parse_qs(parsed.fragment).get("token", [None])[0]
            assert re.fullmatch(r"[A-Za-z0-9_-]{43}", token or "")
            ttl = self.sql(
                f"SELECT round(extract(epoch FROM (expires_at - created_at)))::integer "
                f"FROM server_login_tokens WHERE server_id='{server_id}' "
                "ORDER BY created_at DESC LIMIT 1"
            )
            assert ttl == "900", f"server login link TTL was {ttl!r}, expected 900 seconds"
            return token, int(ttl)

        first_token, first_ttl = link_token_and_ttl(link, server)
        second_token, second_ttl = link_token_and_ttl(second_link, second_server)

        def redeem(token, request_origin, scoped_cookie=None):
            headers = {"Origin": request_origin, "Content-Type": "application/json"}
            if scoped_cookie:
                headers["Cookie"] = scoped_cookie
            request = urllib.request.Request(
                f"{origin}/auth/server/redeem",
                data=json.dumps({"token": token}).encode(),
                method="POST",
                headers=headers,
            )
            try:
                with urllib.request.urlopen(request, timeout=15) as response:
                    return response.status, json.load(response), response.headers.get("Set-Cookie", "")
            except urllib.error.HTTPError as error:
                return error.code, None, ""

        cross_origin_status, _, _ = redeem(first_token, "https://attacker.example")
        assert cross_origin_status == 403
        status, payload, set_cookie = redeem(first_token, origin)
        assert status == 200 and payload.get("server_id") == server
        cookie = set_cookie.split(";", 1)[0]
        assert cookie.startswith("kurbis_server_session=")
        assert "HttpOnly" in set_cookie and "Secure" in set_cookie and "SameSite=Lax" in set_cookie

        def scoped_status(path, scoped_cookie=cookie):
            request = urllib.request.Request(
                f"{origin}{path}", headers={"Cookie": scoped_cookie}
            )
            try:
                with urllib.request.urlopen(request, timeout=15) as response:
                    return response.status
            except urllib.error.HTTPError as error:
                return error.code

        assert scoped_status(f"/api/v1/servers/{server}/players") == 200
        cross_server_status = scoped_status(
            f"/api/v1/servers/{uuid.uuid4()}/players"
        )
        assert cross_server_status in (401, 404)
        second_status, second_payload, second_set_cookie = redeem(
            second_token, origin, cookie
        )
        assert second_status == 200 and second_payload.get("server_id") == second_server
        assert second_set_cookie.split(";", 1)[0] == cookie
        first_replay_status, _, _ = redeem(first_token, origin, cookie)
        second_replay_status, _, _ = redeem(second_token, origin, cookie)
        assert first_replay_status == second_replay_status == 401

        expiry_columns = self.sql(
            "SELECT count(*) FROM information_schema.columns "
            "WHERE table_schema = current_schema() "
            "AND table_name IN ('server_sessions', 'server_session_grants') "
            "AND (column_name ILIKE '%expir%' OR column_name ILIKE '%valid_until%')"
        )
        assert expiry_columns == "0", "server-scoped grants must not have a time expiry"
        list_request = urllib.request.Request(
            f"{origin}/api/v1/server/sessions", headers={"Cookie": cookie}
        )
        with urllib.request.urlopen(list_request, timeout=15) as response:
            sessions = json.load(response)
        sessions_by_server = {item.get("server_id"): item for item in sessions}
        assert set(sessions_by_server) == {server, second_server}
        assert all(isinstance(item.get("id"), str) for item in sessions_by_server.values())
        grant_id = self.sql(
            f"SELECT grant_id FROM server_sessions WHERE server_id='{server}'"
        )
        self.sql(
            "UPDATE server_session_grants SET created_at=now()-interval '50 years' "
            f"WHERE id='{grant_id}'; "
            "UPDATE server_sessions SET created_at=now()-interval '50 years' "
            f"WHERE grant_id='{grant_id}'"
        )
        oldest_grant_age_years = self.sql(
            f"SELECT min(extract(year FROM age(now(), created_at)))::integer "
            "FROM ("
            f"SELECT created_at FROM server_session_grants WHERE id='{grant_id}' UNION ALL "
            f"SELECT created_at FROM server_sessions WHERE grant_id='{grant_id}'"
            ") aged_grants"
        )
        assert int(oldest_grant_age_years) >= 49
        assert scoped_status(f"/api/v1/servers/{server}/players") == 200
        assert scoped_status(f"/api/v1/servers/{second_server}/players") == 200
        assert scoped_status("/api/v1/account") == 401

        def server_ids(headers):
            request = urllib.request.Request(f"{origin}/api/v1/servers", headers=headers)
            with urllib.request.urlopen(request, timeout=15) as response:
                return {item["id"] for item in json.load(response)}

        account_servers = server_ids({"Cookie": account_cookie})
        assert account_servers == {server}
        union_servers = server_ids({"Cookie": f"{account_cookie}; {cookie}"})
        assert union_servers == {server, second_server}

        def revoke(session_id):
            request = urllib.request.Request(
                f"{origin}/api/v1/server/sessions/{session_id}",
                method="DELETE",
                headers={"Cookie": cookie},
            )
            with urllib.request.urlopen(request, timeout=15) as response:
                assert response.status == 204

        revoke(sessions_by_server[server]["id"])
        first_scope_revoked_status = scoped_status(f"/api/v1/servers/{server}/players")
        retained_scope_status = scoped_status(f"/api/v1/servers/{second_server}/players")
        assert first_scope_revoked_status in (401, 404)
        assert retained_scope_status == 200
        self.scoped_command(cookie, "stop", second_server)
        self.wait_for(
            lambda: self.server_state(second_server) == "sleeping",
            "scoped grant stops its retained server",
        )
        self.scoped_command(cookie, "start", second_server)
        self.wait_for(
            lambda: self.server_state(second_server) == "online",
            "scoped grant starts its retained server after another scope is revoked",
        )
        self.scoped_command(cookie, "reset_world", second_server)
        self.wait_for(
            lambda: self.server_state(second_server) == "online"
            and self.sql(f"SELECT desired_state FROM servers WHERE id='{second_server}'") == "running",
            "scoped grant resets its bound server world and returns it online",
        )
        reset_snapshots = self.sql(
            "SELECT count(*) FROM snapshots sn JOIN servers s ON s.id=sn.server_id "
            f"WHERE sn.server_id='{second_server}' AND sn.complete AND sn.reason='reset' "
            "AND sn.generation < s.current_snapshot_generation"
        )
        assert int(reset_snapshots) >= 1, "scoped reset did not preserve a pre-reset recovery snapshot"
        current_reset_snapshot = self.sql(
            "SELECT count(*) FROM snapshots sn JOIN servers s ON s.id=sn.server_id "
            f"WHERE sn.server_id='{second_server}' AND sn.complete AND sn.reason='reset' "
            "AND sn.generation=s.current_snapshot_generation"
        )
        assert current_reset_snapshot == "1", "scoped reset did not publish a replacement snapshot"
        revoke(sessions_by_server[second_server]["id"])
        final_retained_scope_status = scoped_status(
            f"/api/v1/servers/{second_server}/players"
        )
        assert final_retained_scope_status == 401
        assert scoped_status("/api/v1/account") == 401

        delete_link = self.mint_server_login_link(second_server)
        delete_token, delete_ttl = link_token_and_ttl(delete_link, second_server)
        delete_status, delete_payload, delete_set_cookie = redeem(delete_token, origin)
        assert delete_status == 200 and delete_payload.get("server_id") == second_server
        delete_cookie = delete_set_cookie.split(";", 1)[0]
        assert scoped_status(
            f"/api/v1/servers/{second_server}/players", delete_cookie
        ) == 200
        self.scoped_command(delete_cookie, "delete", second_server)
        self.wait_for(
            lambda: self.sql(
                f"SELECT count(*) FROM servers WHERE id='{second_server}'"
            ) == "0",
            "scoped grant deletes only its bound server",
            180,
        )
        deleted_server_status = scoped_status(
            f"/api/v1/servers/{second_server}/players", delete_cookie
        )
        assert deleted_server_status in (401, 404)
        self.checks.append({
            "kurbis_link_redemption": "one-use",
            "server_login_link_ttl_seconds": [first_ttl, second_ttl, delete_ttl],
            "cross_origin_status": cross_origin_status,
            "scoped_grant_scopes": sorted((server, second_server)),
            "grant_record_age_years": int(oldest_grant_age_years),
            "account_server_scope_union": sorted(union_servers),
            "account_api_from_scoped_cookie_status": 401,
            "cross_server_scoped_status": cross_server_status,
            "replay_statuses": [first_replay_status, second_replay_status],
            "first_scope_revoked_status": first_scope_revoked_status,
            "retained_scope_status_after_first_revoke": retained_scope_status,
            "final_retained_scope_revoked_status": final_retained_scope_status,
            "account_api_status": 401,
            "scoped_session_expiry_columns": expiry_columns,
            "explicit_revoke_status": 204,
            "retained_scope_stop_start_after_first_revoke": True,
            "scoped_reset_preserved_prior_snapshots": int(reset_snapshots),
            "scoped_reset_published_current_snapshot": True,
            "scoped_bound_server_delete_status": 202,
            "deleted_server_scoped_status": deleted_server_status,
        })
