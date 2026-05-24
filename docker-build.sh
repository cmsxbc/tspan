#!/bin/bash
set -euo pipefail

# Fast Docker image build for local testing.
#
# This script builds the binary locally (reusing your ~/.cargo/registry cache)
# and then packages it into a minimal distroless image.
#
# Usage:
#   ./docker-build.sh              # build image as tspan-server:latest
#   ./docker-build.sh mytag        # build image as tspan-server:mytag
#   REGISTRY=ghcr.io/user ./docker-build.sh v1.0.0
#
# Compared to `docker build -f Dockerfile .`, this avoids re-downloading
# crates inside the container every time source files change.

TAG="${1:-latest}"
REGISTRY="${REGISTRY:-}"
IMAGE_NAME="${REGISTRY:+${REGISTRY}/}tspan-server:${TAG}"

echo "==> Building release binary locally..."
cargo build --release

echo "==> Building Docker image ${IMAGE_NAME}..."
# Use --no-cache to ensure the freshly compiled binary is always copied.
# Dockerfile.local has no expensive intermediate steps, so the cost is negligible.
docker build -f Dockerfile.local --no-cache -t "${IMAGE_NAME}" .

echo "==> Done: ${IMAGE_NAME}"
