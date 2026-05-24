#!/usr/bin/env bash
set -euo pipefail

# Development helper: build and run the server with auto-restart on source changes.
# Usage:
#   ./dev.sh                    # build & run with default password (changeme)
#   WEB_PASSWORD=secret ./dev.sh # build & run with custom password
#   ./dev.sh --bind 127.0.0.1:3000  # override bind address

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Kill any existing tspan-server process
pkill -f 'tspan-server' 2>/dev/null || true
sleep 0.5

echo "[dev] Building..."
cargo build 2>&1

echo "[dev] Starting server..."
export WEB_PASSWORD="${WEB_PASSWORD:-changeme}"
./target/debug/tspan-server "$@"
