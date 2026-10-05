#!/usr/bin/env python3
"""Serialize local build entry points and admit them only with host headroom.

The lock is a per-user file lock (automatically released on process death), not
an OS service or permanent global configuration. Every invocation measures the
actual execution host, waits at most ten minutes for memory, and then runs the
build while holding the shared lock. Container memory remains independently
bounded by the Docker Buildx --resource limits passed by the caller.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Sequence

GIB = 1024**3
DEFAULT_RESERVE = 4 * GIB
DEFAULT_WAIT_SECONDS = 10 * 60
DEFAULT_BUILD_CONCURRENCY = 2
MIN_BUILD_CONCURRENCY = 1
MAX_BUILD_CONCURRENCY = 64


def parse_meminfo(contents: str) -> tuple[int, int]:
    values: dict[str, int] = {}
    for line in contents.splitlines():
        key, separator, rest = line.partition(":")
        if not separator:
            continue
        parts = rest.split()
        if not parts:
            continue
        value = int(parts[0])
        if len(parts) > 1 and parts[1] not in {"kB", "B"}:
            raise RuntimeError(f"unsupported /proc/meminfo unit {parts[1]!r}")
        values[key] = value * (1024 if len(parts) > 1 and parts[1] == "kB" else 1)
    try:
        total, available = values["MemTotal"], values["MemAvailable"]
    except KeyError as error:
        raise RuntimeError(f"/proc/meminfo is missing {error.args[0]}") from error
    if total <= 0 or available <= 0 or available > total:
        raise RuntimeError("/proc/meminfo reported an impossible memory snapshot")
    return total, available


def linux_memory() -> tuple[int, int]:
    return parse_meminfo(Path("/proc/meminfo").read_text())


def macos_memory() -> tuple[int, int]:
    total = int(subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True).strip())
    output = subprocess.check_output(["vm_stat"], text=True)
    page_match = re.search(r"page size of (\d+) bytes", output)
    if not page_match:
        raise RuntimeError("vm_stat did not report the page size")
    page_size = int(page_match.group(1))
    counts: dict[str, int] = {}
    for line in output.splitlines():
        key, separator, value = line.partition(":")
        if not separator:
            continue
        match = re.search(r"([\d,]+)", value)
        if match:
            counts[key.strip()] = int(match.group(1).replace(",", ""))
    # Deliberately conservative: only free, inactive, and speculative pages
    # are treated as immediately reclaimable. Compressed memory is not counted.
    keys = ("Pages free", "Pages inactive", "Pages speculative")
    missing = [key for key in keys if key not in counts]
    if missing:
        raise RuntimeError(f"vm_stat is missing memory counters: {', '.join(missing)}")
    available = sum(counts[key] for key in keys) * page_size
    if total <= 0 or available <= 0 or available > total:
        raise RuntimeError("vm_stat reported an impossible memory snapshot")
    return total, available


def host_memory() -> tuple[int, int]:
    if sys.platform.startswith("linux"):
        return linux_memory()
    if sys.platform == "darwin":
        return macos_memory()
    raise RuntimeError(f"host-memory admission is unsupported on {sys.platform}")


def lock_path() -> Path:
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return cache / "kurbis" / "build-admission.lock"


def private_build_temp_root() -> Path:
    """Return a user-owned, mode-0700 temp root below HOME.

    Remote build preparation runs on a shared lxhalle host. Never use the
    host-wide /tmp directory for source, BuildKit probe context, or generated
    build metadata.
    """
    home = Path.home().resolve()
    root = (home / ".cache" / "kurbis" / "build-tmp").resolve()
    if home not in root.parents:
        raise RuntimeError("private build temp root escaped HOME")
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    root.chmod(0o700)
    home_stat = home.stat()
    root_stat = root.stat()
    if hasattr(os, "getuid") and (
        root_stat.st_uid != os.getuid() or home_stat.st_uid != os.getuid()
    ):
        raise RuntimeError("private build temp root and HOME must be owned by the current user")
    if root_stat.st_mode & 0o077:
        raise RuntimeError("private build temp root is accessible to other users")
    return root


def private_build_tempdir(prefix: str) -> tempfile.TemporaryDirectory[str]:
    if not prefix or not all(char.isalnum() or char in "-_" for char in prefix):
        raise ValueError("invalid private build temp prefix")
    return tempfile.TemporaryDirectory(prefix=prefix, dir=private_build_temp_root())


def verify_buildkit_resource_limits(image: str) -> None:
    """Prove per-RUN cgroup RAM/swap limits before any expensive Buildx build."""
    tag = f"kurbis-build-resource-probe:{os.getpid()}-{time.time_ns()}"
    with private_build_tempdir("kurbis-build-resource-probe-") as directory:
        context = Path(directory)
        dockerfile = r"""ARG PROBE_IMAGE
FROM ${PROBE_IMAGE}
ARG EXPECTED_MEMORY_BYTES
ARG EXPECTED_CPU_MILLIS
RUN set -eu; \
    memory=$(cat /sys/fs/cgroup/memory.max); test "$memory" -gt 0; test "$memory" -le "$EXPECTED_MEMORY_BYTES"; \
    test "$(cat /sys/fs/cgroup/memory.swap.max)" -eq 0; \
    read -r quota period < /sys/fs/cgroup/cpu.max; test "$quota" -gt 0; test "$period" -gt 0; \
    test $((quota * 1000)) -le $((period * EXPECTED_CPU_MILLIS)); \
    before=$(awk '$1 == "oom_kill" { print $2 }' /sys/fs/cgroup/memory.events); \
    if dd if=/dev/zero of=/dev/shm/kurbis-build-memory-probe bs=1M count=128 conv=fsync status=none 2>/dev/null; then \
        echo "BuildKit memory cap did not stop cgroup pressure" >&2; exit 90; \
    fi; \
    after=$(awk '$1 == "oom_kill" { print $2 }' /sys/fs/cgroup/memory.events); \
    test -n "$before"; test -n "$after"; test "$after" -gt "$before"; \
    echo BUILDKIT_CGROUP_LIMITS_PASSED
"""
        (context / "Dockerfile").write_text(dockerfile)
        memory = 64 * 1024 * 1024
        command = [
            "docker",
            "buildx",
            "build",
            "--pull=true",
            "--no-cache",
            "--network=none",
            "--progress=plain",
            "--shm-size=128m",
            "--resource",
            f"memory={memory}",
            "--resource",
            f"memory-swap={memory}",
            "--resource",
            "cpu-period=100000",
            "--resource",
            "cpu-quota=25000",
            "--build-arg",
            f"PROBE_IMAGE={image}",
            "--build-arg",
            f"EXPECTED_MEMORY_BYTES={memory}",
            "--build-arg",
            "EXPECTED_CPU_MILLIS=250",
            "--tag",
            tag,
            "--load",
            str(context),
        ]
        result = subprocess.run(command, text=True, capture_output=True, check=False)
        try:
            output = f"{result.stdout}\n{result.stderr}"
            if result.returncode != 0 or "BUILDKIT_CGROUP_LIMITS_PASSED" not in output:
                raise RuntimeError(
                    "selected BuildKit builder did not prove bounded per-RUN cgroup "
                    f"memory/swap/CPU enforcement; upgrade/configure it before builds: "
                    f"{output[-4000:]}"
                )
        finally:
            subprocess.run(
                ["docker", "image", "rm", tag],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )


def docker_memory_probe(docker_host: str, image: str) -> tuple[int, int]:
    command = ["docker"]
    if docker_host:
        command.extend(["--host", docker_host])
    command.extend([
        "run", "--rm", "--pull=missing", "--network=none",
        "--memory=64m", "--memory-swap=64m", "--pids-limit=16",
        "--read-only", "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=4m",
        "--entrypoint", "/bin/sh", image, "-ec", "cat /proc/meminfo",
    ])
    output = subprocess.run(command, text=True, capture_output=True, check=False)
    if output.returncode != 0:
        raise RuntimeError(
            "Docker host memory probe failed; refusing to run a build: "
            f"{output.stderr.strip()[-2000:]}"
        )
    return parse_meminfo(output.stdout)


def parse_concurrency(value: str | None) -> int:
    """Parse the shared cross-process build slot count."""
    if value is None:
        return DEFAULT_BUILD_CONCURRENCY
    try:
        concurrency = int(value, 10)
    except ValueError as error:
        raise ValueError(
            f"CI_BUILD_CONCURRENCY must be an integer from {MIN_BUILD_CONCURRENCY} to {MAX_BUILD_CONCURRENCY}"
        ) from error
    if not MIN_BUILD_CONCURRENCY <= concurrency <= MAX_BUILD_CONCURRENCY:
        raise ValueError(
            f"CI_BUILD_CONCURRENCY must be from {MIN_BUILD_CONCURRENCY} to {MAX_BUILD_CONCURRENCY}"
        )
    return concurrency


def _identity_digest(identity: str) -> str:
    if not identity or len(identity) > 512:
        raise ValueError("builder identity must contain 1 to 512 characters")
    return hashlib.sha256(identity.encode("utf-8")).hexdigest()[:32]


class _BuildSlotLease:
    """A crash-safe slot lock plus its memory reservation record.

    The lock descriptor is inherited by the build child, so killing this
    wrapper cannot release a live build's slot. The metadata is advisory only;
    a claimant first obtains the slot lock before trusting/removing it.
    """

    def __init__(self, handle, metadata: Path, memory_bytes: int):
        self.handle = handle
        self.metadata = metadata
        self.memory_bytes = memory_bytes
        self.released = False

    def release(self) -> None:
        if self.released:
            return
        self.released = True
        try:
            self.metadata.unlink(missing_ok=True)
        finally:
            fcntl.flock(self.handle, fcntl.LOCK_UN)
            self.handle.close()


def _read_reservation(path: Path) -> int:
    try:
        payload = json.loads(path.read_text())
        value = int(payload["memory_bytes"])
        return value if value > 0 else 0
    except (OSError, ValueError, TypeError, KeyError, json.JSONDecodeError):
        # A malformed record is never counted as capacity. The corresponding
        # slot lock remains the source of truth and is checked by the caller.
        return 0


def _write_reservation(path: Path, identity: str, memory_bytes: int) -> None:
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(
        json.dumps(
            {
                "builder_identity": identity,
                "memory_bytes": memory_bytes,
                "pid": os.getpid(),
            },
            separators=(",", ":"),
        )
    )
    os.replace(temporary, path)


def _try_slot(slot_path: Path, metadata_path: Path):
    handle = slot_path.open("a+")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        handle.close()
        return None
    # The lock was free, so any old record belongs to a crashed wrapper.
    metadata_path.unlink(missing_ok=True)
    return handle


def _acquire_build_slot(
    lock_root: Path,
    *,
    identity: str,
    concurrency: int,
    memory_bytes: int,
    reserve_bytes: int,
    deadline: float,
    memory_probe,
) -> _BuildSlotLease:
    digest = _identity_digest(identity)
    lock_root.parent.mkdir(parents=True, exist_ok=True)
    allocator_path = lock_root.with_name(f"{lock_root.name}.{digest}.allocator")
    slot_paths = [
        (
            lock_root.with_name(f"{lock_root.name}.{digest}.slot-{slot}"),
            lock_root.with_name(f"{lock_root.name}.{digest}.slot-{slot}.json"),
        )
        for slot in range(concurrency)
    ]
    last_memory_failure = None
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            if last_memory_failure is not None:
                available, required, active_memory = last_memory_failure
                raise TimeoutError(
                    "host build admission timed out: "
                    f"available={available // (1024**2)}MiB, "
                    f"required={required // (1024**2)}MiB "
                    f"({memory_bytes // (1024**2)}MiB build + "
                    f"{active_memory // (1024**2)}MiB active builds + "
                    f"{reserve_bytes // (1024**2)}MiB system/runtime reserve)"
                )
            raise TimeoutError("host build admission timed out waiting for another build lease")
        with allocator_path.open("a+") as allocator:
            try:
                fcntl.flock(allocator, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                time.sleep(min(0.1, remaining))
                continue
            candidate = None
            for slot_path, metadata_path in slot_paths:
                handle = _try_slot(slot_path, metadata_path)
                if handle is not None:
                    candidate = (handle, metadata_path)
                    break
            if candidate is None:
                fcntl.flock(allocator, fcntl.LOCK_UN)
                time.sleep(min(0.1, remaining))
                continue
            handle, metadata_path = candidate
            try:
                total, available = memory_probe()
            except BaseException:
                fcntl.flock(handle, fcntl.LOCK_UN)
                handle.close()
                fcntl.flock(allocator, fcntl.LOCK_UN)
                raise
            active_memory = sum(
                _read_reservation(metadata_path_other)
                for _, metadata_path_other in slot_paths
                if metadata_path_other != metadata_path and metadata_path_other.exists()
            )
            required = active_memory + memory_bytes + reserve_bytes
            if available < required:
                last_memory_failure = (available, required, active_memory)
                fcntl.flock(handle, fcntl.LOCK_UN)
                handle.close()
                fcntl.flock(allocator, fcntl.LOCK_UN)
                if time.monotonic() >= deadline:
                    raise TimeoutError(
                        "host build admission timed out: "
                        f"available={available // (1024**2)}MiB, "
                        f"required={required // (1024**2)}MiB "
                        f"({memory_bytes // (1024**2)}MiB build + "
                        f"{active_memory // (1024**2)}MiB active builds + "
                        f"{reserve_bytes // (1024**2)}MiB system/runtime reserve)"
                    )
                print(
                    "build deferred for host memory headroom: "
                    f"available={available // (1024**2)}MiB required={required // (1024**2)}MiB",
                    file=sys.stderr,
                    flush=True,
                )
                time.sleep(min(2.0, deadline - time.monotonic()))
                continue
            _write_reservation(metadata_path, identity, memory_bytes)
            fcntl.flock(allocator, fcntl.LOCK_UN)
            print(
                "build admission granted: "
                f"identity={identity} slots={concurrency} "
                f"total={total // (1024**2)}MiB "
                f"available={available // (1024**2)}MiB "
                f"build_cap={memory_bytes // (1024**2)}MiB "
                f"active_builds={active_memory // (1024**2)}MiB "
                f"system_reserve={reserve_bytes // (1024**2)}MiB",
                file=sys.stderr,
                flush=True,
            )
            return _BuildSlotLease(handle, metadata_path, memory_bytes)


def run_build(
    command: Sequence[str],
    *,
    memory_bytes: int,
    reserve_bytes: int,
    wait_seconds: int,
    path: Path | None = None,
    memory_probe=None,
    resource_probe_image: str | None = None,
    builder_identity: str = "local-host",
    concurrency: int = DEFAULT_BUILD_CONCURRENCY,
) -> int:
    if not command:
        raise ValueError("a build command is required after --")
    if memory_bytes <= 0 or reserve_bytes <= 0 or wait_seconds <= 0:
        raise ValueError("build memory, system reserve, and wait timeout must be positive")
    if not MIN_BUILD_CONCURRENCY <= concurrency <= MAX_BUILD_CONCURRENCY:
        raise ValueError(
            f"build concurrency must be from {MIN_BUILD_CONCURRENCY} to {MAX_BUILD_CONCURRENCY}"
        )
    if memory_probe is None:
        memory_probe = host_memory
    deadline = time.monotonic() + wait_seconds
    lease = _acquire_build_slot(
        path or lock_path(),
        identity=builder_identity,
        concurrency=concurrency,
        memory_bytes=memory_bytes,
        reserve_bytes=reserve_bytes,
        deadline=deadline,
        memory_probe=memory_probe,
    )
    try:
        probe_image = resource_probe_image
        if probe_image:
            print(
                f"verifying BuildKit per-RUN cgroup limits with {probe_image}",
                file=sys.stderr,
                flush=True,
            )
            verify_buildkit_resource_limits(probe_image)
        process = subprocess.Popen(
            command,
            start_new_session=True,
            pass_fds=(lease.handle.fileno(),),
        )
        previous_handlers: dict[int, object] = {}

        def forward_signal(signum: int, _frame: object) -> None:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signum)
                except ProcessLookupError:
                    pass

        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            previous_handlers[signum] = signal.signal(signum, forward_signal)
        try:
            return process.wait()
        finally:
            for signum, handler in previous_handlers.items():
                signal.signal(signum, handler)
    finally:
        lease.release()

def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--memory-bytes", type=int, required=True)
    parser.add_argument("--reserve-bytes", type=int, default=DEFAULT_RESERVE)
    parser.add_argument("--wait-seconds", type=int, default=DEFAULT_WAIT_SECONDS)
    parser.add_argument("--lock-file", type=Path, default=lock_path())
    parser.add_argument("--docker-host", default=os.environ.get("DOCKER_HOST", ""))
    parser.add_argument("--probe-image", default="")
    parser.add_argument("--builder-identity", default=os.environ.get("KURBIS_BUILD_BUILDER_IDENTITY", "local-host"))
    parser.add_argument("--concurrency", type=parse_concurrency, default=None)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    args.command = list(args.command)
    if args.command and args.command[0] == "--":
        args.command.pop(0)
    return args


def main(argv: Sequence[str] = sys.argv[1:]) -> int:
    try:
        args = parse_args(argv)
        probe = None
        if args.probe_image:
            # Probe the selected daemon/container host rather than assuming the
            # control plane's /proc/meminfo describes a remote SSH Docker host.
            probe = lambda: docker_memory_probe(args.docker_host, args.probe_image)
        return run_build(
            args.command,
            memory_bytes=args.memory_bytes,
            reserve_bytes=args.reserve_bytes,
            wait_seconds=args.wait_seconds,
            path=args.lock_file,
            memory_probe=probe,
            resource_probe_image=args.probe_image or None,
            builder_identity=args.builder_identity,
            concurrency=(
                args.concurrency
                if args.concurrency is not None
                else parse_concurrency(os.environ.get("CI_BUILD_CONCURRENCY"))
            ),
        )
    except (OSError, RuntimeError, ValueError, TimeoutError) as error:
        print(f"build admission failed closed: {error}", file=sys.stderr)
        return 75


if __name__ == "__main__":
    raise SystemExit(main())
