#!/usr/bin/env bash
set -euo pipefail

# Yurei POC 1 End-to-End Test
#
# This script tests the full pipeline: Camera → Server → Client
# Uses the test source for development without camera hardware.
#
# Usage:
#   ./scripts/test-e2e.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_ROOT"

echo "=== Yurei POC 1 E2E Test ==="
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
    rm -f /tmp/yurei-server.log /tmp/yurei-camera.log /tmp/yurei-client.log
    rm -f ./server.key ./camera.key  # Clean up keys
}

trap cleanup EXIT

# Build all binaries with test-source feature for camera
echo "Building binaries..."
cargo build --release -p yurei-server-bin
cargo build --release -p yurei-camera --features test-source
cargo build --release -p yurei-desktop
echo "Build complete."
echo ""

# Start server
echo "Starting server..."
./target/release/yurei-server-bin > /tmp/yurei-server.log 2>&1 &
SERVER_PID=$!
sleep 2

# Check server is running
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: Server failed to start"
    cat /tmp/yurei-server.log
    exit 1
fi

# Extract server's public key from logs
SERVER_KEY=$(grep "Server PublicKey:" /tmp/yurei-server.log | head -1 | awk '{print $NF}')
if [ -z "$SERVER_KEY" ]; then
    echo "ERROR: Could not find server PublicKey in logs"
    cat /tmp/yurei-server.log
    exit 1
fi

echo "Server started with PublicKey: $SERVER_KEY"
echo ""

# Start camera with test source
echo "Starting camera (test source)..."
YUREI_SERVER_KEY="$SERVER_KEY" YUREI_KEY_PATH="./camera.key" ./target/release/yurei-camera --test-source > /tmp/yurei-camera.log 2>&1 &
CAMERA_PID=$!
sleep 2

# Check camera is running
if ! kill -0 "$CAMERA_PID" 2>/dev/null; then
    echo "ERROR: Camera failed to start"
    cat /tmp/yurei-camera.log
    exit 1
fi

CAMERA_KEY=$(grep "Camera PublicKey:" /tmp/yurei-camera.log | head -1 | awk '{print $NF}')
echo "Camera started with PublicKey: $CAMERA_KEY"
echo ""

# Start client
echo "Starting client..."
YUREI_SERVER_KEY="$SERVER_KEY" ./target/release/yurei-desktop > /tmp/yurei-client.log 2>&1 &
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
echo "  Server: /tmp/yurei-server.log"
echo "  Camera: /tmp/yurei-camera.log"
echo "  Client: /tmp/yurei-client.log"
echo ""
echo "Running for 20 seconds to collect stats..."
echo ""

# Let it run for a bit
sleep 20

# Show stats
echo "=== Results ==="
echo ""

echo "--- Server Stats ---"
grep -E "(Stats:|frames|cameras|clients)" /tmp/yurei-server.log | tail -5 || echo "(no stats yet)"
echo ""

echo "--- Camera Stats ---"
grep -E "(Stats:|frames|keyframes|Mbps)" /tmp/yurei-camera.log | tail -5 || echo "(no stats yet)"
echo ""

echo "--- Client Stats ---"
grep -E "(Stats:|frames|keyframes|Mbps)" /tmp/yurei-client.log | tail -5 || echo "(no stats yet)"
echo ""

# Check for errors
if grep -q "error" /tmp/yurei-server.log; then
    echo "WARNING: Errors found in server log"
    grep "error" /tmp/yurei-server.log | head -5
fi

if grep -q "error" /tmp/yurei-camera.log; then
    echo "WARNING: Errors found in camera log"
    grep "error" /tmp/yurei-camera.log | head -5
fi

if grep -q "error" /tmp/yurei-client.log; then
    echo "WARNING: Errors found in client log"
    grep "error" /tmp/yurei-client.log | head -5
fi

echo ""
echo "=== Test Complete ==="
echo ""

# Success criteria check
echo "Checking success criteria..."

CAMERA_FRAMES=$(grep "Stats:" /tmp/yurei-camera.log | tail -1 | grep -oE '[0-9]+ frames' | head -1 | grep -oE '[0-9]+' || echo "0")
CLIENT_FRAMES=$(grep "Stats:" /tmp/yurei-client.log | tail -1 | grep -oE '[0-9]+ frames' | head -1 | grep -oE '[0-9]+' || echo "0")

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
