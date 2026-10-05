import fcntl
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

from run_bounded_build import (
    GIB,
    parse_meminfo,
    private_build_temp_root,
    private_build_tempdir,
    run_build,
)


class BuildMemoryGuardTests(unittest.TestCase):
    def test_linux_memory_units_and_bad_values(self):
        total, available = parse_meminfo(
            "MemTotal: 16384 kB\nMemAvailable: 8192 kB\nSwapTotal: 0 kB\n"
        )
        self.assertEqual((total, available), (16 * 1024**2, 8 * 1024**2))
        with self.assertRaises(RuntimeError):
            parse_meminfo("MemTotal: 100 kB\nMemAvailable: 101 kB\n")
        with self.assertRaises(RuntimeError):
            parse_meminfo("MemTotal: 100 MB\nMemAvailable: 80 MB\n")

    def test_build_probe_tempdir_is_private_to_home(self):
        root = private_build_temp_root()
        self.assertTrue(root.is_relative_to(Path.home().resolve()))
        self.assertNotEqual(root, Path("/tmp"))
        with private_build_tempdir("kurbis-test-") as directory:
            path = Path(directory).resolve()
            self.assertTrue(path.is_relative_to(root))
            self.assertEqual(path.stat().st_mode & 0o077, 0)

    def test_host_memory_deferral_fails_closed_with_measured_headroom(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(TimeoutError, "available=1024MiB"):
                run_build(
                    [sys.executable, "-c", "pass"],
                    memory_bytes=2 * GIB,
                    reserve_bytes=4 * GIB,
                    wait_seconds=1,
                    path=Path(directory) / "lock",
                    memory_probe=lambda: (8 * GIB, 1 * GIB),
                )

    def test_independent_process_builds_share_one_admission_lock(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            events = root / "events"
            counter_lock = root / "counter.lock"
            code = r"""
import fcntl, pathlib, sys, time
lock_path = sys.argv[1]
events = pathlib.Path(sys.argv[2])
def update(delta):
    with open(lock_path, 'a+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        current, maximum = map(int, events.read_text().split()) if events.exists() else (0, 0)
        current += delta
        maximum = max(maximum, current)
        events.write_text(f'{current} {maximum}')
        fcntl.flock(lock, fcntl.LOCK_UN)
update(1)
time.sleep(.25)
update(-1)
"""
            command = [
                sys.executable,
                str(Path(__file__).with_name("run_bounded_build.py")),
                "--memory-bytes",
                "1",
                "--reserve-bytes",
                "1",
                "--wait-seconds",
                "3",
                "--lock-file",
                str(root / "host.lock"),
                "--",
                sys.executable,
                "-c",
                code,
                str(counter_lock),
                str(events),
            ]
            # Spawn concurrently. The guard serializes the subprocesses by
            # flocking a file shared across independent Python interpreters.
            first = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            second = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            first_output = first.communicate(timeout=8)
            second_output = second.communicate(timeout=8)
            self.assertEqual(first.returncode, 0, first_output)
            self.assertEqual(second.returncode, 0, second_output)
            self.assertEqual(events.read_text(), "0 1")


    def test_sigkill_of_wrapper_keeps_build_lock_until_child_exits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock = root / "host.lock"
            pid_file = root / "child.pid"
            command = [
                sys.executable,
                str(Path(__file__).with_name("run_bounded_build.py")),
                "--memory-bytes",
                "1",
                "--reserve-bytes",
                "1",
                "--wait-seconds",
                "5",
                "--lock-file",
                str(lock),
                "--",
                "env",
                f"CHILD_PID={pid_file}",
                sys.executable,
                "-c",
                "import os,pathlib,time; pathlib.Path(os.environ['CHILD_PID']).write_text(str(os.getpid())); time.sleep(60)",
            ]
            wrapper = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + 3
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(pid_file.exists(), "guard never started its build subprocess")
            child_pid = int(pid_file.read_text())
            wrapper.kill()
            wrapper.wait(timeout=3)
            try:
                run_build(
                    [sys.executable, "-c", "pass"],
                    memory_bytes=1,
                    reserve_bytes=1,
                    wait_seconds=1,
                    path=lock,
                    memory_probe=lambda: (8 * GIB, 7 * GIB),
                )
            except TimeoutError as error:
                self.assertIn("another build lease", str(error))
            else:
                self.fail("SIGKILLed wrapper released the lease while its build child was active")
            os.kill(child_pid, signal.SIGKILL)
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    os.kill(child_pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.05)
            else:
                self.fail("test build subprocess failed to terminate")

    def test_termination_releases_lock_and_kills_child_process_group(self):
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / "host.lock"
            pid_file = Path(directory) / "child.pid"
            command = [
                sys.executable,
                str(Path(__file__).with_name("run_bounded_build.py")),
                "--memory-bytes",
                "1",
                "--reserve-bytes",
                "1",
                "--lock-file",
                str(lock),
                "--",
                "env",
                f"CHILD_PID={pid_file}",
                sys.executable,
                "-c",
                "import os,pathlib,time; pathlib.Path(os.environ['CHILD_PID']).write_text(str(os.getpid())); time.sleep(60)",
            ]
            process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(0.2)
            process.terminate()
            process.wait(timeout=3)
            result = run_build(
                [sys.executable, "-c", "pass"],
                memory_bytes=1,
                reserve_bytes=1,
                wait_seconds=1,
                path=lock,
                memory_probe=lambda: (8 * GIB, 7 * GIB),
            )
            self.assertEqual(result, 0)
            child_pid = int(pid_file.read_text())
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    os.kill(child_pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.05)
            else:
                self.fail("signal forwarding left the build subprocess running")


if __name__ == "__main__":
    unittest.main()
