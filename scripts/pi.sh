#!/usr/bin/env bash
set -euo pipefail

# Kodama Pi Camera Management
#
# Single entry point for all Pi operations. Run from the dev machine.
#
# Usage:
#   ./scripts/pi.sh setup    [HOST] [USER] [PASS]   # Full provisioning (fresh Pi)
#   ./scripts/pi.sh deploy   [HOST] [USER] [PASS]   # Quick build + deploy binary
#   ./scripts/pi.sh wifi-off [SECS] [HOST] [USER] [PASS]  # Toggle WiFi off for testing
#
# Defaults: HOST=10.0.0.229  USER=yurei  PASS=password
#
# Config files deployed to the Pi live in pi/:
#   pi/gpsd.conf                               → /etc/default/gpsd
#   pi/kodama-enable-gps.sh                    → /usr/local/bin/kodama-enable-gps.sh
#   pi/systemd/kodama-gps.service              → /etc/systemd/system/
#   pi/networkmanager/no-connectivity-check.conf → /etc/NetworkManager/conf.d/
#   pi/networkmanager/99-keep-cellular-route    → /etc/NetworkManager/dispatcher.d/

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "${SCRIPT_DIR}")"
PI_DIR="${PROJECT_ROOT}/pi"

SSH_OPTS="-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10"

# --- Helpers ---

pi_ssh() {
    sshpass -p "${PI_PASSWORD}" ssh ${SSH_OPTS} "${PI_USER}@${PI_HOST}" "$@"
}

pi_scp() {
    sshpass -p "${PI_PASSWORD}" scp ${SSH_OPTS} "$1" "${PI_USER}@${PI_HOST}:$2"
}

check_sshpass() {
    if ! command -v sshpass &> /dev/null; then
        echo "ERROR: sshpass not found. Install it:"
        echo "  macOS: brew install hudochenkov/sshpass/sshpass"
        echo "  Linux: sudo apt install sshpass"
        exit 1
    fi
}

check_connection() {
    echo "Connecting to ${PI_USER}@${PI_HOST}..."
    if ! pi_ssh "echo ok" > /dev/null 2>&1; then
        echo "ERROR: Cannot connect to ${PI_USER}@${PI_HOST}"
        exit 1
    fi
}

stop_camera() {
    echo "Stopping camera..."
    pi_ssh "sudo systemctl stop yurei 2>/dev/null || true"
    pi_ssh "sudo pkill -f '[k]odama-camera' 2>/dev/null || true"
    pi_ssh "sudo pkill -f '/usr/local/bin/yurei' 2>/dev/null || true"
}

build_and_deploy() {
    local target_bin="${PROJECT_ROOT}/target/aarch64-unknown-linux-gnu/release/kodama-camera"
    local deploy_dir="/home/${PI_USER}/kodama"

    echo "Cross-compiling for aarch64..."
    cargo build --release --target aarch64-unknown-linux-gnu -p kodama-camera 2>&1 | tail -3

    if [ ! -f "${target_bin}" ]; then
        echo "ERROR: Build failed - binary not found"
        exit 1
    fi

    pi_ssh "mkdir -p ${deploy_dir}"
    echo "Deploying binary..."
    pi_scp "${target_bin}" "${deploy_dir}/kodama-camera"
    echo "Deployed to ${deploy_dir}/kodama-camera"
}

# --- Commands ---

cmd_deploy() {
    check_sshpass
    check_connection
    stop_camera
    build_and_deploy

    local deploy_dir="/home/${PI_USER}/kodama"
    echo ""
    echo "Done. Start with:"
    echo "  ssh ${PI_USER}@${PI_HOST} 'cd ${deploy_dir} && KODAMA_SERVER_KEY=<key> KODAMA_KEY_PATH=${deploy_dir}/camera.key ./kodama-camera'"
}

cmd_setup() {
    check_sshpass
    check_connection

    local deploy_dir="/home/${PI_USER}/kodama"
    pi_ssh "mkdir -p ${deploy_dir}"

    stop_camera

    # --- Install system dependencies ---
    echo ""
    echo "=== Installing dependencies ==="
    # Video: rpicam-vid (Pi OS) or libcamera-apps (other ARM)
    # Audio: alsa-utils (arecord)
    # GPS:   gpsd + gpsd-clients
    local packages="alsa-utils gpsd gpsd-clients"

    local has_video
    has_video=$(pi_ssh "command -v rpicam-vid >/dev/null 2>&1 && echo rpicam || (command -v libcamera-vid >/dev/null 2>&1 && echo libcamera) || echo none")
    if [ "${has_video}" = "none" ]; then
        echo "  No video capture tool found, will install libcamera-apps"
        packages="${packages} libcamera-apps"
    else
        echo "  Video capture: ${has_video}"
    fi

    echo "  Packages: ${packages}"
    pi_ssh "sudo apt-get update -qq && sudo apt-get install -y -qq ${packages} 2>&1 | tail -5"

    # --- Configure NetworkManager ---
    echo ""
    echo "=== Configuring NetworkManager ==="

    pi_scp "${PI_DIR}/networkmanager/no-connectivity-check.conf" "/tmp/no-connectivity-check.conf"
    pi_ssh "sudo mv /tmp/no-connectivity-check.conf /etc/NetworkManager/conf.d/no-connectivity-check.conf"
    echo "  Installed no-connectivity-check.conf"

    pi_scp "${PI_DIR}/networkmanager/99-keep-cellular-route" "/tmp/99-keep-cellular-route"
    pi_ssh "sudo mv /tmp/99-keep-cellular-route /etc/NetworkManager/dispatcher.d/99-keep-cellular-route && sudo chmod +x /etc/NetworkManager/dispatcher.d/99-keep-cellular-route"
    echo "  Installed 99-keep-cellular-route"

    pi_ssh "sudo systemctl reload NetworkManager 2>/dev/null || sudo systemctl restart NetworkManager"

    # --- Configure GPS ---
    echo ""
    echo "=== Configuring GPS ==="

    local gps_port
    gps_port=$(pi_ssh "ls /dev/ttyUSB1 2>/dev/null && echo /dev/ttyUSB1 || echo ''")
    if [ -n "${gps_port}" ]; then
        echo "  GPS serial port: ${gps_port}"

        pi_scp "${PI_DIR}/gpsd.conf" "/tmp/gpsd.conf"
        pi_ssh "sudo mv /tmp/gpsd.conf /etc/default/gpsd"

        pi_scp "${PI_DIR}/kodama-enable-gps.sh" "/tmp/kodama-enable-gps.sh"
        pi_ssh "sudo mv /tmp/kodama-enable-gps.sh /usr/local/bin/kodama-enable-gps.sh && sudo chmod +x /usr/local/bin/kodama-enable-gps.sh"

        pi_scp "${PI_DIR}/systemd/kodama-gps.service" "/tmp/kodama-gps.service"
        pi_ssh "sudo mv /tmp/kodama-gps.service /etc/systemd/system/kodama-gps.service"
        pi_ssh "sudo systemctl daemon-reload && sudo systemctl enable kodama-gps.service && sudo systemctl start kodama-gps.service"
        echo "  GPS boot service installed"

        pi_ssh "sudo systemctl restart gpsd && sudo systemctl enable gpsd"

        echo "  Verifying GPS..."
        sleep 2
        local gps_check
        gps_check=$(pi_ssh "timeout 5 gpspipe -w 2>/dev/null | grep -m1 TPV || echo 'no TPV'")
        if echo "${gps_check}" | grep -q '"lat"'; then
            echo "  GPS fix acquired"
        elif echo "${gps_check}" | grep -q 'TPV'; then
            echo "  GPS connected, no fix yet (needs sky visibility)"
        else
            echo "  WARNING: No GPS data. Check antenna and modem."
        fi
    else
        echo "  No GPS serial port (/dev/ttyUSB1), skipping"
    fi

    # --- Build and deploy ---
    echo ""
    echo "=== Building and deploying ==="
    build_and_deploy

    echo ""
    echo "=== Setup complete ==="
    echo ""
    echo "Start camera:"
    echo "  ssh ${PI_USER}@${PI_HOST} 'cd ${deploy_dir} && KODAMA_SERVER_KEY=<key> KODAMA_KEY_PATH=${deploy_dir}/camera.key ./kodama-camera'"
    echo ""
    echo "Quick deploy after code changes:"
    echo "  ./scripts/pi.sh deploy"
}

cmd_wifi_off() {
    local duration="${1:-60}"
    shift 2>/dev/null || true

    check_sshpass
    check_connection

    local wifi_conn log="/tmp/wifi-toggle.log"
    wifi_conn=$(pi_ssh "nmcli -t -f NAME,DEVICE con show --active 2>/dev/null | grep wlan0 | cut -d: -f1 || echo ''")
    if [ -z "${wifi_conn}" ]; then
        echo "ERROR: No active WiFi connection on wlan0"
        exit 1
    fi

    echo "Dropping WiFi (${wifi_conn}) for ${duration}s with auto-recovery."
    echo "SSH will disconnect — reconnect after ~$((duration + 10))s."
    echo ""

    pi_ssh "
cat > /tmp/wifi-toggle-run.sh << 'SCRIPT'
#!/bin/bash
echo \"[\$(date -Iseconds)] Dropping WiFi (\$2) for \${1}s\" > \"\$3\"
sudo nmcli connection down \"\$2\" >> \"\$3\" 2>&1
sleep \"\$1\"
echo \"[\$(date -Iseconds)] Restoring WiFi (\$2)\" >> \"\$3\"
sudo nmcli connection up \"\$2\" >> \"\$3\" 2>&1
echo \"[\$(date -Iseconds)] Done\" >> \"\$3\"
SCRIPT
chmod +x /tmp/wifi-toggle-run.sh
nohup /tmp/wifi-toggle-run.sh '${duration}' '${wifi_conn}' '${log}' </dev/null >/dev/null 2>&1 &
echo \"Launched (PID=\$!)\"
"

    echo ""
    echo "Check results after ~$((duration + 10))s:"
    echo "  sshpass -p '${PI_PASSWORD}' ssh ${PI_USER}@${PI_HOST} 'cat ${log}'"
}

# --- Main ---

CMD="${1:-}"
shift 2>/dev/null || true

case "${CMD}" in
    setup)
        PI_HOST="${1:-10.0.0.229}"
        PI_USER="${2:-yurei}"
        PI_PASSWORD="${3:-password}"
        cmd_setup
        ;;
    deploy)
        PI_HOST="${1:-10.0.0.229}"
        PI_USER="${2:-yurei}"
        PI_PASSWORD="${3:-password}"
        cmd_deploy
        ;;
    wifi-off)
        # First arg is duration, then host/user/pass
        DURATION="${1:-60}"
        PI_HOST="${2:-10.0.0.229}"
        PI_USER="${3:-yurei}"
        PI_PASSWORD="${4:-password}"
        cmd_wifi_off "${DURATION}"
        ;;
    *)
        echo "Kodama Pi Camera Management"
        echo ""
        echo "Usage:"
        echo "  ./scripts/pi.sh setup    [HOST] [USER] [PASS]         # Full provisioning"
        echo "  ./scripts/pi.sh deploy   [HOST] [USER] [PASS]         # Quick build + deploy"
        echo "  ./scripts/pi.sh wifi-off [SECS] [HOST] [USER] [PASS]  # Toggle WiFi for testing"
        echo ""
        echo "Defaults: HOST=10.0.0.229  USER=yurei  PASS=password"
        exit 1
        ;;
esac
