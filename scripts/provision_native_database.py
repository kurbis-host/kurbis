#!/usr/bin/env python3
"""Provision a local PostgreSQL role and database from DATABASE_URL_NATIVE."""

from __future__ import annotations

import os
import subprocess
import sys
from urllib.parse import unquote, urlsplit


class ProvisionError(Exception):
    pass


def sql_identifier(value: str) -> str:
    return '"' + value.replace('"', '""') + '"'


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def admin_psql(host: str, port: int, statement: str) -> str:
    result = subprocess.run(
        [
            "psql",
            "-X",
            "-w",
            "-v",
            "ON_ERROR_STOP=1",
            "-qAt",
            "-h",
            host,
            "-p",
            str(port),
            "-d",
            "postgres",
        ],
        input=statement + "\n",
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode:
        message = result.stderr.strip() or "psql failed without an error message"
        raise ProvisionError(message)
    return result.stdout.strip()


def main() -> int:
    database_url = os.environ.get("DATABASE_URL_NATIVE", "")
    if not database_url:
        print("DATABASE_URL_NATIVE is empty.", file=sys.stderr)
        return 2

    try:
        parsed = urlsplit(database_url)
        host = parsed.hostname or ""
        if parsed.scheme not in {"postgres", "postgresql"}:
            raise ProvisionError("DATABASE_URL_NATIVE must use postgres:// or postgresql://")
        if host not in {"localhost", "127.0.0.1", "::1"}:
            raise ProvisionError(
                "automatic provisioning only supports a local PostgreSQL server; "
                "create the configured role/database on the remote server"
            )
        if not parsed.username or parsed.password is None:
            raise ProvisionError("DATABASE_URL_NATIVE must include a role and password")

        role = unquote(parsed.username)
        password = unquote(parsed.password)
        database = unquote(parsed.path.lstrip("/"))
        if not role or not password or not database:
            raise ProvisionError(
                "DATABASE_URL_NATIVE must include a non-empty role, password, and database"
            )
        port = parsed.port or 5432
        role_identifier = sql_identifier(role)
        database_identifier = sql_identifier(database)

        role_exists = admin_psql(
            host,
            port,
            f"SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {sql_literal(role)});",
        ) == "t"
        if role_exists:
            statement = f"ALTER ROLE {role_identifier} WITH LOGIN PASSWORD {sql_literal(password)};"
        else:
            statement = f"CREATE ROLE {role_identifier} WITH LOGIN PASSWORD {sql_literal(password)};"
        admin_psql(host, port, statement)

        database_exists = admin_psql(
            host,
            port,
            f"SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = {sql_literal(database)});",
        ) == "t"
        if database_exists:
            print(f"PostgreSQL database {database!r} already exists.")
        else:
            admin_psql(
                host,
                port,
                f"CREATE DATABASE {database_identifier} OWNER {role_identifier} ENCODING 'UTF8' TEMPLATE template0;",
            )
            print(f"Created PostgreSQL database {database!r} owned by role {role!r}.")

        action = "Updated password for" if role_exists else "Created"
        print(f"{action} local PostgreSQL role {role!r}.")
        return 0
    except (ProvisionError, ValueError) as error:
        print(f"PostgreSQL setup failed: {error}", file=sys.stderr)
        print(
            "For local PostgreSQL, connect as an administrator; for remote PostgreSQL, "
            "create the configured role/database there. Then rerun ./start.sh native debug.",
            file=sys.stderr,
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
