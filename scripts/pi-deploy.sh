#!/usr/bin/env bash
set -euo pipefail

# Quick deploy script - builds and copies kodama-camera to the Pi.
# Usage: ./scripts/pi-deploy.sh [PI_HOST] [PI_USER] [PI_PASSWORD]

PI_HOST="${1:-10.0.0.229}"
PI_USER="${2:-yurei}"
PI_PASSWORD="${3:-password}"
PI_DEPLOY_DIR="/home/${PI_USER}/kodama"
SSH_OPTS="-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "${SCRIPT_DIR}")"
TARGET_BIN="${PROJECT_ROOT}/target/aarch64-unknown-linux-gnu/release/kodama-camera"

echo "Building kodama-camera for aarch64..."
cargo build --release --target aarch64-unknown-linux-gnu -p kodama-camera 2>&1 | tail -3

echo "Stopping camera on Pi..."
sshpass -p "${PI_PASSWORD}" ssh ${SSH_OPTS} "${PI_USER}@${PI_HOST}" \
    "sudo pkill -f kodama-camera 2>/dev/null || true"

echo "Deploying to ${PI_USER}@${PI_HOST}:${PI_DEPLOY_DIR}/..."
sshpass -p "${PI_PASSWORD}" scp ${SSH_OPTS} \
    "${TARGET_BIN}" "${PI_USER}@${PI_HOST}:${PI_DEPLOY_DIR}/kodama-camera"

echo "Done. Start with:"
echo "  ssh ${PI_USER}@${PI_HOST} 'cd ${PI_DEPLOY_DIR} && KODAMA_SERVER_KEY=<key> KODAMA_KEY_PATH=${PI_DEPLOY_DIR}/camera.key ./kodama-camera'"
