#!/usr/bin/env bash
# Build man pages from scdoc sources.
#
# Usage: ./man/build.sh
#
# Requires scdoc (https://git.sr.ht/~sircmpwn/scdoc).
# Generated roff files are written to man/generated/ and should not be
# committed to source control.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="${SCRIPT_DIR}/generated"

if ! command -v scdoc >/dev/null 2>&1; then
    echo "error: scdoc not found. Install it with your package manager:" >&2
    echo "  apt install scdoc    # Debian/Ubuntu" >&2
    echo "  brew install scdoc   # macOS" >&2
    echo "  pacman -S scdoc      # Arch" >&2
    exit 1
fi

mkdir -p "$OUT_DIR"

count=0
for src in "$SCRIPT_DIR"/*.scd; do
    [ -f "$src" ] || continue
    base="$(basename "$src" .scd)"
    scdoc < "$src" > "$OUT_DIR/$base"
    echo "  $base"
    count=$((count + 1))
done

if [ "$count" -eq 0 ]; then
    echo "No .scd files found in $SCRIPT_DIR" >&2
    exit 1
fi

echo "Built $count man page(s) in $OUT_DIR"
