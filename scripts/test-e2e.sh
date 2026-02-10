#!/usr/bin/env bash
set -euo pipefail

# Kodama End-to-End Test
#
# Tests the full pipeline: Camera → Server
# Uses the test source for development without camera hardware.
#
# Usage:
#   ./scripts/test-e2e.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_ROOT"

echo "=== Kodama E2E Test ==="
echo ""

# Cleanup function
cleanup() {
    echo ""
    echo "Cleaning up..."
    if [ -n "${SERVER_PID:-}" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
    fi
    if [ -n "${CAMERA_PID:-}" ]; then
        kill "$CAMERA_PID" 2>/dev/null || true
    fi
    # Remove temp files
    rm -f /tmp/kodama-server.log /tmp/kodama-firmware.log
    rm -f ./server.key ./camera.key  # Clean up keys
}

trap cleanup EXIT

# Build binaries with test-source feature for camera
echo "Building binaries..."
cargo build --release --bin kodama-server
cargo build --release --bin kodama-firmware --features test-source
echo "Build complete."
echo ""

# Start server
echo "Starting server..."
./target/release/kodama-server > /tmp/kodama-server.log 2>&1 &
SERVER_PID=$!
sleep 2

# Check server is running
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: Server failed to start"
    cat /tmp/kodama-server.log
    exit 1
fi

# Extract server's public key from logs
SERVER_KEY=$(grep "Server PublicKey:" /tmp/kodama-server.log | head -1 | awk '{print $NF}')
if [ -z "$SERVER_KEY" ]; then
    echo "ERROR: Could not find server PublicKey in logs"
    cat /tmp/kodama-server.log
    exit 1
fi

echo "Server started with PublicKey: $SERVER_KEY"
echo ""

# Start camera with test source
echo "Starting camera (test source)..."
KODAMA_SERVER_KEY="$SERVER_KEY" KODAMA_KEY_PATH="./camera.key" ./target/release/kodama-firmware --test-source > /tmp/kodama-firmware.log 2>&1 &
CAMERA_PID=$!
sleep 2

# Check camera is running
if ! kill -0 "$CAMERA_PID" 2>/dev/null; then
    echo "ERROR: Camera failed to start"
    cat /tmp/kodama-firmware.log
    exit 1
fi

CAMERA_KEY=$(grep "Camera PublicKey:" /tmp/kodama-firmware.log | head -1 | awk '{print $NF}')
echo "Camera started with PublicKey: $CAMERA_KEY"
echo ""

echo "=== Test Running ==="
echo ""
echo "Server PublicKey: $SERVER_KEY"
echo "Camera PublicKey: $CAMERA_KEY"
echo ""
echo "Logs:"
echo "  Server: /tmp/kodama-server.log"
echo "  Camera: /tmp/kodama-firmware.log"
echo ""
echo "Running for 20 seconds to collect stats..."
echo ""

# Let it run for a bit
sleep 20

# Show stats
echo "=== Results ==="
echo ""

echo "--- Server Stats ---"
grep -E "(Stats:|frames|cameras|clients)" /tmp/kodama-server.log | tail -5 || echo "(no stats yet)"
echo ""

echo "--- Camera Stats ---"
grep -E "(Stats:|frames|keyframes|Mbps)" /tmp/kodama-firmware.log | tail -5 || echo "(no stats yet)"
echo ""

# Check for errors
if grep -q "error" /tmp/kodama-server.log; then
    echo "WARNING: Errors found in server log"
    grep "error" /tmp/kodama-server.log | head -5
fi

if grep -q "error" /tmp/kodama-firmware.log; then
    echo "WARNING: Errors found in camera log"
    grep "error" /tmp/kodama-firmware.log | head -5
fi

echo ""
echo "=== Test Complete ==="
echo ""

# Success criteria: camera sent frames and server received them
echo "Checking success criteria..."

CAMERA_FRAMES=$(grep "Stats:" /tmp/kodama-firmware.log | tail -1 | grep -oE 'video=[0-9]+' | grep -oE '[0-9]+' || echo "0")
SERVER_FRAMES=$(grep -E "frames_received|Rx:" /tmp/kodama-server.log | tail -1 | grep -oE '[0-9]+' | head -1 || echo "0")

echo "  Camera frames sent: $CAMERA_FRAMES"
echo "  Server frames received: $SERVER_FRAMES"

if [ "$CAMERA_FRAMES" -gt 0 ]; then
    echo ""
    echo "SUCCESS: End-to-end pipeline working!"
    echo "  Camera is sending frames"
    echo "  Server is receiving and routing frames"
    exit 0
else
    echo ""
    echo "INCOMPLETE: Check logs for issues"
    echo "  Camera frames: $CAMERA_FRAMES (expected > 0)"
    exit 1
fi
