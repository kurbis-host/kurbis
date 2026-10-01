#!/usr/bin/env python3
"""Regenerate the integration patch from an owned edited source archive.

The reference repository is read-only. Write the resulting patch atomically.
"""
import argparse
import difflib
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument('--reference', type=Path, required=True)
parser.add_argument('--edited', type=Path, required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
patch = root / 'runtime/pumpkin/host-control.patch'
revision = (patch.parent / 'REVISION').read_text().strip()
files = [line.split(' b/', 1)[1].strip() for line in patch.read_text().splitlines() if line.startswith('diff --git')]
parts = []
for name in dict.fromkeys(files):
    tracked = subprocess.run(
        ['git', '-C', str(args.reference), 'cat-file', '-e', f'{revision}:{name}'],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if tracked.returncode:
        continue
    original = subprocess.check_output(['git', '-C', str(args.reference), 'show', f'{revision}:{name}']).decode()
    edited = (args.edited / name).read_text()
    if original != edited:
        parts.append(f'diff --git a/{name} b/{name}\n')
        parts.extend(difflib.unified_diff(original.splitlines(True), edited.splitlines(True), fromfile=f'a/{name}', tofile=f'b/{name}'))
temporary = patch.with_suffix('.tmp')
temporary.write_text(''.join(parts))
temporary.replace(patch)
