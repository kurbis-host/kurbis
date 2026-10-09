#!/usr/bin/env python3
"""Static contract checks for the local-only CI build cache wiring."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]


class BuildCacheContractTests(unittest.TestCase):
    def test_builder_target_cache_survives_source_cleanup(self):
        dockerfile = (ROOT / "runtime/ci/builder.Dockerfile").read_text()
        self.assertIn("target=/hoster/target", dockerfile)
        self.assertIn("find /hoster -mindepth 1 -maxdepth 1 ! -name target", dockerfile)
        self.assertNotIn("rm -rf /hoster\n", dockerfile)

    def test_compose_forwards_and_persists_exact_output_cache(self):
        compose = (ROOT / "compose.yaml").read_text()
        self.assertIn("CI_BUILD_CACHE_DIR:", compose)
        self.assertIn("CI_BUILD_CACHE_MAX_BYTES:", compose)
        self.assertIn("ci-build-cache:/home/kurbis/.cache/kurbis/build-cache", compose)
        self.assertIn("  ci-build-cache:\n", compose)

    def test_runtime_image_prepares_private_volume_copy_up(self):
        dockerfile = (ROOT / "runtime/Dockerfile").read_text()
        self.assertIn("/home/kurbis/.cache/kurbis/build-cache", dockerfile)
        self.assertIn("chmod 700 /home/kurbis/.cache/kurbis/build-cache", dockerfile)
        self.assertIn("chown -R 65532:65532", dockerfile)


if __name__ == "__main__":
    unittest.main()
