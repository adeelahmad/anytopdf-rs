#!/usr/bin/env python3
"""Conventional Commits, SemVer planning, and checked GitHub release publication."""
import argparse
from dataclasses import dataclass
from datetime import date
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TYPES = "feat fix perf refactor docs style test build ci chore revert".split()
HEADER = re.compile(r"(?P<kind>[a-z]+)(?:\((?P<scope>[^()\r\n]+)\))?(?P<breaking>!)?: (?P<description>\S[^\r\n]*)", re.IGNORECASE)
SEMVER = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?")
VERSION_FILES = ("Cargo.toml", "Cargo.lock", "CHANGELOG.md")


def run(*args, root=ROOT, capture=True, env=None):
    result = subprocess.run(args, cwd=root, check=True, text=True, encoding="utf-8", stdout=subprocess.PIPE if capture else None, env=env)
    return result.stdout.strip() if capture else ""


def git(*args, root=ROOT):
    return run("git", *args, root=root)


def manifest_version(root=ROOT):
    version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    if not SEMVER.fullmatch(version):
        raise ValueError(f"Invalid SemVer in Cargo.toml: {version}")
    return version


@dataclass
class Commit:
    sha: str
    kind: str
    scope: str
    description: str
    breaking: bool
    breaking_note: str = ""


def parse_commit(message, sha=""):
    lines = message.strip().splitlines()
    match = HEADER.fullmatch(lines[0]) if lines else None
    if not match or match['kind'].lower() not in TYPES:
        raise ValueError(f"{sha[:8]} invalid Conventional Commit: {lines[0] if lines else '(empty)'}; use feat(scope): description or fix: description")
    if len(lines) > 1 and lines[1].strip():
        raise ValueError(f"{sha[:8]} commit body must follow a blank line")
    breaking = re.search(r"^BREAKING[ -]CHANGE: (\S.*)", message, re.MULTILINE)
    return Commit(sha, match['kind'].lower(), match['scope'] or "", match['description'].strip(), bool(match['breaking'] or breaking), breaking[1] if breaking else "")


def latest_tag(root=ROOT):
    candidates = []
    for tag in git("tag", "--merged", "HEAD", root=root).splitlines():
        if re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", tag):
            candidates.append((tuple(map(int, tag[1:].split('.'))), tag))
    return max(candidates)[1] if candidates else None


def commits_since(base=None, head="HEAD", root=ROOT):
    revision = f"{base}..{head}" if base else head
    shas = git("rev-list", "--reverse", "--no-merges", revision, root=root).splitlines()
    return [parse_commit(git("show", "-s", "--format=%B", sha, root=root), sha) for sha in shas]


def bump_version(version, bump):
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", version):
        raise ValueError("Automatic releases require a stable X.Y.Z version; prereleases use the documented manual tag path")
    major, minor, patch = map(int, version.split('.'))
    return {"major": f"{major + 1}.0.0", "minor": f"{major}.{minor + 1}.0", "patch": f"{major}.{minor}.{patch + 1}"}[bump]


def plan(root=ROOT, bump="auto"):
    if git("rev-parse", "--is-shallow-repository", root=root) == "true":
        raise ValueError("Release planning requires full Git history; run git fetch --unshallow --tags")
    git("rev-parse", "--verify", "HEAD", root=root)
    current = manifest_version(root)
    base = latest_tag(root)
    commits = commits_since(base, root=root)
    if not commits:
        return None, commits
    automatic = "major" if any(c.breaking for c in commits) else "minor" if any(c.kind == "feat" for c in commits) else "patch" if any(c.kind in ("fix", "perf", "revert") for c in commits) else None
    if base:
        if current != base[1:]:
            raise ValueError(f"Workspace version {current} differs from latest reachable release {base}; let make release update the version")
        chosen = automatic if bump == "auto" else bump
        if chosen is None:
            return None, commits
        # An explicit bump may raise the level, but must not hide a breaking change.
        rank = {None: 0, "patch": 1, "minor": 2, "major": 3}
        if rank[chosen] < rank[automatic]:
            raise ValueError(f"Commits require a {automatic} bump; refusing {chosen}")
        version = bump_version(current, chosen)
    else:
        # Bootstrap the first release at the declared workspace version.
        bump_version(current, "patch")  # Validate stable version syntax.
        version = current if bump == "auto" else bump_version(current, bump)
    return version, commits


def release_notes(version, commits):
    groups = {}
    labels = {"feat": "Features", "fix": "Fixes", "perf": "Performance", "revert": "Reverts"}
    for commit in commits:
        if commit.scope == "release" and commit.kind == "chore":
            continue
        group = "Breaking changes" if commit.breaking else labels.get(commit.kind, "Maintenance")
        label = f"{commit.scope}: " if commit.scope else ""
        note = f" — {commit.breaking_note}" if commit.breaking_note else ""
        groups.setdefault(group, []).append(f"- {label}{commit.description}{note} ({commit.sha[:8]})")
    text = f"## {version} - {date.today().isoformat()}\n"
    for group in ("Breaking changes", "Features", "Fixes", "Performance", "Reverts", "Maintenance"):
        if group in groups:
            text += f"\n### {group}\n\n" + "\n".join(groups[group]) + "\n"
    return text


def update_versions(root, version, notes):
    manifest = root / "Cargo.toml"
    data = manifest.read_text(encoding="utf-8")
    data, count = re.subn(r'(\[workspace\.package\]\s*\n(?:(?!\[)[^\n]*\n)*?version\s*=\s*)"[^"]+"', lambda m: m[1] + f'"{version}"', data, count=1)
    if count != 1:
        raise ValueError("Cannot locate [workspace.package].version")
    changelog = root / "CHANGELOG.md"
    previous = changelog.read_text(encoding="utf-8")
    # Preserve hand-written initial release notes and append generated history there.
    section = re.search(rf"^## {re.escape(version)}(?:\s+-[^\n]*)?\s*$", previous, re.MULTILINE)
    if section:
        end = previous.find("\n## ", section.end())
        position = len(previous) if end < 0 else end
        addition = "\n\n### Commit history\n" + notes.split('\n', 1)[1].replace("\n### ", "\n#### ")
        previous = previous[:position].rstrip() + addition + "\n" + previous[position:].lstrip()
    else:
        heading = re.match(r"# Changelog\s*\n", previous)
        if not heading:
            raise ValueError("CHANGELOG.md must begin with # Changelog")
        previous = "# Changelog\n\n" + notes + "\n" + previous[heading.end():].lstrip()
    manifest.write_text(data, encoding="utf-8")
    changelog.write_text(previous, encoding="utf-8")


def notes_for_version(root, version):
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    match = re.search(rf"^## {re.escape(version)}(?:\s+-[^\n]*)?\s*$", changelog, re.MULTILINE)
    if not match:
        raise ValueError(f"Missing changelog entry for {version}")
    end = changelog.find("\n## ", match.end())
    return changelog[match.start():end if end >= 0 else len(changelog)].strip() + "\n"


def clean_tree(root):
    if git("status", "--porcelain", "--untracked-files=normal", root=root):
        raise ValueError("Commit or stash all changes before release (including untracked files)")


def repository_slug(url):
    match = re.fullmatch(r"(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+?)(?:\.git)?/?", url)
    if not match:
        raise ValueError("Release remote must be an HTTPS or SSH github.com repository URL")
    return match[1]


def preflight(root, remote):
    clean_tree(root)
    branch = git("symbolic-ref", "--quiet", "--short", "HEAD", root=root)
    git("rev-parse", "--verify", "HEAD", root=root)
    push_urls = git("remote", "get-url", "--push", "--all", remote, root=root).splitlines()
    if len(push_urls) != 1:
        raise ValueError("Release requires a single push URL")
    repo = repository_slug(push_urls[0])
    if repository_slug(git("remote", "get-url", remote, root=root)) != repo:
        raise ValueError("Fetch and push repositories must match")
    run("gh", "auth", "status", "--hostname", "github.com", root=root)
    # Prove repository access before modifying any release files.
    run("gh", "repo", "view", repo, "--json", "nameWithOwner", root=root)
    git("var", "GIT_AUTHOR_IDENT", root=root)
    git("fetch", remote, "--tags", root=root)
    return branch, repo


def publish_and_wait(root, remote, branch, repo, version):
    tag = f"v{version}"
    commit = git("rev-parse", f"refs/tags/{tag}^{{commit}}", root=root)
    if commit != git("rev-parse", "HEAD", root=root):
        raise ValueError(f"{tag} must point to HEAD")
    # No force push; either both references update or neither does.
    git("push", "--atomic", remote, f"HEAD:refs/heads/{branch}", f"refs/tags/{tag}:refs/tags/{tag}", root=root)
    print(f"Pushed {tag}; waiting for GitHub Release workflow in {repo}...", flush=True)
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        runs = json.loads(run("gh", "run", "list", "--repo", repo, "--workflow", "release.yml", "--event", "push", "--commit", commit, "--limit", "30", "--json", "databaseId,headBranch,createdAt", root=root))
        matches = [item for item in runs if item['headBranch'] == tag]
        if matches:
            selected = max(matches, key=lambda item: item['createdAt'])
            run("gh", "run", "watch", str(selected['databaseId']), "--repo", repo, "--exit-status", root=root, capture=False)
            result = json.loads(run("gh", "release", "view", tag, "--repo", repo, "--json", "url,isDraft,assets", root=root))
            if result['isDraft'] or len(result['assets']) < 11:
                raise ValueError("GitHub release is not published with all five archives and checksums")
            print(f"Published: {result['url']}")
            return
        time.sleep(5)
    raise ValueError(f"Release workflow did not appear for {tag}; check Actions in {repo}. Then run make release-resume")


def publish(root=ROOT, bump="auto", remote="origin", resume=False):
    branch, repo = preflight(root, remote)
    if resume:
        version = manifest_version(root)
        publish_and_wait(root, remote, branch, repo, version)
        return
    version, commits = plan(root, bump)
    if version is None:
        print("No releasable changes. Use BUMP=patch to release maintenance commits.")
        return
    if git("tag", "--list", f"v{version}", root=root):
        raise ValueError(f"Tag v{version} already exists; use make release-resume for a pending release")
    print(release_notes(version, commits), flush=True)
    cargo = os.environ.get("CARGO", "cargo")
    # Fetch before changing the manifest, so lockfile updating can remain offline.
    run(cargo, "fetch", "--locked", root=root, capture=False)
    original = {name: (root / name).read_bytes() for name in VERSION_FILES}
    before = git("rev-parse", "HEAD", root=root)
    try:
        update_versions(root, version, release_notes(version, commits))
        run(cargo, "update", "--workspace", "--offline", root=root, capture=False)
        # No caller MAKEFLAGS: release must not inherit dry-run, ignore-errors, or parallel mode.
        env = os.environ.copy()
        for key in ("MAKEFLAGS", "MFLAGS", "MAKELEVEL", "MAKEOVERRIDES"):
            env.pop(key, None)
        env["PYTHON"] = sys.executable
        run("make", "ci", "package", root=root, capture=False, env=env)
        changed = set(git("diff", "--name-only", root=root).splitlines())
        staged = set(git("diff", "--cached", "--name-only", root=root).splitlines())
        untracked = git("ls-files", "--others", "--exclude-standard", root=root)
        if changed - set(VERSION_FILES) or staged or untracked:
            raise ValueError("Verification changed files outside release metadata; inspect the working tree")
        git("add", "--", *VERSION_FILES, root=root)
        git("commit", "-m", f"chore(release): {version}", root=root)
    except BaseException:
        if git("rev-parse", "HEAD", root=root) == before:
            # The tree was clean at entry; restore only files owned by this operation.
            git("reset", "--quiet", "HEAD", "--", *VERSION_FILES, root=root)
            for name, contents in original.items():
                (root / name).write_bytes(contents)
        raise
    git("tag", "-a", f"v{version}", "-m", f"Release {version}", root=root)
    try:
        publish_and_wait(root, remote, branch, repo, version)
    except (subprocess.CalledProcessError, ValueError):
        print(f"Release commit/tag v{version} retained. Fix the push or rerun failed Actions, then run make release-resume. No rollback of published history was attempted.", file=sys.stderr)
        raise


def check_event(root=ROOT):
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text(encoding="utf-8"))
    if 'pull_request' in event:
        parse_commit(event['pull_request']['title'])
        print("Pull request title is a Conventional Commit; squash-merge with this title.")
    else:
        before = event.get('before', '')
        base = before if before and set(before) != {'0'} else None
        commits = commits_since(base, event.get('after') or "HEAD", root)
        print(f"Checked {len(commits)} Conventional Commits")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["plan", "publish", "resume", "check", "check-event", "validate", "notes"])
    parser.add_argument("--bump", choices=["auto", "patch", "minor", "major"], default="auto")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--tag")
    args = parser.parse_args()
    if args.command == "check-event":
        check_event()
    elif args.command == "check":
        commits = commits_since(latest_tag())
        print(f"Checked {len(commits)} Conventional Commits")
    elif args.command == "validate":
        version = manifest_version()
        manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        workspace = manifest["workspace"]
        excluded = {path.resolve() for pattern in workspace.get("exclude", []) for path in ROOT.glob(pattern)}
        members = {path.resolve() for pattern in workspace.get("members", []) for path in ROOT.glob(pattern)}
        if "package" in manifest:
            members.add(ROOT)
        locked = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8")).get("package", [])
        for member in sorted(members - excluded):
            package = tomllib.loads((member / "Cargo.toml").read_text(encoding="utf-8"))["package"]
            expected = package["version"]
            if isinstance(expected, dict) and expected.get("workspace") is True:
                expected = version
            # Registry/git dependencies may share a workspace package's name.
            entries = [entry for entry in locked if entry["name"] == package["name"] and "source" not in entry]
            if len(entries) != 1 or entries[0]["version"] != expected:
                raise ValueError(f"Cargo.lock must contain local {package['name']} at version {expected}")
        if args.tag and args.tag != f"v{version}":
            raise ValueError(f"Release tag must match Cargo.toml: v{version}")
        notes_for_version(ROOT, version)
        print(f"Validated anytopdf {version}")
    elif args.command == "notes":
        print(notes_for_version(ROOT, manifest_version()), end="")
    elif args.command == "plan":
        version, commits = plan(bump=args.bump)
        print(release_notes(version, commits) if version else "No releasable changes.")
    else:
        publish(bump=args.bump, remote=args.remote, resume=args.command == "resume")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(f"Release error: {error}")
