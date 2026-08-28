#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR"

DEMO_DIR="$ROOT_DIR/stone-crash-demo"

rm -rf "$DEMO_DIR"

cargo build --release

BIN="$ROOT_DIR/target/release/stone"

if [[ "${OS:-}" == "Windows_NT" ]]; then
    BIN="$ROOT_DIR/target/release/stone.exe"
fi

echo "=== Create valid WAL records ==="

"$BIN" set alpha one --dir "$DEMO_DIR"
"$BIN" set beta two --dir "$DEMO_DIR"

echo
echo "=== Verify before damage ==="

"$BIN" verify --dir "$DEMO_DIR"

WAL="$DEMO_DIR/wal.log"

echo
echo "=== Append incomplete crash-tail bytes ==="

printf '\x00\x10\x00' >> "$WAL"

echo
echo "WAL size before recovery:"

wc -c "$WAL"

echo
echo "=== Reopen Stone ==="

"$BIN" get alpha --dir "$DEMO_DIR"
"$BIN" get beta --dir "$DEMO_DIR"

echo
echo "=== WAL after automatic tail recovery ==="

wc -c "$WAL"

echo
echo "=== Final verification ==="

"$BIN" verify --dir "$DEMO_DIR"

echo
echo "Crash-tail recovery demo completed."