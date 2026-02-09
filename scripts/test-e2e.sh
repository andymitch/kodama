#!/usr/bin/env bash
set -euo pipefail

# Kodama POC 1 End-to-End Test
#
# This script tests the full pipeline: Camera → Server → Client
# Uses the test source for development without camera hardware.
#
# Usage:
#   ./scripts/test-e2e.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_ROOT"

echo "=== Kodama POC 1 E2E Test ==="
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
    if [ -n "${CLIENT_PID:-}" ]; then
        kill "$CLIENT_PID" 2>/dev/null || true
    fi
    # Remove temp files
    rm -f /tmp/kodama-cli.log /tmp/kodama-camera.log /tmp/kodama-client.log
    rm -f ./server.key ./camera.key  # Clean up keys
}

trap cleanup EXIT

# Build all binaries with test-source feature for camera
echo "Building binaries..."
cargo build --release --bin kodama-cli
cargo build --release --bin kodama-camera --features test-source
cargo build --release --bin kodama-client
echo "Build complete."
echo ""

# Start server
echo "Starting server..."
./target/release/kodama-cli > /tmp/kodama-cli.log 2>&1 &
SERVER_PID=$!
sleep 2

# Check server is running
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: Server failed to start"
    cat /tmp/kodama-cli.log
    exit 1
fi

# Extract server's public key from logs
SERVER_KEY=$(grep "Server PublicKey:" /tmp/kodama-cli.log | head -1 | awk '{print $NF}')
if [ -z "$SERVER_KEY" ]; then
    echo "ERROR: Could not find server PublicKey in logs"
    cat /tmp/kodama-cli.log
    exit 1
fi

echo "Server started with PublicKey: $SERVER_KEY"
echo ""

# Start camera with test source
echo "Starting camera (test source)..."
KODAMA_SERVER_KEY="$SERVER_KEY" KODAMA_KEY_PATH="./camera.key" ./target/release/kodama-camera --test-source > /tmp/kodama-camera.log 2>&1 &
CAMERA_PID=$!
sleep 2

# Check camera is running
if ! kill -0 "$CAMERA_PID" 2>/dev/null; then
    echo "ERROR: Camera failed to start"
    cat /tmp/kodama-camera.log
    exit 1
fi

CAMERA_KEY=$(grep "Camera PublicKey:" /tmp/kodama-camera.log | head -1 | awk '{print $NF}')
echo "Camera started with PublicKey: $CAMERA_KEY"
echo ""

# Start client
echo "Starting client..."
KODAMA_SERVER_KEY="$SERVER_KEY" ./target/release/kodama-client > /tmp/kodama-client.log 2>&1 &
CLIENT_PID=$!
sleep 5  # Give time for connection and frame stream

# Check client is running
if ! kill -0 "$CLIENT_PID" 2>/dev/null; then
    echo "WARNING: Client may have exited"
fi

echo ""
echo "=== Test Running ==="
echo ""
echo "Server PublicKey: $SERVER_KEY"
echo "Camera PublicKey: $CAMERA_KEY"
echo ""
echo "Logs:"
echo "  Server: /tmp/kodama-cli.log"
echo "  Camera: /tmp/kodama-camera.log"
echo "  Client: /tmp/kodama-client.log"
echo ""
echo "Running for 20 seconds to collect stats..."
echo ""

# Let it run for a bit
sleep 20

# Show stats
echo "=== Results ==="
echo ""

echo "--- Server Stats ---"
grep -E "(Stats:|frames|cameras|clients)" /tmp/kodama-cli.log | tail -5 || echo "(no stats yet)"
echo ""

echo "--- Camera Stats ---"
grep -E "(Stats:|frames|keyframes|Mbps)" /tmp/kodama-camera.log | tail -5 || echo "(no stats yet)"
echo ""

echo "--- Client Stats ---"
grep -E "(Stats:|frames|keyframes|Mbps)" /tmp/kodama-client.log | tail -5 || echo "(no stats yet)"
echo ""

# Check for errors
if grep -q "error" /tmp/kodama-cli.log; then
    echo "WARNING: Errors found in server log"
    grep "error" /tmp/kodama-cli.log | head -5
fi

if grep -q "error" /tmp/kodama-camera.log; then
    echo "WARNING: Errors found in camera log"
    grep "error" /tmp/kodama-camera.log | head -5
fi

if grep -q "error" /tmp/kodama-client.log; then
    echo "WARNING: Errors found in client log"
    grep "error" /tmp/kodama-client.log | head -5
fi

echo ""
echo "=== Test Complete ==="
echo ""

# Success criteria check
echo "Checking success criteria..."

# Camera format: "Stats: video=601 (12 kf), audio=1002..."
CAMERA_FRAMES=$(grep "Stats:" /tmp/kodama-camera.log | tail -1 | grep -oE 'video=[0-9]+' | grep -oE '[0-9]+' || echo "0")
# Client format: "video=626 frames (16 kf)..."
CLIENT_FRAMES=$(grep "video=" /tmp/kodama-client.log | tail -1 | grep -oE 'video=[0-9]+' | grep -oE '[0-9]+' || echo "0")

echo "  Camera frames sent: $CAMERA_FRAMES"
echo "  Client frames received: $CLIENT_FRAMES"

if [ "$CAMERA_FRAMES" -gt 0 ] && [ "$CLIENT_FRAMES" -gt 0 ]; then
    echo ""
    echo "SUCCESS: End-to-end pipeline working!"
    echo "  Camera is sending frames"
    echo "  Client is receiving frames"
    exit 0
else
    echo ""
    echo "INCOMPLETE: Check logs for issues"
    echo "  Camera frames: $CAMERA_FRAMES (expected > 0)"
    echo "  Client frames: $CLIENT_FRAMES (expected > 0)"
    exit 1
fi
