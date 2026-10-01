#!/usr/bin/env python3
"""Create a private first-run .env with unique deployment secrets."""

from __future__ import annotations

import os
import secrets
import sys
import tempfile
import uuid
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: initialize_deploy_env.py TEMPLATE TARGET", file=sys.stderr)
        return 2

    template, target = map(Path, sys.argv[1:])
    if target.exists():
        print(f"Refusing to overwrite existing {target}", file=sys.stderr)
        return 1

    password = secrets.token_hex(24)
    replacements = {
        "POSTGRES_PASSWORD": password,
        "INTERNAL_TOKEN": secrets.token_hex(32),
        "PUMPKIN_GATEWAY_CONTROL_TOKEN": secrets.token_hex(32),
        "WORKER_ID": str(uuid.uuid4()),
    }

    lines = template.read_text(encoding="utf-8").splitlines()
    rendered: list[str] = []
    for line in lines:
        key, separator, _ = line.partition("=")
        if separator and key in replacements:
            line = f"{key}={replacements[key]}"
        rendered.append(line)
    content = "\n".join(rendered) + "\n"

    target.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{target.name}.", dir=target.parent, text=True
    )
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(content)
        os.replace(temporary_name, target)
    except BaseException:
        try:
            os.unlink(temporary_name)
        except FileNotFoundError:
            pass
        raise

    print(f"Created {target} with private, random secrets.")
    print("Edit the public URLs, worker address, and four OAuth values before starting.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
