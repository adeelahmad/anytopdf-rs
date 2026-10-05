#!/usr/bin/env python3
"""Package a built binary, documentation and licenses with a SHA-256 checksum."""
import argparse
import hashlib
import re
from pathlib import Path
import shutil
import tarfile
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--plugin", type=Path, action="append", default=[],
                        help="runtime plugin executable to ship beside the CLI (repeatable)")
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error(f"binary does not exist: {args.binary}")
    for plugin in args.plugin:
        if not plugin.is_file():
            parser.error(f"plugin does not exist: {plugin}")
        if not re.fullmatch(r"anytopdf-plugin-[a-z0-9-]+(?:\.exe)?", plugin.name):
            parser.error(f"plugin executables must be named anytopdf-plugin-*: {plugin.name}")
    if any(c not in "abcdefghijklmnopqrstuvwxyz0123456789_-" for c in args.target):
        parser.error("invalid target name")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    name = f"anytopdf-{version}-{args.target}"
    args.output.mkdir(parents=True, exist_ok=True)
    windows = "windows" in args.target
    archive = args.output / (name + (".zip" if windows else ".tar.gz"))
    with tempfile.TemporaryDirectory() as temporary:
        folder = Path(temporary) / name
        folder.mkdir()
        shutil.copy2(args.binary, folder / ("anytopdf.exe" if windows else "anytopdf"))
        if args.plugin:
            # Opt-in: plugins are found only once ANYTOPDF_PLUGIN_PATH names this folder.
            (folder / "plugins").mkdir()
            for plugin in args.plugin:
                shutil.copy2(plugin, folder / "plugins" / plugin.name)
        for filename in ["README.md", "ARCHITECTURE.md", "PLUGIN_PROTOCOL.md", "RELEASING.md", "ROADMAP.md", "SECURITY.md", "CHANGELOG.md", "LICENSE-MIT", "LICENSE-APACHE"]:
            shutil.copy2(ROOT / filename, folder / filename)
        if windows:
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
                for item in sorted(folder.rglob("*")):
                    output.write(item, arcname=f"{name}/{item.relative_to(folder).as_posix()}")
        else:
            with tarfile.open(archive, "w:gz") as output:
                output.add(folder, arcname=name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n")
    print(archive)


if __name__ == "__main__":
    main()
