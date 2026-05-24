#!/usr/bin/env bash
set -euo pipefail

# Auto-rebuild & restart on source changes (requires cargo-watch).
# Install: cargo install cargo-watch
# Usage: WEB_PASSWORD=secret ./watch.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if ! command -v cargo-watch &>/dev/null; then
    echo "[watch] cargo-watch not found. Installing..."
    cargo install cargo-watch
fi

export WEB_PASSWORD="${WEB_PASSWORD:-changeme}"
echo "[watch] Watching src/ for changes..."
cargo watch -x 'build' -s './target/debug/tspan-server'
