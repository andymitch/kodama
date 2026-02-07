#!/usr/bin/env bash
set -euo pipefail

# Pi Camera Setup Script
# Provisions a fresh Raspberry Pi for use as a Kodama camera node.
# Run this FROM the development machine (not on the Pi itself).
#
# Prerequisites:
#   - Pi accessible via SSH with known IP, user, and password
#   - Pi running Debian 12+ (bookworm/trixie) with rpicam-vid available
#   - SimTech SIM7600 USB modem connected (for GPS; optional)
#
# Usage:
#   ./scripts/pi-setup.sh [PI_HOST] [PI_USER] [PI_PASSWORD]
#
# Defaults:
#   PI_HOST=10.0.0.229  PI_USER=yurei  PI_PASSWORD=password

PI_HOST="${1:-10.0.0.229}"
PI_USER="${2:-yurei}"
PI_PASSWORD="${3:-password}"
PI_DEPLOY_DIR="/home/${PI_USER}/kodama"
SSH_OPTS="-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10"

echo "=== Kodama Pi Camera Setup ==="
echo "  Host: ${PI_HOST}"
echo "  User: ${PI_USER}"
echo "  Deploy dir: ${PI_DEPLOY_DIR}"
echo ""

# Helper: run command on Pi via SSH
pi_ssh() {
    sshpass -p "${PI_PASSWORD}" ssh ${SSH_OPTS} "${PI_USER}@${PI_HOST}" "$@"
}

# Helper: copy file to Pi via SCP
pi_scp() {
    sshpass -p "${PI_PASSWORD}" scp ${SSH_OPTS} "$1" "${PI_USER}@${PI_HOST}:$2"
}

# Check sshpass is available
if ! command -v sshpass &> /dev/null; then
    echo "ERROR: sshpass not found. Install it:"
    echo "  macOS: brew install hudochenkov/sshpass/sshpass"
    echo "  Linux: sudo apt install sshpass"
    exit 1
fi

# Check connectivity
echo "Checking connectivity..."
if ! pi_ssh "echo ok" > /dev/null 2>&1; then
    echo "ERROR: Cannot connect to ${PI_USER}@${PI_HOST}"
    exit 1
fi
echo "  Connected."

# --- Step 1: Create deployment directory ---
echo ""
echo "=== Step 1: Create deployment directory ==="
pi_ssh "mkdir -p ${PI_DEPLOY_DIR}"
echo "  Created ${PI_DEPLOY_DIR}"

# --- Step 2: Stop any existing kodama/yurei services ---
echo ""
echo "=== Step 2: Stop existing services ==="
pi_ssh "sudo systemctl stop yurei 2>/dev/null || true"
pi_ssh "sudo pkill -f '[k]odama-camera' 2>/dev/null || true"
pi_ssh "sudo pkill -f '/usr/local/bin/yurei' 2>/dev/null || true"
echo "  Stopped."

# --- Step 3: Install system dependencies ---
echo ""
echo "=== Step 3: Install system dependencies ==="
echo "  Installing gpsd, gpsd-clients..."
pi_ssh "sudo apt-get update -qq && sudo apt-get install -y -qq gpsd gpsd-clients 2>&1 | tail -3"
echo "  Done."

# --- Step 4: Configure gpsd for SimTech SIM7600 GPS ---
echo ""
echo "=== Step 4: Configure GPS (gpsd) ==="

# Detect the NMEA GPS port (SimTech SIM7600 exposes GPS on ttyUSB1)
GPS_PORT=$(pi_ssh "ls /dev/ttyUSB1 2>/dev/null && echo /dev/ttyUSB1 || echo ''")
if [ -n "${GPS_PORT}" ]; then
    echo "  GPS serial port found: ${GPS_PORT}"

    # Write gpsd config
    pi_ssh "sudo tee /etc/default/gpsd > /dev/null" <<'GPSD_EOF'
# Kodama GPS configuration
# SimTech SIM7600 NMEA port
DEVICES="/dev/ttyUSB1"

# -n: don't wait for a client to connect before polling
GPSD_OPTIONS="-n"

# Auto-detect USB GPS devices
USBAUTO="true"
GPSD_EOF

    # Enable GPS on the SIM7600 modem via ModemManager
    echo "  Enabling GPS on SIM7600 modem..."
    MODEM_IDX=$(pi_ssh "sudo mmcli -L 2>/dev/null | grep -oP '/Modem/\K[0-9]+' || echo ''")
    if [ -n "${MODEM_IDX}" ]; then
        pi_ssh "sudo mmcli -m ${MODEM_IDX} --location-enable-gps-nmea --location-enable-gps-raw 2>&1 || true"
        echo "  GPS enabled on modem index ${MODEM_IDX}."
    else
        echo "  WARNING: No modem found in ModemManager. GPS may not work."
    fi

    # Start gpsd
    pi_ssh "sudo systemctl restart gpsd && sudo systemctl enable gpsd"
    echo "  gpsd started and enabled."

    # Verify GPS
    echo "  Waiting for GPS data..."
    sleep 2
    GPS_CHECK=$(pi_ssh "timeout 5 gpspipe -w 2>/dev/null | grep -m1 TPV || echo 'no TPV'")
    if echo "${GPS_CHECK}" | grep -q '"lat"'; then
        echo "  GPS fix acquired!"
    elif echo "${GPS_CHECK}" | grep -q 'TPV'; then
        echo "  GPS connected but no fix yet (may need outdoor antenna)."
    else
        echo "  WARNING: No GPS data from gpsd. Check antenna and modem."
    fi
else
    echo "  No GPS serial port found (/dev/ttyUSB1 missing)."
    echo "  Skipping GPS setup. Camera will run without GPS."
fi

# --- Step 5: Build and deploy camera binary ---
echo ""
echo "=== Step 5: Build and deploy camera binary ==="

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "${SCRIPT_DIR}")"
TARGET_BIN="${PROJECT_ROOT}/target/aarch64-unknown-linux-gnu/release/kodama-camera"

echo "  Cross-compiling for aarch64..."
cargo build --release --target aarch64-unknown-linux-gnu -p kodama-camera 2>&1 | tail -3

if [ ! -f "${TARGET_BIN}" ]; then
    echo "ERROR: Build failed - binary not found at ${TARGET_BIN}"
    exit 1
fi

echo "  Deploying to Pi..."
pi_scp "${TARGET_BIN}" "${PI_DEPLOY_DIR}/kodama-camera"
echo "  Deployed."

# --- Step 6: Create systemd service (optional) ---
echo ""
echo "=== Step 6: Create systemd service ==="
echo "  To run as a service, set KODAMA_SERVER_KEY and run:"
echo ""
echo "    sshpass -p \"${PI_PASSWORD}\" ssh ${PI_USER}@${PI_HOST} \\"
echo "      \"cd ${PI_DEPLOY_DIR} && \\"
echo "       KODAMA_SERVER_KEY=<server_key> \\"
echo "       KODAMA_KEY_PATH=${PI_DEPLOY_DIR}/camera.key \\"
echo "       ./kodama-camera\""
echo ""

# --- Summary ---
echo "=== Setup complete! ==="
echo ""
echo "To start the camera manually:"
echo "  ssh ${PI_USER}@${PI_HOST}"
echo "  cd ${PI_DEPLOY_DIR}"
echo "  KODAMA_SERVER_KEY=<key> KODAMA_KEY_PATH=${PI_DEPLOY_DIR}/camera.key ./kodama-camera"
echo ""
echo "To deploy updates in the future:"
echo "  cargo build --release --target aarch64-unknown-linux-gnu -p kodama-camera"
echo "  sshpass -p \"${PI_PASSWORD}\" scp ${SSH_OPTS} \\"
echo "    target/aarch64-unknown-linux-gnu/release/kodama-camera \\"
echo "    ${PI_USER}@${PI_HOST}:${PI_DEPLOY_DIR}/"
echo ""
