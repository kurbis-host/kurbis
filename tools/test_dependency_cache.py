"""The builder's cache of compiled dependencies: keyed, restorable, bounded."""
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SPEC = importlib.util.spec_from_file_location("kurbis_runner", Path(__file__).with_name("ci_bubblewrap_runner.py"))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class DependencyCacheTest(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.TemporaryDirectory()
        self.addCleanup(self.home.cleanup)
        root = Path(self.home.name).resolve()
        self.header = {"rustc": "1.98.1", "target": "x86_64-unknown-linux-gnu", "cache_root": str(root / "cache")}
        patcher = mock.patch.dict(os.environ, {"HOME": str(root)})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.root = root

    def workspace(self, name, lock="lock"):
        workspace = self.root / name
        for directory in ("source", "plugin"):
            (workspace / directory).mkdir(parents=True)
            (workspace / directory / "Cargo.lock").write_text(lock)
        return workspace

    def test_a_saved_build_is_restored_for_the_same_dependency_set(self):
        first = self.workspace("first")
        (first / "target/release").mkdir(parents=True)
        (first / "target/release/libdep.rlib").write_text("compiled")
        (first / "plugin-target").mkdir()
        archive = runner.dependency_cache_path(self.header, first)
        self.assertFalse(runner.restore_dependency_cache(archive, first))
        self.assertTrue(runner.save_dependency_cache(archive, first))

        second = self.workspace("second")
        self.assertEqual(runner.dependency_cache_path(self.header, second), archive)
        self.assertTrue(runner.restore_dependency_cache(archive, second))
        self.assertEqual((second / "target/release/libdep.rlib").read_text(), "compiled")

    def test_other_dependencies_or_another_compiler_do_not_share_a_cache(self):
        same = runner.dependency_cache_path(self.header, self.workspace("same"))
        other_lock = runner.dependency_cache_path(self.header, self.workspace("other", lock="changed"))
        other_rustc = runner.dependency_cache_path({**self.header, "rustc": "1.99.0"}, self.workspace("rustc"))
        self.assertEqual(len({same, other_lock, other_rustc}), 3)

    def test_a_damaged_archive_costs_a_cold_build_and_is_removed(self):
        workspace = self.workspace("damaged")
        archive = runner.dependency_cache_path(self.header, workspace)
        archive.write_text("not a tar archive")
        self.assertFalse(runner.restore_dependency_cache(archive, workspace))
        self.assertFalse(archive.exists())

    def test_only_the_newest_caches_are_kept(self):
        for index in range(runner.KEPT_DEPENDENCY_CACHES + 2):
            workspace = self.workspace(f"w{index}", lock=f"lock {index}")
            for name in runner.DEPENDENCY_TARGETS:
                (workspace / name).mkdir()
            archive = runner.dependency_cache_path(self.header, workspace)
            self.assertTrue(runner.save_dependency_cache(archive, workspace))
            os.utime(archive, (index, index))
        self.assertEqual(len(list(archive.parent.glob("*.tar"))), runner.KEPT_DEPENDENCY_CACHES)


if __name__ == "__main__":
    unittest.main()
