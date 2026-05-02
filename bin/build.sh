#!/usr/bin/env bash
set -e

if command -v cargo &>/dev/null; then
    echo "  Building fledge-plugin-bench (Rust)..."
    cargo build --release --quiet
    cp target/release/fledge-plugin-bench bin/fledge-bench
    echo "  Build complete."
else
    echo "  Cargo not found — using pre-built binary if present."
fi
