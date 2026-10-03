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
                self.assertTrue(any(name.endswith("/README.md") for name in names))
