"""Package manager manifests, cargo-binstall metadata and the container recipe."""
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import distribution  # noqa: E402

TARGETS = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
]


def checksums(version, skip=()):
    lines = []
    for target in TARGETS:
        name = distribution.archive_name(version, target)
        if name not in skip:
            lines.append(f"{hashlib.sha256(name.encode()).hexdigest()}  {name}\n")
    return "".join(lines)


class DistributionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="anytopdf distribution ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def render(self, sums, *extra):
        path = self.root / "SHA256SUMS"
        path.write_text(sums, encoding="utf-8")
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts/distribution.py"), "--checksums", str(path),
             "--output", str(self.root / "out"), "--version", "1.2.3", *extra],
            capture_output=True, text=True, encoding="utf-8")

    def test_release_targets_match_the_release_workflow(self):
        release = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        workflow = set(re.findall(r"^\s+target:\s*([\w-]+)\s*$", release, re.MULTILINE))
        self.assertEqual(workflow, set(TARGETS))
        mapped = set(distribution.HOMEBREW.values()) | set(distribution.SCOOP.values())
        self.assertEqual(mapped, set(TARGETS))

    def test_formula_and_manifest_pin_every_archive_digest(self):
        result = self.render(checksums("1.2.3"), "--repository", "owner/repo")
        self.assertEqual(result.returncode, 0, result.stderr)
        formula = (self.root / "out/anytopdf.rb").read_text(encoding="utf-8")
        manifest = json.loads((self.root / "out/anytopdf.json").read_text(encoding="utf-8"))
        for target in TARGETS:
            name = distribution.archive_name("1.2.3", target)
            url = f"https://github.com/owner/repo/releases/download/v1.2.3/{name}"
            digest = hashlib.sha256(name.encode()).hexdigest()
            if "windows" in target:
                self.assertEqual(manifest["architecture"]["64bit"]["url"], url)
                self.assertEqual(manifest["architecture"]["64bit"]["hash"], digest)
                self.assertEqual(manifest["architecture"]["64bit"]["extract_dir"],
                                 f"anytopdf-1.2.3-{target}")
            else:
                self.assertRegex(formula, rf'url "{re.escape(url)}"\n\s+sha256 "{digest}"')
        self.assertIn('version "1.2.3"', formula)
        for plugin in distribution.PLUGINS:
            self.assertIn(f"#{{libexec}}/plugins/{plugin} --anytopdf-manifest", formula)
        self.assertIn('libexec.install "plugins"', formula)
        self.assertIn('bin.install "anytopdf"\n', formula, "plugins stay off PATH until opted in")
        self.assertTrue(any("ANYTOPDF_PLUGIN_PATH" in line for line in manifest["notes"]))
        self.assertEqual(manifest["version"], "1.2.3")
        self.assertEqual(manifest["bin"], "anytopdf.exe")
        self.assertIn("$version", manifest["autoupdate"]["architecture"]["64bit"]["url"])

    def test_formula_is_valid_ruby(self):
        ruby = shutil.which("ruby")
        if not ruby:
            self.skipTest("ruby is not installed")
        result = self.render(checksums("1.2.3"))
        self.assertEqual(result.returncode, 0, result.stderr)
        check = subprocess.run([ruby, "-c", str(self.root / "out/anytopdf.rb")],
                               capture_output=True, text=True)
        self.assertEqual(check.returncode, 0, check.stderr)

    def test_missing_or_malformed_checksums_fail_without_output(self):
        missing = distribution.archive_name("1.2.3", "x86_64-apple-darwin")
        for label, sums in (
            ("missing archive", checksums("1.2.3", skip={missing})),
            ("wrong version", checksums("1.2.4")),
            ("malformed line", checksums("1.2.3") + "not a checksum\n"),
            ("path traversal", checksums("1.2.3") + "0" * 64 + "  ../x.tar.gz\n"),
        ):
            with self.subTest(case=label):
                result = self.render(sums)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("distribution:", result.stderr)
                self.assertFalse((self.root / "out").exists())

    def test_invalid_version_and_repository_are_rejected(self):
        for extra in (("--version", "1.2"), ("--repository", "not a repo")):
            with self.subTest(argument=extra[0]):
                self.assertNotEqual(self.render(checksums("1.2.3"), *extra).returncode, 0)

    def test_makefile_packages_every_bundled_plugin(self):
        makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
        listed = re.search(r"^PLUGINS \?= (.*)$", makefile, re.MULTILINE)
        self.assertIsNotNone(listed)
        self.assertEqual(listed[1].split(), distribution.PLUGINS)
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
        for plugin in distribution.PLUGINS:
            self.assertIn(f"crates/{plugin}", workspace["members"])

    def test_default_repository_comes_from_cargo_metadata(self):
        self.assertEqual(distribution.default_repository(), "adeelahmad/anytopdf-rs")

    def test_binstall_urls_name_the_packaged_archives(self):
        manifest = tomllib.loads((ROOT / "crates/anytopdf-cli/Cargo.toml").read_text(encoding="utf-8"))
        package = manifest["package"]
        binstall = package["metadata"]["binstall"]
        for target in TARGETS:
            settings = {**binstall, **binstall.get("overrides", {}).get(target, {})}
            windows = "windows" in target
            values = {"repo": "https://github.com/o/r", "version": "1.2.3", "name": package["name"],
                      "target": target, "bin": "anytopdf", "binary-ext": ".exe" if windows else ""}
            render = lambda template: re.sub(r"\{ ([\w-]+) \}", lambda m: values[m[1]], template)
            with self.subTest(target=target):
                self.assertEqual(render(settings["pkg-url"]).rsplit("/", 1)[1],
                                 distribution.archive_name("1.2.3", target))
                self.assertEqual(settings["pkg-fmt"], "zip" if windows else "tgz")
                # package.py puts the binary inside a folder named like the archive stem.
                self.assertEqual(render(settings["bin-dir"]),
                                 f"anytopdf-1.2.3-{target}/anytopdf" + (".exe" if windows else ""))

    def test_container_recipe_uses_pinned_toolchain_and_release_binaries(self):
        dockerfile = (ROOT / "Dockerfile").read_text(encoding="utf-8")
        toolchain = tomllib.loads((ROOT / "rust-toolchain.toml").read_text(encoding="utf-8"))
        major_minor = ".".join(toolchain["toolchain"]["channel"].split(".")[:2])
        self.assertIn(f"FROM rust:{major_minor}-", dockerfile)
        self.assertIn("--locked", dockerfile)
        self.assertIn("--no-default-features", dockerfile)
        self.assertIn("COPY dist/docker/${TARGETARCH}/anytopdf", dockerfile)
        for plugin in distribution.PLUGINS:
            self.assertIn(f"dist/docker/${{TARGETARCH}}/{plugin}", dockerfile)
            self.assertIn(f"/opt/anytopdf/plugins/{plugin}", dockerfile)
        self.assertIn("ARG WHISPER=none", dockerfile, "whisper.cpp stays an opt-in build")
        self.assertRegex(dockerfile, r"(?m)^USER (?!root)\w+")
        ignore = (ROOT / ".dockerignore").read_text(encoding="utf-8").splitlines()
        for needed in ("!Cargo.lock", "!crates/", "!schemas/", "!README.md", "!dist/docker/"):
            self.assertIn(needed, ignore)


if __name__ == "__main__":
    unittest.main()
