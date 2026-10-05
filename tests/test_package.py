import hashlib
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]


class PackageTests(unittest.TestCase):
    def test_archive_contents_and_checksums(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "fixture-binary"
            binary.write_bytes(b"packaging test fixture")
            binary.chmod(0o755)
            for target in ["aarch64-apple-darwin", "x86_64-pc-windows-msvc"]:
                subprocess.run([sys.executable, str(ROOT / "scripts/package.py"), "--target", target, "--binary", str(binary), "--output", str(root / "dist")], check=True, capture_output=True)
            for checksum in (root / "dist").glob("*.sha256"):
                archive = checksum.with_suffix("")
                self.assertEqual(checksum.read_text().split()[0], hashlib.sha256(archive.read_bytes()).hexdigest())
                if archive.suffix == ".zip":
                    with zipfile.ZipFile(archive) as package:
                        names = package.namelist()
                else:
                    with tarfile.open(archive) as package:
                        names = package.getnames()
                self.assertTrue(any(name.endswith("/LICENSE-MIT") for name in names))
                self.assertTrue(any(name.endswith("/LICENSE-APACHE") for name in names))
                self.assertTrue(any(name.endswith("/LICENSE-DejaVu.txt") for name in names))
                self.assertTrue(any(name.endswith("/README.md") for name in names))

    def test_plugins_ship_in_an_opt_in_folder(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "anytopdf"
            binary.write_bytes(b"cli")
            plugins = {"x86_64-unknown-linux-musl": root / "anytopdf-plugin-whisper",
                       "x86_64-pc-windows-msvc": root / "anytopdf-plugin-whisper.exe"}
            for target, plugin in plugins.items():
                plugin.write_bytes(b"plugin")
                subprocess.run([sys.executable, str(ROOT / "scripts/package.py"), "--target", target,
                                "--binary", str(binary), "--plugin", str(plugin),
                                "--output", str(root / "dist")], check=True, capture_output=True)
                archive = next((root / "dist").glob(f"*-{target}.*[a-z]"))
                if archive.suffix == ".zip":
                    with zipfile.ZipFile(archive) as package:
                        names = package.namelist()
                else:
                    with tarfile.open(archive) as package:
                        names = package.getnames()
                stem = archive.name.removesuffix(".tar.gz").removesuffix(".zip")
                with self.subTest(target=target):
                    self.assertIn(f"{stem}/plugins/{plugin.name}", names)
                    self.assertNotIn(f"{stem}/{plugin.name}", names)

    def test_plugins_must_follow_the_runtime_plugin_name(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "anytopdf"
            binary.write_bytes(b"cli")
            misnamed = root / "whisper"
            misnamed.write_bytes(b"plugin")
            for plugin in (misnamed, root / "anytopdf-plugin-missing"):
                with self.subTest(plugin=plugin.name):
                    result = subprocess.run(
                        [sys.executable, str(ROOT / "scripts/package.py"), "--target", "aarch64-apple-darwin",
                         "--binary", str(binary), "--plugin", str(plugin), "--output", str(root / "dist")],
                        capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse((root / "dist").exists())
