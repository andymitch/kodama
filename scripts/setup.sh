#!/usr/bin/env bash
set -euo pipefail

# Yurei development setup script
# Handles crypto dependency pinning required for iroh compatibility

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_ROOT"

echo "=== Yurei Development Setup ==="
echo ""

# Check for Rust
if ! command -v cargo &> /dev/null; then
    echo "ERROR: Rust/Cargo not found. Install from https://rustup.rs"
    exit 1
fi

echo "Rust version: $(rustc --version)"
echo ""

# Initial cargo fetch to populate Cargo.lock if needed
if [ ! -f "Cargo.lock" ]; then
    echo "Fetching dependencies..."
    cargo fetch
fi

# Pin crypto dependencies for iroh compatibility
# See: https://github.com/n0-computer/iroh/issues/XXXX
echo "Pinning crypto dependencies for iroh compatibility..."
cargo update -p sha2 --precise 0.11.0-rc.4
cargo update -p digest --precise 0.11.0-rc.9

echo ""
echo "Verifying build..."
cargo check

echo ""
echo "=== Setup complete! ==="
echo ""
echo "Next steps:"
echo "  cargo build          # Build all crates"
echo "  cargo test           # Run tests"
echo "  cargo run -p yurei-server-bin  # Run server"
echo ""
echo "For development without camera hardware:"
echo "  cargo test -p yurei-capture --features test-source"
echo ""
