"""Regression checks for SDK recipe isolation; no SDK/network required."""
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

from build_openwrt import PACKAGE, extract_snapshot, rebuild_recipe, sdk_rustc


class RecipeTests(unittest.TestCase):
    def test_sdk_rust_is_in_target_host_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            sdk = Path(temporary)
            compiler = sdk / "staging_dir/target-example_musl/host/bin/rustc"
            compiler.parent.mkdir(parents=True)
            compiler.touch()
            self.assertEqual(sdk_rustc(sdk), compiler)

    def test_stale_patch_and_files_are_removed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sdk, snapshot = root / "sdk", root / "source"
            recipe = sdk / "package" / PACKAGE
            (recipe / "patches").mkdir(parents=True)
            (recipe / "patches/999-stale.patch").write_text("unwanted")
            (recipe / "old-file").write_text("unwanted")
            incoming = snapshot / "packaging/openwrt"
            incoming.mkdir(parents=True)
            (incoming / "Makefile").write_text("committed recipe")
            rebuilt = rebuild_recipe(sdk, snapshot)
            self.assertEqual(list(rebuilt.iterdir()), [rebuilt / "Makefile"])
            self.assertEqual((rebuilt / "Makefile").read_text(), "committed recipe")

    def test_existing_symlink_cannot_escape_or_delete_outside(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            outside = root / "outside"
            outside.mkdir()
            sentinel = outside / "keep"
            sentinel.write_text("untouched")
            sdk = root / "sdk"
            (sdk / "package").mkdir(parents=True)
            (sdk / "package" / PACKAGE).symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                rebuild_recipe(sdk, root / "snapshot")
            self.assertEqual(sentinel.read_text(), "untouched")

    def test_archive_rejects_links_and_traversal(self):
        for name, kind in [("../escape", tarfile.REGTYPE), ("link", tarfile.SYMTYPE)]:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                archive = root / "input.tar"
                with tarfile.open(archive, "w") as output:
                    member = tarfile.TarInfo(name)
                    member.type = kind
                    member.linkname = "../outside" if kind == tarfile.SYMTYPE else ""
                    output.addfile(member, io.BytesIO())
                with self.assertRaises(ValueError):
                    extract_snapshot(archive, root / "extracted")


if __name__ == "__main__":
    unittest.main()
