import importlib.util
import io
import json
import os
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "package_smoke", Path(__file__).resolve().parents[1] / "package_smoke.py"
)
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


class PackageSmokeTests(unittest.TestCase):
    def _archive(self, path: Path, members: dict[str, bytes], *, link: str | None = None):
        with tarfile.open(path, "w:gz") as archive:
            for name, content in members.items():
                info = tarfile.TarInfo(name)
                info.size = len(content)
                archive.addfile(info, io.BytesIO(content))
            if link:
                info = tarfile.TarInfo(link)
                info.type = tarfile.SYMTYPE
                info.linkname = "/tmp/unexpected"
                archive.addfile(info)

    def test_extract_archive_and_record_file_list(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            archive = directory / "campfire-runtime-0.1.0.crate"
            self._archive(archive, {
                "campfire-runtime-0.1.0/Cargo.toml": b"[package]\nname = 'campfire-runtime'\n",
                "campfire-runtime-0.1.0/src/lib.rs": b"pub fn ok() {}\n",
            })
            self.assertEqual(len(smoke.package_members(archive, "campfire-runtime")), 2)
            extracted = smoke.extract_package(archive, directory / "unpacked", "campfire-runtime")
            self.assertEqual((extracted / "src/lib.rs").read_text(), "pub fn ok() {}\n")
            self.assertEqual(len(smoke.sha256(archive)), 64)

    def test_rejects_outside_archive_members_and_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            archive = directory / "bad.crate"
            for bad in ("campfire-runtime-0.1.0/../outside", "other-0.1.0/Cargo.toml"):
                with self.subTest(bad=bad):
                    self._archive(archive, {bad: b"bad"})
                    with self.assertRaises(ValueError):
                        smoke.extract_package(archive, directory / "unpacked", "campfire-runtime")
            self._archive(archive, {"campfire-runtime-0.1.0/Cargo.toml": b"ok"}, link="campfire-runtime-0.1.0/link")
            with self.assertRaises(ValueError):
                smoke.package_members(archive, "campfire-runtime")

    def test_consumers_use_independent_paths_and_local_unpublished_sources(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            paths = {name: directory / name for name in smoke.CRATES}
            manifest = smoke.consumer_manifest(paths)
            patch = smoke.local_patch_config(paths)
            self.assertIn(f'path = "{paths["campfire-runtime"]}"', manifest)
            self.assertIn('version = "0.1.0"', manifest)
            self.assertIn("[patch.crates-io]", patch)
            self.assertIn(str(paths["campfire-arena"]), patch)
            self.assertNotIn("campfire-server", patch)
            self.assertIn("impl GameModule for LocalModule", smoke.CONSUMER)
            self.assertIn("shutdown_with_timeout", smoke.CONSUMER)

    def test_normalized_manifest_requires_msrv_license_and_registry_dependencies(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "Cargo.toml").write_text('''[package]
name = "campfire-arena"
version = "0.1.0"
rust-version = "1.96"
license = "MIT"
repository = "https://github.com/sangjinsu/campfire"
description = "Test arena"
[dependencies.campfire-runtime]
version = "0.1.0"
''')
            (directory / "LICENSE").write_bytes((smoke.ROOT / "LICENSE").read_bytes())
            members = ["campfire-arena-0.1.0/Cargo.toml", "campfire-arena-0.1.0/LICENSE"]
            self.assertEqual(smoke.validate_package_source(directory, "campfire-arena", members)["rust-version"], "1.96")
            manifest = directory / "Cargo.toml"
            manifest.write_text(manifest.read_text().replace('rust-version = "1.96"', 'rust-version = "1.97"'))
            with self.assertRaisesRegex(ValueError, "rust-version"):
                smoke.validate_package_source(directory, "campfire-arena", members)
            manifest.write_text(manifest.read_text().replace('rust-version = "1.97"', 'rust-version = "1.96"').replace('[dependencies.campfire-runtime]\nversion = "0.1.0"', '[dependencies.campfire-runtime]\nversion = "0.1.0"\npath = "../campfire-runtime"'))
            with self.assertRaisesRegex(ValueError, "registry-ready"):
                smoke.validate_package_source(directory, "campfire-arena", members)
            with self.assertRaisesRegex(ValueError, "excluded workspace data"):
                smoke.validate_package_source(directory, "campfire-arena", members + ["campfire-arena-0.1.0/.env"])

    def test_rejects_truncated_license_even_with_correct_first_line(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "Cargo.toml").write_text('''[package]
name = "campfire-runtime"
version = "0.1.0"
rust-version = "1.96"
license = "MIT"
repository = "https://github.com/sangjinsu/campfire"
description = "Test runtime"
''')
            (directory / "LICENSE").write_text("MIT License\n")
            with self.assertRaisesRegex(ValueError, "LICENSE"):
                smoke.validate_package_source(directory, "campfire-runtime", ["campfire-runtime-0.1.0/LICENSE"])

    def test_target_choice_prefers_argument_then_environment_then_temporary_workspace(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            work = base / "work"
            explicit = base / "explicit"
            inherited = base / "inherited"
            self.assertEqual(smoke.choose_target_dir(explicit, {"CARGO_TARGET_DIR": str(inherited)}, work), (explicit.resolve(), "argument"))
            self.assertEqual(smoke.choose_target_dir(None, {"CARGO_TARGET_DIR": str(inherited)}, work), (inherited.resolve(), "environment"))
            self.assertEqual(smoke.choose_target_dir(None, {}, work), ((work / "target").resolve(), "temporary"))
            self.assertNotEqual(smoke.choose_target_dir(None, {}, work)[0], smoke.ROOT / "target")

    def test_default_main_records_temporary_target_without_running_cargo(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "evidence"
            with patch.dict(os.environ, {"CARGO_TARGET_DIR": ""}), patch.object(sys, "argv", ["package_smoke.py", "--output", str(output)]), patch.object(smoke, "run_command", side_effect=RuntimeError("dry run")):
                self.assertEqual(smoke.main(), 1)
            summary = json.loads((output / "summary.json").read_text())
            self.assertEqual(summary["target_source"], "temporary")
            self.assertNotEqual(Path(summary["target_dir"]), smoke.ROOT / "target")
            self.assertFalse(Path(summary["target_dir"]).exists())


if __name__ == "__main__":
    unittest.main()
