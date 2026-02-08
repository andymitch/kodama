#!/bin/bash
# Enable GPS on SIM7600 modem via ModemManager
# ModemManager doesn't persist location settings across reboots,
# so this script runs at boot via kodama-gps.service.

# Wait for ModemManager to register the modem (up to 30s)
for i in $(seq 1 30); do
    mmcli -L 2>/dev/null | grep -q Modem && break
    sleep 1
done

# Get modem index dynamically (index can change across reboots)
IDX=$(mmcli -L 2>/dev/null | grep -oP '/Modem/\K[0-9]+')
if [ -z "$IDX" ]; then
    echo "No modem found"
    exit 1
fi

echo "Enabling GPS on modem $IDX"
mmcli -m "$IDX" --location-enable-gps-nmea --location-enable-gps-raw
