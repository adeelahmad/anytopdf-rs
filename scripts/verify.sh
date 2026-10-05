#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/.."
if [ ! -f Cargo.lock ]; then
    printf '%s\n' 'Cargo.lock is missing. Run cargo generate-lockfile with network access and commit it before release.' >&2
    exit 1
fi
CARGO=${CARGO:-cargo}
PYTHON=${PYTHON:-python3}
"$CARGO" fmt --all -- --check
"$CARGO" check --workspace --locked
"$CARGO" clippy --workspace --all-targets --locked -- -D warnings
"$CARGO" clippy -p anytopdf --all-targets --features imap --locked -- -D warnings
"$CARGO" test --workspace --locked
"$CARGO" test --workspace --no-default-features --locked
"$CARGO" test -p anytopdf --features imap --locked
"$PYTHON" -m unittest discover -s tests -p 'test_*.py'
