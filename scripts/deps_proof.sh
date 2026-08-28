#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR"

echo "=== Cargo.toml dependencies ==="
echo

grep -A 3 '^\[dependencies\]' Cargo.toml || true

echo
echo "=== cargo tree -e normal ==="
echo

cargo tree -e normal

echo
echo "=== generating deps-proof.txt ==="
echo

cargo metadata --format-version 1 --no-deps > deps-proof.txt

echo "dependency proof written to deps-proof.txt"