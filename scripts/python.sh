#!/usr/bin/env bash
# Select a Python with the standard-library TOML parser used by our tooling.
set -eu
if [[ -n "${PYTHON:-}" ]]; then
    "$PYTHON" -c 'import sys; assert sys.version_info >= (3, 11)' >/dev/null 2>&1 || {
        echo "PYTHON=$PYTHON must be Python 3.11 or newer" >&2; exit 1;
    }
    printf '%s\n' "$PYTHON"
    exit 0
fi
for candidate in python3 python /opt/homebrew/bin/python3 /usr/local/bin/python3; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c 'import sys; assert sys.version_info >= (3, 11)' >/dev/null 2>&1; then
        command -v "$candidate"
        exit 0
    fi
done
echo 'Python 3.11+ is required; run make deps.' >&2
exit 1
