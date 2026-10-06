import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-musl"


@unittest.skipUnless(os.name == "posix" and shutil.which("curl"), "install.sh targets macOS and Linux with curl")
class InstallScriptTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        binary = self.root / "anytopdf"
        binary.write_text("#!/bin/sh\necho anytopdf fixture\n")
        binary.chmod(0o755)
        plugin = self.root / "anytopdf-plugin-whisper"
        plugin.write_text("#!/bin/sh\n")
        plugin.chmod(0o755)
        self.version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
        self.release = self.root / "releases" / f"v{self.version}"
        subprocess.run(
            [sys.executable, str(ROOT / "scripts/package.py"), "--target", TARGET, "--binary", str(binary),
             "--plugin", str(plugin), "--output", str(self.release)],
            check=True, capture_output=True,
        )
        self.archive = self.release / f"anytopdf-{self.version}-{TARGET}.tar.gz"

    def install(self):
        env = dict(
            os.environ,
            HOME=str(self.root / "home"),
            ANYTOPDF_VERSION=f"v{self.version}",
            ANYTOPDF_TARGET=TARGET,
            ANYTOPDF_DOWNLOAD_URL=(self.root / "releases").as_uri(),
        )
        for name in ("XDG_DATA_HOME", "ANYTOPDF_DATA_DIR", "ANYTOPDF_PLUGIN_DIR", "ANYTOPDF_INSTALL_DIR"):
            env.pop(name, None)
        return subprocess.run(["sh", str(ROOT / "install.sh")], env=env, capture_output=True, text=True)

    def test_install_verifies_and_places_binary_and_plugins(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        installed = self.root / "home/.local/bin/anytopdf"
        self.assertTrue(os.access(installed, os.X_OK))
        self.assertEqual(subprocess.run([str(installed)], capture_output=True, text=True).stdout, "anytopdf fixture\n")
        data = "Library/Application Support/anytopdf" if sys.platform == "darwin" else ".local/share/anytopdf"
        self.assertTrue((self.root / "home" / data / "plugins/anytopdf-plugin-whisper").is_file())
        self.assertIn("anytopdf setup whisper", result.stderr)
        self.assertIn("ANYTOPDF_PLUGIN_PATH=", result.stderr)

    def test_install_rejects_checksum_mismatch(self):
        with open(self.archive, "ab") as archive:
            archive.write(b"tampered")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertFalse((self.root / "home/.local/bin/anytopdf").exists())

    def test_install_reports_missing_release(self):
        self.archive.unlink()
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("download failed", result.stderr)


if __name__ == "__main__":
    unittest.main()
