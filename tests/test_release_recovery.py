"""Release recovery contracts: real Git history, isolated external service seams."""
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout, redirect_stderr
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("release_recovery_subject", ROOT / "scripts/release.py")
release = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = release
SPEC.loader.exec_module(release)
TARGETS = ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
           "x86_64-apple-darwin", "aarch64-apple-darwin", "x86_64-pc-windows-msvc")


def assets(version="1.2.3"):
    names = []
    for target in TARGETS:
        archive = f"anytopdf-{version}-{target}" + (".zip" if "windows" in target else ".tar.gz")
        names.extend((archive, archive + ".sha256"))
    return [{"name": name} for name in [*names, "SHA256SUMS"]]


class ReleaseRecoveryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="anytopdf release recovery ")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "working tree"
        self.root.mkdir()
        self.remote = self.base / "remote.git"
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        self.git("init", "--quiet", "--initial-branch=main")
        self.git("init", "--quiet", "--bare", "--initial-branch=main", str(self.remote))
        self.git("config", "user.name", "Release Recovery Test")
        self.git("config", "user.email", "recovery@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "tag.gpgsign", "false")
        self.git("remote", "add", "origin", "https://github.com/example/anytopdf-rs.git")
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/anytopdf"]\n\n'
            '[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
        crate = self.root / "crates/anytopdf"
        crate.mkdir(parents=True)
        (crate / "Cargo.toml").write_text('[package]\nname = "anytopdf"\nversion.workspace = true\n', encoding="utf-8")
        (self.root / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "anytopdf"\nversion = "1.2.3"\n', encoding="utf-8")
        (self.root / "CHANGELOG.md").write_text(
            '# Changelog\n\n## 1.2.3 - 2026-01-01\n\nHandwritten notes — retain.\n', encoding="utf-8")
        self.commit("feat: initial converter")
        self.git("push", str(self.remote), "HEAD:refs/heads/main")
        self.calls = []
        self.push_error = self.watch_error = self.verify_error = None
        self.verify_effect = None
        self.now = 0
        self.run_responses = None
        self.result = {"tagName": "v1.2.3", "isDraft": False, "isPrerelease": False,
                       "url": "https://github.com/example/anytopdf-rs/releases/tag/v1.2.3", "assets": assets()}
        self.addCleanup(patch.stopall)
        patch.object(release, "run", side_effect=self.command).start()
        patch.object(release.time, "monotonic", side_effect=lambda: self.now).start()
        patch.object(release.time, "sleep", side_effect=self.sleep).start()
        patch.dict(os.environ, {"CARGO": "cargo"}).start()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, env=self.env, check=True,
                              text=True, encoding="utf-8", capture_output=True).stdout.strip()

    def commit(self, message):
        self.git("add", "--all")
        self.git("commit", "--quiet", "--allow-empty", "-m", message)

    def tag(self):
        self.git("tag", "-a", "v1.2.3", "-m", "Release 1.2.3")

    def sleep(self, seconds):
        self.now += seconds

    def matching_run(self):
        return {"databaseId": 101, "headBranch": "v1.2.3", "headSha": self.git("rev-parse", "HEAD"),
                "createdAt": "2026-01-01T01:00:00Z", "status": "completed", "conclusion": "success"}

    def command(self, *args, root=None, capture=True, env=None):
        self.calls.append(args)
        if args[0] == "git":
            if args[1] == "fetch":
                return self.git("fetch", str(self.remote), *args[3:])
            if args[1] == "push":
                if self.push_error:
                    raise self.push_error
                self.assertIn("--atomic", args)
                self.assertFalse(any("force" in arg or arg.startswith("+") for arg in args))
                return self.git("push", "--atomic", str(self.remote), *args[4:])
            return self.git(*args[1:])
        if args[:3] == ("gh", "auth", "status"):
            return "authenticated"
        if args[:3] == ("gh", "repo", "view"):
            return json.dumps({"nameWithOwner": "example/anytopdf-rs"})
        if args[:3] == ("gh", "run", "list"):
            self.assertIn("release.yml", args)
            self.assertEqual(args[args.index("--commit") + 1], self.git("rev-parse", "HEAD"))
            self.assertEqual(args[args.index("--event") + 1], "push")
            response = self.run_responses.pop(0) if self.run_responses else [self.matching_run()]
            if isinstance(response, Exception):
                raise response
            return json.dumps(response)
        if args[:3] == ("gh", "run", "watch"):
            if self.watch_error:
                raise self.watch_error
            self.assertIn("--exit-status", args)
            return ""
        if args[:3] == ("gh", "release", "view"):
            self.assertEqual(args[3], "v1.2.3")
            return json.dumps(self.result)
        if args[:2] == ("cargo", "fetch"):
            self.assertIn("--locked", args)
            return ""
        if args[:2] == ("cargo", "update"):
            self.assertIn("--offline", args)
            lock = self.root / "Cargo.lock"
            lock.write_text(lock.read_text(encoding="utf-8").replace('version = "1.2.2"', 'version = "1.2.3"'), encoding="utf-8")
            return ""
        if args[:3] == ("make", "ci", "package"):
            self.assertFalse(self.git("tag", "--list", "v1.2.3"), "verification must precede tag creation")
            self.assertFalse(self.external("git", "push"), "verification must precede publication")
            self.assertEqual(env["PYTHON"], sys.executable)
            self.assertTrue(all(key not in env for key in ("MAKEFLAGS", "MFLAGS", "MAKELEVEL", "MAKEOVERRIDES")))
            if self.verify_effect:
                self.verify_effect()
            if self.verify_error:
                raise self.verify_error
            return ""
        self.fail(f"Unexpected external command: {args!r}")

    def external(self, *prefix):
        return [args for args in self.calls if args[:len(prefix)] == prefix]

    def invoke(self, resume=False, direct=False):
        output = io.StringIO()
        error = None
        with redirect_stdout(output), redirect_stderr(output):
            try:
                if direct:
                    release.publish_and_wait(self.root, "origin", "main", "example/anytopdf-rs", "1.2.3")
                else:
                    release.publish(self.root, resume=resume)
            except (ValueError, OSError, subprocess.CalledProcessError) as exc:
                error = exc
        return error, output.getvalue()

    def assert_rejected(self, direct=False, resume=False):
        error, output = self.invoke(resume=resume, direct=direct)
        self.assertIsNotNone(error, f"unsafe release accepted: {output}")
        self.assertNotIn("Published:", output)
        return error

    def test_wrong_eleven_assets_never_count_as_published(self):
        self.tag()
        variants = {"arbitrary_eleven": [{"name": f"wrong-{i}"} for i in range(11)],
                    "old_version": assets("1.2.2"), "duplicate": [*assets()[:-1], assets()[0]],
                    "missing_checksum": assets()[:-1], "unexpected_extra": [*assets(), {"name": "wrong.zip"}]}
        for label, inventory in variants.items():
            with self.subTest(inventory=label):
                self.result["assets"] = inventory
                self.assert_rejected(direct=True)

    def test_exact_assets_published_state_and_matching_run(self):
        self.tag()
        expected = self.matching_run()
        wrong_branch = dict(expected, databaseId=202, headBranch="main", createdAt="2026-02-01T00:00:00Z")
        self.run_responses = [[wrong_branch, expected]]
        error, output = self.invoke(direct=True)
        self.assertIsNone(error, repr(error))
        self.assertIn("Published:", output)
        self.assertEqual(self.external("gh", "run", "watch")[-1][3], "101")
        original = copy.deepcopy(self.result)
        for field, value in (("isDraft", True), ("isPrerelease", True), ("tagName", "v0.0.1")):
            with self.subTest(field=field):
                self.result = dict(original, **{field: value})
                self.assert_rejected(direct=True)
        self.result = original
        self.calls.clear()
        wrong_sha = dict(expected, databaseId=303, headSha="0" * 40, createdAt="2026-03-01T00:00:00Z")
        self.run_responses = [[expected, wrong_sha]]
        error, output = self.invoke(direct=True)
        self.assertIsNone(error, repr(error))
        self.assertEqual(self.external("gh", "run", "watch")[-1][3], "101", "must select matching tag AND commit")

    def test_resume_release_commit_before_tag(self):
        self.commit("chore(release): 1.2.3")
        before = self.git("rev-parse", "HEAD")
        self.verify_error = subprocess.CalledProcessError(2, ["make", "ci", "package"])
        self.assert_rejected(resume=True)
        self.assertEqual(self.git("rev-parse", "HEAD"), before)
        self.assertFalse(self.git("tag", "--list", "v1.2.3"))
        self.assertFalse(self.external("git", "push"))
        self.assertFalse(self.git("status", "--porcelain"))
        self.verify_error = None
        self.calls.clear()
        error, output = self.invoke(resume=True)
        self.assertIsNone(error, f"clean interrupted release must resume after verification: {error}; {output}")
        self.assertTrue(self.external("make", "ci", "package"))
        self.assertEqual(self.git("rev-parse", "HEAD"), before)
        self.assertEqual(self.git("cat-file", "-t", "refs/tags/v1.2.3"), "tag")
        self.assertEqual(self.git("rev-parse", "v1.2.3^{commit}"), before)
        self.assertEqual(self.git("--git-dir", str(self.remote), "rev-parse", "refs/tags/v1.2.3^{commit}"), before)
        self.assertIn("Published:", output)

    def test_resume_already_published_and_wrong_head(self):
        self.commit("chore(release): 1.2.3")
        self.tag()
        before = self.git("rev-parse", "HEAD")
        error, output = self.invoke(resume=True)
        self.assertIsNone(error, repr(error))
        self.assertIn("Published:", output)
        self.assertEqual(self.git("rev-parse", "HEAD"), before)
        self.assertFalse(self.external("make"))
        self.commit("fix: work after release")
        self.calls.clear()
        self.assert_rejected(resume=True)
        self.assertFalse(self.external("git", "push"))
        self.git("tag", "-d", "v1.2.3")
        self.git("--git-dir", str(self.remote), "tag", "-d", "v1.2.3")
        self.calls.clear()
        self.assert_rejected(resume=True)
        self.assertFalse(self.git("tag", "--list", "v1.2.3"))
        self.assertFalse(self.external("git", "push"))
        original = {name: (self.root / name).read_bytes() for name in release.VERSION_FILES}
        for filename, invalid in (("Cargo.lock", 'version = 4\n[[package]]\nname = "anytopdf"\nversion = "1.2.2"\n'),
                                  ("CHANGELOG.md", "# Changelog\n\n## 1.2.2\n\nOld notes.\n")):
            with self.subTest(inconsistent_metadata=filename):
                for name, contents in original.items():
                    (self.root / name).write_bytes(contents)
                (self.root / filename).write_text(invalid, encoding="utf-8")
                self.commit("chore(release): 1.2.3")
                self.calls.clear()
                self.assert_rejected(resume=True)
                self.assertFalse(self.external("git", "push"))
                self.assertFalse(self.git("tag", "--list", "v1.2.3"))

    def test_preflight_dirty_and_mismatched_remote(self):
        original = self.git("rev-parse", "HEAD")
        (self.root / "untracked.txt").write_text("keep me", encoding="utf-8")
        self.assert_rejected()
        self.assertFalse(self.external("gh"))
        (self.root / "untracked.txt").unlink()
        self.git("remote", "set-url", "--push", "origin", "https://github.com/wrong/repository.git")
        self.assert_rejected()
        self.assertFalse(self.external("gh"))
        self.assertEqual(self.git("rev-parse", "HEAD"), original)
        self.assertFalse(self.git("tag", "--list"))

    def test_verification_precedes_tag_and_push_and_restores_failure(self):
        original = {name: (self.root / name).read_bytes() for name in release.VERSION_FILES}
        before = self.git("rev-parse", "HEAD")
        self.verify_error = subprocess.CalledProcessError(2, ["make", "ci", "package"])
        self.verify_effect = lambda: (self.root / "verification-note.txt").write_text("retain unrelated output", encoding="utf-8")
        with patch.dict(os.environ, {"MAKEFLAGS": "-n -i -j8", "MFLAGS": "-n", "MAKELEVEL": "1", "MAKEOVERRIDES": "X=Y"}):
            self.assert_rejected()
        self.assertTrue(self.external("make", "ci", "package"))
        self.assertFalse(self.external("git", "push"))
        self.assertFalse(self.git("tag", "--list"))
        self.assertEqual(self.git("rev-parse", "HEAD"), before)
        for name, contents in original.items():
            self.assertEqual((self.root / name).read_bytes(), contents)
        self.assertEqual((self.root / "verification-note.txt").read_text(encoding="utf-8"), "retain unrelated output")
        self.assertFalse(self.git("diff", "--cached", "--name-only"))

    def test_push_workflow_failure_and_timeout_preserve_recovery_state(self):
        before = self.git("rev-parse", "HEAD")
        self.push_error = subprocess.CalledProcessError(1, ["git", "push"], stderr="remote unavailable")
        self.assert_rejected()
        head = self.git("rev-parse", "HEAD")
        self.assertNotEqual(head, before)
        self.assertEqual(self.git("log", "-1", "--format=%s"), "chore(release): 1.2.3")
        self.assertEqual(self.git("rev-parse", "v1.2.3^{commit}"), head)
        self.assertFalse(self.git("status", "--porcelain"))
        self.push_error = None
        self.watch_error = subprocess.CalledProcessError(1, ["gh", "run", "watch"], stderr="workflow failed")
        self.assert_rejected(resume=True)
        self.watch_error = None
        self.run_responses = [[] for _ in range(40)]
        error = self.assert_rejected(resume=True)
        self.assertIn("release-resume", str(error))
        self.assertGreaterEqual(self.now, 180)
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual(self.git("rev-parse", "v1.2.3^{commit}"), head)
        permanent = subprocess.CalledProcessError(1, ["gh", "run", "list"], stderr="HTTP 403: Resource not accessible by integration")
        self.calls.clear()
        self.run_responses = [permanent]
        self.assertIs(self.assert_rejected(resume=True), permanent)
        self.assertEqual(len(self.external("gh", "run", "list")), 1)
        transient = subprocess.CalledProcessError(1, ["gh", "run", "list"], stderr="HTTP 404: workflow release.yml not found on the default branch")
        self.calls.clear()
        self.run_responses = [transient, [], [self.matching_run()]]
        error, output = self.invoke(resume=True)
        self.assertIsNone(error, f"first-push workflow indexing must retry 404 and empty list: {error}; {output}")
        self.assertEqual(len(self.external("gh", "run", "list")), 3)
        self.assertIn("Published:", output)
        self.assertEqual(self.git("rev-parse", "HEAD"), head)


if __name__ == "__main__":
    unittest.main()
