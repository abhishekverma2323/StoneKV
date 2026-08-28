#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR"

hash_binary() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "No SHA-256 utility found." >&2
        exit 1
    fi
}

BINARY="target/release/stone"

if [[ "${OS:-}" == "Windows_NT" ]]; then
    BINARY="target/release/stone.exe"
fi

echo "=== First clean release build ==="

cargo clean
cargo build --release

HASH_ONE="$(hash_binary "$BINARY")"

echo "first hash:  $HASH_ONE"

echo
echo "=== Second clean release build ==="

cargo clean
cargo build --release

HASH_TWO="$(hash_binary "$BINARY")"

echo "second hash: $HASH_TWO"

echo

if [[ "$HASH_ONE" == "$HASH_TWO" ]]; then
    echo "PASS: release binaries are identical in this environment."
else
    echo "FAIL: release binaries differ."
    exit 1
fi