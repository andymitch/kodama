#!/usr/bin/env bash
set -euo pipefail

# Toggle WiFi off on the Pi for a set duration, then auto-restore.
#
# Runs a self-recovering script on the Pi via nohup so it survives
# the SSH disconnect that happens when WiFi goes down.
#
# Usage: ./scripts/pi-wifi-toggle.sh [DURATION_SECS] [PI_HOST] [PI_USER] [PI_PASSWORD]
#
# The Pi must have both wlan0 (WiFi) and wwan0 (cellular) active.
# SSH will disconnect when WiFi drops. Wait for DURATION + ~5s, then
# SSH back in to check /tmp/wifi-toggle.log.

DURATION="${1:-60}"
PI_HOST="${2:-10.0.0.229}"
PI_USER="${3:-yurei}"
PI_PASSWORD="${4:-password}"
SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"

WIFI_CONN="netplan-wlan0-mitchellhaus"
LOG="/tmp/wifi-toggle.log"

echo "Will drop WiFi on ${PI_HOST} for ${DURATION}s with auto-recovery."
echo "SSH will disconnect when WiFi goes down — this is expected."
echo "Reconnect after ~$((DURATION + 10))s to check ${LOG}."
echo ""

# Upload and run a self-contained script via nohup.
# The script: log start → drop WiFi → sleep → restore WiFi → log done.
sshpass -p "${PI_PASSWORD}" ssh ${SSH_OPTS} "${PI_USER}@${PI_HOST}" "
cat > /tmp/wifi-toggle-run.sh << 'SCRIPT'
#!/bin/bash
DURATION=\$1
CONN=\$2
LOG=\$3

echo \"[\$(date -Iseconds)] WiFi toggle started (duration=\${DURATION}s)\" > \"\$LOG\"

echo \"[\$(date -Iseconds)] Dropping WiFi (\$CONN)...\" >> \"\$LOG\"
sudo nmcli connection down \"\$CONN\" >> \"\$LOG\" 2>&1

echo \"[\$(date -Iseconds)] WiFi down. Sleeping \${DURATION}s...\" >> \"\$LOG\"
sleep \"\$DURATION\"

echo \"[\$(date -Iseconds)] Restoring WiFi (\$CONN)...\" >> \"\$LOG\"
sudo nmcli connection up \"\$CONN\" >> \"\$LOG\" 2>&1

echo \"[\$(date -Iseconds)] WiFi restored. Toggle complete.\" >> \"\$LOG\"
SCRIPT
chmod +x /tmp/wifi-toggle-run.sh

nohup /tmp/wifi-toggle-run.sh '${DURATION}' '${WIFI_CONN}' '${LOG}' </dev/null >/dev/null 2>&1 &
echo \"Toggle script launched (PID=\$!). WiFi will drop momentarily.\"
"

echo ""
echo "Script launched. WiFi will drop in ~1s."
echo "Wait ${DURATION}s + reconnect time, then:"
echo "  sshpass -p '${PI_PASSWORD}' ssh ${SSH_OPTS} ${PI_USER}@${PI_HOST} 'cat ${LOG}'"
