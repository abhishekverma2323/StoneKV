#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR"

echo "=== Cargo.toml dependencies ==="
echo

awk '
/^\[dependencies\]$/ { print; in_deps=1; next }
in_deps && /^\[/ { exit }
in_deps { print }
' Cargo.toml

echo
echo "=== cargo tree -e normal ==="
echo

cargo tree -e normal

echo
echo "=== generating deps-proof.txt (human-readable summary + full cargo metadata) ==="
echo

{
    echo "StoneKV dependency proof"
    echo "========================="
    echo
    echo "Quick check (cargo tree -e normal):"
    echo
    cargo tree -e normal
    echo
    echo "Cargo.toml [dependencies] section:"
    echo
    awk '
    /^\[dependencies\]$/ { print; in_deps=1; next }
    in_deps && /^\[/ { exit }
    in_deps { print }
    ' Cargo.toml
    echo
    echo "-------------------------------------------------------------------"
    echo "Full cargo metadata (--no-deps) below, for machine verification."
    echo "The field to check is \"dependencies\":[] on the \"stone\" package."
    echo "-------------------------------------------------------------------"
    echo
    cargo metadata --format-version 1 --no-deps
} > deps-proof.txt

echo "dependency proof written to deps-proof.txt"