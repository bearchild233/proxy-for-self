#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend build
export CPR_GIT_SHA="$(git rev-parse HEAD)"
export CPR_BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
export CPR_BUILD_TYPE=source
(cd backend && cargo build --release --locked --bin codex-proxy-rs)
mkdir -p dist/bin dist/frontend/dist
cp backend/target/release/codex-proxy-rs dist/bin/
cp -R frontend/dist/. dist/frontend/dist/
# Build only. Never change a running service or database here.
