#!/usr/bin/env bash
# Legacy entry point now runs assertions against isolated test data, not localhost accounts.
set -euo pipefail
cd -- "$(dirname -- "$0")"
cargo build --no-default-features --features local
node tests/session-local.cjs
node tests/session-worker.cjs
