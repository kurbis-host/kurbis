#!/usr/bin/env python3
"""Disposable offline identity fixture; never used by production startup."""
import json
import os
from pathlib import Path
import sys

config = Path('pumpkin.toml')
section = ''
lines = []
for line in config.read_text().splitlines():
    if line.startswith('['):
        section = line
        lines.append(line)
        if section == '[networking.java]':
            lines.extend(['online_mode = false', 'encryption = false'])
        elif section == '[networking.bedrock]':
            lines.append('online_mode = false')
    elif section in ('[networking.java]', '[networking.bedrock]') and line.split('=', 1)[0].strip() in ('online_mode', 'encryption'):
        continue
    else:
        lines.append(line)
config.write_text('\n'.join(lines) + '\n')
Path('data').mkdir(exist_ok=True)
ops = Path('data/ops.json')
if not ops.exists():
    ops.write_text(json.dumps([{'uuid': '63afe389-0d98-f833-4096-cc01dd62a9f0', 'name': 'RuntimePlayer', 'level': 4, 'bypasses_player_limit': True}]))
binary = os.environ['NATIVE_FIXTURE_BINARY']
os.execve(binary, [os.environ.get('PUMPKIN_PROCESS_MARKER', binary)], os.environ)
