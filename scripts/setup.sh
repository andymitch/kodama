#!/usr/bin/env bash
set -euo pipefail

# Kodama development setup script
# Handles crypto dependency pinning required for iroh compatibility

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_ROOT"

echo "=== Kodama Development Setup ==="
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
# Note: Tauri brings in sha2/digest 0.10.x, while Iroh needs 0.11.0-rc.x
# Both versions can coexist, we just need to pin the ones Iroh uses
echo "Pinning crypto dependencies for iroh compatibility..."

# Use version-qualified package specs to handle multiple versions
# If only one version exists, use the unqualified name; otherwise use qualified
if cargo update -p sha2 --precise 0.11.0-rc.4 2>/dev/null; then
    echo "  sha2 pinned to 0.11.0-rc.4"
elif cargo update -p sha2@0.11.0-rc.4 --precise 0.11.0-rc.4 2>/dev/null; then
    echo "  sha2@0.11.0-rc.4 already at correct version"
else
    echo "  sha2 pinning skipped (may already be correct)"
fi

if cargo update -p digest --precise 0.11.0-rc.9 2>/dev/null; then
    echo "  digest pinned to 0.11.0-rc.9"
elif cargo update -p digest@0.11.0-rc.9 --precise 0.11.0-rc.9 2>/dev/null; then
    echo "  digest@0.11.0-rc.9 already at correct version"
else
    echo "  digest pinning skipped (may already be correct)"
fi

echo ""
echo "Verifying build..."
cargo check

echo ""
echo "=== Setup complete! ==="
echo ""
echo "Next steps:"
echo "  cargo build                      # Build library and binaries"
echo "  cargo test                       # Run tests"
echo "  cargo run --bin kodama-server-cli  # Run server"
echo ""
echo "For development without camera hardware:"
echo "  cargo run --bin kodama-camera --features test-source"
echo ""
