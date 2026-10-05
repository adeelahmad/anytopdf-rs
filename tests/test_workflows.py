"""Workflow source contracts plus real policy and extracted shell behavior.

These tests do not emulate Actions or claim hosted runner validation. The small
indentation reader handles this repository's block-style jobs/steps; actionlint
remains the workflow syntax validator.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[1]
BASH = shutil.which("bash")
TARGETS = {
    "x86_64-unknown-linux-musl": "tar.gz",
    "aarch64-unknown-linux-musl": "tar.gz",
    "aarch64-apple-darwin": "tar.gz",
    "x86_64-apple-darwin": "tar.gz",
    "x86_64-pc-windows-msvc": "zip",
}


def job_source(source, name):
    match = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:|\Z)",
                      source, re.MULTILINE | re.DOTALL)
    if not match:
        raise AssertionError(f"Missing workflow job {name}")
    return match.group(1)


def step_sources(job):
    return re.findall(r"^      - .*?(?=^      - |\Z)", job, re.MULTILINE | re.DOTALL)


def run_source(step):
    match = re.search(r"^        run: (.*)$", step, re.MULTILINE)
    if not match:
        raise AssertionError(f"Missing run command in step: {step}")
    if match.group(1) in ("|", "|-", "|+"):
        return textwrap.dedent(step[match.end() + 1:])
    return match.group(1).strip("'\"") + "\n"


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.release = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        temporary = tempfile.TemporaryDirectory(prefix="anytopdf workflow ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = os.environ.copy()
        for key in tuple(self.env):
            if key.startswith("GIT_") or key.startswith("GITHUB_"):
                self.env.pop(key)
        self.env.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                         "PYTHON": sys.executable, "PYTHONUTF8": "1"})
        self.git("init", "--quiet")
        self.git("config", "user.name", "Workflow Test")
        self.git("config", "user.email", "workflow@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/release.py", self.root / "scripts/release.py")
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = []\n[workspace.package]\nversion = "1.2.3"\n',
            encoding="utf-8")
        (self.root / "Cargo.lock").write_text("version = 4\n", encoding="utf-8")
        self.notes = "## 1.2.3 - 2026-01-01\n\nHandwritten notes — retained.\n\n### Fixes\n\n- Searchable PDFs\n"
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n" + self.notes + "\n## 1.2.2\n\nOld release only.\n",
            encoding="utf-8")
        self.commit("feat: initial fixture")

    def git(self, *arguments):
        return subprocess.run(["git", *arguments], cwd=self.root, env=self.env,
                              text=True, capture_output=True, check=True).stdout.strip()

    def commit(self, message):
        self.git("add", "--all")
        self.git("commit", "--quiet", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def cli(self, *arguments, event=None):
        environment = self.env.copy()
        if event is not None:
            path = self.root / "event.json"
            path.write_text(json.dumps(event), encoding="utf-8")
            environment["GITHUB_EVENT_PATH"] = str(path)
        return subprocess.run([sys.executable, str(self.root / "scripts/release.py"), *arguments],
                              cwd=self.root, env=environment, capture_output=True,
                              text=True, encoding="utf-8")

    def shell(self, script, cwd, **variables):
        # Select the same Python as the Rust wrapper, including paths with spaces.
        script = re.sub(r"(?<![\w/])python(?:3)?(?=\s)",
                        lambda _: shlex.quote(Path(sys.executable).as_posix()), script)
        environment = {**self.env, **variables}
        return subprocess.run([BASH, "--noprofile", "--norc", "-e", "-o", "pipefail", "-c", script],
                              cwd=cwd, env=environment, text=True, encoding="utf-8",
                              capture_output=True)

    def test_ci_enforces_commit_event_with_full_history(self):
        verify = job_source(self.ci, "verify")
        checkout = next(step for step in step_sources(verify) if "actions/checkout@" in step)
        with self.subTest(contract="complete history"):
            self.assertRegex(checkout, r"fetch-depth:\s*0(?:\s|$)",
                             "CI checkout must fetch full history for push/new-branch policy")
        policies = [step for step in step_sources(verify) if "scripts/release.py check-event" in step]
        with self.subTest(contract="event policy wiring"):
            self.assertEqual(len(policies), 1, "CI must invoke release.py check-event exactly once")
        if policies:
            policy = policies[0]
            self.assertLess(verify.index(policy), verify.index("make ci"))
            condition = re.search(r"^        if: (.+)$", policy, re.MULTILINE)
            self.assertIsNotNone(condition, "Event policy must be guarded for dispatch/reusable invocations")
            self.assertIn("github.event_name", condition.group(1))
            self.assertIn("'push'", condition.group(1))
            self.assertIn("'pull_request'", condition.group(1))
            self.assertNotIn("'workflow_dispatch'", condition.group(1))
            self.assertNotIn("'workflow_call'", condition.group(1))
            # GITHUB_EVENT_PATH is supplied by Actions; overrides must preserve it.
            overrides = re.findall(r"GITHUB_EVENT_PATH:\s*(.+)", policy)
            self.assertTrue(not overrides or overrides == ["${{ github.event_path }}"])
            event = self.root / "wired-event.json"
            event.write_text(json.dumps({"pull_request": {"title": "broken title"}}), encoding="utf-8")
            result = self.shell(run_source(policy), self.root, GITHUB_EVENT_PATH=str(event),
                                GITHUB_EVENT_NAME="pull_request")
            self.assertNotEqual(result.returncode, 0, "The wired policy accepted an invalid PR title")
            self.assertIn("Conventional Commit", result.stderr)
        self.assertIn("workflow_dispatch:", self.ci)
        self.assertIn("workflow_call:", self.ci)

    def test_invalid_pr_push_and_new_branch_events_fail(self):
        valid = self.cli("check-event", event={"pull_request": {"title": "fix(pdf): retain glyphs"}})
        self.assertEqual(valid.returncode, 0, valid.stderr)
        base = self.git("rev-parse", "HEAD")
        good = self.commit("fix: valid push")
        for event in ({"before": base, "after": good}, {"before": "0" * 40, "after": good}):
            result = self.cli("check-event", event=event)
            self.assertEqual(result.returncode, 0, result.stderr)
        bad = self.commit("unstructured change")
        for label, event in (
            ("PR", {"pull_request": {"title": "unstructured title"}}),
            ("push", {"before": good, "after": bad}),
            ("new branch", {"before": "0" * 40, "after": bad}),
        ):
            with self.subTest(event=label):
                result = self.cli("check-event", event=event)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("Conventional Commit", result.stderr)
        missing = self.cli("check-event", event={"before": "a" * 40, "after": good})
        self.assertNotEqual(missing.returncode, 0, "Missing event history must fail closed")

    def test_release_validates_metadata_and_uses_changelog_notes(self):
        valid = self.cli("validate", "--tag", "v1.2.3")
        self.assertEqual(valid.returncode, 0, valid.stderr)
        wrong = self.cli("validate", "--tag", "v9.9.9")
        self.assertNotEqual(wrong.returncode, 0)
        self.assertIn("tag", wrong.stderr)
        notes = self.cli("notes")
        self.assertEqual(notes.returncode, 0, notes.stderr)
        self.assertEqual(notes.stdout, self.notes)
        version = job_source(self.release, "version")
        publish = job_source(self.release, "publish")
        validation_steps = [step for step in step_sources(version)
                            if "scripts/release.py validate" in step]
        if validation_steps:
            validation_script = run_source(validation_steps[0])
            self.assertNotIn("${{", validation_script,
                             "Tag values must arrive through env, not shell expression injection")
            for label, tag, ref, success in (
                ("matching tag", "v1.2.3", "refs/tags/v1.2.3", True),
                ("wrong tag", "v9.9.9", "refs/tags/v9.9.9", False),
                ("manual branch package", "main", "refs/heads/main", True),
            ):
                with self.subTest(validation=label):
                    result = self.shell(validation_script, self.root, RELEASE_TAG=tag,
                                        RELEASE_REF=ref, GITHUB_REF=ref, GITHUB_REF_NAME=tag)
                    self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
                    if not success:
                        self.assertIn("tag", result.stderr.lower())
        with self.subTest(contract="shared metadata validation"):
            self.assertRegex(version, r"scripts/release\.py validate[^\n]*--tag",
                             "Tag builds must use shared Cargo.lock/changelog/tag validation")
        with self.subTest(contract="changelog notes file"):
            self.assertRegex(publish, r"scripts/release\.py notes\s*>",
                             "Publication must generate its body from the exact changelog section")
            self.assertIn("--notes-file", publish)
            self.assertNotIn("--generate-notes", publish)
        note_steps = [step for step in step_sources(publish) if "scripts/release.py notes" in step]
        if note_steps:
            self.assertIn("actions/checkout@", publish, "Notes require the tagged source checkout")
            command = next(line for line in run_source(note_steps[0]).splitlines()
                           if "scripts/release.py notes" in line)
            generated = self.shell(command, self.root)
            self.assertEqual(generated.returncode, 0, generated.stderr)
            destination = shlex.split(command.partition(">")[2].strip())[0]
            self.assertEqual((self.root / destination).read_text(encoding="utf-8"), notes.stdout)
            self.assertRegex(publish, rf"--notes-file\s+['\"]?{re.escape(destination)}(?:['\"]|\s|$)")

    def test_all_five_targets_and_verification_dependencies(self):
        verify = job_source(self.ci, "verify")
        self.assertIn("dtolnay/rust-toolchain@1.92.0", verify)
        self.assertIn("components: rustfmt, clippy", verify)
        self.assertIn("make ci", verify)
        for runner in ("ubuntu-24.04", "macos-14", "windows-2025"):
            self.assertIn(runner, verify)
        package = job_source(self.release, "package")
        targets = re.findall(r"^\s+target:\s*([\w-]+)\s*$", package, re.MULTILINE)
        self.assertEqual(set(targets), set(TARGETS))
        self.assertEqual(len(targets), 5)
        self.assertRegex(job_source(self.release, "verify"), r"needs:\s*version\b")
        self.assertIn("uses: ./.github/workflows/ci.yml", job_source(self.release, "verify"))
        self.assertRegex(package, r"needs:\s*verify\b")
        self.assertIn("make package", package)
        self.assertIn("if-no-files-found: error", package)
        publish = job_source(self.release, "publish")
        self.assertRegex(publish, r"needs:\s*package\b")
        self.assertRegex(publish, r"if:.*github.event_name == 'push'.*refs/tags/v")
        self.assertLess(publish.index("SHA256SUMS"), publish.index("gh release create"))
        self.assertIn("--verify-tag", publish)
        self.assertIn("workflow_dispatch:", self.release)

    def test_publish_rejects_missing_extra_or_corrupt_archives(self):
        publish = job_source(self.release, "publish")
        validation = [step for step in step_sources(publish)
                      if "SHA256SUMS" in step and "gh release create" not in step]
        self.assertTrue(validation, "Publication needs an executable validation step before gh release create")
        step = validation[0]
        script = run_source(step)
        self.assertNotIn("${{", script, "Pass runner values via env so validation can execute independently")
        expected = [f"anytopdf-1.2.3-{target}.{suffix}" for target, suffix in TARGETS.items()]
        for scenario in ("valid", "missing pair", "extra pair", "corrupt", "missing checksum", "unsafe checksum"):
            with self.subTest(inventory=scenario):
                folder = self.root / scenario
                dist = folder / "dist"
                dist.mkdir(parents=True)
                for name in expected:
                    payload = (name + "\n").encode()
                    (dist / name).write_bytes(payload)
                    (dist / (name + ".sha256")).write_text(
                        hashlib.sha256(payload).hexdigest() + "  " + name + "\n", encoding="utf-8")
                if scenario == "missing pair":
                    (dist / expected[0]).unlink()
                    (dist / (expected[0] + ".sha256")).unlink()
                elif scenario == "extra pair":
                    payload = b"unexpected binary"
                    (dist / "unexpected.tar.gz").write_bytes(payload)
                    (dist / "unexpected.tar.gz.sha256").write_text(
                        hashlib.sha256(payload).hexdigest() + "  unexpected.tar.gz\n", encoding="utf-8")
                elif scenario == "corrupt":
                    (dist / expected[0]).write_bytes(b"tampered")
                elif scenario == "missing checksum":
                    (dist / (expected[0] + ".sha256")).unlink()
                elif scenario == "unsafe checksum":
                    (folder / "outside").write_bytes(b"outside")
                    (dist / (expected[0] + ".sha256")).write_text(
                        hashlib.sha256(b"outside").hexdigest() + "  ../outside\n", encoding="utf-8")
                cwd = dist if re.search(r"working-directory:\s*dist\b", step) else folder
                result = self.shell(script, cwd, RELEASE_TAG="v1.2.3", VERSION="1.2.3",
                                    RELEASE_VERSION="1.2.3", GITHUB_REF_NAME="v1.2.3")
                if scenario == "valid":
                    self.assertEqual(result.returncode, 0, result.stderr)
                    lines = (dist / "SHA256SUMS").read_text(encoding="utf-8").splitlines()
                    self.assertEqual(len(lines), 5)
                    self.assertEqual({line.split()[-1] for line in lines}, set(expected))
                else:
                    self.assertNotEqual(result.returncode, 0,
                                        f"Publication validation accepted {scenario}: {result.stdout}")


if __name__ == "__main__":
    unittest.main()
