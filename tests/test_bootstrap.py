"""Bootstrap subprocess contracts; all package/install operations are local fakes."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
BASH = shutil.which("bash")


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.host_env = os.environ.copy()
        for name in ("BASH_ENV", "ENV"):
            self.host_env.pop(name, None)
        self.shell_bash = subprocess.check_output(
            [BASH, "-c", 'printf "%s" "$BASH"'],
            env=self.host_env, text=True, timeout=15,
        ).strip()
        self.temp = tempfile.TemporaryDirectory(prefix="anytopdf bootstrap ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.root / "scripts").mkdir()
        for name in ("bootstrap.sh", "python.sh"):
            (self.root / "scripts" / name).write_text(
                (ROOT / "scripts" / name).read_text(), newline="\n",
            )
        shutil.copy2(ROOT / "rust-toolchain.toml", self.root / "rust-toolchain.toml")
        self.log = self.root / "commands.log"
        self.log.touch()
        self.env = os.environ.copy()
        for name in ("PYTHON", "TARGET", "BASH_ENV", "ENV"):
            self.env.pop(name, None)
        self.env.update(PATH=self.shell_path(self.bin),
                        CARGO_HOME=self.shell_path(self.root / "cargo"),
                        FIXTURE_BIN=self.shell_path(self.bin),
                        FIXTURE_ROOT=self.shell_path(self.root),
                        FIXTURE_LOG=self.shell_path(self.log), FIXTURE_PLATFORM="Linux",
                        INSTALL_EXIT="0", TOOLCHAIN_READY="1")
        # bootstrap appends absolute host PATH directories. Restrict discovery to
        # fixture executables so host packages cannot mask a missing prerequisite.
        # This is a process seam, not a replacement for any bootstrap behavior.
        env_file = self.root / "shell-env"
        env_file.write_text('''command() {
    if [[ "$1" == -v ]]; then
        PATH="$FIXTURE_BIN" builtin command "$@"
    else
        builtin command "$@"
    fi
}
''', newline="\n")
        self.env["BASH_ENV"] = self.shell_path(env_file)
        for name in ("dirname", "sed", "grep"):
            source = subprocess.check_output(
                [BASH, "-c", 'command -v "$1"', "--", name],
                env=self.host_env, text=True, timeout=15,
            ).strip()
            self.assertTrue(source, name)
            self.write_tool(name, 'exec ' + self.quote(source) + ' "$@"\n')
        self.write_tool("bash", 'exec ' + self.quote(self.shell_bash) + ' "$@"\n')
        self.write_tool("uname", 'printf "%s\\n" "$FIXTURE_PLATFORM"\n')
        self.write_tool("id", 'printf "0\\n"\n')
        for name in ("cc", "git", "python3"):
            self.write_tool(name, "exit 0\n")
        self.write_tool("curl", 'echo "Unexpected network request" >&2; exit 99\n')
        self.write_tool("apt-get", 'exit "$INSTALL_EXIT"\n')
        self.write_tool("rustup", '''case "$*" in
    "run "*) [[ "$TOOLCHAIN_READY" == 1 || -f "$FIXTURE_ROOT/toolchain" ]];;
    "toolchain install "*) : > "$FIXTURE_ROOT/toolchain";;
    "component list "*)
        components=()
        [[ ! -f "$FIXTURE_ROOT/rustfmt" ]] || components+=(rustfmt-fixture)
        [[ ! -f "$FIXTURE_ROOT/clippy" ]] || components+=(clippy-fixture)
        printf '%s\\n' "${components[@]}"
        exit 0;;
    "component add "*) for last; do :; done; : > "$FIXTURE_ROOT/$last";;
    "target add "*) exit 0;;
    *) echo "Unexpected rustup command: $*" >&2; exit 98;;
esac
''')
        for component in ("rustfmt", "clippy"):
            (self.root / component).touch()

    def shell_path(self, path):
        # Native Python paths must be converted before Git Bash consumes them.
        # Keep the native BASH launcher path for Python's CreateProcess calls.
        if os.name == "nt":
            return subprocess.check_output(
                [BASH, "-c", 'cygpath -u "$1"', "--", str(path)],
                env=self.host_env, text=True, timeout=15,
            ).strip()
        return str(path)

    @staticmethod
    def quote(value):
        import shlex
        return shlex.quote(str(value))

    def write_tool(self, name, body):
        path = self.bin / name
        path.write_text(f'#!{self.shell_bash}\nprintf "%s\\n" "{name} $*" >> "$FIXTURE_LOG"\n' + body, newline="\n")
        path.chmod(0o755)
        return path

    def run_script(self, name="bootstrap.sh", *args):
        return subprocess.run([BASH, self.shell_path(self.root / "scripts" / name), *args],
                              cwd=self.root, env=self.env, text=True,
                              capture_output=True, timeout=15)

    def assert_ready(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Dependencies ready:", result.stdout)

    def assert_not_ready(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("Dependencies ready:", result.stdout + result.stderr)

    def test_unsupported_platform_fails_even_when_tools_exist(self):
        self.env["FIXTURE_PLATFORM"] = "UnsupportedOS"
        result = self.run_script()
        self.assert_not_ready(result)
        self.assertIn("Unsupported", result.stderr)

    def test_ineffective_install_never_reports_ready(self):
        # apt-get deliberately succeeds without creating the required compiler.
        (self.bin / "cc").unlink()
        result = self.run_script()
        self.assertIn("apt-get install -y build-essential", self.log.read_text())
        self.assert_not_ready(result)

    def test_explicit_python_path_with_spaces_and_invalid_override(self):
        interpreter = self.shell_path(self.write_tool("Python with spaces", "exit 0\n"))
        self.env["PYTHON"] = str(interpreter)
        result = self.run_script("python.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(interpreter))
        self.assert_ready(self.run_script())
        for override in (self.shell_path(self.root / "missing python"), str(interpreter)):
            with self.subTest(override=override):
                self.write_tool("Python with spaces", "exit 1\n")
                self.env["PYTHON"] = override
                result = self.run_script("python.sh")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("must be Python 3.11 or newer", result.stderr)
                self.assert_not_ready(self.run_script())
        self.assertNotIn("apt-get", self.log.read_text())

    def test_missing_candidate_falls_back(self):
        (self.bin / "python3").unlink()
        fallback = self.shell_path(self.write_tool("python", "exit 0\n"))
        result = self.run_script("python.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(fallback))
        self.write_tool("python3", "exit 1\n")
        result = self.run_script("python.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(fallback))

    def test_install_failure_propagates(self):
        (self.bin / "cc").unlink()
        self.env["INSTALL_EXIT"] = "23"
        result = self.run_script()
        self.assert_not_ready(result)
        self.assertEqual(result.returncode, 23)
        self.assertIn("apt-get update", self.log.read_text())
        self.assertNotIn("rustup", self.log.read_text())

    def test_toolchain_components_target_and_idempotence(self):
        self.env["TOOLCHAIN_READY"] = "0"
        self.env["TARGET"] = "aarch64-unknown-linux-gnu"
        for component in ("rustfmt", "clippy"):
            (self.root / component).unlink()
        self.assert_ready(self.run_script())
        first = self.log.read_text()
        self.assertIn("rustup toolchain install 1.92.0 --profile minimal --component rustfmt --component clippy", first)
        for component in ("rustfmt", "clippy"):
            self.assertIn(f"rustup component add --toolchain 1.92.0 {component}", first)
        self.assertIn("rustup target add --toolchain 1.92.0 aarch64-unknown-linux-gnu", first)
        self.log.write_text("")
        self.assert_ready(self.run_script())
        second = self.log.read_text()
        self.assertNotIn("toolchain install", second)
        self.assertNotIn("component add", second)
        self.assertNotIn("apt-get", second)

    def test_unknown_mode_and_provider_separation(self):
        result = self.run_script("bootstrap.sh", "unknown")
        self.assert_not_ready(result)
        self.assertIn("Unknown bootstrap mode", result.stderr)
        self.assert_ready(self.run_script("bootstrap.sh", "build"))
        self.assertNotIn("apt-get", self.log.read_text())
        # Install fixtures publish the executable as a real package manager would.
        self.write_tool("apt-get", '''if [[ "$1" == install ]]; then
    case "$*" in
        *libimage-exiftool-perl*) tool=exiftool;;
        *tesseract-ocr*) tool=tesseract;;
        *poppler-utils*) tool=pdftotext;;
        *ffmpeg*) tool=ffmpeg;;
        *" gh") tool=gh;;
        *) exit 0;;
    esac
    printf '#!%s\\nexit 0\\n' "$BASH" > "$FIXTURE_BIN/$tool"
    /bin/chmod +x "$FIXTURE_BIN/$tool"
fi
''')
        self.assert_ready(self.run_script("bootstrap.sh", "providers"))
        log = self.log.read_text()
        for package in ("ffmpeg", "libimage-exiftool-perl", "tesseract-ocr", "poppler-utils", "fonts-dejavu-core"):
            self.assertIn(f"apt-get install -y {package}", log)
        self.assertNotIn("apt-get install -y gh", log)
        self.assert_ready(self.run_script("bootstrap.sh", "release"))
        self.assertIn("apt-get install -y gh", self.log.read_text())


if __name__ == "__main__":
    unittest.main()
