"""Release policy contracts using real, isolated Git history and metadata."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("release_policy_subject", ROOT / "scripts/release.py")
release = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = release
SPEC.loader.exec_module(release)
WORKSPACE_PACKAGES = ("anytopdf", "anytopdf-core", "anytopdf-builtin", "anytopdf-pdf")


class ReleasePolicyTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="anytopdf release policy ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git_env = os.environ.copy()
        for key in tuple(self.git_env):
            if key.startswith("GIT_"):
                self.git_env.pop(key)
        self.git_env.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull})
        self.git("init", "--quiet")
        self.git("config", "user.name", "Release Policy Test")
        self.git("config", "user.email", "policy@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        # No background auto-gc or maintenance: a detached git writing into
        # .git races the temporary directory cleanup.
        self.git("config", "gc.auto", "0")
        self.git("config", "maintenance.auto", "false")
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/release.py", self.root / "scripts/release.py")
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n\n'
            '[workspace.package]\nversion = "1.2.3"\nedition = "2024"\n', encoding="utf-8")
        for package in WORKSPACE_PACKAGES:
            crate = self.root / "crates" / package
            (crate / "src").mkdir(parents=True)
            (crate / "Cargo.toml").write_text(
                f'[package]\nname = "{package}"\nversion.workspace = true\nedition.workspace = true\n',
                encoding="utf-8")
            (crate / "src/lib.rs").write_text("", encoding="utf-8")
        self.write_lock()
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## 1.2.3 - 2026-01-01\n\nHandwritten release notes — preserve me.\n"
            "\n## 1.1.0 - 2025-12-01\n\nOlder notes with [links](https://example.invalid).\n",
            encoding="utf-8")
        self.commit("feat: establish searchable PDF conversion")

    def git(self, *args, root=None):
        return subprocess.run(["git", *args], cwd=root or self.root, env=self.git_env,
                              check=True, text=True, capture_output=True).stdout.strip()

    def commit(self, message):
        self.git("add", "--all")
        self.git("commit", "--quiet", "--allow-empty", "-m", message)

    def write_lock(self, mismatch=None):
        text = "version = 4\n"
        for package in WORKSPACE_PACKAGES:
            version = "1.2.2" if package == mismatch else "1.2.3"
            text += f'\n[[package]]\nname = "{package}"\nversion = "{version}"\n'
        text += '\n[[package]]\nname = "serde"\nversion = "1.0.219"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n'
        (self.root / "Cargo.lock").write_text(text, encoding="utf-8")

    def cli(self, *args, root=None):
        root = root or self.root
        return subprocess.run([sys.executable, str(root / "scripts/release.py"), *args],
                              cwd=root, env=self.git_env, text=True, capture_output=True)

    def test_validate_rejects_workspace_lock_version_mismatch(self):
        valid = self.cli("validate", "--tag", "v1.2.3")
        self.assertEqual(valid.returncode, 0, valid.stderr)
        self.assertIn("Validated anytopdf 1.2.3", valid.stdout)
        for package in WORKSPACE_PACKAGES:
            with self.subTest(package=package):
                self.write_lock(mismatch=package)
                result = self.cli("validate")
                self.assertNotEqual(result.returncode, 0,
                                    f"validate accepted mismatched {package} Cargo.lock version: {result.stdout}")
        self.write_lock()
        wrong_tag = self.cli("validate", "--tag", "v9.9.9")
        self.assertNotEqual(wrong_tag.returncode, 0)
        self.assertIn("tag", wrong_tag.stderr.lower())
        (self.root / "CHANGELOG.md").write_text("# Changelog\n\n## 0.0.1\n\nOld notes.\n", encoding="utf-8")
        missing_notes = self.cli("validate")
        self.assertNotEqual(missing_notes.returncode, 0)
        self.assertIn("Missing changelog entry", missing_notes.stderr)

    def test_invalid_changelog_preserves_all_version_files(self):
        (self.root / "CHANGELOG.md").write_text("Malformed history without a heading.\n", encoding="utf-8")
        original = {name: (self.root / name).read_bytes() for name in release.VERSION_FILES}
        with self.assertRaisesRegex(ValueError, "CHANGELOG"):
            release.update_versions(self.root, "1.3.0", "## 1.3.0\n\n### Features\n\n- New feature\n")
        for name, contents in original.items():
            with self.subTest(file=name):
                self.assertEqual((self.root / name).read_bytes(), contents,
                                 f"invalid changelog partially mutated {name}")

    def test_first_and_subsequent_release_semver(self):
        version, commits = release.plan(self.root)
        self.assertEqual(version, "1.2.3")
        self.assertEqual([commit.kind for commit in commits], ["feat"])
        self.git("tag", "v1.2.3")
        self.assertIsNone(release.plan(self.root)[0])
        self.commit("fix(pdf): retain Unicode glyphs")
        self.assertEqual(release.plan(self.root)[0], "1.2.4")
        self.commit("feat(ocr): add searchable text")
        self.assertEqual(release.plan(self.root)[0], "1.3.0")
        self.commit("feat!: replace the plugin protocol")
        self.assertEqual(release.plan(self.root)[0], "2.0.0")
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('"1.2.3"', '"1.2.4"'), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "differs from latest reachable release"):
            release.plan(self.root)

    def test_breaking_footer_and_explicit_bump_floor(self):
        self.git("tag", "v1.2.3")
        self.commit("feat(ocr): enable a provider")
        with self.assertRaisesRegex(ValueError, "require a minor bump"):
            release.plan(self.root, "patch")
        self.assertEqual(release.plan(self.root, "major")[0], "2.0.0")
        self.commit("refactor(core): revise graph\n\nBREAKING CHANGE: consumers must migrate")
        version, commits = release.plan(self.root)
        self.assertEqual(version, "2.0.0")
        self.assertTrue(commits[-1].breaking)
        self.assertEqual(commits[-1].breaking_note, "consumers must migrate")
        for bump in ("patch", "minor"):
            with self.subTest(bump=bump), self.assertRaisesRegex(ValueError, "require a major bump"):
                release.plan(self.root, bump)
        self.assertTrue(release.parse_commit("fix: new behavior\n\nBREAKING-CHANGE: migrate configuration").breaking)
        parsed = release.parse_commit("fix(pdf): preserve glyphs\n\nDetailed context.", "abc12345")
        self.assertEqual((parsed.kind, parsed.scope, parsed.description, parsed.breaking),
                         ("fix", "pdf", "preserve glyphs", False))

    def test_maintenance_only_and_explicit_bump(self):
        self.git("tag", "v1.2.3")
        self.commit("docs: explain plugin installation")
        self.commit("ci: check release policy")
        version, commits = release.plan(self.root)
        self.assertIsNone(version)
        self.assertEqual(len(commits), 2)
        self.assertEqual(release.plan(self.root, "patch")[0], "1.2.4")
        result = self.cli("plan")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("No releasable changes", result.stdout)

    def test_invalid_semver_shallow_history_and_commit_messages(self):
        manifest = self.root / "Cargo.toml"
        original = manifest.read_text(encoding="utf-8")
        for version in ("01.2.3", "1.2", "1.2.3-alpha.01", "v1.2.3"):
            with self.subTest(version=version):
                manifest.write_text(original.replace('"1.2.3"', f'"{version}"'), encoding="utf-8")
                result = self.cli("validate")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Invalid SemVer", result.stderr)
        manifest.write_text(original.replace('"1.2.3"', '"1.2.3-rc.1"'), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "stable X.Y.Z"):
            release.plan(self.root)
        manifest.write_text(original, encoding="utf-8")
        for message in ("", "made changes", "unknown: feature", "fix: message\nbody lacks blank line"):
            with self.subTest(message=message), self.assertRaises(ValueError):
                release.parse_commit(message)
        self.commit("fix: second commit for shallow clone")
        shallow = self.root / "shallow clone"
        self.git("clone", "--quiet", "--depth", "1", self.root.as_uri(), str(shallow))
        result = self.cli("plan", root=shallow)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires full Git history", result.stderr)
        self.commit("not a Conventional Commit")
        result = self.cli("plan")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid Conventional Commit", result.stderr)

    def test_changelog_preserves_handwritten_history(self):
        changelog = self.root / "CHANGELOG.md"
        original = changelog.read_text(encoding="utf-8")
        old_section = original[original.index("## 1.1.0"):]
        commits = [release.parse_commit("feat(pdf): Unicode support", "1234567890abcdef")]
        release.update_versions(self.root, "1.2.3", release.release_notes("1.2.3", commits))
        first = changelog.read_text(encoding="utf-8")
        self.assertIn("Handwritten release notes — preserve me.", first)
        self.assertIn("### Commit history", first)
        self.assertIn("#### Features", first)
        self.assertTrue(first.endswith(old_section))
        notes = release.notes_for_version(self.root, "1.2.3")
        self.assertIn("Unicode support (12345678)", notes)
        self.assertNotIn("## 1.1.0", notes)
        release.update_versions(self.root, "1.3.0", release.release_notes("1.3.0", commits))
        updated = changelog.read_text(encoding="utf-8")
        self.assertTrue(updated.endswith(first[len("# Changelog\n\n"):]))
        self.assertEqual(release.manifest_version(self.root), "1.3.0")
        self.assertLess(updated.index("## 1.3.0"), updated.index("## 1.2.3"))


if __name__ == "__main__":
    unittest.main()
