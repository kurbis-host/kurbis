#!/usr/bin/env python3
"""Private SSH job runner for the remote Bubblewrap CI builder.

The control plane sends one JSON header followed by two bounded tar archives. The
runner emits one JSON result followed by the two bounded output files. It never
uses public /tmp or evaluates source-controlled shell input; only this checked-in
build script runs inside the Bubblewrap namespace.
"""
import hashlib
import json
import os
import secrets
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

MAX_SOURCE = 256 * 1024 * 1024
MAX_PLUGIN = 64 * 1024 * 1024
MAX_ARTIFACT = 1024 * 1024 * 1024
MAX_LOG = 512 * 1024
REMOTE_PROTOCOL_VERSION = 2

PREFLIGHT_SCRIPT = r"""
set -eu
[ "$(id -u)" = 0 ]
grep -Eq '^NoNewPrivs:[[:space:]]+1$' /proc/self/status
grep -Eq '^CapEff:[[:space:]]+0+$' /proc/self/status
[ "$HOME" = /work/home ]
[ ! -e /run/docker.sock ]
[ ! -e /var/run/docker.sock ]
[ -d /cache ] && [ ! -L /cache ]
awk 'NR > 1 && $1 != "lo" { route = 1 } END { exit route }' /proc/net/route
awk '$NF != "lo" { route = 1 } END { exit route }' /proc/net/ipv6_route
for tool in rustc cargo gcc tar git strip awk; do
  command -v "$tool" >/dev/null || { echo "builder tool unavailable: $tool" >&2; exit 78; }
done
actual_rustc="$(rustc -vV | awk -F': ' '/^release: / { print $2; exit }')"
[ "$actual_rustc" = "$KURBIS_EXPECTED_RUSTC" ] || {
  echo "builder Rust compiler mismatch: expected $KURBIS_EXPECTED_RUSTC, got $actual_rustc" >&2
  exit 78
}
# Exercise the same GCC/native link and Rust target linker paths used by Cargo;
# a namespace-only probe alone would miss a truncated rootfs or sysroot.
printf 'int main(void) { return 0; }\n' > /work/tmp/c-link-probe.c
gcc /work/tmp/c-link-probe.c -o /work/out/c-link-probe
/work/out/c-link-probe
strip /work/out/c-link-probe
printf 'fn main() {}\n' > /work/tmp/rust-link-probe.rs
rustc --target "$KURBIS_RUST_TARGET" -C strip=debuginfo /work/tmp/rust-link-probe.rs -o /work/out/rust-link-probe
/work/out/rust-link-probe
printf '#!/bin/sh\nprintf REMOTE_ARTIFACT_PROBE\n' > /work/out/artifact-probe
chmod 0700 /work/out/artifact-probe
[ "$(/work/out/artifact-probe)" = REMOTE_ARTIFACT_PROBE ]
printf 'artifact_execution=true\n'
wasm_libdir="$(rustc --print target-libdir --target wasm32-wasip2)"
test -d "$wasm_libdir" || { echo "WASI target libraries unavailable: $wasm_libdir" >&2; exit 78; }
test -x /work/.ci-source-preparer || { echo "trusted source preparer unavailable" >&2; exit 78; }
mkdir -p /work/tmp/tar-source /work/tmp/tar-dest
printf probe > /work/tmp/tar-source/probe
tar -czf /work/tmp/archive-probe.tar.gz -C /work/tmp/tar-source .
tar -xzf /work/tmp/archive-probe.tar.gz -C /work/tmp/tar-dest --no-same-owner --no-same-permissions
[ "$(cat /work/tmp/tar-dest/probe)" = probe ]
printf 'BWRAP_NAMESPACE_AND_TOOLCHAIN_PROBE_PASSED\n'
printf 'rustc=%s\n' "$actual_rustc"
printf 'rust_target=%s\n' "$KURBIS_RUST_TARGET"
printf 'wasi_libdir=%s\n' "$wasm_libdir"
"""

BUILD_SCRIPT = r"""
set -eu
umask 077
ulimit -n 4096 2>/dev/null || true
mkdir -p /work/source /work/plugin /work/out /work/tmp /work/home
export HOME=/work/home
export TMPDIR=/work/tmp
export CARGO_HOME=/toolchain/cargo
export CARGO_BUILD_JOBS=1
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_PROFILE_RELEASE_OPT_LEVEL=2
export CARGO_PROFILE_RELEASE_STRIP=debuginfo
cache_mode=disabled
if command -v sccache >/dev/null 2>&1 && [ -d /cache ]; then
  mkdir -p /cache/sccache
  export SCCACHE_DIR=/cache/sccache
  export SCCACHE_CACHE_SIZE="$KURBIS_BUILD_CACHE_MAX_BYTES"
  export RUSTC_WRAPPER=sccache
  cache_mode=sccache
fi
cargo_build() {
  if cargo "$@"; then
    return 0
  fi
  if [ "$cache_mode" = sccache ]; then
    echo 'build_cache=sccache_error_retry_without_cache'
    unset RUSTC_WRAPPER SCCACHE_DIR SCCACHE_CACHE_SIZE
    cache_mode=disabled-after-error
    cargo "$@"
    return $?
  fi
  return 1
}
actual_rustc="$(rustc -vV | awk -F': ' '/^release: / { print $2; exit }')"
[ "$actual_rustc" = "$KURBIS_EXPECTED_RUSTC" ] || {
  echo "builder Rust compiler mismatch: expected $KURBIS_EXPECTED_RUSTC, got $actual_rustc" >&2
  exit 78
}
build_elapsed_seconds=0
SECONDS=0
tar -xzf /work/input/source.tar.gz -C /work/source --no-same-owner --no-same-permissions
tar -xzf /work/input/plugin.tar.gz -C /work/plugin --no-same-owner --no-same-permissions
cd /work/source
cargo_build build --locked --offline --release --jobs 1 --package pumpkin --bin pumpkin --target "$KURBIS_RUST_TARGET" --target-dir /work/target
cp "/work/target/$KURBIS_RUST_TARGET/release/pumpkin" /work/out/pumpkin
cd /work/plugin
cargo_build build --locked --offline --release --jobs 1 --manifest-path /work/plugin/Cargo.toml --target wasm32-wasip2 --target-dir /work/plugin-target
cp /work/plugin-target/wasm32-wasip2/release/kurbis_control.wasm /work/out/kurbis-control.wasm
strip /work/out/pumpkin
build_elapsed_seconds=$SECONDS
printf 'build_elapsed_seconds=%s\n' "$build_elapsed_seconds"
printf 'build_cache=%s\n' "$cache_mode"
if [ "$cache_mode" != disabled ] && command -v sccache >/dev/null 2>&1; then sccache --show-stats || true; fi
printf 'bubblewrap_guarantee=%s\n' "$KURBIS_BWRAP_GUARANTEE"
printf 'cgroup_delegation=%s\n' "$KURBIS_CGROUP_DELEGATION"
printf 'workspace_boundary=operator_tmpfs_explicit_size_and_inodes\n'
"""


def fail(message, status=1, stdout="", stderr=""):
    result = {"ok": False, "status": status, "stdout": stdout[-MAX_LOG:], "stderr": (message + "\n" + stderr)[-MAX_LOG:], "artifacts": [], "capability": None}
    sys.stdout.write(json.dumps(result, separators=(",", ":")) + "\n")
    sys.stdout.flush()
    raise SystemExit(0)


def read_exact(stream, length):
    if length < 0 or length > MAX_SOURCE:
        raise ValueError("invalid framed input length")
    result = bytearray()
    while len(result) < length:
        chunk = stream.read(min(1024 * 1024, length - len(result)))
        if not chunk:
            raise ValueError("truncated framed input")
        result.extend(chunk)
    return bytes(result)


def private_workspace_root(value):
    root = Path(value).expanduser()
    home = Path(os.environ["HOME"]).resolve(strict=True)
    root.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    root.mkdir(mode=0o700, exist_ok=True)
    root = root.resolve(strict=True)
    if root == home or not root.is_relative_to(home):
        raise ValueError("workspace root must be below HOME")
    # The SSH command enters a private user+mount namespace. Mounting the
    # operator-specified five-GiB workspace there gives each job the exact
    # bounded tmpfs contract without requiring a host-wide root mount.
    mounts = Path("/proc/self/mountinfo").read_text()
    if str(root) not in mounts or "size=5242880k" not in mounts or "nr_inodes=1310720" not in mounts:
        subprocess.run(["mount", "-t", "tmpfs", "-o", "size=5G,nr_inodes=1310720,mode=700", "tmpfs", str(root)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    mode = stat.S_IMODE(root.stat().st_mode)
    if mode != 0o700 or root.stat().st_uid != os.getuid():
        raise ValueError("workspace root must be a mode-0700 directory owned by the namespace user")
    return root


def private_cache_root(value):
    root = Path(value).expanduser()
    home = Path(os.environ["HOME"]).resolve(strict=True)
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    root = root.resolve(strict=True)
    if root == home or not root.is_relative_to(home):
        raise ValueError("compiler cache root must be below HOME")
    if str(root) == "/tmp" or str(root).startswith("/tmp/"):
        raise ValueError("compiler cache root may not use public /tmp")
    mode = stat.S_IMODE(root.stat().st_mode)
    if mode != 0o700 or root.stat().st_uid != os.getuid():
        raise ValueError("compiler cache root must be a mode-0700 directory owned by the namespace user")
    return root


def validate_cache_size(value):
    size = int(value)
    if size < 256 * 1024 * 1024 or size > 64 * 1024 * 1024 * 1024:
        raise ValueError("compiler cache size is outside the bounded range")
    return size


def hash_path(path):
    """Hash immutable files/directories with stable metadata and content."""
    digest = hashlib.sha256()
    if path.is_file():
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()
    if not path.is_dir():
        raise ValueError(f"immutable input is not a regular file or directory: {path}")
    entries = []
    for entry in path.rglob("*"):
        relative = entry.relative_to(path).as_posix()
        info = entry.lstat()
        if stat.S_ISLNK(info.st_mode):
            kind = b"l"
            content = os.readlink(entry).encode()
        elif stat.S_ISDIR(info.st_mode):
            kind = b"d"
            content = b""
        elif stat.S_ISREG(info.st_mode):
            kind = b"f"
            file_digest = hashlib.sha256()
            with entry.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    file_digest.update(chunk)
            content = file_digest.digest()
        else:
            raise ValueError(f"immutable input contains unsupported entry: {entry}")
        entries.append((relative, kind, info.st_size, info.st_mode & 0o7777, content))
    for relative, kind, size, mode, content in sorted(entries):
        digest.update(relative.encode())
        digest.update(b"\\0")
        digest.update(kind)
        digest.update(size.to_bytes(8, "big"))
        digest.update(mode.to_bytes(4, "big"))
        digest.update(content)
    return digest.hexdigest()


def meminfo():
    values = {}
    for line in Path("/proc/meminfo").read_text().splitlines():
        key, _, raw = line.partition(":")
        parts = raw.strip().split()
        if parts and parts[0].isdigit():
            values[key] = int(parts[0]) * (1024 if len(parts) > 1 and parts[1] == "kB" else 1)
    return values.get("MemTotal", 0), values.get("MemAvailable", 0)


def cgroup_mode():
    root = Path("/sys/fs/cgroup")
    try:
        controllers = set((root / "cgroup.controllers").read_text().split())
        delegated = set((root / "cgroup.subtree_control").read_text().split())
    except OSError:
        return "unavailable"
    required = {"cpu", "memory", "pids"}
    if required <= controllers and required <= {value.lstrip("+") for value in delegated}:
        return "delegated"
    if required <= controllers:
        return "available_not_delegated"
    return "unavailable"

def runner_command(header, workspace, script):
    bwrap = Path(header["bwrap"])
    rootfs = Path(header["rootfs"])
    toolchain = Path(header["toolchain"])
    preparer = Path(header["source_preparer"])
    for path, label in ((bwrap, "bubblewrap"), (rootfs, "rootfs"), (toolchain, "toolchain"), (preparer, "source_preparer")):
        if label in ("rootfs", "toolchain") and path == Path("/"):
            raise ValueError(f"{label} may not be host root")
        if not path.is_absolute() or not path.exists():
            raise ValueError(f"{label} path is unavailable")
    env_values = {
        "KURBIS_RUST_TARGET": header["target"],
        "KURBIS_EXPECTED_RUSTC": header["rustc"],
        "KURBIS_BUILD_CACHE_MAX_BYTES": str(validate_cache_size(header["cache_max_bytes"])),
        "KURBIS_BWRAP_GUARANTEE": "bubblewrap_namespace_only",
        "KURBIS_CGROUP_DELEGATION": "remote_probe",
    }
    command = [
        str(bwrap), "--die-with-parent", "--new-session", "--unshare-user", "--unshare-pid",
        "--unshare-ipc", "--unshare-uts", "--unshare-net", "--uid", "0", "--gid", "0",
        "--cap-drop", "ALL", "--ro-bind", str(rootfs), "/", "--ro-bind", str(toolchain), "/toolchain",
        "--bind", str(private_cache_root(header["cache_root"])), "/cache",
        "--bind", str(workspace), "/work", "--proc", "/proc", "--dev", "/dev",
        "--bind", str(workspace / "tmp"), "/tmp", "--bind", str(workspace / "run"), "/run",
        "--bind", str(workspace / "root"), "/root", "--bind", str(workspace / "home"), "/home",
        "--bind", str(workspace / "sys"), "/sys", "--chdir", "/work", "--clearenv",
        "--setenv", "HOME", "/work/home", "--setenv", "PATH", "/toolchain/bin:/usr/local/bin:/usr/bin:/bin",
        "--ro-bind", str(preparer), "/work/.ci-source-preparer",
    ]
    for key, value in env_values.items():
        command.extend(("--setenv", key, value))
    command.extend(("/bin/sh", "-ec", script))
    return command


def main():
    line = sys.stdin.buffer.readline(16385)
    if not line or len(line) > 16384 or not line.endswith(b"\n"):
        fail("invalid job header")
    try:
        header = json.loads(line)
        if header.get("version") != REMOTE_PROTOCOL_VERSION or header.get("mode") not in ("preflight", "compile"):
            raise ValueError("unsupported job header")
        if any(not isinstance(header.get(key), str) or not header[key] for key in ("job_id", "target", "rustc", "workspace_root", "cache_root", "bwrap", "rootfs", "toolchain", "source_preparer")):
            raise ValueError("missing job field")
        if not all(c in "0123456789abcdef" for c in header["job_id"].lower()) or len(header["job_id"]) > 64:
            raise ValueError("invalid job id")
        source_len = int(header.get("source_len", 0))
        plugin_len = int(header.get("plugin_len", 0))
        validate_cache_size(header.get("cache_max_bytes", 0))
        if int(header.get("profile_memory_bytes", 0)) <= 0 or int(header.get("profile_workspace_bytes", 0)) <= 0 or int(header.get("profile_workspace_inodes", 0)) <= 0 or int(header.get("profile_concurrency", 0)) <= 0:
            raise ValueError("invalid resource profile")
        if source_len < 0 or source_len > MAX_SOURCE or plugin_len < 0 or plugin_len > MAX_PLUGIN:
            raise ValueError("framed input exceeds bounds")
        if header["mode"] == "compile" and not isinstance(header.get("capability_fingerprint"), str):
            raise ValueError("compile requires a registered capability fingerprint")
        if header["mode"] == "preflight" and (source_len or plugin_len):
            raise ValueError("preflight cannot carry input")
        workspace_root = private_workspace_root(header["workspace_root"])
        workspace = workspace_root / ("kurbis-bwrap-remote-" + header["job_id"])
        workspace.mkdir(mode=0o700)
        for name in ("input", "out", "tmp", "home", "run", "root", "sys"):
            (workspace / name).mkdir(mode=0o700)
        source = read_exact(sys.stdin.buffer, source_len)
        plugin = read_exact(sys.stdin.buffer, plugin_len)
        if source_len:
            (workspace / "input/source.tar.gz").write_bytes(source)
            (workspace / "input/source.tar.gz").chmod(0o600)
        if plugin_len:
            (workspace / "input/plugin.tar.gz").write_bytes(plugin)
            (workspace / "input/plugin.tar.gz").chmod(0o600)
        if header["mode"] == "preflight":
            command = runner_command(header, workspace, PREFLIGHT_SCRIPT)
        else:
            command = runner_command(header, workspace, BUILD_SCRIPT)
        completed = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=int(header.get("timeout_seconds", 5400)), check=False)
        stdout = completed.stdout[-MAX_LOG:].decode("utf-8", "replace")
        stderr = completed.stderr[-MAX_LOG:].decode("utf-8", "replace")
        artifacts = []
        capability = None
        if completed.returncode == 0 and header["mode"] == "preflight":
            workspace_stat = os.statvfs(workspace)
            workspace_bytes = workspace_stat.f_frsize * workspace_stat.f_blocks
            workspace_inodes = workspace_stat.f_files
            if workspace_bytes != int(header["profile_workspace_bytes"]) or workspace_inodes != int(header["profile_workspace_inodes"]):
                raise ValueError("measured workspace size or inode budget differs from profile")
            probe = workspace / "out" / "artifact-probe"
            transferred = probe.is_file() and probe.read_bytes() == b"#!/bin/sh\nprintf REMOTE_ARTIFACT_PROBE\n"
            rustc_release = ""
            for line in stdout.splitlines():
                if line.startswith("rustc="):
                    rustc_release = line.split("=", 1)[1].strip()
                    break
            total_memory, available_memory = meminfo()
            capability = {
                "protocol_version": REMOTE_PROTOCOL_VERSION,
                "host_uid": os.getuid(),
                "host_gid": os.getgid(),
                "sandbox_uid": 0,
                "sandbox_gid": 0,
                "bwrap_hash": hash_path(Path(header["bwrap"])),
                "rootfs_hash": hash_path(Path(header["rootfs"])),
                "toolchain_hash": hash_path(Path(header["toolchain"])),
                "source_preparer_hash": hash_path(Path(header["source_preparer"])),
                "rustc_version": rustc_release,
                "workspace_bytes": workspace_bytes,
                "workspace_inodes": workspace_inodes,
                "memory_total_bytes": total_memory,
                "memory_available_bytes": available_memory,
                "cgroup_mode": cgroup_mode(),
                "smoke_build": completed.returncode == 0 and "BWRAP_NAMESPACE_AND_TOOLCHAIN_PROBE_PASSED" in stdout,
                "artifact_transfer": transferred,
                "artifact_execution": "artifact_execution=true" in stdout,
            }
        if completed.returncode == 0 and header["mode"] == "compile":
            for filename in ("pumpkin", "kurbis-control.wasm"):
                path = workspace / "out" / filename
                info = path.lstat()
                if not stat.S_ISREG(info.st_mode) or info.st_size <= 0 or info.st_size > MAX_ARTIFACT:
                    raise ValueError(f"invalid output {filename}")
                data = path.read_bytes()
                artifacts.append({"name": filename, "size": len(data), "sha256": hashlib.sha256(data).hexdigest(), "data": data})
        result = {"ok": completed.returncode == 0, "status": completed.returncode, "stdout": stdout, "stderr": stderr, "artifacts": [{k: v for k, v in item.items() if k != "data"} for item in artifacts], "capability": capability}
        sys.stdout.write(json.dumps(result, separators=(",", ":")) + "\n")
        sys.stdout.flush()
        for item in artifacts:
            sys.stdout.buffer.write(item["data"])
        sys.stdout.buffer.flush()
    except subprocess.TimeoutExpired:
        fail("bubblewrap job timed out", 124)
    except Exception as exc:
        fail(str(exc))
    finally:
        try:
            shutil.rmtree(locals().get("workspace", ""), ignore_errors=True)
        except Exception:
            pass


if __name__ == "__main__":
    main()
